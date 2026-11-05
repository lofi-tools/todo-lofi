//! Git operations, kept behind one boundary so the core never parses Git
//! output at call sites.
//!
//! The user's `git` binary is authoritative: hooks, signing, sparse checkout and
//! worktree bookkeeping must behave as they do in a terminal.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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
    #[error("worktree has staged, unstaged or untracked changes: {path:?}")]
    DirtyWorktree { path: PathBuf },
    #[error("task branch {branch} is missing and has no recorded tip")]
    MissingBranch { branch: String },
    #[error("file is no longer in Git status: {path}")]
    UnknownPath { path: String },
    #[error("enter a commit message")]
    EmptyCommitMessage,
    #[error("staging changed; refresh Git status before committing")]
    StagingChanged,
    #[error("check out the task branch {branch} before collecting its worktree")]
    UnexpectedBranch { branch: String },
    #[error("local branch does not exist: {branch}")]
    UnknownBranch { branch: String },
    #[error("squash merge is unavailable: {reason}")]
    MergeUnavailable { reason: String },
    #[error(
        "squash merge has conflicts in: {paths}. Resolve them in a project terminal.\n{detail}"
    )]
    MergeConflicts { paths: String, detail: String },
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
            return Err(GitError::InvalidWorktree {
                path: path.to_path_buf(),
            });
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
        if registered
            .split('\0')
            .any(|field| field.strip_prefix("worktree ") == Some(path_text))
        {
            run_git(&repository.root, &["worktree", "remove", "--", path_text])?;
        }
        let reference = format!("refs/heads/{branch}");
        if run_git(
            &repository.root,
            &["show-ref", "--verify", "--quiet", &reference],
        )
        .is_ok()
        {
            run_git(
                &repository.root,
                &["worktree", "add", "--", path_text, branch],
            )?;
        } else if let Some(tip) = recorded_tip {
            self.create_worktree(repository, path, branch, tip)?;
        } else {
            return Err(GitError::MissingBranch {
                branch: branch.to_string(),
            });
        }
        Ok(())
    }

    pub fn validate_worktree(
        &self,
        repository: &GitRepository,
        path: &Path,
    ) -> Result<(), GitError> {
        let invalid = || GitError::InvalidWorktree {
            path: path.to_path_buf(),
        };
        if path.parent() != Some(repository.git_dir.join("shepherd/worktrees").as_path())
            || path
                .symlink_metadata()
                .map_err(|source| GitError::Io { source })?
                .is_symlink()
        {
            return Err(invalid());
        }
        let root = run_git(path, &["rev-parse", "--show-toplevel"])?;
        let common = |cwd: &Path| -> Result<PathBuf, GitError> {
            let value = run_git(
                cwd,
                &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            )?;
            canonicalize(Path::new(&value))
        };
        if canonicalize(Path::new(&root))? != canonicalize(path)?
            || common(path)? != common(&repository.root)?
        {
            return Err(invalid());
        }
        Ok(())
    }

    pub fn remove_clean_worktree(
        &self,
        repository: &GitRepository,
        path: &Path,
    ) -> Result<(), GitError> {
        self.validate_worktree(repository, path)?;
        if self.has_changes(path)? {
            return Err(GitError::DirtyWorktree {
                path: path.to_path_buf(),
            });
        }
        let path_text = path.to_str().ok_or_else(|| GitError::InvalidWorktree {
            path: path.to_path_buf(),
        })?;
        run_git(&repository.root, &["worktree", "remove", "--", path_text])?;
        Ok(())
    }

    pub fn turn_baseline(&self, path: &Path) -> Result<TurnBaseline, GitError> {
        let status = git_bytes(
            path,
            &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        )?;
        let tracked = |cached| {
            let mut args = vec!["diff", "--no-ext-diff", "--no-textconv", "--binary"];
            if cached {
                args.push("--cached");
            }
            git_bytes(path, &args).map(|bytes| Sha256::digest(bytes).into())
        };
        let mut untracked = Vec::new();
        let names = git_bytes(path, &["ls-files", "--others", "--exclude-standard", "-z"])?;
        for relative in names
            .split(|byte| *byte == 0)
            .filter(|name| !name.is_empty())
        {
            #[cfg(unix)]
            let relative = {
                use std::os::unix::ffi::OsStrExt;
                std::ffi::OsStr::from_bytes(relative)
            };
            #[cfg(not(unix))]
            let relative = std::str::from_utf8(relative).map_err(|source| GitError::Io {
                source: std::io::Error::other(source),
            })?;
            let file_path = path.join(relative);
            let mut hash = Sha256::new();
            let hash_file = || -> std::io::Result<[u8; 32]> {
                let metadata = file_path.symlink_metadata()?;
                if metadata.is_symlink() {
                    hash.update(b"symlink");
                    hash.update(
                        std::fs::read_link(&file_path)?
                            .as_os_str()
                            .as_encoded_bytes(),
                    );
                } else if metadata.is_file() {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        hash.update((metadata.permissions().mode() & 0o111).to_le_bytes());
                    }
                    let mut file = std::fs::File::open(&file_path)?;
                    let mut buffer = [0; 8192];
                    loop {
                        let count = file.read(&mut buffer)?;
                        if count == 0 {
                            break;
                        }
                        hash.update(&buffer[..count]);
                    }
                }
                Ok(hash.finalize().into())
            };
            untracked.push(hash_file().map_err(|source| GitError::Io { source })?);
        }
        Ok(TurnBaseline {
            head: run_git(path, &["rev-parse", "HEAD"])?,
            branch: run_git(path, &["rev-parse", "--symbolic-full-name", "HEAD"])?,
            staged: tracked(true)?,
            unstaged: tracked(false)?,
            status,
            untracked,
        })
    }

    pub fn review(&self, path: &Path, base: &str) -> Result<ReviewSnapshot, GitError> {
        let base_ref = format!("refs/heads/{base}");
        let ancestor = run_git(path, &["merge-base", "HEAD", &base_ref])?;
        let changed = git_bytes(path, &["diff", "--name-only", "-z", &ancestor, "--"])?;
        let mut files = self.status(path)?;
        for name in git_paths(&changed) {
            if !files.iter().any(|file| file.path == name) {
                files.push(GitFile {
                    path: name,
                    index: " ".into(),
                    worktree: " ".into(),
                });
            }
        }
        let mut diff = run_git(
            path,
            &["diff", "--no-ext-diff", "--no-textconv", &ancestor, "--"],
        )?;
        for file in files
            .iter()
            .filter(|file| file.index == "?" && file.worktree == "?")
        {
            let output = Command::new("git")
                .arg("-C")
                .arg(path)
                .args([
                    "diff",
                    "--no-index",
                    "--no-ext-diff",
                    "--no-textconv",
                    "--",
                    "/dev/null",
                    &file.path,
                ])
                .output()
                .map_err(|source| GitError::Io { source })?;
            if output.status.code() == Some(1) {
                diff.push('\n');
                diff.push_str(&String::from_utf8_lossy(&output.stdout));
            } else if !output.status.success() {
                return Err(GitError::CommandFailed {
                    command: "git diff --no-index".into(),
                    status: output.status,
                    stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                });
            }
        }
        Ok(ReviewSnapshot { files, diff })
    }

    pub fn snapshot(&self, path: &Path) -> Result<GitSnapshot, GitError> {
        let head = run_git(path, &["rev-parse", "--abbrev-ref", "HEAD"])?;
        let branches = run_git(
            path,
            &["for-each-ref", "--format=%(refname:short)", "refs/heads/"],
        )?;
        Ok(GitSnapshot {
            branch: (head != "HEAD").then_some(head),
            staged_revision: self.staged_revision(path)?,
            merge_blocked: None,
            branches: branches.lines().map(str::to_string).collect(),
            files: self.status(path)?,
            staged_diff: run_git(
                path,
                &["diff", "--cached", "--no-ext-diff", "--no-textconv", "--"],
            )?,
            unstaged_diff: run_git(path, &["diff", "--no-ext-diff", "--no-textconv", "--"])?,
        })
    }

    pub fn stage(&self, path: &Path, file: Option<&str>) -> Result<(), GitError> {
        if let Some(file) = file {
            self.require_changed_path(path, file)?;
        }
        run_git(
            path,
            &[
                "--literal-pathspecs",
                "add",
                "-A",
                "--",
                file.unwrap_or("."),
            ],
        )?;
        Ok(())
    }

    pub fn unstage(&self, path: &Path, file: &str) -> Result<(), GitError> {
        self.require_changed_path(path, file)?;
        run_git(
            path,
            &["--literal-pathspecs", "restore", "--staged", "--", file],
        )?;
        Ok(())
    }

    fn require_changed_path(&self, path: &Path, file: &str) -> Result<(), GitError> {
        if self.status(path)?.iter().any(|entry| entry.path == file) {
            Ok(())
        } else {
            Err(GitError::UnknownPath {
                path: file.to_string(),
            })
        }
    }

    pub fn commit(&self, path: &Path, message: &str, expected: &str) -> Result<(), GitError> {
        if message.trim().is_empty() {
            return Err(GitError::EmptyCommitMessage);
        }
        if self.staged_revision(path)? != expected {
            return Err(GitError::StagingChanged);
        }
        commit_with_message(path, message)
    }

    fn staged_revision(&self, path: &Path) -> Result<String, GitError> {
        let mut hash = Sha256::new();
        hash.update(git_bytes(path, &["rev-parse", "HEAD"])?);
        hash.update(git_bytes(path, &["ls-files", "--stage", "-z"])?);
        Ok(hash
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect())
    }

    pub fn checkout(&self, path: &Path, branch: &str) -> Result<(), GitError> {
        if self.has_changes(path)? {
            return Err(GitError::DirtyWorktree {
                path: path.to_path_buf(),
            });
        }
        let reference = format!("refs/heads/{branch}");
        if run_git(path, &["show-ref", "--verify", "--quiet", &reference]).is_err() {
            return Err(GitError::UnknownBranch {
                branch: branch.into(),
            });
        }
        run_git(path, &["switch", "--no-guess", "--", branch])?;
        Ok(())
    }

    pub fn merge_blocked(
        &self,
        root: &Path,
        path: &Path,
        base: &str,
        branch: &str,
    ) -> Result<Option<String>, GitError> {
        let current = run_git(root, &["rev-parse", "--abbrev-ref", "HEAD"])?;
        if current != base {
            return Ok(Some(format!("Switch the project root to {base} first.")));
        }
        if self.has_changes(root)? {
            return Ok(Some("The project root has uncommitted changes.".into()));
        }
        if self.has_changes(path)? {
            return Ok(Some("Commit the task worktree changes first.".into()));
        }
        let current = run_git(path, &["rev-parse", "--abbrev-ref", "HEAD"])?;
        if current != branch || branch == base {
            return Ok(Some(
                "Check out the task branch in its worktree first.".into(),
            ));
        }
        let base_tip = run_git(root, &["rev-parse", "HEAD"])?;
        let task_tip = run_git(path, &["rev-parse", "HEAD"])?;
        if run_git(root, &["merge-base", &base_tip, &task_tip])? == task_tip {
            return Ok(Some("The task branch has no commits to merge.".into()));
        }
        Ok(None)
    }

    pub fn checked_out_tip(&self, path: &Path, branch: &str) -> Result<String, GitError> {
        let reference = format!("refs/heads/{branch}");
        if run_git(path, &["symbolic-ref", "--quiet", "HEAD"])? != reference {
            return Err(GitError::UnexpectedBranch {
                branch: branch.into(),
            });
        }
        run_git(path, &["rev-parse", "--verify", "HEAD"])
    }

    pub fn squash_merge(
        &self,
        root: &Path,
        path: &Path,
        base: &str,
        branch: &str,
        message: &str,
    ) -> Result<(), GitError> {
        if let Some(reason) = self.merge_blocked(root, path, base, branch)? {
            return Err(GitError::MergeUnavailable { reason });
        }
        let reference = format!("refs/heads/{branch}");
        if let Err(error) = run_git(root, &["merge", "--squash", "--", &reference]) {
            let conflicts = git_bytes(root, &["diff", "--name-only", "--diff-filter=U", "-z"])?;
            let paths = git_paths(&conflicts).collect::<Vec<_>>().join(", ");
            if !paths.is_empty() {
                return Err(GitError::MergeConflicts {
                    paths,
                    detail: error.to_string(),
                });
            }
            return Err(error);
        }
        commit_with_message(root, message)
    }

    fn status(&self, path: &Path) -> Result<Vec<GitFile>, GitError> {
        let bytes = git_bytes(
            path,
            &[
                "status",
                "--porcelain=v1",
                "-z",
                "--no-renames",
                "--untracked-files=all",
            ],
        )?;
        Ok(bytes
            .split(|byte| *byte == 0)
            .filter_map(|entry| {
                let [index, worktree, b' ', name @ ..] = entry else {
                    return None;
                };
                Some(GitFile {
                    path: String::from_utf8_lossy(name).into_owned(),
                    index: char::from(*index).to_string(),
                    worktree: char::from(*worktree).to_string(),
                })
            })
            .collect())
    }

    /// True when the worktree has staged, unstaged or untracked changes.
    pub fn has_changes(&self, worktree_path: &Path) -> Result<bool, GitError> {
        let status = run_git(worktree_path, &["status", "--porcelain"])?;
        Ok(!status.is_empty())
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitFile {
    pub path: String,
    pub index: String,
    pub worktree: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewSnapshot {
    pub files: Vec<GitFile>,
    pub diff: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitSnapshot {
    pub branch: Option<String>,
    pub branches: Vec<String>,
    pub files: Vec<GitFile>,
    pub staged_diff: String,
    pub staged_revision: String,
    pub merge_blocked: Option<String>,
    pub unstaged_diff: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum GitMutation {
    Stage { path: Option<String> },
    Unstage { path: String },
    Commit { message: String, expected: String },
    Checkout { branch: String },
    SquashMerge,
}

fn commit_with_message(path: &Path, message: &str) -> Result<(), GitError> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["commit", "-F", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| GitError::Io { source })?;
    let write_result = child
        .stdin
        .take()
        .ok_or_else(|| std::io::Error::other("git commit has no stdin"))
        .and_then(|mut stdin| stdin.write_all(message.as_bytes()));
    let output = child
        .wait_with_output()
        .map_err(|source| GitError::Io { source })?;
    if !output.status.success() {
        return Err(GitError::CommandFailed {
            command: "git commit -F -".into(),
            status: output.status,
            stderr: format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ),
        });
    }
    write_result.map_err(|source| GitError::Io { source })
}

fn git_paths(bytes: &[u8]) -> impl Iterator<Item = String> + '_ {
    bytes
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
        .map(|name| String::from_utf8_lossy(name).into_owned())
}

#[derive(PartialEq, Eq)]
pub struct TurnBaseline {
    head: String,
    branch: String,
    staged: [u8; 32],
    unstaged: [u8; 32],
    status: Vec<u8>,
    untracked: Vec<[u8; 32]>,
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
    Ok(String::from_utf8_lossy(&git_bytes(cwd, args)?)
        .trim()
        .to_string())
}

fn git_bytes(cwd: &Path, args: &[&str]) -> Result<Vec<u8>, GitError> {
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
    Ok(output.stdout)
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
    fn baseline_detects_branch_switch_and_commit_without_file_changes()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = TestRepository::new()?;
        let path = fixture.path();
        let before_switch = GitService.turn_baseline(path)?;
        run_git(path, &["switch", "-c", "another-branch"])?;
        let before_commit = GitService.turn_baseline(path)?;
        assert!(before_switch != before_commit);
        run_git(
            path,
            &["commit", "--allow-empty", "-m", "Record a decision"],
        )?;
        assert!(before_commit != GitService.turn_baseline(path)?);
        assert!(!GitService.has_changes(path)?);
        Ok(())
    }

    #[test]
    fn a_non_repository_is_reported() {
        let directory = TestDirectory::new().expect("temp directory");
        let error = GitService::discover(directory.path()).expect_err("not a repository");
        assert!(matches!(error, GitError::CommandFailed { .. }));
    }
}
