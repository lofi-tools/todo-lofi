//! The core's boundary around an agent adapter.
//!
//! `agent-harness` owns the protocol contract and transcript model. This module
//! owns the session lifecycle: start, open, prompt, pause on approval, cancel and
//! shutdown. It does not know which CLI is underneath.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use agent_harness::{
    AdapterTier, AgentDiagnostics, AgentHarness, AgentModels, AgentTerminal, ApprovalId,
    ApprovalRequest, Capabilities, Decision, Entry, EntryKind, FailureKind, HarnessError,
    HarnessEvent, ItemId, ProjectApprovalPolicy, PromptInput, SessionApproval, SessionId,
    SpawnSpec, Transcript, TranscriptChange,
};
use async_trait::async_trait;

use crate::session::SessionRecord;
use crate::store::{ProjectApproval, Store, checksum};
use crate::{Agent, AgentTier, CoreError, ProjectId, Task};

pub(crate) const MAX_EVENTS_PER_PUMP: usize = 256;
const HIDDEN_DIAGNOSTIC: &str =
    "Agent diagnostic output is hidden because launch environment values are configured.";

fn present(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

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

    fn environment_for_project(
        &self,
        _agent: Agent,
        _project: &std::path::Path,
    ) -> Result<BTreeMap<String, String>, CoreError> {
        Ok(BTreeMap::new())
    }

    /// The app-selected default model for `agent` in `project`, if any.
    fn default_model_for_project(
        &self,
        _agent: Agent,
        _project: &std::path::Path,
    ) -> Result<Option<String>, CoreError> {
        Ok(None)
    }

    /// Agents to try, in order, when `agent` cannot launch.
    fn fallback_agents(&self, _agent: Agent) -> Vec<Agent> {
        Vec::new()
    }

    fn launch_for_task(
        &self,
        _task_id: &crate::TaskId,
        agent: Agent,
        cwd: PathBuf,
    ) -> Result<(Box<dyn AgentHarness>, SpawnSpec), CoreError> {
        self.launch(agent, cwd)
    }

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
            Agent::Pi => Err(CoreError::HarnessMissing {
                report: "Pi RPC executable is unavailable".into(),
            }),
            Agent::Claude => Err(CoreError::HarnessMissing {
                report: "Claude Code ACP executable is unavailable".into(),
            }),
            Agent::Freebuff => Err(CoreError::HarnessMissing {
                report: "Freebuff executable is unavailable".into(),
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
    dirty_entries: BTreeSet<usize>,
    agent_terminals: Vec<AgentTerminal>,
    pending_approval: Option<ApprovalId>,
    pending_questions: BTreeSet<String>,
    answering_questions: BTreeSet<String>,
    cancelling: bool,
    failed: bool,
    closed: bool,
    cleanup_complete: bool,
    pending_event: Option<HarnessEvent>,
    hide_diagnostics: bool,
}

impl HarnessSession {
    pub fn new(harness: Box<dyn AgentHarness>) -> Self {
        Self {
            harness,
            capabilities: Capabilities::default(),
            session_id: None,
            transcript: Transcript::default(),
            dirty_entries: BTreeSet::new(),
            agent_terminals: Vec::new(),
            pending_approval: None,
            pending_questions: BTreeSet::new(),
            answering_questions: BTreeSet::new(),
            cancelling: false,
            failed: false,
            closed: false,
            cleanup_complete: false,
            pending_event: None,
            hide_diagnostics: false,
        }
    }

    pub(crate) fn set_project_approval_policy(&mut self, policy: Arc<dyn ProjectApprovalPolicy>) {
        self.harness.set_project_approval_policy(policy);
    }

    pub(crate) fn set_default_model(&mut self, model: Option<String>) {
        self.harness.set_default_model(model);
    }

    pub async fn start(&mut self, spec: SpawnSpec) -> Result<Capabilities, CoreError> {
        self.hide_diagnostics = !spec.environment.is_empty();
        self.capabilities = self
            .harness
            .start(spec)
            .await
            .map_err(|error| self.safe_error(error))?;
        Ok(self.capabilities.clone())
    }

    pub async fn open_session(
        &mut self,
        resume: Option<SessionId>,
    ) -> Result<SessionId, CoreError> {
        let fresh = resume.is_none();
        let session_id = self
            .harness
            .open_session(resume)
            .await
            .map_err(|error| self.safe_error(error))?;
        let replay = self.harness.take_replayed_transcript();
        if fresh {
            // An adapter can reuse item IDs in a fresh session.
            self.transcript.clear_adapter_row_keys();
        }
        if self.transcript.is_empty() && !replay.is_empty() {
            self.restore_transcript(replay);
        }
        self.session_id = Some(session_id.clone());
        self.capabilities.mode_picker = self.harness.modes().is_some();
        self.capabilities.model_picker = self.harness.models().is_some();
        Ok(session_id)
    }

    pub fn modes(&self) -> Option<agent_harness::AgentModes> {
        if self.closed {
            return None;
        }
        self.harness.modes()
    }

    pub fn models(&self) -> Option<AgentModels> {
        if self.closed {
            return None;
        }
        self.harness.models()
    }

    pub async fn set_mode(&mut self, mode: &str) -> Result<Vec<HarnessEvent>, CoreError> {
        if self.closed {
            return Err(HarnessError::ModeUnavailable.into());
        }
        self.harness
            .set_mode(mode)
            .await
            .map_err(|error| self.safe_error(error))?;
        self.capabilities.mode_picker = self.modes().is_some();
        Ok(self.pump())
    }

    /// Record the user's prompt, ask the adapter, then fold an available event batch.
    pub async fn prompt(&mut self, mut input: PromptInput) -> Result<Vec<HarnessEvent>, CoreError> {
        if let Some(mode) = input.mode.take() {
            self.harness
                .set_mode(&mode)
                .await
                .map_err(|error| self.safe_error(error))?;
            self.capabilities.mode_picker = self.modes().is_some();
        }
        // Completed messages stay separate even if the adapter reuses an item ID.
        self.transcript.clear_streaming_row_keys();
        let attachments = input
            .attachments
            .iter()
            .filter_map(|file| file.metadata.clone())
            .collect();
        let change = self
            .transcript
            .push_user_message_with_attachments(input.text.clone(), attachments);
        self.track_entry(change);
        self.harness
            .prompt(input)
            .await
            .map_err(|error| self.safe_error(error))?;
        Ok(self.pump())
    }

    pub async fn cancel(&mut self) -> Result<Vec<HarnessEvent>, CoreError> {
        if !self.capabilities.cancel {
            return Err(HarnessError::CancelUnavailable.into());
        }
        if !self.failed && !self.closed && !self.cancelling {
            self.harness
                .cancel()
                .await
                .map_err(|error| self.safe_error(error))?;
            self.cancelling = true;
        }
        Ok(self.pump())
    }

    pub async fn respond(
        &mut self,
        approval_id: ApprovalId,
        decision: Decision,
    ) -> Result<Vec<HarnessEvent>, CoreError> {
        self.harness
            .respond(approval_id, decision)
            .await
            .map_err(|error| self.safe_error(error))?;
        Ok(self.pump())
    }

    pub async fn answer_question(
        &mut self,
        question_id: &str,
        answer: &str,
    ) -> Result<Vec<HarnessEvent>, CoreError> {
        if self.closed
            || self.failed
            || self.cancelling
            || !self.pending_questions.contains(question_id)
        {
            return Err(
                HarnessError::Protocol("Question is no longer awaiting an answer".into()).into(),
            );
        }
        if self.answering_questions.contains(question_id) {
            return Err(
                HarnessError::Protocol("Question answer delivery is not confirmed".into()).into(),
            );
        }
        if answer.len() > 64 * 1024 {
            return Err(HarnessError::Protocol(
                "Question answer exceeds the supported size".into(),
            )
            .into());
        }
        self.answering_questions.insert(question_id.to_owned());
        self.harness
            .answer_question(question_id, answer)
            .await
            .map_err(|error| self.safe_error(error))?;
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
            self.harness
                .reset_session_approval(id)
                .await
                .map_err(|error| self.safe_error(error))?;
        }
        Ok(())
    }

    pub async fn shutdown(&mut self) -> Result<Vec<HarnessEvent>, CoreError> {
        if self.cleanup_complete {
            return Ok(Vec::new());
        }
        self.harness
            .shutdown()
            .await
            .map_err(|error| self.safe_error(error))?;
        self.cleanup_complete = true;
        self.closed = true;
        self.pending_approval = None;
        self.pending_questions.clear();
        self.answering_questions.clear();
        self.agent_terminals.clear();
        Ok(self.pump_with_limit(usize::MAX))
    }

    /// Fold a bounded batch of available events, preserving the rest for the next wakeup.
    pub fn pump(&mut self) -> Vec<HarnessEvent> {
        self.pump_with_limit(MAX_EVENTS_PER_PUMP)
    }

    pub(crate) fn pump_existing(&mut self) -> Vec<HarnessEvent> {
        let pending = usize::from(self.pending_event.is_some());
        let available = self.harness.events().len();
        self.pump_with_limit(pending.saturating_add(available))
    }

    fn pump_with_limit(&mut self, limit: usize) -> Vec<HarnessEvent> {
        let mut events = Vec::new();
        while events.len() < limit {
            let Some(mut event) = self
                .pending_event
                .take()
                .or_else(|| self.harness.events().try_recv().ok())
            else {
                break;
            };
            if self.hide_diagnostics {
                match &mut event {
                    HarnessEvent::Exited { stderr, .. } if !stderr.is_empty() => {
                        *stderr = HIDDEN_DIAGNOSTIC.into()
                    }
                    HarnessEvent::ConnectionFailed { text, .. }
                    | HarnessEvent::TurnError { text, .. }
                    | HarnessEvent::ApprovalPolicyError { text }
                    | HarnessEvent::AgentTerminalError { text, .. } => {
                        *text = HIDDEN_DIAGNOSTIC.into()
                    }
                    _ => {}
                }
            }
            match &event {
                HarnessEvent::AgentTerminalStarted { id, title, .. } => {
                    self.capabilities.agent_terminals = true;
                    self.agent_terminals.push(AgentTerminal {
                        id: id.clone(),
                        title: title.clone(),
                        live: true,
                        exit_code: None,
                    });
                }
                HarnessEvent::AgentTerminalExited { id, exit_code, .. } => {
                    if let Some(terminal) = self
                        .agent_terminals
                        .iter_mut()
                        .find(|terminal| terminal.id == *id)
                    {
                        terminal.live = false;
                        terminal.exit_code = exit_code.or(terminal.exit_code);
                    }
                }
                HarnessEvent::AgentTerminalRemoved { id, .. } => {
                    self.agent_terminals.retain(|terminal| terminal.id != *id);
                }
                HarnessEvent::ToolCall {
                    terminal_id: Some(id),
                    title,
                    ..
                } => {
                    if let Some(terminal) = self
                        .agent_terminals
                        .iter_mut()
                        .find(|terminal| terminal.id == *id)
                    {
                        terminal.title.clone_from(title);
                    }
                }
                HarnessEvent::ApprovalRequested { request } => {
                    self.capabilities.approvals = true;
                    self.pending_approval = Some(request.approval_id.clone());
                }
                HarnessEvent::ApprovalResolved { approval_id, .. }
                | HarnessEvent::ApprovalWithdrawn { approval_id } => {
                    if self.pending_approval.as_ref() == Some(approval_id) {
                        self.pending_approval = None;
                    }
                }
                HarnessEvent::QuestionRequested { question_id, .. } => {
                    self.pending_questions.insert(question_id.clone());
                }
                HarnessEvent::QuestionAnswered { question_id, .. } => {
                    self.pending_questions.remove(question_id);
                    self.answering_questions.remove(question_id);
                }
                HarnessEvent::TurnFinished { .. } => {
                    self.cancelling = false;
                    self.pending_approval = None;
                    self.pending_questions.clear();
                    self.answering_questions.clear();
                }
                HarnessEvent::Exited { .. } | HarnessEvent::ConnectionFailed { .. } => {
                    self.cancelling = false;
                    self.pending_questions.clear();
                    self.answering_questions.clear();
                    if let Some(approval_id) = self.pending_approval.take() {
                        let change = self
                            .transcript
                            .apply(&HarnessEvent::ApprovalWithdrawn { approval_id });
                        self.track_entry(change);
                    }
                    self.closed = true;
                    self.agent_terminals.clear();
                }
                _ => {}
            }
            let change = self.transcript.apply(&event);
            if change.appended
                && matches!(
                    event,
                    HarnessEvent::TurnError { .. } | HarnessEvent::ConnectionFailed { .. }
                )
                && let Some(index) = change.changed
            {
                let diagnostics = self.harness.diagnostics();
                self.transcript.set_failure_context(
                    index,
                    diagnostics.model,
                    present(diagnostics.protocol),
                );
            }
            self.track_entry(change);
            events.push(event);
        }
        if self.closed
            || events.iter().any(|event| {
                matches!(
                    event,
                    HarnessEvent::ModeChanged { .. } | HarnessEvent::ModelsChanged { .. }
                )
            })
        {
            self.capabilities.mode_picker = self.modes().is_some();
            self.capabilities.model_picker = self.models().is_some();
        }
        events
    }

    pub(crate) fn arm_events(&mut self, cx: &mut std::task::Context<'_>) -> bool {
        if self.closed {
            return false;
        }
        if self.pending_event.is_some() {
            return true;
        }
        match self.harness.events().poll_recv(cx) {
            std::task::Poll::Ready(Some(event)) => {
                self.pending_event = Some(event);
                true
            }
            _ => false,
        }
    }

    pub fn transcript(&self) -> &Transcript {
        &self.transcript
    }

    fn track_entry(&mut self, change: TranscriptChange) {
        if let Some(index) = change.changed {
            self.dirty_entries.insert(index);
        }
        if change.appended
            && let Some(index) = self.transcript.len().checked_sub(1)
        {
            self.dirty_entries.insert(index);
        }
    }

    pub(crate) async fn persist_transcript(
        &mut self,
        store: &dyn Store,
        record: &SessionRecord,
        settled: bool,
        task: Option<&Task>,
    ) -> Result<bool, CoreError> {
        let transcript = &self.transcript;
        let can_save = |entry: &Entry| {
            settled
                || matches!(
                    entry.kind,
                    EntryKind::UserMessage { .. }
                        | EntryKind::Failure { .. }
                        | EntryKind::Notice { is_error: true, .. }
                        | EntryKind::Permission {
                            decision: Some(_),
                            ..
                        }
                        | EntryKind::Permission {
                            withdrawn: true,
                            ..
                        }
                )
        };
        let entries: Vec<_> = self
            .dirty_entries
            .iter()
            .filter_map(|index| transcript.entry(*index))
            .filter(|entry| can_save(entry))
            .collect();
        if entries.is_empty() {
            if let Some(task) = task {
                store.save_task(task).await?;
            }
            return Ok(false);
        }
        if let Some(task) = task {
            store
                .save_task_with_transcript(task, record, &entries)
                .await?;
        } else {
            store.save_transcript_entries(record, &entries).await?;
        }
        self.dirty_entries.retain(|index| {
            transcript
                .entry(*index)
                .is_some_and(|entry| !can_save(entry))
        });
        Ok(true)
    }

    #[allow(dead_code)]
    pub(crate) async fn persist_terminal_prompt(
        &mut self,
        store: &dyn Store,
        record: &SessionRecord,
        task: &Task,
        item_id: ItemId,
        text: String,
    ) -> Result<(), CoreError> {
        if self.closed || self.failed || self.cancelling || self.session_id.is_none() {
            return Err(HarnessError::Protocol(
                "terminal session is not ready for a prompt".into(),
            )
            .into());
        }
        if task.id != record.task_id
            || task.state != crate::TaskState::Working
            || text.trim().is_empty()
        {
            return Err(HarnessError::Protocol("invalid terminal prompt admission".into()).into());
        }
        if self.transcript.entries().iter().any(|entry| {
            matches!(entry.kind, EntryKind::UserMessage { .. })
                && entry.source_id.as_deref() == Some(item_id.as_str())
        }) {
            return Err(
                HarnessError::Protocol("terminal prompt was already admitted".into()).into(),
            );
        }
        let previous_entries = self.transcript.entries().to_vec();
        let previous_dirty = self.dirty_entries.clone();
        let change = self
            .transcript
            .apply(&HarnessEvent::TerminalPrompt { item_id, text });
        self.track_entry(change);
        if let Err(error) = self
            .persist_transcript(store, record, false, Some(task))
            .await
        {
            self.transcript.restore(previous_entries);
            self.dirty_entries = previous_dirty;
            return Err(error);
        }
        Ok(())
    }

    pub fn agent_terminals(&self) -> &[AgentTerminal] {
        &self.agent_terminals
    }

    /// Record a core-owned observation. Used when the core has to tell the user
    /// something the adapter cannot report, such as a lost session.
    pub fn note(&mut self, text: impl Into<String>, is_error: bool) {
        let change = self.transcript.push_notice(text, is_error);
        self.track_entry(change);
    }

    /// Replace the transcript, e.g. with rows carried over from a session that
    /// never started, so its failure row stays visible.
    pub fn restore_transcript(&mut self, entries: impl IntoIterator<Item = Entry>) {
        self.transcript.restore(entries);
        self.dirty_entries = (0..self.transcript.len()).collect();
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
        let change = self.transcript.push_failure(kind, text, detail);
        if let Some(index) = change.changed {
            let diagnostics = self.harness.diagnostics();
            self.transcript.set_failure_context(
                index,
                diagnostics.model,
                present(diagnostics.protocol),
            );
        }
        self.track_entry(change);
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }

    pub fn needs_shutdown(&self) -> bool {
        !self.cleanup_complete
    }

    pub fn has_failed(&self) -> bool {
        self.failed
    }

    pub fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    pub fn diagnostics(&self) -> AgentDiagnostics {
        let mut diagnostics = self.harness.diagnostics();
        if self.hide_diagnostics {
            if !diagnostics.stderr_tail.is_empty() {
                diagnostics.stderr_tail = HIDDEN_DIAGNOSTIC.into();
            }
            if diagnostics.fallback_reason.is_some() {
                diagnostics.fallback_reason = Some(HIDDEN_DIAGNOSTIC.into());
            }
        }
        diagnostics
    }

    fn safe_error(&self, error: HarnessError) -> HarnessError {
        if self.hide_diagnostics
            && let Some(kind) = FailureKind::classify(&error)
        {
            HarnessError::Reported {
                kind,
                message: HIDDEN_DIAGNOSTIC.into(),
            }
        } else {
            error
        }
    }

    pub fn tier(&self) -> AgentTier {
        match self.harness.tier() {
            AdapterTier::Native => AgentTier::Native,
            AdapterTier::Acp => AgentTier::Acp,
            AdapterTier::TerminalWrapper => AgentTier::TerminalWrapper,
        }
    }

    pub fn session_id(&self) -> Option<&SessionId> {
        self.session_id.as_ref()
    }

    pub fn pending_approval(&self) -> Option<&ApprovalId> {
        self.pending_approval.as_ref().filter(|_| !self.cancelling)
    }

    pub fn has_pending_questions(&self) -> bool {
        !self.closed && !self.cancelling && !self.pending_questions.is_empty()
    }

    pub fn is_cancelling(&self) -> bool {
        self.cancelling
    }
}

impl crate::Core {
    #[allow(dead_code)]
    pub(crate) async fn admit_terminal_turn(
        &mut self,
        task_id: &crate::TaskId,
        item_id: ItemId,
        text: String,
    ) -> Result<(), CoreError> {
        let mut task = self.task(task_id)?.clone();
        if !matches!(
            task.state,
            crate::TaskState::Ready | crate::TaskState::Review
        ) || self.turn_baselines.contains_key(task_id)
            || self
                .queued_prompts
                .get(task_id)
                .is_some_and(|queue| !queue.is_empty())
        {
            return Err(CoreError::TaskNotReady {
                id: task_id.as_str().to_owned(),
                state: task.state.as_str(),
            });
        }
        let worktree = task
            .worktree_path
            .as_ref()
            .ok_or_else(|| HarnessError::Protocol("terminal task has no worktree".into()))?;
        let record = self.session_records.get(task_id).cloned().ok_or_else(|| {
            CoreError::SessionMissing {
                id: task_id.as_str().to_owned(),
            }
        })?;
        if record.is_ended() {
            return Err(HarnessError::Protocol("terminal session has already ended".into()).into());
        }
        self.task_workspace(task_id)?;
        self.task_list(&task.project_id)?;
        let baseline = crate::git::GitService.turn_baseline(worktree)?;
        task.set_state(crate::TaskState::Working)?;
        task.error = None;
        let session = self
            .sessions
            .get_mut(task_id)
            .ok_or_else(|| CoreError::SessionMissing {
                id: task_id.as_str().to_owned(),
            })?;
        session
            .persist_terminal_prompt(&*self.store, &record, &task, item_id, text)
            .await?;
        self.tasks.insert(task_id.clone(), task);
        self.turn_baselines.insert(task_id.clone(), baseline);
        if let Err(error) = self.publish_task(task_id) {
            tracing::error!(%error, "could not publish admitted terminal turn");
        }
        Ok(())
    }

    async fn fail_terminal_turn(&mut self, task_id: &crate::TaskId) -> Result<(), CoreError> {
        self.turn_baselines.remove(task_id);
        let message = "Terminal input could not be delivered; the turn may have started.";
        if let Some(session) = self.sessions.get_mut(task_id) {
            session.push_failure(FailureKind::Protocol, message, None);
        }
        self.set_task_state(task_id, crate::TaskState::Failed)?;
        if let Some(task) = self.tasks.get_mut(task_id) {
            task.error = Some(message.into());
        }
        self.save_task(task_id).await?;
        self.publish_task(task_id)?;
        Ok(())
    }
}

pub struct FreebuffLiveCapture {
    worktree: PathBuf,
    identity: crate::git::CaptureGitIdentity,
    session: Arc<agent_harness::freebuff_capture::CaptureSession>,
    complete: bool,
}

impl FreebuffLiveCapture {
    pub fn begin(
        workspace: &std::path::Path,
        worktree: PathBuf,
        scenario: &str,
    ) -> Result<Self, CoreError> {
        let writer = agent_harness::freebuff_capture::CaptureWriter::create(workspace, scenario)?;
        let session = Arc::new(agent_harness::freebuff_capture::CaptureSession::new(writer));
        let identity = begin_worktree_capture(&worktree, &session)?;
        Ok(Self {
            worktree,
            identity,
            session,
            complete: false,
        })
    }

    pub fn session(&self) -> Arc<agent_harness::freebuff_capture::CaptureSession> {
        self.session.clone()
    }

    /// The caller must first stop the process and confirm that the PTY reader drained.
    pub fn finish_after_drain(mut self) -> Result<(), CoreError> {
        capture_git_worktree_effects(&self.worktree, &self.identity, &self.session)?;
        self.session.finish()?;
        self.complete = true;
        Ok(())
    }
}

impl Drop for FreebuffLiveCapture {
    fn drop(&mut self) {
        if !self.complete
            && let Err(error) = self.session.fail()
        {
            tracing::error!(%error, "could not invalidate incomplete Freebuff capture");
        }
    }
}

#[allow(dead_code)]
fn begin_worktree_capture(
    root: &std::path::Path,
    capture: &agent_harness::freebuff_capture::CaptureSession,
) -> Result<crate::git::CaptureGitIdentity, CoreError> {
    let result = (|| {
        if !crate::git::GitService.status(root)?.is_empty() {
            return Err(HarnessError::Protocol("capture worktree must start clean".into()).into());
        }
        reject_ignored_worktree_files(root)?;
        let identity = crate::git::GitService.capture_git_identity(root)?;
        let encoded = serde_json::to_value(&identity).map_err(|error| {
            HarnessError::Protocol(format!("could not encode capture identity: {error}"))
        })?;
        capture.record_and_apply(
            agent_harness::freebuff_capture::CaptureEvent::WorktreeBase { identity: encoded },
            || Ok(((), Vec::new())),
        )?;
        Ok(identity)
    })();
    if result.is_err()
        && let Err(error) = capture.fail()
    {
        return Err(HarnessError::Protocol(format!(
            "could not invalidate failed capture: {error}"
        ))
        .into());
    }
    result
}

#[allow(dead_code)]
pub(crate) fn capture_worktree_effects(
    root: &std::path::Path,
    paths: &[PathBuf],
    capture: &agent_harness::freebuff_capture::CaptureSession,
) -> Result<(), CoreError> {
    let result = capture_worktree_effects_inner(root, paths, capture);
    if let Err(error) = result {
        if let Err(failure) = capture.fail() {
            return Err(HarnessError::Protocol(format!(
                "{error}; could not invalidate capture: {failure}"
            ))
            .into());
        }
        return Err(error);
    }
    Ok(())
}

#[allow(dead_code)]
pub(crate) fn capture_git_worktree_effects(
    root: &std::path::Path,
    initial_identity: &crate::git::CaptureGitIdentity,
    capture: &agent_harness::freebuff_capture::CaptureSession,
) -> Result<(), CoreError> {
    let result = (|| {
        if crate::git::GitService.capture_git_identity(root)? != *initial_identity {
            return Err(HarnessError::Protocol(
                "worktree Git identity or index changed during capture".into(),
            )
            .into());
        }
        inspect_ignored_worktree_files(root, true)?;
        let mut paths = BTreeSet::new();
        for entry in crate::git::GitService.status(root)? {
            if !entry.actionable {
                return Err(
                    HarnessError::Protocol("worktree has a non-UTF-8 changed path".into()).into(),
                );
            }
            if entry.index != " " && entry.index != "?" {
                return Err(HarnessError::Protocol(
                    "staged worktree changes cannot yet be captured".into(),
                )
                .into());
            }
            paths.insert(PathBuf::from(entry.path));
            if let Some(original) = entry.original_path {
                paths.insert(PathBuf::from(original));
            }
        }
        let project_id = std::path::Path::new(".freebuff/project-id");
        match std::fs::symlink_metadata(root.join(project_id)) {
            Ok(_) => {
                let status = std::process::Command::new("git")
                    .arg("-C")
                    .arg(root)
                    .args(["--no-optional-locks", "check-ignore", "--quiet", "--"])
                    .arg(project_id)
                    .status()
                    .map_err(|error| {
                        HarnessError::Protocol(format!(
                            "could not check Freebuff project metadata: {error}"
                        ))
                    })?;
                match status.code() {
                    Some(0) => {
                        paths.insert(project_id.to_path_buf());
                    }
                    Some(1) => {}
                    _ => {
                        return Err(HarnessError::Protocol(format!(
                            "could not check Freebuff project metadata: {status}"
                        ))
                        .into());
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(HarnessError::Protocol(format!(
                    "could not inspect Freebuff project metadata: {error}"
                ))
                .into());
            }
        }
        capture_worktree_effects_inner(root, &paths.into_iter().collect::<Vec<_>>(), capture)?;
        if crate::git::GitService.capture_git_identity(root)? != *initial_identity {
            return Err(HarnessError::Protocol(
                "worktree Git identity or index changed while collecting effects".into(),
            )
            .into());
        }
        Ok(())
    })();
    if let Err(error) = result {
        if let Err(failure) = capture.fail() {
            return Err(HarnessError::Protocol(format!(
                "{error}; could not invalidate capture: {failure}"
            ))
            .into());
        }
        return Err(error);
    }
    Ok(())
}

fn reject_ignored_worktree_files(root: &std::path::Path) -> Result<(), CoreError> {
    inspect_ignored_worktree_files(root, false)
}

fn inspect_ignored_worktree_files(
    root: &std::path::Path,
    allow_project_id: bool,
) -> Result<(), CoreError> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let mut command = Command::new("git");
    command.arg("-C").arg(root).args([
        "--no-optional-locks",
        "ls-files",
        "--others",
        "--ignored",
        "--exclude-standard",
        "-z",
    ]);
    if allow_project_id {
        command.args(["--", ".", ":(exclude).freebuff/project-id"]);
    } else {
        command.arg("--directory");
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| {
            HarnessError::Protocol(format!("could not inspect ignored worktree files: {error}"))
        })?;
    let read = match child.stdout.take() {
        Some(mut output) => output.read(&mut [0u8; 1]),
        None => Err(std::io::Error::other("missing Git output")),
    };
    if !matches!(read, Ok(0)) {
        match child.try_wait() {
            Ok(Some(_)) => {}
            Ok(None) => {
                if let Err(error) = child.kill() {
                    if !matches!(child.try_wait(), Ok(Some(_))) {
                        return Err(HarnessError::Protocol(format!(
                            "could not stop ignored-file inspection: {error}"
                        ))
                        .into());
                    }
                }
            }
            Err(error) => {
                return Err(HarnessError::Protocol(format!(
                    "could not inspect Git process: {error}"
                ))
                .into());
            }
        }
    }
    let status = child.wait().map_err(|error| {
        HarnessError::Protocol(format!(
            "could not wait for ignored-file inspection: {error}"
        ))
    })?;
    match read {
        Ok(0) if status.success() => Ok(()),
        Ok(0) => {
            Err(HarnessError::Protocol(format!("ignored-file inspection failed: {status}")).into())
        }
        Ok(_) => Err(HarnessError::Protocol(
            "ignored worktree files cannot yet be captured".into(),
        )
        .into()),
        Err(error) => Err(HarnessError::Protocol(format!(
            "could not read ignored-file inspection: {error}"
        ))
        .into()),
    }
}

fn capture_worktree_effects_inner(
    root: &std::path::Path,
    paths: &[PathBuf],
    capture: &agent_harness::freebuff_capture::CaptureSession,
) -> Result<(), CoreError> {
    use agent_harness::freebuff_capture::CaptureEvent;
    use std::io::Read;
    use std::path::Component;
    const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
    const MAX_TOTAL_BYTES: usize = 64 * 1024 * 1024;

    let root_metadata = std::fs::symlink_metadata(root).map_err(|error| {
        HarnessError::Protocol(format!("could not inspect capture worktree: {error}"))
    })?;
    if !root_metadata.is_dir() {
        return Err(HarnessError::Protocol("capture worktree is not a directory".into()).into());
    }
    let mut total_bytes = 0usize;
    let mut seen = BTreeSet::new();
    for path in paths {
        if path.as_os_str().is_empty()
            || path.to_str().is_none()
            || path
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
            || !seen.insert(path.clone())
        {
            return Err(HarnessError::Protocol(
                "invalid or duplicate captured worktree path".into(),
            )
            .into());
        }
        let file = match crate::files::open_file(root, path) {
            Ok(file) => Some(file),
            Err(crate::files::FileError::Io(error))
                if error.kind() == std::io::ErrorKind::NotFound =>
            {
                None
            }
            Err(error) => {
                return Err(HarnessError::Protocol(format!(
                    "could not safely read captured worktree file {}: {error}",
                    path.display()
                ))
                .into());
            }
        };
        let event = if let Some(file) = file {
            let metadata = file.metadata().map_err(|error| {
                HarnessError::Protocol(format!(
                    "could not inspect captured worktree file {}: {error}",
                    path.display()
                ))
            })?;
            if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES {
                return Err(HarnessError::Protocol(format!(
                    "unsupported or oversized captured worktree file {}",
                    path.display()
                ))
                .into());
            }
            #[cfg(unix)]
            let mode = {
                use std::os::unix::fs::PermissionsExt;
                Some(metadata.permissions().mode() & 0o777)
            };
            #[cfg(not(unix))]
            let mode = None;
            let mut contents = Vec::new();
            file.take(MAX_FILE_BYTES + 1)
                .read_to_end(&mut contents)
                .map_err(|error| {
                    HarnessError::Protocol(format!(
                        "could not read captured worktree file {}: {error}",
                        path.display()
                    ))
                })?;
            if contents.len() as u64 > MAX_FILE_BYTES {
                return Err(HarnessError::Protocol(
                    "captured worktree file grew beyond the limit".into(),
                )
                .into());
            }
            total_bytes = total_bytes
                .checked_add(contents.len())
                .filter(|total| *total <= MAX_TOTAL_BYTES)
                .ok_or_else(|| {
                    HarnessError::Protocol("captured worktree effects exceed the size limit".into())
                })?;
            CaptureEvent::WorktreeFile {
                path: path.clone(),
                contents,
                mode,
            }
        } else {
            CaptureEvent::WorktreeDeleted { path: path.clone() }
        };
        capture.record_and_apply(event, || Ok(((), Vec::new())))?;
    }
    Ok(())
}

#[allow(dead_code)]
enum WorktreeReplayEffect {
    File {
        path: PathBuf,
        contents: Vec<u8>,
        mode: u32,
    },
    Deleted {
        path: PathBuf,
    },
}

#[allow(dead_code)]
fn plan_worktree_replay(
    capture_path: &std::path::Path,
) -> Result<(crate::git::CaptureGitIdentity, Vec<WorktreeReplayEffect>), CoreError> {
    use agent_harness::freebuff_capture::{CaptureEvent, visit_complete_capture};
    let mut effects = Vec::new();
    let mut paths = BTreeSet::new();
    let mut total_bytes = 0usize;
    let mut identity = None;
    let mut input_seen = false;
    visit_complete_capture(capture_path, |record| {
        let effect = match record.event {
            CaptureEvent::WorktreeBase { identity: value } => {
                if identity.is_some() || input_seen || !effects.is_empty() {
                    return Err(HarnessError::Protocol(
                        "capture base identity is duplicated or late".into(),
                    ));
                }
                identity = Some(serde_json::from_value(value).map_err(|error| {
                    HarnessError::Protocol(format!("invalid capture base identity: {error}"))
                })?);
                None
            }
            CaptureEvent::Input { .. } => {
                if identity.is_none() {
                    return Err(HarnessError::Protocol(
                        "capture input precedes its base identity".into(),
                    ));
                }
                input_seen = true;
                None
            }
            CaptureEvent::WorktreeFile {
                path,
                contents,
                mode,
            } => {
                if identity.is_none() {
                    return Err(HarnessError::Protocol(
                        "worktree effect precedes its base identity".into(),
                    ));
                }
                let mode = mode.filter(|mode| *mode <= 0o777).ok_or_else(|| {
                    HarnessError::Protocol("captured file has no supported mode".into())
                })?;
                if contents.len() > 16 * 1024 * 1024 {
                    return Err(HarnessError::Protocol(
                        "captured file exceeds replay limit".into(),
                    ));
                }
                total_bytes = total_bytes
                    .checked_add(contents.len())
                    .filter(|total| *total <= 64 * 1024 * 1024)
                    .ok_or_else(|| {
                        HarnessError::Protocol("captured effects exceed replay limit".into())
                    })?;
                if !paths.insert(path.clone()) {
                    return Err(HarnessError::Protocol(
                        "duplicate captured worktree path".into(),
                    ));
                }
                Some(WorktreeReplayEffect::File {
                    path,
                    contents,
                    mode,
                })
            }
            CaptureEvent::WorktreeDeleted { path } => {
                if identity.is_none() {
                    return Err(HarnessError::Protocol(
                        "worktree effect precedes its base identity".into(),
                    ));
                }
                if !paths.insert(path.clone()) {
                    return Err(HarnessError::Protocol(
                        "duplicate captured worktree path".into(),
                    ));
                }
                Some(WorktreeReplayEffect::Deleted { path })
            }
            CaptureEvent::WorktreeEffect { .. } => {
                return Err(HarnessError::Protocol(
                    "legacy worktree effects cannot be replayed safely".into(),
                ));
            }
            _ => None,
        };
        if let Some(effect) = effect {
            if effects.len() >= 10_000 {
                return Err(HarnessError::Protocol(
                    "too many captured worktree effects".into(),
                ));
            }
            effects.push(effect);
        }
        Ok(())
    })?;
    let identity = identity
        .ok_or_else(|| HarnessError::Protocol("capture has no worktree base identity".into()))?;
    Ok((identity, effects))
}

#[cfg(unix)]
pub fn replay_worktree_effects(
    root: &std::path::Path,
    capture_path: &std::path::Path,
) -> Result<(), CoreError> {
    use rustix::fs::{
        AtFlags, FileType, Mode, OFlags, fchmod, mkdirat, open, openat, renameat, statat, unlinkat,
    };
    use std::io::Write;
    use std::path::Component;

    let io_error = |source: std::io::Error| CoreError::Io {
        path: root.display().to_string(),
        source,
    };
    let (expected, effects) = plan_worktree_replay(capture_path)?;
    if crate::git::GitService.capture_git_identity(root)? != expected
        || !crate::git::GitService.status(root)?.is_empty()
    {
        return Err(HarnessError::Protocol(
            "replay worktree does not match the clean capture base".into(),
        )
        .into());
    }
    reject_ignored_worktree_files(root)?;
    let paths: BTreeSet<_> = effects
        .iter()
        .map(|effect| match effect {
            WorktreeReplayEffect::File { path, .. } | WorktreeReplayEffect::Deleted { path } => {
                path.clone()
            }
        })
        .collect();
    for path in &paths {
        if path
            .ancestors()
            .skip(1)
            .any(|ancestor| paths.contains(ancestor))
        {
            return Err(HarnessError::Protocol("captured worktree paths overlap".into()).into());
        }
    }
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    for (applied, effect) in effects.into_iter().enumerate() {
        let effect_path = match &effect {
            WorktreeReplayEffect::File { path, .. } | WorktreeReplayEffect::Deleted { path } => {
                path.clone()
            }
        };
        let result = (|| -> Result<(), CoreError> {
            let (path, create_parent) = match &effect {
                WorktreeReplayEffect::File { path, .. } => (path, true),
                WorktreeReplayEffect::Deleted { path } => (path, false),
            };
            let mut directory =
                open(root, flags, Mode::empty()).map_err(|error| io_error(error.into()))?;
            let parent = path
                .parent()
                .ok_or_else(|| HarnessError::Protocol("invalid replay parent".into()))?;
            let mut missing = false;
            for component in parent.components() {
                let Component::Normal(name) = component else {
                    return Err(HarnessError::Protocol("invalid replay path".into()).into());
                };
                let next = match openat(&directory, name, flags, Mode::empty()) {
                    Ok(next) => next,
                    Err(rustix::io::Errno::NOENT) if create_parent => {
                        match mkdirat(&directory, name, Mode::from_raw_mode(0o755)) {
                            Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                            Err(error) => return Err(io_error(error.into())),
                        }
                        openat(&directory, name, flags, Mode::empty())
                            .map_err(|error| io_error(error.into()))?
                    }
                    Err(rustix::io::Errno::NOENT) => {
                        missing = true;
                        break;
                    }
                    Err(error) => return Err(io_error(error.into())),
                };
                directory = next;
            }
            if missing {
                return Ok(());
            }
            let name = path
                .file_name()
                .ok_or_else(|| HarnessError::Protocol("invalid replay filename".into()))?
                .to_os_string();
            match statat(&directory, name.as_os_str(), AtFlags::SYMLINK_NOFOLLOW) {
                Ok(stat) if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile => {
                    return Err(HarnessError::Protocol(
                        "replay target is not a regular file".into(),
                    )
                    .into());
                }
                Ok(_) | Err(rustix::io::Errno::NOENT) => {}
                Err(error) => return Err(io_error(error.into())),
            }
            match effect {
                WorktreeReplayEffect::Deleted { .. } => {
                    match unlinkat(&directory, name.as_os_str(), AtFlags::empty()) {
                        Ok(()) | Err(rustix::io::Errno::NOENT) => {}
                        Err(error) => return Err(io_error(error.into())),
                    }
                }
                WorktreeReplayEffect::File { contents, mode, .. } => {
                    let temporary = format!(".shepherd-replay-{}", uuid::Uuid::new_v4());
                    let fd = openat(
                        &directory,
                        temporary.as_str(),
                        OFlags::WRONLY
                            | OFlags::CREATE
                            | OFlags::EXCL
                            | OFlags::NOFOLLOW
                            | OFlags::CLOEXEC,
                        Mode::from_raw_mode(0o600),
                    )
                    .map_err(|error| io_error(error.into()))?;
                    let mut file = std::fs::File::from(fd);
                    let written = file.write_all(&contents).and_then(|()| {
                        fchmod(&file, Mode::from_raw_mode(mode as _)).map_err(std::io::Error::from)
                    });
                    if let Err(error) = written {
                        if let Err(cleanup) =
                            unlinkat(&directory, temporary.as_str(), AtFlags::empty())
                        {
                            return Err(HarnessError::Protocol(format!(
                                "replay write failed: {error}; temporary cleanup failed: {cleanup}"
                            ))
                            .into());
                        }
                        return Err(io_error(error));
                    }
                    if let Err(error) =
                        renameat(&directory, temporary.as_str(), &directory, name.as_os_str())
                    {
                        if let Err(cleanup) =
                            unlinkat(&directory, temporary.as_str(), AtFlags::empty())
                        {
                            return Err(HarnessError::Protocol(format!(
                                "replay rename failed: {error}; temporary cleanup failed: {cleanup}"
                            ))
                            .into());
                        }
                        return Err(io_error(error.into()));
                    }
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            return Err(HarnessError::Protocol(format!(
                "replay failed at {} after {applied} effects; the worktree may be partially modified: {error}",
                effect_path.display()
            )).into());
        }
    }
    Ok(())
}

pub(crate) struct TerminalSubmitGate {
    core: Arc<tokio::sync::Mutex<crate::Core>>,
    failed: tokio::sync::Mutex<bool>,
}

#[allow(dead_code)]
impl TerminalSubmitGate {
    pub(crate) fn new(core: Arc<tokio::sync::Mutex<crate::Core>>) -> Self {
        Self {
            core,
            failed: tokio::sync::Mutex::new(false),
        }
    }

    pub(crate) async fn write_unsubmitted<F, Fut>(&self, write: F) -> Result<(), CoreError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<(), CoreError>>,
    {
        let mut failed = self.failed.lock().await;
        if *failed {
            return Err(HarnessError::Protocol("terminal input gate has failed".into()).into());
        }
        let result = write().await;
        if result.is_err() {
            *failed = true;
        }
        result
    }

    pub(crate) async fn submit<F, Fut>(
        &self,
        task_id: &crate::TaskId,
        item_id: ItemId,
        text: String,
        write: F,
    ) -> Result<(), CoreError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<(), CoreError>>,
    {
        let mut failed = self.failed.lock().await;
        if *failed {
            return Err(HarnessError::Protocol("terminal input gate has failed".into()).into());
        }
        self.core
            .lock()
            .await
            .admit_terminal_turn(task_id, item_id, text)
            .await?;
        if let Err(error) = write().await {
            *failed = true;
            if let Err(cleanup) = self.core.lock().await.fail_terminal_turn(task_id).await {
                return Err(HarnessError::Protocol(format!(
                    "terminal submit failed: {error}; could not persist failure: {cleanup}"
                ))
                .into());
            }
            return Err(error);
        }
        Ok(())
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

    #[cfg(unix)]
    #[cfg(feature = "git")]
    #[test]
    fn capture_lifecycle_records_and_replays_worktree_effects()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::test_support::TestRepository;
        use agent_harness::freebuff_capture::{CaptureEvent, read_complete_capture};
        let repository = TestRepository::new()?;
        let cache = TestDirectory::new()?;
        let abandoned =
            FreebuffLiveCapture::begin(cache.path(), repository.path().to_path_buf(), "abandoned")?;
        drop(abandoned);
        assert!(
            read_complete_capture(&cache.join(".cache/shepherd-live/freebuff/abandoned.jsonl"))
                .is_err()
        );
        let capture =
            FreebuffLiveCapture::begin(cache.path(), repository.path().to_path_buf(), "lifecycle")?;
        capture.session().record_and_apply(
            CaptureEvent::Output {
                bytes: b"fixture reply".to_vec(),
            },
            || Ok(((), Vec::new())),
        )?;
        repository.write_file("README.md", "captured\n")?;
        capture.finish_after_drain()?;
        let path = cache.join(".cache/shepherd-live/freebuff/lifecycle.jsonl");
        let records = read_complete_capture(&path)?;
        assert!(records.iter().any(|record| matches!(&record.event, CaptureEvent::WorktreeFile { path, contents, .. } if path == std::path::Path::new("README.md") && contents == b"captured\n")));
        repository.write_file("README.md", "initial\n")?;
        replay_worktree_effects(repository.path(), &path)?;
        assert_eq!(
            std::fs::read(repository.path().join("README.md"))?,
            b"captured\n"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[cfg(feature = "git")]
    #[test]
    fn replay_applies_effects_only_to_a_clean_matching_worktree()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::test_support::TestRepository;
        use agent_harness::freebuff_capture::{CaptureEvent, CaptureSession, CaptureWriter};
        let repository = TestRepository::new()?;
        let cache = TestDirectory::new()?;
        let capture = CaptureSession::new(CaptureWriter::create(cache.path(), "replay")?);
        begin_worktree_capture(repository.path(), &capture)?;
        capture.record_and_apply(
            CaptureEvent::WorktreeFile {
                path: "nested/new.txt".into(),
                contents: b"hello".to_vec(),
                mode: Some(0o640),
            },
            || Ok(((), Vec::new())),
        )?;
        capture.record_and_apply(
            CaptureEvent::WorktreeDeleted {
                path: "README.md".into(),
            },
            || Ok(((), Vec::new())),
        )?;
        capture.finish()?;
        let path = cache.join(".cache/shepherd-live/freebuff/replay.jsonl");
        let switched = std::process::Command::new("git")
            .arg("-C")
            .arg(repository.path())
            .args(["switch", "-c", "wrong-base"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?;
        assert!(switched.success());
        assert!(replay_worktree_effects(repository.path(), &path).is_err());
        assert!(!repository.path().join("nested/new.txt").exists());
        let restored = std::process::Command::new("git")
            .arg("-C")
            .arg(repository.path())
            .args(["switch", "main"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?;
        assert!(restored.success());
        replay_worktree_effects(repository.path(), &path)?;
        assert_eq!(
            std::fs::read(repository.path().join("nested/new.txt"))?,
            b"hello"
        );
        assert!(!repository.path().join("README.md").exists());
        assert!(replay_worktree_effects(repository.path(), &path).is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[cfg(feature = "git")]
    #[test]
    fn replay_reports_partial_application_on_failure() -> Result<(), Box<dyn std::error::Error>> {
        use agent_harness::freebuff_capture::{CaptureEvent, CaptureSession, CaptureWriter};
        let repository = crate::test_support::TestRepository::new()?;
        let cache = TestDirectory::new()?;
        let capture = CaptureSession::new(CaptureWriter::create(cache.path(), "partial")?);
        begin_worktree_capture(repository.path(), &capture)?;
        for path in ["first.txt", "README.md/child.txt"] {
            capture.record_and_apply(
                CaptureEvent::WorktreeFile {
                    path: path.into(),
                    contents: b"captured".to_vec(),
                    mode: Some(0o644),
                },
                || Ok(((), Vec::new())),
            )?;
        }
        capture.finish()?;
        let error = replay_worktree_effects(
            repository.path(),
            &cache.join(".cache/shepherd-live/freebuff/partial.jsonl"),
        )
        .err()
        .ok_or("expected replay failure")?
        .to_string();
        assert!(error.contains("README.md/child.txt"));
        assert!(error.contains("after 1 effects"));
        assert!(error.contains("worktree may be partially modified"));
        assert_eq!(
            std::fs::read(repository.path().join("first.txt"))?,
            b"captured"
        );
        Ok(())
    }

    #[cfg(feature = "git")]
    #[test]
    fn replay_planning_requires_a_complete_capture_without_duplicate_paths()
    -> Result<(), Box<dyn std::error::Error>> {
        use agent_harness::freebuff_capture::{CaptureEvent, CaptureSession, CaptureWriter};
        let repository = crate::test_support::TestRepository::new()?;
        let directory = TestDirectory::new()?;
        let capture = CaptureSession::new(CaptureWriter::create(directory.path(), "plan")?);
        begin_worktree_capture(repository.path(), &capture)?;
        let path = directory.join(".cache/shepherd-live/freebuff/plan.jsonl");
        capture.record_and_apply(
            CaptureEvent::WorktreeFile {
                path: "file".into(),
                contents: b"data".to_vec(),
                mode: Some(0o644),
            },
            || Ok(((), Vec::new())),
        )?;
        assert!(plan_worktree_replay(&path).is_err());
        capture.record_and_apply(
            CaptureEvent::WorktreeDeleted {
                path: "file".into(),
            },
            || Ok(((), Vec::new())),
        )?;
        capture.finish()?;
        assert!(plan_worktree_replay(&path).is_err());
        let valid = CaptureSession::new(CaptureWriter::create(directory.path(), "valid")?);
        begin_worktree_capture(repository.path(), &valid)?;
        valid.record_and_apply(
            CaptureEvent::WorktreeFile {
                path: "file".into(),
                contents: b"data".to_vec(),
                mode: Some(0o644),
            },
            || Ok(((), Vec::new())),
        )?;
        valid.finish()?;
        let (_, effects) =
            plan_worktree_replay(&directory.join(".cache/shepherd-live/freebuff/valid.jsonl"))?;
        assert_eq!(effects.len(), 1);
        Ok(())
    }

    #[cfg(feature = "git")]
    #[test]
    fn discovers_unstaged_and_untracked_worktree_effects() -> Result<(), Box<dyn std::error::Error>>
    {
        use crate::test_support::TestRepository;
        use agent_harness::freebuff_capture::{
            CaptureEvent, CaptureSession, CaptureWriter, read_capture,
        };
        let repository = TestRepository::new()?;
        let cache = TestDirectory::new()?;
        let identity = crate::git::GitService.capture_git_identity(repository.path())?;
        repository.write_file("README.md", "changed\n")?;
        repository.write_file("new.txt", "new\n")?;
        let capture = CaptureSession::new(CaptureWriter::create(cache.path(), "git-effects")?);
        capture_git_worktree_effects(repository.path(), &identity, &capture)?;
        let records = read_capture(&cache.join(".cache/shepherd-live/freebuff/git-effects.jsonl"))?;
        assert!(records.iter().any(|record| matches!(&record.event, CaptureEvent::WorktreeFile { path, contents, .. } if path == std::path::Path::new("README.md") && contents == b"changed\n")));
        assert!(records.iter().any(|record| matches!(&record.event, CaptureEvent::WorktreeFile { path, contents, .. } if path == std::path::Path::new("new.txt") && contents == b"new\n")));
        Ok(())
    }

    #[cfg(feature = "git")]
    #[test]
    #[cfg(unix)]
    fn captures_and_replays_freebuff_project_id_without_other_ignored_files()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::test_support::TestRepository;
        let repository = TestRepository::new()?;
        let cache = TestDirectory::new()?;
        repository.write_file(".gitignore", ".freebuff/\n")?;
        for arguments in [
            ["add", ".gitignore"].as_slice(),
            ["commit", "-m", "ignore"].as_slice(),
        ] {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(repository.path())
                .args(arguments)
                .stdout(std::process::Stdio::null())
                .status()?;
            assert!(status.success());
        }
        let capture =
            FreebuffLiveCapture::begin(cache.path(), repository.path().into(), "project-id")?;
        repository.write_file(".freebuff/project-id", "fixture-id\n")?;
        capture.finish_after_drain()?;
        std::fs::remove_dir_all(repository.path().join(".freebuff"))?;
        replay_worktree_effects(
            repository.path(),
            &cache.join(".cache/shepherd-live/freebuff/project-id.jsonl"),
        )?;
        assert_eq!(
            std::fs::read(repository.path().join(".freebuff/project-id"))?,
            b"fixture-id\n"
        );

        repository.write_file(".freebuff/other", "ignored secret")?;
        assert!(inspect_ignored_worktree_files(repository.path(), true).is_err());
        Ok(())
    }

    #[cfg(feature = "git")]
    #[test]
    fn rejects_staged_and_ignored_worktree_effects() -> Result<(), Box<dyn std::error::Error>> {
        use crate::test_support::TestRepository;
        use agent_harness::freebuff_capture::{CaptureSession, CaptureWriter};
        let repository = TestRepository::new()?;
        let cache = TestDirectory::new()?;
        let identity = crate::git::GitService.capture_git_identity(repository.path())?;
        repository.write_file("README.md", "staged\n")?;
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(repository.path())
            .args(["add", "README.md"])
            .status()?;
        assert!(status.success());
        let capture = CaptureSession::new(CaptureWriter::create(cache.path(), "staged")?);
        assert!(capture_git_worktree_effects(repository.path(), &identity, &capture).is_err());
        assert!(capture.finish().is_err());
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(repository.path())
            .args(["reset", "--hard", "HEAD"])
            .stdout(std::process::Stdio::null())
            .status()?;
        assert!(status.success());
        repository.write_file(".gitignore", "ignored.txt\n")?;
        repository.write_file("ignored.txt", "secret\n")?;
        let capture = CaptureSession::new(CaptureWriter::create(cache.path(), "ignored")?);
        assert!(capture_git_worktree_effects(repository.path(), &identity, &capture).is_err());
        assert!(capture.finish().is_err());
        Ok(())
    }

    #[cfg(feature = "git")]
    #[test]
    fn rejects_branch_changes_during_effect_capture() -> Result<(), Box<dyn std::error::Error>> {
        use crate::test_support::TestRepository;
        use agent_harness::freebuff_capture::{CaptureSession, CaptureWriter};
        let repository = TestRepository::new()?;
        let cache = TestDirectory::new()?;
        let identity = crate::git::GitService.capture_git_identity(repository.path())?;
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(repository.path())
            .args(["switch", "-c", "other"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?;
        assert!(status.success());
        let capture = CaptureSession::new(CaptureWriter::create(cache.path(), "branch")?);
        assert!(capture_git_worktree_effects(repository.path(), &identity, &capture).is_err());
        assert!(capture.finish().is_err());
        Ok(())
    }

    #[test]
    fn worktree_effect_capture_records_files_and_deletions()
    -> Result<(), Box<dyn std::error::Error>> {
        use agent_harness::freebuff_capture::{
            CaptureEvent, CaptureSession, CaptureWriter, read_complete_capture,
        };
        let directory = TestDirectory::new()?;
        std::fs::write(directory.join("created.txt"), b"created")?;
        let capture = CaptureSession::new(CaptureWriter::create(directory.path(), "effects")?);
        capture_worktree_effects(
            directory.path(),
            &[PathBuf::from("created.txt"), PathBuf::from("deleted.txt")],
            &capture,
        )?;
        capture.finish()?;
        let records =
            read_complete_capture(&directory.join(".cache/shepherd-live/freebuff/effects.jsonl"))?;
        assert!(records.iter().any(|record| matches!(&record.event, CaptureEvent::WorktreeFile { path, contents, .. } if path == std::path::Path::new("created.txt") && contents == b"created")));
        assert!(records.iter().any(|record| matches!(&record.event, CaptureEvent::WorktreeDeleted { path } if path == std::path::Path::new("deleted.txt"))));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_worktree_effect_invalidates_capture() -> Result<(), Box<dyn std::error::Error>> {
        use agent_harness::freebuff_capture::{CaptureSession, CaptureWriter};
        let directory = TestDirectory::new()?;
        std::os::unix::fs::symlink("/", directory.join("outside"))?;
        let capture = CaptureSession::new(CaptureWriter::create(directory.path(), "unsafe")?);
        assert!(
            capture_worktree_effects(
                directory.path(),
                &[PathBuf::from("outside/etc/passwd")],
                &capture
            )
            .is_err()
        );
        assert!(capture.finish().is_err());
        Ok(())
    }

    #[cfg(feature = "git")]
    #[tokio::test]
    async fn terminal_turn_admission_records_baseline_before_changes()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::test_support::TestRepository;
        let repository = TestRepository::new()?;
        let mut core = crate::Core::new();
        let project = Project::new(repository.path().to_path_buf());
        core.store.save_project(&project).await?;
        core.project_order.push(project.id.clone());
        core.projects.insert(project.id.clone(), project.clone());
        let mut task = Task::draft(project.id.clone(), "task", repository.path());
        task.state = crate::TaskState::Ready;
        task.worktree_path = Some(repository.path().to_path_buf());
        core.store.save_task(&task).await?;
        let id = task.id.clone();
        core.tasks.insert(id.clone(), task);
        let mut session = HarnessSession::new(Box::new(FakeHarness::new(Vec::new())));
        session.start(SpawnSpec::default()).await?;
        session.open_session(None).await?;
        let record = SessionRecord::started(id.clone(), "fixture", None);
        core.store.save_session(&record).await?;
        core.sessions.insert(id.clone(), session);
        core.session_records.insert(id.clone(), record);

        let core = Arc::new(tokio::sync::Mutex::new(core));
        let gate = TerminalSubmitGate::new(core.clone());
        let submitting_core = core.clone();
        let submitting_id = id.clone();
        gate.submit(
            &id,
            ItemId::new("terminal-turn"),
            "make a change".into(),
            || async {
                assert_eq!(
                    submitting_core.lock().await.task(&submitting_id)?.state,
                    crate::TaskState::Working
                );
                repository
                    .write_file("README.md", "changed\n")
                    .map_err(|error| HarnessError::Protocol(error.to_string()))?;
                Ok(())
            },
        )
        .await?;
        let mut core = core.lock().await;
        assert_eq!(core.task(&id)?.state, crate::TaskState::Working);
        assert_eq!(core.store.load_transcript(&id).await?.len(), 1);
        assert_eq!(
            core.state_after_completed_turn(&id)?,
            crate::TaskState::Review
        );
        assert!(
            core.admit_terminal_turn(&id, ItemId::new("duplicate"), "next".into())
                .await
                .is_err()
        );
        Ok(())
    }

    #[tokio::test]
    async fn terminal_submit_does_not_write_when_admission_fails() {
        let core = Arc::new(tokio::sync::Mutex::new(crate::Core::new()));
        let gate = TerminalSubmitGate::new(core);
        let mut wrote = false;
        let result = gate
            .submit(
                &crate::TaskId::new("missing"),
                ItemId::new("prompt"),
                "hello".into(),
                || async {
                    wrote = true;
                    Ok(())
                },
            )
            .await;
        assert!(result.is_err());
        assert!(!wrote);
    }

    #[tokio::test]
    async fn terminal_prompt_admission_persists_once_and_rolls_back_on_failure()
    -> Result<(), Box<dyn std::error::Error>> {
        let database = TestDatabase::new()?;
        let store = TursoStore::open(&database.uri()).await?;
        let project = Project::new(database.path().join("project"));
        store.save_project(&project).await?;
        let mut task = Task::draft(project.id.clone(), "task", database.path());
        task.state = crate::TaskState::Working;
        store.save_task(&task).await?;
        let record = SessionRecord::started(task.id.clone(), "fixture", None);
        let mut session = HarnessSession::new(Box::new(FakeHarness::new(Vec::new())));
        session.start(SpawnSpec::default()).await?;
        session.open_session(None).await?;
        let failed = session
            .persist_terminal_prompt(
                &store,
                &record,
                &task,
                ItemId::new("prompt-1"),
                "hello".into(),
            )
            .await;
        assert!(failed.is_err());
        assert!(session.transcript().is_empty());
        store.save_session(&record).await?;
        session
            .persist_terminal_prompt(
                &store,
                &record,
                &task,
                ItemId::new("prompt-1"),
                "hello".into(),
            )
            .await?;
        assert_eq!(store.load_transcript(&task.id).await?.len(), 1);
        assert!(
            session
                .persist_terminal_prompt(
                    &store,
                    &record,
                    &task,
                    ItemId::new("prompt-1"),
                    "hello".into()
                )
                .await
                .is_err()
        );
        assert_eq!(session.transcript().len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn configured_environment_hides_agent_diagnostic_output()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;
        let turn = FakeTurn::new(vec![
            FakeStep::TurnError {
                kind: FailureKind::Protocol,
                text: "fixture-private-value".into(),
            },
            FakeStep::TerminalError {
                id: "terminal".into(),
                text: "fixture-private-value".into(),
            },
            FakeStep::Exit {
                code: 1,
                stderr: "fixture-private-value".into(),
            },
        ]);
        let mut session = HarnessSession::new(Box::new(FakeHarness::one_turn(turn)));
        session
            .start(SpawnSpec {
                program: "fake".into(),
                cwd: directory.path().to_owned(),
                environment: BTreeMap::from([("TOKEN".into(), "fixture-private-value".into())]),
                ..SpawnSpec::default()
            })
            .await?;
        session.open_session(None).await?;
        let events = session.prompt(PromptInput::text("go")).await?;
        assert!(events.iter().any(|event| matches!(event, HarnessEvent::TurnError { text, .. } if text == HIDDEN_DIAGNOSTIC)));
        assert!(events.iter().any(|event| matches!(event, HarnessEvent::Exited { stderr, .. } if stderr == HIDDEN_DIAGNOSTIC)));
        assert!(events.iter().any(|event| matches!(event, HarnessEvent::AgentTerminalError { text, .. } if text == HIDDEN_DIAGNOSTIC)));
        assert!(!format!("{events:?}").contains("fixture-private-value"));
        let error = session.safe_error(HarnessError::TransportClosed(
            "fixture-private-value".into(),
        ));
        assert!(!error.to_string().contains("fixture-private-value"));
        assert_eq!(FailureKind::classify(&error), Some(FailureKind::Protocol));
        Ok(())
    }

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
            stream_permit: None,
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

        session.prompt(PromptInput::text("go")).await?;

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
    #[cfg(unix)]
    async fn exec_backpressure_preserves_messages_and_tool_calls_through_the_core()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt as _;
        let directory = TestDirectory::new()?;
        let executable = directory.path().join("exec-fixture");
        std::fs::write(
            &executable,
            r##"#!/bin/sh
number=0
while [ "$number" -lt 150 ]; do
  printf '{"id":"%s","msg":{"type":"agent_message","message":"message %s"}}\n' "$number" "$number"
  printf '{"id":"%s","msg":{"type":"exec_command_begin","call_id":"call-%s","command":"command %s"}}\n' "$number" "$number" "$number"
  number=$((number + 1))
done
"##,
        )?;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))?;
        let mut session = HarnessSession::new(Box::new(agent_harness::codex::ExecHarness::new()));
        session
            .start(SpawnSpec {
                program: executable.to_string_lossy().into_owned(),
                cwd: directory.path().to_path_buf(),
                ..Default::default()
            })
            .await?;
        session.open_session(None).await?;
        drop(session.prompt(PromptInput::text("go")).await?);
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while session.harness.events().len() < 64 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await?;
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let events = session.pump();
                let finished = events.iter().any(|event| {
                    matches!(
                        event,
                        HarnessEvent::TurnFinished {
                            reason: agent_harness::TurnFinishReason::Completed
                        }
                    )
                });
                drop(events);
                if finished {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await?;
        assert_eq!(session.transcript().len(), 301);
        for index in 0..150 {
            assert_eq!(
                session
                    .transcript()
                    .entry(index * 2 + 1)
                    .map(|entry| entry.summary()),
                Some(format!("agent: message {index}"))
            );
            assert!(matches!(
                session.transcript().entry(index * 2 + 2).map(|entry| &entry.kind),
                Some(agent_harness::EntryKind::ToolCall { tool_call_id, title, .. })
                    if tool_call_id == &format!("call-{index}") && title == &format!("command {index}")
            ));
        }
        drop(session.shutdown().await?);
        Ok(())
    }

    #[tokio::test]
    async fn command_batches_preserve_bursts_and_shutdown_drains_the_remainder()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;
        let count = MAX_EVENTS_PER_PUMP * 2 + 1;
        for shutdown in [false, true] {
            let turn = FakeTurn::new(
                (0..count)
                    .map(|index| FakeStep::Message(format!("message {index}")))
                    .collect(),
            );
            let mut session = HarnessSession::new(Box::new(FakeHarness::one_turn(turn)));
            session
                .start(SpawnSpec {
                    program: "fake".into(),
                    cwd: directory.path().to_path_buf(),
                    ..SpawnSpec::default()
                })
                .await?;
            session.open_session(None).await?;
            let first = session.prompt(PromptInput::text("go")).await?;
            assert_eq!(first.len(), MAX_EVENTS_PER_PUMP);
            let mut events = first;
            if shutdown {
                events.extend(session.shutdown().await?);
            } else {
                loop {
                    let batch = session.pump();
                    if batch.is_empty() {
                        break;
                    }
                    assert!(batch.len() <= MAX_EVENTS_PER_PUMP);
                    events.extend(batch);
                }
            }
            let messages = events
                .iter()
                .filter_map(|event| match event {
                    HarnessEvent::Delta { text, .. } => Some(text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(
                messages,
                (0..count)
                    .map(|index| format!("message {index}"))
                    .collect::<Vec<_>>()
            );
            assert!(
                events
                    .iter()
                    .any(|event| matches!(event, HarnessEvent::TurnFinished { .. }))
            );
        }
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

        session.prompt(PromptInput::text("go")).await?;
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
