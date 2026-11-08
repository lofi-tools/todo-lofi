//! Filesystem capability: serve the agent's read/write requests, confined to
//! the session's roots.

use std::collections::{HashMap, hash_map::Entry};
use std::fs::File;
use std::io::{BufRead, BufReader, ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::schema::SessionId;

const MAX_READ_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

/// The directories a session is allowed to touch.
#[derive(Clone, Debug, Default)]
pub struct SessionRoots {
    roots: Vec<PathBuf>,
    initialization_errors: Vec<(PathBuf, Arc<std::io::Error>)>,
    #[cfg(unix)]
    root_handles: Vec<Option<Arc<std::os::fd::OwnedFd>>>,
}

impl SessionRoots {
    pub fn new(roots: impl IntoIterator<Item = PathBuf>) -> Self {
        let mut resolved_roots = Vec::new();
        let mut initialization_errors = Vec::new();
        #[cfg(unix)]
        let mut root_handles = Vec::new();
        for root in roots {
            #[cfg(unix)]
            let initial_metadata = std::fs::metadata(&root);
            match root.canonicalize() {
                Ok(resolved) => {
                    #[cfg(unix)]
                    {
                        use rustix::fs::{Mode, OFlags, fstat, open};
                        use std::os::unix::fs::MetadataExt;
                        let flags =
                            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
                        let opened = initial_metadata.and_then(|metadata| {
                            let handle = open(&resolved, flags, Mode::empty())
                                .map_err(std::io::Error::from)?;
                            let opened_metadata = fstat(&handle).map_err(std::io::Error::from)?;
                            if metadata.dev() != opened_metadata.st_dev as u64
                                || metadata.ino() != opened_metadata.st_ino as u64
                            {
                                return Err(std::io::Error::other(
                                    "session root changed while opening",
                                ));
                            }
                            Ok(handle)
                        });
                        match opened {
                            Ok(handle) => root_handles.push(Some(Arc::new(handle))),
                            Err(error) => {
                                initialization_errors.push((resolved.clone(), Arc::new(error)));
                                root_handles.push(None);
                            }
                        }
                    }
                    resolved_roots.push(resolved);
                }
                Err(error) => {
                    initialization_errors.push((root.clone(), Arc::new(error)));
                    resolved_roots.push(root);
                    #[cfg(unix)]
                    root_handles.push(None);
                }
            }
        }
        Self {
            roots: resolved_roots,
            initialization_errors,
            #[cfg(unix)]
            root_handles,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }

    /// Resolve a requested path inside the session's roots.
    ///
    /// Relative paths resolve against the first root. The result is
    /// canonicalized so `..` cannot escape, and paths that end up outside every
    /// root are refused rather than served.
    pub fn resolve(&self, requested: &Path) -> anyhow::Result<PathBuf> {
        if let Some((root, error)) = self.initialization_errors.first() {
            anyhow::bail!("could not resolve session root {}: {error}", root.display());
        }
        let absolute = if requested.is_absolute() {
            requested.to_path_buf()
        } else {
            let base = self
                .roots
                .first()
                .ok_or_else(|| anyhow::anyhow!("the session has no directory"))?;
            base.join(requested)
        };
        let resolved = canonicalize_allow_missing(&absolute)?;
        if self.roots.iter().any(|root| resolved.starts_with(root)) {
            Ok(resolved)
        } else {
            anyhow::bail!(
                "{} is outside the session's directories",
                requested.display()
            )
        }
    }

    pub(crate) fn read_text_file(
        &self,
        requested: &Path,
        line: Option<u32>,
        limit: Option<u32>,
    ) -> anyhow::Result<String> {
        let path = self.resolve(requested)?;
        #[cfg(unix)]
        {
            let (root, handle) = self.anchored_root(&path)?;
            let file = open_confined_file_at(root, handle, &path)?;
            read_selected_lines(file, line, limit).map_err(Into::into)
        }
        #[cfg(not(unix))]
        read_text_file(&path, line, limit)
    }

    pub(crate) fn write_text_file(&self, requested: &Path, content: &str) -> anyhow::Result<()> {
        let path = self.resolve(requested)?;
        #[cfg(unix)]
        {
            let (root, handle) = self.anchored_root(&path)?;
            let (directory, name) = open_confined_parent_at(root, handle, &path)?;
            write_confined_file(&directory, &name, content)
        }
        #[cfg(not(unix))]
        write_text_file(&path, content)
    }

    #[cfg(unix)]
    fn anchored_root(&self, path: &Path) -> anyhow::Result<(&Path, &std::os::fd::OwnedFd)> {
        let (index, root) = self
            .roots
            .iter()
            .enumerate()
            .find(|(_, root)| path.starts_with(root))
            .ok_or_else(|| {
                anyhow::anyhow!("{} is outside the session's directories", path.display())
            })?;
        let handle = self
            .root_handles
            .get(index)
            .and_then(Option::as_deref)
            .ok_or_else(|| anyhow::anyhow!("session root {} is unavailable", root.display()))?;
        Ok((root, handle))
    }
}

#[cfg(unix)]
fn open_confined_parent(
    root: &Path,
    path: &Path,
) -> anyhow::Result<(std::os::fd::OwnedFd, std::ffi::OsString)> {
    use rustix::fs::{Mode, OFlags, open};

    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let root_handle = open(root, flags, Mode::empty())?;
    open_confined_parent_at(root, &root_handle, path)
}

#[cfg(unix)]
fn open_confined_parent_at(
    root: &Path,
    root_handle: &std::os::fd::OwnedFd,
    path: &Path,
) -> anyhow::Result<(std::os::fd::OwnedFd, std::ffi::OsString)> {
    use rustix::fs::{Mode, OFlags, openat};

    let relative = path.strip_prefix(root)?;
    let parent = relative
        .parent()
        .ok_or_else(|| anyhow::anyhow!("file has no parent"))?;
    let name = relative
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("file has no name"))?;
    let directory_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut directory = openat(root_handle, ".", directory_flags, Mode::empty())?;
    for component in parent {
        directory = openat(&directory, component, directory_flags, Mode::empty())?;
    }
    Ok((directory, name.to_owned()))
}

#[cfg(unix)]
fn open_confined_file(root: &Path, path: &Path) -> anyhow::Result<File> {
    let (directory, name) = open_confined_parent(root, path)?;
    open_file_in(&directory, &name, path)
}

#[cfg(unix)]
fn open_confined_file_at(
    root: &Path,
    handle: &std::os::fd::OwnedFd,
    path: &Path,
) -> anyhow::Result<File> {
    let (directory, name) = open_confined_parent_at(root, handle, path)?;
    open_file_in(&directory, &name, path)
}

#[cfg(unix)]
fn open_file_in(
    directory: &std::os::fd::OwnedFd,
    name: &std::ffi::OsStr,
    path: &Path,
) -> anyhow::Result<File> {
    use rustix::fs::{Mode, OFlags, openat};
    let file = File::from(openat(
        directory,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )?);
    if !file.metadata()?.is_file() {
        anyhow::bail!("{} is not a regular file", path.display());
    }
    Ok(file)
}

#[cfg(unix)]
fn write_confined_file(
    directory: &std::os::fd::OwnedFd,
    name: &std::ffi::OsStr,
    content: &str,
) -> anyhow::Result<()> {
    use rustix::fs::{AtFlags, FileType, Mode, OFlags, fchmod, openat, renameat, statat, unlinkat};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMPORARY: AtomicU64 = AtomicU64::new(0);
    let mode = match statat(directory, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(metadata) if FileType::from_raw_mode(metadata.st_mode) == FileType::RegularFile => {
            Some(Mode::from_raw_mode(metadata.st_mode))
        }
        Ok(_) => anyhow::bail!("{} is not a regular file", name.to_string_lossy()),
        Err(rustix::io::Errno::NOENT) => None,
        Err(error) => return Err(error.into()),
    };
    let (temporary_name, mut temporary) = loop {
        let number = NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed);
        let temporary_name = format!(".shepherd-write-{}-{number}", std::process::id());
        match openat(
            directory,
            temporary_name.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::RUSR | Mode::WUSR,
        ) {
            Ok(file) => break (temporary_name, File::from(file)),
            Err(rustix::io::Errno::EXIST) => continue,
            Err(error) => return Err(error.into()),
        }
    };
    let outcome = (|| -> anyhow::Result<()> {
        if let Some(mode) = mode {
            fchmod(&temporary, mode)?;
        }
        temporary.write_all(content.as_bytes())?;
        renameat(directory, temporary_name.as_str(), directory, name)?;
        Ok(())
    })();
    if let Err(error) = outcome {
        return match unlinkat(directory, temporary_name.as_str(), AtFlags::empty()) {
            Ok(()) => Err(error),
            Err(cleanup) => {
                Err(error.context(format!("could not remove temporary file: {cleanup}")))
            }
        };
    }
    Ok(())
}

type AttachedFiles = HashMap<PathBuf, Arc<Mutex<File>>>;

#[derive(Clone, Default)]
pub(crate) struct PromptReads(Arc<Mutex<HashMap<SessionId, AttachedFiles>>>);

pub(crate) struct PromptReadGrant {
    reads: PromptReads,
    session: SessionId,
}

impl PromptReads {
    pub(crate) fn grant(
        &self,
        session: SessionId,
        paths: Vec<PathBuf>,
    ) -> anyhow::Result<PromptReadGrant> {
        let files = paths
            .into_iter()
            .map(|path| {
                let file = open_attachment(&path)?;
                Ok((path, Arc::new(Mutex::new(file))))
            })
            .collect::<anyhow::Result<AttachedFiles>>()?;
        let mut sessions = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("attachment access is unavailable"))?;
        match sessions.entry(session.clone()) {
            Entry::Vacant(entry) => {
                entry.insert(files);
                Ok(PromptReadGrant {
                    reads: self.clone(),
                    session,
                })
            }
            Entry::Occupied(_) => anyhow::bail!("a prompt already holds attachment access"),
        }
    }

    pub(crate) fn read(
        &self,
        session: &SessionId,
        path: &Path,
        line: Option<u32>,
        limit: Option<u32>,
    ) -> anyhow::Result<Option<String>> {
        let file = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("attachment access is unavailable"))?
            .get(session)
            .and_then(|files| files.get(path))
            .cloned();
        let Some(file) = file else {
            return Ok(None);
        };
        let mut file = file
            .lock()
            .map_err(|_| anyhow::anyhow!("attachment read is unavailable"))?;
        file.seek(SeekFrom::Start(0))?;
        let content = read_selected_lines(&mut *file, line, limit)
            .map_err(|error| anyhow::anyhow!("failed to read {}: {error}", path.display()))?;
        Ok(Some(content))
    }
}

impl Drop for PromptReadGrant {
    fn drop(&mut self) {
        self.reads
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.session);
    }
}

fn open_attachment(path: &Path) -> anyhow::Result<File> {
    if !path.is_absolute() || path.canonicalize()? != path || !path.symlink_metadata()?.is_file() {
        anyhow::bail!("{} is not a regular attachment file", path.display());
    }
    #[cfg(unix)]
    let file = open_confined_file(Path::new("/"), path)?;
    #[cfg(not(unix))]
    let file = std::fs::OpenOptions::new().read(true).open(path)?;
    // Retain the open file so replacing its path during the turn cannot redirect a read.
    if !file.metadata()?.is_file() || path.canonicalize()? != path {
        anyhow::bail!("{} is not a regular attachment file", path.display());
    }
    Ok(file)
}

/// Canonicalize a path that may not exist yet by canonicalizing its parent.
fn canonicalize_allow_missing(path: &Path) -> anyhow::Result<PathBuf> {
    match path.canonicalize() {
        Ok(resolved) => return Ok(resolved),
        Err(error) if error.kind() == ErrorKind::NotFound => {
            // A dangling symlink is an existing path with an unknown target,
            // not a missing file that can be created under its parent.
            match path.symlink_metadata() {
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                _ => anyhow::bail!("{} cannot be resolved: {error}", path.display()),
            }
        }
        Err(error) => anyhow::bail!("{} cannot be resolved: {error}", path.display()),
    }
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("{} has no parent directory", path.display()))?;
    let parent = parent
        .canonicalize()
        .map_err(|e| anyhow::anyhow!("{} is not accessible: {e}", parent.display()))?;
    Ok(match path.file_name() {
        Some(name) => parent.join(name),
        None => parent,
    })
}

/// Read a text file, optionally from `line` (1-based) for at most `limit` lines.
pub fn read_text_file(
    path: &Path,
    line: Option<u32>,
    limit: Option<u32>,
) -> anyhow::Result<String> {
    let file = File::open(path)
        .map_err(|error| anyhow::anyhow!("failed to open {}: {error}", path.display()))?;
    read_selected_lines(file, line, limit)
        .map_err(|error| anyhow::anyhow!("failed to read {}: {error}", path.display()))
}

fn read_selected_lines(
    input: impl Read,
    line: Option<u32>,
    limit: Option<u32>,
) -> std::io::Result<String> {
    if line.is_none() && limit.is_none() {
        let mut bytes = Vec::new();
        input
            .take((MAX_READ_RESPONSE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_READ_RESPONSE_BYTES {
            return Err(read_limit_error());
        }
        return String::from_utf8(bytes)
            .map_err(|error| std::io::Error::new(ErrorKind::InvalidData, error));
    }
    let mut reader = BufReader::new(input);
    for _ in 0..line.unwrap_or(1).saturating_sub(1) {
        if reader.skip_until(b'\n')? == 0 {
            return Ok(String::new());
        }
    }
    let mut output = String::new();
    let mut buffer = Vec::new();
    for index in 0..limit.unwrap_or(u32::MAX) {
        buffer.clear();
        let remaining = MAX_READ_RESPONSE_BYTES.saturating_sub(output.len());
        if (&mut reader)
            .take((remaining + 3) as u64)
            .read_until(b'\n', &mut buffer)?
            == 0
        {
            break;
        }
        if buffer.last() == Some(&b'\n') {
            buffer.pop();
            if buffer.last() == Some(&b'\r') {
                buffer.pop();
            }
        }
        let separator = usize::from(index > 0);
        if buffer.len().saturating_add(separator) > remaining {
            return Err(read_limit_error());
        }
        if index > 0 {
            output.push('\n');
        }
        output.push_str(
            std::str::from_utf8(&buffer)
                .map_err(|error| std::io::Error::new(ErrorKind::InvalidData, error))?,
        );
    }
    Ok(output)
}

fn read_limit_error() -> std::io::Error {
    std::io::Error::new(
        ErrorKind::InvalidData,
        "ACP file read result exceeds the 8 MiB limit; request a smaller line range",
    )
}

/// Write a text file, creating its parent directory when needed.
pub fn write_text_file(path: &Path, content: &str) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    match path.metadata() {
        Ok(metadata) if metadata.is_file() => temporary
            .as_file()
            .set_permissions(metadata.permissions())?,
        Ok(_) => anyhow::bail!("{} is not a regular file", path.display()),
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    temporary.write_all(content.as_bytes())?;
    temporary.persist(path).map_err(|error| {
        anyhow::anyhow!("failed to replace {}: {}", path.display(), error.error)
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn workspace_reads_and_writes_stay_on_open_root_after_replacement() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("workspace");
        let moved = directory.path().join("original");
        std::fs::create_dir(&root)?;
        std::fs::write(root.join("file"), "original")?;
        let roots = SessionRoots::new([root.clone()]);
        let resolved = roots.resolve(Path::new("file"))?;
        std::fs::rename(&root, &moved)?;
        std::fs::create_dir(&root)?;
        std::fs::write(root.join("file"), "replacement")?;

        let (root_path, handle) = roots.anchored_root(&resolved)?;
        let mut file = open_confined_file_at(root_path, handle, &resolved)?;
        let mut content = String::new();
        file.read_to_string(&mut content)?;
        assert_eq!(content, "original");
        let (parent, name) = open_confined_parent_at(root_path, handle, &resolved)?;
        write_confined_file(&parent, &name, "changed")?;
        assert_eq!(std::fs::read_to_string(moved.join("file"))?, "changed");
        assert_eq!(std::fs::read_to_string(root.join("file"))?, "replacement");
        Ok(())
    }

    #[test]
    fn unresolved_session_root_requires_a_new_session_after_repair() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let missing = directory.path().join("missing");
        let roots = SessionRoots::new([missing.clone()]);
        std::fs::create_dir(&missing)?;
        std::fs::write(missing.join("file"), "content")?;
        let error = roots
            .read_text_file(Path::new("file"), None, None)
            .err()
            .ok_or_else(|| anyhow::anyhow!("unresolved root was accepted"))?;
        assert!(error.to_string().contains("could not resolve session root"));
        let repaired = SessionRoots::new([missing]);
        assert_eq!(
            repaired.read_text_file(Path::new("file"), None, None)?,
            "content"
        );
        Ok(())
    }

    #[test]
    fn ranged_reads_honor_limit_without_a_start_line_and_stop_early() -> anyhow::Result<()> {
        struct OneChunk(Option<&'static [u8]>);
        impl Read for OneChunk {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                let bytes = self
                    .0
                    .take()
                    .ok_or_else(|| std::io::Error::other("read beyond requested range"))?;
                buffer[..bytes.len()].copy_from_slice(bytes);
                Ok(bytes.len())
            }
        }
        assert_eq!(
            read_selected_lines(OneChunk(Some(b"first\n")), None, Some(1))?,
            "first"
        );
        assert_eq!(
            read_selected_lines(&b"first\r\nsecond\nthird\n"[..], Some(2), Some(2))?,
            "second\nthird"
        );
        assert_eq!(
            read_selected_lines(&b"first\nsecond\n"[..], None, None)?,
            "first\nsecond\n"
        );
        assert_eq!(read_selected_lines(&b"first\n"[..], None, Some(0))?, "");
        Ok(())
    }

    #[test]
    fn file_reads_limit_responses_and_skip_large_unselected_lines() -> anyhow::Result<()> {
        let oversized = vec![b'x'; MAX_READ_RESPONSE_BYTES + 1];
        for (line, limit) in [(None, None), (Some(1), Some(1))] {
            let error = read_selected_lines(oversized.as_slice(), line, limit)
                .err()
                .ok_or_else(|| anyhow::anyhow!("oversized read unexpectedly succeeded"))?;
            assert!(error.to_string().contains("8 MiB limit"));
        }
        let mut content = oversized;
        content.extend_from_slice(b"\nsmall\n");
        assert_eq!(
            read_selected_lines(content.as_slice(), Some(2), Some(1))?,
            "small"
        );
        let exact = vec![b'y'; MAX_READ_RESPONSE_BYTES];
        assert_eq!(
            read_selected_lines(exact.as_slice(), None, None)?.len(),
            MAX_READ_RESPONSE_BYTES
        );
        let mut exact_line = exact.clone();
        exact_line.extend_from_slice(b"\r\n");
        assert_eq!(
            read_selected_lines(exact_line.as_slice(), Some(1), Some(1))?.len(),
            MAX_READ_RESPONSE_BYTES
        );
        let mut multiple_lines = b"a\n".to_vec();
        multiple_lines.extend_from_slice(&exact);
        assert!(read_selected_lines(multiple_lines.as_slice(), Some(1), None).is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn confined_write_stays_in_open_directory_after_symlink_swap() -> anyhow::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir()?;
        let outside = tempfile::tempdir()?;
        std::fs::create_dir(root.path().join("folder"))?;
        std::fs::write(root.path().join("folder/file"), "inside")?;
        std::fs::set_permissions(
            root.path().join("folder/file"),
            std::fs::Permissions::from_mode(0o640),
        )?;
        std::fs::write(outside.path().join("file"), "private")?;
        let roots = SessionRoots::new([root.path().to_owned()]);
        let resolved = roots.resolve(Path::new("folder/file"))?;
        let (parent, name) = open_confined_parent(&root.path().canonicalize()?, &resolved)?;
        std::fs::rename(root.path().join("folder"), root.path().join("old"))?;
        std::os::unix::fs::symlink(outside.path(), root.path().join("folder"))?;
        write_confined_file(&parent, &name, "updated\n")?;
        assert_eq!(
            std::fs::read_to_string(root.path().join("old/file"))?,
            "updated\n"
        );
        assert_eq!(
            std::fs::metadata(root.path().join("old/file"))?
                .permissions()
                .mode()
                & 0o777,
            0o640
        );
        assert_eq!(
            std::fs::read_to_string(outside.path().join("file"))?,
            "private"
        );
        assert!(
            roots
                .write_text_file(Path::new("folder/file"), "bad")
                .is_err()
        );
        roots.write_text_file(Path::new("old/new"), "new")?;
        assert_eq!(std::fs::read_to_string(root.path().join("old/new"))?, "new");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn confined_read_rejects_a_directory_replaced_after_resolution() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let outside = tempfile::tempdir()?;
        std::fs::create_dir(root.path().join("folder"))?;
        std::fs::write(root.path().join("folder/file"), "inside")?;
        std::fs::write(outside.path().join("file"), "private")?;
        let roots = SessionRoots::new([root.path().to_owned()]);
        assert_eq!(
            roots.read_text_file(Path::new("folder/file"), None, None)?,
            "inside"
        );
        let resolved = roots.resolve(Path::new("folder/file"))?;
        let canonical_root = root.path().canonicalize()?;
        std::fs::rename(root.path().join("folder"), root.path().join("old"))?;
        std::os::unix::fs::symlink(outside.path(), root.path().join("folder"))?;
        assert!(open_confined_file(&canonical_root, &resolved).is_err());
        assert!(
            roots
                .read_text_file(Path::new("folder/file"), None, None)
                .is_err()
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn attachment_open_refuses_a_substituted_parent() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().canonicalize()?;
        let outside = tempfile::tempdir()?;
        std::fs::create_dir(root.join("selected"))?;
        std::fs::write(root.join("selected/file"), "selected")?;
        std::fs::write(outside.path().join("file"), "private")?;
        let path = root.join("selected/file");
        std::fs::rename(root.join("selected"), root.join("old"))?;
        std::os::unix::fs::symlink(outside.path(), root.join("selected"))?;
        assert!(open_confined_file(Path::new("/"), &path).is_err());
        assert!(open_attachment(&path).is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn prompt_reads_hold_the_selected_file_and_expire_with_the_grant() -> anyhow::Result<()> {
        let cache = tempfile::tempdir()?;
        let cache_path = cache.path().canonicalize()?;
        let selected = cache_path.join("attached.txt");
        let neighbor = cache_path.join("other.txt");
        std::fs::write(&selected, "first\nsecond\nthird\n")?;
        std::fs::write(&neighbor, "private")?;
        let reads = PromptReads::default();
        let session = SessionId::new("attached");
        let other = SessionId::new("unrelated");
        let grant = reads.grant(session.clone(), vec![selected.clone()])?;
        assert!(reads.read(&other, &selected, None, None)?.is_none());
        assert!(reads.read(&session, &neighbor, None, None)?.is_none());

        std::fs::remove_file(&selected)?;
        std::os::unix::fs::symlink(&neighbor, &selected)?;
        assert_eq!(
            reads
                .read(&session, &selected, Some(2), Some(1))?
                .as_deref(),
            Some("second")
        );
        drop(grant);
        assert!(reads.read(&session, &selected, None, None)?.is_none());
        assert!(reads.grant(session, vec![selected]).is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_dangling_link_to_an_outside_file() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let outside = tempfile::tempdir()?;
        let target = outside.path().join("new-file");
        std::os::unix::fs::symlink(&target, root.path().join("link"))?;
        let roots = SessionRoots::new([root.path().to_owned()]);
        assert!(roots.resolve(Path::new("link")).is_err());
        assert!(!target.exists());
        Ok(())
    }

    #[test]
    fn replaces_file_content_with_the_exact_trailing_newlines() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let path = root.path().join("file");
        write_text_file(&path, "first\n")?;
        write_text_file(&path, "second\n\n")?;
        assert_eq!(std::fs::read_to_string(path)?, "second\n\n");
        Ok(())
    }
}
