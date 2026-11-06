//! Adapt the shared ACP client to Shepherd's harness contract.

use std::collections::{HashMap, VecDeque};
use std::future::{Future, pending};
use std::path::PathBuf;
use std::pin::Pin;
use std::time::Duration;

use acp_client::schema::{
    self, ContentBlock, ContentChunk, PermissionOptionKind, SessionUpdate, StopReason,
    ToolCallContent, ToolCallUpdateFields, ToolKind,
};
use acp_client::{
    AcpConnection, AcpError, AcpEvent, ConnectOptions, PermissionDecision, PermissionReply,
    SessionRoots, ToolPermissions,
};
use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::{
    AdapterTier, AgentDiagnostics, AgentHarness, ApprovalId, ApprovalRequest, ApprovalSource,
    Capabilities, Decision, DeltaKind, Entry, FailureKind, FileDiff, HarnessError, HarnessEvent,
    HarnessResult, ItemId, PlanRow, PromptInput, SessionId, SessionState, SpawnSpec, ToolCallId,
    ToolStatus, Transcript, TurnFinishReason, TurnId, command_line, limit_command_output,
};

const SESSION_TIMEOUT: Duration = Duration::from_secs(15);

pub struct AcpHarness {
    connection: Option<AcpConnection>,
    cwd: PathBuf,
    diagnostics: AgentDiagnostics,
    runtime: Option<Runtime>,
    replay: Vec<Entry>,
    sender: mpsc::UnboundedSender<HarnessEvent>,
    events: mpsc::UnboundedReceiver<HarnessEvent>,
}

impl Default for AcpHarness {
    fn default() -> Self {
        let (sender, events) = mpsc::unbounded_channel();
        Self {
            connection: None,
            cwd: PathBuf::new(),
            diagnostics: AgentDiagnostics::default(),
            runtime: None,
            replay: Vec::new(),
            sender,
            events,
        }
    }
}

impl AcpHarness {
    fn attach(&mut self, connection: AcpConnection, cwd: PathBuf, command: String) -> Capabilities {
        let info = &connection.info;
        self.diagnostics.command = command;
        self.diagnostics.protocol = format!("ACP v1 {} {}", info.name, info.version)
            .trim_end()
            .to_owned();
        let capabilities = Capabilities {
            streaming: true,
            thoughts: true,
            tool_calls: true,
            approvals: true,
            resume: info.load_session,
            usage: true,
            patches_in_approvals: true,
            ..Capabilities::default()
        };
        self.cwd = cwd;
        self.connection = Some(connection);
        capabilities
    }

    async fn control(&self, command: Control) -> HarnessResult<()> {
        let runtime = self.runtime.as_ref().ok_or(HarnessError::SessionMissing)?;
        let (answer, response) = oneshot::channel();
        runtime
            .commands
            .send((command, answer))
            .await
            .map_err(|_| HarnessError::ChannelClosed)?;
        response.await.map_err(|_| HarnessError::ChannelClosed)?
    }
}

#[async_trait]
impl AgentHarness for AcpHarness {
    fn events(&mut self) -> &mut mpsc::UnboundedReceiver<HarnessEvent> {
        &mut self.events
    }

    fn diagnostics(&self) -> AgentDiagnostics {
        let mut diagnostics = self.diagnostics.clone();
        if let Some(connection) = &self.connection {
            diagnostics.stderr_tail = connection.recent_stderr().join("\n");
        }
        diagnostics
    }

    fn tier(&self) -> AdapterTier {
        AdapterTier::Acp
    }

    fn take_replayed_transcript(&mut self) -> Vec<Entry> {
        std::mem::take(&mut self.replay)
    }

    async fn start(&mut self, spec: SpawnSpec) -> HarnessResult<Capabilities> {
        if self.connection.is_some() {
            return Err(protocol("ACP harness is already started"));
        }
        self.diagnostics.command = command_line(&spec);
        let program = PathBuf::from(&spec.program);
        if program.is_absolute()
            && let Err(error) = tokio::fs::metadata(&program).await
            && error.kind() == std::io::ErrorKind::NotFound
        {
            return Err(HarnessError::MissingProgram {
                program: spec.program,
            });
        }
        let cwd = tokio::fs::canonicalize(&spec.cwd).await.map_err(|error| {
            protocol(format!(
                "could not resolve ACP worktree {}: {error}",
                spec.cwd.display()
            ))
        })?;
        let launch = acp_client::SpawnSpec {
            program,
            args: spec.args,
            cwd: cwd.clone(),
            env: spec.environment,
        };
        let options = ConnectOptions {
            roots: SessionRoots::new([cwd.clone()]),
            tool_permissions: ToolPermissions::default(),
        };
        let connection =
            session_request("initialize", acp_client::connect_spawn(launch, options)).await?;
        Ok(self.attach(connection, cwd, self.diagnostics.command.clone()))
    }

    async fn open_session(&mut self, resume: Option<SessionId>) -> HarnessResult<SessionId> {
        if self.runtime.is_some() {
            return Err(protocol("ACP session is already open"));
        }
        let connection = self.connection.as_mut().ok_or(HarnessError::NotStarted)?;
        let loading = resume.is_some();
        let (session, modes, config) = if let Some(resume) = resume {
            if !connection.info.load_session {
                return Err(protocol("ACP agent does not support session/load"));
            }
            let session = schema::SessionId::new(resume.as_str().to_owned());
            let (modes, config) = session_request(
                "session/load",
                connection.requester.load_session(
                    session.clone(),
                    self.cwd.clone(),
                    Vec::new(),
                    Vec::new(),
                ),
            )
            .await?;
            (session, modes, config)
        } else {
            session_request(
                "session/new",
                connection
                    .requester
                    .new_session(self.cwd.clone(), Vec::new(), Vec::new()),
            )
            .await?
        };
        self.diagnostics.model = configured_model(&config);
        let mut events = connection.take_events();
        if loading {
            self.replay = replay_history(&session, &mut events)?;
        }
        if let Some(modes) = modes {
            publish(
                &self.sender,
                HarnessEvent::ModeChanged {
                    mode: modes.current_mode_id.0.to_string(),
                },
            )?;
        }
        let id = SessionId::new(session.0.to_string());
        self.runtime = Some(Runtime::start(
            connection.requester.clone(),
            session,
            events,
            self.sender.clone(),
        ));
        publish(
            &self.sender,
            HarnessEvent::SessionState {
                state: SessionState::Ready,
            },
        )?;
        Ok(id)
    }

    async fn prompt(&mut self, input: PromptInput) -> HarnessResult<()> {
        self.control(Control::Prompt(input.text)).await
    }

    async fn cancel(&mut self) -> HarnessResult<()> {
        self.control(Control::Cancel).await
    }

    async fn respond(&mut self, approval_id: ApprovalId, decision: Decision) -> HarnessResult<()> {
        self.control(Control::Respond(approval_id, decision)).await
    }

    async fn shutdown(&mut self) -> HarnessResult<()> {
        self.runtime = None;
        if let Some(connection) = self.connection.take() {
            connection.close();
            publish(
                &self.sender,
                HarnessEvent::Exited {
                    code: Some(0),
                    stderr: String::new(),
                },
            )?;
        }
        Ok(())
    }
}

async fn session_request<T>(
    operation: &str,
    request: impl Future<Output = Result<T, AcpError>>,
) -> HarnessResult<T> {
    timeout(SESSION_TIMEOUT, request)
        .await
        .map_err(|_| protocol(format!("ACP {operation} timed out")))?
        .map_err(acp_error)
}

fn configured_model(options: &[schema::SessionConfigOption]) -> Option<String> {
    options.iter().find_map(|option| {
        if option.category == Some(schema::SessionConfigOptionCategory::Model)
            && let schema::SessionConfigKind::Select(select) = &option.kind
        {
            Some(select.current_value.0.to_string())
        } else {
            None
        }
    })
}

fn acp_error(error: AcpError) -> HarnessError {
    match error {
        AcpError::AgentNotFound { program } => HarnessError::MissingProgram { program },
        other => protocol(other.to_string()),
    }
}

fn protocol(message: impl Into<String>) -> HarnessError {
    HarnessError::Protocol(message.into())
}

fn publish(sender: &mpsc::UnboundedSender<HarnessEvent>, event: HarnessEvent) -> HarnessResult<()> {
    sender.send(event).map_err(|_| HarnessError::ChannelClosed)
}

enum Control {
    Prompt(String),
    Cancel,
    Respond(ApprovalId, Decision),
}

type Command = (Control, oneshot::Sender<HarnessResult<()>>);
type Prompt = Pin<Box<dyn Future<Output = Result<StopReason, AcpError>> + Send>>;

struct Runtime {
    commands: mpsc::Sender<Command>,
    worker: JoinHandle<()>,
}

impl Runtime {
    fn start(
        requester: acp_client::Requester,
        session: schema::SessionId,
        events: mpsc::UnboundedReceiver<AcpEvent>,
        sender: mpsc::UnboundedSender<HarnessEvent>,
    ) -> Self {
        let (commands, incoming) = mpsc::channel(8);
        let worker = tokio::spawn(async move {
            if let Err(error) = drive_session(requester, session, events, incoming, sender).await {
                tracing::debug!(%error, "ACP harness event loop ended");
            }
        });
        Self { commands, worker }
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.worker.abort();
    }
}

async fn drive_session(
    requester: acp_client::Requester,
    session: schema::SessionId,
    mut events: mpsc::UnboundedReceiver<AcpEvent>,
    mut commands: mpsc::Receiver<Command>,
    sender: mpsc::UnboundedSender<HarnessEvent>,
) -> HarnessResult<()> {
    let mut prompt: Option<Prompt> = None;
    let mut updates = Updates::default();
    let mut approvals = VecDeque::<PendingApproval>::new();
    let mut session_grants = Vec::new();
    let mut cancelling = false;
    loop {
        tokio::select! {
            biased;
            // ACP sends its final updates before replying to session/prompt.
            event = events.recv() => {
                match event {
                    Some(AcpEvent::SessionUpdate(notification)) if notification.session_id == session => {
                        if let Some(event) = updates.apply(&notification.update) {
                            updates.publish(&sender, event)?;
                        }
                    }
                    Some(AcpEvent::PermissionRequested { session_id, tool_call, options, decision })
                        if session_id == session && prompt.is_some() && !cancelling => {
                        let approval = PendingApproval::new(tool_call, options, decision);
                        let idle = approvals.is_empty();
                        approvals.push_back(approval);
                        if idle {
                            show_next_approval(&sender, &mut approvals, &session_grants)?;
                        }
                    }
                    Some(AcpEvent::TerminalOutput { terminal_id, chunk, .. }) if prompt.is_some() => {
                        updates.terminal_output(&sender, terminal_id, chunk)?;
                    }
                    Some(AcpEvent::Exited { code, stderr }) => {
                        withdraw_approvals(&sender, &mut approvals)?;
                        return publish(&sender, HarnessEvent::Exited { code, stderr });
                    }
                    None => {
                        withdraw_approvals(&sender, &mut approvals)?;
                        return publish(&sender, HarnessEvent::Exited { code: None, stderr: "ACP connection closed".into() });
                    }
                    _ => {}
                }
            }
            outcome = async {
                match &mut prompt {
                    Some(prompt) => prompt.await,
                    None => pending().await,
                }
            } => {
                prompt = None;
                cancelling = false;
                withdraw_approvals(&sender, &mut approvals)?;
                let reason = finish_turn(&sender, outcome)?;
                publish(&sender, HarnessEvent::TurnFinished { reason })?;
            }
            command = commands.recv() => {
                let Some((command, answer)) = command else { return Ok(()); };
                let result = match command {
                    Control::Prompt(_) if prompt.is_some() => Err(protocol("ACP turn is already running")),
                    Control::Prompt(text) => {
                        updates = Updates::default();
                        publish(&sender, HarnessEvent::TurnStarted { turn_id: TurnId::new(uuid::Uuid::new_v4().to_string()) })?;
                        prompt = Some(Box::pin({
                            let requester = requester.clone();
                            let session = session.clone();
                            async move { requester.prompt(&session, text).await }
                        }));
                        Ok(())
                    }
                    Control::Cancel if prompt.is_some() && !cancelling => {
                        let result = requester.cancel(&session).map_err(acp_error);
                        if result.is_ok() {
                            cancelling = true;
                            withdraw_approvals(&sender, &mut approvals)?;
                        }
                        result
                    }
                    Control::Cancel => Ok(()),
                    Control::Respond(id, decision) => {
                        respond(&sender, &mut approvals, &mut session_grants, id, decision)
                    }
                };
                if answer.send(result).is_err() {
                    tracing::debug!("ACP command caller released its reply");
                }
            }
        }
    }
}

fn finish_turn(
    sender: &mpsc::UnboundedSender<HarnessEvent>,
    outcome: Result<StopReason, AcpError>,
) -> HarnessResult<TurnFinishReason> {
    let failure = match outcome {
        Ok(StopReason::EndTurn) => return Ok(TurnFinishReason::Completed),
        Ok(StopReason::Cancelled) => return Ok(TurnFinishReason::Cancelled),
        Ok(StopReason::MaxTokens) => "The ACP agent reached its token limit".to_string(),
        Ok(StopReason::MaxTurnRequests) => {
            "The ACP agent reached its turn request limit".to_string()
        }
        Ok(StopReason::Refusal) => "The ACP agent refused to continue".to_string(),
        Ok(reason) => format!("The ACP agent stopped the turn ({reason:?})"),
        Err(error) => error.to_string(),
    };
    let kind = FailureKind::classify(&protocol(&failure)).unwrap_or(FailureKind::Protocol);
    publish(
        sender,
        HarnessEvent::TurnError {
            kind,
            text: failure,
        },
    )?;
    Ok(TurnFinishReason::Failed)
}

struct PendingApproval {
    request: ApprovalRequest,
    allow: Option<schema::PermissionOptionId>,
    deny: Option<schema::PermissionOptionId>,
    grant: Option<SessionGrant>,
    reply: PermissionReply,
}

#[derive(PartialEq)]
struct SessionGrant {
    fields: ToolCallUpdateFields,
    meta: Option<schema::Meta>,
}

impl PendingApproval {
    fn new(
        call: schema::ToolCallUpdate,
        options: Vec<schema::PermissionOption>,
        reply: PermissionReply,
    ) -> Self {
        let once = |kind| {
            options
                .iter()
                .find(|option| option.kind == kind)
                .map(|option| option.option_id.clone())
        };
        let fields = call.fields;
        let allow = once(PermissionOptionKind::AllowOnce);
        let mut choices = Vec::new();
        let mut grant = None;
        if allow.is_some() {
            choices.push(Decision::Allow);
            // The title is agent-authored display text. Reuse an approval only
            // when the request identifies both an operation and its input.
            if fields.kind.is_some_and(|kind| kind != ToolKind::Other)
                && fields.raw_input.as_ref().is_some_and(has_approval_input)
            {
                choices.push(Decision::AllowSession);
                grant = Some(SessionGrant {
                    fields: fields.clone().status(None).raw_output(None),
                    meta: call.meta,
                });
            }
        }
        choices.push(Decision::Deny);
        let patch = fields
            .content
            .as_deref()
            .unwrap_or_default()
            .iter()
            .find_map(|content| {
                if let ToolCallContent::Diff(diff) = content {
                    Some(FileDiff {
                        path: diff.path.display().to_string(),
                        old_text: diff.old_text.clone(),
                        new_text: diff.new_text.clone(),
                    })
                } else {
                    None
                }
            });
        let request = ApprovalRequest {
            approval_id: ApprovalId::new(uuid::Uuid::new_v4().to_string()),
            tool_call_id: ToolCallId::new(call.tool_call_id.0.to_string()),
            title: fields.title.unwrap_or_else(|| "Approve tool call".into()),
            arguments: fields.raw_input.unwrap_or_default(),
            paths: fields
                .locations
                .unwrap_or_default()
                .into_iter()
                .map(|location| location.path)
                .collect(),
            patch,
            choices: Some(choices),
        };
        Self {
            request,
            allow,
            deny: once(PermissionOptionKind::RejectOnce),
            grant,
            reply,
        }
    }
}

fn has_approval_input(input: &serde_json::Value) -> bool {
    match input {
        serde_json::Value::String(text) => !text.trim().is_empty(),
        serde_json::Value::Array(values) => values.iter().any(has_approval_input),
        serde_json::Value::Object(values) => values.values().any(has_approval_input),
        serde_json::Value::Bool(_) | serde_json::Value::Number(_) => true,
        serde_json::Value::Null => false,
    }
}

fn show_next_approval(
    sender: &mpsc::UnboundedSender<HarnessEvent>,
    approvals: &mut VecDeque<PendingApproval>,
    session_grants: &[SessionGrant],
) -> HarnessResult<()> {
    while let Some(approval) = approvals.front() {
        let Some(option) = approval.allow.clone().filter(|_| {
            approval
                .grant
                .as_ref()
                .is_some_and(|grant| session_grants.contains(grant))
        }) else {
            return publish(
                sender,
                HarnessEvent::ApprovalRequested {
                    request: approval.request.clone(),
                },
            );
        };
        let approval = approvals
            .pop_front()
            .ok_or(HarnessError::NoPendingApproval)?;
        if approval
            .reply
            .answer(PermissionDecision::Selected(option))
            .is_ok()
        {
            publish(
                sender,
                HarnessEvent::ApprovalAutomaticallyResolved {
                    request: approval.request,
                    decision: Decision::Allow,
                    source: ApprovalSource::SessionGrant,
                },
            )?;
        } else {
            tracing::debug!("ACP permission ended before its session grant could be applied");
        }
    }
    Ok(())
}

fn respond(
    sender: &mpsc::UnboundedSender<HarnessEvent>,
    approvals: &mut VecDeque<PendingApproval>,
    session_grants: &mut Vec<SessionGrant>,
    id: ApprovalId,
    decision: Decision,
) -> HarnessResult<()> {
    let pending = approvals
        .front()
        .filter(|approval| approval.request.approval_id == id)
        .ok_or(HarnessError::NoPendingApproval)?;
    let option = match decision {
        Decision::AllowSession if pending.grant.is_none() => {
            return Err(HarnessError::ApprovalOptionUnavailable);
        }
        Decision::Allow | Decision::AllowSession => Some(
            pending
                .allow
                .clone()
                .ok_or(HarnessError::ApprovalOptionUnavailable)?,
        ),
        Decision::Deny => pending.deny.clone(),
    };
    let approval = approvals
        .pop_front()
        .ok_or(HarnessError::NoPendingApproval)?;
    let reply = option.map_or(PermissionDecision::Cancelled, PermissionDecision::Selected);
    if approval.reply.answer(reply).is_err() {
        publish(sender, HarnessEvent::ApprovalWithdrawn { approval_id: id })?;
    } else {
        if decision == Decision::AllowSession
            && let Some(grant) = approval.grant
        {
            session_grants.push(grant);
        }
        publish(
            sender,
            HarnessEvent::ApprovalResolved {
                approval_id: id,
                decision,
                source: ApprovalSource::User,
            },
        )?;
    }
    show_next_approval(sender, approvals, session_grants)
}

fn withdraw_approvals(
    sender: &mpsc::UnboundedSender<HarnessEvent>,
    approvals: &mut VecDeque<PendingApproval>,
) -> HarnessResult<()> {
    if let Some(visible) = approvals.pop_front() {
        publish(
            sender,
            HarnessEvent::ApprovalWithdrawn {
                approval_id: visible.request.approval_id,
            },
        )?;
    }
    approvals.clear();
    Ok(())
}

#[derive(Default)]
struct Updates {
    tools: acp_client::Transcript,
    implicit: Option<(DeltaKind, ItemId)>,
    terminal_tools: HashMap<String, ToolCallId>,
    early_output: HashMap<String, String>,
}

impl Updates {
    fn apply(&mut self, update: &SessionUpdate) -> Option<HarnessEvent> {
        match update {
            SessionUpdate::AgentMessageChunk(chunk) => {
                return self.chunk(DeltaKind::Message, chunk);
            }
            SessionUpdate::AgentThoughtChunk(chunk) => {
                return self.chunk(DeltaKind::Thought, chunk);
            }
            SessionUpdate::UserMessageChunk(_) => {
                self.implicit = None;
                return None;
            }
            _ => {}
        }
        let event = match update {
            SessionUpdate::ToolCall(_) | SessionUpdate::ToolCallUpdate(_) => {
                let delta = self.tools.apply_update(update);
                self.tools.entry(delta.changed?).and_then(tool_event)
            }
            SessionUpdate::Plan(plan) => Some(HarnessEvent::Plan {
                rows: plan
                    .entries
                    .iter()
                    .map(|entry| PlanRow {
                        text: entry.content.clone(),
                        status: format!("{:?}", entry.status),
                    })
                    .collect(),
            }),
            SessionUpdate::UsageUpdate(usage) => Some(HarnessEvent::Usage {
                used: usage.used,
                size: usage.size,
                cost: usage.cost.as_ref().map(|cost| cost.amount),
            }),
            SessionUpdate::CurrentModeUpdate(mode) => Some(HarnessEvent::ModeChanged {
                mode: mode.current_mode_id.0.to_string(),
            }),
            _ => None,
        };
        if matches!(update, SessionUpdate::ToolCall(_)) {
            self.implicit = None;
        }
        event
    }

    fn chunk(&mut self, kind: DeltaKind, chunk: &ContentChunk) -> Option<HarnessEvent> {
        let ContentBlock::Text(content) = &chunk.content else {
            return None;
        };
        let item_id = if let Some(id) = &chunk.message_id {
            self.implicit = None;
            ItemId::new(id.0.to_string())
        } else if let Some((previous, item)) = &self.implicit
            && *previous == kind
        {
            item.clone()
        } else {
            let item = ItemId::new(uuid::Uuid::new_v4().to_string());
            self.implicit = Some((kind, item.clone()));
            item
        };
        Some(HarnessEvent::Delta {
            item_id,
            kind,
            text: content.text.clone(),
        })
    }

    fn publish(
        &mut self,
        sender: &mpsc::UnboundedSender<HarnessEvent>,
        event: HarnessEvent,
    ) -> HarnessResult<()> {
        let terminal = match &event {
            HarnessEvent::ToolCall {
                tool_call_id,
                terminal_id: Some(terminal_id),
                kind,
                ..
            } if kind == "command" => {
                self.terminal_tools
                    .insert(terminal_id.clone(), tool_call_id.clone());
                self.early_output
                    .remove(terminal_id)
                    .map(|output| (tool_call_id.clone(), output))
            }
            _ => None,
        };
        publish(sender, event)?;
        if let Some((tool_call_id, text)) = terminal {
            publish(sender, HarnessEvent::CommandOutput { tool_call_id, text })?;
        }
        Ok(())
    }

    fn terminal_output(
        &mut self,
        sender: &mpsc::UnboundedSender<HarnessEvent>,
        terminal_id: String,
        chunk: String,
    ) -> HarnessResult<()> {
        if let Some(tool_call_id) = self.terminal_tools.get(&terminal_id) {
            publish(
                sender,
                HarnessEvent::CommandOutput {
                    tool_call_id: tool_call_id.clone(),
                    text: chunk,
                },
            )?;
        } else {
            let output = self.early_output.entry(terminal_id).or_default();
            output.push_str(&chunk);
            limit_command_output(output);
        }
        Ok(())
    }
}

fn tool_event(entry: &acp_client::Entry) -> Option<HarnessEvent> {
    let acp_client::EntryKind::ToolCall {
        tool_call_id,
        title,
        kind,
        status,
        output,
        diffs,
        terminal_id,
    } = &entry.kind
    else {
        return None;
    };
    Some(HarnessEvent::ToolCall {
        tool_call_id: ToolCallId::new(tool_call_id.clone()),
        title: title.clone(),
        kind: if kind == "Execute" {
            "command".into()
        } else {
            kind.to_lowercase()
        },
        status: match status {
            acp_client::ToolStatus::Pending => ToolStatus::Pending,
            acp_client::ToolStatus::Running => ToolStatus::Running,
            acp_client::ToolStatus::Completed => ToolStatus::Completed,
            acp_client::ToolStatus::Failed => ToolStatus::Failed,
        },
        output: output.clone().filter(|output| !output.is_empty()),
        diffs: diffs
            .iter()
            .map(|diff| FileDiff {
                path: diff.path.clone(),
                old_text: diff.old_text.clone(),
                new_text: diff.new_text.clone(),
            })
            .collect(),
        terminal_id: terminal_id.clone(),
    })
}

fn replay_history(
    session: &schema::SessionId,
    events: &mut mpsc::UnboundedReceiver<AcpEvent>,
) -> HarnessResult<Vec<Entry>> {
    let mut transcript = Transcript::default();
    let mut updates = Updates::default();
    let mut user = None::<(Option<schema::MessageId>, String)>;
    while let Ok(event) = events.try_recv() {
        match event {
            AcpEvent::SessionUpdate(notification) if &notification.session_id == session => {
                if let SessionUpdate::UserMessageChunk(chunk) = &notification.update
                    && let ContentBlock::Text(content) = &chunk.content
                {
                    if user.as_ref().is_some_and(|(id, _)| id != &chunk.message_id)
                        && let Some((_, text)) = user.take()
                    {
                        transcript.push_user_message(text);
                    }
                    user.get_or_insert_with(|| (chunk.message_id.clone(), String::new()))
                        .1
                        .push_str(&content.text);
                }
                if let Some(event) = updates.apply(&notification.update) {
                    if let Some((_, text)) = user.take() {
                        transcript.push_user_message(text);
                    }
                    transcript.apply(&event);
                }
            }
            AcpEvent::Exited { code, stderr } => {
                return Err(protocol(format!(
                    "ACP agent exited while loading history ({code:?}): {stderr}"
                )));
            }
            _ => {}
        }
    }
    if let Some((_, text)) = user {
        transcript.push_user_message(text);
    }
    transcript.apply(&HarnessEvent::TurnFinished {
        reason: TurnFinishReason::Completed,
    });
    Ok(transcript.entries)
}

#[cfg(test)]
mod tests {
    use acp_client::fake_agent::{FakeScript, Shared, connect_fake_agent};
    use acp_client::schema::{
        Diff, PermissionOption, TextContent, ToolCall, ToolCallStatus, ToolCallUpdate,
        ToolCallUpdateFields, ToolKind,
    };

    use super::*;

    async fn fake(
        script: FakeScript,
    ) -> Result<(AcpHarness, Capabilities, Shared), Box<dyn std::error::Error>> {
        let cwd = std::env::temp_dir();
        let (connection, observed) = connect_fake_agent(
            script,
            ConnectOptions {
                roots: SessionRoots::new([cwd.clone()]),
                tool_permissions: ToolPermissions::default(),
            },
        )
        .await?;
        let mut harness = AcpHarness::default();
        let capabilities = harness.attach(connection, cwd, "fake-acp".into());
        Ok((harness, capabilities, observed))
    }

    async fn next(harness: &mut AcpHarness) -> Result<HarnessEvent, Box<dyn std::error::Error>> {
        timeout(Duration::from_secs(5), harness.events().recv())
            .await?
            .ok_or("ACP harness closed".into())
    }

    fn chunk(text: &str) -> ContentChunk {
        ContentChunk::new(ContentBlock::Text(TextContent::new(text)))
    }

    fn tool_request() -> ToolCallUpdate {
        ToolCallUpdate::new(
            "tool-1",
            ToolCallUpdateFields::new()
                .title("Edit a file")
                .content(vec![ToolCallContent::Diff(Diff::new(
                    "file.txt",
                    "updated\n",
                ))]),
        )
    }

    #[tokio::test]
    async fn resumes_history_and_keeps_one_tool_row_through_approval()
    -> Result<(), Box<dyn std::error::Error>> {
        let script = FakeScript {
            load_updates: vec![
                SessionUpdate::UserMessageChunk(chunk("earlier ")),
                SessionUpdate::UserMessageChunk(chunk("question")),
                SessionUpdate::AgentMessageChunk(chunk("earlier answer")),
            ],
            chunks: vec!["new ".into(), "answer".into()],
            tool_call: Some(ToolCall::new("tool-1", "Edit a file").kind(ToolKind::Edit)),
            tool_update: Some(ToolCallUpdate::new(
                "tool-1",
                ToolCallUpdateFields::new().status(ToolCallStatus::Completed),
            )),
            permission: Some(tool_request()),
            permission_options: vec![
                PermissionOption::new("always", "Always allow", PermissionOptionKind::AllowAlways),
                PermissionOption::new("once", "Allow once", PermissionOptionKind::AllowOnce),
            ],
            ..FakeScript::default()
        };
        let (mut harness, capabilities, observed) = fake(script).await?;
        assert!(capabilities.resume);
        let session = harness
            .open_session(Some(SessionId::new("old-session")))
            .await?;
        assert_eq!(session.as_str(), "old-session");
        let mut transcript = Transcript::default();
        transcript.restore(harness.take_replayed_transcript());
        assert_eq!(
            transcript
                .entries()
                .iter()
                .map(Entry::summary)
                .collect::<Vec<_>>(),
            ["you: earlier question", "agent: earlier answer"]
        );

        harness
            .prompt(PromptInput {
                text: "continue".into(),
            })
            .await?;
        let approval = loop {
            let event = next(&mut harness).await?;
            transcript.apply(&event);
            if let HarnessEvent::ApprovalRequested { request } = event {
                break request;
            }
        };
        assert_eq!(approval.tool_call_id.as_str(), "tool-1");
        assert_eq!(
            approval.patch.as_ref().map(|patch| patch.path.as_str()),
            Some("file.txt")
        );
        harness
            .respond(approval.approval_id, Decision::Allow)
            .await?;
        loop {
            let event = next(&mut harness).await?;
            transcript.apply(&event);
            if let HarnessEvent::TurnFinished { reason } = event {
                assert_eq!(reason, TurnFinishReason::Completed);
                break;
            }
        }
        assert!(
            transcript
                .entries()
                .iter()
                .any(|entry| entry.summary() == "agent: new answer")
        );
        assert_eq!(
            transcript
                .entries()
                .iter()
                .filter(|entry| matches!(entry.kind, crate::EntryKind::ToolCall { .. }))
                .count(),
            1
        );
        let observed = observed.lock().map_err(|_| "fake observations poisoned")?;
        assert_eq!(observed.load_attempts, ["old-session"]);
        assert!(
            observed
                .permission_responses
                .first()
                .is_some_and(|choice| choice.contains("once"))
        );
        Ok(())
    }

    #[tokio::test]
    async fn reuses_an_exact_permission_for_the_live_session()
    -> Result<(), Box<dyn std::error::Error>> {
        let script = FakeScript {
            permission: Some(ToolCallUpdate::new(
                "tool-1",
                ToolCallUpdateFields::new()
                    .kind(ToolKind::Execute)
                    .title("Run a command")
                    .raw_input(serde_json::json!({"command": "git status"})),
            )),
            permission_options: vec![
                PermissionOption::new("always", "Always allow", PermissionOptionKind::AllowAlways),
                PermissionOption::new("once", "Allow once", PermissionOptionKind::AllowOnce),
            ],
            ..FakeScript::default()
        };
        let (mut harness, _, observed) = fake(script).await?;
        harness.open_session(None).await?;
        harness
            .prompt(PromptInput {
                text: "first".into(),
            })
            .await?;
        let mut transcript = Transcript::default();
        let request = loop {
            let event = next(&mut harness).await?;
            transcript.apply(&event);
            if let HarnessEvent::ApprovalRequested { request } = event {
                break request;
            }
        };
        assert_eq!(
            request.choices.as_deref(),
            Some([Decision::Allow, Decision::AllowSession, Decision::Deny].as_slice()),
        );
        harness
            .respond(request.approval_id, Decision::AllowSession)
            .await?;
        loop {
            let event = next(&mut harness).await?;
            transcript.apply(&event);
            if matches!(event, HarnessEvent::TurnFinished { .. }) {
                break;
            }
        }

        harness
            .prompt(PromptInput {
                text: "second".into(),
            })
            .await?;
        loop {
            let event = next(&mut harness).await?;
            transcript.apply(&event);
            assert!(!matches!(event, HarnessEvent::ApprovalRequested { .. }));
            if matches!(event, HarnessEvent::TurnFinished { .. }) {
                break;
            }
        }
        assert!(transcript.entries().iter().any(|entry| matches!(
            &entry.kind,
            crate::EntryKind::Permission {
                decision: Some(Decision::Allow),
                source: Some(ApprovalSource::SessionGrant),
                ..
            }
        )));
        let observed = observed.lock().map_err(|_| "fake observations poisoned")?;
        assert_eq!(observed.permission_responses.len(), 2);
        assert!(
            observed
                .permission_responses
                .iter()
                .all(|choice| choice.contains("once"))
        );
        Ok(())
    }

    #[test]
    fn queued_session_grant_never_covers_a_changed_command()
    -> Result<(), Box<dyn std::error::Error>> {
        let pending = |command: &str| {
            let (reply, response) = futures_channel::oneshot::channel();
            let request = ToolCallUpdate::new(
                "tool-1",
                ToolCallUpdateFields::new()
                    .kind(ToolKind::Execute)
                    .title("Run a command")
                    .raw_input(serde_json::json!({"command": command})),
            );
            let options = vec![PermissionOption::new(
                "once",
                "Allow once",
                PermissionOptionKind::AllowOnce,
            )];
            (
                PendingApproval::new(request, options, PermissionReply::new(reply)),
                response,
            )
        };
        let (first, mut first_response) = pending("git status");
        let (identical, mut identical_response) = pending("git status");
        let (changed, mut changed_response) = pending("git clean -fd");
        let first_id = first.request.approval_id.clone();
        let changed_id = changed.request.approval_id.clone();
        let mut queue = VecDeque::from([first, identical, changed]);
        let mut grants = Vec::new();
        let (sender, mut events) = mpsc::unbounded_channel();
        respond(
            &sender,
            &mut queue,
            &mut grants,
            first_id,
            Decision::AllowSession,
        )?;
        assert!(matches!(
            first_response.try_recv()?,
            Some(PermissionDecision::Selected(_))
        ));
        assert!(matches!(
            identical_response.try_recv()?,
            Some(PermissionDecision::Selected(_))
        ));
        assert!(changed_response.try_recv()?.is_none());
        assert!(matches!(
            events.try_recv()?,
            HarnessEvent::ApprovalResolved {
                source: ApprovalSource::User,
                ..
            }
        ));
        assert!(matches!(
            events.try_recv()?,
            HarnessEvent::ApprovalAutomaticallyResolved {
                source: ApprovalSource::SessionGrant,
                ..
            }
        ));
        assert!(
            matches!(events.try_recv()?, HarnessEvent::ApprovalRequested { request } if request.approval_id == changed_id)
        );
        Ok(())
    }

    #[tokio::test]
    async fn cancel_withdraws_pending_approval_and_waits_for_stop_reason()
    -> Result<(), Box<dyn std::error::Error>> {
        let script = FakeScript {
            permission: Some(tool_request()),
            permission_options: vec![PermissionOption::new(
                "always",
                "Always allow",
                PermissionOptionKind::AllowAlways,
            )],
            ..FakeScript::default()
        };
        let (mut harness, _, observed) = fake(script).await?;
        let session = harness.open_session(None).await?;
        harness
            .prompt(PromptInput {
                text: "wait for permission".into(),
            })
            .await?;
        let approval = loop {
            if let HarnessEvent::ApprovalRequested { request } = next(&mut harness).await? {
                assert_eq!(
                    request.choices.as_deref(),
                    Some([Decision::Deny].as_slice())
                );
                break request.approval_id;
            }
        };
        assert!(matches!(
            harness.respond(approval.clone(), Decision::Allow).await,
            Err(HarnessError::ApprovalOptionUnavailable)
        ));
        harness.cancel().await?;
        assert!(
            matches!(next(&mut harness).await?, HarnessEvent::ApprovalWithdrawn { approval_id } if approval_id == approval)
        );
        assert!(matches!(
            next(&mut harness).await?,
            HarnessEvent::TurnFinished {
                reason: TurnFinishReason::Cancelled
            }
        ));
        let observed = observed.lock().map_err(|_| "fake observations poisoned")?;
        assert_eq!(observed.cancels, [session.as_str()]);
        assert_eq!(observed.permission_responses, ["Cancelled"]);
        Ok(())
    }
}
