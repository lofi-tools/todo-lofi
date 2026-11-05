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
    data_directory: Option<&std::path::Path>,
) -> std::io::Result<Option<Box<dyn HarnessFactory>>> {
    #[cfg(feature = "desktop-codex-smoke")]
    if let Some(factory) = codex_smoke_factory(_app, data_directory)? {
        return Ok(Some(factory));
    }
    if data_directory.is_some()
        && std::env::var("SHEPHERD_DEV_SCRIPTED_HARNESS").as_deref() == Ok("1")
    {
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
    data_directory: Option<&std::path::Path>,
) -> std::io::Result<Option<Box<dyn HarnessFactory>>> {
    let mode = std::env::var("SHEPHERD_DEV_CODEX_SMOKE").unwrap_or_default();
    if mode.is_empty() {
        return Ok(None);
    }
    let Some(data_directory) = data_directory else {
        return Err(std::io::Error::other("Codex smoke requires isolated data"));
    };
    let scenario = match mode.as_str() {
        "approval-replay" => "network-approval",
        "approval-cancel" => "desktop-approval-cancel",
        "worktree-edit" => "desktop-worktree-edit",
        "review-merge" => "desktop-review-merge",
        "mid-turn-exit" => "desktop-mid-turn-exit",
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
    let patch_replay = if run.is_replay() {
        match mode.as_str() {
            "worktree-edit" => Some("1"),
            "review-merge" => Some("sequence"),
            _ => None,
        }
    } else {
        None
    };
    let exit_gate = (mode == "mid-turn-exit").then(|| data_directory.join("codex-exit"));
    if let Some(gate) = &exit_gate {
        app.manage(CodexExitGate(gate.clone()));
    }
    app.manage(run.clone());
    Ok(Some(Box::new(CodexSmoke {
        run,
        session,
        patch_replay,
        exit_gate,
    })))
}

#[cfg(all(debug_assertions, feature = "desktop-codex-smoke"))]
struct CodexExitGate(std::path::PathBuf);

#[cfg(all(debug_assertions, feature = "desktop-codex-smoke"))]
#[tauri::command]
pub fn trigger_codex_smoke_exit(app: tauri::AppHandle) -> Result<(), String> {
    let gate = app
        .try_state::<CodexExitGate>()
        .ok_or_else(|| "Codex exit smoke is inactive".to_string())?;
    std::fs::write(&gate.0, b"kill").map_err(|error| error.to_string())
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
    patch_replay: Option<&'static str>,
    exit_gate: Option<std::path::PathBuf>,
}

#[cfg(all(debug_assertions, feature = "desktop-codex-smoke"))]
impl HarnessFactory for CodexSmoke {
    fn create(&self) -> Result<Box<dyn AgentHarness>, CoreError> {
        Ok(Box::new(
            self.run.harness(self.session, self.exit_gate.is_some()),
        ))
    }

    fn spawn_spec(&self, cwd: std::path::PathBuf) -> agent_harness::SpawnSpec {
        if let Some(gate) = &self.exit_gate {
            return self.run.spec_with_exit_gate(cwd, self.session, gate);
        }
        let mut spec = self.run.spec(cwd, self.session, None);
        if let Some(mode) = self.patch_replay {
            spec.environment
                .insert("SHEPHERD_REPLAY_WORKTREE_PATCH".into(), mode.into());
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
