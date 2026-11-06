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
    let file = fs::File::open(&file_path)?;
    let bytes = file.metadata()?.len();
    limit.check(bytes)?;
    let mut header = Vec::with_capacity(12);
    file.take(12).read_to_end(&mut header)?;
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
    let regular_file = || {
        path.symlink_metadata()
            .is_ok_and(|metadata| metadata.is_file())
    };
    if regular_file() {
        return Ok(());
    }
    let parent = path
        .parent()
        .ok_or_else(|| invalid("Missing attachment directory."))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    if let Err(error) = temporary.persist_noclobber(path)
        && (error.error.kind() != ErrorKind::AlreadyExists || !regular_file())
    {
        return Err(error.error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        Ok(())
    }
}
