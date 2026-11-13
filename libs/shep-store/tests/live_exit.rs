#![allow(clippy::too_many_lines)]

use std::path::PathBuf;
use std::time::Duration;

use shep_harness_core::{AgentHarness, PromptInput, SpawnSpec};
use shep_harness_full::live_codex::LiveCodexRun;

use shep_core::harness::{HarnessFactory, UnavailableHarnessFactory};
use shep_core::test_support::{TestDatabase, TestRepository};
use shep_core::{Agent, Command, CommandResult, Core, CoreError, ProjectId, TaskId, TaskState};
use shep_store::TursoStore;

/// Starts the real Codex, then kills it with SIGKILL once the test creates
/// the marker file, so a mid-turn death is reproducible on demand.
struct KilledCodexFactory {
    run: LiveCodexRun,
    marker: PathBuf,
}

impl HarnessFactory for KilledCodexFactory {
    fn create(&self) -> Result<Box<dyn AgentHarness>, CoreError> {
        Ok(Box::new(self.run.harness("killed", true)))
    }

    fn spawn_spec(&self, cwd: PathBuf) -> SpawnSpec {
        self.run.spec_with_exit_gate(cwd, "killed", &self.marker)
    }
}

#[tokio::test]
#[ignore]
async fn a_killed_codex_process_fails_the_task_with_its_stderr_tail()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(run) = LiveCodexRun::load("core-mid-turn-exit").await? else {
        return Ok(());
    };

    let repository = TestRepository::new()?;
    let database = TestDatabase::new()?;
    let directory = tempfile::TempDir::new()?;
    let marker = directory.path().join("kill");
    let mut core = Core::with_services(
        Box::new(TursoStore::open(&database.uri()).await?),
        Box::new(KilledCodexFactory {
            run: run.clone(),
            marker: marker.clone(),
        }),
    );

    let CommandResult::ProjectList(projects) = core
        .execute(Command::AddProject {
            path: repository.path().to_path_buf(),
        })
        .await?
    else {
        return Err("expected a project list".into());
    };
    let project = projects.projects.first().ok_or("the project is listed")?;
    let CommandResult::TaskWorkspace(draft) = core
        .execute(Command::CreateDraft {
            agent: Agent::Codex,
            project_id: ProjectId::new(project.id.clone()),
            title: "Kill a live turn".to_string(),
        })
        .await?
    else {
        return Err("expected a draft".into());
    };
    let task_id = TaskId::new(draft.task.id.clone());
    core.execute(Command::SendPrompt {
        task_id: task_id.clone(),
        input: PromptInput::text("Run the shell command `sleep 30` in the repository and wait for it to finish, then say done."),
    })
    .await?;

    let mut live = false;
    for _ in 0..900 {
        core.pump().await?;
        let workspace = core.task_workspace(&task_id)?;
        if workspace.transcript.len() > 1 || workspace.task.state == TaskState::Waiting {
            live = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(live, "the turn must stream something before the kill");
    std::fs::write(&marker, b"kill")?;

    let mut failed = None;
    for _ in 0..900 {
        core.pump().await?;
        let workspace = core.task_workspace(&task_id)?;
        if workspace.task.state == TaskState::Failed {
            failed = Some(workspace);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let workspace = failed.ok_or("the killed process must fail the task")?;

    let error = workspace.task.error.clone().unwrap_or_default();
    assert!(
        error.contains("before the turn finished"),
        "task error: {error}"
    );
    let rows: Vec<String> = workspace
        .transcript
        .iter()
        .map(|entry| entry.summary())
        .collect();
    assert!(
        rows.iter().any(|row| row.starts_with("failure:")),
        "rows: {rows:?}"
    );
    let diagnostics = workspace
        .diagnostics
        .as_ref()
        .ok_or("the workspace carries diagnostics")?;
    assert!(
        diagnostics.stderr_tail.contains("killed mid-turn"),
        "stderr tail: {}",
        diagnostics.stderr_tail
    );
    run.finish(None)?;
    drop(core);
    let mut reopened = Core::load(
        Box::new(TursoStore::open(&database.uri()).await?),
        Box::new(UnavailableHarnessFactory),
    )
    .await?;
    let workspace = reopened.open_task_workspace(&task_id).await?;
    assert_eq!(workspace.task.state, TaskState::Failed);
    assert!(
        workspace
            .task
            .error
            .as_deref()
            .is_some_and(|error| error.contains("before the turn finished"))
    );
    assert!(
        workspace
            .transcript
            .iter()
            .any(|entry| entry.summary().starts_with("failure:"))
    );
    Ok(())
}
