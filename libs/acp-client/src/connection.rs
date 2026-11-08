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
use futures::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _};

use crate::agent::{AgentServer, SpawnSpec};
use crate::fs::{PromptReads, SessionRoots};
use crate::permissions::{
    PermissionDecision, PermissionReply, ToolPermissions, Verdict, haystack, strongest_allow,
    strongest_reject, tool_key,
};
use crate::terminal::{TerminalBackend, TerminalRegistry};
use crate::{AcpError, AcpEvent, SessionSpec, StderrMessage, schema};

/// How many stderr lines are kept for the error state.
const STDERR_LINES: usize = 250;
const PENDING_STDERR_EVENTS: usize = 64;
const MAX_PROTOCOL_LINE_BYTES: usize = 8 * 1024 * 1024;

fn record_agent_stderr(
    text: String,
    buffer: &Mutex<VecDeque<String>>,
    events: &tokio::sync::mpsc::UnboundedSender<AcpEvent>,
    permits: &Arc<tokio::sync::Semaphore>,
) {
    match buffer.lock() {
        Ok(mut buffer) => {
            buffer.push_back(text.clone());
            while buffer.len() > STDERR_LINES {
                buffer.pop_front();
            }
        }
        Err(error) => tracing::error!(%error, "could not retain ACP stderr"),
    }
    if let Ok(permit) = permits.clone().try_acquire_owned()
        && events
            .send(AcpEvent::Stderr(StderrMessage::limited(text, permit)))
            .is_err()
    {
        tracing::debug!("ACP stderr receiver closed");
    }
}

async fn read_protocol_line<R: futures::io::AsyncBufRead + Unpin>(
    reader: &mut R,
    limit: usize,
) -> std::io::Result<Option<String>> {
    let mut bytes = Vec::new();
    let read_limit = u64::try_from(limit.saturating_add(2)).unwrap_or(u64::MAX);
    let count = reader
        .take(read_limit)
        .read_until(b'\n', &mut bytes)
        .await?;
    if count == 0 {
        return Ok(None);
    }
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
    if bytes.len() > limit {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("ACP protocol message exceeds {limit} bytes"),
        ));
    }
    String::from_utf8(bytes).map(Some).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("ACP protocol message is not UTF-8: {error}"),
        )
    })
}

/// What the client learned about the agent during `initialize`.
#[derive(Clone, Debug, Default)]
pub struct AgentInfo {
    pub name: String,
    pub version: String,
    pub load_session: bool,
    pub prompt_images: bool,
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
    auth_methods: Arc<[AuthMethod]>,
    prompt_reads: PromptReads,
}

impl Requester {
    fn request_error(&self, error: acp::Error) -> AcpError {
        match AcpError::from(error) {
            AcpError::AuthRequired(message) if !self.auth_methods.is_empty() => {
                let methods = self
                    .auth_methods
                    .iter()
                    .map(AuthMethod::name)
                    .collect::<Vec<_>>()
                    .join(", ");
                AcpError::AuthRequired(format!("{message}. Agent sign-in methods: {methods}"))
            }
            error => error,
        }
    }

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
            .map_err(|error| self.request_error(error))?;
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
            .map_err(|error| self.request_error(error))?;
        Ok((response.modes, response.config_options.unwrap_or_default()))
    }

    /// Send a prompt and wait for the turn to end.
    pub async fn prompt(
        &self,
        session_id: &SessionId,
        text: String,
    ) -> Result<StopReason, AcpError> {
        self.start_prompt_content(
            session_id,
            vec![schema::ContentBlock::Text(schema::TextContent::new(text))],
            Vec::new(),
        )
        .await?
        .await
    }

    /// Open the selected files and enqueue the prompt before returning its result future.
    pub async fn start_prompt_content(
        &self,
        session_id: &SessionId,
        content: Vec<schema::ContentBlock>,
        readable_files: Vec<PathBuf>,
    ) -> Result<impl Future<Output = Result<StopReason, AcpError>> + Send + use<>, AcpError> {
        let grant = if readable_files.is_empty() {
            None
        } else {
            let session = session_id.clone();
            let reads = self.prompt_reads.clone();
            Some(
                tokio::task::spawn_blocking(move || reads.grant(session, readable_files))
                    .await
                    .map_err(|error| {
                        AcpError::Protocol(format!("attachment access worker failed: {error}"))
                    })?
                    .map_err(|error| {
                        AcpError::Protocol(format!("could not open prompt attachments: {error}"))
                    })?,
            )
        };
        let request = PromptRequest::new(session_id.clone(), content);
        let pending = self.connection.send_request(request);
        let requester = self.clone();
        Ok(async move {
            let _grant = grant;
            let response = pending
                .block_task()
                .await
                .map_err(|error| requester.request_error(error))?;
            Ok(response.stop_reason)
        })
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
    supervision: Option<ProcessSupervision>,
    supervision_failure: Option<String>,
}

struct ProcessSupervision {
    results: tokio::sync::mpsc::UnboundedReceiver<Result<(), String>>,
    retry: tokio::sync::mpsc::UnboundedSender<()>,
    retry_needed: bool,
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

    pub async fn close_and_wait(&mut self) -> Result<(), AcpError> {
        self.close_and_wait_with_timeout(Duration::from_secs(5))
            .await
    }

    async fn close_and_wait_with_timeout(&mut self, deadline: Duration) -> Result<(), AcpError> {
        self.shutdown_now();
        if let Some(error) = &self.supervision_failure {
            return Err(AcpError::Protocol(error.clone()));
        }
        if let Some(supervision) = &mut self.supervision {
            if supervision.retry_needed {
                supervision.retry.send(()).map_err(|_| {
                    AcpError::Protocol("ACP process supervisor is unavailable".into())
                })?;
                supervision.retry_needed = false;
            }
            let result = tokio::time::timeout(deadline, supervision.results.recv())
                .await
                .map_err(|_| {
                    AcpError::Protocol(
                        "ACP process did not stop before the shutdown deadline".into(),
                    )
                })?;
            match result {
                Some(Ok(())) => self.supervision = None,
                Some(Err(error)) => {
                    supervision.retry_needed = true;
                    return Err(AcpError::Protocol(error));
                }
                None => {
                    let error = "ACP process supervision ended without a result".to_owned();
                    self.supervision_failure = Some(error.clone());
                    return Err(AcpError::Protocol(error));
                }
            }
        }
        Ok(())
    }

    pub fn close_terminals(&self) -> impl Future<Output = ()> + Send + 'static {
        let terminals = self.terminals.clone();
        async move { terminals.shutdown().await }
    }

    fn shutdown_now(&mut self) {
        if let Some(shutdown) = self.shutdown.take()
            && shutdown.send(()).is_err()
        {
            tracing::debug!("agent connection already finished");
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

#[allow(
    clippy::double_must_use,
    reason = "async_trait generates must-use futures"
)]
#[async_trait::async_trait]
pub trait ProcessTracker: Send + Sync {
    async fn started(&self, pid: u32) -> Result<String, String>;
    async fn stopped(&self, record_id: &str) -> Result<(), String>;
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
    connect_spawn_inner(spawn, options, None, None).await
}

pub async fn connect_spawn_with_terminals(
    spawn: SpawnSpec,
    options: ConnectOptions,
    terminals: Arc<dyn TerminalBackend>,
) -> Result<AcpConnection, AcpError> {
    connect_spawn_inner(spawn, options, Some(terminals), None).await
}

pub async fn connect_spawn_tracked(
    spawn: SpawnSpec,
    options: ConnectOptions,
    terminals: Option<Arc<dyn TerminalBackend>>,
    tracker: Arc<dyn ProcessTracker>,
) -> Result<AcpConnection, AcpError> {
    connect_spawn_inner(spawn, options, terminals, Some(tracker)).await
}

async fn connect_spawn_inner(
    spawn: SpawnSpec,
    options: ConnectOptions,
    terminals: Option<Arc<dyn TerminalBackend>>,
    tracker: Option<Arc<dyn ProcessTracker>>,
) -> Result<AcpConnection, AcpError> {
    let (completion, results) = tokio::sync::mpsc::unbounded_channel();
    let (retry, retry_requests) = tokio::sync::mpsc::unbounded_channel();
    let mut connection = connect_over_with_terminals(
        move |events, stderr| {
            ProjectAgent::new(spawn)
                .with_stderr_reporting(events, stderr)
                .with_supervision(completion, retry_requests)
                .with_process_tracker(tracker)
        },
        options,
        terminals,
    )
    .await?;
    connection.supervision = Some(ProcessSupervision {
        results,
        retry,
        retry_needed: false,
    });
    Ok(connection)
}

/// Connect an in-process test transport.
#[cfg(any(test, feature = "test-support"))]
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
    let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
    let stderr_buffer = Arc::new(Mutex::new(VecDeque::new()));
    let transport = build_transport(events_tx.clone(), stderr_buffer.clone());
    let (ready_tx, ready_rx) = oneshot::channel();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let tool_permissions = Arc::new(RwLock::new(options.tool_permissions));
    let policy_for_handlers = tool_permissions.clone();

    let roots = options.roots.clone();
    let prompt_reads = PromptReads::default();
    let prompt_reads_for_handler = prompt_reads.clone();
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
                if events_for_notifications
                    .send(AcpEvent::SessionUpdate(Box::new(notification)))
                    .is_err()
                {
                    tracing::debug!("ACP session update receiver closed");
                }
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
                    if auto_approvals
                        .send(AcpEvent::PermissionAutoDecided {
                            tool_call: request.tool_call.clone(),
                            choice: crate::thread::PermissionChoice::from_option(option),
                        })
                        .is_err()
                    {
                        tracing::debug!("ACP automatic permission receiver closed");
                    }
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
                        log_reply(
                            responder.respond(permission_response(PermissionDecision::Cancelled)),
                            "permission",
                        );
                        return;
                    }
                    let decision = decision_rx.await.unwrap_or(PermissionDecision::Cancelled);
                    log_reply(
                        responder.respond(permission_response(decision)),
                        "permission",
                    );
                });
                Ok(())
            },
            acp::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: ReadTextFileRequest, responder, _cx| {
                let roots = roots_for_read.clone();
                let prompt_reads = prompt_reads_for_handler.clone();
                let events = reads.clone();
                tokio::task::spawn_blocking(move || {
                    let content = prompt_reads
                        .read(
                            &request.session_id,
                            &request.path,
                            request.line,
                            request.limit,
                        )
                        .and_then(|content| match content {
                            Some(content) => Ok(content),
                            None => {
                                roots.read_text_file(&request.path, request.line, request.limit)
                            }
                        });
                    let reply = match content {
                        Ok(content) => responder.respond(ReadTextFileResponse::new(content)),
                        Err(error) => respond_with_error(responder, error, &events, "read"),
                    };
                    log_reply(reply, "file read");
                });
                Ok(())
            },
            acp::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: WriteTextFileRequest, responder, _cx| {
                let roots = roots_for_write.clone();
                let events = writes.clone();
                tokio::task::spawn_blocking(move || {
                    let reply = match roots.write_text_file(&request.path, &request.content) {
                        Ok(()) => responder.respond(WriteTextFileResponse::new()),
                        Err(error) => respond_with_error(responder, error, &events, "write"),
                    };
                    log_reply(reply, "file write");
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
                    let reply = match registry.create(request).await {
                        Ok(response) => responder.respond(response),
                        Err(error) => {
                            let message = format!("terminal failed to start: {error}");
                            if events.send(terminal_notice(message.clone())).is_err() {
                                tracing::debug!("ACP terminal error receiver closed");
                            }
                            responder.respond_with_internal_error(message)
                        }
                    };
                    log_reply(reply, "terminal creation");
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
                    log_reply(responder.respond(response), "terminal output");
                });
                Ok(())
            },
            acp::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: WaitForTerminalExitRequest, responder, _cx| {
                let registry = registry_for_wait.clone();
                let cancellation = responder.cancellation();
                tokio::spawn(async move {
                    let reply = tokio::select! {
                        result = registry.wait_for_exit(&request.terminal_id) => match result {
                            Ok(response) => responder.respond(response),
                            Err(error) => responder.respond_with_internal_error(error),
                        },
                        _ = cancellation.cancelled() => responder.respond_with_error(acp::Error::request_cancelled()),
                    };
                    log_reply(reply, "terminal exit wait");
                });
                Ok(())
            },
            acp::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: KillTerminalRequest, responder, _cx| {
                let registry = registry_for_kill.clone();
                tokio::spawn(async move {
                    let reply = match registry.kill(&request.terminal_id).await {
                        Ok(response) => responder.respond(response),
                        Err(error) => responder.respond_with_internal_error(error),
                    };
                    log_reply(reply, "terminal kill");
                });
                Ok(())
            },
            acp::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: ReleaseTerminalRequest, responder, _cx| {
                let registry = registry_for_release.clone();
                tokio::spawn(async move {
                    let reply = match registry.release(&request.terminal_id).await {
                        Ok(response) => responder.respond(response),
                        Err(error) => responder.respond_with_internal_error(error),
                    };
                    log_reply(reply, "terminal release");
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
                initialize = initialize => match initialize {
                    Ok(response) => response,
                    Err(error) if !acp::is_incoming_transport_closed(&error) => {
                        if ready_tx.send(Err(error.into())).is_err() {
                            tracing::debug!("ACP caller dropped during initialization");
                        }
                        return Ok(());
                    }
                    Err(error) => return Err(error),
                },
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
                prompt_images: initialize.agent_capabilities.prompt_capabilities.image,
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
        if connection.await.is_err() {
            tracing::warn!("agent connection ended with an error");
        }
        registry.shutdown().await;
    });

    let (requester, info) = match ready_rx.await {
        Ok(result) => {
            let (connection, info) = result?;
            (
                Requester {
                    connection,
                    auth_methods: info.auth_methods.clone().into(),
                    prompt_reads,
                },
                info,
            )
        }
        Err(_) => {
            while let Ok(event) = events_rx.try_recv() {
                if let AcpEvent::ProtocolFailed { message } = event {
                    return Err(AcpError::Protocol(message));
                }
            }
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
        supervision: None,
        supervision_failure: None,
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
    AcpEvent::Stderr(message.into())
}

fn log_reply(reply: Result<(), acp::Error>, operation: &'static str) {
    if reply.is_err() {
        tracing::debug!(operation, "could not deliver ACP response");
    }
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
    if events
        .send(AcpEvent::Stderr(message.clone().into()))
        .is_err()
    {
        tracing::debug!(operation, "ACP file error receiver closed");
    }
    responder.respond_with_internal_error(message)
}

/// Launches an agent process in a project directory.
///
/// Mirrors the SDK's own agent component, minus the launch configuration it
/// cannot express: the child's working directory.
type SupervisionChannels = (
    tokio::sync::mpsc::UnboundedSender<Result<(), String>>,
    tokio::sync::mpsc::UnboundedReceiver<()>,
);

struct AgentDiagnostics {
    stderr_task: Option<tokio::task::JoinHandle<()>>,
    events: Option<tokio::sync::mpsc::UnboundedSender<AcpEvent>>,
    stderr: Option<Arc<Mutex<VecDeque<String>>>>,
}

pub struct ProjectAgent {
    spec: SpawnSpec,
    events: Option<tokio::sync::mpsc::UnboundedSender<AcpEvent>>,
    stderr: Option<Arc<Mutex<VecDeque<String>>>>,
    tracker: Option<Arc<dyn ProcessTracker>>,
    supervision: Option<SupervisionChannels>,
}

impl ProjectAgent {
    pub fn new(spec: SpawnSpec) -> Self {
        Self {
            spec,
            events: None,
            stderr: None,
            tracker: None,
            supervision: None,
        }
    }

    fn with_process_tracker(mut self, tracker: Option<Arc<dyn ProcessTracker>>) -> Self {
        self.tracker = tracker;
        self
    }

    fn with_supervision(
        mut self,
        completion: tokio::sync::mpsc::UnboundedSender<Result<(), String>>,
        retry: tokio::sync::mpsc::UnboundedReceiver<()>,
    ) -> Self {
        self.supervision = Some((completion, retry));
        self
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

struct StartupProcessGroup(#[cfg(unix)] Option<libc::pid_t>);

impl StartupProcessGroup {
    fn new(child: &async_process::Child) -> Self {
        #[cfg(unix)]
        {
            Self(libc::pid_t::try_from(child.id()).ok())
        }
        #[cfg(not(unix))]
        {
            let _child = child;
            Self()
        }
    }

    fn disarm(&mut self) {
        #[cfg(unix)]
        {
            self.0 = None;
        }
    }
}

#[cfg(unix)]
impl Drop for StartupProcessGroup {
    fn drop(&mut self) {
        if let Some(group) = self.0.take()
            && unsafe { libc::killpg(group, libc::SIGKILL) } != 0
        {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                tracing::warn!(%error, "could not stop the ACP startup process group");
            }
        }
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
        let mut startup_group = StartupProcessGroup::new(&child);
        let process_record = if let Some(tracker) = self.tracker {
            let id = tracker.started(child.id()).await.map_err(|error| {
                let message = format!("Could not record ACP agent process: {error}");
                if let Some(events) = &self.events
                    && events
                        .send(AcpEvent::ProtocolFailed {
                            message: message.clone(),
                        })
                        .is_err()
                {
                    tracing::debug!("ACP protocol failure receiver closed");
                }
                acp::Error::internal_error().data(message)
            })?;
            Some((tracker, id))
        } else {
            None
        };

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| acp::Error::internal_error().data("agent stdin unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| acp::Error::internal_error().data("agent stdout unavailable"))?;
        let stderr = child.stderr.take();

        let stderr_task = if let (Some(mut stderr), Some(events), Some(buffer)) =
            (stderr, self.events.clone(), self.stderr.clone())
        {
            // stderr has no protocol framing, so it is drained on its own task:
            // a full pipe from a chatty agent would otherwise stall the child.
            Some(tokio::spawn(async move {
                let mut lines = crate::bounded_lines::BoundedLines::default();
                let mut bytes = [0u8; 4096];
                let permits = Arc::new(tokio::sync::Semaphore::new(PENDING_STDERR_EVENTS));
                let record = |line: crate::bounded_lines::BoundedLine| {
                    record_agent_stderr(line.text, &buffer, &events, &permits);
                };
                loop {
                    match stderr.read(&mut bytes).await {
                        Ok(0) => break,
                        Ok(count) => {
                            for line in lines.push(&bytes[..count]) {
                                record(line);
                            }
                        }
                        Err(error) => {
                            tracing::debug!(%error, "could not read ACP stderr");
                            break;
                        }
                    }
                }
                if let Some(line) = lines.finish() {
                    record(line);
                }
            }))
        } else {
            None
        };

        let protocol_events = self.events.clone();
        let incoming =
            futures::stream::try_unfold(BufReader::new(stdout), |mut reader| async move {
                Ok::<_, std::io::Error>(
                    read_protocol_line(&mut reader, MAX_PROTOCOL_LINE_BYTES)
                        .await?
                        .map(|line| (line, reader)),
                )
            });
        let incoming = futures::StreamExt::map(incoming, move |line| {
            let line = line.inspect_err(|error| {
                if error.kind() == std::io::ErrorKind::InvalidData
                    && let Some(events) = &protocol_events
                    && events
                        .send(AcpEvent::ProtocolFailed {
                            message: error.to_string(),
                        })
                        .is_err()
                {
                    tracing::debug!("ACP protocol failure receiver closed");
                }
            })?;
            if !line.trim().is_empty()
                && let Err(error) = serde_json::from_str::<serde::de::IgnoredAny>(&line)
            {
                let message = format!("The ACP agent sent malformed JSON: {error}");
                if let Some(events) = &protocol_events
                    && events
                        .send(AcpEvent::ProtocolFailed {
                            message: message.clone(),
                        })
                        .is_err()
                {
                    tracing::debug!("ACP protocol failure receiver closed");
                }
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    message,
                ));
            }
            Ok(line)
        });
        let outgoing = futures::sink::unfold(stdin, |mut writer, line: String| async move {
            writer.write_all(line.as_bytes()).await?;
            writer.write_all(b"\n").await?;
            Ok::<_, std::io::Error>(writer)
        });

        // The SDK may drop the wire future on shutdown. Reaping and reporting
        // the subprocess must outlive that future.
        let (wire_finished, finished) = oneshot::channel();
        let supervisor = tokio::spawn(async move {
            supervise_agent(
                child,
                finished,
                AgentDiagnostics {
                    stderr_task,
                    events: self.events,
                    stderr: self.stderr,
                },
                self.supervision,
                process_record,
                kill_agent_process,
            )
            .await;
        });
        startup_group.disarm();
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
    diagnostics: AgentDiagnostics,
    mut supervision: Option<SupervisionChannels>,
    process_record: Option<(Arc<dyn ProcessTracker>, String)>,
    mut stop: impl FnMut(&mut async_process::Child) -> std::io::Result<()> + Send,
) {
    let AgentDiagnostics {
        mut stderr_task,
        events,
        stderr,
    } = diagnostics;
    let mut status = tokio::select! {
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
                Err(_) => match stop(&mut child) {
                    Ok(()) => child.status().await,
                    Err(error) => Err(error),
                },
            }
        }
    };
    while let Err(error) = status {
        tracing::warn!(%error, "could not stop or reap the ACP agent");
        let message = format!("Could not stop or reap the ACP agent: {error}");
        if let Some(events) = &events
            && events
                .send(AcpEvent::ProtocolFailed {
                    message: message.clone(),
                })
                .is_err()
        {
            tracing::debug!("ACP protocol failure receiver closed");
        }
        let retry = match &mut supervision {
            Some((results, retry)) => {
                if results.send(Err(message)).is_err() {
                    false
                } else {
                    retry.recv().await.is_some()
                }
            }
            None => false,
        };
        if !retry {
            if let Err(error) = child.kill() {
                tracing::warn!(%error, "could not stop the abandoned ACP child");
            }
            return;
        }
        status = match stop(&mut child) {
            Ok(()) => child.status().await,
            Err(error) => Err(error),
        };
    }
    let code = status.ok().and_then(|status| status.code());
    if let Some((tracker, record_id)) = process_record {
        loop {
            match tracker.stopped(&record_id).await {
                Ok(()) => break,
                Err(error) => {
                    let message = format!("Could not clear ACP agent process record: {error}");
                    tracing::warn!(%error, "could not clear ACP agent process record");
                    if let Some(events) = &events
                        && events
                            .send(AcpEvent::ProtocolFailed {
                                message: message.clone(),
                            })
                            .is_err()
                    {
                        tracing::debug!("ACP protocol failure receiver closed");
                    }
                    let retry = match &mut supervision {
                        Some((results, retry)) => {
                            results.send(Err(message)).is_ok() && retry.recv().await.is_some()
                        }
                        None => false,
                    };
                    if !retry {
                        return;
                    }
                }
            }
        }
    }
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
    if let Some((results, _)) = supervision
        && results.send(Ok(())).is_err()
    {
        tracing::debug!("ACP process supervision receiver closed");
    }
}

fn kill_agent_process(child: &mut async_process::Child) -> std::io::Result<()> {
    if child.try_status()?.is_some() {
        return Ok(());
    }
    #[cfg(unix)]
    return kill_agent_process_with(child, |group| {
        if unsafe { libc::killpg(group, libc::SIGKILL) } == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    });
    #[cfg(not(unix))]
    child.kill()
}

#[cfg(unix)]
fn kill_agent_process_with(
    child: &mut async_process::Child,
    signal_group: impl FnOnce(libc::pid_t) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let group = libc::pid_t::try_from(child.id()).map_err(std::io::Error::other)?;
    match signal_group(group) {
        Ok(()) => Ok(()),
        Err(error) if error.raw_os_error() == Some(libc::ESRCH) => child.kill(),
        Err(error) => Err(std::io::Error::other(format!(
            "could not stop ACP process group: {error}"
        ))),
    }
}

/// The trailing stderr lines worth showing in an error state.
pub fn stderr_summary(lines: &[String]) -> String {
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_response_logs_context_without_error_details() -> Result<(), Box<dyn std::error::Error>>
    {
        use std::io::Write;

        #[derive(Clone)]
        struct Writer(Arc<Mutex<Vec<u8>>>);
        impl Write for Writer {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0
                    .lock()
                    .map_err(|error| std::io::Error::other(error.to_string()))?
                    .extend_from_slice(bytes);
                Ok(bytes.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let logs = Arc::new(Mutex::new(Vec::new()));
        let writer = Writer(logs.clone());
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .without_time()
            .with_max_level(tracing::Level::DEBUG)
            .with_writer(move || writer.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            log_reply(
                Err(acp::Error::internal_error().data("fixture-private-value")),
                "file write",
            );
        });
        let output = String::from_utf8(logs.lock().map_err(|error| error.to_string())?.clone())?;
        assert!(output.contains("could not deliver ACP response"));
        assert!(output.contains("file write"));
        assert!(!output.contains("fixture-private-value"));
        Ok(())
    }

    #[tokio::test]
    async fn close_wait_retries_supervision_failure() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let options = ConnectOptions {
            roots: SessionRoots::new([directory.path().to_path_buf()]),
            tool_permissions: ToolPermissions::default(),
        };
        let (mut connection, _) =
            crate::fake_agent::connect_fake_agent(Default::default(), options).await?;
        let (sender, results) = tokio::sync::mpsc::unbounded_channel();
        let (retry, mut retries) = tokio::sync::mpsc::unbounded_channel();
        connection.supervision = Some(ProcessSupervision {
            results,
            retry,
            retry_needed: false,
        });
        sender
            .send(Err("fixture shutdown failure".into()))
            .map_err(|_| "receiver closed")?;
        let error = connection
            .close_and_wait()
            .await
            .expect_err("failure must be reported");
        assert!(error.to_string().contains("fixture shutdown failure"));
        let task = tokio::spawn(async move {
            if retries.recv().await.is_some() {
                sender.send(Ok(())).map_err(|_| "receiver closed")?;
            }
            Ok::<(), &'static str>(())
        });
        connection.close_and_wait().await?;
        task.await??;
        Ok(())
    }

    #[tokio::test]
    async fn close_wait_timeout_can_be_retried() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let options = ConnectOptions {
            roots: SessionRoots::new([directory.path().to_path_buf()]),
            tool_permissions: ToolPermissions::default(),
        };
        let (mut connection, _) =
            crate::fake_agent::connect_fake_agent(Default::default(), options).await?;
        let (sender, results) = tokio::sync::mpsc::unbounded_channel();
        let (retry, _retries) = tokio::sync::mpsc::unbounded_channel();
        connection.supervision = Some(ProcessSupervision {
            results,
            retry,
            retry_needed: false,
        });
        let error = connection
            .close_and_wait_with_timeout(Duration::from_millis(20))
            .await
            .expect_err("the supervisor is still running");
        assert!(error.to_string().contains("shutdown deadline"));
        sender.send(Ok(())).map_err(|_| "receiver closed")?;
        connection.close_and_wait().await?;
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn supervisor_retries_stop_without_losing_child() -> Result<(), Box<dyn std::error::Error>>
    {
        let child = async_process::Command::new("/bin/sleep")
            .arg("30")
            .kill_on_drop(true)
            .spawn()?;
        let pid = libc::pid_t::try_from(child.id())?;
        let (wire_done, wire_finished) = oneshot::channel();
        let (events, mut event_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (results, mut result_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (retry, retries) = tokio::sync::mpsc::unbounded_channel();
        let mut attempts = 0;
        let supervisor = tokio::spawn(supervise_agent(
            child,
            wire_finished,
            AgentDiagnostics {
                stderr_task: None,
                events: Some(events),
                stderr: None,
            },
            Some((results, retries)),
            None,
            move |child| {
                attempts += 1;
                if attempts == 1 {
                    Err(std::io::Error::other("fixture stop failure"))
                } else {
                    child.kill()
                }
            },
        ));
        wire_done.send(()).map_err(|_| "wire closed")?;
        let failure = tokio::time::timeout(Duration::from_secs(3), result_receiver.recv())
            .await?
            .ok_or("missing result")?;
        assert!(
            failure
                .expect_err("first stop must fail")
                .contains("fixture stop failure")
        );
        assert_eq!(unsafe { libc::kill(pid, 0) }, 0);
        assert!(matches!(
            event_receiver.try_recv(),
            Ok(AcpEvent::ProtocolFailed { .. })
        ));
        assert!(matches!(
            event_receiver.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
        retry.send(()).map_err(|_| "retry closed")?;
        tokio::time::timeout(Duration::from_secs(3), result_receiver.recv())
            .await?
            .ok_or("missing result")??;
        supervisor.await?;
        assert!(matches!(
            event_receiver.try_recv(),
            Ok(AcpEvent::Exited { .. })
        ));
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn failed_acp_group_signal_keeps_child_for_retry()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut child = async_process::Command::new("/bin/sleep")
            .arg("30")
            .kill_on_drop(true)
            .spawn()?;
        let error = kill_agent_process_with(&mut child, |_| {
            Err(std::io::Error::other("fixture group failure"))
        })
        .expect_err("group failure must be reported");
        assert!(error.to_string().contains("fixture group failure"));
        assert!(child.try_status()?.is_none());
        child.kill()?;
        let status = tokio::time::timeout(Duration::from_secs(3), child.status()).await??;
        assert!(!status.success());
        Ok(())
    }

    #[test]
    fn stderr_flood_keeps_a_bounded_queue_and_the_latest_diagnostics()
    -> Result<(), Box<dyn std::error::Error>> {
        let buffer = Mutex::new(VecDeque::new());
        let permits = Arc::new(tokio::sync::Semaphore::new(PENDING_STDERR_EVENTS));
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        for index in 0..10_000 {
            record_agent_stderr(format!("line {index}"), &buffer, &sender, &permits);
        }
        assert_eq!(receiver.len(), PENDING_STDERR_EVENTS);
        let tail = buffer.lock().map_err(|_| "stderr buffer poisoned")?;
        assert_eq!(tail.len(), STDERR_LINES);
        assert_eq!(tail.back().map(String::as_str), Some("line 9999"));
        drop(tail);
        sender.send(AcpEvent::ProtocolFailed {
            message: "failure".into(),
        })?;
        assert_eq!(receiver.len(), PENDING_STDERR_EVENTS + 1);
        drop(receiver.try_recv()?);
        record_agent_stderr("after drain".into(), &buffer, &sender, &permits);
        let mut found = false;
        while let Ok(event) = receiver.try_recv() {
            if let AcpEvent::Stderr(message) = event {
                found |= message.as_str() == "after drain";
            }
        }
        assert!(found);
        Ok(())
    }

    #[tokio::test]
    async fn protocol_lines_are_bounded_and_validate_utf8() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut reader = BufReader::new(futures::io::Cursor::new(b"1234\r\nnext\nlast".to_vec()));
        assert_eq!(
            read_protocol_line(&mut reader, 4).await?.as_deref(),
            Some("1234")
        );
        assert_eq!(
            read_protocol_line(&mut reader, 4).await?.as_deref(),
            Some("next")
        );
        assert_eq!(
            read_protocol_line(&mut reader, 4).await?.as_deref(),
            Some("last")
        );
        assert!(read_protocol_line(&mut reader, 4).await?.is_none());
        let mut large = BufReader::new(futures::io::Cursor::new(b"12345".to_vec()));
        let Err(error) = read_protocol_line(&mut large, 4).await else {
            return Err("oversized line succeeded".into());
        };
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("exceeds 4 bytes"));
        let mut invalid = BufReader::new(futures::io::Cursor::new(vec![0xff, b'\n']));
        let Err(error) = read_protocol_line(&mut invalid, 4).await else {
            return Err("invalid UTF-8 succeeded".into());
        };
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        Ok(())
    }

    #[cfg(feature = "live-opencode")]
    #[tokio::test]
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
