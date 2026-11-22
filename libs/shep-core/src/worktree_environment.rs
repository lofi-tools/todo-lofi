use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use tokio::process::Command;

use crate::{CoreError, TaskId};

#[derive(Deserialize)]
struct DirenvStatus {
    state: State,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct State {
    #[serde(rename = "foundRC")]
    found_rc: Option<EnvironmentFile>,
}

#[derive(Deserialize)]
struct EnvironmentFile {
    path: PathBuf,
    allowed: u8,
}

pub async fn direnv_approved(directory: &Path) -> Result<bool, String> {
    let directory = directory
        .canonicalize()
        .map_err(|error| format!("Could not open the task worktree: {error}"))?;
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        Command::new("direnv")
            .args(["status", "--json"])
            .current_dir(&directory)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| "Timed out checking direnv approval.".to_string())?
    .map_err(|error| format!("Could not run direnv: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "Could not check direnv approval: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let status: DirenvStatus = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("Could not read direnv status: {error}"))?;
    let file = status
        .state
        .found_rc
        .filter(|file| file.path == directory.join(".envrc"))
        .ok_or("No .envrc was found at the root of this worktree.")?;
    Ok(file.allowed == 0)
}

pub async fn require_direnv(directory: &Path, task_id: &TaskId) -> Result<(), CoreError> {
    let (message, awaiting_approval) = match direnv_approved(directory).await {
        Ok(true) => return Ok(()),
        Ok(false) => (
            "Worktree ready. Review this worktree’s .envrc and run direnv allow . in the setup terminal.".into(),
            true,
        ),
        Err(message) => (message, false),
    };
    Err(CoreError::DevelopmentEnvironment {
        task_id: task_id.to_string(),
        message,
        awaiting_approval,
    })
}
