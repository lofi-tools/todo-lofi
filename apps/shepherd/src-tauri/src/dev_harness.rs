//! Agent fixtures for isolated desktop runs.

use agent_harness::fake_harness::{FakeHarness, FakeStep, FakeTurn};
use agent_harness::{AgentHarness, ToolStatus};
use shepherd_core::CoreError;
use shepherd_core::harness::HarnessFactory;

#[cfg(all(debug_assertions, feature = "desktop-codex-smoke"))]
use agent_harness::live_codex::LiveCodexRun;
#[cfg(all(debug_assertions, feature = "desktop-codex-smoke"))]
use tauri::Manager;

#[cfg(debug_assertions)]
pub fn factory(
    _app: &tauri::App,
    isolated: bool,
) -> std::io::Result<Option<Box<dyn HarnessFactory>>> {
    #[cfg(feature = "desktop-codex-smoke")]
    if let Some(factory) = codex_smoke_factory(_app, isolated)? {
        return Ok(Some(factory));
    }
    if isolated && std::env::var("SHEPHERD_DEV_SCRIPTED_HARNESS").as_deref() == Ok("1") {
        return Ok(Some(Box::new(ScriptedHarnessFactory)));
    }
    Ok(None)
}

pub struct ScriptedHarnessFactory;

impl HarnessFactory for ScriptedHarnessFactory {
    fn create(&self) -> Result<Box<dyn AgentHarness>, CoreError> {
        Ok(Box::new(FakeHarness::new(script())))
    }
}

#[cfg(all(debug_assertions, feature = "desktop-codex-smoke"))]
fn codex_smoke_factory(
    app: &tauri::App,
    isolated: bool,
) -> std::io::Result<Option<Box<dyn HarnessFactory>>> {
    let mode = std::env::var("SHEPHERD_DEV_CODEX_SMOKE").unwrap_or_default();
    if mode.is_empty() {
        return Ok(None);
    }
    if !isolated {
        return Err(std::io::Error::other("Codex smoke requires isolated data"));
    }
    let scenario = match mode.as_str() {
        "approval-replay" => "network-approval",
        "approval-cancel" => "desktop-approval-cancel",
        "worktree-edit" => "desktop-worktree-edit",
        "restart" => match std::env::var("SHEPHERD_DESKTOP_SMOKE_PHASE").as_deref() {
            Ok("codex-before-restart") => "desktop-before-restart",
            Ok("codex-after-restart") => "desktop-after-restart",
            _ => return Err(std::io::Error::other("Unknown Codex restart phase")),
        },
        _ => return Err(std::io::Error::other("Unknown Codex smoke mode")),
    };
    let (run, session) = if mode == "approval-replay" {
        (LiveCodexRun::load_replay(scenario), "approval")
    } else {
        let run = tauri::async_runtime::block_on(LiveCodexRun::load(scenario))
            .and_then(|run| run.ok_or_else(|| "Codex smoke requires a Codex installation".into()));
        (run, "desktop")
    };
    let run = run.map_err(|error| std::io::Error::other(error.to_string()))?;
    let apply_patch = mode == "worktree-edit" && run.is_replay();
    app.manage(run.clone());
    Ok(Some(Box::new(CodexSmoke {
        run,
        session,
        apply_patch,
    })))
}

#[cfg(all(debug_assertions, feature = "desktop-codex-smoke"))]
#[tauri::command]
pub fn finish_codex_smoke(
    app: tauri::AppHandle,
    worktree_patch: Option<String>,
) -> Result<bool, String> {
    let run = app
        .try_state::<LiveCodexRun>()
        .ok_or_else(|| "Codex smoke is inactive".to_string())?;
    run.finish(worktree_patch)
        .map_err(|error| error.to_string())?;
    Ok(run.is_replay())
}

#[cfg(all(debug_assertions, feature = "desktop-codex-smoke"))]
struct CodexSmoke {
    run: LiveCodexRun,
    session: &'static str,
    apply_patch: bool,
}

#[cfg(all(debug_assertions, feature = "desktop-codex-smoke"))]
impl HarnessFactory for CodexSmoke {
    fn create(&self) -> Result<Box<dyn AgentHarness>, CoreError> {
        Ok(Box::new(self.run.harness(self.session, false)))
    }

    fn spawn_spec(&self, cwd: std::path::PathBuf) -> agent_harness::SpawnSpec {
        let mut spec = self.run.spec(cwd, self.session, None);
        if self.apply_patch {
            spec.environment
                .insert("SHEPHERD_REPLAY_WORKTREE_PATCH".into(), "1".into());
        }
        spec
    }
}

fn script() -> Vec<FakeTurn> {
    vec![
        FakeTurn::new(vec![
            FakeStep::Message("Scripted adapter: this turn uses a local fixture.".to_string()),
            FakeStep::ToolCall {
                tool_call_id: "fixture-inspection".into(),
                title: "Inspect the scripted workspace".into(),
                kind: "shell".into(),
                status: ToolStatus::Completed,
                output: None,
                diffs: Vec::new(),
            },
            FakeStep::approval("approval-1", "Run the scripted tool"),
            FakeStep::Message("Scripted turn complete.".to_string()),
        ]),
        FakeTurn::new(vec![FakeStep::Message(
            "Scripted follow-up turn complete.".to_string(),
        )]),
    ]
}
