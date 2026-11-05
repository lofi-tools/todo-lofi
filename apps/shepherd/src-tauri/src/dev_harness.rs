//! The scripted adapter used in tests and isolated development runs.
//!
//! The app itself runs the live Codex adapter. Tests use this deterministic fake
//! so the command bridge, transcript and approval flow need no installed CLI. The
//! scripted text says what it is; this factory never pretends to be a real agent.

use agent_harness::AgentHarness;
use agent_harness::fake_harness::{FakeHarness, FakeStep, FakeTurn};
use shepherd_core::CoreError;
use shepherd_core::harness::HarnessFactory;

#[cfg(debug_assertions)]
pub fn factory(isolated: bool) -> std::io::Result<Option<Box<dyn HarnessFactory>>> {
    #[cfg(feature = "desktop-codex-smoke")]
    if std::env::var("SHEPHERD_DEV_CODEX_APPROVAL_REPLAY").as_deref() == Ok("1") {
        if !isolated {
            return Err(std::io::Error::other("Codex replay requires isolated data"));
        }
        let replay = agent_harness::live_codex::LiveCodexRun::load_replay("network-approval")
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        return Ok(Some(Box::new(CodexApprovalReplay(replay))));
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
struct CodexApprovalReplay(agent_harness::live_codex::LiveCodexRun);

#[cfg(all(debug_assertions, feature = "desktop-codex-smoke"))]
impl HarnessFactory for CodexApprovalReplay {
    fn create(&self) -> Result<Box<dyn AgentHarness>, CoreError> {
        Ok(Box::new(self.0.harness("approval", false)))
    }

    fn spawn_spec(&self, cwd: std::path::PathBuf) -> agent_harness::SpawnSpec {
        self.0.spec(cwd, "approval", None)
    }
}

fn script() -> Vec<FakeTurn> {
    vec![
        FakeTurn::new(vec![
            FakeStep::Message("Scripted adapter: this turn uses a local fixture.".to_string()),
            FakeStep::approval("approval-1", "Run the scripted tool"),
            FakeStep::Message("Scripted turn complete.".to_string()),
        ]),
        FakeTurn::new(vec![FakeStep::Message(
            "Scripted follow-up turn complete.".to_string(),
        )]),
    ]
}
