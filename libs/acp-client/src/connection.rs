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
use std::path::PathBuf;
use std::time::Duration;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use agent_client_protocol as acp;
use agent_client_protocol::schema::v1::{
    AuthMethod, AuthenticateRequest, CancelNotification, ClientCapabilities, CloseSessionRequest,
    CreateTerminalRequest, FileSystemCapabilities, InitializeRequest, KillTerminalRequest,
    LoadSessionRequest, NewSessionRequest, PromptRequest, ReadTextFileRequest,
    ReadTextFileResponse, ReleaseTerminalRequest, RequestPermissionOutcome,
    RequestPermissionRequest, RequestPermissionResponse, SelectedPermissionOutcome,
    SessionConfigId, SessionConfigOption, SessionConfigOptionValue, SessionId, SessionModeId,
    SessionModeState, SessionNotification, SetSessionConfigOptionRequest, SetSessionModeRequest,
    StopReason, TerminalOutputRequest, WaitForTerminalExitRequest, WriteTextFileRequest,
    WriteTextFileResponse,
};
use acp::schema::ProtocolVersion;
use acp::{Agent, Client, ConnectTo, ConnectionTo, Lines};
use futures::channel::oneshot;
use futures::io::BufReader;
use futures::{AsyncBufReadExt as _, AsyncWriteExt as _};

use crate::agent::{AgentServer, SpawnSpec};
use crate::fs::{SessionRoots, read_text_file, write_text_file};
use crate::permissions::{ApprovalMode, PermissionDecision, PermissionReply};
use crate::terminal::TerminalRegistry;
use crate::{AcpError, AcpEvent, SessionSpec, schema};

/// How many stderr lines are kept for the error state.
const STDERR_LINES: usize = 250;

/// What the client learned about the agent during `initialize`.
#[derive(Clone, Debug, Default)]
pub struct AgentInfo {
    pub name: String,
    pub version: String,
    pub load_session: bool,
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
    /// Create a session rooted at `cwd` with extra workspace roots.
    pub async fn new_session(
        &self,
        cwd: PathBuf,
        additional: Vec<PathBuf>,
    ) -> Result<
        (
            SessionId,
            Option<SessionModeState>,
            Vec<SessionConfigOption>,
        ),
        AcpError,
    > {
        let request = NewSessionRequest::new(cwd).additional_directories(additional);
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
    ) -> Result<(Option<SessionModeState>, Vec<SessionConfigOption>), AcpError> {
        let request = LoadSessionRequest::new(session_id, cwd).additional_directories(additional);
        let response = self
            .connection
            .send_request(request)
            .block_task()
            .await
            .map_err(AcpError::from)?;
        Ok((response.modes, response.config_options.unwrap_or_default()))
    }

    /// Send a prompt and wait for the turn to end.
    pub async fn prompt(&self, session_id: &SessionId, text: String) -> Result<StopReason, AcpError> {
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
    pub async fn set_config_option(
        &self,
        session_id: &SessionId,
        config_id: SessionConfigId,
        value: SessionConfigOptionValue,
    ) -> Result<(), AcpError> {
        self.connection
            .send_request(SetSessionConfigOptionRequest::new(
                session_id.clone(),
                config_id,
                value,
            ))
            .block_task()
            .await
            .map(|_| ())
            .map_err(AcpError::from)
    }

    pub async fn authenticate(&self, method_id: impl Into<schema::AuthMethodId>) -> Result<(), AcpError> {
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
    /// Whether tool calls are answered automatically for this project.
    auto_approve: Arc<AtomicBool>,
    /// Recent agent stderr, newest last.
    stderr: Arc<Mutex<VecDeque<String>>>,
    /// Dropping this ends the connection and terminates the process group.
    shutdown: Option<oneshot::Sender<()>>,
}

impl AcpConnection {
    /// Toggle automatic approval of permission requests.
    pub fn set_auto_approve(&self, auto_approve: bool) {
        self.auto_approve.store(auto_approve, Ordering::Relaxed);
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
    pub auto_approve: bool,
}

/// Connect to an agent process launched in the project directory.
pub async fn connect(
    agent: Arc<dyn AgentServer>,
    spec: &SessionSpec,
    options_for: impl FnOnce(&PathBuf) -> ConnectOptions,
) -> Result<AcpConnection, AcpError> {
    let (cwd, _additional) = spec.resolve().ok_or(AcpError::NoProjectDirectory)?;
    let spawn = agent.spawn_spec(&cwd)?;
    let options = options_for(&cwd);
    connect_over(
        move |events, stderr| ProjectAgent::new(spawn).with_stderr_reporting(events, stderr),
        options,
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
    let (events_tx, events_rx) = tokio::sync::mpsc::unbounded_channel();
    let stderr_buffer = Arc::new(Mutex::new(VecDeque::new()));
    let transport = build_transport(events_tx.clone(), stderr_buffer.clone());
    let (ready_tx, ready_rx) = oneshot::channel();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let auto_approve = Arc::new(AtomicBool::new(options.auto_approve));
    let auto_approve_for_handlers = auto_approve.clone();

    let roots = options.roots.clone();
    let roots_for_handlers = roots.clone();
    let events_for_notifications = events_tx.clone();
    let permissions = events_tx.clone();
    let reads = events_tx.clone();
    let writes = events_tx.clone();
    let terminal_create = events_tx.clone();
    let registries = events_tx.clone();
    let auto_approvals = events_tx.clone();
    let registry = TerminalRegistry::new(roots_for_handlers.clone(), registries);
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
        .fs(
            FileSystemCapabilities::new()
                .read_text_file(true)
                .write_text_file(true),
        );

    let connection = Client
        .builder()
        .name("todo-2")
        // Notifications: hand session updates straight to the UI, which reduces
        // them through the transcript model.
        .on_receive_notification(
            async move |notification: SessionNotification, _cx| {
                drop(
                    events_for_notifications
                        .send(AcpEvent::SessionUpdate(Box::new(notification))),
                );
                Ok(())
            },
            acp::on_receive_notification!(),
        )
        // Permissions: answer immediately when auto-approving, otherwise wait
        // for the user without blocking the event loop.
        .on_receive_request(
            async move |request: RequestPermissionRequest, responder, _cx| {
                if let Some(option) = auto_approve_for_handlers
                    .load(Ordering::Relaxed)
                    .then(|| ApprovalMode::strongest_allow(&request.options))
                    .flatten()
                {
                    responder.respond(permission_response(PermissionDecision::Selected(
                        option.option_id.clone(),
                    )))?;
                    // Report the auto-grant so the transcript records it.
                    drop(auto_approvals.send(AcpEvent::PermissionAutoApproved {
                        tool_call: request.tool_call.clone(),
                        choice: crate::thread::PermissionChoice::from_option(option),
                    }));
                    return Ok(());
                }
                let permissions = permissions.clone();
                tokio::spawn(async move {
                    let (decision_tx, decision_rx) = oneshot::channel();
                    let sent = permissions.send(AcpEvent::PermissionRequested {
                        tool_call: request.tool_call.clone(),
                        options: request.options.clone(),
                        decision: PermissionReply::new(decision_tx),
                    });
                    if sent.is_err() {
                        responder.respond(permission_response(PermissionDecision::Cancelled))?;
                        return Ok::<(), acp::Error>(());
                    }
                    let decision = decision_rx
                        .await
                        .unwrap_or(PermissionDecision::Cancelled);
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
                    match registry.create(request) {
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
                    responder.respond(registry.kill(&request.terminal_id))?;
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
                    responder.respond(registry.release(&request.terminal_id))?;
                    Ok::<(), acp::Error>(())
                });
                Ok(())
            },
            acp::on_receive_request!(),
        )
        .connect_with(transport, async move |cx: ConnectionTo<Agent>| {
            let initialize = cx
                .send_request(
                    InitializeRequest::new(ProtocolVersion::V1)
                        .client_capabilities(capabilities),
                )
                .block_task()
                .await?;
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
                auth_methods: initialize.auth_methods.clone(),
            };
            if ready_tx.send((cx.clone(), info)).is_err() {
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
    tokio::spawn(async move {
        if let Err(error) = connection.await {
            tracing::warn!("agent connection ended: {error}");
        }
    });

    let (requester, info) = match ready_rx.await {
        Ok((connection, info)) => (Requester { connection }, info),
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
        auto_approve,
        stderr: stderr_buffer,
        shutdown: Some(shutdown_tx),
    })
}

fn permission_response(decision: PermissionDecision) -> RequestPermissionResponse {
    match decision {
        PermissionDecision::Selected(option_id) => {
            RequestPermissionResponse::new(RequestPermissionOutcome::Selected(
                SelectedPermissionOutcome::new(option_id),
            ))
        }
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
            .stderr(Stdio::piped());
        command.spawn()
    }
}

impl ConnectTo<Client> for ProjectAgent {
    async fn connect_to(
        self,
        client: impl ConnectTo<Agent>,
    ) -> Result<(), acp::Error> {
        let mut child = self.spawn_child().map_err(|error| {
            acp::Error::internal_error()
                .data(format!("failed to launch {}: {error}", self.spec.command_line()))
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

        if let (Some(stderr), Some(events), Some(buffer)) =
            (stderr, self.events.clone(), self.stderr.clone())
        {
            // stderr has no protocol framing, so it is drained on its own task:
            // a full pipe from a chatty agent would otherwise stall the child.
            tokio::spawn(async move {
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
            });
        }

        let incoming = BufReader::new(stdout).lines();
        let outgoing = futures::sink::unfold(stdin, |mut writer, line: String| async move {
            writer.write_all(line.as_bytes()).await?;
            writer.write_all(b"\n").await?;
            Ok::<_, std::io::Error>(writer)
        });

        let result = ConnectTo::<Client>::connect_to(Lines::new(outgoing, incoming), client).await;

        // The connection is over: make sure the process tree is gone. Killing
        // here (rather than in a guard) keeps the failure path explicit.
        if let Err(error) = child.kill() {
            tracing::debug!("agent process was already gone: {error}");
        }
        drop(child.status().await);

        result
    }
}

/// The trailing stderr lines worth showing in an error state.
pub fn stderr_summary(lines: &[String]) -> String {
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

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
