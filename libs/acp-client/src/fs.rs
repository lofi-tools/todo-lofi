//! Filesystem capability: serve the agent's read/write requests, confined to
//! the session's roots.

use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

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
    let Some(start) = line else {
        return Ok(content);
    };
    let start = start.saturating_sub(1) as usize;
    let lines: Vec<&str> = content.lines().collect();
    let end = match limit {
        Some(limit) => (start + limit as usize).min(lines.len()),
        None => lines.len(),
    };
    if start >= lines.len() {
        return Ok(String::new());
    }
    Ok(lines[start..end].join("\n"))
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
