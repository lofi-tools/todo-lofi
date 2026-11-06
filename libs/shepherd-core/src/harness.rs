//! The core's boundary around an agent adapter.
//!
//! `agent-harness` owns the protocol contract and transcript model. This module
//! owns the session lifecycle: start, open, prompt, pause on approval, cancel and
//! shutdown. It does not know which CLI is underneath.

use std::path::PathBuf;
use std::sync::Arc;

use agent_harness::{
    AdapterTier, AgentDiagnostics, AgentHarness, ApprovalId, ApprovalRequest, Capabilities,
    Decision, Entry, FailureKind, HarnessError, HarnessEvent, ProjectApprovalPolicy, PromptInput,
    SessionApproval, SessionId, SpawnSpec, Transcript,
};
use async_trait::async_trait;

use crate::store::{ProjectApproval, Store, checksum};
use crate::{Agent, AgentTier, CoreError, ProjectId};

pub(crate) struct ProjectApprovals {
    store: Arc<dyn Store>,
    project: ProjectId,
    agent: Agent,
    profile: String,
}

impl ProjectApprovals {
    pub(crate) fn new(
        store: Arc<dyn Store>,
        project: ProjectId,
        agent: Agent,
        spec: &SpawnSpec,
    ) -> Result<Self, HarnessError> {
        let profile = serde_json::to_string(&(agent, &spec.program, &spec.args, &spec.environment))
            .map_err(|error| {
                HarnessError::Protocol(format!("could not encode the approval profile: {error}"))
            })?;
        Ok(Self {
            store,
            project,
            agent,
            profile: checksum(&profile),
        })
    }

    fn id(&self, key: &str) -> String {
        checksum(&format!("acp-v1:{}:{key}", self.profile))
    }
}

#[async_trait]
impl ProjectApprovalPolicy for ProjectApprovals {
    async fn allows(&self, key: &str) -> Result<bool, String> {
        self.store
            .has_project_approval(&self.project, &self.id(key))
            .await
            .map_err(|error| error.to_string())
    }

    async fn remember(&self, key: &str, request: &ApprovalRequest) -> Result<(), String> {
        let grant = ProjectApproval {
            id: self.id(key),
            agent: self.agent,
            title: request.title.clone(),
            arguments: request.arguments.clone(),
            paths: request.paths.clone(),
        };
        self.store
            .save_project_approval(&self.project, &grant)
            .await
            .map_err(|error| error.to_string())
    }
}

/// Creates one adapter instance for a task. The shell or test supplies the
/// implementation; the core never chooses a concrete CLI here.
pub trait HarnessFactory: Send + Sync {
    fn create(&self) -> Result<Box<dyn AgentHarness>, CoreError>;

    fn launch(
        &self,
        agent: Agent,
        cwd: PathBuf,
    ) -> Result<(Box<dyn AgentHarness>, SpawnSpec), CoreError> {
        match agent {
            Agent::Codex => Ok((self.create()?, self.spawn_spec(cwd))),
            Agent::OpenCode => Err(CoreError::HarnessMissing {
                report: "OpenCode executable is unavailable".into(),
            }),
            Agent::Claude => Err(CoreError::HarnessMissing {
                report: "Claude Code ACP executable is unavailable".into(),
            }),
        }
    }

    /// The command to launch for a task running in `cwd`. The core stays
    /// CLI-agnostic, so a factory that wraps a specific executable supplies
    /// both the program and its subcommand here.
    fn spawn_spec(&self, cwd: PathBuf) -> SpawnSpec {
        SpawnSpec {
            program: "shepherd-agent".to_string(),
            cwd,
            ..SpawnSpec::default()
        }
    }
}

/// The factory a core built without an adapter gets. Agent work fails with a
/// clear error instead of appearing to start and then doing nothing.
pub struct UnavailableHarnessFactory;

impl HarnessFactory for UnavailableHarnessFactory {
    fn create(&self) -> Result<Box<dyn AgentHarness>, CoreError> {
        Err(CoreError::HarnessUnavailable)
    }
}

/// One live adapter session with its core-owned transcript.
pub struct HarnessSession {
    harness: Box<dyn AgentHarness>,
    capabilities: Capabilities,
    session_id: Option<SessionId>,
    transcript: Transcript,
    pending_approval: Option<ApprovalId>,
    cancelling: bool,
    failed: bool,
    closed: bool,
}

impl HarnessSession {
    pub fn new(harness: Box<dyn AgentHarness>) -> Self {
        Self {
            harness,
            capabilities: Capabilities::default(),
            session_id: None,
            transcript: Transcript::default(),
            pending_approval: None,
            cancelling: false,
            failed: false,
            closed: false,
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
        let replay = self.harness.take_replayed_transcript();
        if self.transcript.is_empty() && !replay.is_empty() {
            self.transcript.restore(replay);
        }
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
        if !self.failed && !self.closed && !self.cancelling {
            self.harness.cancel().await?;
            self.cancelling = true;
        }
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

    pub fn session_approvals(&self) -> Vec<SessionApproval> {
        if self.closed {
            return Vec::new();
        }
        self.harness.session_approvals()
    }

    pub async fn reset_session_approval(&mut self, id: &str) -> Result<(), CoreError> {
        if !self.closed {
            self.harness.reset_session_approval(id).await?;
        }
        Ok(())
    }

    pub async fn shutdown(&mut self) -> Result<Vec<HarnessEvent>, CoreError> {
        self.harness.shutdown().await?;
        self.closed = true;
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
                HarnessEvent::ApprovalResolved { approval_id, .. }
                | HarnessEvent::ApprovalWithdrawn { approval_id } => {
                    if self.pending_approval.as_ref() == Some(approval_id) {
                        self.pending_approval = None;
                    }
                }
                HarnessEvent::TurnFinished { .. } => {
                    self.cancelling = false;
                    self.pending_approval = None;
                }
                HarnessEvent::Exited { .. } => {
                    self.cancelling = false;
                    self.pending_approval = None;
                    self.closed = true;
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

    /// Record a core-owned observation. Used when the core has to tell the user
    /// something the adapter cannot report, such as a lost session.
    pub fn note(&mut self, text: impl Into<String>, is_error: bool) {
        self.transcript.push_notice(text, is_error);
    }

    /// Replace the transcript, e.g. with rows carried over from a session that
    /// never started, so its failure row stays visible.
    pub fn restore_transcript(&mut self, entries: impl IntoIterator<Item = Entry>) {
        self.transcript.restore(entries);
    }

    /// Record a failure row and mark the session unusable: the next prompt opens
    /// a fresh adapter process while the row stays in the transcript.
    pub fn push_failure(
        &mut self,
        kind: FailureKind,
        text: impl Into<String>,
        detail: Option<String>,
    ) {
        self.failed = true;
        self.transcript.push_failure(kind, text, detail);
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }

    pub fn has_failed(&self) -> bool {
        self.failed
    }

    pub fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    pub fn diagnostics(&self) -> AgentDiagnostics {
        self.harness.diagnostics()
    }

    pub fn tier(&self) -> AgentTier {
        match self.harness.tier() {
            AdapterTier::Native => AgentTier::Native,
            AdapterTier::Acp => AgentTier::Acp,
        }
    }

    pub fn session_id(&self) -> Option<&SessionId> {
        self.session_id.as_ref()
    }

    pub fn pending_approval(&self) -> Option<&ApprovalId> {
        self.pending_approval.as_ref().filter(|_| !self.cancelling)
    }

    pub fn is_cancelling(&self) -> bool {
        self.cancelling
    }
}

#[cfg(test)]
mod tests {
    use agent_harness::fake_harness::{FakeHarness, FakeStep, FakeTurn};
    use agent_harness::{ApprovalId, Decision, FileDiff, ToolCallId, ToolStatus};

    use super::*;
    use crate::Project;
    use crate::store::TursoStore;
    use crate::test_support::{TestDatabase, TestDirectory};

    #[tokio::test]
    async fn a_project_grant_survives_reopen_and_reset_is_scoped()
    -> Result<(), Box<dyn std::error::Error>> {
        let database = TestDatabase::new()?;
        let project = Project::new(database.path().join("first"));
        let other = Project::new(database.path().join("other"));
        let spec = SpawnSpec {
            program: "/bin/fixture".into(),
            ..SpawnSpec::default()
        };
        let request = ApprovalRequest {
            approval_id: ApprovalId::new("approval-1"),
            tool_call_id: ToolCallId::new("tool-1"),
            title: "Check status".into(),
            arguments: serde_json::json!({"command": "git status"}),
            paths: Vec::new(),
            patch: None,
            choices: None,
        };
        let key = "exact ACP input";
        {
            let store: Arc<dyn Store> = Arc::new(TursoStore::open(&database.uri()).await?);
            store.save_project(&project).await?;
            store.save_project(&other).await?;
            let policy =
                ProjectApprovals::new(store.clone(), project.id.clone(), Agent::Claude, &spec)?;
            let other_policy =
                ProjectApprovals::new(store, other.id.clone(), Agent::Claude, &spec)?;
            policy.remember(key, &request).await?;
            policy.remember(key, &request).await?;
            assert!(!other_policy.allows(key).await?);
            other_policy.remember(key, &request).await?;
        }

        let store: Arc<dyn Store> = Arc::new(TursoStore::open(&database.uri()).await?);
        let policy =
            ProjectApprovals::new(store.clone(), project.id.clone(), Agent::Claude, &spec)?;
        let other_policy =
            ProjectApprovals::new(store.clone(), other.id.clone(), Agent::Claude, &spec)?;
        let changed_spec = SpawnSpec {
            args: vec!["--other-profile".into()],
            ..spec.clone()
        };
        let changed = ProjectApprovals::new(
            store.clone(),
            project.id.clone(),
            Agent::Claude,
            &changed_spec,
        )?;
        let other_agent =
            ProjectApprovals::new(store.clone(), project.id.clone(), Agent::OpenCode, &spec)?;
        assert!(policy.allows(key).await?);
        assert!(!changed.allows(key).await?);
        assert!(!other_agent.allows(key).await?);
        assert!(!policy.allows("different ACP input").await?);
        let listed = store.project_approvals(&project.id).await?;
        assert_eq!(listed.len(), 1);
        let grant = listed.first().ok_or("missing persisted grant")?;
        assert_eq!(grant.arguments, request.arguments);
        store.reset_project_approval(&project.id, &grant.id).await?;
        store.reset_project_approval(&project.id, &grant.id).await?;
        assert!(store.project_approvals(&project.id).await?.is_empty());
        assert!(!policy.allows(key).await?);
        assert!(other_policy.allows(key).await?);
        Ok(())
    }

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
