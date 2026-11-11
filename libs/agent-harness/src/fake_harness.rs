//! A deterministic adapter used by the core's lifecycle tests.
//!
//! It has no child process and no sleeps. A scripted turn emits events
//! synchronously when `prompt` is called, so a failed test shows the exact
//! command/event order rather than timing noise.

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::{
    AgentDiagnostics, AgentHarness, ApprovalId, ApprovalRequest, Capabilities, Decision, DeltaKind,
    FailureKind, FileDiff, HarnessError, HarnessEvent, HarnessResult, ItemId, PlanRow, PromptInput,
    SessionId, SessionRoots, SpawnSpec, ToolCallId, ToolStatus, TurnFinishReason, TurnId,
};

/// One scripted step inside a turn.
#[derive(Clone, Debug)]
pub enum FakeStep {
    Message(String),
    Thought(String),
    TurnError {
        kind: FailureKind,
        text: String,
    },
    TerminalError {
        id: String,
        text: String,
    },
    ToolCall {
        tool_call_id: String,
        title: String,
        kind: String,
        status: ToolStatus,
        output: Option<String>,
        diffs: Vec<FileDiff>,
    },
    Approval(ApprovalRequest),
    Question {
        question_id: String,
        text: String,
    },
    Plan(Vec<PlanRow>),
    Usage {
        used: u64,
        size: u64,
        cost: Option<f64>,
    },
    WriteFile {
        path: PathBuf,
        contents: String,
    },
    /// The process dies before the turn finishes, the way a crash looks.
    Exit {
        code: i32,
        stderr: String,
    },
}

impl FakeStep {
    pub fn approval(id: &str, title: &str) -> Self {
        Self::Approval(ApprovalRequest {
            approval_id: ApprovalId::new(id),
            tool_call_id: ToolCallId::new(format!("tool-{id}")),
            title: title.to_string(),
            arguments: Value::Null,
            paths: Vec::new(),
            patch: None,
            choices: None,
            stream_permit: None,
        })
    }
}

/// One scripted turn and how it ends.
#[derive(Clone, Debug)]
pub struct FakeTurn {
    pub steps: Vec<FakeStep>,
    pub finish: TurnFinishReason,
}

impl FakeTurn {
    pub fn new(steps: Vec<FakeStep>) -> Self {
        Self {
            steps,
            finish: TurnFinishReason::Completed,
        }
    }
}

/// A failure the fake adapter reports instead of a turn, so the core's failure
/// rows are reachable without a broken install.
#[derive(Clone, Copy, Debug)]
pub enum FakeFailure {
    MissingProgram,
    UnsupportedVersion,
    AuthRequired,
}

impl FakeFailure {
    fn error(self) -> HarnessError {
        match self {
            Self::MissingProgram => HarnessError::MissingProgram {
                program: "codex".to_string(),
            },
            // The wording a server uses when it does not know a method, and the
            // one it uses when the caller is not signed in.
            Self::UnsupportedVersion => {
                HarnessError::Protocol("unknown method thread/start".to_string())
            }
            Self::AuthRequired => {
                HarnessError::Protocol("401 Unauthorized: run `codex login`".to_string())
            }
        }
    }
}

impl Default for FakeTurn {
    fn default() -> Self {
        Self::new(vec![FakeStep::Message("fake answer".to_string())])
    }
}

struct PendingApproval {
    request: ApprovalRequest,
    remaining: Vec<FakeStep>,
    /// The scripted offset of the first remaining step, so post-approval steps
    /// keep distinct item IDs from the steps before the approval.
    next_offset: usize,
    finish: TurnFinishReason,
}

struct PendingQuestion {
    question_id: String,
    remaining: Vec<FakeStep>,
    next_offset: usize,
    finish: TurnFinishReason,
}

/// A scripted adapter. Start it, open a session, then prompt it.
pub struct FakeHarness {
    turns: Vec<FakeTurn>,
    turn_index: usize,
    sender: mpsc::UnboundedSender<HarnessEvent>,
    receiver: mpsc::UnboundedReceiver<HarnessEvent>,
    cwd: Option<PathBuf>,
    session_id: Option<SessionId>,
    next_turn: u64,
    started: bool,
    turn_active: bool,
    pending: Option<PendingApproval>,
    pending_question: Option<PendingQuestion>,
    capabilities: Capabilities,
    start_failure: Option<FakeFailure>,
    prompt_failure: Option<FakeFailure>,
}

impl FakeHarness {
    pub fn new(turns: Vec<FakeTurn>) -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        Self {
            turns,
            turn_index: 0,
            sender,
            receiver,
            cwd: None,
            session_id: None,
            next_turn: 1,
            started: false,
            turn_active: false,
            pending: None,
            pending_question: None,
            capabilities: Capabilities::fake(),
            start_failure: None,
            prompt_failure: None,
        }
    }

    pub fn one_turn(turn: FakeTurn) -> Self {
        Self::new(vec![turn])
    }

    /// The same script without resume, to model an adapter that cannot reattach
    /// a stored thread.
    pub fn without_resume(mut self) -> Self {
        self.capabilities.resume = false;
        self
    }

    /// Fail every start with this error, the way a missing binary does.
    pub fn failing_start(mut self, failure: FakeFailure) -> Self {
        self.start_failure = Some(failure);
        self
    }

    /// Reject every prompt with this error, the way an expired sign-in does.
    pub fn failing_prompt(mut self, failure: FakeFailure) -> Self {
        self.prompt_failure = Some(failure);
        self
    }

    fn send(&self, event: HarnessEvent) -> HarnessResult<()> {
        self.sender
            .send(event)
            .map_err(|_| HarnessError::ChannelClosed)
    }

    fn emit_steps(
        &mut self,
        steps: &[FakeStep],
        offset_base: usize,
        finish: TurnFinishReason,
    ) -> HarnessResult<()> {
        for (offset, step) in steps.iter().enumerate() {
            let offset = offset_base + offset;
            match step {
                FakeStep::TurnError { kind, text } => {
                    self.send(HarnessEvent::TurnError {
                        kind: *kind,
                        text: text.clone(),
                        stream_permit: None,
                    })?;
                }
                FakeStep::TerminalError { id, text } => {
                    self.send(HarnessEvent::AgentTerminalError {
                        id: id.clone(),
                        text: text.clone(),
                        stream_permit: None,
                    })?;
                }
                FakeStep::Message(text) => {
                    self.send(HarnessEvent::Delta {
                        item_id: ItemId::new(format!("message-{}-{offset}", self.next_turn)),
                        kind: DeltaKind::Message,
                        text: text.clone(),
                        stream_permit: None,
                    })?;
                }
                FakeStep::Thought(text) => {
                    self.send(HarnessEvent::Delta {
                        item_id: ItemId::new(format!("thought-{}-{offset}", self.next_turn)),
                        kind: DeltaKind::Thought,
                        text: text.clone(),
                        stream_permit: None,
                    })?;
                }
                FakeStep::ToolCall {
                    tool_call_id,
                    title,
                    kind,
                    status,
                    output,
                    diffs,
                } => {
                    self.send(HarnessEvent::ToolCall {
                        tool_call_id: ToolCallId::new(tool_call_id.clone()),
                        title: title.clone(),
                        kind: kind.clone(),
                        status: *status,
                        output: output.clone(),
                        diffs: diffs.clone(),
                        terminal_id: None,
                        stream_permit: None,
                    })?;
                }
                FakeStep::Approval(request) => {
                    self.send(HarnessEvent::ApprovalRequested {
                        request: request.clone(),
                    })?;
                    self.pending = Some(PendingApproval {
                        request: request.clone(),
                        remaining: steps[offset - offset_base + 1..].to_vec(),
                        next_offset: offset + 1,
                        finish,
                    });
                    return Ok(());
                }
                FakeStep::Question { question_id, text } => {
                    self.send(HarnessEvent::QuestionRequested {
                        question_id: question_id.clone(),
                        text: text.clone(),
                    })?;
                    self.pending_question = Some(PendingQuestion {
                        question_id: question_id.clone(),
                        remaining: steps[offset - offset_base + 1..].to_vec(),
                        next_offset: offset + 1,
                        finish,
                    });
                    return Ok(());
                }
                FakeStep::Plan(rows) => {
                    self.send(HarnessEvent::Plan {
                        rows: rows.clone(),
                        stream_permit: None,
                    })?;
                }
                FakeStep::Usage { used, size, cost } => {
                    self.send(HarnessEvent::Usage {
                        used: *used,
                        size: *size,
                        cost: *cost,
                        stream_permit: None,
                    })?;
                }
                FakeStep::WriteFile { path, contents } => {
                    let cwd = self.cwd.as_ref().ok_or(HarnessError::NotStarted)?;
                    let target = if path.is_absolute() {
                        path.clone()
                    } else {
                        cwd.join(path)
                    };
                    if let Some(parent) = target.parent() {
                        std::fs::create_dir_all(parent).map_err(|source| {
                            HarnessError::WriteFile {
                                path: parent.display().to_string(),
                                source,
                            }
                        })?;
                    }
                    std::fs::write(&target, contents).map_err(|source| {
                        HarnessError::WriteFile {
                            path: target.display().to_string(),
                            source,
                        }
                    })?;
                }
                FakeStep::Exit { code, stderr } => {
                    self.turn_active = false;
                    self.pending = None;
                    self.pending_question = None;
                    self.send(HarnessEvent::Exited {
                        code: Some(*code),
                        stderr: stderr.clone(),
                    })?;
                    return Ok(());
                }
            }
        }

        self.finish_turn(finish)
    }

    fn finish_turn(&mut self, reason: TurnFinishReason) -> HarnessResult<()> {
        self.turn_active = false;
        self.send(HarnessEvent::TurnFinished { reason })
    }
}

#[async_trait]
impl AgentHarness for FakeHarness {
    fn events(&mut self) -> &mut mpsc::UnboundedReceiver<HarnessEvent> {
        &mut self.receiver
    }

    fn diagnostics(&self) -> AgentDiagnostics {
        AgentDiagnostics {
            command: "fake".to_string(),
            protocol: "fake-harness".to_string(),
            fallback_reason: None,
            model: None,
            reasoning_effort: None,
            stderr_tail: String::new(),
        }
    }

    async fn start(&mut self, spec: SpawnSpec) -> HarnessResult<Capabilities> {
        if let Some(failure) = self.start_failure {
            return Err(failure.error());
        }
        self.cwd = Some(spec.cwd);
        self.started = true;
        self.send(HarnessEvent::SessionState {
            state: crate::SessionState::Ready,
        })?;
        Ok(self.capabilities.clone())
    }

    async fn open_session(&mut self, resume: Option<SessionId>) -> HarnessResult<SessionId> {
        if !self.started {
            return Err(HarnessError::NotStarted);
        }
        let session_id = resume.unwrap_or_else(|| SessionId::new("fake-session-1"));
        self.session_id = Some(session_id.clone());
        Ok(session_id)
    }

    async fn prompt(&mut self, _input: PromptInput) -> HarnessResult<()> {
        if !self.started || self.session_id.is_none() {
            return Err(HarnessError::SessionMissing);
        }
        if self.pending.is_some() || self.pending_question.is_some() {
            return Err(HarnessError::ApprovalPending);
        }
        if let Some(failure) = self.prompt_failure {
            return Err(failure.error());
        }

        let turn = self
            .turns
            .get(self.turn_index)
            .cloned()
            .ok_or(HarnessError::ScriptExhausted)?;
        self.turn_index += 1;
        let turn_id = self.next_turn;
        self.next_turn += 1;
        self.turn_active = true;
        self.send(HarnessEvent::TurnStarted {
            turn_id: TurnId::new(format!("turn-{turn_id}")),
        })?;
        // The core records the user's prompt itself. An adapter echo would
        // duplicate it, so the fake does not send one.
        self.emit_steps(&turn.steps, 0, turn.finish)
    }

    async fn cancel(&mut self) -> HarnessResult<()> {
        if !self.turn_active {
            return Ok(());
        }
        if let Some(pending) = self.pending.take() {
            // A cancelled turn cannot leave an approval pending, so report the
            // implicit refusal the adapter would really send.
            self.send(HarnessEvent::ApprovalResolved {
                approval_id: pending.request.approval_id,
                decision: Decision::Deny,
                source: crate::ApprovalSource::Agent,
            })?;
        }
        self.pending_question = None;
        self.finish_turn(TurnFinishReason::Cancelled)
    }

    async fn respond(&mut self, approval_id: ApprovalId, decision: Decision) -> HarnessResult<()> {
        let allow = match decision {
            Decision::Allow => true,
            Decision::Deny => false,
            Decision::AllowSession | Decision::AllowProject => {
                return Err(HarnessError::ApprovalOptionUnavailable);
            }
        };
        let pending = self.pending.take().ok_or(HarnessError::NoPendingApproval)?;
        if pending.request.approval_id != approval_id {
            self.pending = Some(pending);
            return Err(HarnessError::NoPendingApproval);
        }
        self.send(HarnessEvent::ApprovalResolved {
            approval_id,
            decision,
            source: crate::ApprovalSource::User,
        })?;
        if allow {
            self.emit_steps(&pending.remaining, pending.next_offset, pending.finish)
        } else {
            self.finish_turn(TurnFinishReason::Cancelled)
        }
    }

    async fn answer_question(&mut self, question_id: &str, answer: &str) -> HarnessResult<()> {
        let pending = self
            .pending_question
            .take()
            .ok_or_else(|| HarnessError::Protocol("No question is awaiting an answer".into()))?;
        if pending.question_id != question_id {
            self.pending_question = Some(pending);
            return Err(HarnessError::Protocol("Question does not match".into()));
        }
        self.send(HarnessEvent::QuestionAnswered {
            question_id: question_id.to_owned(),
            answer: answer.to_owned(),
        })?;
        self.emit_steps(&pending.remaining, pending.next_offset, pending.finish)
    }

    async fn shutdown(&mut self) -> HarnessResult<()> {
        if !self.started {
            return Ok(());
        }
        self.started = false;
        self.turn_active = false;
        self.pending = None;
        self.pending_question = None;
        self.send(HarnessEvent::Exited {
            code: Some(0),
            stderr: String::new(),
        })
    }
}

/// Drain every event already sent by a fake harness.
pub fn drain_events(harness: &mut FakeHarness) -> Vec<HarnessEvent> {
    let mut events = Vec::new();
    while let Ok(event) = harness.events().try_recv() {
        events.push(event);
    }
    events
}

/// A convenience constructor for tests that only need a session root.
pub fn roots_for(path: impl Into<PathBuf>) -> SessionRoots {
    SessionRoots::new([path.into()])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Transcript;

    async fn started_harness(turns: Vec<FakeTurn>) -> FakeHarness {
        let mut harness = FakeHarness::new(turns);
        let result = harness
            .start(SpawnSpec {
                program: "fake".to_string(),
                cwd: PathBuf::from("."),
                ..SpawnSpec::default()
            })
            .await;
        assert!(result.is_ok());
        let session = harness.open_session(None).await;
        assert!(session.is_ok());
        drain_events(&mut harness);
        harness
    }

    #[tokio::test]
    async fn question_pauses_until_matching_answer_and_resumes_the_turn()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut harness = started_harness(vec![FakeTurn::new(vec![
            FakeStep::Question {
                question_id: "question-1".into(),
                text: "Which target?".into(),
            },
            FakeStep::Message("Continuing".into()),
        ])])
        .await;
        harness.prompt(PromptInput::text("start")).await?;
        let before = drain_events(&mut harness);
        assert!(before.iter().any(|event| matches!(event, HarnessEvent::QuestionRequested { question_id, .. } if question_id == "question-1")));
        assert!(
            !before
                .iter()
                .any(|event| matches!(event, HarnessEvent::TurnFinished { .. }))
        );
        assert!(harness.answer_question("wrong", "debug").await.is_err());
        harness.answer_question("question-1", "debug").await?;
        let after = drain_events(&mut harness);
        assert!(after.iter().any(|event| matches!(event, HarnessEvent::QuestionAnswered { question_id, answer } if question_id == "question-1" && answer == "debug")));
        assert!(after.iter().any(|event| matches!(
            event,
            HarnessEvent::TurnFinished {
                reason: TurnFinishReason::Completed
            }
        )));
        Ok(())
    }

    #[tokio::test]
    async fn a_scripted_turn_emits_message_and_finishes() {
        let mut harness = started_harness(vec![FakeTurn::new(vec![
            FakeStep::Message("hello".to_string()),
            FakeStep::ToolCall {
                tool_call_id: "tool-1".to_string(),
                title: "Read file".to_string(),
                kind: "read".to_string(),
                status: ToolStatus::Completed,
                output: None,
                diffs: Vec::new(),
            },
        ])])
        .await;

        let result = harness.prompt(PromptInput::text("hi")).await;
        assert!(result.is_ok());
        let events = drain_events(&mut harness);
        assert!(
            events
                .iter()
                .any(|event| matches!(event, HarnessEvent::TurnFinished { .. }))
        );

        let mut transcript = Transcript::default();
        for event in &events {
            transcript.apply(event);
        }
        assert_eq!(transcript.len(), 2);
        assert_eq!(
            transcript.entry(0).map(|entry| entry.summary()),
            Some("agent: hello".to_string())
        );
    }

    #[tokio::test]
    async fn an_approval_pauses_and_allow_resumes() {
        let mut harness = started_harness(vec![FakeTurn::new(vec![
            FakeStep::approval("approval-1", "Run tests"),
            FakeStep::Message("done".to_string()),
        ])])
        .await;

        assert!(harness.prompt(PromptInput::text("go")).await.is_ok());
        let events = drain_events(&mut harness);
        assert!(
            events
                .iter()
                .any(|event| matches!(event, HarnessEvent::ApprovalRequested { .. }))
        );

        assert!(
            harness
                .respond(ApprovalId::new("approval-1"), Decision::Allow)
                .await
                .is_ok()
        );
        let events = drain_events(&mut harness);
        assert!(events.iter().any(|event| matches!(
            event,
            HarnessEvent::TurnFinished {
                reason: TurnFinishReason::Completed
            }
        )));
    }

    #[tokio::test]
    async fn multiple_approvals_preserve_remaining_steps() -> Result<(), crate::HarnessError> {
        let mut harness = started_harness(vec![FakeTurn::new(vec![
            FakeStep::approval("first", "First approval"),
            FakeStep::Message("before".to_string()),
            FakeStep::approval("second", "Second approval"),
            FakeStep::Message("after".to_string()),
        ])])
        .await;
        harness.prompt(PromptInput::default()).await?;
        harness
            .respond(ApprovalId::new("first"), Decision::Allow)
            .await?;
        harness
            .respond(ApprovalId::new("second"), Decision::Allow)
            .await?;
        let mut transcript = Transcript::default();
        for event in drain_events(&mut harness) {
            transcript.apply(&event);
        }
        let summaries: Vec<_> = transcript
            .entries()
            .iter()
            .map(crate::Entry::summary)
            .collect();
        assert!(summaries.contains(&"agent: before".to_string()));
        assert!(summaries.contains(&"agent: after".to_string()));
        Ok(())
    }

    #[tokio::test]
    async fn deny_ends_the_turn_as_cancelled() {
        let mut harness = started_harness(vec![FakeTurn::new(vec![
            FakeStep::approval("approval-1", "Run tests"),
            FakeStep::Message("done".to_string()),
        ])])
        .await;

        assert!(harness.prompt(PromptInput::text("go")).await.is_ok());
        drain_events(&mut harness);
        assert!(
            harness
                .respond(ApprovalId::new("approval-1"), Decision::Deny)
                .await
                .is_ok()
        );
        let events = drain_events(&mut harness);
        assert!(events.iter().any(|event| matches!(
            event,
            HarnessEvent::TurnFinished {
                reason: TurnFinishReason::Cancelled
            }
        )));
    }

    #[tokio::test]
    async fn cancel_ends_an_active_turn() {
        // A turn with no approval finishes in `prompt`, so start one that waits.
        let mut waiting = started_harness(vec![FakeTurn::new(vec![FakeStep::approval(
            "approval-1",
            "Run tests",
        )])])
        .await;
        assert!(waiting.prompt(PromptInput::text("go")).await.is_ok());
        drain_events(&mut waiting);
        assert!(waiting.cancel().await.is_ok());
        let events = drain_events(&mut waiting);
        assert!(events.iter().any(|event| matches!(
            event,
            HarnessEvent::TurnFinished {
                reason: TurnFinishReason::Cancelled
            }
        )));
    }

    #[tokio::test]
    async fn shutdown_reports_exit() {
        let mut harness = started_harness(Vec::new()).await;
        assert!(harness.shutdown().await.is_ok());
        let events = drain_events(&mut harness);
        assert!(
            events
                .iter()
                .any(|event| matches!(event, HarnessEvent::Exited { code: Some(0), .. }))
        );
    }

    #[test]
    fn failure_messages_classify_into_actionable_kinds() {
        use crate::FailureKind;

        assert_eq!(
            FailureKind::classify(&HarnessError::MissingProgram {
                program: "codex".to_string(),
            }),
            Some(FailureKind::MissingBinary)
        );
        assert_eq!(
            FailureKind::classify(&HarnessError::Protocol(
                "401 Unauthorized: run codex login".to_string()
            )),
            Some(FailureKind::AuthRequired)
        );
        assert_eq!(
            FailureKind::classify(&HarnessError::Protocol(
                "unknown method thread/start".to_string()
            )),
            Some(FailureKind::UnsupportedVersion)
        );
        assert_eq!(
            FailureKind::classify(&HarnessError::Protocol(
                "turn/start: malformed response".to_string()
            )),
            Some(FailureKind::Protocol)
        );
        assert_eq!(FailureKind::classify(&HarnessError::NotStarted), None);
    }

    #[test]
    fn a_failure_row_carries_its_kind_and_hint() {
        use crate::{Entry, FailureKind};

        let mut transcript = Transcript::default();
        transcript.push_failure(
            FailureKind::MissingBinary,
            "the `codex` program was not found",
            Some(FailureKind::MissingBinary.hint().to_string()),
        );
        assert_eq!(
            transcript.entry(0).map(Entry::summary),
            Some("failure: agent not installed: the `codex` program was not found".to_string())
        );
    }
}
