use std::sync::Mutex;

use async_trait::async_trait;
use shep_harness_core::{
    AgentHarness, ApprovalId, Capabilities, Decision, HarnessEvent, HarnessResult, PromptInput,
    SessionApproval, SessionId, SpawnSpec,
};
use tokio::sync::mpsc;

use shep_core::CoreError;
use shep_core::harness::HarnessFactory;

pub struct LateHarnessFactory {
    pub receiver: Mutex<Option<mpsc::UnboundedReceiver<HarnessEvent>>>,
    pub prompts: mpsc::UnboundedSender<(Option<String>, PromptInput)>,
}

impl HarnessFactory for LateHarnessFactory {
    fn create(&self) -> Result<Box<dyn AgentHarness>, CoreError> {
        let receiver = self
            .receiver
            .lock()
            .expect("the test holds the receiver lock")
            .take()
            .expect("the factory creates one harness per test");
        Ok(Box::new(LateHarness {
            events: receiver,
            approvals: Vec::new(),
            prompts: Some(self.prompts.clone()),
            mode: None,
        }))
    }
}

/// Emits nothing on its own; the test sends events after the prompt returns,
/// the way a real adapter streams them between commands.
pub struct LateHarness {
    pub events: mpsc::UnboundedReceiver<HarnessEvent>,
    pub approvals: Vec<SessionApproval>,
    pub prompts: Option<mpsc::UnboundedSender<(Option<String>, PromptInput)>>,
    pub mode: Option<String>,
}

#[async_trait]
impl AgentHarness for LateHarness {
    fn events(&mut self) -> &mut mpsc::UnboundedReceiver<HarnessEvent> {
        &mut self.events
    }

    fn session_approvals(&self) -> Vec<SessionApproval> {
        self.approvals.clone()
    }

    async fn reset_session_approval(&mut self, id: &str) -> HarnessResult<()> {
        self.approvals.retain(|approval| approval.id != id);
        Ok(())
    }

    async fn start(&mut self, _spec: SpawnSpec) -> HarnessResult<Capabilities> {
        Ok(Capabilities {
            cancel: true,
            ..Capabilities::default()
        })
    }

    async fn open_session(&mut self, resume: Option<SessionId>) -> HarnessResult<SessionId> {
        Ok(resume.unwrap_or_else(|| SessionId::new("late-thread")))
    }

    async fn prompt(&mut self, input: PromptInput) -> HarnessResult<()> {
        if let Some(prompts) = &self.prompts {
            prompts
                .send((self.mode.clone(), input))
                .map_err(|_| shep_harness_core::HarnessError::ChannelClosed)?;
        }
        Ok(())
    }

    async fn set_mode(&mut self, mode: &str) -> HarnessResult<()> {
        if mode != "plan" {
            return Err(shep_harness_core::HarnessError::ModeUnavailable);
        }
        self.mode = Some(mode.to_owned());
        Ok(())
    }

    async fn cancel(&mut self) -> HarnessResult<()> {
        Ok(())
    }

    async fn respond(
        &mut self,
        _approval_id: ApprovalId,
        _decision: Decision,
    ) -> HarnessResult<()> {
        Ok(())
    }

    async fn shutdown(&mut self) -> HarnessResult<()> {
        Ok(())
    }
}
