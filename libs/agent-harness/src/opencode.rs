//! OpenCode's private HTTP server and session control.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use reqwest::{Client, Method, RequestBuilder, Response, Url};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{Instant, timeout, timeout_at};

use crate::acp::{AcpHarness, TerminalBackend};
use crate::settings::AgentSettings;
use crate::{
    AdapterTier, AgentDiagnostics, AgentHarness, ApprovalId, ApprovalRequest, Capabilities,
    Decision, DeltaKind, Entry, HarnessError, HarnessEvent, HarnessResult, ItemId, PromptInput,
    SessionId, SessionState, SpawnSpec, ToolCallId, ToolStatus, TurnFinishReason, TurnId,
    command_line,
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
        })
    }
}

#[derive(Deserialize)]
struct SessionRecord {
    id: String,
    location: SessionLocation,
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
            diagnostics.protocol = format!(
                "{} (native unavailable: {})",
                diagnostics.protocol, self.diagnostics.protocol
            );
            diagnostics
        } else {
            self.diagnostics.clone()
        }
    }

    fn tier(&self) -> AdapterTier {
        if self.fallback.is_some() {
            AdapterTier::Acp
        } else {
            AdapterTier::Native
        }
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
            Err(HarnessError::Protocol(reason)) => {
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
                self.diagnostics.protocol = reason.clone();
                let capabilities = fallback.start(spec).await.map_err(|error| {
                    HarnessError::Protocol(format!(
                        "OpenCode native server unavailable: {reason}. ACP fallback failed: {error}"
                    ))
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
        if let Err(error) = server
            .enqueue_prompt(&session, &id, &input.text, true)
            .await
        {
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
            async move {
                let terminal = match read_turn(events, session, &id, &sender).await {
                    Ok(event) => event,
                    Err(error) => {
                        if let Err(cleanup) = send_request(remove).await {
                            tracing::warn!(%cleanup, "could not remove the disconnected OpenCode prompt");
                        }
                        if let Err(cleanup) = send_request(interrupt).await {
                            tracing::warn!(%cleanup, "could not interrupt the disconnected OpenCode turn");
                        }
                        HarnessEvent::Exited {
                            code: None,
                            stderr: error.to_string(),
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
        if let Some(server) = self.server.take() {
            server.shutdown().await?;
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
    child: Child,
    client: Client,
    address: Url,
    password: String,
    directory: String,
    probe: OpenCodeProbe,
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
    pending: VecDeque<HarnessResult<OpenCodeEvent>>,
}

impl OpenCodeEvents {
    pub async fn next(&mut self) -> HarnessResult<OpenCodeEvent> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                return event;
            }
            let chunk = self.response.chunk().await.map_err(|error| {
                protocol_error("could not read OpenCode event stream", error.without_url())
            })?;
            let chunk = chunk.ok_or_else(|| {
                HarnessError::Protocol("OpenCode event stream disconnected".into())
            })?;
            if let Err(error) = self.decoder.push(&chunk, &mut self.pending) {
                self.pending.push_back(Err(error));
            }
        }
    }
}

#[derive(Default)]
struct EventDecoder {
    line: Vec<u8>,
    data: Vec<u8>,
}

impl EventDecoder {
    fn push(
        &mut self,
        chunk: &[u8],
        events: &mut VecDeque<HarnessResult<OpenCodeEvent>>,
    ) -> HarnessResult<()> {
        for fragment in chunk.split_inclusive(|byte| *byte == b'\n') {
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
                events.push_back(Ok(event));
                self.data.clear();
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
        Ok(())
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
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|source| HarnessError::Spawn {
                program: spec.program.clone(),
                source,
            })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            HarnessError::Protocol("OpenCode did not provide a startup pipe".into())
        })?;
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
            child,
            client,
            address,
            password,
            directory,
            probe,
        })
    }

    pub fn probe(&self) -> &OpenCodeProbe {
        &self.probe
    }

    pub async fn subscribe(&self) -> HarnessResult<OpenCodeEvents> {
        let handshake = async {
            let response = self
                .request(Method::GET, "/api/event")
                .header(reqwest::header::ACCEPT, "text/event-stream")
                .send()
                .await
                .and_then(Response::error_for_status)
                .map_err(|error| {
                    protocol_error(
                        "could not subscribe to OpenCode events",
                        error.without_url(),
                    )
                })?;
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
                pending: VecDeque::new(),
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
        self.session_id(response.data)
    }

    pub async fn resume_session(&self, session: &SessionId) -> HarnessResult<SessionId> {
        if !self.probe.resume {
            return Err(HarnessError::Protocol(
                "OpenCode server does not support session lookup".into(),
            ));
        }
        let path = session_path(session)?;
        let response: ResponseData<SessionRecord> =
            response_json(self.request(Method::GET, &path)).await?;
        let resumed = self.session_id(response.data)?;
        if &resumed != session {
            return Err(HarnessError::Protocol(
                "OpenCode returned a different session".into(),
            ));
        }
        Ok(resumed)
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
        text: &str,
        resume: bool,
    ) -> HarnessResult<()> {
        let path = format!("{}/prompt", session_path(session)?);
        let response: ResponseData<Value> = response_json(
            self.request(Method::POST, &path)
                .json(&json!({"id": prompt_id, "text": text, "resume": resume})),
        )
        .await?;
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

    pub async fn shutdown(mut self) -> HarnessResult<()> {
        self.child
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

    fn session_id(&self, record: SessionRecord) -> HarnessResult<SessionId> {
        if record.location.directory != Path::new(&self.directory) {
            return Err(HarnessError::Protocol(
                "OpenCode returned a session outside the task directory".into(),
            ));
        }
        let session = SessionId::new(record.id);
        session_path(&session)?;
        Ok(session)
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
        .map_err(|error| protocol_error("OpenCode HTTP request failed", error.without_url()))
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
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

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
        let mut api: Api = serde_json::from_value(json!({"paths": {
            "/api/event": {"get": {}},
            "/api/session": {"post": {}},
            "/api/session/{sessionID}/prompt": {"post": {}},
            "/api/session/{sessionID}/inbox/{inboxID}": {"delete": {}},
            "/api/session/{sessionID}/interrupt": {"post": {}}
        }}))?;
        let health = || Health {
            version: "fixture".into(),
        };
        let error = api
            .probe(health())
            .expect_err("permission handling is required");
        assert!(error.to_string().contains("/permission/{requestID}/reply"));
        api.paths.insert(
            "/api/session/{sessionID}/permission/{requestID}/reply".into(),
            BTreeMap::from([("post".into(), json!({}))]),
        );
        assert!(!api.probe(health())?.resume);
        api.paths.insert(
            "/api/session/{sessionID}".into(),
            BTreeMap::from([("get".into(), json!({}))]),
        );
        assert!(api.probe(health())?.resume);
        Ok(())
    }

    #[test]
    fn decodes_split_event_lines_and_preserves_events_before_failure()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut decoder = EventDecoder::default();
        let mut events = VecDeque::new();
        for byte in ": heartbeat\n\r\ndata: {\"type\":\"session.text.delta\",\r\n\
                     data: \"data\":{\"delta\":\"héllo\"}}\r\n\r\n"
            .as_bytes()
            .chunks(1)
        {
            decoder.push(byte, &mut events)?;
        }
        let error = decoder.push(
            b"data: {\"type\":\"session.execution.succeeded\",\"data\":{}}\n\n\
              event: effect/httpapi/stream/failure\n",
            &mut events,
        );
        let text = events.pop_front().ok_or("expected text event")??;
        assert_eq!(text.kind, "session.text.delta");
        assert_eq!(text.data, json!({"delta": "héllo"}));
        let finished = events.pop_front().ok_or("expected execution event")??;
        assert_eq!(finished.kind, "session.execution.succeeded");
        assert!(events.is_empty());
        assert!(error.is_err());
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

    #[tokio::test]
    #[ignore = "requires a configured OpenCode executable and a loopback socket"]
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
            .enqueue_prompt(&session, &prompt_id, "queued fixture", false)
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

    #[tokio::test]
    #[ignore = "requires a configured OpenCode executable and loopback sockets"]
    async fn installed_harness_streams_a_turn_through_a_local_fixture()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let mut spec = installed_fixture_spec(root.path())?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = format!("http://{}/v1", listener.local_addr()?);
        let config = spec.cwd.join("opencode.json");
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
                                "capabilities": { "tools": true, "input": ["text"], "output": ["text"] },
                                "limit": { "context": 32768, "output": 8192 }
                            }
                        }
                    }
                }
            }))?,
        )?;
        spec.environment.insert(
            "OPENCODE_CONFIG".into(),
            config.to_string_lossy().into_owned(),
        );
        let provider = tokio::spawn(serve_fixture_model(listener));
        let outcome = async {
            let mut harness = OpenCodeHarness::default();
            harness.start(spec).await?;
            assert_eq!(harness.tier(), AdapterTier::Native);
            harness.open_session(None).await?;
            let transcript = fixture_reply(&mut harness).await?;
            harness.shutdown().await?;
            Ok::<_, HarnessError>(transcript)
        }
        .await;
        provider.abort();
        let transcript = outcome?;
        assert_eq!(transcript.len(), 1);
        assert_eq!(
            transcript.entry(0).map(crate::Entry::summary).as_deref(),
            Some("agent: local fixture reply")
        );
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn falls_back_to_acp_and_streams_a_turn() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let program = fixture_launcher(
            root.path(),
            r#"#!/bin/sh
if [ "$1" != acp ]; then exit 78; fi
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
        let spec = SpawnSpec {
            program,
            cwd: root.path().to_owned(),
            ..SpawnSpec::default()
        };
        let mut harness = OpenCodeHarness::default();
        harness.start(spec).await?;
        assert_eq!(harness.tier(), AdapterTier::Acp);
        let diagnostics = harness.diagnostics();
        assert!(diagnostics.command.ends_with(" acp"));
        assert!(diagnostics.protocol.contains("native unavailable"));
        assert_eq!(harness.open_session(None).await?.as_str(), "fixture");
        let transcript = fixture_reply(&mut harness).await?;
        assert_eq!(
            transcript.entry(0).map(crate::Entry::summary).as_deref(),
            Some("agent: ACP fixture reply")
        );
        harness.shutdown().await?;
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "requires a configured OpenCode executable and loopback sockets"]
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
            .prompt(PromptInput {
                text: "Reply with local fixture text only".into(),
            })
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
        let path = directory.join("opencode-acp-fixture");
        std::fs::write(&path, script)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
        Ok(path.to_string_lossy().into_owned())
    }

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

    async fn serve_fixture_model(listener: TcpListener) -> std::io::Result<()> {
        loop {
            let (socket, _) = listener.accept().await?;
            let (reader, mut writer) = socket.into_split();
            let mut reader = BufReader::new(reader);
            let mut line = String::new();
            let mut length = 0;
            while reader.read_line(&mut line).await? > 0 && line != "\r\n" {
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse::<u64>().map_err(std::io::Error::other)?;
                }
                line.clear();
            }
            tokio::io::copy(&mut reader.take(length), &mut tokio::io::sink()).await?;
            let mut body = String::new();
            for (delta, finish) in [
                (
                    json!({"role":"assistant", "content":"local fixture reply"}),
                    None,
                ),
                (json!({}), Some("stop")),
            ] {
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
}
