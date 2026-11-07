//! Filesystem capability: serve the agent's read/write requests, confined to
//! the session's roots.

use std::collections::{HashMap, hash_map::Entry};
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::schema::SessionId;

/// The directories a session is allowed to touch.
#[derive(Clone, Debug, Default)]
pub struct SessionRoots {
    roots: Vec<PathBuf>,
}

impl SessionRoots {
    pub fn new(roots: impl IntoIterator<Item = PathBuf>) -> Self {
        Self {
            roots: roots
                .into_iter()
                .map(|root| root.canonicalize().unwrap_or(root))
                .collect(),
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
            let root = self
                .roots
                .iter()
                .find(|root| path.starts_with(root))
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "{} is outside the session's directories",
                        requested.display()
                    )
                })?;
            let mut file = open_confined_file(root, &path)?;
            let mut content = String::new();
            file.read_to_string(&mut content)?;
            Ok(select_lines(content, line, limit))
        }
        #[cfg(not(unix))]
        read_text_file(&path, line, limit)
    }

    pub(crate) fn write_text_file(&self, requested: &Path, content: &str) -> anyhow::Result<()> {
        let path = self.resolve(requested)?;
        #[cfg(unix)]
        {
            let root = self
                .roots
                .iter()
                .find(|root| path.starts_with(root))
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "{} is outside the session's directories",
                        requested.display()
                    )
                })?;
            let (directory, name) = open_confined_parent(root, &path)?;
            write_confined_file(&directory, &name, content)
        }
        #[cfg(not(unix))]
        write_text_file(&path, content)
    }
}

#[cfg(unix)]
fn open_confined_parent(
    root: &Path,
    path: &Path,
) -> anyhow::Result<(std::os::fd::OwnedFd, std::ffi::OsString)> {
    use rustix::fs::{Mode, OFlags, open, openat};

    let relative = path.strip_prefix(root)?;
    let parent = relative
        .parent()
        .ok_or_else(|| anyhow::anyhow!("file has no parent"))?;
    let name = relative
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("file has no name"))?;
    let directory_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut directory = open(root, directory_flags, Mode::empty())?;
    for component in parent {
        directory = openat(&directory, component, directory_flags, Mode::empty())?;
    }
    Ok((directory, name.to_owned()))
}

#[cfg(unix)]
fn open_confined_file(root: &Path, path: &Path) -> anyhow::Result<File> {
    use rustix::fs::{Mode, OFlags, openat};
    let (directory, name) = open_confined_parent(root, path)?;
    let file = File::from(openat(
        &directory,
        &name,
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
        let mut content = String::new();
        file.seek(SeekFrom::Start(0))?;
        file.read_to_string(&mut content)
            .map_err(|error| anyhow::anyhow!("failed to read {}: {error}", path.display()))?;
        Ok(Some(select_lines(content, line, limit)))
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
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    // Retain the open file so replacing its path during the turn cannot redirect a read.
    let file = options.open(path)?;
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
    let content = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", path.display()))?;
    Ok(select_lines(content, line, limit))
}

fn select_lines(content: String, line: Option<u32>, limit: Option<u32>) -> String {
    let Some(start) = line else {
        return content;
    };
    let start = start.saturating_sub(1) as usize;
    let lines: Vec<&str> = content.lines().collect();
    let end = match limit {
        Some(limit) => start.saturating_add(limit as usize).min(lines.len()),
        None => lines.len(),
    };
    if start >= lines.len() {
        return String::new();
    }
    lines[start..end].join("\n")
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
