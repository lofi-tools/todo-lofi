//! Adapt the shared ACP client to Shepherd's harness contract.

use std::collections::{HashMap, HashSet, VecDeque};
use std::future::{Future, pending};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

pub use acp_client::schema::TerminalExitStatus;
use acp_client::schema::{
    self, ContentBlock, ContentChunk, PermissionOptionKind, SessionUpdate, StopReason,
    ToolCallContent, ToolCallUpdateFields, ToolKind,
};
pub use acp_client::terminal::{TerminalBackend, TerminalLaunch, TerminalProcessEvent};
use acp_client::{
    AcpConnection, AcpError, AcpEvent, ConnectOptions, PermissionDecision, PermissionReply,
    SessionRoots, ToolPermissions,
};
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep_until, timeout};

use crate::{
    AdapterTier, AgentDiagnostics, AgentHarness, AgentMode, AgentModes, ApprovalId,
    ApprovalRequest, ApprovalSource, Capabilities, Decision, DeltaKind, Entry, FailureKind,
    FileDiff, HarnessError, HarnessEvent, HarnessResult, ItemId, PlanRow, ProjectApprovalPolicy,
    PromptInput, SessionApproval, SessionId, SessionState, SpawnSpec, ToolCallId, ToolStatus,
    Transcript, TurnFinishReason, TurnId, bounded_command_output, command_line,
    limit_command_output,
};

const SESSION_TIMEOUT: Duration = Duration::from_secs(15);

pub struct AcpHarness {
    connection: Option<AcpConnection>,
    cwd: PathBuf,
    diagnostics: AgentDiagnostics,
    runtime: Option<Runtime>,
    project_approvals: Option<Arc<dyn ProjectApprovalPolicy>>,
    terminals: Option<Arc<dyn TerminalBackend>>,
    process_tracker: Option<Arc<dyn crate::AgentProcessTracker>>,
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
            project_approvals: None,
            terminals: None,
            process_tracker: None,
            replay: Vec::new(),
            sender,
            events,
        }
    }
}

struct AcpProcessTracker(Arc<dyn crate::AgentProcessTracker>);

#[async_trait]
impl acp_client::connection::ProcessTracker for AcpProcessTracker {
    async fn started(&self, pid: u32) -> Result<String, String> {
        self.0.started(pid).await
    }

    async fn stopped(&self, record_id: &str) -> Result<(), String> {
        self.0.stopped(record_id).await
    }
}

impl AcpHarness {
    pub fn with_terminals(mut self, terminals: Arc<dyn TerminalBackend>) -> Self {
        self.terminals = Some(terminals);
        self
    }

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
            cancel: true,
            agent_terminals: self.terminals.is_some(),
            usage: true,
            patches_in_approvals: true,
            images: info.prompt_images,
            resource_links: true,
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
    fn set_process_tracker(&mut self, tracker: Arc<dyn crate::AgentProcessTracker>) {
        self.process_tracker = Some(tracker);
    }

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

    fn modes(&self) -> Option<AgentModes> {
        self.runtime
            .as_ref()?
            .modes
            .borrow()
            .as_ref()
            .map(|mode| mode.options.clone())
    }

    async fn set_mode(&mut self, mode: &str) -> HarnessResult<()> {
        self.control(Control::SetMode(mode.to_owned())).await
    }

    fn set_project_approval_policy(&mut self, policy: Arc<dyn ProjectApprovalPolicy>) {
        self.project_approvals = Some(policy);
    }

    fn session_approvals(&self) -> Vec<SessionApproval> {
        let Some(runtime) = &self.runtime else {
            return Vec::new();
        };
        if runtime.grants.has_changed().is_err() {
            return Vec::new();
        }
        runtime.grants.borrow().clone()
    }

    async fn reset_session_approval(&mut self, id: &str) -> HarnessResult<()> {
        if self.runtime.is_none() {
            return Ok(());
        }
        match self.control(Control::ResetSessionApproval(id.into())).await {
            Err(HarnessError::ChannelClosed) => Ok(()),
            result => result,
        }
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
        let connection = session_request("initialize", async {
            if let Some(tracker) = &self.process_tracker {
                return acp_client::connection::connect_spawn_tracked(
                    launch,
                    options,
                    self.terminals.clone(),
                    Arc::new(AcpProcessTracker(tracker.clone())),
                )
                .await;
            }
            match &self.terminals {
                Some(terminals) => {
                    acp_client::connect_spawn_with_terminals(launch, options, terminals.clone())
                        .await
                }
                None => acp_client::connect_spawn(launch, options).await,
            }
        })
        .await?;
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
        let mut startup_events = VecDeque::new();
        if loading {
            self.replay = replay_history(&session, &mut events, &mut startup_events)?;
        }
        let modes =
            ModeControl::from_config(&config).or_else(|| modes.and_then(ModeControl::from_legacy));
        let id = SessionId::new(session.0.to_string());
        self.runtime = Some(Runtime::start(
            connection.requester.clone(),
            session,
            (startup_events, events),
            self.sender.clone(),
            self.project_approvals.clone(),
            connection.close_terminals(),
            modes,
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
        let info = &self
            .connection
            .as_ref()
            .ok_or(HarnessError::NotStarted)?
            .info;
        let mut content = vec![ContentBlock::Text(schema::TextContent::new(input.text))];
        let mut readable_files = Vec::new();
        for attachment in input.attachments {
            if let Some(mime_type) = attachment
                .image_mime_type
                .as_ref()
                .filter(|_| info.prompt_images)
            {
                let bytes = tokio::fs::read(&attachment.path).await.map_err(|error| {
                    protocol(format!(
                        "could not read image {}: {error}",
                        attachment.path.display()
                    ))
                })?;
                content.push(ContentBlock::Image(schema::ImageContent::new(
                    STANDARD.encode(bytes),
                    mime_type,
                )));
            } else {
                let name = attachment
                    .path
                    .file_name()
                    .ok_or_else(|| protocol("an attachment is missing its file name"))?;
                content.push(ContentBlock::ResourceLink(schema::ResourceLink::new(
                    name.to_string_lossy(),
                    attachment.file_uri()?.to_string(),
                )));
                readable_files.push(attachment.path);
            }
        }
        self.control(Control::Prompt {
            content,
            readable_files,
        })
        .await
    }

    async fn cancel(&mut self) -> HarnessResult<()> {
        self.control(Control::Cancel).await
    }

    async fn respond(&mut self, approval_id: ApprovalId, decision: Decision) -> HarnessResult<()> {
        self.control(Control::Respond(approval_id, decision)).await
    }

    async fn shutdown(&mut self) -> HarnessResult<()> {
        self.runtime = None;
        if let Some(connection) = self.connection.as_mut() {
            connection.close_and_wait().await.map_err(acp_error)?;
            self.connection = None;
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

#[derive(Clone, PartialEq, Eq)]
struct ModeControl {
    config_id: Option<schema::SessionConfigId>,
    options: AgentModes,
}

impl ModeControl {
    fn from_legacy(modes: schema::SessionModeState) -> Option<Self> {
        let options: Vec<_> = modes
            .available_modes
            .into_iter()
            .map(|mode| AgentMode {
                id: mode.id.0.to_string(),
                name: mode.name,
                description: mode.description,
            })
            .collect();
        (!options.is_empty()).then(|| Self {
            config_id: None,
            options: AgentModes {
                current: Some(modes.current_mode_id.0.to_string()),
                options,
            },
        })
    }

    fn from_config(config: &[schema::SessionConfigOption]) -> Option<Self> {
        let option = config
            .iter()
            .find(|option| option.category == Some(schema::SessionConfigOptionCategory::Mode))?;
        let schema::SessionConfigKind::Select(select) = &option.kind else {
            return None;
        };
        let choices: Vec<_> = match &select.options {
            schema::SessionConfigSelectOptions::Ungrouped(options) => options.iter().collect(),
            schema::SessionConfigSelectOptions::Grouped(groups) => {
                groups.iter().flat_map(|group| &group.options).collect()
            }
            _ => return None,
        };
        let options: Vec<_> = choices
            .into_iter()
            .map(|choice| AgentMode {
                id: choice.value.0.to_string(),
                name: choice.name.clone(),
                description: choice.description.clone(),
            })
            .collect();
        (!options.is_empty()).then(|| Self {
            config_id: Some(option.id.clone()),
            options: AgentModes {
                current: Some(select.current_value.0.to_string()),
                options,
            },
        })
    }
}

fn update_modes(modes: &watch::Sender<Option<ModeControl>>, update: &SessionUpdate) -> bool {
    match update {
        SessionUpdate::CurrentModeUpdate(mode) => modes.send_if_modified(|current| {
            let Some(current) = current
                .as_mut()
                .filter(|control| control.config_id.is_none())
            else {
                return false;
            };
            let next = Some(mode.current_mode_id.0.to_string());
            if current.options.current == next {
                return false;
            }
            current.options.current = next;
            true
        }),
        SessionUpdate::ConfigOptionUpdate(config) => {
            let next = ModeControl::from_config(&config.config_options);
            modes.send_if_modified(move |current| {
                if *current == next
                    || (next.is_none()
                        && current
                            .as_ref()
                            .is_some_and(|control| control.config_id.is_none()))
                {
                    return false;
                }
                *current = next;
                true
            })
        }
        _ => false,
    }
}

async fn set_session_mode(
    requester: &acp_client::Requester,
    session: &schema::SessionId,
    modes: &watch::Sender<Option<ModeControl>>,
    mode: &str,
) -> HarnessResult<()> {
    let mut control = modes
        .borrow()
        .clone()
        .ok_or(HarnessError::ModeUnavailable)?;
    if !control
        .options
        .options
        .iter()
        .any(|option| option.id == mode)
    {
        return Err(HarnessError::ModeUnavailable);
    }
    if control.options.current.as_deref() == Some(mode) {
        return Ok(());
    }
    let mode = mode.to_owned();
    let next = if let Some(config_id) = &control.config_id {
        let config = session_request(
            "session/set_config_option",
            requester.set_config_option(
                session,
                config_id.clone(),
                schema::SessionConfigOptionValue::value_id(mode),
            ),
        )
        .await?;
        ModeControl::from_config(&config)
    } else {
        session_request(
            "session/set_mode",
            requester.set_mode(session, schema::SessionModeId::new(mode.clone())),
        )
        .await?;
        control.options.current = Some(mode);
        Some(control)
    };
    modes.send_replace(next);
    Ok(())
}

fn acp_error(error: AcpError) -> HarnessError {
    match error {
        AcpError::AgentNotFound { program } => HarnessError::MissingProgram { program },
        AcpError::AuthRequired(message) => HarnessError::Reported {
            kind: FailureKind::AuthRequired,
            message,
        },
        AcpError::UnsupportedVersion { .. } => HarnessError::Reported {
            kind: FailureKind::UnsupportedVersion,
            message: error.to_string(),
        },
        AcpError::TransportClosed(message) => HarnessError::TransportClosed(message),
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
    Prompt {
        content: Vec<ContentBlock>,
        readable_files: Vec<PathBuf>,
    },
    Cancel,
    SetMode(String),
    Respond(ApprovalId, Decision),
    ResetSessionApproval(String),
}

type Command = (Control, oneshot::Sender<HarnessResult<()>>);
type Prompt = Pin<Box<dyn Future<Output = Result<StopReason, AcpError>> + Send>>;
type AgentEvents = (VecDeque<AcpEvent>, mpsc::UnboundedReceiver<AcpEvent>);

struct Runtime {
    commands: mpsc::Sender<Command>,
    worker: JoinHandle<()>,
    grants: watch::Receiver<Vec<SessionApproval>>,
    modes: watch::Receiver<Option<ModeControl>>,
}

struct SessionChanges {
    grants: watch::Sender<Vec<SessionApproval>>,
    modes: watch::Sender<Option<ModeControl>>,
}

impl Runtime {
    fn start(
        requester: acp_client::Requester,
        session: schema::SessionId,
        events: AgentEvents,
        sender: mpsc::UnboundedSender<HarnessEvent>,
        project_approvals: Option<Arc<dyn ProjectApprovalPolicy>>,
        close_terminals: impl Future<Output = Result<(), String>> + Send + 'static,
        mode: Option<ModeControl>,
    ) -> Self {
        let (commands, incoming) = mpsc::channel(8);
        let (grant_changes, grants) = watch::channel(Vec::new());
        let (mode_changes, modes) = watch::channel(mode);
        let worker = tokio::spawn(async move {
            if drive_session(
                requester,
                session,
                events,
                incoming,
                sender,
                project_approvals.as_deref(),
                SessionChanges {
                    grants: grant_changes,
                    modes: mode_changes,
                },
            )
            .await
            .is_err()
            {
                tracing::debug!("ACP harness event loop ended with an error");
            }
            if close_terminals.await.is_err() {
                tracing::warn!("ACP terminal cleanup did not complete");
            }
        });
        Self {
            commands,
            worker,
            grants,
            modes,
        }
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
    (mut startup_events, mut events): AgentEvents,
    mut commands: mpsc::Receiver<Command>,
    sender: mpsc::UnboundedSender<HarnessEvent>,
    project_approvals: Option<&dyn ProjectApprovalPolicy>,
    changes: SessionChanges,
) -> HarnessResult<()> {
    let mut prompt: Option<Prompt> = None;
    let mut updates = Updates::default();
    let mut approvals = VecDeque::<PendingApproval>::new();
    let mut session_grants = SessionGrants::new(changes.grants);
    let mut terminals = HashSet::new();
    let mut released = Vec::new();
    let mut cancelling = false;
    let mut disconnect_deadline = None;
    loop {
        tokio::select! {
            biased;
            // ACP sends its final updates before replying to session/prompt.
            event = async {
                match startup_events.pop_front() {
                    Some(event) => Some(event),
                    None => events.recv().await,
                }
            } => {
                match event {
                    Some(AcpEvent::SessionUpdate(notification)) if notification.session_id == session => {
                        if update_modes(&changes.modes, &notification.update) {
                            publish(&sender, HarnessEvent::ModeChanged)?;
                        }
                        if let Some(event) = updates.apply(&notification.update) {
                            updates.publish(&sender, event)?;
                        }
                    }
                    Some(AcpEvent::PermissionRequested { request_id, session_id, tool_call, options, decision })
                        if session_id == session && prompt.is_some() && !cancelling => {
                        let approval = PendingApproval::new(request_id, tool_call, options, decision, project_approvals.is_some());
                        let idle = approvals.is_empty();
                        approvals.push_back(approval);
                        if idle {
                            show_next_approval(&sender, &mut approvals, &session_grants, project_approvals).await?;
                        }
                    }
                    Some(AcpEvent::PermissionCancelled { request_id }) => {
                        cancel_approval(&sender, &mut approvals, &session_grants, project_approvals, request_id).await?;
                    }
                    Some(AcpEvent::TerminalOutput { terminal_id, chunk, permit, .. }) if prompt.is_some() => {
                        updates.terminal_output(&sender, terminal_id, chunk, Arc::new(permit))?;
                    }
                    Some(AcpEvent::TerminalStarted { session_id, terminal_id, title })
                        if session_id == session => {
                        terminals.insert(terminal_id.clone());
                        publish(&sender, HarnessEvent::AgentTerminalStarted { id: terminal_id, title })?;
                    }
                    Some(AcpEvent::TerminalExited { terminal_id, exit_code, .. })
                        if terminals.contains(&terminal_id) => {
                        publish(&sender, HarnessEvent::AgentTerminalExited { id: terminal_id, exit_code })?;
                    }
                    Some(AcpEvent::TerminalReleased { terminal_id }) if terminals.remove(&terminal_id) => {
                        if prompt.is_some() {
                            publish(&sender, HarnessEvent::AgentTerminalExited { id: terminal_id.clone(), exit_code: None })?;
                            released.push(terminal_id);
                        } else {
                            publish(&sender, HarnessEvent::AgentTerminalRemoved { id: terminal_id })?;
                        }
                    }
                    Some(AcpEvent::Exited { code, stderr }) => {
                        withdraw_approvals(&sender, &mut approvals)?;
                        return publish(&sender, HarnessEvent::Exited { code, stderr });
                    }
                    Some(AcpEvent::ProtocolFailed { message }) => {
                        withdraw_approvals(&sender, &mut approvals)?;
                        return publish(&sender, HarnessEvent::ConnectionFailed { kind: FailureKind::Protocol, text: message });
                    }
                    None => {
                        withdraw_approvals(&sender, &mut approvals)?;
                        return publish(&sender, HarnessEvent::ConnectionFailed {
                            kind: FailureKind::Protocol, text: "The ACP connection closed.".into(),
                        });
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
                for id in released.drain(..) {
                    publish(&sender, HarnessEvent::AgentTerminalRemoved { id })?;
                }
                if matches!(outcome, Err(AcpError::TransportClosed(_))) {
                    // The process supervisor can report the actual status after the wire closes.
                    disconnect_deadline = Some(Instant::now() + Duration::from_secs(1));
                    continue;
                }
                let reason = finish_turn(&sender, outcome)?;
                publish(&sender, HarnessEvent::TurnFinished { reason })?;
            }
            _ = async {
                match disconnect_deadline {
                    Some(deadline) => sleep_until(deadline).await,
                    None => pending().await,
                }
            } => {
                return publish(&sender, HarnessEvent::ConnectionFailed {
                    kind: FailureKind::Protocol,
                    text: "The ACP connection closed without an exit status.".into(),
                });
            }
            command = commands.recv() => {
                let Some((command, answer)) = command else { return Ok(()); };
                let result = match command {
                    Control::Prompt { .. } if prompt.is_some() => Err(protocol("ACP turn is already running")),
                    Control::Prompt { .. } if disconnect_deadline.is_some() => Err(protocol("the ACP connection is closing")),
                    Control::Prompt { content, readable_files } => {
                        match requester.start_prompt_content(&session, content, readable_files).await {
                            Ok(turn) => {
                                updates = Updates::default();
                                publish(&sender, HarnessEvent::TurnStarted { turn_id: TurnId::new(uuid::Uuid::new_v4().to_string()) })?;
                                prompt = Some(Box::pin(turn));
                                Ok(())
                            }
                            Err(error) => Err(acp_error(error)),
                        }
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
                    Control::SetMode(_) if prompt.is_some() || disconnect_deadline.is_some() => Err(HarnessError::ModeUnavailable),
                    Control::SetMode(mode) => set_session_mode(&requester, &session, &changes.modes, &mode).await,
                    Control::Respond(id, decision) => {
                        respond(&sender, &mut approvals, &mut session_grants, project_approvals, id, decision).await
                    }
                    Control::ResetSessionApproval(id) => {
                        session_grants.reset(&id);
                        Ok(())
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
    let (kind, text) = match outcome {
        Ok(StopReason::EndTurn) => return Ok(TurnFinishReason::Completed),
        Ok(StopReason::Cancelled) => return Ok(TurnFinishReason::Cancelled),
        Ok(StopReason::MaxTokens) => (
            FailureKind::AgentRejected,
            "The ACP agent reached its token limit".into(),
        ),
        Ok(StopReason::MaxTurnRequests) => (
            FailureKind::AgentRejected,
            "The ACP agent reached its turn request limit".into(),
        ),
        Ok(StopReason::Refusal) => (
            FailureKind::AgentRejected,
            "The ACP agent refused to continue".into(),
        ),
        Ok(reason) => (
            FailureKind::AgentRejected,
            format!("The ACP agent stopped the turn ({reason:?})"),
        ),
        Err(error) => {
            let error = acp_error(error);
            (
                FailureKind::classify(&error).unwrap_or(FailureKind::Protocol),
                error.to_string(),
            )
        }
    };
    publish(sender, HarnessEvent::TurnError { kind, text })?;
    Ok(TurnFinishReason::Failed)
}

struct PendingApproval {
    request_id: u64,
    request: ApprovalRequest,
    allow: Option<schema::PermissionOptionId>,
    deny: Option<schema::PermissionOptionId>,
    fingerprint: Option<ApprovalFingerprint>,
    project_key: Option<String>,
    reply: PermissionReply,
}

#[derive(PartialEq)]
struct ApprovalFingerprint {
    fields: ToolCallUpdateFields,
    meta: Option<schema::Meta>,
}

struct SessionGrants {
    entries: Vec<(ApprovalFingerprint, SessionApproval)>,
    changes: watch::Sender<Vec<SessionApproval>>,
}

impl SessionGrants {
    fn new(changes: watch::Sender<Vec<SessionApproval>>) -> Self {
        Self {
            entries: Vec::new(),
            changes,
        }
    }

    fn contains(&self, fingerprint: &ApprovalFingerprint) -> bool {
        self.entries.iter().any(|(saved, _)| saved == fingerprint)
    }

    fn remember(&mut self, fingerprint: ApprovalFingerprint, request: ApprovalRequest) {
        self.entries.push((
            fingerprint,
            SessionApproval {
                id: request.approval_id.to_string(),
                title: request.title,
                arguments: request.arguments,
                paths: request.paths,
            },
        ));
        self.publish();
    }

    fn reset(&mut self, id: &str) {
        self.entries.retain(|(_, approval)| approval.id != id);
        self.publish();
    }

    fn publish(&self) {
        self.changes.send_replace(
            self.entries
                .iter()
                .map(|(_, grant)| grant.clone())
                .collect(),
        );
    }
}

impl PendingApproval {
    fn new(
        request_id: u64,
        call: schema::ToolCallUpdate,
        options: Vec<schema::PermissionOption>,
        reply: PermissionReply,
        project_approvals: bool,
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
        let mut fingerprint = None;
        if allow.is_some() {
            choices.push(Decision::Allow);
            // The title is agent-authored display text. Reuse an approval only
            // when the request identifies both an operation and its input.
            if fields.kind.is_some_and(|kind| kind != ToolKind::Other)
                && fields.raw_input.as_ref().is_some_and(has_approval_input)
            {
                choices.push(Decision::AllowSession);
                fingerprint = Some(ApprovalFingerprint {
                    fields: fields.clone().status(None).raw_output(None),
                    meta: call.meta,
                });
            }
        }
        let project_key = fingerprint
            .as_ref()
            .filter(|_| project_approvals)
            .and_then(
                |grant| match serde_json::to_string(&(&grant.fields, &grant.meta)) {
                    Ok(key) => Some(key),
                    Err(error) => {
                        tracing::warn!(%error, "could not encode an ACP project approval");
                        None
                    }
                },
            );
        if project_key.is_some() {
            choices.push(Decision::AllowProject);
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
            request_id,
            request,
            allow,
            deny: once(PermissionOptionKind::RejectOnce),
            fingerprint,
            project_key,
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

async fn cancel_approval(
    sender: &mpsc::UnboundedSender<HarnessEvent>,
    approvals: &mut VecDeque<PendingApproval>,
    session_grants: &SessionGrants,
    project_approvals: Option<&dyn ProjectApprovalPolicy>,
    request_id: u64,
) -> HarnessResult<()> {
    let Some(index) = approvals
        .iter()
        .position(|approval| approval.request_id == request_id)
    else {
        return Ok(());
    };
    let Some(approval) = approvals.remove(index) else {
        return Ok(());
    };
    if index == 0 {
        publish(
            sender,
            HarnessEvent::ApprovalWithdrawn {
                approval_id: approval.request.approval_id,
            },
        )?;
        show_next_approval(sender, approvals, session_grants, project_approvals).await?;
    }
    Ok(())
}

async fn show_next_approval(
    sender: &mpsc::UnboundedSender<HarnessEvent>,
    approvals: &mut VecDeque<PendingApproval>,
    session_grants: &SessionGrants,
    project_approvals: Option<&dyn ProjectApprovalPolicy>,
) -> HarnessResult<()> {
    while let Some(approval) = approvals.front() {
        let source = if approval
            .fingerprint
            .as_ref()
            .is_some_and(|grant| session_grants.contains(grant))
        {
            Some(ApprovalSource::SessionGrant)
        } else if let (Some(policy), Some(key)) = (project_approvals, &approval.project_key) {
            match policy.allows(key).await {
                Ok(true) => Some(ApprovalSource::ProjectGrant),
                Ok(false) => None,
                Err(error) => {
                    publish(
                        sender,
                        HarnessEvent::ApprovalPolicyError {
                            text: format!("Could not check saved project approvals: {error}"),
                        },
                    )?;
                    None
                }
            }
        } else {
            None
        };
        let (Some(source), Some(option)) = (source, approval.allow.clone()) else {
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
            .answer_confirmed(PermissionDecision::Selected(option))
            .await
            .is_ok()
        {
            publish(
                sender,
                HarnessEvent::ApprovalAutomaticallyResolved {
                    request: approval.request,
                    decision: Decision::Allow,
                    source,
                },
            )?;
        } else {
            tracing::debug!("ACP permission ended before its grant could be applied");
        }
    }
    Ok(())
}

async fn respond(
    sender: &mpsc::UnboundedSender<HarnessEvent>,
    approvals: &mut VecDeque<PendingApproval>,
    session_grants: &mut SessionGrants,
    project_approvals: Option<&dyn ProjectApprovalPolicy>,
    id: ApprovalId,
    decision: Decision,
) -> HarnessResult<()> {
    let pending = approvals
        .front()
        .filter(|approval| approval.request.approval_id == id)
        .ok_or(HarnessError::NoPendingApproval)?;
    if !pending
        .request
        .choices
        .as_ref()
        .is_some_and(|choices| choices.contains(&decision))
    {
        return Err(HarnessError::ApprovalOptionUnavailable);
    }
    let option = match decision {
        Decision::Allow | Decision::AllowSession | Decision::AllowProject => Some(
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
    if approval.reply.answer_confirmed(reply).await.is_err() {
        publish(sender, HarnessEvent::ApprovalWithdrawn { approval_id: id })?;
    } else {
        let decision = if decision == Decision::AllowProject
            && let (Some(policy), Some(key)) = (project_approvals, approval.project_key)
            && let Err(error) = policy.remember(&key, &approval.request).await
        {
            publish(
                sender,
                HarnessEvent::ApprovalPolicyError {
                    text: format!("Allowed once. Could not save the project approval: {error}"),
                },
            )?;
            Decision::Allow
        } else {
            decision
        };
        if decision == Decision::AllowSession
            && let Some(fingerprint) = approval.fingerprint
        {
            session_grants.remember(fingerprint, approval.request);
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
    show_next_approval(sender, approvals, session_grants, project_approvals).await
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
            stream_permit: None,
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
            publish(
                sender,
                HarnessEvent::CommandOutput {
                    tool_call_id,
                    text,
                    output_permit: None,
                },
            )?;
        }
        Ok(())
    }

    fn terminal_output(
        &mut self,
        sender: &mpsc::UnboundedSender<HarnessEvent>,
        terminal_id: String,
        chunk: String,
        permit: Arc<tokio::sync::OwnedSemaphorePermit>,
    ) -> HarnessResult<()> {
        if let Some(tool_call_id) = self.terminal_tools.get(&terminal_id) {
            publish(
                sender,
                HarnessEvent::CommandOutput {
                    tool_call_id: tool_call_id.clone(),
                    text: chunk,
                    output_permit: Some(permit),
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
        output: output
            .as_deref()
            .filter(|output| !output.is_empty())
            .map(|output| {
                if kind == "Execute" {
                    bounded_command_output(output)
                } else {
                    output.to_owned()
                }
            }),
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
    startup_events: &mut VecDeque<AcpEvent>,
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
            AcpEvent::ProtocolFailed { message } => return Err(protocol(message)),
            event @ (AcpEvent::TerminalStarted { .. }
            | AcpEvent::TerminalExited { .. }
            | AcpEvent::TerminalReleased { .. }) => startup_events.push_back(event),
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
    use acp_client::fake_agent::{
        FakeScript, Shared, connect_fake_agent, connect_fake_agent_with_terminals,
    };
    use acp_client::schema::{
        Diff, PermissionOption, TextContent, ToolCall, ToolCallStatus, ToolCallUpdate,
        ToolCallUpdateFields, ToolKind,
    };

    use super::*;

    #[test]
    fn command_tool_events_keep_a_bounded_unicode_tail() -> Result<(), Box<dyn std::error::Error>> {
        let output = format!("{}tail", "界".repeat(crate::MAX_COMMAND_OUTPUT_BYTES));
        let mut entry = acp_client::Entry {
            id: 1,
            kind: acp_client::EntryKind::ToolCall {
                tool_call_id: "tool".into(),
                title: "command".into(),
                kind: "Execute".into(),
                status: acp_client::ToolStatus::Completed,
                output: Some(output.clone()),
                diffs: Vec::new(),
                terminal_id: None,
            },
        };
        let Some(HarnessEvent::ToolCall {
            output: Some(bounded),
            ..
        }) = tool_event(&entry)
        else {
            return Err("missing command output".into());
        };
        assert!(bounded.len() <= crate::MAX_COMMAND_OUTPUT_BYTES);
        assert!(bounded.starts_with(crate::COMMAND_OUTPUT_OMITTED));
        assert!(bounded.ends_with("tail"));
        if let acp_client::EntryKind::ToolCall { kind, .. } = &mut entry.kind {
            *kind = "Read".into();
        }
        assert!(
            matches!(tool_event(&entry), Some(HarnessEvent::ToolCall { output: Some(text), .. }) if text == output)
        );
        Ok(())
    }

    #[tokio::test]
    async fn terminal_output_keeps_its_permit_until_the_harness_event_is_consumed()
    -> Result<(), Box<dyn std::error::Error>> {
        let permits = Arc::new(tokio::sync::Semaphore::new(1));
        let permit = permits.clone().acquire_owned().await?;
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let mut updates = Updates::default();
        updates
            .terminal_tools
            .insert("terminal".into(), ToolCallId::new("tool"));
        updates.terminal_output(
            &sender,
            "terminal".into(),
            "output".into(),
            Arc::new(permit),
        )?;
        assert_eq!(permits.available_permits(), 0);
        let event = receiver.recv().await.ok_or("missing output")?;
        let copy = event.clone();
        drop(event);
        assert_eq!(permits.available_permits(), 0);
        drop(copy);
        assert_eq!(permits.available_permits(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn early_terminal_output_releases_its_permit_after_bounded_buffering()
    -> Result<(), Box<dyn std::error::Error>> {
        let permits = Arc::new(tokio::sync::Semaphore::new(1));
        let permit = permits.clone().acquire_owned().await?;
        let (sender, _receiver) = mpsc::unbounded_channel();
        let mut updates = Updates::default();
        updates.terminal_output(
            &sender,
            "terminal".into(),
            "x".repeat(crate::MAX_COMMAND_OUTPUT_BYTES + 10),
            Arc::new(permit),
        )?;
        assert_eq!(permits.available_permits(), 1);
        let buffered = updates
            .early_output
            .get("terminal")
            .ok_or("missing early output")?;
        assert!(buffered.len() <= crate::MAX_COMMAND_OUTPUT_BYTES);
        assert!(buffered.ends_with("xxx"));
        Ok(())
    }

    async fn fake(
        script: FakeScript,
    ) -> Result<(AcpHarness, Capabilities, Shared), Box<dyn std::error::Error>> {
        fake_at(script, std::env::temp_dir()).await
    }

    async fn fake_at(
        script: FakeScript,
        cwd: PathBuf,
    ) -> Result<(AcpHarness, Capabilities, Shared), Box<dyn std::error::Error>> {
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

    async fn complete_attachment_turn(harness: &mut AcpHarness) -> HarnessResult<()> {
        timeout(Duration::from_secs(5), async {
            while let Some(event) = harness.events().recv().await {
                if let HarnessEvent::TurnFinished { reason } = event {
                    assert_eq!(reason, TurnFinishReason::Completed);
                    return Ok(());
                }
            }
            Err(HarnessError::ChannelClosed)
        })
        .await
        .map_err(|_| protocol("attachment turn did not finish"))?
    }

    #[tokio::test]
    async fn chooses_an_advertised_acp_mode_through_the_negotiated_api()
    -> Result<(), Box<dyn std::error::Error>> {
        for modern in [false, true] {
            let legacy = schema::SessionModeState::new(
                "ask",
                vec![
                    schema::SessionMode::new("ask", "Ask"),
                    schema::SessionMode::new("plan", "Plan"),
                ],
            );
            let config = |current: &str| -> Result<schema::SessionConfigOption, serde_json::Error> {
                serde_json::from_value(serde_json::json!({
                    "id": "operation-mode", "name": "Mode", "category": "mode", "type": "select",
                    "currentValue": current,
                    "options": [{"value": "ask", "name": "Ask"}, {"value": "plan", "name": "Plan"}],
                }))
            };
            let (mut harness, _, observed) = fake(FakeScript {
                legacy_modes: Some(legacy),
                mode_config: if modern {
                    Some((config("ask")?, config("plan")?))
                } else {
                    None
                },
                ..FakeScript::default()
            })
            .await?;
            harness.open_session(None).await?;
            assert_eq!(
                harness.modes().ok_or("missing modes")?.current.as_deref(),
                Some("ask")
            );
            assert!(matches!(
                harness.set_mode("unknown").await,
                Err(HarnessError::ModeUnavailable)
            ));
            harness.set_mode("plan").await?;
            assert_eq!(
                harness
                    .modes()
                    .ok_or("missing mode after selection")?
                    .current
                    .as_deref(),
                Some("plan")
            );
            let observed = observed.lock().map_err(|_| "poisoned observations")?;
            if modern {
                let [request] = observed.config_changes.as_slice() else {
                    return Err("expected one config request".into());
                };
                assert_eq!(request["configId"], "operation-mode");
                assert!(observed.mode_changes.is_empty());
            } else {
                assert_eq!(observed.mode_changes, ["plan"]);
                assert!(observed.config_changes.is_empty());
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn an_acp_prompt_can_read_only_the_selected_cached_files_until_it_finishes()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let directory_path = directory.path().canonicalize()?;
        let worktree = directory_path.join("worktree");
        let cache = directory_path.join("cache");
        std::fs::create_dir(&worktree)?;
        std::fs::create_dir(&cache)?;
        let selected = cache.join("attached.txt");
        let neighbor = cache.join("other.txt");
        std::fs::write(&selected, "selected attachment\n")?;
        std::fs::write(&neighbor, "other content")?;
        let (mut harness, _, observed) = fake_at(
            FakeScript {
                read_paths: vec![selected.clone(), neighbor],
                write: Some((selected.clone(), "changed".into())),
                ..FakeScript::default()
            },
            worktree,
        )
        .await?;
        harness.open_session(None).await?;
        harness
            .prompt(PromptInput {
                text: "Read the attachment.".into(),
                attachments: vec![crate::PromptAttachment {
                    path: selected.clone(),
                    image_mime_type: None,
                    metadata: None,
                }],
                ..PromptInput::default()
            })
            .await?;
        complete_attachment_turn(&mut harness).await?;
        harness
            .prompt(PromptInput::text("Continue without an attachment."))
            .await?;
        complete_attachment_turn(&mut harness).await?;
        let observed = observed.lock().map_err(|_| "poisoned ACP observations")?;
        let [first, neighbor, after, _] = observed.file_reads.as_slice() else {
            return Err("expected both read attempts on both turns".into());
        };
        assert_eq!(first.as_deref(), Ok("selected attachment\n"));
        assert!(neighbor.is_err());
        assert!(after.is_err());
        assert_eq!(observed.file_writes.len(), 2);
        assert!(observed.file_writes.iter().all(Result::is_err));
        assert_eq!(std::fs::read_to_string(selected)?, "selected attachment\n");
        Ok(())
    }

    #[tokio::test]
    async fn sends_image_bytes_only_when_acp_advertises_image_support()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().canonicalize()?;
        let image_path = root.join("a screenshot.png");
        let file_path = root.join("notes.txt");
        let bytes = b"fixture image bytes";
        std::fs::write(&image_path, bytes)?;
        std::fs::write(&file_path, "fixture text")?;
        for prompt_images in [true, false] {
            let (mut harness, capabilities, observed) = fake(FakeScript {
                prompt_images,
                ..FakeScript::default()
            })
            .await?;
            assert_eq!(capabilities.images, prompt_images);
            assert!(capabilities.resource_links);
            harness.open_session(None).await?;
            harness
                .prompt(PromptInput {
                    text: "Review these attachments.".into(),
                    attachments: vec![
                        crate::PromptAttachment {
                            path: image_path.clone(),
                            image_mime_type: Some("image/png".into()),
                            metadata: None,
                        },
                        crate::PromptAttachment {
                            path: file_path.clone(),
                            image_mime_type: None,
                            metadata: None,
                        },
                    ],
                    ..PromptInput::default()
                })
                .await?;
            complete_attachment_turn(&mut harness).await?;
            let observed = observed.lock().map_err(|_| "poisoned ACP observations")?;
            let [content] = observed.prompt_content.as_slice() else {
                return Err("expected one prompt".into());
            };
            let [
                ContentBlock::Text(text),
                image,
                ContentBlock::ResourceLink(file),
            ] = content.as_slice()
            else {
                return Err("expected a text block and two attachments".into());
            };
            assert_eq!(text.text, "Review these attachments.");
            assert_eq!(file.name, "notes.txt");
            match image {
                ContentBlock::Image(image) if prompt_images => {
                    assert_eq!(image.mime_type, "image/png");
                    assert_eq!(STANDARD.decode(&image.data)?, bytes);
                }
                ContentBlock::ResourceLink(link) if !prompt_images => {
                    assert_eq!(link.name, "a screenshot.png");
                    assert!(link.uri.ends_with("/a%20screenshot.png"));
                }
                _ => return Err("unexpected ACP image content".into()),
            }
        }
        Ok(())
    }

    async fn configured_claude_session(
        settings: &std::path::Path,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let command = crate::settings::AgentSettings::read(settings)?
            .command("agents.claude")?
            .ok_or("configure agents.claude.executable in the smoke settings file")?;
        let root = tempfile::tempdir()?;
        let project = root.path().join("project");
        std::fs::create_dir(&project)?;
        let environment = [
            ("HOME", "home"),
            ("CLAUDE_CONFIG_DIR", "claude"),
            ("XDG_CONFIG_HOME", "config"),
            ("XDG_DATA_HOME", "data"),
            ("XDG_CACHE_HOME", "cache"),
            ("XDG_STATE_HOME", "state"),
        ]
        .into_iter()
        .map(|(name, path)| {
            let path = root.path().join(path);
            std::fs::create_dir(&path)?;
            Ok((name.to_owned(), path.to_string_lossy().into_owned()))
        })
        .collect::<std::io::Result<_>>()?;
        let spec = SpawnSpec {
            program: command
                .executable
                .into_os_string()
                .into_string()
                .map_err(|_| "the Claude Code ACP executable path must be UTF-8")?,
            args: command.args,
            cwd: project,
            environment,
        };
        let mut harness = AcpHarness::default();
        harness.start(spec).await?;
        let opened = harness.open_session(None).await;
        let shutdown = harness.shutdown().await;
        assert!(!opened?.as_str().is_empty());
        shutdown?;
        Ok(())
    }

    #[cfg(feature = "live-claude")]
    #[tokio::test]
    async fn installed_claude_code_opens_an_acp_session() -> Result<(), Box<dyn std::error::Error>>
    {
        let settings = std::env::var_os("SHEPHERD_CLAUDE_TEST_SETTINGS")
            .ok_or("set SHEPHERD_CLAUDE_TEST_SETTINGS to a settings.json path")?;
        configured_claude_session(&PathBuf::from(settings)).await
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn claude_smoke_launches_the_configured_command_in_an_isolated_profile()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let script = directory.path().join("claude-acp-fixture.sh");
        std::fs::write(
            &script,
            r#"
[ "$1" = "argument with spaces" ] && [ -z "$2" ] || exit 2
[ -d "$HOME" ] && [ -d "$CLAUDE_CONFIG_DIR" ] && [ "${PWD##*/}" = project ] || exit 3
while IFS= read -r request; do
    id=$(printf '%s\n' "$request" | sed -nE 's/.*"id":("[^"]+"|[0-9]+).*/\1/p')
    case "$request" in
        *'"method":"initialize"'*)
            printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":1}}\n' "$id";;
        *'"method":"session/new"'*)
            printf '{"jsonrpc":"2.0","id":%s,"result":{"sessionId":"claude-fixture"}}\n' "$id";;
        *) exit 4;;
    esac
done
"#,
        )?;
        let settings = directory.path().join("settings.json");
        std::fs::write(
            &settings,
            serde_json::to_vec(&serde_json::json!({"app": {
                "agents.claude.executable": "/bin/sh",
                "agents.claude.args": [script, "argument with spaces"]
            }}))?,
        )?;
        configured_claude_session(&settings).await
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn acp_process_record_is_retained_until_cleanup_succeeds()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::sync::atomic::{AtomicBool, Ordering};
        struct Tracker {
            pid: std::sync::Mutex<Option<u32>>,
            fail_start: bool,
            fail_clear: AtomicBool,
        }
        #[async_trait]
        impl crate::AgentProcessTracker for Tracker {
            async fn started(&self, pid: u32) -> Result<String, String> {
                *self.pid.lock().map_err(|error| error.to_string())? = Some(pid);
                if self.fail_start {
                    return Err("fixture save failure".into());
                }
                Ok("fixture-record".into())
            }
            async fn stopped(&self, id: &str) -> Result<(), String> {
                assert_eq!(id, "fixture-record");
                if self.fail_clear.swap(false, Ordering::SeqCst) {
                    return Err("fixture clear failure".into());
                }
                *self.pid.lock().map_err(|error| error.to_string())? = None;
                Ok(())
            }
        }
        let directory = tempfile::tempdir()?;
        let script = directory.path().join("tracked-acp.sh");
        std::fs::write(
            &script,
            r#"
while IFS= read -r request; do
    case "$request" in
        *'"method":"initialize"'*)
            id=$(printf '%s\n' "$request" | sed -nE 's/.*"id":("[^"]+"|[0-9]+).*/\1/p')
            printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":1}}\n' "$id";;
    esac
done
"#,
        )?;
        let spec = SpawnSpec {
            program: "/bin/sh".into(),
            args: vec![script.to_string_lossy().into_owned()],
            cwd: directory.path().to_owned(),
            ..Default::default()
        };
        let tracker = Arc::new(Tracker {
            pid: std::sync::Mutex::new(None),
            fail_start: false,
            fail_clear: AtomicBool::new(true),
        });
        let mut harness = AcpHarness::default();
        harness.set_process_tracker(tracker.clone());
        harness.start(spec.clone()).await?;
        assert!(
            tracker
                .pid
                .lock()
                .map_err(|error| error.to_string())?
                .is_some()
        );
        let error = harness
            .shutdown()
            .await
            .expect_err("cleanup must fail once");
        assert!(error.to_string().contains("fixture clear failure"));
        assert!(harness.connection.is_some());
        harness.shutdown().await?;
        assert!(
            tracker
                .pid
                .lock()
                .map_err(|error| error.to_string())?
                .is_none()
        );
        let failing = Arc::new(Tracker {
            pid: std::sync::Mutex::new(None),
            fail_start: true,
            fail_clear: AtomicBool::new(false),
        });
        let mut harness = AcpHarness::default();
        harness.set_process_tracker(failing.clone());
        let error = harness.start(spec).await.expect_err("recording must fail");
        assert!(error.to_string().contains("fixture save failure"));
        let pid = failing
            .pid
            .lock()
            .map_err(|error| error.to_string())?
            .ok_or("missing PID")?;
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if unsafe { libc::kill(libc::pid_t::try_from(pid)?, 0) } != 0 {
                    break Ok::<(), Box<dyn std::error::Error>>(());
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await??;
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shutdown_waits_for_the_acp_process_to_exit() -> Result<(), Box<dyn std::error::Error>>
    {
        let directory = tempfile::tempdir()?;
        let script = directory.path().join("acp-shutdown.sh");
        std::fs::write(
            &script,
            r#"
while IFS= read -r request; do
    case "$request" in
        *'"method":"initialize"'*)
            id=$(printf '%s\n' "$request" | sed -nE 's/.*"id":("[^"]+"|[0-9]+).*/\1/p')
            printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":1}}\n' "$id";;
    esac
done
sleep 0.1
printf stopped > stopped
"#,
        )?;
        let mut harness = AcpHarness::default();
        harness
            .start(SpawnSpec {
                program: "/bin/sh".into(),
                args: vec![script.to_string_lossy().into_owned()],
                cwd: directory.path().to_owned(),
                ..SpawnSpec::default()
            })
            .await?;
        harness.shutdown().await?;
        assert_eq!(
            std::fs::read_to_string(directory.path().join("stopped"))?,
            "stopped"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn separates_sign_in_protocol_and_exit_failures() -> Result<(), Box<dyn std::error::Error>>
    {
        let directory = tempfile::tempdir()?;
        let script = directory.path().join("acp-failure.sh");
        std::fs::write(
            &script,
            r#"
while IFS= read -r request; do
    id=$(printf '%s\n' "$request" | sed -nE 's/.*"id":("[^"]+"|[0-9]+).*/\1/p')
    case "$request" in
        *'"method":"initialize"'*)
            printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":1,"authMethods":[{"id":"cli","name":"Local CLI sign-in"}]}}\n' "$id";;
        *'"method":"session/new"'*)
            printf '{"jsonrpc":"2.0","id":%s,"result":{"sessionId":"fixture"}}\n' "$id";;
        *'"method":"session/prompt"'*)
            case "$1" in
                auth) printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32000,"message":"Account expired"}}\n' "$id";;
                protocol) echo 'not valid JSON';;
                oversized) python3 -c 'import sys; sys.stdout.write("x" * (8 * 1024 * 1024 + 2)); sys.stdout.flush()';;
                exit) echo 'ACP fixture crashed' >&2; exit 23;;
            esac;;
    esac
done
"#,
        )?;
        for mode in ["auth", "protocol", "oversized", "exit"] {
            let mut harness = AcpHarness::default();
            harness
                .start(SpawnSpec {
                    program: "/bin/sh".into(),
                    args: vec![script.to_string_lossy().into_owned(), mode.into()],
                    cwd: directory.path().to_owned(),
                    ..SpawnSpec::default()
                })
                .await?;
            harness.open_session(None).await?;
            harness.prompt(PromptInput::text("go")).await?;
            let outcome = timeout(Duration::from_secs(5), async {
                while let Some(event) = harness.events().recv().await {
                    if matches!(
                        event,
                        HarnessEvent::TurnError { .. }
                            | HarnessEvent::ConnectionFailed { .. }
                            | HarnessEvent::Exited { .. }
                    ) {
                        return Ok(event);
                    }
                }
                Err(HarnessError::ChannelClosed)
            })
            .await??;
            match (mode, outcome) {
                (
                    "auth",
                    HarnessEvent::TurnError {
                        kind: FailureKind::AuthRequired,
                        text,
                    },
                ) => {
                    assert!(
                        text.contains("Account expired") && text.contains("Local CLI sign-in"),
                        "{text}"
                    );
                }
                (
                    "protocol",
                    HarnessEvent::ConnectionFailed {
                        kind: FailureKind::Protocol,
                        text,
                    },
                ) => assert!(text.contains("malformed JSON")),
                (
                    "oversized",
                    HarnessEvent::ConnectionFailed {
                        kind: FailureKind::Protocol,
                        text,
                    },
                ) => assert!(text.contains("exceeds 8388608 bytes"), "{text}"),
                (
                    "exit",
                    HarnessEvent::Exited {
                        code: Some(23),
                        stderr,
                    },
                ) => assert!(stderr.contains("ACP fixture crashed")),
                (mode, event) => panic!("{mode}: {event:?}"),
            }
            harness.shutdown().await?;
        }
        Ok(())
    }

    struct ExitingTerminal;

    #[async_trait]
    impl TerminalBackend for ExitingTerminal {
        async fn spawn(
            &self,
            _: TerminalLaunch,
            events: mpsc::Sender<TerminalProcessEvent>,
        ) -> Result<(), String> {
            events
                .send(TerminalProcessEvent::Exited(
                    TerminalExitStatus::new().exit_code(7),
                ))
                .await
                .map_err(|error| error.to_string())
        }

        async fn stop(&self, _: &str) -> Result<(), String> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn shutdown_reports_terminal_stop_failure_and_retries()
    -> Result<(), Box<dyn std::error::Error>> {
        struct FailingOnce(std::sync::atomic::AtomicUsize);
        #[async_trait]
        impl TerminalBackend for FailingOnce {
            async fn spawn(
                &self,
                _: TerminalLaunch,
                events: mpsc::Sender<TerminalProcessEvent>,
            ) -> Result<(), String> {
                events
                    .send(TerminalProcessEvent::Exited(
                        TerminalExitStatus::new().exit_code(0),
                    ))
                    .await
                    .map_err(|error| error.to_string())
            }
            async fn stop(&self, _: &str) -> Result<(), String> {
                if self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                    Err("fixture terminal stop failure".to_owned())
                } else {
                    Ok(())
                }
            }
        }
        let directory = tempfile::tempdir()?;
        let cwd = directory.path().to_path_buf();
        let backend = Arc::new(FailingOnce(std::sync::atomic::AtomicUsize::new(0)));
        let (connection, _) = connect_fake_agent_with_terminals(
            FakeScript {
                terminal: Some(("fixture".into(), Vec::new())),
                ..Default::default()
            },
            ConnectOptions {
                roots: SessionRoots::new([cwd.clone()]),
                tool_permissions: ToolPermissions::default(),
            },
            backend.clone(),
        )
        .await?;
        let mut harness = AcpHarness::default();
        harness.attach(connection, cwd, "fake-acp".into());
        harness.open_session(None).await?;
        harness.prompt(PromptInput::text("run a terminal")).await?;
        loop {
            if matches!(next(&mut harness).await?, HarnessEvent::TurnFinished { .. }) {
                break;
            }
        }
        let error = match harness.shutdown().await {
            Ok(()) => return Err("terminal stop failure was not reported".into()),
            Err(error) => error,
        };
        assert!(error.to_string().contains("fixture terminal stop failure"));
        assert!(harness.connection.is_some());
        harness.shutdown().await?;
        assert_eq!(backend.0.load(std::sync::atomic::Ordering::SeqCst), 2);
        Ok(())
    }

    #[tokio::test]
    async fn a_released_pty_pane_is_removed_when_the_acp_turn_settles()
    -> Result<(), Box<dyn std::error::Error>> {
        let cwd = std::env::temp_dir();
        let (connection, _) = connect_fake_agent_with_terminals(
            FakeScript {
                terminal: Some(("fixture".into(), Vec::new())),
                release_terminal: true,
                ..FakeScript::default()
            },
            ConnectOptions {
                roots: SessionRoots::new([cwd.clone()]),
                tool_permissions: ToolPermissions::default(),
            },
            Arc::new(ExitingTerminal),
        )
        .await?;
        let mut harness = AcpHarness::default();
        harness.attach(connection, cwd, "fake-acp".into());
        harness.open_session(None).await?;
        harness.prompt(PromptInput::text("run a terminal")).await?;
        let mut terminal = None;
        let mut exited = false;
        let mut removed = false;
        loop {
            match next(&mut harness).await? {
                HarnessEvent::AgentTerminalStarted { id, title } => {
                    assert_eq!(title, "fixture");
                    terminal = Some(id);
                }
                HarnessEvent::AgentTerminalExited {
                    id,
                    exit_code: Some(7),
                } => {
                    assert_eq!(terminal.as_deref(), Some(id.as_str()));
                    exited = true;
                }
                HarnessEvent::AgentTerminalRemoved { id } => {
                    assert_eq!(terminal.as_deref(), Some(id.as_str()));
                    removed = true;
                }
                HarnessEvent::TurnFinished { reason } => {
                    assert_eq!(reason, TurnFinishReason::Completed);
                    assert!(exited && removed);
                    break;
                }
                _ => {}
            }
        }
        Ok(())
    }

    async fn next(harness: &mut AcpHarness) -> Result<HarnessEvent, Box<dyn std::error::Error>> {
        timeout(Duration::from_secs(5), harness.events().recv())
            .await?
            .ok_or("ACP harness closed".into())
    }

    fn chunk(text: &str) -> ContentChunk {
        ContentChunk::new(ContentBlock::Text(TextContent::new(text)))
    }

    #[derive(Default)]
    struct TestProjectPolicy {
        key: tokio::sync::RwLock<Option<String>>,
        read_only: std::sync::atomic::AtomicBool,
    }

    #[async_trait]
    impl ProjectApprovalPolicy for TestProjectPolicy {
        async fn allows(&self, key: &str) -> Result<bool, String> {
            Ok(self.key.read().await.as_deref() == Some(key))
        }

        async fn remember(&self, key: &str, _: &ApprovalRequest) -> Result<(), String> {
            if self.read_only.load(std::sync::atomic::Ordering::Relaxed) {
                return Err("read-only test store".into());
            }
            *self.key.write().await = Some(key.into());
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_project_grant_is_reused_until_reset_in_the_live_connection()
    -> Result<(), Box<dyn std::error::Error>> {
        let script = FakeScript {
            permission: Some(ToolCallUpdate::new(
                "tool-1",
                ToolCallUpdateFields::new()
                    .kind(ToolKind::Execute)
                    .title("Run a command")
                    .raw_input(serde_json::json!({"command": "git status"})),
            )),
            permission_options: vec![PermissionOption::new(
                "once",
                "Allow once",
                PermissionOptionKind::AllowOnce,
            )],
            ..FakeScript::default()
        };
        let (mut harness, _, observed) = fake(script).await?;
        let policy = Arc::new(TestProjectPolicy::default());
        harness.set_project_approval_policy(policy.clone());
        harness.open_session(None).await?;
        for (prompt, decision) in [
            ("grant", Some(Decision::AllowProject)),
            ("reuse", None),
            ("after reset", Some(Decision::AllowProject)),
            ("after failed save", Some(Decision::Allow)),
        ] {
            let save_fails = prompt == "after reset";
            if save_fails {
                *policy.key.write().await = None;
                policy
                    .read_only
                    .store(true, std::sync::atomic::Ordering::Relaxed);
            }
            harness.prompt(PromptInput::text(prompt)).await?;
            let mut automatic = false;
            let mut saved_decision = None;
            let mut save_error = false;
            loop {
                match next(&mut harness).await? {
                    HarnessEvent::ApprovalRequested { request } => {
                        let decision = decision.ok_or("unexpected manual approval")?;
                        assert!(
                            request
                                .choices
                                .as_ref()
                                .is_some_and(|choices| choices.contains(&Decision::AllowProject))
                        );
                        harness.respond(request.approval_id, decision).await?;
                    }
                    HarnessEvent::ApprovalResolved { decision, .. } => {
                        saved_decision = Some(decision)
                    }
                    HarnessEvent::ApprovalPolicyError { text } => {
                        assert!(text.contains("Allowed once."));
                        save_error = true;
                    }
                    HarnessEvent::ApprovalAutomaticallyResolved { source, .. } => {
                        assert_eq!(source, ApprovalSource::ProjectGrant);
                        automatic = true;
                    }
                    HarnessEvent::TurnFinished { reason } => {
                        assert_eq!(reason, TurnFinishReason::Completed);
                        break;
                    }
                    _ => {}
                }
            }
            assert_eq!(automatic, decision.is_none());
            assert_eq!(save_error, save_fails);
            assert_eq!(
                saved_decision,
                if save_fails {
                    Some(Decision::Allow)
                } else {
                    decision
                }
            );
        }
        let observed = observed.lock().map_err(|_| "fake observations poisoned")?;
        assert_eq!(observed.permission_responses.len(), 4);
        assert!(
            observed
                .permission_responses
                .iter()
                .all(|choice| choice.contains("once"))
        );
        Ok(())
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

        harness.prompt(PromptInput::text("continue")).await?;
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
    async fn reuses_and_resets_an_exact_permission_for_the_live_session()
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
        harness.prompt(PromptInput::text("first")).await?;
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
        let grant_id = request.approval_id.to_string();
        harness
            .respond(request.approval_id, Decision::AllowSession)
            .await?;
        assert_eq!(
            harness.session_approvals(),
            [SessionApproval {
                id: grant_id.clone(),
                title: "Run a command".into(),
                arguments: serde_json::json!({"command": "git status"}),
                paths: Vec::new(),
            }]
        );
        loop {
            let event = next(&mut harness).await?;
            transcript.apply(&event);
            if matches!(event, HarnessEvent::TurnFinished { .. }) {
                break;
            }
        }

        harness.prompt(PromptInput::text("second")).await?;
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

        harness.reset_session_approval("stale-grant").await?;
        assert_eq!(harness.session_approvals().len(), 1);
        harness.reset_session_approval(&grant_id).await?;
        harness.reset_session_approval(&grant_id).await?;
        assert!(harness.session_approvals().is_empty());
        harness.prompt(PromptInput::text("after reset")).await?;
        let request = loop {
            match next(&mut harness).await? {
                HarnessEvent::ApprovalRequested { request } => break request,
                HarnessEvent::ApprovalAutomaticallyResolved { .. } => {
                    return Err("reset grant was reused".into());
                }
                _ => {}
            }
        };
        harness
            .respond(request.approval_id, Decision::AllowSession)
            .await?;
        loop {
            if matches!(next(&mut harness).await?, HarnessEvent::TurnFinished { .. }) {
                break;
            }
        }
        assert_eq!(harness.session_approvals().len(), 1);
        harness.shutdown().await?;
        assert!(harness.session_approvals().is_empty());
        let observed = observed.lock().map_err(|_| "fake observations poisoned")?;
        assert_eq!(observed.permission_responses.len(), 3);
        assert!(
            observed
                .permission_responses
                .iter()
                .all(|choice| choice.contains("once"))
        );
        Ok(())
    }

    #[tokio::test]
    async fn cancelled_approval_withdraws_visible_and_skips_queued_requests()
    -> Result<(), Box<dyn std::error::Error>> {
        let pending = |request_id| {
            let (reply, _response) = futures_channel::oneshot::channel();
            PendingApproval::new(
                request_id,
                ToolCallUpdate::new("tool", ToolCallUpdateFields::new()),
                Vec::new(),
                PermissionReply::new(reply),
                false,
            )
        };
        let first = pending(1);
        let second = pending(2);
        let third = pending(3);
        let first_id = first.request.approval_id.clone();
        let third_id = third.request.approval_id.clone();
        let mut queue = VecDeque::from([first, second, third]);
        let (grant_changes, _) = watch::channel(Vec::new());
        let grants = SessionGrants::new(grant_changes);
        let (sender, mut events) = mpsc::unbounded_channel();

        cancel_approval(&sender, &mut queue, &grants, None, 2).await?;
        assert!(events.try_recv().is_err());
        cancel_approval(&sender, &mut queue, &grants, None, 1).await?;
        assert!(
            matches!(events.try_recv()?, HarnessEvent::ApprovalWithdrawn { approval_id } if approval_id == first_id)
        );
        assert!(
            matches!(events.try_recv()?, HarnessEvent::ApprovalRequested { request } if request.approval_id == third_id)
        );
        cancel_approval(&sender, &mut queue, &grants, None, 99).await?;
        assert!(events.try_recv().is_err());
        Ok(())
    }

    #[tokio::test]
    async fn queued_session_grant_never_covers_a_changed_command()
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
                PendingApproval::new(0, request, options, PermissionReply::new(reply), false),
                response,
            )
        };
        let (first, mut first_response) = pending("git status");
        let (identical, mut identical_response) = pending("git status");
        let (changed, mut changed_response) = pending("git clean -fd");
        let first_id = first.request.approval_id.clone();
        let changed_id = changed.request.approval_id.clone();
        let mut queue = VecDeque::from([first, identical, changed]);
        let (grant_changes, _) = watch::channel(Vec::new());
        let mut grants = SessionGrants::new(grant_changes);
        let (sender, mut events) = mpsc::unbounded_channel();
        respond(
            &sender,
            &mut queue,
            &mut grants,
            None,
            first_id,
            Decision::AllowSession,
        )
        .await?;
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
    async fn agent_cancelled_approval_is_withdrawn_before_next_request()
    -> Result<(), Box<dyn std::error::Error>> {
        let (mut harness, _, _) = fake(FakeScript {
            permission: Some(tool_request()),
            cancel_permission: true,
            permission_after_cancel: true,
            ..FakeScript::default()
        })
        .await?;
        harness.open_session(None).await?;
        harness
            .prompt(PromptInput::text("request approval"))
            .await?;
        let first = loop {
            if let HarnessEvent::ApprovalRequested { request } = next(&mut harness).await? {
                break request.approval_id;
            }
        };
        assert!(
            matches!(next(&mut harness).await?, HarnessEvent::ApprovalWithdrawn { approval_id } if approval_id == first)
        );
        let second = loop {
            if let HarnessEvent::ApprovalRequested { request } = next(&mut harness).await? {
                break request.approval_id;
            }
        };
        assert_ne!(first, second);
        harness.respond(second, Decision::Deny).await?;
        loop {
            if matches!(next(&mut harness).await?, HarnessEvent::TurnFinished { .. }) {
                break;
            }
        }
        harness.shutdown().await?;
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
            .prompt(PromptInput::text("wait for permission"))
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
