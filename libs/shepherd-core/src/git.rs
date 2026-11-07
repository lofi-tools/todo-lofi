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
    #[error("worktree deletion identity does not match: {path:?}")]
    WorktreeIdentityMismatch { path: PathBuf },
    #[error("worktree has staged, unstaged or untracked changes: {path:?}")]
    DirtyWorktree { path: PathBuf },
    #[error("archived worktree contains ignored files: {path:?}")]
    IgnoredWorktreeFiles { path: PathBuf },
    #[error("task branch {branch} is missing and has no recorded tip")]
    MissingBranch { branch: String },
    #[error("git returned an invalid status record")]
    InvalidStatus,
    #[error("file is no longer in Git status: {path}")]
    UnknownPath { path: String },
    #[error("the unstaged diff changed or cannot be discarded; open it again before retrying")]
    DiscardChanged,
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

pub struct WorktreeDeletionLock(std::fs::File);

impl Drop for WorktreeDeletionLock {
    fn drop(&mut self) {
        if let Err(error) = self.0.unlock() {
            tracing::warn!(%error, "could not unlock worktree deletion journal");
        }
    }
}

impl GitService {
    pub fn primary_remote(path: &Path) -> Result<Option<String>, GitError> {
        let names = run_git(path, &["remote"])?;
        let mut remotes = names.lines();
        let name = remotes
            .clone()
            .find(|name| *name == "origin")
            .or_else(|| remotes.next());
        name.map(|name| run_git(path, &["remote", "get-url", "--", name]))
            .transpose()
    }

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

    pub fn common_directory(path: &Path) -> Result<PathBuf, GitError> {
        let directory = run_git(path, &["rev-parse", "--git-common-dir"])?;
        canonicalize(&path.join(directory))
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

    pub fn remove_archived_worktree(
        &self,
        repository: &GitRepository,
        path: &Path,
    ) -> Result<(), GitError> {
        self.validate_worktree(repository, path)?;
        if !git_bytes(
            path,
            &[
                "ls-files",
                "--others",
                "--ignored",
                "--exclude-standard",
                "--directory",
                "-z",
            ],
        )?
        .is_empty()
        {
            return Err(GitError::IgnoredWorktreeFiles {
                path: path.to_path_buf(),
            });
        }
        self.remove_clean_worktree(repository, path)
    }

    pub fn move_worktree(
        &self,
        repository: &GitRepository,
        source: &Path,
        destination: &Path,
    ) -> Result<(), GitError> {
        self.validate_worktree(repository, source)?;
        if destination.parent() != Some(repository.git_dir.join("shepherd/worktrees").as_path()) {
            return Err(GitError::InvalidWorktree {
                path: destination.to_path_buf(),
            });
        }
        match destination.symlink_metadata() {
            Ok(_) => {
                return Err(GitError::InvalidWorktree {
                    path: destination.to_path_buf(),
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(GitError::Io { source }),
        }
        let source = source.to_str().ok_or_else(|| GitError::InvalidWorktree {
            path: source.to_path_buf(),
        })?;
        let destination = destination
            .to_str()
            .ok_or_else(|| GitError::InvalidWorktree {
                path: destination.to_path_buf(),
            })?;
        run_git(
            &repository.root,
            &["worktree", "move", "--", source, destination],
        )?;
        Ok(())
    }

    pub fn mark_worktree_deletion(
        &self,
        repository: &GitRepository,
        path: &Path,
        identity: &str,
    ) -> Result<PathBuf, GitError> {
        let git_dir = self.worktree_git_dir(repository, path)?;
        let marker = deletion_marker(&git_dir, identity)?;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(marker)
            .map_err(|source| GitError::Io { source })?;
        Ok(git_dir)
    }

    pub fn lock_worktree_deletions(
        &self,
        repository: &GitRepository,
    ) -> Result<WorktreeDeletionLock, GitError> {
        let directory = repository.git_dir.join("shepherd");
        std::fs::create_dir_all(&directory).map_err(|source| GitError::Io { source })?;
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(directory.join("worktree-deletions.lock"))
            .map_err(|source| GitError::Io { source })?;
        lock.try_lock().map_err(|error| GitError::Io {
            source: std::io::Error::other(format!("could not lock worktree deletions: {error}")),
        })?;
        Ok(WorktreeDeletionLock(lock))
    }

    pub fn verify_worktree_deletion(
        &self,
        repository: &GitRepository,
        path: &Path,
        expected_git_dir: &Path,
        identity: &str,
    ) -> Result<(), GitError> {
        let git_dir = self.worktree_git_dir(repository, path)?;
        if git_dir != expected_git_dir {
            return Err(GitError::WorktreeIdentityMismatch {
                path: path.to_path_buf(),
            });
        }
        let marker = deletion_marker(&git_dir, identity)?;
        if !marker
            .symlink_metadata()
            .map_err(|source| GitError::Io { source })?
            .is_file()
        {
            return Err(GitError::WorktreeIdentityMismatch {
                path: path.to_path_buf(),
            });
        }
        Ok(())
    }

    pub fn clear_worktree_deletion_marker(
        &self,
        repository: &GitRepository,
        path: &Path,
        expected_git_dir: &Path,
        identity: &str,
    ) -> Result<(), GitError> {
        self.verify_worktree_deletion(repository, path, expected_git_dir, identity)?;
        std::fs::remove_file(deletion_marker(expected_git_dir, identity)?)
            .map_err(|source| GitError::Io { source })
    }

    fn worktree_git_dir(
        &self,
        repository: &GitRepository,
        path: &Path,
    ) -> Result<PathBuf, GitError> {
        self.validate_worktree(repository, path)?;
        let git_dir = canonicalize(Path::new(&run_git(
            path,
            &["rev-parse", "--absolute-git-dir"],
        )?))?;
        if git_dir.parent() != Some(repository.git_dir.join("worktrees").as_path()) {
            return Err(GitError::InvalidWorktree {
                path: path.to_path_buf(),
            });
        }
        Ok(git_dir)
    }

    pub fn prune_worktrees(&self, repository: &GitRepository) -> Result<(), GitError> {
        run_git(&repository.root, &["worktree", "prune"])?;
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
        let ancestor = self.review_base(path, base)?;
        let changed = git_bytes(path, &["diff", "--name-only", "-z", &ancestor, "--"])?;
        let mut files = self.status(path)?;
        for name in changed
            .split(|byte| *byte == 0)
            .filter(|name| !name.is_empty())
        {
            let display_name = String::from_utf8_lossy(name);
            if !files.iter().any(|file| file.path == display_name) {
                files.push(GitFile {
                    path: display_name.into_owned(),
                    original_path: None,
                    actionable: std::str::from_utf8(name).is_ok(),
                    index: " ".into(),
                    worktree: " ".into(),
                });
            }
        }
        Ok(ReviewSnapshot { files })
    }

    fn review_base(&self, path: &Path, base: &str) -> Result<String, GitError> {
        let base_ref = format!("refs/heads/{base}");
        run_git(path, &["merge-base", "HEAD", &base_ref])
    }

    pub fn review_file_diff(
        &self,
        path: &Path,
        base: &str,
        name: &str,
    ) -> Result<String, GitError> {
        let review = self.review(path, base)?;
        let file = review
            .files
            .into_iter()
            .find(|file| file.actionable && file.path == name)
            .ok_or_else(|| GitError::UnknownPath { path: name.into() })?;
        if file.index == "?" && file.worktree == "?" {
            let output = Command::new("git")
                .arg("-C")
                .arg(path)
                .args([
                    "diff",
                    "--no-index",
                    "--no-color",
                    "--no-ext-diff",
                    "--no-textconv",
                    "--",
                    "/dev/null",
                    &file.path,
                ])
                .output()
                .map_err(|source| GitError::Io { source })?;
            return match output.status.code() {
                Some(0 | 1) => Ok(String::from_utf8_lossy(&output.stdout).into_owned()),
                _ => Err(GitError::CommandFailed {
                    command: "git diff --no-index".into(),
                    status: output.status,
                    stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                }),
            };
        }
        let ancestor = self.review_base(path, base)?;
        let mut args = vec![
            "--literal-pathspecs",
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            &ancestor,
            "--",
            &file.path,
        ];
        args.extend(file.original_path.as_deref());
        let bytes = git_bytes(path, &args)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    pub fn file_diff(&self, path: &Path, name: &str) -> Result<GitFileDiff, GitError> {
        let file = self.require_changed_path(path, name)?;
        let mut args = vec![
            "--literal-pathspecs",
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--",
            &file.path,
        ];
        args.extend(file.original_path.as_deref());
        let staged = {
            let mut staged_args = args.clone();
            staged_args.insert(2, "--cached");
            git_bytes(path, &staged_args)?
        };
        let unstaged = git_bytes(path, &args)?;
        args.insert(2, "--binary");
        let full_diff = git_bytes(path, &args)?;
        let revision = Sha256::digest(full_diff)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let conflict = ["DD", "AU", "UD", "UA", "DU", "AA", "UU"]
            .contains(&format!("{}{}", file.index, file.worktree).as_str());
        Ok(GitFileDiff {
            staged: String::from_utf8_lossy(&staged).into_owned(),
            unstaged: String::from_utf8_lossy(&unstaged).into_owned(),
            revision,
            can_discard: !conflict && matches!(file.worktree.as_str(), "M" | "D" | "T"),
        })
    }

    pub fn discard_unstaged(
        &self,
        path: &Path,
        file: &str,
        expected: &str,
    ) -> Result<(), GitError> {
        let diff = self.file_diff(path, file)?;
        if !diff.can_discard || diff.revision != expected {
            return Err(GitError::DiscardChanged);
        }
        run_git(
            path,
            &["--literal-pathspecs", "restore", "--worktree", "--", file],
        )?;
        Ok(())
    }

    pub fn checkout_status(&self, path: &Path) -> Result<CheckoutStatus, GitError> {
        let head = run_git(path, &["rev-parse", "--abbrev-ref", "HEAD"])?;
        let branch = (head != "HEAD").then_some(head);
        let tracking = if let Some(branch) = &branch {
            let reference = format!("refs/heads/{branch}");
            let summary = run_git(
                path,
                &[
                    "for-each-ref",
                    "--format=%(upstream:short) %(upstream:track)",
                    &reference,
                ],
            )?;
            (!summary.is_empty()).then_some(summary)
        } else {
            None
        };
        Ok(CheckoutStatus {
            branch,
            tracking,
            files: self.status(path)?,
        })
    }

    pub fn snapshot(&self, path: &Path) -> Result<GitSnapshot, GitError> {
        let checkout = self.checkout_status(path)?;
        let branches = run_git(
            path,
            &["for-each-ref", "--format=%(refname:short)", "refs/heads/"],
        )?;
        Ok(GitSnapshot {
            branch: checkout.branch,
            tracking: checkout.tracking,
            project_root: None,
            staged_revision: self.staged_revision(path)?,
            merge_blocked: None,
            branches: branches.lines().map(str::to_string).collect(),
            files: checkout.files,
            history: self.history(path)?,
            staged_diff: run_git(
                path,
                &["diff", "--cached", "--no-ext-diff", "--no-textconv", "--"],
            )?,
            unstaged_diff: run_git(path, &["diff", "--no-ext-diff", "--no-textconv", "--"])?,
        })
    }

    fn history(&self, path: &Path) -> Result<Vec<GitCommit>, GitError> {
        let bytes = git_bytes(
            path,
            &["log", "-z", "-n", "50", "--format=%H%x00%an%x00%ar%x00%s"],
        )?;
        let fields = bytes.split(|byte| *byte == 0).collect::<Vec<_>>();
        Ok(fields
            .as_chunks::<4>()
            .0
            .iter()
            .map(|[id, author, time, subject]| GitCommit {
                id: String::from_utf8_lossy(id).into_owned(),
                author: String::from_utf8_lossy(author).into_owned(),
                time: String::from_utf8_lossy(time).into_owned(),
                subject: String::from_utf8_lossy(subject).into_owned(),
            })
            .collect())
    }

    pub fn stage(&self, path: &Path, file: Option<&str>) -> Result<(), GitError> {
        let mut args = vec!["--literal-pathspecs", "add", "-A", "--"];
        let entry = file
            .map(|file| self.require_changed_path(path, file))
            .transpose()?;
        if let Some(entry) = &entry {
            args.push(&entry.path);
            args.extend(entry.original_path.as_deref());
        } else {
            args.push(".");
        }
        run_git(path, &args)?;
        Ok(())
    }

    pub fn unstage(&self, path: &Path, file: &str) -> Result<(), GitError> {
        let entry = self.require_changed_path(path, file)?;
        let mut args = vec![
            "--literal-pathspecs",
            "restore",
            "--staged",
            "--",
            &entry.path,
        ];
        args.extend(entry.original_path.as_deref());
        run_git(path, &args)?;
        Ok(())
    }

    fn require_changed_path(&self, path: &Path, file: &str) -> Result<GitFile, GitError> {
        self.status(path)?
            .into_iter()
            .find(|entry| entry.actionable && entry.path == file)
            .ok_or_else(|| GitError::UnknownPath {
                path: file.to_string(),
            })
    }

    pub fn commit(&self, path: &Path, message: &str, expected: &str) -> Result<(), GitError> {
        if message.trim().is_empty() {
            return Err(GitError::EmptyCommitMessage);
        }
        if self.staged_revision(path)? != expected {
            return Err(GitError::StagingChanged);
        }
        commit_with_message(path, message, false)
    }

    pub fn amend(&self, path: &Path, message: &str, expected: &str) -> Result<(), GitError> {
        if message.trim().is_empty() {
            return Err(GitError::EmptyCommitMessage);
        }
        if self.staged_revision(path)? != expected {
            return Err(GitError::StagingChanged);
        }
        run_git(path, &["symbolic-ref", "--quiet", "HEAD"])?;
        commit_with_message(path, message, true)
    }

    fn staged_revision(&self, path: &Path) -> Result<String, GitError> {
        let mut hash = Sha256::new();
        hash.update(git_bytes(path, &["rev-parse", "HEAD"])?);
        hash.update(git_bytes(
            path,
            &["rev-parse", "--symbolic-full-name", "HEAD"],
        )?);
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

    pub fn create_branch(&self, path: &Path, branch: &str) -> Result<(), GitError> {
        if self.has_changes(path)? {
            return Err(GitError::DirtyWorktree {
                path: path.to_path_buf(),
            });
        }
        let reference = format!("refs/heads/{branch}");
        run_git(path, &["check-ref-format", &reference])?;
        run_git(path, &["switch", "-c", branch, "--"])?;
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

    pub fn retain_task_tip(
        &self,
        repository: &GitRepository,
        task_id: &str,
        tip: &str,
    ) -> Result<(), GitError> {
        let reference = format!("refs/shepherd/tasks/{task_id}");
        run_git(&repository.root, &["update-ref", &reference, tip])?;
        Ok(())
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
        commit_with_message(root, message, false)
    }

    pub(crate) fn status(&self, path: &Path) -> Result<Vec<GitFile>, GitError> {
        Ok(self.status_with_branch(path)?.1)
    }

    pub(crate) fn status_with_branch(
        &self,
        path: &Path,
    ) -> Result<(Option<String>, Vec<GitFile>), GitError> {
        let bytes = git_bytes(
            path,
            &[
                "--no-optional-locks",
                "status",
                "--porcelain=v2",
                "--branch",
                "--no-ahead-behind",
                "-z",
                "--renames",
                "--untracked-files=all",
            ],
        )?;
        let mut records = bytes
            .split(|byte| *byte == 0)
            .filter(|record| !record.is_empty());
        let mut files = Vec::new();
        let mut branch = None;
        while let Some(record) = records.next() {
            let (columns, original) = match record.first() {
                Some(b'#') => {
                    if let Some(head) = record.strip_prefix(b"# branch.head ")
                        && head != b"(detached)"
                    {
                        branch = Some(String::from_utf8_lossy(head).into_owned());
                    }
                    continue;
                }
                Some(b'1') => (9, None),
                Some(b'2') => (10, Some(records.next().ok_or(GitError::InvalidStatus)?)),
                Some(b'u') => (11, None),
                Some(b'?') => {
                    let path = record.strip_prefix(b"? ").ok_or(GitError::InvalidStatus)?;
                    files.push(GitFile::from_status(path, None, b"??")?);
                    continue;
                }
                _ => return Err(GitError::InvalidStatus),
            };
            let mut fields = record.splitn(columns, |byte| *byte == b' ');
            fields.next();
            let codes = fields.next().ok_or(GitError::InvalidStatus)?;
            let path = fields.nth(columns - 3).ok_or(GitError::InvalidStatus)?;
            files.push(GitFile::from_status(path, original, codes)?);
        }
        Ok((branch, files))
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
    pub original_path: Option<String>,
    pub actionable: bool,
    pub index: String,
    pub worktree: String,
}

impl GitFile {
    fn from_status(path: &[u8], original: Option<&[u8]>, codes: &[u8]) -> Result<Self, GitError> {
        let [index, worktree] = codes else {
            return Err(GitError::InvalidStatus);
        };
        let code = |byte| {
            if byte == b'.' {
                " ".to_string()
            } else {
                char::from(byte).to_string()
            }
        };
        Ok(Self {
            path: String::from_utf8_lossy(path).into_owned(),
            original_path: original.map(|path| String::from_utf8_lossy(path).into_owned()),
            actionable: std::str::from_utf8(path).is_ok()
                && original.is_none_or(|path| std::str::from_utf8(path).is_ok()),
            index: code(*index),
            worktree: code(*worktree),
        })
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewSnapshot {
    pub files: Vec<GitFile>,
}

#[derive(Debug, Serialize)]
pub struct GitCommit {
    pub id: String,
    pub author: String,
    pub time: String,
    pub subject: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitFileDiff {
    pub staged: String,
    pub unstaged: String,
    pub revision: String,
    pub can_discard: bool,
}

#[derive(Debug, Serialize)]
pub struct CheckoutStatus {
    pub branch: Option<String>,
    pub tracking: Option<String>,
    pub files: Vec<GitFile>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitSnapshot {
    pub branch: Option<String>,
    pub tracking: Option<String>,
    pub project_root: Option<CheckoutStatus>,
    pub branches: Vec<String>,
    pub history: Vec<GitCommit>,
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
    Amend { message: String, expected: String },
    Discard { path: String, expected: String },
    Checkout { branch: String },
    CreateBranch { branch: String },
    SquashMerge,
}

fn commit_with_message(path: &Path, message: &str, amend: bool) -> Result<(), GitError> {
    let mut arguments = vec!["commit", "-F", "-"];
    if amend {
        arguments.push("--amend");
    }
    let mut child = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(&arguments)
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
            command: format!("git {}", arguments.join(" ")),
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

fn deletion_marker(git_dir: &Path, identity: &str) -> Result<PathBuf, GitError> {
    let id = uuid::Uuid::parse_str(identity).map_err(|_| GitError::WorktreeIdentityMismatch {
        path: git_dir.to_path_buf(),
    })?;
    if id.to_string() != identity {
        return Err(GitError::WorktreeIdentityMismatch {
            path: git_dir.to_path_buf(),
        });
    }
    Ok(git_dir.join(format!("shepherd-deletion-{id}")))
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

    #[cfg(unix)]
    #[test]
    fn file_staging_and_commit_use_git_and_its_hooks() -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;

        let fixture = TestRepository::new()?;
        let hook = fixture.repository().git_dir.join("hooks/pre-commit");
        std::fs::write(&hook, "#!/bin/sh\nprintf ran > hook-ran\n")?;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755))?;
        fixture.write_file("README.md", "edited\n")?;
        fixture.write_file("other.txt", "leave untracked\n")?;

        GitService.stage(fixture.path(), Some("README.md"))?;
        let staged = GitService.snapshot(fixture.path())?;
        assert!(staged.staged_diff.contains("+edited"));
        GitService.unstage(fixture.path(), "README.md")?;
        assert!(GitService.snapshot(fixture.path())?.staged_diff.is_empty());
        assert!(matches!(
            GitService.commit(fixture.path(), "Wrong index", &staged.staged_revision),
            Err(GitError::StagingChanged),
        ));

        GitService.stage(fixture.path(), Some("README.md"))?;
        let staged = GitService.snapshot(fixture.path())?;
        GitService.commit(fixture.path(), "Edit readme", &staged.staged_revision)?;
        assert_eq!(
            run_git(fixture.path(), &["log", "-1", "--format=%s"])?,
            "Edit readme"
        );
        assert_eq!(
            std::fs::read_to_string(fixture.path().join("hook-ran"))?,
            "ran"
        );
        assert_eq!(
            run_git(fixture.path(), &["ls-files", "--", "other.txt"])?,
            ""
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn a_rejected_commit_keeps_the_index_available_for_retry()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;

        let fixture = TestRepository::new()?;
        let hook = fixture.repository().git_dir.join("hooks/commit-msg");
        std::fs::write(&hook, "#!/bin/sh\ncat \"$1\" >&2\nexit 1\n")?;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755))?;
        fixture.write_file("README.md", "edited\n")?;
        GitService.stage(fixture.path(), Some("README.md"))?;
        let snapshot = GitService.snapshot(fixture.path())?;
        let result = GitService.commit(
            fixture.path(),
            "Preserve this message",
            &snapshot.staged_revision,
        );
        assert!(
            matches!(result, Err(GitError::CommandFailed { stderr, .. }) if stderr.contains("Preserve this message"))
        );
        assert_eq!(
            GitService.snapshot(fixture.path())?.staged_revision,
            snapshot.staged_revision
        );
        Ok(())
    }

    fn task_worktree(fixture: &TestRepository) -> Result<PathBuf, GitError> {
        let repository = fixture.repository();
        let path = repository.git_dir.join("shepherd/worktrees/example");
        GitService.create_worktree(repository, &path, "shepherd/example", "main")?;
        Ok(path)
    }

    #[test]
    fn squash_merge_commits_to_the_base_and_keeps_the_task_branch()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = TestRepository::new()?;
        let task = task_worktree(&fixture)?;
        let original = run_git(fixture.path(), &["rev-parse", "HEAD"])?;
        std::fs::write(task.join("README.md"), "task content\n")?;
        GitService.stage(&task, None)?;
        GitService.commit(
            &task,
            "Task commit",
            &GitService.snapshot(&task)?.staged_revision,
        )?;
        let tip = run_git(&task, &["rev-parse", "HEAD"])?;

        GitService.squash_merge(
            fixture.path(),
            &task,
            "main",
            "shepherd/example",
            "Task result",
        )?;
        assert_eq!(
            std::fs::read_to_string(fixture.path().join("README.md"))?,
            "task content\n"
        );
        assert_eq!(
            run_git(fixture.path(), &["log", "-1", "--format=%P"])?,
            original
        );
        assert_eq!(
            run_git(fixture.path(), &["log", "-1", "--format=%s"])?,
            "Task result"
        );
        assert_eq!(
            run_git(
                fixture.path(),
                &["rev-parse", "refs/heads/shepherd/example"]
            )?,
            tip
        );
        Ok(())
    }

    #[test]
    fn squash_merge_refuses_dirty_and_off_base_project_roots()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = TestRepository::new()?;
        let task = task_worktree(&fixture)?;
        fixture.write_file("untracked.txt", "keep me")?;
        let merge = || {
            GitService.squash_merge(
                fixture.path(),
                &task,
                "main",
                "shepherd/example",
                "Task result",
            )
        };
        assert!(
            matches!(merge(), Err(GitError::MergeUnavailable { reason }) if reason.contains("uncommitted"))
        );
        assert_eq!(
            std::fs::read_to_string(fixture.path().join("untracked.txt"))?,
            "keep me"
        );
        std::fs::remove_file(fixture.path().join("untracked.txt"))?;
        run_git(fixture.path(), &["switch", "-c", "off-base"])?;
        assert!(
            matches!(merge(), Err(GitError::MergeUnavailable { reason }) if reason.contains("Switch the project root to main"))
        );
        assert!(!GitService.has_changes(fixture.path())?);
        Ok(())
    }

    #[test]
    fn squash_merge_lists_conflicts_and_leaves_the_base_tip_alone()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = TestRepository::new()?;
        let task = task_worktree(&fixture)?;
        std::fs::write(task.join("README.md"), "task edit\n")?;
        run_git(&task, &["commit", "-am", "Task edit"])?;
        fixture.write_file("README.md", "base edit\n")?;
        run_git(fixture.path(), &["commit", "-am", "Base edit"])?;
        let base_tip = run_git(fixture.path(), &["rev-parse", "HEAD"])?;

        let result = GitService.squash_merge(
            fixture.path(),
            &task,
            "main",
            "shepherd/example",
            "Task result",
        );
        assert!(
            matches!(result, Err(GitError::MergeConflicts { paths, .. }) if paths == "README.md")
        );
        assert_eq!(run_git(fixture.path(), &["rev-parse", "HEAD"])?, base_tip);
        assert_eq!(
            run_git(fixture.path(), &["diff", "--name-only", "--diff-filter=U"])?,
            "README.md"
        );
        Ok(())
    }

    #[test]
    fn repairs_the_missing_checkout_without_touching_other_registrations()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = TestRepository::new()?;
        let path = task_worktree(&fixture)?;
        let other = fixture
            .repository()
            .git_dir
            .join("shepherd/worktrees/other");
        GitService.create_worktree(fixture.repository(), &other, "shepherd/other", "main")?;
        std::fs::remove_dir_all(&path)?;
        std::fs::remove_dir_all(&other)?;
        let tip = run_git(
            fixture.path(),
            &["rev-parse", "refs/heads/shepherd/example"],
        )?;

        GitService.restore_worktree(fixture.repository(), &path, "shepherd/example", None)?;
        assert_eq!(run_git(&path, &["rev-parse", "HEAD"])?, tip);
        let list = run_git(fixture.path(), &["worktree", "list", "--porcelain"])?;
        assert!(list.contains(other.to_str().ok_or("non-UTF-8 worktree path")?));
        assert!(!other.exists());
        Ok(())
    }

    #[test]
    fn refuses_to_remove_a_worktree_containing_an_untracked_file()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = TestRepository::new()?;
        let path = task_worktree(&fixture)?;
        let untracked = path.join("draft.txt");
        std::fs::write(&untracked, "keep this draft")?;
        assert!(matches!(
            GitService.remove_clean_worktree(fixture.repository(), &path),
            Err(GitError::DirtyWorktree { .. }),
        ));
        assert_eq!(std::fs::read_to_string(&untracked)?, "keep this draft");

        std::fs::remove_file(&untracked)?;
        GitService.remove_clean_worktree(fixture.repository(), &path)?;
        assert!(!path.exists());
        assert!(
            run_git(
                fixture.path(),
                &["show-ref", "--verify", "refs/heads/shepherd/example"]
            )
            .is_ok()
        );
        Ok(())
    }

    #[test]
    fn archived_collection_preserves_ignored_files() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = TestRepository::new()?;
        let path = task_worktree(&fixture)?;
        std::fs::write(
            fixture.repository().git_dir.join("info/exclude"),
            "ignored.txt\n",
        )?;
        let ignored = path.join("ignored.txt");
        std::fs::write(&ignored, "keep this data")?;
        assert!(!GitService.has_changes(&path)?);
        assert!(matches!(
            GitService.remove_archived_worktree(fixture.repository(), &path),
            Err(GitError::IgnoredWorktreeFiles { .. })
        ));
        assert_eq!(std::fs::read_to_string(&ignored)?, "keep this data");
        std::fs::remove_file(ignored)?;
        GitService.remove_archived_worktree(fixture.repository(), &path)?;
        assert!(!path.exists());
        Ok(())
    }

    #[test]
    fn moving_a_worktree_preserves_ignored_files_and_registration()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = TestRepository::new()?;
        let repository = fixture.repository();
        let lock = GitService.lock_worktree_deletions(repository)?;
        assert!(GitService.lock_worktree_deletions(repository).is_err());
        drop(lock);
        let _lock = GitService.lock_worktree_deletions(repository)?;
        let original = task_worktree(&fixture)?;
        let staged = repository.git_dir.join("shepherd/worktrees/staged-fixture");
        std::fs::write(repository.git_dir.join("info/exclude"), "ignored.txt\n")?;
        std::fs::write(original.join("ignored.txt"), "keep me")?;
        assert!(!GitService.has_changes(&original)?);
        let identity = uuid::Uuid::new_v4().to_string();
        let git_dir = GitService.mark_worktree_deletion(repository, &original, &identity)?;
        GitService.move_worktree(repository, &original, &staged)?;
        assert!(!original.exists());
        assert_eq!(
            std::fs::read_to_string(staged.join("ignored.txt"))?,
            "keep me"
        );
        GitService.validate_worktree(repository, &staged)?;
        GitService.verify_worktree_deletion(repository, &staged, &git_dir, &identity)?;
        assert!(
            GitService
                .verify_worktree_deletion(
                    repository,
                    &staged,
                    &git_dir,
                    &uuid::Uuid::new_v4().to_string()
                )
                .is_err()
        );
        assert!(
            GitService
                .verify_worktree_deletion(
                    repository,
                    &staged,
                    repository.git_dir.as_path(),
                    &identity
                )
                .is_err()
        );
        assert!(
            GitService
                .move_worktree(
                    repository,
                    &staged,
                    fixture.path().join("outside").as_path()
                )
                .is_err()
        );
        std::fs::create_dir(&original)?;
        assert!(
            GitService
                .move_worktree(repository, &staged, &original)
                .is_err()
        );
        std::fs::remove_dir(&original)?;
        GitService.move_worktree(repository, &staged, &original)?;
        assert_eq!(
            std::fs::read_to_string(original.join("ignored.txt"))?,
            "keep me"
        );
        GitService.validate_worktree(repository, &original)?;
        GitService.clear_worktree_deletion_marker(repository, &original, &git_dir, &identity)?;
        assert!(
            GitService
                .verify_worktree_deletion(repository, &original, &git_dir, &identity)
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn a_recorded_tip_can_restore_a_worktree_after_its_branch_is_deleted()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = TestRepository::new()?;
        let repository = fixture.repository();
        let path = task_worktree(&fixture)?;
        std::fs::write(path.join("README.md"), "archived content\n")?;
        run_git(&path, &["commit", "-am", "Keep this result"])?;
        let tip = GitService.checked_out_tip(&path, "shepherd/example")?;

        GitService.retain_task_tip(repository, "example-id", &tip)?;
        GitService.remove_clean_worktree(repository, &path)?;
        run_git(fixture.path(), &["branch", "-D", "shepherd/example"])?;
        assert_eq!(
            run_git(
                fixture.path(),
                &["rev-parse", "refs/shepherd/tasks/example-id"]
            )?,
            tip
        );
        GitService.restore_worktree(repository, &path, "shepherd/example", Some(&tip))?;
        assert_eq!(
            std::fs::read_to_string(path.join("README.md"))?,
            "archived content\n"
        );
        assert_eq!(GitService.checked_out_tip(&path, "shepherd/example")?, tip);
        Ok(())
    }

    #[test]
    fn status_reports_a_rename_and_unstages_both_paths() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = TestRepository::new()?;
        run_git(fixture.path(), &["mv", "README.md", "new name.md"])?;
        let status = GitService.snapshot(fixture.path())?;
        let [file] = status.files.as_slice() else {
            return Err("expected one rename".into());
        };
        assert_eq!(file.index, "R");
        assert_eq!(file.path, "new name.md");
        assert_eq!(file.original_path.as_deref(), Some("README.md"));
        GitService.unstage(fixture.path(), &file.path)?;
        assert!(GitService.snapshot(fixture.path())?.staged_diff.is_empty());
        Ok(())
    }

    #[test]
    fn a_non_repository_is_reported() {
        let directory = TestDirectory::new().expect("temp directory");
        let error = GitService::discover(directory.path()).expect_err("not a repository");
        assert!(matches!(error, GitError::CommandFailed { .. }));
    }
}
