use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub(crate) async fn read_optional(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match tokio::fs::read(path).await {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub(crate) fn backup_path(path: &Path) -> PathBuf {
    let mut backup = path.as_os_str().to_owned();
    backup.push(".bak");
    backup.into()
}

pub(crate) fn write_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("Missing settings directory."))?;
    std::fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(contents)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path)?;
    Ok(())
}

pub(crate) fn encode_json(document: &serde_json::Value) -> serde_json::Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(document)?;
    bytes.push(b'\n');
    Ok(bytes)
}
