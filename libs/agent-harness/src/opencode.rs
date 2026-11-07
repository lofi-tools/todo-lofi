//! OpenCode's private HTTP server and session control.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use reqwest::{Client, Method, RequestBuilder, Response, Url};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex as AsyncMutex, mpsc};
use tokio::task::JoinHandle;
use tokio::time::{Instant, timeout, timeout_at};

use crate::acp::{AcpHarness, TerminalBackend};
use crate::settings::AgentSettings;
use crate::{
    AdapterTier, AgentDiagnostics, AgentHarness, AgentMode, AgentModes, ApprovalId,
    ApprovalRequest, Capabilities, Decision, DeltaKind, Entry, FailureKind, HarnessError,
    HarnessEvent, HarnessResult, ItemId, PromptInput, SessionId, SessionState, SpawnSpec,
    ToolCallId, ToolStatus, TurnFinishReason, TurnId, command_line,
};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(20);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const USERNAME: &str = "opencode";
const SERVER_ARGS: [&str; 5] = ["serve", "--hostname", "127.0.0.1", "--port", "0"];

/// Read the configured executable without resolving aliases, modifying the
/// settings file, or installing a binary.
pub fn configured_executable(settings_path: &Path) -> HarnessResult<Option<PathBuf>> {
    AgentSettings::read(settings_path)?.executable("agents.opencode.executable")
}

pub fn configured_launch(settings: &AgentSettings) -> HarnessResult<Option<SpawnSpec>> {
    let Some(executable) = settings.executable("agents.opencode.executable")? else {
        return Ok(None);
    };
    let program = executable
        .into_os_string()
        .into_string()
        .map_err(|_| HarnessError::Protocol("agents.opencode.executable must be UTF-8".into()))?;
    let mut spec = SpawnSpec {
        program,
        ..SpawnSpec::default()
    };
    if let Some(path) = settings.absolute_path("agents.opencode.config_file")? {
        let path = path.into_os_string().into_string().map_err(|_| {
            HarnessError::Protocol("agents.opencode.config_file must be UTF-8".into())
        })?;
        spec.environment.insert("OPENCODE_CONFIG".into(), path);
    }
    Ok(Some(spec))
}

pub async fn project_modes(spec: &SpawnSpec) -> HarnessResult<Vec<AgentMode>> {
    let hide_diagnostics = !spec.environment.is_empty();
    let server = OpenCodeServer::start(spec)
        .await
        .map_err(|error| crate::protect_discovery_error(error, hide_diagnostics))?;
    let modes = if server.probe.modes {
        server.mode_catalog().await
    } else {
        Ok(Vec::new())
    };
    let shutdown = server.shutdown().await;
    let modes = modes.map_err(|error| crate::protect_discovery_error(error, hide_diagnostics))?;
    shutdown.map_err(|error| crate::protect_discovery_error(error, hide_diagnostics))?;
    Ok(modes)
}

fn require_absolute_executable(path: &Path) -> HarnessResult<()> {
    if !path.is_absolute() {
        return Err(HarnessError::Protocol(
            "agents.opencode.executable must be an absolute path".into(),
        ));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenCodeProbe {
    pub version: String,
    pub resume: bool,
    pub prompt_files: bool,
    pub modes: bool,
}

#[derive(Deserialize)]
struct Health {
    version: String,
}

#[derive(Deserialize)]
struct Api {
    paths: BTreeMap<String, BTreeMap<String, Value>>,
}

impl Api {
    fn supports(&self, method: &str, path: &str) -> bool {
        self.paths
            .get(path)
            .is_some_and(|operations| operations.get(method).is_some_and(Value::is_object))
    }

    fn probe(&self, health: Health) -> HarnessResult<OpenCodeProbe> {
        if health.version.is_empty() {
            return Err(HarnessError::Protocol(
                "OpenCode did not report a healthy versioned server".into(),
            ));
        }
        for (method, path) in [
            ("get", "/api/event"),
            ("post", "/api/session"),
            ("post", "/api/session/{sessionID}/prompt"),
            ("post", "/api/session/{sessionID}/interrupt"),
            ("delete", "/api/session/{sessionID}/inbox/{inboxID}"),
            (
                "post",
                "/api/session/{sessionID}/permission/{requestID}/reply",
            ),
        ] {
            if !self.supports(method, path) {
                return Err(HarnessError::Protocol(format!(
                    "OpenCode server does not support {} {path}",
                    method.to_ascii_uppercase()
                )));
            }
        }
        Ok(OpenCodeProbe {
            version: health.version,
            resume: self.supports("get", "/api/session/{sessionID}"),
            modes: self.supports("get", "/api/session/{sessionID}")
                && self.supports("get", "/api/agent")
                && self.supports("post", "/api/session/{sessionID}/agent"),
            prompt_files: self
                .paths
                .get("/api/session/{sessionID}/prompt")
                .and_then(|operations| operations.get("post"))
                .and_then(|operation| {
                    operation
                        .pointer("/requestBody/content/application~1json/schema/properties/files")
                })
                .is_some(),
        })
    }
}

#[derive(Deserialize)]
struct SessionRecord {
    id: String,
    location: SessionLocation,
    agent: Option<String>,
}

#[derive(Deserialize)]
struct OpenCodeAgent {
    id: String,
    name: String,
    description: Option<String>,
    mode: String,
    hidden: bool,
}

#[derive(Deserialize)]
struct OpenCodeAgents {
    location: SessionLocation,
    data: Vec<OpenCodeAgent>,
}

#[derive(Deserialize)]
struct SessionLocation {
    directory: PathBuf,
}

#[derive(Deserialize)]
struct ResponseData<T> {
    data: T,
}

#[derive(Deserialize)]
struct InterruptResponse {
    interrupted: bool,
}

pub struct OpenCodeHarness {
    server: Option<OpenCodeServer>,
    session: Option<SessionId>,
    modes: Option<AgentModes>,
    turn: Option<ActiveTurn>,
    sender: mpsc::UnboundedSender<HarnessEvent>,
    receiver: mpsc::UnboundedReceiver<HarnessEvent>,
    diagnostics: AgentDiagnostics,
    fallback: Option<AcpHarness>,
    project_approvals: Option<Arc<dyn crate::ProjectApprovalPolicy>>,
    terminals: Option<Arc<dyn TerminalBackend>>,
}

impl Default for OpenCodeHarness {
    fn default() -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        Self {
            server: None,
            session: None,
            modes: None,
            turn: None,
            sender,
            receiver,
            diagnostics: AgentDiagnostics::default(),
            fallback: None,
            project_approvals: None,
            terminals: None,
        }
    }
}

impl OpenCodeHarness {
    pub fn with_terminals(mut self, terminals: Arc<dyn TerminalBackend>) -> Self {
        self.terminals = Some(terminals);
        self
    }
}

struct ActiveTurn {
    id: String,
    finished: Arc<AtomicBool>,
    reader: JoinHandle<()>,
}

impl Drop for ActiveTurn {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

#[async_trait]
impl AgentHarness for OpenCodeHarness {
    fn events(&mut self) -> &mut mpsc::UnboundedReceiver<HarnessEvent> {
        match &mut self.fallback {
            Some(fallback) => fallback.events(),
            None => &mut self.receiver,
        }
    }

    fn diagnostics(&self) -> AgentDiagnostics {
        if let Some(fallback) = &self.fallback {
            let mut diagnostics = fallback.diagnostics();
            diagnostics
                .fallback_reason
                .clone_from(&self.diagnostics.fallback_reason);
            diagnostics
        } else {
            let mut diagnostics = self.diagnostics.clone();
            if let Some(server) = &self.server {
                diagnostics.stderr_tail = server.stderr_tail();
            }
            diagnostics
        }
    }

    fn tier(&self) -> AdapterTier {
        if self.fallback.is_some() {
            AdapterTier::Acp
        } else {
            AdapterTier::Native
        }
    }

    fn modes(&self) -> Option<AgentModes> {
        match &self.fallback {
            Some(fallback) => fallback.modes(),
            None => self.modes.clone(),
        }
    }

    async fn set_mode(&mut self, mode: &str) -> HarnessResult<()> {
        if let Some(fallback) = &mut self.fallback {
            return fallback.set_mode(mode).await;
        }
        if self
            .turn
            .as_ref()
            .is_some_and(|turn| !turn.finished.load(Ordering::Acquire))
        {
            return Err(HarnessError::ModeUnavailable);
        }
        let server = self.server.as_ref().ok_or(HarnessError::NotStarted)?;
        let session = self.session.as_ref().ok_or(HarnessError::SessionMissing)?;
        let modes = self
            .modes
            .as_mut()
            .filter(|modes| modes.options.iter().any(|option| option.id == mode))
            .ok_or(HarnessError::ModeUnavailable)?;
        if modes.current.as_deref() != Some(mode) {
            let path = format!("{}/agent", session_path(session)?);
            send_request(
                server
                    .request(Method::POST, &path)
                    .json(&json!({ "agent": mode })),
            )
            .await?;
            modes.current = Some(mode.to_owned());
        }
        Ok(())
    }

    fn take_replayed_transcript(&mut self) -> Vec<Entry> {
        self.fallback
            .as_mut()
            .map_or_else(Vec::new, AgentHarness::take_replayed_transcript)
    }

    fn set_project_approval_policy(&mut self, policy: Arc<dyn crate::ProjectApprovalPolicy>) {
        self.project_approvals = Some(policy);
    }

    fn session_approvals(&self) -> Vec<crate::SessionApproval> {
        self.fallback
            .as_ref()
            .map_or_else(Vec::new, AgentHarness::session_approvals)
    }

    async fn reset_session_approval(&mut self, id: &str) -> HarnessResult<()> {
        if let Some(fallback) = &mut self.fallback {
            fallback.reset_session_approval(id).await?;
        }
        Ok(())
    }

    async fn start(&mut self, mut spec: SpawnSpec) -> HarnessResult<Capabilities> {
        if self.server.is_some() || self.fallback.is_some() {
            return Err(HarnessError::Protocol(
                "OpenCode harness is already started".into(),
            ));
        }
        require_absolute_executable(Path::new(&spec.program))?;
        let native = async {
            let server = OpenCodeServer::start(&spec).await?;
            server.subscribe().await?;
            Ok::<_, HarnessError>(server)
        };
        let server = match native.await {
            Ok(server) => server,
            Err(HarnessError::Protocol(reason) | HarnessError::TransportClosed(reason)) => {
                spec.args.push("acp".into());
                spec.environment
                    .insert("OPENCODE_DISABLE_AUTOUPDATE".into(), "1".into());
                let mut fallback = AcpHarness::default();
                if let Some(terminals) = self.terminals.take() {
                    fallback = fallback.with_terminals(terminals);
                }
                if let Some(policy) = self.project_approvals.take() {
                    fallback.set_project_approval_policy(policy);
                }
                self.diagnostics.command = command_line(&spec);
                self.diagnostics.fallback_reason =
                    Some(format!("OpenCode native server unavailable: {reason}"));
                let capabilities = fallback.start(spec).await.map_err(|error| {
                    HarnessError::Reported {
                        kind: FailureKind::classify(&error).unwrap_or(FailureKind::Protocol),
                        message: format!("OpenCode native server unavailable: {reason}. ACP fallback failed: {error}"),
                    }
                })?;
                self.fallback = Some(fallback);
                return Ok(capabilities);
            }
            Err(error) => return Err(error),
        };
        let probe = server.probe();
        let capabilities = Capabilities {
            streaming: true,
            thoughts: true,
            tool_calls: true,
            approvals: true,
            resume: probe.resume,
            cancel: true,
            images: probe.prompt_files,
            resource_links: probe.prompt_files,
            ..Capabilities::default()
        };
        spec.args.extend(SERVER_ARGS.map(str::to_owned));
        self.diagnostics = AgentDiagnostics {
            command: command_line(&spec),
            protocol: format!("OpenCode {} native HTTP", probe.version),
            ..AgentDiagnostics::default()
        };
        self.server = Some(server);
        Ok(capabilities)
    }

    async fn open_session(&mut self, resume: Option<SessionId>) -> HarnessResult<SessionId> {
        if let Some(fallback) = &mut self.fallback {
            return fallback.open_session(resume).await;
        }
        if self.session.is_some() {
            return Err(HarnessError::Protocol(
                "OpenCode session is already open".into(),
            ));
        }
        let server = self.server.as_ref().ok_or(HarnessError::NotStarted)?;
        let session = match resume {
            Some(session) => server.resume_session(&session).await?,
            None => server.create_session().await?,
        };
        if server.probe.modes {
            match server.session_modes(&session).await {
                Ok(modes) => self.modes = modes,
                Err(error) => tracing::warn!(%error, "could not load OpenCode session modes"),
            }
        }
        self.session = Some(session.clone());
        publish(
            &self.sender,
            HarnessEvent::SessionState {
                state: SessionState::Ready,
            },
        )?;
        Ok(session)
    }

    async fn prompt(&mut self, input: PromptInput) -> HarnessResult<()> {
        if let Some(fallback) = &mut self.fallback {
            return fallback.prompt(input).await;
        }
        if self
            .turn
            .as_ref()
            .is_some_and(|turn| !turn.finished.load(Ordering::Acquire))
        {
            return Err(HarnessError::Protocol(
                "OpenCode turn is already running".into(),
            ));
        }
        let server = self.server.as_ref().ok_or(HarnessError::NotStarted)?;
        let session = self.session.clone().ok_or(HarnessError::SessionMissing)?;
        let path = session_path(&session)?;
        let events = server.subscribe().await?;
        let id = format!("msg_{}", uuid::Uuid::new_v4().simple());
        if let Err(error) = server.enqueue_prompt(&session, &id, &input, true).await {
            if let Err(cleanup) = server.cancel_prompt(&session, &id).await {
                tracing::warn!(%cleanup, "could not clean up an unconfirmed OpenCode prompt");
            }
            return Err(error);
        }
        publish(
            &self.sender,
            HarnessEvent::TurnStarted {
                turn_id: TurnId::new(id.clone()),
            },
        )?;
        let finished = Arc::new(AtomicBool::new(false));
        let reader = tokio::spawn({
            let id = id.clone();
            let sender = self.sender.clone();
            let finished = finished.clone();
            let remove = server.request(Method::DELETE, &format!("{path}/inbox/{id}"));
            let interrupt = server.request(Method::POST, &format!("{path}/interrupt"));
            let child = server.child.clone();
            let stderr = server.stderr.clone();
            async move {
                let terminal = match read_turn(events, session, &id, &sender).await {
                    Ok(event) => event,
                    Err(error) => {
                        let exit = if matches!(error, HarnessError::TransportClosed(_)) {
                            match timeout(Duration::from_millis(250), child.lock().await.wait())
                                .await
                            {
                                Ok(status) => status.map(Some),
                                Err(_) => Ok(None),
                            }
                        } else {
                            child.lock().await.try_wait()
                        };
                        if let Ok(Some(status)) = exit {
                            HarnessEvent::Exited {
                                code: status.code(),
                                stderr: stderr_tail(&stderr),
                            }
                        } else {
                            if let Err(status) = exit {
                                tracing::warn!(%status, "could not read the OpenCode exit status");
                            }
                            if timeout(Duration::from_secs(1), async {
                                if let Err(cleanup) = send_request(remove).await {
                                    tracing::warn!(%cleanup, "could not remove the disconnected OpenCode prompt");
                                }
                                if let Err(cleanup) = send_request(interrupt).await {
                                    tracing::warn!(%cleanup, "could not interrupt the disconnected OpenCode turn");
                                }
                            }).await.is_err() {
                                tracing::warn!("OpenCode turn cleanup timed out");
                            }
                            if let Err(stop) = child.lock().await.kill().await {
                                tracing::debug!(%stop, "could not stop OpenCode after the connection failed");
                            }
                            HarnessEvent::ConnectionFailed {
                                kind: FailureKind::classify(&error)
                                    .unwrap_or(FailureKind::Protocol),
                                text: error.to_string(),
                            }
                        }
                    }
                };
                finished.store(true, Ordering::Release);
                if let Err(error) = publish(&sender, terminal) {
                    tracing::debug!(%error, "could not deliver OpenCode turn outcome");
                }
            }
        });
        self.turn = Some(ActiveTurn {
            id,
            finished,
            reader,
        });
        Ok(())
    }

    async fn cancel(&mut self) -> HarnessResult<()> {
        if let Some(fallback) = &mut self.fallback {
            return fallback.cancel().await;
        }
        let server = self.server.as_ref().ok_or(HarnessError::NotStarted)?;
        let session = self.session.as_ref().ok_or(HarnessError::SessionMissing)?;
        if let Some(turn) = &self.turn
            && !turn.finished.load(Ordering::Acquire)
        {
            server.cancel_prompt(session, &turn.id).await?;
        }
        Ok(())
    }

    async fn respond(&mut self, approval_id: ApprovalId, decision: Decision) -> HarnessResult<()> {
        if let Some(fallback) = &mut self.fallback {
            return fallback.respond(approval_id, decision).await;
        }
        let server = self.server.as_ref().ok_or(HarnessError::NotStarted)?;
        let session = self.session.as_ref().ok_or(HarnessError::SessionMissing)?;
        let id = approval_id.as_str();
        if !id.starts_with("per") || !valid_id(id) {
            return Err(HarnessError::NoPendingApproval);
        }
        let path = format!("{}/permission/{id}/reply", session_path(session)?);
        let reply = match decision {
            Decision::Allow => "once",
            Decision::Deny => "reject",
            Decision::AllowSession | Decision::AllowProject => {
                return Err(HarnessError::ApprovalOptionUnavailable);
            }
        };
        send_request(
            server
                .request(Method::POST, &path)
                .json(&json!({"decision": reply})),
        )
        .await?;
        Ok(())
    }

    async fn shutdown(&mut self) -> HarnessResult<()> {
        if let Some(fallback) = &mut self.fallback {
            return fallback.shutdown().await;
        }
        self.turn = None;
        self.session = None;
        self.modes = None;
        if let Some(server) = self.server.take() {
            self.diagnostics.stderr_tail = server.stderr_tail();
            server.shutdown().await?;
            publish(
                &self.sender,
                HarnessEvent::Exited {
                    code: Some(0),
                    stderr: self.diagnostics.stderr_tail.clone(),
                },
            )?;
        }
        Ok(())
    }
}

fn publish(sender: &mpsc::UnboundedSender<HarnessEvent>, event: HarnessEvent) -> HarnessResult<()> {
    sender.send(event).map_err(|_| HarnessError::ChannelClosed)
}

async fn read_turn(
    mut events: OpenCodeEvents,
    session: SessionId,
    prompt_id: &str,
    sender: &mpsc::UnboundedSender<HarnessEvent>,
) -> HarnessResult<HarnessEvent> {
    let mut transcript = OpenCodeTranscript::new(session.clone());
    let mut admitted = false;
    let mut approvals = HashSet::new();
    let admission_deadline = Instant::now() + REQUEST_TIMEOUT;
    loop {
        let event = if admitted {
            events.next().await?
        } else {
            timeout_at(admission_deadline, events.next())
                .await
                .map_err(|_| {
                    HarnessError::Protocol("OpenCode prompt admission event timed out".into())
                })??
        };
        if event.data["sessionID"] != session.as_str() {
            continue;
        }
        if event.data["inboxID"] == prompt_id {
            match event.kind.as_str() {
                "session.inbox.enqueued" => admitted = true,
                "session.inbox.cancelled" => {
                    return Ok(HarnessEvent::TurnFinished {
                        reason: TurnFinishReason::Cancelled,
                    });
                }
                _ => {}
            }
        }
        if !admitted {
            continue;
        }
        if event.kind == "form.created" {
            return Err(HarnessError::Protocol(
                "OpenCode requested a form that Shepherd cannot display".into(),
            ));
        }
        if event.kind == "session.execution.failed" {
            let error = &event.data["error"];
            if let Some(message) = error["message"].as_str().or_else(|| error.as_str()) {
                let kind = if error["statusCode"] == 401 {
                    FailureKind::AuthRequired
                } else {
                    FailureKind::from_message(message, FailureKind::AgentRejected)
                };
                publish(
                    sender,
                    HarnessEvent::TurnError {
                        kind,
                        text: message.to_owned(),
                    },
                )?;
            }
        }
        let Some(event) = transcript.apply(event)? else {
            continue;
        };
        match &event {
            HarnessEvent::ApprovalRequested { request } => {
                approvals.insert(request.approval_id.clone());
            }
            HarnessEvent::ApprovalResolved { approval_id, .. } => {
                approvals.remove(approval_id);
            }
            HarnessEvent::TurnFinished { .. } => {
                for approval_id in approvals {
                    publish(sender, HarnessEvent::ApprovalWithdrawn { approval_id })?;
                }
                return Ok(event);
            }
            _ => {}
        }
        publish(sender, event)?;
    }
}

/// A loopback server owned by one Shepherd task.
pub struct OpenCodeServer {
    child: Arc<AsyncMutex<Child>>,
    client: Client,
    address: Url,
    password: String,
    directory: String,
    probe: OpenCodeProbe,
    stderr: Arc<Mutex<VecDeque<String>>>,
}

#[derive(Debug, Deserialize)]
pub struct OpenCodeEvent {
    #[serde(rename = "type")]
    pub kind: String,
    pub data: Value,
}

/// Maps one OpenCode turn to Shepherd transcript events.
pub struct OpenCodeTranscript {
    session: SessionId,
    fragments: BTreeMap<String, String>,
    tools: BTreeMap<String, String>,
}

impl OpenCodeTranscript {
    pub fn new(session: SessionId) -> Self {
        Self {
            session,
            fragments: BTreeMap::new(),
            tools: BTreeMap::new(),
        }
    }

    pub fn apply(&mut self, event: OpenCodeEvent) -> HarnessResult<Option<HarnessEvent>> {
        if event.data["sessionID"].as_str() != Some(self.session.as_str()) {
            return Ok(None);
        }
        let mapped = match event.kind.as_str() {
            "session.text.delta" | "session.text.ended" => {
                return self.text(event, DeltaKind::Message);
            }
            "session.reasoning.delta" | "session.reasoning.ended" => {
                return self.text(event, DeltaKind::Thought);
            }
            "session.tool.input.started" => self.tool(event, ToolStatus::Pending)?,
            "session.tool.called" => self.tool(event, ToolStatus::Running)?,
            "session.tool.success" => self.tool(event, ToolStatus::Completed)?,
            "session.tool.failed" => self.tool(event, ToolStatus::Failed)?,
            "permission.asked" => HarnessEvent::ApprovalRequested {
                request: event.decode::<PermissionRequest>()?.into_approval(),
            },
            "permission.replied" => {
                let reply: PermissionReply = event.decode()?;
                HarnessEvent::ApprovalResolved {
                    approval_id: ApprovalId::new(reply.request_id),
                    decision: match reply.reply {
                        OpenCodeDecision::Once | OpenCodeDecision::Always => Decision::Allow,
                        OpenCodeDecision::Reject => Decision::Deny,
                    },
                    source: crate::ApprovalSource::Agent,
                }
            }
            "session.execution.succeeded" => HarnessEvent::TurnFinished {
                reason: TurnFinishReason::Completed,
            },
            "session.execution.failed" => HarnessEvent::TurnFinished {
                reason: TurnFinishReason::Failed,
            },
            "session.execution.interrupted" => HarnessEvent::TurnFinished {
                reason: if event.data["reason"] == "user" {
                    TurnFinishReason::Cancelled
                } else {
                    TurnFinishReason::Interrupted
                },
            },
            _ => return Ok(None),
        };
        Ok(Some(mapped))
    }

    fn text(
        &mut self,
        event: OpenCodeEvent,
        kind: DeltaKind,
    ) -> HarnessResult<Option<HarnessEvent>> {
        let stream = match kind {
            DeltaKind::Message => "text",
            DeltaKind::Thought => "reasoning",
        };
        let is_delta = event.kind.ends_with(".delta");
        let part: TextPart = event.decode()?;
        let id = format!("{}:{stream}:{}", part.message_id, part.ordinal);
        let text = if is_delta {
            let delta = part.delta.ok_or_else(|| {
                HarnessError::Protocol(format!("OpenCode {stream} delta is missing"))
            })?;
            self.fragments
                .entry(id.clone())
                .or_default()
                .push_str(&delta);
            delta
        } else {
            let text = part.text.ok_or_else(|| {
                HarnessError::Protocol(format!("OpenCode final {stream} is missing"))
            })?;
            let prefix = self.fragments.remove(&id).unwrap_or_default();
            // The final event repeats the fragments already delivered live.
            text.strip_prefix(&prefix)
                .ok_or_else(|| {
                    HarnessError::Protocol(format!(
                        "OpenCode final {stream} changed streamed content"
                    ))
                })?
                .to_owned()
        };
        Ok((!text.is_empty()).then_some(HarnessEvent::Delta {
            item_id: ItemId::new(id),
            kind,
            text,
        }))
    }

    fn tool(&mut self, event: OpenCodeEvent, status: ToolStatus) -> HarnessResult<HarnessEvent> {
        let tool: ToolUpdate = event.decode()?;
        let id = tool.source.key();
        if let Some(name) = tool.name {
            self.tools.insert(id.clone(), name);
        }
        let name = self.tools.get(&id).map_or("Tool", String::as_str);
        let title = tool.metadata["title"].as_str().unwrap_or(name).to_owned();
        let output = tool
            .content
            .iter()
            .filter(|part| part["type"] == "text")
            .filter_map(|part| part["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let output = if output.is_empty() {
            tool.error["message"].as_str().map(str::to_owned)
        } else {
            Some(output)
        };
        Ok(HarnessEvent::ToolCall {
            tool_call_id: ToolCallId::new(id),
            title,
            kind: name.to_owned(),
            status,
            output,
            diffs: Vec::new(),
            terminal_id: None,
        })
    }
}

impl OpenCodeEvent {
    fn decode<T: DeserializeOwned>(self) -> HarnessResult<T> {
        serde_json::from_value(self.data).map_err(|error| {
            protocol_error(
                &format!("could not parse OpenCode {} event", self.kind),
                error,
            )
        })
    }
}

#[derive(Deserialize)]
struct TextPart {
    #[serde(rename = "assistantMessageID")]
    message_id: String,
    ordinal: u64,
    delta: Option<String>,
    text: Option<String>,
}

#[derive(Deserialize)]
struct ToolSource {
    #[serde(rename = "assistantMessageID", alias = "messageID")]
    message_id: String,
    id: String,
}

impl ToolSource {
    fn key(&self) -> String {
        format!("{}:{}", self.message_id, self.id)
    }
}

#[derive(Deserialize)]
struct ToolUpdate {
    #[serde(flatten)]
    source: ToolSource,
    name: Option<String>,
    #[serde(default)]
    content: Vec<Value>,
    #[serde(default)]
    metadata: Value,
    #[serde(default)]
    error: Value,
}

#[derive(Deserialize)]
struct PermissionRequest {
    id: String,
    action: String,
    resources: Vec<String>,
    #[serde(default)]
    metadata: Value,
    source: Option<ToolSource>,
    message: Option<String>,
}

impl PermissionRequest {
    fn into_approval(self) -> ApprovalRequest {
        let tool = self
            .source
            .map_or_else(|| self.id.clone(), |source| source.key());
        ApprovalRequest {
            approval_id: ApprovalId::new(self.id),
            tool_call_id: ToolCallId::new(tool),
            title: self.message.unwrap_or(self.action),
            arguments: json!({"resources": self.resources, "metadata": self.metadata}),
            paths: Vec::new(),
            patch: None,
            choices: None,
        }
    }
}

#[derive(Deserialize)]
struct PermissionReply {
    #[serde(rename = "requestID")]
    request_id: String,
    reply: OpenCodeDecision,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum OpenCodeDecision {
    Once,
    Always,
    Reject,
}

pub struct OpenCodeEvents {
    response: Response,
    decoder: EventDecoder,
    pending: bytes::Bytes,
}

impl OpenCodeEvents {
    pub async fn next(&mut self) -> HarnessResult<OpenCodeEvent> {
        loop {
            if !self.pending.is_empty() {
                let (consumed, event) = self.decoder.push(&self.pending)?;
                self.pending = self.pending.slice(consumed..);
                if let Some(event) = event {
                    return Ok(event);
                }
            }
            self.pending = self
                .response
                .chunk()
                .await
                .map_err(|error| {
                    HarnessError::TransportClosed(format!(
                        "Could not read the OpenCode event stream: {}",
                        error.without_url()
                    ))
                })?
                .ok_or_else(|| {
                    HarnessError::TransportClosed("The OpenCode event stream disconnected.".into())
                })?;
        }
    }
}

#[derive(Default)]
struct EventDecoder {
    line: Vec<u8>,
    data: Vec<u8>,
}

impl EventDecoder {
    fn push(&mut self, chunk: &[u8]) -> HarnessResult<(usize, Option<OpenCodeEvent>)> {
        let mut consumed = 0;
        for fragment in chunk.split_inclusive(|byte| *byte == b'\n') {
            consumed += fragment.len();
            if self.line.len().saturating_add(fragment.len()) > MAX_RESPONSE_BYTES {
                return Err(HarnessError::Protocol(
                    "OpenCode event line is too large".into(),
                ));
            }
            self.line.extend_from_slice(fragment);
            if !self.line.ends_with(b"\n") {
                continue;
            }
            let line = self.line.strip_suffix(b"\n").unwrap_or(&self.line);
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if line.is_empty() && !self.data.is_empty() {
                let event = serde_json::from_slice(&self.data).map_err(|error| {
                    protocol_error("could not parse OpenCode stream event", error)
                })?;
                self.data.clear();
                self.line.clear();
                return Ok((consumed, Some(event)));
            } else if let Some(data) = line.strip_prefix(b"data:") {
                let data = data.strip_prefix(b" ").unwrap_or(data);
                if self.data.len().saturating_add(data.len()).saturating_add(1) > MAX_RESPONSE_BYTES
                {
                    return Err(HarnessError::Protocol("OpenCode event is too large".into()));
                }
                self.data.extend_from_slice(data);
                self.data.push(b'\n');
            } else if line == b"event: effect/httpapi/stream/failure" {
                return Err(HarnessError::Protocol(
                    "OpenCode reported an event stream failure".into(),
                ));
            }
            self.line.clear();
        }
        Ok((consumed, None))
    }
}

impl OpenCodeServer {
    pub async fn start(spec: &SpawnSpec) -> HarnessResult<Self> {
        require_absolute_executable(Path::new(&spec.program))?;
        let directory = tokio::fs::canonicalize(&spec.cwd)
            .await
            .map_err(|error| {
                HarnessError::Protocol(format!("invalid OpenCode directory: {error}"))
            })?
            .into_os_string()
            .into_string()
            .map_err(|_| HarnessError::Protocol("OpenCode requires a UTF-8 directory".into()))?;
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|error| protocol_error("could not configure OpenCode HTTP client", error))?;
        let password = uuid::Uuid::new_v4().to_string();
        let mut child = Command::new(&spec.program)
            .args(&spec.args)
            .args(SERVER_ARGS)
            .current_dir(&directory)
            .envs(&spec.environment)
            .env("OPENCODE_PASSWORD", &password)
            .env("OPENCODE_SERVER_PASSWORD", &password)
            .env("OPENCODE_DISABLE_AUTOUPDATE", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|source| HarnessError::Spawn {
                program: spec.program.clone(),
                source,
            })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            HarnessError::Protocol("OpenCode did not provide a startup pipe".into())
        })?;
        let stderr = Arc::new(Mutex::new(VecDeque::new()));
        if let Some(mut output) = child.stderr.take() {
            let stderr = stderr.clone();
            let password = password.clone();
            tokio::spawn(async move {
                let mut lines = acp_client::bounded_lines::BoundedLines::default();
                let mut bytes = [0u8; 4096];
                let record = |line: acp_client::bounded_lines::BoundedLine| {
                    let mut tail = match stderr.lock() {
                        Ok(tail) => tail,
                        Err(error) => {
                            tracing::error!(%error, "could not retain OpenCode stderr");
                            error.into_inner()
                        }
                    };
                    if tail.len() == 20 {
                        tail.pop_front();
                    }
                    tail.push_back(sanitize_stderr_line(line, &password));
                };
                loop {
                    match output.read(&mut bytes).await {
                        Ok(0) => break,
                        Ok(count) => {
                            for line in lines.push(&bytes[..count]) {
                                record(line);
                            }
                        }
                        Err(error) => {
                            tracing::debug!(%error, "could not read OpenCode stderr");
                            break;
                        }
                    }
                }
                if let Some(line) = lines.finish() {
                    record(line);
                }
            });
        }
        let mut startup = BufReader::new(stdout).take(64 * 1024).lines();
        let address = timeout(STARTUP_TIMEOUT, async {
            while let Some(line) = startup
                .next_line()
                .await
                .map_err(|error| protocol_error("could not read OpenCode startup", error))?
            {
                let banner = line.trim().trim_start_matches("opencode ");
                if let Some(address) = banner.strip_prefix("server listening on ") {
                    return loopback_address(address);
                }
            }
            Err(HarnessError::Protocol(
                "OpenCode did not announce a loopback server".into(),
            ))
        })
        .await
        .map_err(|_| HarnessError::Protocol("OpenCode server startup timed out".into()))??;
        let mut stdout = startup.into_inner().into_inner();
        tokio::spawn(async move {
            if let Err(error) = tokio::io::copy(&mut stdout, &mut tokio::io::sink()).await {
                tracing::debug!(%error, "could not drain OpenCode stdout");
            }
        });
        let request = |path: &str| {
            client
                .get(format!("{}{path}", address.as_str().trim_end_matches('/')))
                .basic_auth(USERNAME, Some(&password))
        };
        let health = response_json(request("/api/info")).await?;
        let api: Api = response_json(request("/openapi.json")).await?;
        let probe = api.probe(health)?;
        Ok(Self {
            child: Arc::new(AsyncMutex::new(child)),
            client,
            address,
            password,
            directory,
            probe,
            stderr,
        })
    }

    pub fn probe(&self) -> &OpenCodeProbe {
        &self.probe
    }

    fn stderr_tail(&self) -> String {
        stderr_tail(&self.stderr)
    }

    pub async fn subscribe(&self) -> HarnessResult<OpenCodeEvents> {
        let handshake = async {
            let response = self
                .request(Method::GET, "/api/event")
                .header(reqwest::header::ACCEPT, "text/event-stream")
                .send()
                .await
                .and_then(Response::error_for_status)
                .map_err(|error| http_error("could not subscribe to OpenCode events", error))?;
            let content_type = response.headers().get(reqwest::header::CONTENT_TYPE);
            if !content_type
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.split(';').next() == Some("text/event-stream"))
            {
                return Err(HarnessError::Protocol(
                    "OpenCode did not return an event stream".into(),
                ));
            }
            let mut events = OpenCodeEvents {
                response,
                decoder: EventDecoder::default(),
                pending: bytes::Bytes::new(),
            };
            if events.next().await?.kind != "server.connected" {
                return Err(HarnessError::Protocol(
                    "OpenCode did not acknowledge the event stream".into(),
                ));
            }
            Ok(events)
        };
        timeout(REQUEST_TIMEOUT, handshake).await.map_err(|_| {
            HarnessError::Protocol("OpenCode event stream handshake timed out".into())
        })?
    }

    pub async fn create_session(&self) -> HarnessResult<SessionId> {
        let response: ResponseData<SessionRecord> =
            response_json(self.request(Method::POST, "/api/session").json(&json!({
                "location": { "directory": self.directory },
                "permissions": [{ "action": "*", "resource": "*", "effect": "ask" }]
            })))
            .await?;
        self.session_id(&response.data)
    }

    pub async fn resume_session(&self, session: &SessionId) -> HarnessResult<SessionId> {
        if !self.probe.resume {
            return Err(HarnessError::Protocol(
                "OpenCode server does not support session lookup".into(),
            ));
        }
        self.read_session(session).await?;
        Ok(session.clone())
    }

    async fn read_session(&self, session: &SessionId) -> HarnessResult<SessionRecord> {
        let response: ResponseData<SessionRecord> =
            response_json(self.request(Method::GET, &session_path(session)?)).await?;
        if &self.session_id(&response.data)? != session {
            return Err(HarnessError::Protocol(
                "OpenCode returned a different session".into(),
            ));
        }
        Ok(response.data)
    }

    async fn session_modes(&self, session: &SessionId) -> HarnessResult<Option<AgentModes>> {
        let options = self.mode_catalog().await?;
        if options.is_empty() {
            return Ok(None);
        }
        let current = self.read_session(session).await?.agent;
        Ok(Some(AgentModes { current, options }))
    }

    async fn mode_catalog(&self) -> HarnessResult<Vec<AgentMode>> {
        let request = || {
            self.request(Method::GET, "/api/agent")
                .query(&[("location[directory]", self.directory.as_str())])
        };
        let mut agents: OpenCodeAgents = response_json(request()).await?;
        if agents.data.is_empty() {
            // OpenCode can answer the first lookup before the workspace registry is ready.
            tokio::time::sleep(Duration::from_millis(200)).await;
            agents = response_json(request()).await?;
        }
        if agents.location.directory != Path::new(&self.directory) {
            return Err(HarnessError::Protocol(
                "OpenCode returned modes outside the task directory".into(),
            ));
        }
        Ok(agents
            .data
            .into_iter()
            .filter(|agent| !agent.hidden && matches!(agent.mode.as_str(), "primary" | "all"))
            .map(|agent| AgentMode {
                id: agent.id,
                name: agent.name,
                description: agent.description,
            })
            .collect())
    }

    pub async fn interrupt_session(&self, session: &SessionId) -> HarnessResult<bool> {
        let path = format!("{}/interrupt", session_path(session)?);
        let response: InterruptResponse = response_json(self.request(Method::POST, &path)).await?;
        Ok(response.interrupted)
    }

    async fn enqueue_prompt(
        &self,
        session: &SessionId,
        prompt_id: &str,
        input: &PromptInput,
        resume: bool,
    ) -> HarnessResult<()> {
        let path = format!("{}/prompt", session_path(session)?);
        let mut body = json!({"id": prompt_id, "text": input.text, "resume": resume});
        if self.probe.prompt_files && !input.attachments.is_empty() {
            let files = input
                .attachments
                .iter()
                .map(|file| {
                    Ok(json!({
                        "uri": file.file_uri()?.as_str(),
                        "name": file.path.file_name().map(|name| name.to_string_lossy()),
                    }))
                })
                .collect::<HarnessResult<Vec<_>>>()?;
            body["files"] = json!(files);
        }
        let response: ResponseData<Value> =
            response_json(self.request(Method::POST, &path).json(&body)).await?;
        if response.data["id"] != prompt_id || response.data["sessionID"] != session.as_str() {
            return Err(HarnessError::Protocol(
                "OpenCode admitted a different prompt".into(),
            ));
        }
        Ok(())
    }

    async fn cancel_prompt(&self, session: &SessionId, prompt_id: &str) -> HarnessResult<()> {
        let path = format!("{}/inbox/{prompt_id}", session_path(session)?);
        // The prompt may still be queued when Cancel arrives. Deleting a delivered item is a no-op.
        send_request(self.request(Method::DELETE, &path)).await?;
        self.interrupt_session(session).await?;
        Ok(())
    }

    pub async fn shutdown(self) -> HarnessResult<()> {
        self.child
            .lock()
            .await
            .kill()
            .await
            .map_err(|error| protocol_error("could not stop OpenCode server", error))
    }

    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        self.client
            .request(
                method,
                format!("{}{path}", self.address.as_str().trim_end_matches('/')),
            )
            .basic_auth(USERNAME, Some(&self.password))
    }

    fn session_id(&self, record: &SessionRecord) -> HarnessResult<SessionId> {
        if record.location.directory != Path::new(&self.directory) {
            return Err(HarnessError::Protocol(
                "OpenCode returned a session outside the task directory".into(),
            ));
        }
        let session = SessionId::new(record.id.clone());
        session_path(&session)?;
        Ok(session)
    }
}

fn sanitize_stderr_line(line: acp_client::bounded_lines::BoundedLine, password: &str) -> String {
    if line.truncated {
        "[long stderr line omitted]".to_owned()
    } else {
        line.text.replace(password, "[redacted]")
    }
}

fn stderr_tail(stderr: &Mutex<VecDeque<String>>) -> String {
    match stderr.lock() {
        Ok(tail) => tail.iter().cloned().collect::<Vec<_>>().join("\n"),
        Err(error) => {
            tracing::error!(%error, "could not read the OpenCode stderr tail");
            String::new()
        }
    }
}

fn loopback_address(address: &str) -> HarnessResult<Url> {
    let address = Url::parse(address)
        .map_err(|error| protocol_error("OpenCode returned an invalid listener address", error))?;
    if address.scheme() != "http"
        || address.host_str() != Some("127.0.0.1")
        || !address.port().is_some_and(|port| port != 0)
        || !address.username().is_empty()
        || address.password().is_some()
        || address.path() != "/"
        || address.query().is_some()
        || address.fragment().is_some()
    {
        return Err(HarnessError::Protocol(
            "OpenCode did not bind an HTTP loopback port".into(),
        ));
    }
    Ok(address)
}

fn session_path(session: &SessionId) -> HarnessResult<String> {
    let id = session.as_str();
    if !valid_id(id) {
        return Err(HarnessError::Protocol("invalid OpenCode session ID".into()));
    }
    Ok(format!("/api/session/{id}"))
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
}

async fn send_request(request: RequestBuilder) -> HarnessResult<Response> {
    request
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|error| http_error("OpenCode HTTP request failed", error))
}

fn http_error(context: &str, error: reqwest::Error) -> HarnessError {
    if error.status() == Some(reqwest::StatusCode::UNAUTHORIZED) {
        HarnessError::Reported {
            kind: FailureKind::AuthRequired,
            message: format!("{context}: OpenCode returned HTTP 401 Unauthorized"),
        }
    } else {
        protocol_error(context, error.without_url())
    }
}

async fn response_json<T: DeserializeOwned>(request: RequestBuilder) -> HarnessResult<T> {
    let mut response = send_request(request).await?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| protocol_error("could not read OpenCode response", error.without_url()))?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(HarnessError::Protocol(
                "OpenCode response is too large".into(),
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| protocol_error("could not parse OpenCode response", error))
}

fn protocol_error(context: &str, error: impl std::fmt::Display) -> HarnessError {
    HarnessError::Protocol(format!("{context}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn mode_discovery_hides_agent_failure_with_environment()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir()?;
        let script = directory.path().join("opencode");
        std::fs::write(
            &script,
            "#!/bin/sh\nprintf 'server listening on %s\\n' \"$TOKEN\"\n",
        )?;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))?;
        let spec = SpawnSpec {
            program: script.to_string_lossy().into_owned(),
            cwd: directory.path().to_owned(),
            environment: BTreeMap::from([("TOKEN".into(), "fixture-private-value".into())]),
            ..SpawnSpec::default()
        };
        let error = match project_modes(&spec).await {
            Ok(_) => return Err("expected discovery failure".into()),
            Err(error) => error,
        };
        assert!(!error.to_string().contains("fixture-private-value"));
        assert!(error.to_string().contains("hidden"));
        Ok(())
    }

    #[test]
    fn stderr_redaction_omits_truncated_lines_and_hides_credentials()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut lines = acp_client::bounded_lines::BoundedLines::default();
        let normal = lines.push(b"token=secret\n");
        assert_eq!(
            sanitize_stderr_line(normal.into_iter().next().ok_or("missing line")?, "secret"),
            "token=[redacted]"
        );
        let mut long = vec![b'x'; 5000];
        long.extend_from_slice(b"secret\n");
        let truncated = lines.push(&long).into_iter().next().ok_or("missing line")?;
        assert_eq!(
            sanitize_stderr_line(truncated, "secret"),
            "[long stderr line omitted]"
        );
        Ok(())
    }
    use tokio::io::{AsyncRead, AsyncWriteExt};
    use tokio::net::TcpListener;

    const NATIVE_API: &str = r#"{"paths":{
        "/api/event": {"get": {}},
        "/api/session": {"post": {}},
        "/api/session/{sessionID}": {"get": {}},
        "/api/session/{sessionID}/prompt": {"post": {}},
        "/api/session/{sessionID}/inbox/{inboxID}": {"delete": {}},
        "/api/session/{sessionID}/interrupt": {"post": {}},
        "/api/session/{sessionID}/permission/{requestID}/reply": {"post": {}}
    }}"#;

    #[test]
    fn reads_an_explicit_opencode_executable() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let settings = directory.path().join("settings.json");
        let executable = directory.path().join("profile/bin/opencode");
        assert_eq!(configured_executable(&settings)?, None);
        std::fs::write(
            &settings,
            serde_json::to_vec(&json!({
                "version": 1,
                "app": { "agents.opencode.executable": executable, "theme": "dark" }
            }))?,
        )?;
        assert_eq!(configured_executable(&settings)?, Some(executable));
        let launch = configured_launch(&AgentSettings::read(&settings)?)?
            .ok_or("expected a configured launch")?;
        assert!(!launch.environment.contains_key("OPENCODE_CONFIG"));
        let invalid: AgentSettings = serde_json::from_value(json!({"app": {
            "agents.opencode.executable": launch.program,
            "agents.opencode.config_file": "relative.json"
        }}))?;
        assert!(configured_launch(&invalid).is_err());
        std::fs::write(
            &settings,
            r#"{"app":{"agents.opencode.executable":"opencode"}}"#,
        )?;
        assert!(configured_executable(&settings).is_err());
        Ok(())
    }

    #[test]
    fn requires_the_server_to_advertise_permission_replies()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut api: Api = serde_json::from_str(NATIVE_API)?;
        let permission_path = "/api/session/{sessionID}/permission/{requestID}/reply";
        let permission = api
            .paths
            .remove(permission_path)
            .ok_or("expected permission API")?;
        let session_path = "/api/session/{sessionID}";
        let session = api
            .paths
            .remove(session_path)
            .ok_or("expected session lookup API")?;
        let health = || Health {
            version: "fixture".into(),
        };
        let error = api
            .probe(health())
            .expect_err("permission handling is required");
        assert!(error.to_string().contains("/permission/{requestID}/reply"));
        api.paths.insert(permission_path.into(), permission);
        assert!(!api.probe(health())?.resume);
        api.paths.insert(session_path.into(), session);
        assert!(api.probe(health())?.resume);
        assert!(!api.probe(health())?.prompt_files);
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn native_http_contract_requires_prompt_admission_before_streaming()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let directory = root.path().canonicalize()?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let program = fixture_launcher(
            &directory,
            r#"#!/bin/sh
[ "$*" = "serve --hostname 127.0.0.1 --port 0" ] || exit 2
printf 'opencode server listening on %s\n' "$SHEPHERD_TEST_OPENCODE_URL"
exec sleep 60
"#,
        )?;
        let spec = SpawnSpec {
            program,
            cwd: directory.clone(),
            environment: BTreeMap::from([(
                "SHEPHERD_TEST_OPENCODE_URL".into(),
                format!("http://{}", listener.local_addr()?),
            )]),
            ..SpawnSpec::default()
        };
        let fixture = serve_native_fixture(listener, &directory);
        let client = async {
            let mut harness = OpenCodeHarness::default();
            let capabilities = harness.start(spec).await?;
            assert_eq!(harness.tier(), AdapterTier::Native);
            assert!(capabilities.streaming && capabilities.resume);
            assert_eq!(harness.open_session(None).await?.as_str(), "ses_fixture");
            let transcript = fixture_reply(&mut harness).await?;
            assert_eq!(transcript.len(), 1);
            assert_eq!(
                transcript.entry(0).map(Entry::summary).as_deref(),
                Some("agent: native fixture reply")
            );
            harness.shutdown().await?;
            Ok::<_, Box<dyn std::error::Error>>(())
        };
        timeout(REQUEST_TIMEOUT, async { tokio::try_join!(fixture, client) }).await??;
        Ok(())
    }

    #[cfg(unix)]
    async fn serve_native_fixture(
        listener: TcpListener,
        directory: &Path,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut stream = None;
        for expected in [
            "GET /api/info",
            "GET /openapi.json",
            "GET /api/event",
            "POST /api/session",
            "GET /api/event",
            "POST /api/session/ses_fixture/prompt",
        ] {
            let (socket, _) = listener.accept().await?;
            let (reader, mut writer) = socket.into_split();
            let (request, body) = read_fixture_request(reader).await?;
            assert_eq!(request, format!("{expected} HTTP/1.1"));
            let response = match expected {
                "GET /api/info" => json!({"version": "fixture"}).to_string(),
                "GET /openapi.json" => NATIVE_API.to_owned(),
                "GET /api/event" => {
                    writer.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"type\":\"server.connected\",\"data\":{}}\n\n").await?;
                    stream = Some(writer);
                    continue;
                }
                "POST /api/session" => {
                    let body: Value = serde_json::from_slice(&body)?;
                    assert_eq!(
                        body,
                        json!({"location": {"directory": directory}, "permissions": [{"action": "*", "resource": "*", "effect": "ask"}]})
                    );
                    json!({"data": {"id": "ses_fixture", "location": {"directory": directory}}})
                        .to_string()
                }
                "POST /api/session/ses_fixture/prompt" => {
                    let body: Value = serde_json::from_slice(&body)?;
                    let prompt_id = body["id"].as_str().ok_or("expected prompt ID")?;
                    assert_eq!(body["text"], "Reply with local fixture text only");
                    assert_eq!(body["resume"], true);
                    let stream = stream.as_mut().ok_or("expected an event stream")?;
                    let delta = |session, delta| json!({"sessionID": session, "assistantMessageID": "msg_fixture", "ordinal": 0, "delta": delta});
                    for (kind, data) in [
                        (
                            "session.inbox.enqueued",
                            json!({"sessionID": "ses_fixture", "inboxID": "msg_previous"}),
                        ),
                        ("session.text.delta", delta("ses_fixture", "prior turn")),
                        (
                            "session.inbox.enqueued",
                            json!({"sessionID": "ses_fixture", "inboxID": prompt_id}),
                        ),
                        ("session.text.delta", delta("ses_other", "other session")),
                        (
                            "session.text.delta",
                            delta("ses_fixture", "native fixture reply"),
                        ),
                        (
                            "session.execution.succeeded",
                            json!({"sessionID": "ses_fixture"}),
                        ),
                    ] {
                        let event = json!({"type": kind, "data": data});
                        stream
                            .write_all(format!("data: {event}\n\n").as_bytes())
                            .await?;
                    }
                    json!({"data": {"id": prompt_id, "sessionID": "ses_fixture"}}).to_string()
                }
                _ => return Err("unexpected fixture request".into()),
            };
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                response.len()
            );
            writer.write_all(header.as_bytes()).await?;
            writer.write_all(response.as_bytes()).await?;
            writer.shutdown().await?;
        }
        Ok(())
    }

    #[test]
    fn decodes_split_event_lines_and_preserves_events_before_failure()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut decoder = EventDecoder::default();
        let mut events = Vec::new();
        for byte in ": heartbeat\n\r\ndata: {\"type\":\"session.text.delta\",\r\n\
                     data: \"data\":{\"delta\":\"héllo\"}}\r\n\r\n"
            .as_bytes()
            .chunks(1)
        {
            let (_, event) = decoder.push(byte)?;
            events.extend(event);
        }
        let input = b"data: {\"type\":\"session.execution.succeeded\",\"data\":{}}\n\n\
              event: effect/httpapi/stream/failure\n";
        let (consumed, event) = decoder.push(input)?;
        events.extend(event);
        assert!(decoder.push(&input[consumed..]).is_err());
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, "session.text.delta");
        assert_eq!(events[0].data, json!({"delta": "héllo"}));
        assert_eq!(events[1].kind, "session.execution.succeeded");
        Ok(())
    }

    #[test]
    fn decodes_one_event_at_a_time_from_a_large_chunk() -> Result<(), Box<dyn std::error::Error>> {
        let mut decoder = EventDecoder::default();
        let input =
            b"data: {\"type\":\"session.execution.succeeded\",\"data\":{}}\n\n".repeat(1000);
        let mut remaining = input.as_slice();
        let mut count = 0;
        while !remaining.is_empty() {
            let (consumed, event) = decoder.push(remaining)?;
            assert!(consumed > 0);
            assert_eq!(
                event.ok_or("expected an event")?.kind,
                "session.execution.succeeded"
            );
            remaining = &remaining[consumed..];
            count += 1;
        }
        assert_eq!(count, 1000);
        Ok(())
    }

    #[test]
    fn maps_only_the_task_session_and_appends_the_unstreamed_suffix()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut mapping = OpenCodeTranscript::new(SessionId::new("ses_1"));
        let mut transcript = crate::Transcript::default();
        for (kind, data) in [
            (
                "session.text.delta",
                json!({
                    "sessionID": "ses_other", "assistantMessageID": "msg_1", "ordinal": 0, "delta": "other"
                }),
            ),
            (
                "session.text.delta",
                json!({
                    "sessionID": "ses_1", "assistantMessageID": "msg_1", "ordinal": 0, "delta": "héllo"
                }),
            ),
            (
                "session.reasoning.ended",
                json!({
                    "sessionID": "ses_1", "assistantMessageID": "msg_1", "ordinal": 0, "text": "considered"
                }),
            ),
            (
                "session.text.ended",
                json!({
                    "sessionID": "ses_1", "assistantMessageID": "msg_1", "ordinal": 0, "text": "héllo world"
                }),
            ),
        ] {
            if let Some(event) = mapping.apply(OpenCodeEvent {
                kind: kind.into(),
                data,
            })? {
                transcript.apply(&event);
            }
        }
        assert_eq!(transcript.len(), 2);
        assert_eq!(
            transcript.entry(0).map(crate::Entry::summary).as_deref(),
            Some("agent: héllo world")
        );
        assert_eq!(
            transcript.entry(1).map(crate::Entry::summary).as_deref(),
            Some("thought: considered")
        );
        Ok(())
    }

    #[test]
    fn maps_one_tool_id_across_execution_and_approval() -> Result<(), Box<dyn std::error::Error>> {
        let mut mapping = OpenCodeTranscript::new(SessionId::new("ses_1"));
        let mut apply = |kind: &str, data| {
            mapping.apply(OpenCodeEvent {
                kind: kind.into(),
                data,
            })
        };
        let started = apply(
            "session.tool.input.started",
            json!({
                "sessionID": "ses_1", "assistantMessageID": "msg_1", "id": "call_1", "name": "bash"
            }),
        )?;
        assert!(
            matches!(started, Some(HarnessEvent::ToolCall { tool_call_id, status: ToolStatus::Pending, .. }) if tool_call_id.as_str() == "msg_1:call_1")
        );
        let approval = apply(
            "permission.asked",
            json!({
                "sessionID": "ses_1", "id": "per_1", "action": "bash", "resources": ["cargo test"],
                "source": { "type": "tool", "messageID": "msg_1", "id": "call_1" }
            }),
        )?;
        assert!(
            matches!(approval, Some(HarnessEvent::ApprovalRequested { request }) if
            request.tool_call_id.as_str() == "msg_1:call_1" && request.approval_id.as_str() == "per_1")
        );
        let replied = apply(
            "permission.replied",
            json!({
                "sessionID": "ses_1", "requestID": "per_1", "reply": "once"
            }),
        )?;
        assert!(matches!(
            replied,
            Some(HarnessEvent::ApprovalResolved {
                decision: Decision::Allow,
                ..
            })
        ));
        let finished = apply(
            "session.tool.success",
            json!({
                "sessionID": "ses_1", "assistantMessageID": "msg_1", "id": "call_1",
                "content": [{ "type": "text", "text": "passed" }]
            }),
        )?;
        assert!(
            matches!(finished, Some(HarnessEvent::ToolCall { tool_call_id, title, status: ToolStatus::Completed, output: Some(output), .. }) if
            tool_call_id.as_str() == "msg_1:call_1" && title == "bash" && output == "passed")
        );
        Ok(())
    }

    #[cfg(feature = "live-opencode")]
    #[tokio::test]
    async fn installed_server_creates_and_resumes_a_session()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let spec = installed_fixture_spec(root.path())?;
        let server = OpenCodeServer::start(&spec).await?;
        assert!(server.probe().resume);
        let mut events = server.subscribe().await?;
        let session = server.create_session().await?;
        let created = timeout(REQUEST_TIMEOUT, async {
            loop {
                let event = events.next().await?;
                if event.kind == "session.created" {
                    return Ok::<_, HarnessError>(event);
                }
            }
        })
        .await??;
        assert_eq!(created.data["sessionID"], session.as_str());
        server.shutdown().await?;

        let restarted = OpenCodeServer::start(&spec).await?;
        assert_eq!(restarted.resume_session(&session).await?, session);
        assert!(!restarted.interrupt_session(&session).await?);
        let events = restarted.subscribe().await?;
        let prompt_id = format!("msg_{}", uuid::Uuid::new_v4().simple());
        restarted
            .enqueue_prompt(
                &session,
                &prompt_id,
                &PromptInput::text("queued fixture"),
                false,
            )
            .await?;
        restarted.cancel_prompt(&session, &prompt_id).await?;
        let (sender, _receiver) = mpsc::unbounded_channel();
        let terminal = timeout(
            REQUEST_TIMEOUT,
            read_turn(events, session, &prompt_id, &sender),
        )
        .await??;
        assert!(matches!(
            terminal,
            HarnessEvent::TurnFinished {
                reason: TurnFinishReason::Cancelled
            }
        ));
        restarted.shutdown().await?;
        Ok(())
    }

    #[cfg(feature = "live-opencode")]
    #[tokio::test]
    async fn installed_harness_switches_mode_and_reads_it_on_reconnect()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let spec = installed_fixture_spec(root.path())?;
        let draft_modes = project_modes(&spec).await?;
        let mode = draft_modes
            .iter()
            .find(|option| option.id == "plan")
            .ok_or("expected Plan before creating a session")?;
        let mut harness = OpenCodeHarness::default();
        harness.start(spec.clone()).await?;
        assert_eq!(harness.tier(), AdapterTier::Native);
        let session = harness.open_session(None).await?;
        let server = harness.server.as_ref().ok_or("missing native server")?;
        assert!(server.probe().modes);
        assert!(
            harness
                .modes()
                .ok_or("missing native modes")?
                .options
                .contains(mode)
        );
        assert!(matches!(
            harness.set_mode("missing-fixture-mode").await,
            Err(HarnessError::ModeUnavailable)
        ));
        harness.set_mode(&mode.id).await?;
        assert_eq!(
            harness
                .modes()
                .ok_or("missing selected mode")?
                .current
                .as_deref(),
            Some(mode.id.as_str())
        );
        harness.shutdown().await?;

        let mut reopened = OpenCodeHarness::default();
        reopened.start(spec).await?;
        assert_eq!(reopened.open_session(Some(session.clone())).await?, session);
        assert_eq!(
            reopened
                .modes()
                .ok_or("missing restored mode")?
                .current
                .as_deref(),
            Some(mode.id.as_str())
        );
        reopened.shutdown().await?;
        Ok(())
    }

    #[cfg(feature = "live-opencode")]
    #[tokio::test]
    async fn installed_harness_streams_a_turn_through_a_local_fixture()
    -> Result<(), Box<dyn std::error::Error>> {
        installed_harness_with_fixture_provider(FixtureModel::Text).await
    }

    #[cfg(feature = "live-opencode")]
    #[tokio::test]
    async fn installed_harness_delivers_an_image_to_the_local_provider()
    -> Result<(), Box<dyn std::error::Error>> {
        use base64::{Engine as _, engine::general_purpose::STANDARD};

        let root = tempfile::tempdir()?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let spec = installed_provider_spec(root.path(), &listener)?;
        let path = root.path().join("a screenshot.png");
        std::fs::write(&path, STANDARD.decode(FIXTURE_PNG)?)?;
        let provider = tokio::spawn(serve_fixture_model(listener, FixtureModel::Image));
        let outcome = async {
            let mut harness = OpenCodeHarness::default();
            assert!(harness.start(spec).await?.images);
            assert_eq!(harness.tier(), AdapterTier::Native);
            harness.open_session(None).await?;
            harness
                .prompt(PromptInput {
                    text: "Inspect the attached image.".into(),
                    attachments: vec![crate::PromptAttachment {
                        path,
                        image_mime_type: Some("image/png".into()),
                        metadata: None,
                    }],
                    ..PromptInput::default()
                })
                .await?;
            let events = fixture_outcome(&mut harness, TurnFinishReason::Completed).await?;
            harness.shutdown().await?;
            Ok::<_, HarnessError>(events)
        }
        .await;
        provider.abort();
        let mut transcript = crate::Transcript::default();
        for event in outcome? {
            transcript.apply(&event);
        }
        assert_eq!(
            transcript.entry(0).map(crate::Entry::summary).as_deref(),
            Some("agent: received fixture image")
        );
        Ok(())
    }

    #[cfg(feature = "live-opencode")]
    #[tokio::test]
    async fn installed_harness_reports_a_provider_sign_in_failure()
    -> Result<(), Box<dyn std::error::Error>> {
        installed_harness_with_fixture_provider(FixtureModel::Unauthorized).await
    }

    #[cfg(feature = "live-opencode")]
    fn installed_provider_spec(
        root: &Path,
        listener: &TcpListener,
    ) -> Result<SpawnSpec, Box<dyn std::error::Error>> {
        let mut spec = installed_fixture_spec(root)?;
        let endpoint = format!("http://{}/v1", listener.local_addr()?);
        let config = root.join("fixture config.json");
        std::fs::write(
            &config,
            serde_json::to_vec(&json!({
                "model": "shepherd_fixture/coder",
                "providers": {
                    "shepherd_fixture": {
                        "name": "Local Shepherd fixture",
                        "package": "@opencode/ai/providers/openai-compatible",
                        "settings": { "baseURL": endpoint },
                        "models": {
                            "coder": {
                                "modelID": "coder",
                                "capabilities": { "tools": true, "input": ["text", "image"], "output": ["text"] },
                                "limit": { "context": 32768, "output": 8192 }
                            }
                        }
                    }
                }
            }))?,
        )?;
        let settings = serde_json::from_value(json!({"app": {
            "agents.opencode.executable": spec.program,
            "agents.opencode.config_file": config
        }}))?;
        let launch = configured_launch(&settings)?.ok_or("expected a configured launch")?;
        spec.environment.extend(launch.environment);
        Ok(spec)
    }

    #[cfg(feature = "live-opencode")]
    async fn installed_harness_with_fixture_provider(
        model: FixtureModel,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let spec = installed_provider_spec(root.path(), &listener)?;
        let provider = tokio::spawn(serve_fixture_model(listener, model));
        let unauthorized = matches!(model, FixtureModel::Unauthorized);
        let outcome = async {
            let mut harness = OpenCodeHarness::default();
            harness.start(spec).await?;
            assert_eq!(harness.tier(), AdapterTier::Native);
            harness.open_session(None).await?;
            let transcript = if unauthorized {
                harness
                    .prompt(PromptInput::text("test local sign-in failure"))
                    .await?;
                let mut transcript = crate::Transcript::default();
                for event in fixture_outcome(&mut harness, TurnFinishReason::Failed).await? {
                    transcript.apply(&event);
                }
                transcript
            } else {
                fixture_reply(&mut harness).await?
            };
            harness.shutdown().await?;
            Ok::<_, HarnessError>(transcript)
        }
        .await;
        provider.abort();
        let transcript = outcome?;
        if unauthorized {
            assert!(
                transcript
                    .entries()
                    .iter()
                    .any(|entry| entry.summary().contains("sign-in required")),
                "{:?}",
                transcript.entries()
            );
        } else {
            assert_eq!(transcript.len(), 1);
            assert_eq!(
                transcript.entry(0).map(crate::Entry::summary).as_deref(),
                Some("agent: local fixture reply")
            );
        }
        Ok(())
    }

    #[cfg(feature = "live-opencode")]
    #[tokio::test]
    async fn installed_harness_approves_and_cancels_after_resuming()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let spec = installed_provider_spec(root.path(), &listener)?;
        let provider = tokio::spawn(serve_fixture_model(listener, FixtureModel::Shell));
        let outcome = async {
            let mut first = OpenCodeHarness::default();
            assert!(first.start(spec.clone()).await?.resume);
            let session = first.open_session(None).await?;
            first.shutdown().await?;

            let mut reopened = OpenCodeHarness::default();
            let capabilities = reopened.start(spec).await?;
            assert_eq!(reopened.tier(), AdapterTier::Native);
            assert!(capabilities.approvals && capabilities.cancel);
            assert_eq!(reopened.open_session(Some(session.clone())).await?, session);

            for (decision, outcome) in [
                (Decision::Allow, TurnFinishReason::Completed),
                (Decision::Deny, TurnFinishReason::Interrupted),
            ] {
                reopened
                    .prompt(PromptInput::text("Run the approval fixture"))
                    .await?;
                let approval = fixture_approval(&mut reopened).await?;
                reopened.respond(approval.clone(), decision).await?;
                let events = fixture_outcome(&mut reopened, outcome).await?;
                assert!(
                    events
                        .iter()
                        .any(|event| matches!(event, HarnessEvent::ApprovalResolved {
                    approval_id, decision: resolved, ..
                } if *approval_id == approval && *resolved == decision)),
                    "{events:?}"
                );
                let marker = events.iter().any(|event| {
                    matches!(event, HarnessEvent::ToolCall {
                    status: ToolStatus::Completed, output: Some(output), ..
                } if output.contains("shepherd_approval_fixture"))
                });
                assert_eq!(marker, decision == Decision::Allow, "{events:?}");
            }

            reopened
                .prompt(PromptInput::text("Run the cancellation fixture"))
                .await?;
            let pending = fixture_approval(&mut reopened).await?;
            reopened.cancel().await?;
            let events = fixture_outcome(&mut reopened, TurnFinishReason::Cancelled).await?;
            assert!(
                events
                    .iter()
                    .any(|event| matches!(event, HarnessEvent::ApprovalWithdrawn {
                approval_id
            } if *approval_id == pending)),
                "{events:?}"
            );
            assert!(
                !events.iter().any(|event| matches!(
                    event,
                    HarnessEvent::ToolCall {
                        status: ToolStatus::Completed,
                        ..
                    }
                )),
                "{events:?}"
            );
            reopened.shutdown().await?;
            Ok::<_, Box<dyn std::error::Error>>(())
        }
        .await;
        provider.abort();
        outcome
    }

    #[cfg(feature = "live-opencode")]
    async fn fixture_approval(harness: &mut OpenCodeHarness) -> HarnessResult<ApprovalId> {
        timeout(REQUEST_TIMEOUT, async {
            while let Some(event) = harness.events().recv().await {
                match event {
                    HarnessEvent::ApprovalRequested { request } => {
                        assert!(
                            request
                                .arguments
                                .to_string()
                                .contains("shepherd_approval_fixture"),
                            "{request:?}"
                        );
                        return Ok(request.approval_id);
                    }
                    HarnessEvent::TurnFinished { .. }
                    | HarnessEvent::ConnectionFailed { .. }
                    | HarnessEvent::Exited { .. } => {
                        return Err(protocol_error(
                            "fixture ended before requesting approval",
                            format_args!("{event:?}"),
                        ));
                    }
                    _ => {}
                }
            }
            Err(HarnessError::ChannelClosed)
        })
        .await
        .map_err(|error| protocol_error("fixture approval timed out", error))?
    }

    #[cfg(feature = "live-opencode")]
    async fn fixture_outcome(
        harness: &mut OpenCodeHarness,
        expected: TurnFinishReason,
    ) -> HarnessResult<Vec<HarnessEvent>> {
        let mut events = Vec::new();
        timeout(REQUEST_TIMEOUT, async {
            while let Some(event) = harness.events().recv().await {
                match event {
                    HarnessEvent::TurnFinished { reason } => {
                        assert_eq!(reason, expected, "{events:?}");
                        return Ok(events);
                    }
                    HarnessEvent::ConnectionFailed { .. } | HarnessEvent::Exited { .. } => {
                        return Err(protocol_error(
                            "fixture connection closed",
                            format_args!("{event:?}"),
                        ));
                    }
                    _ => events.push(event),
                }
            }
            Err(HarnessError::ChannelClosed)
        })
        .await
        .map_err(|error| protocol_error("fixture turn timed out", error))?
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn falls_back_to_acp_and_streams_a_turn() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let program = fixture_launcher(
            root.path(),
            r#"#!/bin/sh
if [ "$1" != acp ]; then exit 78; fi
[ "$OPENCODE_CONFIG" = "$PWD/fixture config.json" ] || exit 79
while IFS= read -r record; do
    request_id=$(printf '%s\n' "$record" | sed -nE 's/.*"id":("[^"]+"|[0-9]+).*/\1/p')
    case "$record" in
        *'"method":"initialize"'*)
            printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":1}}\n' "$request_id";;
        *'"method":"session/new"'*)
            printf '{"jsonrpc":"2.0","id":%s,"result":{"sessionId":"fixture"}}\n' "$request_id";;
        *'"method":"session/prompt"'*)
            printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"fixture","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"ACP fixture reply"}}}}'
            printf '{"jsonrpc":"2.0","id":%s,"result":{"stopReason":"end_turn"}}\n' "$request_id";;
    esac
done
"#,
        )?;
        let cwd = root.path().canonicalize()?;
        let settings = serde_json::from_value(json!({"app": {
            "agents.opencode.executable": program,
            "agents.opencode.config_file": cwd.join("fixture config.json")
        }}))?;
        let spec = SpawnSpec {
            cwd,
            ..configured_launch(&settings)?.ok_or("expected a configured launch")?
        };
        let mut harness = OpenCodeHarness::default();
        harness.start(spec).await?;
        assert_eq!(harness.tier(), AdapterTier::Acp);
        let diagnostics = harness.diagnostics();
        assert!(diagnostics.command.ends_with(" acp"));
        assert!(
            diagnostics
                .fallback_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("native server unavailable"))
        );
        assert_eq!(harness.open_session(None).await?.as_str(), "fixture");
        let transcript = fixture_reply(&mut harness).await?;
        assert_eq!(
            transcript.entry(0).map(crate::Entry::summary).as_deref(),
            Some("agent: ACP fixture reply")
        );
        harness.shutdown().await?;
        Ok(())
    }

    #[cfg(all(unix, feature = "live-opencode"))]
    #[tokio::test]
    async fn installed_harness_falls_back_to_acp_and_loads_a_session()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let mut spec = installed_fixture_spec(root.path())?;
        let launcher = fixture_launcher(
            root.path(),
            "#!/bin/sh\nif [ \"$1\" = serve ]; then exit 78; fi\nexec \"$SHEPHERD_OPENCODE_FIXTURE_EXEC\" \"$@\"\n",
        )?;
        let program = std::mem::replace(&mut spec.program, launcher);
        spec.environment
            .insert("SHEPHERD_OPENCODE_FIXTURE_EXEC".into(), program);
        let mut first = OpenCodeHarness::default();
        assert!(first.start(spec.clone()).await?.resume);
        assert_eq!(first.tier(), AdapterTier::Acp);
        let session = first.open_session(None).await?;
        first.shutdown().await?;
        let mut reopened = OpenCodeHarness::default();
        reopened.start(spec).await?;
        assert_eq!(reopened.open_session(Some(session.clone())).await?, session);
        reopened.shutdown().await?;
        Ok(())
    }

    async fn fixture_reply(harness: &mut OpenCodeHarness) -> HarnessResult<crate::Transcript> {
        harness
            .prompt(PromptInput::text("Reply with local fixture text only"))
            .await?;
        let mut transcript = crate::Transcript::default();
        timeout(REQUEST_TIMEOUT, async {
            loop {
                let event = harness
                    .events()
                    .recv()
                    .await
                    .ok_or(HarnessError::ChannelClosed)?;
                transcript.apply(&event);
                match event {
                    HarnessEvent::TurnFinished {
                        reason: TurnFinishReason::Completed,
                    } => return Ok(transcript),
                    HarnessEvent::TurnFinished { reason } => {
                        return Err(protocol_error(
                            "fixture turn ended",
                            format_args!("{reason:?}"),
                        ));
                    }
                    HarnessEvent::TurnError { text, .. }
                    | HarnessEvent::Exited { stderr: text, .. } => {
                        return Err(HarnessError::Protocol(text));
                    }
                    _ => {}
                }
            }
        })
        .await
        .map_err(|error| protocol_error("fixture turn timed out", error))?
    }

    #[cfg(unix)]
    fn fixture_launcher(directory: &Path, script: &str) -> std::io::Result<String> {
        use std::os::unix::fs::PermissionsExt;
        let path = directory.join("opencode-fixture");
        std::fs::write(&path, script)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
        Ok(path.to_string_lossy().into_owned())
    }

    #[cfg(feature = "live-opencode")]
    fn installed_fixture_spec(root: &Path) -> Result<SpawnSpec, Box<dyn std::error::Error>> {
        let settings = PathBuf::from(
            std::env::var_os("SHEPHERD_OPENCODE_TEST_SETTINGS")
                .ok_or("set SHEPHERD_OPENCODE_TEST_SETTINGS to the path of a settings.json file")?,
        );
        let executable =
            configured_executable(&settings)?.ok_or("configure agents.opencode.executable")?;
        let project = root.join("project");
        std::fs::create_dir(&project)?;
        let mut environment = BTreeMap::new();
        for (name, path) in [
            ("HOME", root.to_owned()),
            ("XDG_CONFIG_HOME", root.join("config")),
            ("XDG_DATA_HOME", root.join("data")),
            ("XDG_CACHE_HOME", root.join("cache")),
            ("XDG_STATE_HOME", root.join("state")),
        ] {
            environment.insert(name.into(), path.to_string_lossy().into_owned());
        }
        for (name, value) in [
            ("OPENCODE_DISABLE_MODELS_FETCH", "1"),
            ("HTTP_PROXY", "http://127.0.0.1:1"),
            ("HTTPS_PROXY", "http://127.0.0.1:1"),
            ("NO_PROXY", "127.0.0.1,localhost"),
        ] {
            environment.insert(name.into(), value.into());
        }
        Ok(SpawnSpec {
            program: executable
                .into_os_string()
                .into_string()
                .map_err(|_| "OpenCode path must be UTF-8")?,
            cwd: project,
            environment,
            ..SpawnSpec::default()
        })
    }

    #[cfg(feature = "live-opencode")]
    #[derive(Clone, Copy)]
    enum FixtureModel {
        Text,
        Image,
        Unauthorized,
        Shell,
    }

    #[cfg(feature = "live-opencode")]
    const FIXTURE_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAIAAACQkWg2AAAAFklEQVR4nGNwURIkCTGMahjVMHw1AAB4uncB7nfs+wAAAABJRU5ErkJggg==";

    #[cfg(feature = "live-opencode")]
    async fn serve_fixture_model(
        listener: TcpListener,
        model: FixtureModel,
    ) -> std::io::Result<()> {
        loop {
            let (socket, _) = listener.accept().await?;
            let (reader, mut writer) = socket.into_split();
            let (_, request) = read_fixture_request(reader).await?;
            if matches!(model, FixtureModel::Unauthorized) {
                let body = r#"{"error":{"message":"Local fixture credential expired","type":"invalid_request_error","code":"invalid_api_key"}}"#;
                let response = format!(
                    "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                writer.write_all(response.as_bytes()).await?;
                writer.shutdown().await?;
                continue;
            }
            let fixture_text = if matches!(model, FixtureModel::Image) {
                let request: Value = serde_json::from_slice(&request)?;
                let expected = format!("data:image/png;base64,{FIXTURE_PNG}");
                let found = request["messages"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|message| message["content"].as_array())
                    .flatten()
                    .any(|part| part["image_url"]["url"] == expected);
                if found {
                    "received fixture image"
                } else {
                    "missing fixture image"
                }
            } else {
                "local fixture reply"
            };
            let shell_call = if matches!(model, FixtureModel::Shell) {
                fixture_shell_call(&serde_json::from_slice(&request)?)
            } else {
                None
            };
            let (reply, finish_reason) = shell_call.map_or_else(
                || (json!({"role":"assistant", "content":fixture_text}), "stop"),
                |call| {
                    (
                        json!({"role":"assistant", "tool_calls":[call]}),
                        "tool_calls",
                    )
                },
            );
            let mut body = String::new();
            for (delta, finish) in [(reply, None), (json!({}), Some(finish_reason))] {
                let chunk = json!({
                    "id":"chatcmpl-fixture", "object":"chat.completion.chunk", "created":1,
                    "model":"coder", "choices":[{"index":0,"delta":delta,"finish_reason":finish}]
                });
                body.push_str(&format!("data: {chunk}\n\n"));
            }
            body.push_str("data: [DONE]\n\n");
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            writer.write_all(response.as_bytes()).await?;
            writer.shutdown().await?;
        }
    }

    #[cfg(feature = "live-opencode")]
    fn fixture_shell_call(request: &Value) -> Option<Value> {
        let last_input = request["messages"]
            .as_array()?
            .iter()
            .rev()
            .find(|message| matches!(message["role"].as_str(), Some("tool" | "user")))?;
        if last_input["role"] != "user" {
            return None;
        }
        let shell = request["tools"]
            .as_array()?
            .iter()
            .find(|tool| tool["function"]["name"] == "shell")?;
        let arguments = json!({"command": "printf shepherd_approval_fixture"});
        Some(json!({
            "index": 0, "id": format!("call_{}", uuid::Uuid::new_v4().simple()), "type": "function",
            "function": {"name": shell["function"]["name"], "arguments": arguments.to_string()}
        }))
    }

    async fn read_fixture_request(
        reader: impl AsyncRead + Unpin,
    ) -> std::io::Result<(String, Vec<u8>)> {
        let mut reader = BufReader::new(reader);
        let mut request = String::new();
        reader.read_line(&mut request).await?;
        request.truncate(request.trim_end().len());
        let mut line = String::new();
        let mut length = 0;
        while reader.read_line(&mut line).await? > 0 && line != "\r\n" {
            if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = value
                    .trim()
                    .parse::<usize>()
                    .map_err(std::io::Error::other)?;
            }
            line.clear();
        }
        if length > MAX_RESPONSE_BYTES {
            return Err(std::io::Error::other("fixture request is too large"));
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).await?;
        Ok((request, body))
    }
}
