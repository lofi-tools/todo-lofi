//! The S1 walking skeleton's scripted adapter.
//!
//! Until the Codex adapter lands in S2, the shell runs a deterministic fake so
//! the command bridge, transcript and approval flow are usable end to end. The
//! scripted text says what it is; this factory never pretends to be a real agent.

use agent_harness::AgentHarness;
use agent_harness::fake_harness::{FakeHarness, FakeStep, FakeTurn};
use shepherd_core::CoreError;
use shepherd_core::harness::HarnessFactory;

pub struct ScriptedHarnessFactory;

impl HarnessFactory for ScriptedHarnessFactory {
    fn create(&self) -> Result<Box<dyn AgentHarness>, CoreError> {
        Ok(Box::new(FakeHarness::new(script())))
    }
}

fn script() -> Vec<FakeTurn> {
    vec![
        FakeTurn::new(vec![
            FakeStep::Message("Scripted adapter: the real Codex agent arrives in S2.".to_string()),
            FakeStep::approval("approval-1", "Run the scripted tool"),
            FakeStep::Message("Scripted turn complete.".to_string()),
        ]),
        FakeTurn::new(vec![FakeStep::Message(
            "Scripted follow-up turn complete.".to_string(),
        )]),
    ]
}
