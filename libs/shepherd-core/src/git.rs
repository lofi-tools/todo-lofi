//! Git operations, kept behind one boundary so the core never parses Git
//! output at call sites.
//!
//! The user's `git` binary is authoritative: hooks, signing, sparse checkout and
//! worktree bookkeeping must behave as they do in a terminal.

use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitRepository {
    pub root: PathBuf,
    pub git_dir: PathBuf,
    pub default_branch: String,
}

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("not a Git repository: {path}")]
    NotARepository { path: String },
    #[error("failed to run git: {source}")]
    Io {
        #[source]
        source: std::io::Error,
    },
    #[error("git command failed ({command}, status {status:?}): {stderr}")]
    CommandFailed {
        command: String,
        status: std::process::ExitStatus,
        stderr: String,
    },
    #[error("not a Shepherd worktree: {path:?}")]
    InvalidWorktree { path: PathBuf },
    #[error("task branch {branch} is missing and has no recorded tip")]
    MissingBranch { branch: String },
    #[error("Git is in detached HEAD state at {path}")]
    DetachedHead { path: String },
}

#[derive(Clone, Copy, Debug, Default)]
pub struct GitService;

impl GitService {
    /// Resolve the repository root, absolute Git directory and current branch.
    pub fn discover(path: &Path) -> Result<GitRepository, GitError> {
        let root = run_git(path, &["rev-parse", "--show-toplevel"])?;
        let git_dir = run_git(path, &["rev-parse", "--absolute-git-dir"])?;
        Ok(GitRepository {
            root: canonicalize(Path::new(&root))?,
            git_dir: canonicalize(Path::new(&git_dir))?,
            default_branch: default_branch(path)?,
        })
    }

    /// Create a task worktree from the repository's default branch. The caller
    /// resolves the slug and path; this function only performs the Git mutation.
    pub fn create_worktree(
        &self,
        repository: &GitRepository,
        worktree_path: &Path,
        branch: &str,
        base_branch: &str,
    ) -> Result<(), GitError> {
        if let Some(parent) = worktree_path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| GitError::Io { source })?;
        }
        run_git(
            &repository.root,
            &[
                "worktree",
                "add",
                "-b",
                branch,
                &worktree_path.display().to_string(),
                base_branch,
            ],
        )?;
        Ok(())
    }

    pub fn restore_worktree(
        &self,
        repository: &GitRepository,
        path: &Path,
        branch: &str,
        recorded_tip: Option<&str>,
    ) -> Result<(), GitError> {
        if path.parent() != Some(repository.git_dir.join("shepherd/worktrees").as_path()) {
            return Err(GitError::InvalidWorktree { path: path.to_path_buf() });
        }
        match path.symlink_metadata() {
            Ok(_) => return self.validate_worktree(repository, path),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(GitError::Io { source }),
        }
        let path_text = path.to_str().ok_or_else(|| GitError::InvalidWorktree {
            path: path.to_path_buf(),
        })?;
        let registered = run_git(&repository.root, &["worktree", "list", "--porcelain", "-z"])?;
        if registered.split('\0').any(|field| field.strip_prefix("worktree ") == Some(path_text)) {
            run_git(&repository.root, &["worktree", "remove", "--", path_text])?;
        }
        let reference = format!("refs/heads/{branch}");
        if run_git(&repository.root, &["show-ref", "--verify", "--quiet", &reference]).is_ok() {
            run_git(&repository.root, &["worktree", "add", "--", path_text, branch])?;
        } else if let Some(tip) = recorded_tip {
            self.create_worktree(repository, path, branch, tip)?;
        } else {
            return Err(GitError::MissingBranch { branch: branch.to_string() });
        }
        Ok(())
    }

    pub fn validate_worktree(&self, repository: &GitRepository, path: &Path) -> Result<(), GitError> {
        let invalid = || GitError::InvalidWorktree { path: path.to_path_buf() };
        if !path.starts_with(repository.git_dir.join("shepherd/worktrees"))
            || path.symlink_metadata().map_err(|source| GitError::Io { source })?.is_symlink()
        {
            return Err(invalid());
        }
        let root = run_git(path, &["rev-parse", "--show-toplevel"])?;
        let common = |cwd: &Path| -> Result<PathBuf, GitError> {
            let value = run_git(cwd, &["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
            canonicalize(Path::new(&value))
        };
        if canonicalize(Path::new(&root))? != canonicalize(path)?
            || common(path)? != common(&repository.root)?
        {
            return Err(invalid());
        }
        Ok(())
    }

    /// True when the worktree has staged, unstaged or untracked changes.
    pub fn has_changes(&self, worktree_path: &Path) -> Result<bool, GitError> {
        let status = run_git(worktree_path, &["status", "--porcelain"])?;
        Ok(!status.is_empty())
    }
}

/// The branch a new worktree starts from. A recorded remote HEAD wins, then
/// the conventional names, and only then whatever happens to be checked out:
/// tasks must not branch off an unrelated feature checkout.
fn default_branch(path: &Path) -> Result<String, GitError> {
    if let Ok(reference) = run_git(
        path,
        &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
    ) && let Some(name) = reference.strip_prefix("refs/remotes/origin/")
    {
        return Ok(name.to_string());
    }
    for candidate in ["main", "master"] {
        let reference = format!("refs/heads/{candidate}");
        if run_git(path, &["rev-parse", "--verify", "--quiet", &reference]).is_ok() {
            return Ok(candidate.to_string());
        }
    }
    run_git(path, &["symbolic-ref", "--short", "HEAD"]).map_err(|_| GitError::DetachedHead {
        path: path.display().to_string(),
    })
}

fn canonicalize(path: &Path) -> Result<PathBuf, GitError> {
    path.canonicalize()
        .map_err(|source| GitError::Io { source })
}

pub(crate) fn run_git(cwd: &Path, args: &[&str]) -> Result<String, GitError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .map_err(|source| GitError::Io { source })?;
    if !output.status.success() {
        return Err(GitError::CommandFailed {
            command: format!("git -C {} {}", cwd.display(), args.join(" ")),
            status: output.status,
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestDirectory, TestRepository};

    #[test]
    fn discovers_the_repository_and_creates_a_worktree_under_the_git_dir() {
        let fixture = TestRepository::new().expect("repository fixture");
        let repository = fixture.repository();
        assert_eq!(repository.default_branch, "main");
        let expected_git_dir = fixture
            .path()
            .canonicalize()
            .expect("canonical repository path")
            .join(".git");
        assert_eq!(repository.git_dir, expected_git_dir);

        let worktree = repository
            .git_dir
            .join("shepherd")
            .join("worktrees")
            .join("task-1");
        GitService
            .create_worktree(repository, &worktree, "shepherd/task-1", "main")
            .expect("worktree should be created");

        assert!(worktree.join("README.md").is_file());
        let list =
            run_git(fixture.path(), &["worktree", "list", "--porcelain"]).expect("worktree list");
        assert!(list.contains(&worktree.display().to_string()));
    }

    #[test]
    fn a_worktree_starts_from_the_default_branch_not_the_checked_out_one() {
        let fixture = TestRepository::new().expect("repository fixture");
        run_git(fixture.path(), &["checkout", "-b", "feature"]).expect("feature branch");
        fixture
            .write_file("NOTES.md", "feature only\n")
            .expect("write change");
        run_git(fixture.path(), &["add", "NOTES.md"]).expect("stage change");
        run_git(fixture.path(), &["commit", "-m", "feature commit"]).expect("commit change");

        let repository = GitService::discover(fixture.path()).expect("discover on feature");
        assert_eq!(repository.default_branch, "main");

        let worktree = repository
            .git_dir
            .join("shepherd")
            .join("worktrees")
            .join("task-2");
        GitService
            .create_worktree(
                &repository,
                &worktree,
                "shepherd/task-2",
                &repository.default_branch,
            )
            .expect("worktree should be created from main");

        assert!(!worktree.join("NOTES.md").exists());
        let head = run_git(&worktree, &["rev-parse", "HEAD"]).expect("worktree head");
        let main = run_git(fixture.path(), &["rev-parse", "main"]).expect("main head");
        assert_eq!(head, main);
    }

    #[test]
    fn a_clean_worktree_has_no_changes_until_a_file_is_written() {
        let fixture = TestRepository::new().expect("repository fixture");
        let repository = fixture.repository();
        assert!(
            !GitService
                .has_changes(&repository.root)
                .expect("status should run")
        );

        fixture
            .write_file("README.md", "changed\n")
            .expect("write change");
        assert!(
            GitService
                .has_changes(&repository.root)
                .expect("status should run")
        );
    }

    #[test]
    fn a_non_repository_is_reported() {
        let directory = TestDirectory::new().expect("temp directory");
        let error = GitService::discover(directory.path()).expect_err("not a repository");
        assert!(matches!(error, GitError::CommandFailed { .. }));
    }
}
