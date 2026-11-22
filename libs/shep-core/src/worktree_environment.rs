use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

use crate::{Agent, CoreError, TaskId};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct WorktreeSetup {
    pub after_checkout: Vec<SetupCommand>,
    pub readiness_check: Option<SetupCommand>,
    pub launch_wrapper: Vec<String>,
    pub setup_terminal: Option<SetupTerminal>,
    pub use_imported_setup: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SetupCommand {
    pub name: String,
    pub command: Vec<String>,
    #[serde(default = "required")]
    pub required: bool,
}

fn required() -> bool {
    true
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SetupTerminal {
    pub name: String,
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default)]
    pub instructions: String,
}

impl WorktreeSetup {
    pub fn validate(&self) -> bool {
        fn valid_command(command: &[String]) -> bool {
            command
                .first()
                .is_some_and(|program| !program.trim().is_empty())
                && command.len() <= 64
                && command
                    .iter()
                    .all(|arg| !arg.contains('\0') && arg.len() <= 8192)
        }
        self.after_checkout.len() <= 32
            && self
                .after_checkout
                .iter()
                .chain(&self.readiness_check)
                .all(|step| {
                    !step.name.trim().is_empty()
                        && step.name.len() <= 200
                        && valid_command(&step.command)
                })
            && (self.launch_wrapper.is_empty() || valid_command(&self.launch_wrapper))
            && self.setup_terminal.as_ref().is_none_or(|terminal| {
                !terminal.name.trim().is_empty()
                    && terminal.name.len() <= 200
                    && terminal.instructions.len() <= 8192
                    && !terminal.instructions.contains('\0')
                    && (terminal.command.is_empty() || valid_command(&terminal.command))
            })
    }

    pub fn fingerprint(&self) -> Result<String, serde_json::Error> {
        Ok(fingerprint(&serde_json::to_vec(self)?))
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupRun {
    pub id: String,
    pub name: String,
    pub command: Vec<String>,
    pub cwd: PathBuf,
    pub status: String,
    pub output: String,
}

fn fingerprint(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

const OUTPUT_LIMIT: usize = 16 * 1024;

fn error(message: impl std::fmt::Display) -> CoreError {
    CoreError::ProjectSetup(message.to_string())
}

pub fn runs_path(git_dir: &Path, task_id: &TaskId) -> PathBuf {
    git_dir
        .join("shepherd")
        .join("setup-runs")
        .join(task_id.as_str())
}

pub fn read_runs(path: &Path) -> Result<Vec<SetupRun>, CoreError> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(error),
        Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(failure) => Err(error(failure)),
    }
}

fn save_runs(path: &Path, runs: &[SetupRun]) -> Result<(), CoreError> {
    let parent = path
        .parent()
        .ok_or_else(|| error("Invalid setup log path."))?;
    std::fs::create_dir_all(parent).map_err(error)?;
    let temporary = parent.join(uuid::Uuid::new_v4().to_string());
    let bytes = serde_json::to_vec(runs).map_err(error)?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut log = options.open(&temporary).map_err(error)?;
    use std::io::Write as _;
    let saved = log.write_all(&bytes).and_then(|()| {
        drop(log);
        std::fs::rename(&temporary, path)
    });
    if let Err(failure) = saved {
        if let Err(cleanup) = std::fs::remove_file(&temporary) {
            tracing::warn!(%cleanup, "could not remove the temporary setup log");
        }
        return Err(error(failure));
    }
    Ok(())
}

async fn tail(mut pipe: impl AsyncRead + Unpin) -> std::io::Result<Vec<u8>> {
    let mut tail = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        let count = pipe.read(&mut buffer).await?;
        if count == 0 {
            return Ok(tail);
        }
        let excess = (tail.len() + count).saturating_sub(OUTPUT_LIMIT);
        tail.drain(..excess);
        tail.extend_from_slice(&buffer[..count]);
    }
}

async fn run_command(
    step: &SetupCommand,
    cwd: &Path,
    agent: Agent,
) -> Result<(bool, String), String> {
    let (program, args) = step.command.split_first().ok_or("The command is empty.")?;
    let mut process = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .env("SHEPHERD_AGENT", agent.as_str())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| error.to_string())?;
    let stdout = process
        .stdout
        .take()
        .ok_or("Could not capture standard output.")?;
    let stderr = process
        .stderr
        .take()
        .ok_or("Could not capture standard error.")?;
    let (exit, stdout, stderr) = tokio::time::timeout(Duration::from_secs(300), async {
        tokio::try_join!(process.wait(), tail(stdout), tail(stderr))
    })
    .await
    .map_err(|_| "The command timed out.")?
    .map_err(|error| error.to_string())?;
    let mut output = vec![
        String::from_utf8_lossy(&stdout).into_owned(),
        String::from_utf8_lossy(&stderr).into_owned(),
    ];
    if !exit.success() {
        output.push(exit.to_string());
    }
    output.retain(|part| !part.is_empty());
    Ok((exit.success(), output.join("\n")))
}

pub async fn record_command(
    path: &Path,
    id: String,
    step: &SetupCommand,
    directory: &Path,
    once: bool,
    agent: Agent,
) -> Result<bool, CoreError> {
    let mut runs = read_runs(path)?;
    if once
        && runs.iter().any(|run| {
            run.id == id
                && run.cwd == directory
                && (run.status == "Succeeded" || (!step.required && run.status == "Failed"))
        })
    {
        return Ok(true);
    }
    runs.retain(|run| run.id != id);
    if runs.len() >= 64 {
        runs.remove(0);
    }
    let mut run = SetupRun {
        id,
        name: step.name.clone(),
        command: step.command.clone(),
        cwd: directory.to_path_buf(),
        status: "Running".into(),
        output: String::new(),
    };
    runs.push(run.clone());
    save_runs(path, &runs)?;
    let (success, output) = run_command(step, directory, agent)
        .await
        .unwrap_or_else(|failure| (false, failure));
    run.status = if success { "Succeeded" } else { "Failed" }.into();
    run.output = output;
    if let Some(last) = runs.last_mut() {
        *last = run;
    }
    save_runs(path, &runs)?;
    Ok(success)
}

pub async fn prepare(
    setup: &WorktreeSetup,
    path: &Path,
    directory: &Path,
    task_id: &TaskId,
    agent: Agent,
) -> Result<(), CoreError> {
    for (index, step) in setup.after_checkout.iter().enumerate() {
        let succeeded = record_command(
            path,
            checkout_step_id(index, step).map_err(error)?,
            step,
            directory,
            true,
            agent,
        )
        .await?;
        if !succeeded && step.required {
            return Err(environment_error(
                task_id,
                format!(
                    "Worktree ready. {} failed. See Setup runs for output.",
                    step.name
                ),
                setup,
            ));
        }
    }
    if let Some(step) = &setup.readiness_check
        && !record_command(path, "readiness".into(), step, directory, false, agent).await?
    {
        return Err(environment_error(
            task_id,
            format!(
                "Worktree ready. {} has not passed. See Setup runs for output.",
                step.name
            ),
            setup,
        ));
    }
    Ok(())
}

pub fn checkout_step_id(index: usize, step: &SetupCommand) -> Result<String, serde_json::Error> {
    let fingerprint = fingerprint(&serde_json::to_vec(step)?);
    Ok(format!("after-checkout-{index}-{fingerprint}"))
}

fn environment_error(task_id: &TaskId, message: String, setup: &WorktreeSetup) -> CoreError {
    CoreError::DevelopmentEnvironment {
        task_id: task_id.to_string(),
        message,
        setup_terminal: setup.setup_terminal.is_some(),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn keeps_setup_output_and_retries_only_the_required_step()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let log = directory.path().join("runs.json");
        let task = TaskId::new("setup-fixture");
        let config: WorktreeSetup = serde_json::from_value(serde_json::json!({
            "afterCheckout": [
                { "name": "Seed", "command": ["/bin/sh", "-c", "printf x >> count; printf %s \"$SHEPHERD_AGENT\""] },
                { "name": "Optional", "required": false, "command": ["/bin/sh", "-c", "echo optional >&2; exit 3"] },
                { "name": "Required", "command": ["/bin/sh", "-c", "test -f ready"] }
            ]
        }))?;
        assert!(matches!(
            prepare(&config, &log, directory.path(), &task, Agent::Codex).await,
            Err(CoreError::DevelopmentEnvironment { .. })
        ));
        std::fs::write(directory.path().join("ready"), "")?;
        prepare(&config, &log, directory.path(), &task, Agent::Codex).await?;
        let runs = read_runs(&log)?;
        assert_eq!(
            std::fs::read_to_string(directory.path().join("count"))?,
            "x"
        );
        assert!(
            runs.iter()
                .any(|run| run.name == "Seed" && run.output == "codex")
        );
        assert!(runs.iter().any(|run| run.name == "Optional"
            && run.status == "Failed"
            && run.output.contains("optional")));
        assert!(
            runs.iter()
                .any(|run| run.name == "Required" && run.status == "Succeeded")
        );
        Ok(())
    }
}
