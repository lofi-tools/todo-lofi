use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Serialize;
use shepherd_core::files::{self, FileChange, WorkspaceGitStatus};
use shepherd_core::git::{GitError, GitService};
use tokio::sync::watch;
use tokio::time::{Instant, sleep_until};

const DEBOUNCE: Duration = Duration::from_millis(150);
const MAX_DEBOUNCE: Duration = Duration::from_secs(1);

#[derive(Debug, thiserror::Error)]
pub(crate) enum WatchError {
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("{0}")]
    Git(#[from] GitError),
    #[error("{0}")]
    Files(#[from] files::FileError),
    #[error("{0}")]
    Notify(#[from] notify::Error),
    #[error("{0}")]
    Join(#[from] tokio::task::JoinError),
    #[error("{0}")]
    Notification(String),
    #[error("The file watcher stopped.")]
    Closed,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum FileWatchUpdate {
    Changes {
        files: Vec<FileChange>,
        git: Option<WorkspaceGitStatus>,
    },
    Error {
        message: String,
    },
}

pub(crate) struct WorkspaceWatch {
    _watcher: RecommendedWatcher,
    root: PathBuf,
    changed: watch::Sender<Option<String>>,
    notifications: watch::Receiver<Option<String>>,
}

struct WatchPaths {
    root: PathBuf,
    git_dir: PathBuf,
    common_dir: PathBuf,
}

impl WorkspaceWatch {
    #[cfg(test)]
    pub(crate) fn inject_error(&self, message: &str) {
        record_notification(&self.changed, Some(message.into()));
    }

    pub(crate) fn start(root: PathBuf) -> Result<Self, WatchError> {
        let root = root.canonicalize()?;
        let (git_dir, common_dir) = if has_git_marker(&root)? {
            (
                GitService::discover(&root)?.git_dir,
                GitService::common_directory(&root)?,
            )
        } else {
            (root.join(".git"), root.join(".git"))
        };
        let paths = WatchPaths {
            root: root.clone(),
            git_dir: git_dir.clone(),
            common_dir: common_dir.clone(),
        };
        let (changed, notifications) = watch::channel(None);
        let callback_changed = changed.clone();
        let mut watcher = RecommendedWatcher::new(
            move |event: notify::Result<Event>| match event {
                Ok(event) if paths.relevant(&event) => {
                    record_notification(&callback_changed, None);
                }
                Err(error) => {
                    record_notification(&callback_changed, Some(error.to_string()));
                }
                Ok(_) => {}
            },
            Config::default().with_follow_symlinks(false),
        )?;
        watcher.watch(&root, RecursiveMode::Recursive)?;
        for (path, mode) in [
            (git_dir, RecursiveMode::NonRecursive),
            (common_dir.clone(), RecursiveMode::NonRecursive),
            (common_dir.join("refs"), RecursiveMode::Recursive),
            (common_dir.join("info"), RecursiveMode::NonRecursive),
        ] {
            if !path.starts_with(&root) && watch_directory_exists(&path)? {
                watcher.watch(&path, mode)?;
            }
        }
        Ok(Self {
            _watcher: watcher,
            root,
            changed,
            notifications,
        })
    }

    pub(crate) async fn snapshot(&self) -> Result<FileWatchUpdate, WatchError> {
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || {
            if has_git_marker(&root)? {
                let (git, files) = files::git_file_status(&root)?;
                Ok(FileWatchUpdate::Changes {
                    files,
                    git: Some(git),
                })
            } else {
                Ok(FileWatchUpdate::Changes {
                    files: Vec::new(),
                    git: None,
                })
            }
        })
        .await?
    }

    pub(crate) async fn next_refresh(&mut self) -> Result<(), WatchError> {
        self.notifications
            .changed()
            .await
            .map_err(|_| WatchError::Closed)?;
        let ceiling = Instant::now() + MAX_DEBOUNCE;
        loop {
            let deadline = (Instant::now() + DEBOUNCE).min(ceiling);
            tokio::select! {
                biased;
                _ = sleep_until(deadline) => break,
                changed = self.notifications.changed() => changed.map_err(|_| WatchError::Closed)?,
            }
        }
        let error = self.notifications.borrow_and_update().clone();
        if let Some(error) = error {
            self.changed.send_if_modified(|pending| {
                if pending.as_ref() == Some(&error) {
                    *pending = None;
                    true
                } else {
                    false
                }
            });
            Err(WatchError::Notification(error))
        } else {
            Ok(())
        }
    }
}

fn record_notification(sender: &watch::Sender<Option<String>>, error: Option<String>) {
    sender.send_modify(|pending| {
        match (pending.as_mut(), error) {
            (None, error) => *pending = error,
            (Some(_), Some(error)) => tracing::warn!(%error, "additional file watcher error while a previous error is pending"),
            (Some(_), None) => {}
        }
    });
}

fn has_git_marker(root: &Path) -> io::Result<bool> {
    match root.join(".git").symlink_metadata() {
        Ok(metadata) => Ok(metadata.is_file() || metadata.is_dir()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn watch_directory_exists(path: &Path) -> io::Result<bool> {
    match path.metadata() {
        Ok(metadata) => Ok(metadata.is_dir()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

impl WatchPaths {
    fn relevant(&self, event: &Event) -> bool {
        event.need_rescan()
            || (!matches!(event.kind, EventKind::Access(_))
                && (event.paths.is_empty()
                    || event.paths.iter().any(|path| self.path_relevant(path))))
    }

    fn path_relevant(&self, path: &Path) -> bool {
        if let Ok(relative) = path.strip_prefix(&self.git_dir)
            && matches!(relative.to_str(), Some("HEAD" | "index" | "commondir"))
        {
            return true;
        }
        if let Ok(relative) = path.strip_prefix(&self.common_dir)
            && (relative.starts_with("refs")
                || matches!(
                    relative.to_str(),
                    Some("packed-refs" | "info/exclude" | "config")
                ))
        {
            return !path
                .extension()
                .is_some_and(|extension| extension == "lock");
        }
        path.strip_prefix(&self.root).is_ok_and(|relative| {
            relative == Path::new(".git")
                || !relative
                    .iter()
                    .any(|component| component.eq_ignore_ascii_case(".git"))
        })
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use shepherd_core::test_support::TestRepository;

    use super::*;

    #[tokio::test]
    async fn watcher_error_survives_later_file_events_and_is_reported_once()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let mut watcher = WorkspaceWatch::start(directory.path().to_path_buf())?;
        record_notification(&watcher.changed, Some("fixture watcher error".into()));
        record_notification(&watcher.changed, None);
        let result = tokio::time::timeout(Duration::from_secs(3), watcher.next_refresh()).await?;
        assert!(
            matches!(result, Err(WatchError::Notification(message)) if message == "fixture watcher error")
        );
        record_notification(&watcher.changed, None);
        tokio::time::timeout(Duration::from_secs(3), watcher.next_refresh()).await??;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn distinguishes_missing_and_inaccessible_watch_directories() -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir()?;
        let nested = directory.path().join("nested");
        std::fs::create_dir(&nested)?;
        assert!(watch_directory_exists(&nested)?);
        assert!(!watch_directory_exists(&nested.join("missing"))?);
        std::fs::set_permissions(&nested, std::fs::Permissions::from_mode(0o000))?;
        let result = watch_directory_exists(&nested.join("inaccessible"));
        std::fs::set_permissions(&nested, std::fs::Permissions::from_mode(0o700))?;
        assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::PermissionDenied));
        Ok(())
    }

    #[tokio::test]
    async fn refreshes_a_linked_worktree_after_file_and_git_metadata_changes()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = TestRepository::new()?;
        let repository = fixture.repository();
        let worktree = repository.git_dir.join("shepherd/worktrees/files");
        GitService.create_worktree(repository, &worktree, "shepherd/files", "main")?;
        let mut watcher = WorkspaceWatch::start(worktree.clone())?;
        assert_eq!(
            serde_json::to_value(watcher.snapshot().await?)?,
            serde_json::json!({
                "type": "changes", "files": [], "git": { "branch": "shepherd/files", "changeCount": 0 },
            })
        );

        std::fs::write(worktree.join("new.txt"), "fixture\n")?;
        tokio::time::timeout(Duration::from_secs(6), watcher.next_refresh()).await??;
        assert_eq!(
            serde_json::to_value(watcher.snapshot().await?)?,
            serde_json::json!({
                "type": "changes", "files": [{"path": "new.txt", "kind": "added", "originalPath": null}],
                "git": {"branch": "shepherd/files", "changeCount": 1},
            })
        );
        for arguments in [
            &["add", "--", "new.txt"][..],
            &["commit", "-m", "fixture change"][..],
        ] {
            let output = Command::new("git")
                .arg("-C")
                .arg(&worktree)
                .args(arguments)
                .output()?;
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        tokio::time::timeout(Duration::from_secs(6), watcher.next_refresh()).await??;
        assert_eq!(
            serde_json::to_value(watcher.snapshot().await?)?,
            serde_json::json!({
                "type": "changes", "files": [], "git": { "branch": "shepherd/files", "changeCount": 0 },
            })
        );

        let output = Command::new("git")
            .arg("-C")
            .arg(&worktree)
            .args(["checkout", "--detach"])
            .output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        tokio::time::timeout(Duration::from_secs(6), watcher.next_refresh()).await??;
        assert_eq!(
            serde_json::to_value(watcher.snapshot().await?)?,
            serde_json::json!({
                "type": "changes", "files": [], "git": { "branch": null, "changeCount": 0 },
            })
        );
        Ok(())
    }

    #[test]
    fn filters_unrelated_git_storage_changes() {
        let paths = WatchPaths {
            root: "/project/.git/shepherd/worktrees/task".into(),
            git_dir: "/project/.git/worktrees/task".into(),
            common_dir: "/project/.git".into(),
        };
        for path in [
            "/project/.git/shepherd/worktrees/task/src/main.rs",
            "/project/.git/worktrees/task/index",
            "/project/.git/worktrees/task/HEAD",
            "/project/.git/refs/heads/shepherd/task",
            "/project/.git/packed-refs",
        ] {
            assert!(paths.path_relevant(Path::new(path)), "{path}");
        }
        for path in [
            "/project/.git/shepherd/worktrees/another/src/main.rs",
            "/project/.git/worktrees/task/index.lock",
            "/project/.git/refs/heads/shepherd/task.lock",
            "/project/.git/objects/a1/hash",
            "/outside/README.md",
        ] {
            assert!(!paths.path_relevant(Path::new(path)), "{path}");
        }
    }
}
