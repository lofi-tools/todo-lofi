#![allow(clippy::too_many_lines)]

use std::path::PathBuf;
use std::time::Duration;

use shep_harness_core::{AgentHarness, PromptInput, SpawnSpec};
use shep_harness_full::live_codex::LiveCodexRun;

use shep_core::harness::HarnessFactory;
use shep_core::store::Store as _;
use shep_core::test_support::{TestDatabase, TestRepository};
use shep_core::{Agent, Command, CommandResult, Core, CoreError, ProjectId, TaskId, TaskState};
use shep_store::TursoStore;

struct LiveCodexFactory {
    run: LiveCodexRun,
    session: &'static str,
}

impl HarnessFactory for LiveCodexFactory {
    fn create(&self) -> Result<Box<dyn AgentHarness>, CoreError> {
        Ok(Box::new(self.run.harness(self.session, false)))
    }

    fn spawn_spec(&self, cwd: PathBuf) -> SpawnSpec {
        self.run.spec(cwd, self.session, None)
    }
}

#[tokio::test]
#[ignore]
async fn a_restarted_core_resumes_the_recorded_thread() -> Result<(), Box<dyn std::error::Error>> {
    let Some(run) = LiveCodexRun::load("core-restart-resume").await? else {
        return Ok(());
    };

    let repository = TestRepository::new()?;
    let database = TestDatabase::new()?;
    let uri = database.uri();
    let factory = |session| LiveCodexFactory {
        run: run.clone(),
        session,
    };

    let mut core = Core::with_services(
        Box::new(TursoStore::open(&uri).await?),
        Box::new(factory("initial")),
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
            title: "Restart and resume".to_string(),
        })
        .await?
    else {
        return Err("expected a draft".into());
    };
    let task_id = TaskId::new(draft.task.id.clone());
    core.execute(Command::SendPrompt {
        task_id: task_id.clone(),
        input: PromptInput::text("Run the shell command `sleep 30` in the repository, wait for it to finish, then say done."),
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
    assert!(live, "the turn must stream before the app quits");

    // Quitting drops the core, which ends the agent process; nothing about
    // the thread survives except what the store recorded.
    drop(core);

    let store = TursoStore::open(&uri).await?;
    let sessions = store.load_sessions().await?;
    let recorded = sessions
        .iter()
        .filter(|session| session.task_id == task_id)
        .max_by_key(|session| session.started_at)
        .and_then(|session| session.adapter_session_id.clone())
        .ok_or("the adapter thread is recorded")?;

    let mut restarted = Core::load(Box::new(store), Box::new(factory("resumed"))).await?;
    let workspace = restarted.task_workspace(&task_id)?;
    assert_eq!(
        workspace.task.state,
        TaskState::Interrupted,
        "a live task loads as interrupted"
    );

    let CommandResult::TaskWorkspace(resumed) = restarted
        .execute(Command::SendPrompt {
            task_id: task_id.clone(),
            input: PromptInput::text("Reply with the single word: resumed."),
        })
        .await?
    else {
        return Err("expected a workspace".into());
    };
    let rows: Vec<String> = resumed
        .transcript
        .iter()
        .map(|entry| entry.summary())
        .collect();
    let note = format!("resumed the previous agent session {recorded}");
    assert!(rows.iter().any(|row| row.contains(&note)), "rows: {rows:?}");

    let mut finished = None;
    for _ in 0..900 {
        restarted.pump().await?;
        let workspace = restarted.task_workspace(&task_id)?;
        if matches!(workspace.task.state, TaskState::Ready | TaskState::Review) {
            finished = Some(workspace);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let workspace = finished.ok_or("the resumed turn must finish")?;
    assert!(
        workspace.task.error.is_none(),
        "task error: {:?}",
        workspace.task.error
    );
    assert!(
        workspace.transcript.len() > resumed.transcript.len(),
        "the resumed turn must add to the transcript"
    );
    run.finish(None)?;
    Ok(())
}
