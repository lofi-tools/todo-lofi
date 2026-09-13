use crate::config::{HooksConfig, normalize_path};
use crate::error::Result;
use crate::error::SymphonyError::*;
use log::{info, warn};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;

/// Longest hook output that is included in log messages.
const HOOK_OUTPUT_LOG_LIMIT: usize = 2_000;

/// A per-issue workspace assignment (Section 4.1.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    /// Absolute path of the workspace directory.
    pub path: PathBuf,
    /// Sanitized issue identifier used as the directory name.
    pub workspace_key: String,
    /// Whether this call created the directory.
    pub created_now: bool,
}

impl Workspace {
    /// Validate the safety invariants before launching a coding agent (Section 9.5).
    pub fn validate_launch_path(&self, root: &Path, cwd: &Path) -> Result<()> {
        if normalize_path(cwd) != normalize_path(&self.path) {
            return Err(InvalidWorkspaceCwd {
                path: cwd.to_string_lossy().into_owned(),
            });
        }
        if !is_inside(root, cwd) {
            return Err(InvalidWorkspacePath {
                path: cwd.to_string_lossy().into_owned(),
                root: root.to_string_lossy().into_owned(),
            });
        }
        Ok(())
    }
}

/// Manage per-issue workspaces and their lifecycle hooks.
///
/// Hook configuration is passed in per call so that a reloaded `WORKFLOW.md`
/// applies to future hook executions without restart (Section 6.2).
#[derive(Debug, Clone)]
pub struct WorkspaceManager {
    root: PathBuf,
}

impl WorkspaceManager {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root: normalize_path(&root),
        }
    }

    /// The configured workspace root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Sanitize an issue identifier into a workspace directory name (Section 9.5).
    pub fn sanitize_identifier(identifier: &str) -> String {
        let sanitized: String = identifier
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric()
                    || character == '.'
                    || character == '-'
                    || character == '_'
                {
                    character
                } else {
                    '_'
                }
            })
            .collect();

        if sanitized.is_empty() {
            "workspace".to_string()
        } else {
            sanitized
        }
    }

    /// Deterministic workspace path for an issue identifier.
    pub fn workspace_path(&self, identifier: &str) -> PathBuf {
        self.root.join(Self::sanitize_identifier(identifier))
    }

    /// Create (or reuse) the workspace for an issue.
    pub async fn create_for_issue(
        &self,
        identifier: &str,
        hooks: &HooksConfig,
    ) -> Result<Workspace> {
        let workspace_key = Self::sanitize_identifier(identifier);
        let path = self.root.join(&workspace_key);

        // Invariant 2: the workspace path must stay inside the workspace root.
        if !is_inside(&self.root, &path) {
            return Err(InvalidWorkspacePath {
                path: path.to_string_lossy().into_owned(),
                root: self.root.to_string_lossy().into_owned(),
            });
        }

        let mut created_now = false;
        match tokio::fs::metadata(&path).await {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                return Err(WorkspaceError {
                    source: Box::new(std::io::Error::new(
                        std::io::ErrorKind::AlreadyExists,
                        format!(
                            "Workspace path {} exists and is not a directory",
                            path.display()
                        ),
                    )),
                });
            }
            Err(_) => {
                tokio::fs::create_dir_all(&path)
                    .await
                    .map_err(|error| WorkspaceError {
                        source: Box::new(error),
                    })?;
                created_now = true;
            }
        }

        let workspace = Workspace {
            path,
            workspace_key,
            created_now,
        };

        // `after_create` only runs for a newly created workspace and is fatal on failure.
        if created_now
            && let Some(script) = &hooks.after_create
        {
            self.run_hook("after_create", script, &workspace.path, hooks.timeout_ms)
                .await?;
        }

        Ok(workspace)
    }

    /// Run the `before_run` hook for an attempt. Failure aborts the attempt.
    pub async fn prepare_run(&self, workspace: &Workspace, hooks: &HooksConfig) -> Result<()> {
        if let Some(script) = &hooks.before_run {
            self.run_hook("before_run", script, &workspace.path, hooks.timeout_ms)
                .await?;
        }
        Ok(())
    }

    /// Run the `after_run` hook for an attempt. Failures are logged and ignored.
    pub async fn finish_run(&self, workspace: &Workspace, hooks: &HooksConfig) {
        if let Some(script) = &hooks.after_run
            && let Err(error) = self
                .run_hook("after_run", script, &workspace.path, hooks.timeout_ms)
                .await
        {
            warn!(
                target: "symphony",
                "hook=after_run workspace={} outcome=failed error={error}",
                workspace.path.display()
            );
        }
    }

    /// Remove the workspace for an issue, running `before_remove` first.
    pub async fn remove_for_issue(
        &self,
        identifier: &str,
        hooks: &HooksConfig,
    ) -> Result<()> {
        let path = self.workspace_path(identifier);

        if !is_inside(&self.root, &path) {
            return Err(InvalidWorkspacePath {
                path: path.to_string_lossy().into_owned(),
                root: self.root.to_string_lossy().into_owned(),
            });
        }

        if !tokio::fs::metadata(&path)
            .await
            .map(|metadata| metadata.is_dir())
            .unwrap_or(false)
        {
            return Ok(());
        }

        // A failing `before_remove` hook is logged and ignored; cleanup proceeds.
        if let Some(script) = &hooks.before_remove
            && let Err(error) = self
                .run_hook("before_remove", script, &path, hooks.timeout_ms)
                .await
        {
            warn!(
                target: "symphony",
                "hook=before_remove workspace_key={} outcome=failed error={error}",
                Self::sanitize_identifier(identifier)
            );
        }

        tokio::fs::remove_dir_all(&path)
            .await
            .map_err(|error| WorkspaceError {
                source: Box::new(error),
            })
    }

    /// Run a workspace hook with the configured timeout (Section 9.4).
    async fn run_hook(
        &self,
        name: &str,
        script: &str,
        workspace_path: &Path,
        timeout_ms: u64,
    ) -> Result<()> {
        info!(
            target: "symphony",
            "hook={name} workspace={} outcome=started",
            workspace_path.display()
        );

        let future = Command::new("sh")
            .arg("-lc")
            .arg(script)
            .current_dir(workspace_path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .output();

        let output = match tokio::time::timeout(Duration::from_millis(timeout_ms), future).await {
            Ok(Ok(output)) => output,
            Ok(Err(error)) => {
                return Err(WorkspaceError {
                    source: Box::new(std::io::Error::other(format!(
                        "{name} hook could not be started: {error}"
                    ))),
                });
            }
            Err(_) => {
                return Err(WorkspaceError {
                    source: Box::new(std::io::Error::other(format!(
                        "{name} hook timed out after {timeout_ms} ms"
                    ))),
                });
            }
        };

        if output.status.success() {
            info!(
                target: "symphony",
                "hook={name} workspace={} outcome=completed",
                workspace_path.display()
            );
            return Ok(());
        }

        Err(WorkspaceError {
            source: Box::new(std::io::Error::other(format!(
                "{name} hook failed with exit code {}: {}",
                output.status.code().unwrap_or(-1),
                truncate_for_log(&String::from_utf8_lossy(&output.stderr))
            ))),
        })
    }
}

/// Whether `path` is `root` itself or lives inside it.
pub fn is_inside(root: &Path, path: &Path) -> bool {
    let root = normalize_path(root);
    let path = normalize_path(path);
    path.starts_with(&root)
}

fn truncate_for_log(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= HOOK_OUTPUT_LOG_LIMIT {
        return trimmed.to_string();
    }
    let truncated: String = trimmed.chars().take(HOOK_OUTPUT_LOG_LIMIT).collect();
    format!("{truncated}… (truncated)")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn manager(directory: &TempDir) -> WorkspaceManager {
        WorkspaceManager::new(directory.path().to_path_buf())
    }

    #[test]
    fn test_sanitize_identifier() {
        assert_eq!(WorkspaceManager::sanitize_identifier("ABC-123"), "ABC-123");
        assert_eq!(
            WorkspaceManager::sanitize_identifier("test.feature"),
            "test.feature"
        );
        assert_eq!(
            WorkspaceManager::sanitize_identifier("test feature"),
            "test_feature"
        );
        assert_eq!(
            WorkspaceManager::sanitize_identifier("test@feature"),
            "test_feature"
        );
        assert_eq!(
            WorkspaceManager::sanitize_identifier("test/feature"),
            "test_feature"
        );
        assert_eq!(
            WorkspaceManager::sanitize_identifier("../escape"),
            ".._escape"
        );
        assert_eq!(WorkspaceManager::sanitize_identifier(""), "workspace");
    }

    #[test]
    fn test_deterministic_workspace_path() {
        let directory = TempDir::new().expect("temp dir");
        let manager = manager(&directory);
        assert_eq!(
            manager.workspace_path("TEST-123"),
            directory.path().join("TEST-123")
        );
        assert_eq!(
            manager.workspace_path("TEST-123"),
            manager.workspace_path("TEST-123")
        );
    }

    #[tokio::test]
    async fn test_create_and_reuse_workspace() {
        let directory = TempDir::new().expect("temp dir");
        let manager = manager(&directory);
        let hooks = HooksConfig::default();

        let created = manager
            .create_for_issue("TEST-123", &hooks)
            .await
            .expect("workspace is created");
        assert!(created.path.is_dir());
        assert!(created.created_now);
        assert_eq!(created.workspace_key, "TEST-123");

        let reused = manager
            .create_for_issue("TEST-123", &hooks)
            .await
            .expect("workspace is reused");
        assert_eq!(reused.path, created.path);
        assert!(!reused.created_now);
    }

    #[tokio::test]
    async fn test_file_at_workspace_location_fails_safely() {
        let directory = TempDir::new().expect("temp dir");
        let manager = manager(&directory);
        std::fs::write(directory.path().join("TEST-123"), "not a directory").expect("write");

        assert!(matches!(
            manager
                .create_for_issue("TEST-123", &HooksConfig::default())
                .await,
            Err(WorkspaceError { .. })
        ));
    }

    #[tokio::test]
    async fn test_after_create_hook_runs_once() {
        let directory = TempDir::new().expect("temp dir");
        let manager = manager(&directory);
        let hooks = HooksConfig {
            after_create: Some("echo created >> ../created.log".to_string()),
            timeout_ms: 5_000,
            ..HooksConfig::default()
        };

        manager
            .create_for_issue("TEST-1", &hooks)
            .await
            .expect("create");
        manager
            .create_for_issue("TEST-1", &hooks)
            .await
            .expect("reuse");

        let log =
            std::fs::read_to_string(directory.path().join("created.log")).expect("hook log exists");
        assert_eq!(log.lines().count(), 1);
    }

    #[tokio::test]
    async fn test_failing_after_create_aborts_creation() {
        let directory = TempDir::new().expect("temp dir");
        let manager = manager(&directory);
        let hooks = HooksConfig {
            after_create: Some("exit 3".to_string()),
            timeout_ms: 5_000,
            ..HooksConfig::default()
        };

        assert!(matches!(
            manager.create_for_issue("TEST-1", &hooks).await,
            Err(WorkspaceError { .. })
        ));
    }

    #[tokio::test]
    async fn test_hook_timeout_aborts() {
        let directory = TempDir::new().expect("temp dir");
        let manager = manager(&directory);
        let hooks = HooksConfig {
            after_create: Some("sleep 30".to_string()),
            timeout_ms: 200,
            ..HooksConfig::default()
        };

        let error = manager
            .create_for_issue("TEST-1", &hooks)
            .await
            .expect_err("hook times out");
        assert!(error.to_string().contains("timed out"));
    }

    #[tokio::test]
    async fn test_after_run_failure_is_ignored() {
        let directory = TempDir::new().expect("temp dir");
        let manager = manager(&directory);
        let hooks = HooksConfig {
            after_run: Some("exit 9".to_string()),
            timeout_ms: 5_000,
            ..HooksConfig::default()
        };

        let workspace = manager
            .create_for_issue("TEST-1", &hooks)
            .await
            .expect("create");
        // Must not panic or return an error.
        manager.finish_run(&workspace, &hooks).await;
    }

    #[tokio::test]
    async fn test_before_run_failure_is_fatal() {
        let directory = TempDir::new().expect("temp dir");
        let manager = manager(&directory);
        let hooks = HooksConfig {
            before_run: Some("exit 1".to_string()),
            timeout_ms: 5_000,
            ..HooksConfig::default()
        };

        let workspace = manager
            .create_for_issue("TEST-1", &hooks)
            .await
            .expect("create");
        assert!(manager.prepare_run(&workspace, &hooks).await.is_err());
    }

    #[tokio::test]
    async fn test_remove_workspace_runs_before_remove() {
        let directory = TempDir::new().expect("temp dir");
        let manager = manager(&directory);
        let hooks = HooksConfig {
            before_remove: Some("exit 7".to_string()),
            timeout_ms: 5_000,
            ..HooksConfig::default()
        };

        let workspace = manager
            .create_for_issue("TEST-1", &hooks)
            .await
            .expect("create");
        // A failing before_remove hook is logged and ignored, cleanup still proceeds.
        manager
            .remove_for_issue("TEST-1", &hooks)
            .await
            .expect("removed");
        assert!(!workspace.path.exists());
    }

    #[test]
    fn test_launch_path_invariants() {
        let directory = TempDir::new().expect("temp dir");
        let manager = manager(&directory);
        let workspace = Workspace {
            path: directory.path().join("TEST-1"),
            workspace_key: "TEST-1".to_string(),
            created_now: true,
        };

        assert!(
            workspace
                .validate_launch_path(manager.root(), &workspace.path)
                .is_ok()
        );
        // A cwd that is not the per-issue workspace is rejected.
        assert!(matches!(
            workspace.validate_launch_path(manager.root(), directory.path()),
            Err(InvalidWorkspaceCwd { .. })
        ));
        // A workspace outside the root is rejected.
        let outside = Workspace {
            path: PathBuf::from("/tmp/elsewhere/TEST-1"),
            workspace_key: "TEST-1".to_string(),
            created_now: true,
        };
        assert!(matches!(
            outside.validate_launch_path(manager.root(), &outside.path),
            Err(InvalidWorkspacePath { .. })
        ));
    }

    #[test]
    fn test_path_containment() {
        assert!(is_inside(Path::new("/tmp/root"), Path::new("/tmp/root/a")));
        assert!(is_inside(Path::new("/tmp/root"), Path::new("/tmp/root")));
        assert!(!is_inside(
            Path::new("/tmp/root"),
            Path::new("/tmp/root/../other")
        ));
        assert!(!is_inside(Path::new("/tmp/root"), Path::new("/tmp/rooted")));
    }
}
