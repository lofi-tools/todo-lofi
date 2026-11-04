//! Test-only directories and repositories. Every helper owns a temporary path,
//! so a test can never write into the real application data directory.

use std::path::{Path, PathBuf};

use crate::git::{GitError, GitRepository, GitService, run_git};

#[derive(Debug, thiserror::Error)]
pub enum FixtureError {
    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),
    #[error("git error: {0}")]
    Git(#[from] GitError),
    #[error("fixture path must be relative: {path}")]
    AbsolutePath { path: String },
}

pub struct TestDirectory {
    directory: tempfile::TempDir,
}

impl TestDirectory {
    pub fn new() -> Result<Self, FixtureError> {
        Ok(Self {
            directory: tempfile::tempdir()?,
        })
    }

    pub fn path(&self) -> &Path {
        self.directory.path()
    }

    pub fn join(&self, path: impl AsRef<Path>) -> PathBuf {
        self.path().join(path)
    }
}

pub struct TestRepository {
    directory: TestDirectory,
    repository: GitRepository,
}

impl TestRepository {
    pub fn new() -> Result<Self, FixtureError> {
        let directory = TestDirectory::new()?;
        run_git(directory.path(), &["init", "-b", "main"])?;
        run_git(directory.path(), &["config", "user.name", "Shepherd Test"])?;
        run_git(
            directory.path(),
            &["config", "user.email", "shepherd@example.com"],
        )?;
        std::fs::write(directory.join("README.md"), "initial\n")?;
        run_git(directory.path(), &["add", "README.md"])?;
        run_git(directory.path(), &["commit", "-m", "initial"])?;
        let repository = GitService::discover(directory.path())?;
        Ok(Self {
            directory,
            repository,
        })
    }

    pub fn path(&self) -> &Path {
        self.directory.path()
    }

    pub fn repository(&self) -> &GitRepository {
        &self.repository
    }

    pub fn write_file(&self, path: impl AsRef<Path>, contents: &str) -> Result<(), FixtureError> {
        let relative = path.as_ref();
        if relative.is_absolute() {
            return Err(FixtureError::AbsolutePath {
                path: relative.display().to_string(),
            });
        }
        let path = self.directory.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, contents)?;
        Ok(())
    }
}

pub struct TestDatabase {
    directory: TestDirectory,
}

impl TestDatabase {
    pub fn new() -> Result<Self, FixtureError> {
        Ok(Self {
            directory: TestDirectory::new()?,
        })
    }

    pub fn path(&self) -> &Path {
        self.directory.path()
    }

    pub fn uri(&self) -> String {
        format!("turso:{}", self.directory.join("shepherd.db").display())
    }
}
