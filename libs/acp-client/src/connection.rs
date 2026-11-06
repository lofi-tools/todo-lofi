//! Connecting to an agent process and driving a session.
//!
//! The SDK's own [`AcpAgent`](agent_client_protocol::AcpAgent) cannot set a
//! child's working directory, which this feature needs, so [`ProjectAgent`]
//! implements the transport directly: it spawns the process with `cwd`, wires
//! stdio into newline-delimited JSON-RPC, drains stderr into a bounded buffer,
//! and tears the process group down when the connection ends.
//!
//! Handlers run on the connection's single event loop task, so any handler that
//! would wait (a permission decision, a terminal exit, a file read) is offloaded
//! with `tokio::spawn` and responds from there.

use std::collections::VecDeque;
use std::future::Future;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use acp::schema::ProtocolVersion;
use acp::{Agent, Client, ConnectTo, ConnectionTo, Lines};
use agent_client_protocol as acp;
use agent_client_protocol::schema::v1::{
    AuthMethod, AuthenticateRequest, CancelNotification, ClientCapabilities, CloseSessionRequest,
    CreateTerminalRequest, FileSystemCapabilities, InitializeRequest, KillTerminalRequest,
    LoadSessionRequest, McpServer, NewSessionRequest, PromptRequest, ReadTextFileRequest,
    ReadTextFileResponse, ReleaseTerminalRequest, RequestPermissionOutcome,
    RequestPermissionRequest, RequestPermissionResponse, SelectedPermissionOutcome,
    SessionConfigId, SessionConfigOption, SessionConfigOptionValue, SessionId, SessionModeId,
    SessionModeState, SessionNotification, SetSessionConfigOptionRequest, SetSessionModeRequest,
    StopReason, TerminalOutputRequest, WaitForTerminalExitRequest, WriteTextFileRequest,
    WriteTextFileResponse,
};
use futures::channel::oneshot;
use futures::io::BufReader;
use futures::{AsyncBufReadExt as _, AsyncWriteExt as _};

use crate::agent::{AgentServer, SpawnSpec};
use crate::fs::{SessionRoots, read_text_file, write_text_file};
use crate::permissions::{
    PermissionDecision, PermissionReply, ToolPermissions, Verdict, haystack, strongest_allow,
    strongest_reject, tool_key,
};
use crate::terminal::{TerminalBackend, TerminalRegistry};
use crate::{AcpError, AcpEvent, SessionSpec, schema};

/// How many stderr lines are kept for the error state.
const STDERR_LINES: usize = 250;

/// What the client learned about the agent during `initialize`.
#[derive(Clone, Debug, Default)]
pub struct AgentInfo {
    pub name: String,
    pub version: String,
    pub load_session: bool,
    /// The agent accepts HTTP MCP servers in `session/new`. Only then may the
    /// client attach its loopback endpoint (`McpServer::Http`).
    pub mcp_http: bool,
    /// The agent accepts SSE MCP servers in `session/new`.
    pub mcp_sse: bool,
    pub auth_methods: Vec<AuthMethod>,
}

impl AgentInfo {
    /// Auth methods the agent advertises, for the auth-required state.
    pub fn auth_method_count(&self) -> usize {
        self.auth_methods.len()
    }
}

/// Sends requests to the agent. Cloneable and `'static`, so it can be moved
/// into a background task while the UI keeps its own handle.
#[derive(Clone)]
pub struct Requester {
    connection: ConnectionTo<Agent>,
}

impl Requester {
    /// Create a session rooted at `cwd` with extra workspace roots and any MCP
    /// servers to attach (the agent must advertise the matching transport).
    pub async fn new_session(
        &self,
        cwd: PathBuf,
        additional: Vec<PathBuf>,
        mcp_servers: Vec<McpServer>,
    ) -> Result<
        (
            SessionId,
            Option<SessionModeState>,
            Vec<SessionConfigOption>,
        ),
        AcpError,
    > {
        let request = NewSessionRequest::new(cwd)
            .additional_directories(additional)
            .mcp_servers(mcp_servers);
        let response = self
            .connection
            .send_request(request)
            .block_task()
            .await
            .map_err(AcpError::from)?;
        Ok((
            response.session_id,
            response.modes,
            response.config_options.unwrap_or_default(),
        ))
    }

    /// Resume an existing session.
    pub async fn load_session(
        &self,
        session_id: SessionId,
        cwd: PathBuf,
        additional: Vec<PathBuf>,
        mcp_servers: Vec<McpServer>,
    ) -> Result<(Option<SessionModeState>, Vec<SessionConfigOption>), AcpError> {
        let request = LoadSessionRequest::new(session_id, cwd)
            .additional_directories(additional)
            .mcp_servers(mcp_servers);
        let response = self
            .connection
            .send_request(request)
            .block_task()
            .await
            .map_err(AcpError::from)?;
        Ok((response.modes, response.config_options.unwrap_or_default()))
    }

    /// Send a prompt and wait for the turn to end.
    pub async fn prompt(
        &self,
        session_id: &SessionId,
        text: String,
    ) -> Result<StopReason, AcpError> {
        let request = PromptRequest::new(
            session_id.clone(),
            vec![schema::ContentBlock::Text(schema::TextContent::new(text))],
        );
        let response = self
            .connection
            .send_request(request)
            .block_task()
            .await
            .map_err(AcpError::from)?;
        Ok(response.stop_reason)
    }

    /// Ask the agent to stop the current turn. Fire and forget, per protocol.
    pub fn cancel(&self, session_id: &SessionId) -> Result<(), AcpError> {
        self.connection
            .send_notification(CancelNotification::new(session_id.clone()))
            .map_err(AcpError::from)
    }

    pub async fn set_mode(
        &self,
        session_id: &SessionId,
        mode_id: SessionModeId,
    ) -> Result<(), AcpError> {
        self.connection
            .send_request(SetSessionModeRequest::new(session_id.clone(), mode_id))
            .block_task()
            .await
            .map(|_| ())
            .map_err(AcpError::from)
    }

    /// Set a session config option; select-valued options take a value id.
    ///
    /// The response carries the full current option set. An agent is not
    /// required to also announce the change, so the returned options are the
    /// only report of it a caller can rely on.
    pub async fn set_config_option(
        &self,
        session_id: &SessionId,
        config_id: SessionConfigId,
        value: SessionConfigOptionValue,
    ) -> Result<Vec<SessionConfigOption>, AcpError> {
        self.connection
            .send_request(SetSessionConfigOptionRequest::new(
                session_id.clone(),
                config_id,
                value,
            ))
            .block_task()
            .await
            .map(|response| response.config_options)
            .map_err(AcpError::from)
    }

    pub async fn authenticate(
        &self,
        method_id: impl Into<schema::AuthMethodId>,
    ) -> Result<(), AcpError> {
        self.connection
            .send_request(AuthenticateRequest::new(method_id))
            .block_task()
            .await
            .map(|_| ())
            .map_err(AcpError::from)
    }

    pub async fn close_session(&self, session_id: &SessionId) -> Result<(), AcpError> {
        self.connection
            .send_request(CloseSessionRequest::new(session_id.clone()))
            .block_task()
            .await
            .map(|_| ())
            .map_err(AcpError::from)
    }
}

/// A live connection to an agent process.
pub struct AcpConnection {
    pub info: AgentInfo,
    pub requester: Requester,
    /// Ask the agent to approve a tool call.
    pub events: tokio::sync::mpsc::UnboundedReceiver<AcpEvent>,
    /// The project's approval policy, consulted for every permission request.
    tool_permissions: Arc<RwLock<ToolPermissions>>,
    /// Recent agent stderr, newest last.
    stderr: Arc<Mutex<VecDeque<String>>>,
    terminals: TerminalRegistry,
    /// Dropping this ends the connection and terminates the process group.
    shutdown: Option<oneshot::Sender<()>>,
}

impl AcpConnection {
    /// Replace the approval policy consulted for permission requests.
    pub fn set_tool_permissions(&self, permissions: ToolPermissions) {
        if let Ok(mut policy) = self.tool_permissions.write() {
            *policy = permissions;
        }
    }

    /// Move the event stream out, leaving the connection usable for requests.
    ///
    /// The pane needs to own the stream in a long-lived task while keeping the
    /// connection around to send prompts.
    pub fn take_events(&mut self) -> tokio::sync::mpsc::UnboundedReceiver<AcpEvent> {
        let (_, receiver) = tokio::sync::mpsc::unbounded_channel();
        std::mem::replace(&mut self.events, receiver)
    }

    /// Recent stderr lines, oldest first.
    pub fn recent_stderr(&self) -> Vec<String> {
        self.stderr
            .lock()
            .map(|lines| lines.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// End the connection now instead of waiting for the drop.
    pub fn close(mut self) {
        self.shutdown_now();
    }

    pub fn close_terminals(&self) -> impl Future<Output = ()> + Send + 'static {
        let terminals = self.terminals.clone();
        async move { terminals.shutdown().await }
    }

    fn shutdown_now(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            if shutdown.send(()).is_err() {
                tracing::debug!("agent connection already finished");
            }
        }
    }
}

impl Drop for AcpConnection {
    fn drop(&mut self) {
        self.shutdown_now();
    }
}

/// Options for a connection that outlive a single session.
#[derive(Clone)]
pub struct ConnectOptions {
    pub roots: SessionRoots,
    pub tool_permissions: ToolPermissions,
}

/// Connect to an agent process launched in the project directory.
pub async fn connect(
    agent: Arc<dyn AgentServer>,
    spec: &SessionSpec,
    options_for: impl FnOnce(&PathBuf) -> ConnectOptions,
) -> Result<AcpConnection, AcpError> {
    let (cwd, _additional) = spec.resolve().ok_or(AcpError::NoProjectDirectory)?;
    let spawn = agent.spawn_spec(&cwd)?;
    connect_spawn(spawn, options_for(&cwd)).await
}

/// Connect with an already resolved executable and arguments.
pub async fn connect_spawn(
    spawn: SpawnSpec,
    options: ConnectOptions,
) -> Result<AcpConnection, AcpError> {
    connect_over(
        move |events, stderr| ProjectAgent::new(spawn).with_stderr_reporting(events, stderr),
        options,
    )
    .await
}

pub async fn connect_spawn_with_terminals(
    spawn: SpawnSpec,
    options: ConnectOptions,
    terminals: Arc<dyn TerminalBackend>,
) -> Result<AcpConnection, AcpError> {
    connect_over_with_terminals(
        move |events, stderr| ProjectAgent::new(spawn).with_stderr_reporting(events, stderr),
        options,
        Some(terminals),
    )
    .await
}

/// Connect over any transport. Used by [`connect`] with a real process, and by
/// tests with an in-process channel.
pub(crate) async fn connect_over<T>(
    build_transport: impl FnOnce(
        tokio::sync::mpsc::UnboundedSender<AcpEvent>,
        Arc<Mutex<VecDeque<String>>>,
    ) -> T,
    options: ConnectOptions,
) -> Result<AcpConnection, AcpError>
where
    T: ConnectTo<Client> + 'static,
{
    connect_over_with_terminals(build_transport, options, None).await
}

pub(crate) async fn connect_over_with_terminals<T>(
    build_transport: impl FnOnce(
        tokio::sync::mpsc::UnboundedSender<AcpEvent>,
        Arc<Mutex<VecDeque<String>>>,
    ) -> T,
    options: ConnectOptions,
    terminals: Option<Arc<dyn TerminalBackend>>,
) -> Result<AcpConnection, AcpError>
where
    T: ConnectTo<Client> + 'static,
{
    let (events_tx, events_rx) = tokio::sync::mpsc::unbounded_channel();
    let stderr_buffer = Arc::new(Mutex::new(VecDeque::new()));
    let transport = build_transport(events_tx.clone(), stderr_buffer.clone());
    let (ready_tx, ready_rx) = oneshot::channel();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let tool_permissions = Arc::new(RwLock::new(options.tool_permissions));
    let policy_for_handlers = tool_permissions.clone();

    let roots = options.roots.clone();
    let roots_for_handlers = roots.clone();
    let events_for_notifications = events_tx.clone();
    let permissions = events_tx.clone();
    let reads = events_tx.clone();
    let writes = events_tx.clone();
    let terminal_create = events_tx.clone();
    let auto_approvals = events_tx.clone();
    let registry = TerminalRegistry::new(roots_for_handlers.clone(), &events_tx, terminals);
    // One clone per handler: the `async move` closures own their captures, so a
    // single shared variable cannot be moved into several of them.
    let roots_for_read = roots.clone();
    let roots_for_write = roots.clone();
    let registry_for_create = registry.clone();
    let registry_for_output = registry.clone();
    let registry_for_wait = registry.clone();
    let registry_for_kill = registry.clone();
    let registry_for_release = registry.clone();

    let capabilities = ClientCapabilities::new()
        .terminal(true)
        .fs(FileSystemCapabilities::new()
            .read_text_file(true)
            .write_text_file(true));

    let connection = Client
        .builder()
        .name("taskstream-desktop")
        // Notifications: hand session updates straight to the UI, which reduces
        // them through the transcript model.
        .on_receive_notification(
            async move |notification: SessionNotification, _cx| {
                drop(
                    events_for_notifications.send(AcpEvent::SessionUpdate(Box::new(notification))),
                );
                Ok(())
            },
            acp::on_receive_notification!(),
        )
        // Permissions: answer from the project's policy when it allows or
        // denies, otherwise wait for the user without blocking the event loop.
        .on_receive_request(
            async move |request: RequestPermissionRequest, responder, _cx| {
                let tool = tool_key(&request.tool_call);
                let text = haystack(&request.tool_call);
                let verdict = policy_for_handlers
                    .read()
                    .map(|policy| policy.decide(&tool, &text))
                    .unwrap_or(Verdict::Confirm);
                let automatic = match verdict {
                    Verdict::Allow => strongest_allow(&request.options),
                    Verdict::Deny => strongest_reject(&request.options),
                    Verdict::Confirm => None,
                };
                if let Some(option) = automatic {
                    responder.respond(permission_response(PermissionDecision::Selected(
                        option.option_id.clone(),
                    )))?;
                    // Report the automatic answer so the transcript records it.
                    drop(auto_approvals.send(AcpEvent::PermissionAutoDecided {
                        tool_call: request.tool_call.clone(),
                        choice: crate::thread::PermissionChoice::from_option(option),
                    }));
                    return Ok(());
                }
                let permissions = permissions.clone();
                tokio::spawn(async move {
                    let (decision_tx, decision_rx) = oneshot::channel();
                    let sent = permissions.send(AcpEvent::PermissionRequested {
                        session_id: request.session_id.clone(),
                        tool_call: request.tool_call.clone(),
                        options: request.options.clone(),
                        decision: PermissionReply::new(decision_tx),
                    });
                    if sent.is_err() {
                        responder.respond(permission_response(PermissionDecision::Cancelled))?;
                        return Ok::<(), acp::Error>(());
                    }
                    let decision = decision_rx.await.unwrap_or(PermissionDecision::Cancelled);
                    responder.respond(permission_response(decision))?;
                    Ok(())
                });
                Ok(())
            },
            acp::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: ReadTextFileRequest, responder, _cx| {
                let roots = roots_for_read.clone();
                let events = reads.clone();
                tokio::spawn(async move {
                    let path = match roots.resolve(&request.path) {
                        Ok(path) => path,
                        Err(error) => {
                            respond_with_error(responder, error, &events, "read")?;
                            return Ok::<(), acp::Error>(());
                        }
                    };
                    match read_text_file(&path, request.line, request.limit) {
                        Ok(content) => responder.respond(ReadTextFileResponse::new(content))?,
                        Err(error) => respond_with_error(responder, error, &events, "read")?,
                    }
                    Ok(())
                });
                Ok(())
            },
            acp::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: WriteTextFileRequest, responder, _cx| {
                let roots = roots_for_write.clone();
                let events = writes.clone();
                tokio::spawn(async move {
                    let path = match roots.resolve(&request.path) {
                        Ok(path) => path,
                        Err(error) => {
                            respond_with_error(responder, error, &events, "write")?;
                            return Ok::<(), acp::Error>(());
                        }
                    };
                    match write_text_file(&path, &request.content) {
                        Ok(()) => responder.respond(WriteTextFileResponse::new())?,
                        Err(error) => respond_with_error(responder, error, &events, "write")?,
                    }
                    Ok(())
                });
                Ok(())
            },
            acp::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: CreateTerminalRequest, responder, _cx| {
                let registry = registry_for_create.clone();
                let events = terminal_create.clone();
                tokio::spawn(async move {
                    match registry.create(request).await {
                        Ok(response) => responder.respond(response)?,
                        Err(error) => {
                            let message = format!("terminal failed to start: {error}");
                            drop(events.send(terminal_notice(message.clone())));
                            responder.respond_with_internal_error(message)?;
                        }
                    }
                    Ok::<(), acp::Error>(())
                });
                Ok(())
            },
            acp::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: TerminalOutputRequest, responder, _cx| {
                let registry = registry_for_output.clone();
                tokio::spawn(async move {
                    let response = registry.output(&request.terminal_id);
                    responder.respond(response)?;
                    Ok::<(), acp::Error>(())
                });
                Ok(())
            },
            acp::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: WaitForTerminalExitRequest, responder, _cx| {
                let registry = registry_for_wait.clone();
                tokio::spawn(async move {
                    match registry.wait_for_exit(&request.terminal_id).await {
                        Ok(response) => responder.respond(response)?,
                        Err(error) => responder.respond_with_internal_error(error)?,
                    }
                    Ok::<(), acp::Error>(())
                });
                Ok(())
            },
            acp::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: KillTerminalRequest, responder, _cx| {
                let registry = registry_for_kill.clone();
                tokio::spawn(async move {
                    match registry.kill(&request.terminal_id).await {
                        Ok(response) => responder.respond(response)?,
                        Err(error) => responder.respond_with_internal_error(error)?,
                    }
                    Ok::<(), acp::Error>(())
                });
                Ok(())
            },
            acp::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: ReleaseTerminalRequest, responder, _cx| {
                let registry = registry_for_release.clone();
                tokio::spawn(async move {
                    match registry.release(&request.terminal_id).await {
                        Ok(response) => responder.respond(response)?,
                        Err(error) => responder.respond_with_internal_error(error)?,
                    }
                    Ok::<(), acp::Error>(())
                });
                Ok(())
            },
            acp::on_receive_request!(),
        )
        .connect_with(transport, async move |cx: ConnectionTo<Agent>| {
            let mut shutdown_rx = shutdown_rx;
            let initialize = cx
                .send_request(
                    InitializeRequest::new(ProtocolVersion::V1).client_capabilities(capabilities),
                )
                .block_task();
            let initialize = tokio::select! {
                initialize = initialize => initialize?,
                _ = &mut shutdown_rx => return Ok(()),
            };
            if initialize.protocol_version != ProtocolVersion::V1 {
                if ready_tx
                    .send(Err(AcpError::UnsupportedVersion {
                        agent: initialize.protocol_version.to_string(),
                    }))
                    .is_err()
                {
                    tracing::debug!("ACP caller dropped during protocol negotiation");
                }
                return Ok(());
            }
            let info = AgentInfo {
                name: initialize
                    .agent_info
                    .as_ref()
                    .map(|info| info.name.clone())
                    .unwrap_or_default(),
                version: initialize
                    .agent_info
                    .as_ref()
                    .map(|info| info.version.clone())
                    .unwrap_or_default(),
                load_session: initialize.agent_capabilities.load_session,
                mcp_http: initialize.agent_capabilities.mcp_capabilities.http,
                mcp_sse: initialize.agent_capabilities.mcp_capabilities.sse,
                auth_methods: initialize.auth_methods.clone(),
            };
            if ready_tx.send(Ok((cx.clone(), info))).is_err() {
                return Ok(());
            }
            // Stay connected until the handle is dropped.
            if shutdown_rx.await.is_err() {
                tracing::debug!("agent connection closed without a shutdown signal");
            }
            Ok(())
        });

    // The connection is driven concurrently with whatever the UI is doing; the
    // shutdown channel above is what ends it.
    let terminals = registry.clone();
    tokio::spawn(async move {
        if let Err(error) = connection.await {
            tracing::warn!("agent connection ended: {error}");
        }
        registry.shutdown().await;
    });

    let (requester, info) = match ready_rx.await {
        Ok(result) => {
            let (connection, info) = result?;
            (Requester { connection }, info)
        }
        Err(_) => {
            // The ready channel and the stderr drain task race on the process's
            // final output; give the drain a moment to flush before capturing
            // the diagnostic context.
            tokio::time::sleep(Duration::from_millis(100)).await;
            let stderr = match stderr_buffer.lock() {
                Ok(mut buffer) => buffer.drain(..).collect::<Vec<_>>(),
                Err(poisoned) => poisoned.into_inner().drain(..).collect::<Vec<_>>(),
            };
            let summary = stderr_summary(&stderr);
            let detail = if summary.trim().is_empty() {
                "the agent exited before completing the handshake".to_string()
            } else {
                format!(
                    "the agent exited before completing the handshake; captured stderr:\n{summary}"
                )
            };
            return Err(AcpError::Launch(detail));
        }
    };

    Ok(AcpConnection {
        info,
        requester,
        events: events_rx,
        tool_permissions,
        stderr: stderr_buffer,
        terminals,
        shutdown: Some(shutdown_tx),
    })
}

fn permission_response(decision: PermissionDecision) -> RequestPermissionResponse {
    match decision {
        PermissionDecision::Selected(option_id) => RequestPermissionResponse::new(
            RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(option_id)),
        ),
        PermissionDecision::Cancelled => {
            RequestPermissionResponse::new(RequestPermissionOutcome::Cancelled)
        }
    }
}

fn terminal_notice(message: String) -> AcpEvent {
    AcpEvent::Stderr(message)
}

fn respond_with_error<T>(
    responder: acp::Responder<T>,
    error: anyhow::Error,
    events: &tokio::sync::mpsc::UnboundedSender<AcpEvent>,
    operation: &str,
) -> Result<(), acp::Error>
where
    T: acp::JsonRpcResponse,
{
    let message = format!("{operation} failed: {error}");
    drop(events.send(AcpEvent::Stderr(message.clone())));
    responder.respond_with_internal_error(message)
}

/// Launches an agent process in a project directory.
///
/// Mirrors the SDK's own agent component, minus the launch configuration it
/// cannot express: the child's working directory.
pub struct ProjectAgent {
    spec: SpawnSpec,
    events: Option<tokio::sync::mpsc::UnboundedSender<AcpEvent>>,
    stderr: Option<Arc<Mutex<VecDeque<String>>>>,
}

impl ProjectAgent {
    pub fn new(spec: SpawnSpec) -> Self {
        Self {
            spec,
            events: None,
            stderr: None,
        }
    }

    /// Report the child's stderr lines to the UI as they arrive.
    pub fn with_stderr_reporting(
        mut self,
        events: tokio::sync::mpsc::UnboundedSender<AcpEvent>,
        buffer: Arc<Mutex<VecDeque<String>>>,
    ) -> Self {
        self.events = Some(events);
        self.stderr = Some(buffer);
        self
    }

    fn command(&self) -> std::process::Command {
        let mut command = std::process::Command::new(&self.spec.program);
        command
            .args(&self.spec.args)
            .current_dir(&self.spec.cwd)
            .envs(&self.spec.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            // Own process group, so a wrapper launcher's children die with it.
            command.process_group(0);
        }
        command
    }

    fn spawn_child(&self) -> std::io::Result<async_process::Child> {
        // NB: `async_process::Command::from` resets its internal
        // stdin/stdout/stderr-configured flags, and `spawn()` overwrites any
        // unflagged stream with `inherit()`. Re-assert piped stdio through
        // the async API so the flags (and the pipes) survive.
        let mut command = async_process::Command::from(self.command());
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command.spawn()
    }
}

impl ConnectTo<Client> for ProjectAgent {
    async fn connect_to(self, client: impl ConnectTo<Agent>) -> Result<(), acp::Error> {
        let mut child = self.spawn_child().map_err(|error| {
            acp::Error::internal_error().data(format!(
                "failed to launch {}: {error}",
                self.spec.command_line()
            ))
        })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| acp::Error::internal_error().data("agent stdin unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| acp::Error::internal_error().data("agent stdout unavailable"))?;
        let stderr = child.stderr.take();

        let stderr_task = if let (Some(stderr), Some(events), Some(buffer)) =
            (stderr, self.events.clone(), self.stderr.clone())
        {
            // stderr has no protocol framing, so it is drained on its own task:
            // a full pipe from a chatty agent would otherwise stall the child.
            Some(tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Some(Ok(line)) = futures::StreamExt::next(&mut lines).await {
                    if let Ok(mut buffer) = buffer.lock() {
                        buffer.push_back(line.clone());
                        while buffer.len() > STDERR_LINES {
                            buffer.pop_front();
                        }
                    }
                    if events.send(AcpEvent::Stderr(line)).is_err() {
                        break;
                    }
                }
            }))
        } else {
            None
        };

        let incoming = BufReader::new(stdout).lines();
        let outgoing = futures::sink::unfold(stdin, |mut writer, line: String| async move {
            writer.write_all(line.as_bytes()).await?;
            writer.write_all(b"\n").await?;
            Ok::<_, std::io::Error>(writer)
        });

        // The SDK may drop the wire future on shutdown. Reaping and reporting
        // the subprocess must outlive that future.
        let (wire_finished, finished) = oneshot::channel();
        let supervisor = tokio::spawn(supervise_agent(
            child,
            finished,
            stderr_task,
            self.events,
            self.stderr,
        ));
        let result = ConnectTo::<Client>::connect_to(Lines::new(outgoing, incoming), client).await;
        if wire_finished.send(()).is_err() {
            tracing::debug!("ACP agent exited before the wire closed");
        }
        if let Err(error) = supervisor.await {
            tracing::debug!(%error, "ACP process supervision ended");
        }
        result
    }
}

async fn supervise_agent(
    mut child: async_process::Child,
    mut wire_finished: oneshot::Receiver<()>,
    mut stderr_task: Option<tokio::task::JoinHandle<()>>,
    events: Option<tokio::sync::mpsc::UnboundedSender<AcpEvent>>,
    stderr: Option<Arc<Mutex<VecDeque<String>>>>,
) {
    let status = tokio::select! {
        status = child.status() => {
            // Give the wire time to deliver any protocol records already in the pipe.
            if tokio::time::timeout(Duration::from_millis(250), &mut wire_finished).await.is_err() {
                tracing::debug!("ACP wire remained open after process exit");
            }
            status
        }
        _ = &mut wire_finished => {
            // EOF can precede a natural exit. Preserve its status when the
            // agent leaves on its own and terminate it if it outlives the wire.
            match tokio::time::timeout(Duration::from_millis(250), child.status()).await {
                Ok(status) => status,
                Err(_) => match kill_agent_process(&mut child) {
                    Ok(()) => child.status().await,
                    Err(error) => Err(error),
                },
            }
        }
    };
    let code = match status {
        Ok(status) => status.code(),
        Err(error) => {
            tracing::debug!(%error, "could not reap the ACP agent");
            None
        }
    };
    if let Some(stderr_task) = &mut stderr_task
        && tokio::time::timeout(Duration::from_millis(100), &mut *stderr_task)
            .await
            .is_err()
    {
        stderr_task.abort();
    }
    if let Some(events) = events {
        let stderr = stderr
            .as_ref()
            .and_then(|buffer| buffer.lock().ok())
            .map(|lines| lines.iter().cloned().collect::<Vec<_>>().join("\n"))
            .unwrap_or_default();
        if let Err(error) = events.send(AcpEvent::Exited { code, stderr }) {
            tracing::debug!(%error, "ACP exit receiver closed");
        }
    }
}

fn kill_agent_process(child: &mut async_process::Child) -> std::io::Result<()> {
    if child.try_status()?.is_some() {
        return Ok(());
    }
    #[cfg(unix)]
    {
        let group = libc::pid_t::try_from(child.id()).map_err(std::io::Error::other)?;
        // `command()` puts the still-owned child in its own process group.
        if unsafe { libc::killpg(group, libc::SIGKILL) } == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            tracing::debug!(%error, "could not stop the ACP process group");
        }
    }
    child.kill()
}

/// The trailing stderr lines worth showing in an error state.
pub fn stderr_summary(lines: &[String]) -> String {
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "requires the configured OpenCode executable and loopback permission"]
    async fn installed_opencode_initializes_and_loads_an_acp_session()
    -> Result<(), Box<dyn std::error::Error>> {
        let program = PathBuf::from(
            std::env::var_os("SHEPHERD_OPENCODE_ACP_TEST_EXECUTABLE")
                .ok_or("set SHEPHERD_OPENCODE_ACP_TEST_EXECUTABLE to an absolute path")?,
        );
        if !program.is_absolute() {
            return Err("the OpenCode executable path must be absolute".into());
        }
        let root = tempfile::tempdir()?;
        let cwd = root.path().join("project");
        std::fs::create_dir(&cwd)?;
        let cwd = cwd.canonicalize()?;
        let mut env = [
            ("OPENCODE_DISABLE_AUTOUPDATE".into(), "1".into()),
            ("OPENCODE_DISABLE_MODELS_FETCH".into(), "1".into()),
            ("HOME".into(), root.path().to_string_lossy().into_owned()),
        ]
        .into_iter()
        .collect::<std::collections::BTreeMap<_, _>>();
        for (variable, directory) in [
            ("XDG_CONFIG_HOME", "config"),
            ("XDG_DATA_HOME", "data"),
            ("XDG_CACHE_HOME", "cache"),
            ("XDG_STATE_HOME", "state"),
        ] {
            env.insert(
                variable.into(),
                root.path().join(directory).to_string_lossy().into_owned(),
            );
        }
        let launch = SpawnSpec {
            program,
            args: vec!["acp".into()],
            cwd: cwd.clone(),
            env,
        };
        let options = ConnectOptions {
            roots: SessionRoots::new([cwd.clone()]),
            tool_permissions: ToolPermissions::default(),
        };
        let mut first = tokio::time::timeout(
            Duration::from_secs(10),
            connect_spawn(launch.clone(), options.clone()),
        )
        .await??;
        assert!(first.info.load_session);
        let (session, _, _) = first
            .requester
            .new_session(cwd.clone(), Vec::new(), Vec::new())
            .await?;
        let mut events = first.take_events();
        first.close();
        tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(event) = events.recv().await {
                if matches!(event, AcpEvent::Exited { .. }) {
                    return Ok(());
                }
            }
            Err("the ACP process did not report its exit")
        })
        .await??;
        let second =
            tokio::time::timeout(Duration::from_secs(10), connect_spawn(launch, options)).await??;
        tokio::time::timeout(
            Duration::from_secs(10),
            second
                .requester
                .load_session(session, cwd, Vec::new(), Vec::new()),
        )
        .await??;
        second.close();
        Ok(())
    }

    /// Regression test: converting the std command via
    /// `async_process::Command::from` used to silently replace the piped
    /// stdio with inherited stdio, so `connect_to` failed with "agent stdin
    /// unavailable" and the UI reported a handshake failure.
    #[test]
    fn spawn_child_keeps_stdio_piped() {
        let agent = ProjectAgent::new(SpawnSpec {
            program: "cat".into(),
            args: Vec::new(),
            cwd: std::env::temp_dir(),
            env: Default::default(),
        });
        let mut child = agent.spawn_child().expect("spawn cat");
        assert!(child.stdin.is_some(), "stdin must stay piped");
        assert!(child.stdout.is_some(), "stdout must stay piped");
        assert!(child.stderr.is_some(), "stderr must stay piped");
        let _ = child.kill();
    }
}
