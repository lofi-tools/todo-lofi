use std::collections::BTreeSet;
use std::fs;
use std::io::{self, ErrorKind, Read, Write};
use std::path::{Path, PathBuf};

use agent_harness::{AttachmentMetadata, PromptAttachment};
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Serialize)]
pub struct AttachmentLimit {
    pub bytes: usize,
    pub label: String,
}

impl AttachmentLimit {
    pub fn from_bytes(bytes: usize) -> Self {
        let label = if bytes.is_multiple_of(1_000_000) {
            format!("{} MB", bytes / 1_000_000)
        } else if bytes.is_multiple_of(1_000) {
            format!("{} kB", bytes / 1_000)
        } else if bytes == 1 {
            "1 byte".to_owned()
        } else {
            format!("{bytes} bytes")
        };
        Self { bytes, label }
    }

    pub fn check(&self, bytes: u64) -> io::Result<()> {
        if bytes > self.bytes as u64 {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                format!("Attachments must be up to {} each.", self.label),
            ));
        }
        Ok(())
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidInput, message)
}

fn directory(path: &Path, create: bool) -> io::Result<()> {
    if create {
        match fs::create_dir(path) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    if path.symlink_metadata()?.is_dir() {
        Ok(())
    } else {
        Err(invalid(
            "An attachment directory is not a regular directory.",
        ))
    }
}

fn task_directory(path: &Path, create: bool) -> io::Result<()> {
    let root = path
        .parent()
        .ok_or_else(|| invalid("Missing attachment root."))?;
    directory(root, create)?;
    directory(path, create)
}

pub(crate) fn remove_task(path: &Path) -> io::Result<()> {
    match task_directory(path, false) {
        Ok(()) => fs::remove_dir_all(path),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

pub(crate) fn stage_task(path: &Path) -> io::Result<Option<PathBuf>> {
    match task_directory(path, false) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    }
    let root = path
        .parent()
        .ok_or_else(|| invalid("Missing attachment root."))?;
    let task_id = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| invalid("Invalid task attachment directory."))?;
    let parsed_id = uuid::Uuid::parse_str(task_id)
        .map_err(|_| invalid("Invalid task attachment directory."))?;
    if parsed_id.to_string() != task_id {
        return Err(invalid("Invalid task attachment directory."));
    }
    let task_id = parsed_id;
    let staged = root.join(format!(".shepherd-deleting-task-{task_id}"));
    match staged.symlink_metadata() {
        Ok(_) => {
            return Err(io::Error::new(
                ErrorKind::AlreadyExists,
                "A staged attachment directory already exists.",
            ));
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let intent = root.join(format!(".shepherd-deletion-intent-{task_id}"));
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&intent)?;
    if let Err(error) = fs::rename(path, &staged) {
        if let Err(cleanup) = fs::remove_file(&intent) {
            return Err(io::Error::other(format!(
                "could not stage attachments: {error}; could not clear deletion intent: {cleanup}"
            )));
        }
        return Err(error);
    }
    Ok(Some(staged))
}

pub(crate) struct DeletionLock(fs::File);

impl Drop for DeletionLock {
    fn drop(&mut self) {
        if let Err(error) = self.0.unlock() {
            tracing::warn!(%error, "could not unlock attachment deletion recovery");
        }
    }
}

pub(crate) fn lock_deletions(data_directory: &Path) -> io::Result<DeletionLock> {
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(data_directory.join(".attachment-deletions.lock"))?;
    if !file.metadata()?.is_file() {
        return Err(invalid(
            "The attachment recovery lock is not a regular file.",
        ));
    }
    file.try_lock().map_err(|error| {
        io::Error::other(format!(
            "could not lock attachment deletion recovery: {error}"
        ))
    })?;
    Ok(DeletionLock(file))
}

pub(crate) fn recover_staged(root: &Path, tasks: &BTreeSet<String>) -> io::Result<()> {
    match directory(root, false) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    }
    let entries = fs::read_dir(root)?.collect::<io::Result<Vec<_>>>()?;
    for entry in entries {
        let name = entry.file_name();
        let Some(suffix) = name
            .to_str()
            .and_then(|name| name.strip_prefix(".shepherd-deletion-intent-"))
        else {
            continue;
        };
        let Ok(task_id) = uuid::Uuid::parse_str(suffix) else {
            continue;
        };
        if task_id.to_string() != suffix {
            continue;
        }
        let intent = entry.path();
        if !intent.symlink_metadata()?.is_file() {
            return Err(invalid(
                "An attachment deletion intent is not a regular file.",
            ));
        }
        let staged = root.join(format!(".shepherd-deleting-task-{task_id}"));
        let original = root.join(task_id.to_string());
        let exists = |path: &Path| -> io::Result<bool> {
            match path.symlink_metadata() {
                Ok(_) => Ok(true),
                Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
                Err(error) => Err(error),
            }
        };
        match (
            tasks.contains(&task_id.to_string()),
            exists(&original)?,
            exists(&staged)?,
        ) {
            (true, false, true) => restore_task(&staged, &original)?,
            (false, false, true) => remove_staged_task(&staged)?,
            (true, true, false) => {
                task_directory(&original, false)?;
                fs::remove_file(intent)?;
            }
            (false, false, false) => fs::remove_file(intent)?,
            _ => {
                return Err(io::Error::other(format!(
                    "Attachment deletion state is ambiguous for task {task_id}."
                )));
            }
        }
    }
    Ok(())
}

pub(crate) fn restore_task(staged: &Path, path: &Path) -> io::Result<()> {
    task_directory(staged, false)?;
    match path.symlink_metadata() {
        Ok(_) => {
            return Err(io::Error::new(
                ErrorKind::AlreadyExists,
                "The original attachment directory already exists.",
            ));
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    fs::rename(staged, path)?;
    fs::remove_file(staged_intent(staged)?)
}

pub(crate) fn remove_staged_task(staged: &Path) -> io::Result<()> {
    let intent = staged_intent(staged)?;
    if !intent.symlink_metadata()?.is_file() {
        return Err(invalid(
            "An attachment deletion intent is not a regular file.",
        ));
    }
    remove_task(staged)?;
    fs::remove_file(intent)
}

fn staged_intent(staged: &Path) -> io::Result<PathBuf> {
    let suffix = staged
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix(".shepherd-deleting-task-"))
        .ok_or_else(|| invalid("Invalid staged attachment directory."))?;
    let task_id = uuid::Uuid::parse_str(suffix)
        .map_err(|_| invalid("Invalid staged attachment directory."))?;
    if task_id.to_string() != suffix {
        return Err(invalid("Invalid staged attachment directory."));
    }
    Ok(staged
        .parent()
        .ok_or_else(|| invalid("Missing attachment root."))?
        .join(format!(".shepherd-deletion-intent-{task_id}")))
}

pub(crate) fn persist_prompt_file(path: &Path, name: &str, bytes: &[u8]) -> io::Result<String> {
    if name.is_empty()
        || name.len() > 255
        || matches!(name, "." | "..")
        || name
            .chars()
            .any(|character| character.is_control() || matches!(character, '/' | '\\' | ':'))
    {
        return Err(invalid("The attachment name must be a single file name."));
    }
    task_directory(path, true)?;
    let mut hash = Sha256::new();
    hash.update(name);
    hash.update([0]);
    hash.update(bytes);
    let id = hex_digest(&hash.finalize());
    let file_directory = path.join(format!("file-{id}"));
    directory(&file_directory, true)?;
    persist(&file_directory.join(name), bytes)?;
    Ok(id)
}

pub(crate) fn prompt_file(path: &Path, id: &str) -> io::Result<PathBuf> {
    if id.len() != 64 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid("Invalid attachment identifier."));
    }
    task_directory(path, false)?;
    let file_directory = path.join(format!("file-{id}"));
    directory(&file_directory, false)?;
    let mut contents = fs::read_dir(file_directory)?;
    let file = contents
        .next()
        .transpose()?
        .ok_or_else(|| invalid("The attachment is missing."))?;
    if contents.next().is_some() || !file.file_type()?.is_file() {
        return Err(invalid("The attachment is not a regular file."));
    }
    file.path().canonicalize()
}

pub(crate) fn resolve_prompt_file(
    path: &Path,
    id: &str,
    limit: &AttachmentLimit,
) -> io::Result<PromptAttachment> {
    let file_path = prompt_file(path, id)?;
    let relative_path = file_path
        .strip_prefix(path.canonicalize()?)
        .map_err(|_| invalid("The attachment is outside the task cache."))?;
    let mut file =
        open_cached_file(&file_path)?.ok_or_else(|| invalid("The attachment is missing."))?;
    let expected_bytes = file.metadata()?.len();
    limit.check(expected_bytes)?;
    let name = relative_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| invalid("The attachment name is invalid."))?;
    let mut hash = Sha256::new();
    hash.update(name.as_bytes());
    hash.update([0]);
    let mut header = Vec::with_capacity(12);
    let mut buffer = [0; 8192];
    let mut bytes = 0_u64;
    loop {
        let length = file.read(&mut buffer)?;
        if length == 0 {
            break;
        }
        bytes = bytes
            .checked_add(length as u64)
            .ok_or_else(|| invalid("The attachment is too large."))?;
        limit.check(bytes)?;
        let header_bytes = length.min(12 - header.len());
        header.extend_from_slice(&buffer[..header_bytes]);
        hash.update(&buffer[..length]);
    }
    if bytes != expected_bytes || !hex_digest(&hash.finalize()).eq_ignore_ascii_case(id) {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            "Cached attachment does not match its content.",
        ));
    }
    let image_mime_type = match header.as_slice() {
        [137, b'P', b'N', b'G', 13, 10, 26, 10, ..] => Some("image/png"),
        [255, 216, 255, ..] => Some("image/jpeg"),
        [b'G', b'I', b'F', b'8', b'7' | b'9', b'a', ..] => Some("image/gif"),
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P'] => Some("image/webp"),
        _ => None,
    };
    let mime = match image_mime_type {
        Some(mime) => mime.to_owned(),
        None => mime_guess::from_path(relative_path)
            .first_or_octet_stream()
            .essence_str()
            .to_owned(),
    };
    let metadata = AttachmentMetadata {
        id: id.to_owned(),
        relative_path: relative_path.to_owned(),
        mime,
        bytes,
    };
    Ok(PromptAttachment {
        path: file_path,
        image_mime_type: image_mime_type.map(str::to_owned),
        metadata: Some(metadata),
    })
}

pub(crate) fn persist_preview_screenshot(path: &Path, bytes: &[u8]) -> io::Result<PathBuf> {
    let extension = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "png"
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        "jpg"
    } else {
        return Err(invalid("Choose a PNG or JPEG screenshot."));
    };
    task_directory(path, true)?;
    let digest = hex_digest(&Sha256::digest(bytes));
    let file = path.join(format!("preview-{digest}.{extension}"));
    persist(&file, bytes)?;
    file.canonicalize()
}

fn hex_digest(digest: &[u8]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn persist(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if verify_cached_file(path, bytes)? {
        return Ok(());
    }
    let parent = path
        .parent()
        .ok_or_else(|| invalid("Missing attachment directory."))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    if let Err(error) = temporary.persist_noclobber(path)
        && (error.error.kind() != ErrorKind::AlreadyExists || !verify_cached_file(path, bytes)?)
    {
        return Err(error.error);
    }
    Ok(())
}

fn verify_cached_file(path: &Path, expected: &[u8]) -> io::Result<bool> {
    let Some(mut file) = open_cached_file(path)? else {
        return Ok(false);
    };
    let metadata = file.metadata()?;
    if metadata.len() != expected.len() as u64 {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            "Cached attachment does not match its content.",
        ));
    }
    let mut buffer = [0; 8192];
    for chunk in expected.chunks(buffer.len()) {
        file.read_exact(&mut buffer[..chunk.len()])?;
        if &buffer[..chunk.len()] != chunk {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                "Cached attachment does not match its content.",
            ));
        }
    }
    if file.read(&mut buffer[..1])? != 0 {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            "Cached attachment does not match its content.",
        ));
    }
    Ok(true)
}

fn open_cached_file(path: &Path) -> io::Result<Option<fs::File>> {
    #[cfg(not(unix))]
    match path.symlink_metadata() {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => return Err(invalid("The cached attachment is not a regular file.")),
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(invalid("The cached attachment is not a regular file."));
    }
    Ok(Some(file))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_files_must_match_content_before_reuse() -> io::Result<()> {
        let directory = tempfile::tempdir()?;
        let task = directory
            .path()
            .join("attachments")
            .join(uuid::Uuid::new_v4().to_string());
        let id = persist_prompt_file(&task, "note.txt", b"first")?;
        let file = prompt_file(&task, &id)?;
        let limit = AttachmentLimit::from_bytes(100);
        assert!(resolve_prompt_file(&task, &id, &limit).is_ok());
        assert_eq!(persist_prompt_file(&task, "note.txt", b"first")?, id);
        fs::write(&file, b"other")?;
        assert_eq!(
            resolve_prompt_file(&task, &id, &limit)
                .err()
                .map(|error| error.kind()),
            Some(ErrorKind::InvalidData)
        );
        assert_eq!(
            persist_prompt_file(&task, "note.txt", b"first")
                .err()
                .map(|error| error.kind()),
            Some(ErrorKind::InvalidData)
        );
        assert_eq!(fs::read(&file)?, b"other");
        let screenshot = b"\x89PNG\r\n\x1a\nimage";
        let path = persist_preview_screenshot(&task, screenshot)?;
        fs::write(&path, b"\x89PNG\r\n\x1a\nother")?;
        assert_eq!(
            persist_preview_screenshot(&task, screenshot)
                .err()
                .map(|error| error.kind()),
            Some(ErrorKind::InvalidData)
        );
        assert_eq!(fs::read(&path)?, b"\x89PNG\r\n\x1a\nother");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn recovery_lock_rejects_symlinks_and_fifos() -> io::Result<()> {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir()?;
        let lock = directory.path().join(".attachment-deletions.lock");
        let target = directory.path().join("outside");
        fs::write(&target, b"preserve")?;
        symlink(&target, &lock)?;
        assert!(lock_deletions(directory.path()).is_err());
        assert_eq!(fs::read(&target)?, b"preserve");
        fs::remove_file(&lock)?;
        let name = std::ffi::CString::new(lock.as_os_str().as_bytes()).map_err(io::Error::other)?;
        if unsafe { libc::mkfifo(name.as_ptr(), 0o600) } != 0 {
            return Err(io::Error::last_os_error());
        }
        assert_eq!(
            lock_deletions(directory.path())
                .err()
                .map(|error| error.kind()),
            Some(ErrorKind::InvalidInput)
        );
        fs::remove_file(&lock)?;
        let held = lock_deletions(directory.path())?;
        assert!(lock_deletions(directory.path()).is_err());
        drop(held);
        assert!(lock_deletions(directory.path()).is_ok());
        Ok(())
    }

    #[test]
    fn default_attachment_limit_is_fifty_megabytes() -> Result<(), Box<dyn std::error::Error>> {
        let limit = AttachmentLimit::from_bytes(crate::settings::DEFAULT_ATTACHMENT_BYTES);
        assert_eq!(limit.bytes, 50_000_000);
        assert_eq!(limit.label, "50 MB");
        assert!(limit.check(50_000_000).is_ok());
        assert!(limit.check(50_000_001).is_err());
        Ok(())
    }

    #[test]
    fn attachments_keep_names_and_bytes_within_their_task() -> io::Result<()> {
        let root = tempfile::tempdir()?;
        let task = root.path().join("attachments/task");
        assert!(persist_prompt_file(&task, "../outside", b"data").is_err());
        let id = persist_prompt_file(&task, "Résumé 1.txt", b"fixture")?;
        assert_eq!(persist_prompt_file(&task, "Résumé 1.txt", b"fixture")?, id);
        let file = prompt_file(&task, &id)?;
        assert_eq!(
            file.file_name().and_then(|name| name.to_str()),
            Some("Résumé 1.txt")
        );
        assert_eq!(fs::read(&file)?, b"fixture");
        assert!(prompt_file(&root.path().join("attachments/other-task"), &id).is_err());
        remove_task(&task)?;
        assert!(!file.exists());
        Ok(())
    }

    #[test]
    fn staged_deletion_can_restore_cached_files() -> io::Result<()> {
        let root = tempfile::tempdir()?;
        let task = root
            .path()
            .join("attachments")
            .join(uuid::Uuid::new_v4().to_string());
        let id = persist_prompt_file(&task, "notes.txt", b"keep")?;
        let staged = stage_task(&task)?.ok_or_else(|| io::Error::other("missing staged cache"))?;
        assert!(!task.exists());
        restore_task(&staged, &task)?;
        assert_eq!(fs::read(prompt_file(&task, &id)?)?, b"keep");
        assert!(!staged.exists());
        Ok(())
    }

    #[test]
    fn staged_recovery_restores_live_tasks_and_removes_deleted_tasks() -> io::Result<()> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("attachments");
        let live = uuid::Uuid::new_v4().to_string();
        let deleted = uuid::Uuid::new_v4().to_string();
        let live_path = root.join(&live);
        let deleted_path = root.join(&deleted);
        let live_file = persist_prompt_file(&live_path, "live.txt", b"keep")?;
        persist_prompt_file(&deleted_path, "deleted.txt", b"remove")?;
        let live_staged =
            stage_task(&live_path)?.ok_or_else(|| io::Error::other("missing live staging"))?;
        let deleted_staged = stage_task(&deleted_path)?
            .ok_or_else(|| io::Error::other("missing deleted staging"))?;
        let legacy = root.join(format!(".deleting-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&legacy)?;
        let unmarked = root.join(format!(".shepherd-deleting-task-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&unmarked)?;
        let lock = lock_deletions(directory.path())?;
        assert!(lock_deletions(directory.path()).is_err());
        recover_staged(&root, &BTreeSet::from([live.clone()]))?;
        assert_eq!(fs::read(prompt_file(&live_path, &live_file)?)?, b"keep");
        assert!(!live_staged.exists() && !deleted_staged.exists());
        assert!(legacy.is_dir());
        assert!(unmarked.is_dir());
        drop(lock);
        let _lock = lock_deletions(directory.path())?;
        Ok(())
    }

    #[test]
    fn staged_recovery_refuses_conflicting_original_directory() -> io::Result<()> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("attachments");
        let task = uuid::Uuid::new_v4().to_string();
        let original = root.join(&task);
        persist_prompt_file(&original, "notes.txt", b"staged data")?;
        let staged = stage_task(&original)?.ok_or_else(|| io::Error::other("missing staging"))?;
        fs::create_dir(&original)?;
        fs::write(original.join("existing"), b"do not replace")?;
        assert!(recover_staged(&root, &BTreeSet::from([task])).is_err());
        assert_eq!(fs::read(original.join("existing"))?, b"do not replace");
        assert!(staged.is_dir());
        Ok(())
    }

    #[test]
    fn recovery_clears_intent_before_move_and_after_completed_removal() -> io::Result<()> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("attachments");
        let task = uuid::Uuid::new_v4().to_string();
        let original = root.join(&task);
        let file = persist_prompt_file(&original, "notes.txt", b"keep")?;
        let intent = root.join(format!(".shepherd-deletion-intent-{task}"));
        fs::write(&intent, b"")?;
        recover_staged(&root, &BTreeSet::from([task.clone()]))?;
        assert!(!intent.exists());
        assert_eq!(fs::read(prompt_file(&original, &file)?)?, b"keep");
        let staged = stage_task(&original)?.ok_or_else(|| io::Error::other("missing staging"))?;
        remove_task(&staged)?;
        assert!(intent.is_file());
        recover_staged(&root, &BTreeSet::new())?;
        assert!(!intent.exists());
        Ok(())
    }

    #[test]
    fn resolves_an_image_using_the_cached_bytes() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let task = root.path().join("attachments/task");
        let image = include_bytes!("../icons/128x128.png");
        let limit = AttachmentLimit::from_bytes(crate::settings::DEFAULT_ATTACHMENT_BYTES);
        let id = persist_prompt_file(&task, "clipboard.data", image)?;
        let attachment = resolve_prompt_file(&task, &id, &limit)?;
        assert_eq!(attachment.image_mime_type.as_deref(), Some("image/png"));
        assert_eq!(fs::read(attachment.path)?, image);
        let id = persist_prompt_file(&task, "unrelated.png", b"plain text")?;
        assert_eq!(
            resolve_prompt_file(&task, &id, &limit)?.image_mime_type,
            None
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn attachment_cache_rejects_redirected_directories_and_files() -> io::Result<()> {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir()?;
        let outside = tempfile::tempdir()?;
        let cache = root.path().join("attachments");
        let task = cache.join("task");
        symlink(outside.path(), &cache)?;
        assert!(persist_prompt_file(&task, "redirect.txt", b"data").is_err());
        assert!(remove_task(&task).is_err());
        fs::remove_file(&cache)?;
        fs::create_dir(&cache)?;
        symlink(outside.path(), &task)?;
        assert!(persist_prompt_file(&task, "redirect.txt", b"data").is_err());
        assert!(remove_task(&task).is_err());
        assert_eq!(fs::read_dir(outside.path())?.count(), 0);
        fs::remove_file(&task)?;
        let id = persist_prompt_file(&task, "fixture.txt", b"data")?;
        let file = prompt_file(&task, &id)?;
        fs::remove_file(&file)?;
        let outside_file = outside.path().join("outside.txt");
        fs::write(&outside_file, b"outside")?;
        symlink(&outside_file, &file)?;
        assert!(prompt_file(&task, &id).is_err());
        assert!(persist_prompt_file(&task, "fixture.txt", b"data").is_err());
        assert_eq!(fs::read(outside_file)?, b"outside");
        fs::remove_file(&file)?;
        let file_directory = file
            .parent()
            .ok_or_else(|| invalid("Missing file directory."))?;
        fs::remove_dir(file_directory)?;
        symlink(outside.path(), file_directory)?;
        assert!(prompt_file(&task, &id).is_err());
        assert!(persist_prompt_file(&task, "fixture.txt", b"data").is_err());
        Ok(())
    }
}
