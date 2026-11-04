//! The core's boundary around an agent adapter.
//!
//! `agent-harness` owns the protocol contract and transcript model. This module
//! owns the session lifecycle: start, open, prompt, pause on approval, cancel and
//! shutdown. It does not know which CLI is underneath.

use agent_harness::{
    AgentHarness, ApprovalId, Capabilities, Decision, HarnessEvent, PromptInput, SessionId,
    SpawnSpec, Transcript,
};

use crate::CoreError;

/// One live adapter session with its core-owned transcript.
pub struct HarnessSession {
    harness: Box<dyn AgentHarness>,
    capabilities: Capabilities,
    session_id: Option<SessionId>,
    transcript: Transcript,
    pending_approval: Option<ApprovalId>,
}

impl HarnessSession {
    pub fn new(harness: Box<dyn AgentHarness>) -> Self {
        Self {
            harness,
            capabilities: Capabilities::default(),
            session_id: None,
            transcript: Transcript::default(),
            pending_approval: None,
        }
    }

    pub async fn start(&mut self, spec: SpawnSpec) -> Result<Capabilities, CoreError> {
        self.capabilities = self.harness.start(spec).await?;
        Ok(self.capabilities.clone())
    }

    pub async fn open_session(
        &mut self,
        resume: Option<SessionId>,
    ) -> Result<SessionId, CoreError> {
        let session_id = self.harness.open_session(resume).await?;
        self.session_id = Some(session_id.clone());
        Ok(session_id)
    }

    /// Record the user's prompt, ask the adapter, then fold every event the
    /// adapter has already produced into the transcript.
    pub async fn prompt(&mut self, text: String) -> Result<Vec<HarnessEvent>, CoreError> {
        self.transcript.push_user_message(text.clone());
        self.harness.prompt(PromptInput { text }).await?;
        Ok(self.pump())
    }

    pub async fn cancel(&mut self) -> Result<Vec<HarnessEvent>, CoreError> {
        self.harness.cancel().await?;
        self.pending_approval = None;
        Ok(self.pump())
    }

    pub async fn respond(
        &mut self,
        approval_id: ApprovalId,
        decision: Decision,
    ) -> Result<Vec<HarnessEvent>, CoreError> {
        self.harness.respond(approval_id, decision).await?;
        Ok(self.pump())
    }

    pub async fn shutdown(&mut self) -> Result<Vec<HarnessEvent>, CoreError> {
        self.harness.shutdown().await?;
        self.pending_approval = None;
        Ok(self.pump())
    }

    /// Drain events that have arrived since the last pump. Returning them as
    /// well as applying them lets the core forward adapter activity to the shell.
    pub fn pump(&mut self) -> Vec<HarnessEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.harness.events().try_recv() {
            match &event {
                HarnessEvent::ApprovalRequested { request } => {
                    self.pending_approval = Some(request.approval_id.clone());
                }
                HarnessEvent::ApprovalResolved { approval_id, .. } => {
                    if self.pending_approval.as_ref() == Some(approval_id) {
                        self.pending_approval = None;
                    }
                }
                _ => {}
            }
            self.transcript.apply(&event);
            events.push(event);
        }
        events
    }

    pub fn transcript(&self) -> &Transcript {
        &self.transcript
    }

    pub fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    pub fn session_id(&self) -> Option<&SessionId> {
        self.session_id.as_ref()
    }

    pub fn pending_approval(&self) -> Option<&ApprovalId> {
        self.pending_approval.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use agent_harness::fake_harness::{FakeHarness, FakeStep, FakeTurn};
    use agent_harness::{ApprovalId, Decision, FileDiff, ToolStatus};

    use super::*;
    use crate::test_support::TestDirectory;

    #[tokio::test]
    async fn a_session_folds_streamed_events_into_one_transcript()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;
        let turn = FakeTurn::new(vec![
            FakeStep::Message("hello".to_string()),
            FakeStep::ToolCall {
                tool_call_id: "tool-1".to_string(),
                title: "Read file".to_string(),
                kind: "read".to_string(),
                status: ToolStatus::Completed,
                output: None,
                diffs: Vec::<FileDiff>::new(),
            },
        ]);
        let mut session = HarnessSession::new(Box::new(FakeHarness::one_turn(turn)));
        session
            .start(SpawnSpec {
                program: "fake".to_string(),
                cwd: directory.path().to_path_buf(),
                ..SpawnSpec::default()
            })
            .await?;
        session.open_session(None).await?;

        session.prompt("go".to_string()).await?;

        assert_eq!(session.transcript().len(), 3);
        assert_eq!(
            session.transcript().entry(0).map(|entry| entry.summary()),
            Some("you: go".to_string())
        );
        assert_eq!(
            session.transcript().entry(1).map(|entry| entry.summary()),
            Some("agent: hello".to_string())
        );
        assert!(session.capabilities().tool_calls);
        Ok(())
    }

    #[tokio::test]
    async fn an_approval_is_pending_until_the_decision_resumes_it()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;
        let turn = FakeTurn::new(vec![
            FakeStep::approval("approval-1", "Run tests"),
            FakeStep::Message("done".to_string()),
        ]);
        let mut session = HarnessSession::new(Box::new(FakeHarness::one_turn(turn)));
        session
            .start(SpawnSpec {
                program: "fake".to_string(),
                cwd: directory.path().to_path_buf(),
                ..SpawnSpec::default()
            })
            .await?;
        session.open_session(None).await?;

        session.prompt("go".to_string()).await?;
        assert_eq!(
            session.pending_approval().map(ApprovalId::as_str),
            Some("approval-1")
        );

        session
            .respond(ApprovalId::new("approval-1"), Decision::Allow)
            .await?;
        assert!(session.pending_approval().is_none());
        assert_eq!(
            session.transcript().entry(1).map(|entry| entry.summary()),
            Some("approval: Run tests (Allow)".to_string())
        );
        Ok(())
    }
}
