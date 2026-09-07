//! ACP (Agent Client Protocol) server over stdio, enabled with `--acp`.
//!
//! Implements the same v1 method surface as `gemini-cli --acp`:
//! - `initialize` (capabilities + auth method advertisement)
//! - `session/new` (with cwd + optional MCP servers)
//! - `session/load` (resume: replays the conversation via `session/update`)
//! - `session/prompt` (streams `session/update` notifications and replies with a stop reason)
//! - `session/set_config_option` (plus `session/set_mode` and `unstable_set_session_model`)
//! - `session/cancel` (notification)
//! - `fs/read_text_file` / `fs/write_text_file` (agent-initiated requests to the
//!   client, gated by the `clientCapabilities.fs` advertised during `initialize`)
//!
//! Requests are newline-delimited JSON-RPC 2.0 on stdin; responses and
//! `session/update` notifications are newline-delimited JSON-RPC 2.0 on stdout.
//! The agent also sends its own requests (`fs/*`) to the client and awaits the
//! matching JSON-RPC response, correlating by `id`.

use crate::cli_commands::Cli;
use crate::config::AppConfig;
use crate::providers;
use anyhow::Context as _;
use cersei::events::AgentEvent;
use cersei::types::{Message, Role, StopReason};
use cersei::Agent;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

const PROTOCOL_VERSION: i64 = 1;
/// Approximate provider context window, reported in `usage_update`.
const CONTEXT_WINDOW: u64 = 128_000;
/// How long to keep draining the agent stream after a cancel request before
/// replying anyway (tool executions in cersei cannot always be aborted).
const CANCEL_GRACE: Duration = Duration::from_secs(2);

// ─── JSON-RPC 2.0 wire types ────────────────────────────────────────────────

/// Incoming JSON-RPC message. A message with `method` is a request (if `id`
/// is present) or a notification (no `id`); a message with `result` or
/// `error` and no `method` is a response to one of our outbound requests.
#[derive(Deserialize)]
struct RpcMessage {
    method: Option<String>,
    id: Option<Value>,
    #[serde(default)]
    params: Option<Value>,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<RpcErrorBody>,
}

#[derive(Serialize)]
struct RpcSuccess<'a> {
    jsonrpc: &'a str,
    id: Value,
    result: Value,
}

#[derive(Serialize)]
struct RpcFailure<'a> {
    jsonrpc: &'a str,
    id: Value,
    error: RpcErrorBody,
}

#[derive(Serialize, Deserialize, Clone)]
struct RpcErrorBody {
    code: i64,
    message: String,
}

#[derive(Serialize)]
struct RpcNotification<'a> {
    jsonrpc: &'a str,
    method: &'a str,
    params: Value,
}

#[derive(Serialize)]
struct RpcRequest<'a> {
    jsonrpc: &'a str,
    id: Value,
    method: &'a str,
    params: Value,
}

/// A pending outbound request awaiting the client's JSON-RPC response.
type PendingResponse = oneshot::Sender<RpcResponse>;

/// The outcome of an outbound request: either the client's `result` payload,
/// or a JSON-RPC error it returned. A dropped response channel (peer gone) is
/// surfaced as `Err` on the receiver rather than a variant.
enum RpcResponse {
    Result(Value),
    Error(RpcErrorBody),
}

/// Serializes all writes to stdout so responses and streaming notifications
/// from concurrent tasks stay ordered and never interleave mid-line.
struct AcpConnection {
    tx: tokio::sync::mpsc::UnboundedSender<String>,
    /// Outstanding outbound JSON-RPC requests (agent -> client), keyed by the
    /// integer `id` we assigned. The sender completes the waiting caller.
    pending: Mutex<HashMap<i64, PendingResponse>>,
    /// Monotonic source of unique request ids for outbound requests.
    next_request_id: AtomicI64,
}

impl AcpConnection {
    fn new() -> (Self, tokio::task::JoinHandle<()>) {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let writer = tokio::spawn(async move {
            let mut stdout = tokio::io::stdout();
            while let Some(line) = rx.recv().await {
                if stdout.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
                if stdout.write_all(b"\n").await.is_err() {
                    break;
                }
                if stdout.flush().await.is_err() {
                    break;
                }
            }
        });
        (Self { tx, pending: Mutex::new(HashMap::new()), next_request_id: AtomicI64::new(1) }, writer)
    }

    fn send_line(&self, line: String) {
        let _ = self.tx.send(line);
    }

    fn send(&self, value: &impl Serialize) {
        match serde_json::to_string(value) {
            Ok(line) => self.send_line(line),
            Err(e) => eprintln!("acp: failed to serialize message: {e}"),
        }
    }

    fn response(&self, id: Value, result: Value) {
        self.send(&RpcSuccess {
            jsonrpc: "2.0",
            id,
            result,
        });
    }

    fn error(&self, id: Value, code: i64, message: impl Into<String>) {
        self.send(&RpcFailure {
            jsonrpc: "2.0",
            id,
            error: RpcErrorBody {
                code,
                message: message.into(),
            },
        });
    }

    fn notification(&self, method: &'static str, params: Value) {
        self.send(&RpcNotification {
            jsonrpc: "2.0",
            method,
            params,
        });
    }

    fn send_update(&self, session_id: &str, update: SessionUpdate) {
        self.notification("session/update", json!({ "sessionId": session_id, "update": update }));
    }

    /// Advertise the slash commands available in this session (per the ACP
    /// slash-commands spec, sent after session creation/load).
    fn send_available_commands(&self, session_id: &str) {
        self.send_update(
            session_id,
            SessionUpdate::AvailableCommandsUpdate {
                available_commands: vec![AvailableCommand {
                    name: "interview".into(),
                    description: "Interview you about a task or spec to produce requirements".into(),
                    input: Some(AvailableCommandInput {
                        ty: "text",
                        hint: "what to interview about".into(),
                    }),
                }],
            },
        );
    }

    /// Send an `interview/started` notification.
    fn send_interview_started(&self, session_id: &str, params: InterviewStartedParams) {
        self.notification(
            "interview/started",
            json!({ "sessionId": session_id, "interview": params }),
        );
    }

    /// Send an `interview/question` notification.
    fn send_interview_question(&self, session_id: &str, params: InterviewQuestionParams) {
        self.notification(
            "interview/question",
            json!({ "sessionId": session_id, "question": params }),
        );
    }

    /// Send an `interview/completed` notification.
    fn send_interview_completed(&self, session_id: &str, params: InterviewCompletedParams) {
        self.notification(
            "interview/completed",
            json!({ "sessionId": session_id, "completed": params }),
        );
    }

    /// Route an inbound JSON-RPC response (a message with `id` but no
    /// `method`) to the outbound caller waiting on that id. Returns true when
    /// a pending caller was found. Errors are delivered as `RpcResponse::Error`.
    fn deliver_response(&self, id: &Value, result: Option<Value>, error: Option<RpcErrorBody>) {
        // ids we mint are integers; ignore anything else (e.g. a stray client
        // request sharing an id) rather than risk misrouting.
        let Some(id_int) = id.as_i64() else { return };
        let sender = self.pending.lock().remove(&id_int);
        let Some(sender) = sender else { return };
        let response = match error {
            Some(err) => RpcResponse::Error(err),
            None => RpcResponse::Result(result.unwrap_or(Value::Null)),
        };
        // The receiver may have timed out and dropped its end; ignore that.
        let _ = sender.send(response);
    }

    /// Send a JSON-RPC request to the client and return a receiver for the
    /// matching response. The caller owns the id and must await the receiver
    /// (with a timeout) to avoid leaking the pending entry.
    fn request(&self, method: &str, params: Value) -> (i64, oneshot::Receiver<RpcResponse>) {
        let id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().insert(id, tx);
        self.send(&RpcRequest {
            jsonrpc: "2.0",
            id: json!(id),
            method,
            params,
        });
        (id, rx)
    }

    /// Drop a pending outbound request without delivering a response (used on
    /// timeout/cancel so the entry doesn't linger).
    fn forget_request(&self, id: i64) {
        self.pending.lock().remove(&id);
    }
}

// ─── ACP content & update types ─────────────────────────────────────────────

#[derive(Deserialize)]
#[allow(dead_code)] // fields are part of the wire format
#[serde(tag = "type", rename_all = "snake_case")]
enum PromptContentBlock {
    Text { text: String },
    Image { mime_type: String, data: String },
    Audio { mime_type: String, data: String },
    Resource { resource: Value },
    ResourceLink { uri: String, name: Option<String>, mime_type: Option<String> },
}

#[derive(Serialize)]
struct TextContent {
    #[serde(rename = "type")]
    ty: &'static str,
    text: String,
}

impl TextContent {
    fn new(text: impl Into<String>) -> Self {
        Self {
            ty: "text",
            text: text.into(),
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ToolCallContent {
    Content { content: TextContent },
}

impl ToolCallContent {
    fn text(text: impl Into<String>) -> Self {
        ToolCallContent::Content {
            content: TextContent::new(text),
        }
    }
}

#[derive(Serialize, Clone, Copy)]
#[allow(dead_code)] // spec statuses not emitted by the current agent loop
#[serde(rename_all = "snake_case")]
enum ToolStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Serialize)]
#[serde(tag = "sessionUpdate", rename_all = "snake_case")]
enum SessionUpdate {
    UserMessageChunk {
        content: TextContent,
        #[serde(skip_serializing_if = "Option::is_none")]
        message_id: Option<String>,
    },
    AgentMessageChunk {
        content: TextContent,
        #[serde(skip_serializing_if = "Option::is_none")]
        message_id: Option<String>,
    },
    AgentThoughtChunk {
        content: TextContent,
    },
    ToolCall {
        tool_call_id: String,
        title: String,
        status: ToolStatus,
        kind: String,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        content: Vec<ToolCallContent>,
    },
    ToolCallUpdate {
        tool_call_id: String,
        title: String,
        status: ToolStatus,
        kind: String,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        content: Vec<ToolCallContent>,
    },
    UsageUpdate {
        used: u64,
        size: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        cost: Option<Cost>,
    },
    /// The model the session runs on changed (e.g. a combo fell back to a
    /// different entry). `modelId` is the user-facing selection, which stays
    /// the combo; `effectiveModelId` is the concrete model now in use.
    ModelChanged {
        #[serde(rename = "modelId")]
        model_id: String,
        #[serde(rename = "effectiveModelId")]
        effective_model_id: String,
    },
    /// Advertise the slash commands available in this session.
    AvailableCommandsUpdate {
        #[serde(rename = "availableCommands")]
        available_commands: Vec<AvailableCommand>,
    },
}

/// A slash command advertised to the client via `available_commands_update`.
/// Clients surface these (e.g. `/interview`) and send them back as the first
/// text block of a `session/prompt`.
#[derive(Serialize)]
struct AvailableCommand {
    /// The command name as typed by the user (without the leading `/`).
    name: String,
    /// Human-readable description of what the command does.
    description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    input: Option<AvailableCommandInput>,
}

/// Optional input specification for an advertised command.
#[derive(Serialize)]
struct AvailableCommandInput {
    #[serde(rename = "type")]
    ty: &'static str,
    /// Hint to display when the input hasn't been provided yet.
    hint: String,
}

#[derive(Serialize)]
struct Cost {
    amount: f64,
    currency: &'static str,
}

// ─── Interview lifecycle notifications ──────────────────────────────────────

/// Sent when an interview flow begins (when a prompt starts with `/interview`).
#[derive(Serialize)]
struct InterviewStartedParams {
    /// Unique identifier for this interview.
    interview_id: String,
    /// The target/request being interviewed.
    target: String,
    /// The base prompt reference used to start the interview.
    base_prompt: String,
}

/// Sent for each round of clarifying questions from the agent.
/// The questions use the same schema as the `ask_user` tool.
#[derive(Serialize)]
struct InterviewQuestionParams {
    /// Unique identifier for this interview.
    interview_id: String,
    /// The questions in this round.
    questions: Vec<AskUserQuestionParams>,
}

/// A single clarifying question, mirroring the `ask_user` tool schema.
#[derive(Serialize)]
struct AskUserQuestionParams {
    question: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    header: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<Vec<AskUserOptionParams>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    multi_select: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    validation: Option<AskUserValidationParams>,
}

#[derive(Serialize)]
struct AskUserOptionParams {
    label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
}

#[derive(Serialize)]
struct AskUserValidationParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    max_length: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    min_length: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pattern: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pattern_error: Option<String>,
}

/// Sent when the interview finishes.
#[derive(Serialize)]
struct InterviewCompletedParams {
    /// Unique identifier for this interview.
    interview_id: String,
    /// Outcome: "success" or "failure".
    status: String,
    /// Path to the written spec file, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    spec_file_path: Option<String>,
    /// Short summary of what was produced.
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
    /// Agent's final reply text, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    final_reply: Option<String>,
}

// ─── Request params ─────────────────────────────────────────────────────────

/// `initialize` request params. The client advertises capabilities here; we
/// only act on `clientCapabilities.fs` (read/write text file support).
#[derive(Deserialize, Default)]
struct InitializeParams {
    #[serde(default, rename = "protocolVersion")]
    #[allow(dead_code)] // part of the initialize wire format
    protocol_version: Option<i64>,
    #[serde(default, rename = "clientCapabilities")]
    client_capabilities: Option<ClientCapabilities>,
}

#[derive(Deserialize, Default)]
struct ClientCapabilities {
    #[serde(default)]
    fs: Option<FileSystemCapabilities>,
}

/// Which `fs/*` methods the client supports. Per the spec, omitted/`null`
/// means unsupported, so missing fields default to `false`.
#[derive(Deserialize, Default, Clone, Copy)]
struct FileSystemCapabilities {
    #[serde(default, rename = "readTextFile")]
    read_text_file: bool,
    #[serde(default, rename = "writeTextFile")]
    write_text_file: bool,
}

/// Params for `fs/read_text_file`.
#[derive(Serialize)]
struct ReadTextFileParams<'a> {
    #[serde(rename = "sessionId")]
    session_id: &'a str,
    path: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<u32>,
}

/// Params for `fs/write_text_file`.
#[derive(Serialize)]
struct WriteTextFileParams<'a> {
    #[serde(rename = "sessionId")]
    session_id: &'a str,
    path: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct ReadTextFileResult {
    content: String,
}

#[derive(Deserialize)]
struct NewSessionParams {
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default, rename = "mcpServers")]
    mcp_servers: Vec<McpServerParam>,
}

#[derive(Deserialize)]
struct McpServerParam {
    name: String,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: Vec<EnvVarParam>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default, rename = "type")]
    server_type: Option<String>,
}

#[derive(Deserialize)]
struct EnvVarParam {
    name: String,
    value: String,
}

#[derive(Deserialize)]
#[allow(dead_code)] // cwd/mcp_servers are part of the session/load wire format
struct SessionIdParams {
    #[serde(default, rename = "sessionId")]
    session_id: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default, rename = "mcpServers")]
    mcp_servers: Vec<McpServerParam>,
}

#[derive(Deserialize)]
struct PromptParams {
    #[serde(default, rename = "sessionId")]
    session_id: Option<String>,
    #[serde(default)]
    prompt: Vec<PromptContentBlock>,
}

#[derive(Deserialize)]
struct SetConfigOptionParams {
    #[serde(default, rename = "sessionId")]
    session_id: Option<String>,
    #[serde(default, rename = "configId")]
    config_id: Option<String>,
    #[serde(default)]
    value: Option<Value>,
}

#[derive(Deserialize)]
struct SetModeParams {
    #[serde(default, rename = "sessionId")]
    session_id: Option<String>,
    #[serde(default, rename = "modeId")]
    mode_id: Option<String>,
    #[serde(default, rename = "modelId")]
    model_id: Option<String>,
}

// ─── Session state ──────────────────────────────────────────────────────────

struct AcpSession {
    id: String,
    cwd: PathBuf,
    provider: String,
    model: String,
    /// The concrete (provider, model) the session currently runs on: the
    /// combo's current fallback entry when the selection is a combo, otherwise
    /// the selection itself. The user-facing selection stays `provider`/`model`.
    effective_provider: String,
    effective_model: String,
    mode: String,
    messages: Vec<Message>,
    /// Monotonic run counter, used to disambiguate pending prompts.
    run_seq: u64,
    /// (run id, cancellation token) for the prompt currently streaming, if any.
    pending_cancel: Option<(u64, CancellationToken)>,
    /// Combo fallback state for this session's selection (disabled when the
    /// session runs a plain provider/model).
    fallback: providers::FallbackManager,
    _mcp_servers: Vec<cersei::mcp::McpServerConfig>,
    /// Active interview ID, if the current prompt is an interview.
    active_interview_id: Option<String>,
}

impl AcpSession {
    /// Reset the effective model to the selection's resolved entry: the first
    /// combo entry for a combo selection, or the selection itself otherwise.
    fn refresh_effective(&mut self, config: &AppConfig) {
        match providers::effective_selection(config, &self.provider, &self.model) {
            Ok((provider, model)) => {
                self.effective_provider = provider;
                self.effective_model = model;
            }
            Err(_) => {
                // Selection is unresolvable; report it as-is rather than
                // silently keeping stale effective state.
                self.effective_provider = self.provider.clone();
                self.effective_model = self.model.clone();
            }
        }
    }
}

// ─── Server ─────────────────────────────────────────────────────────────────

struct AcpServer {
    connection: AcpConnection,
    sessions: Mutex<HashMap<String, Arc<Mutex<AcpSession>>>>,
    config: AppConfig,
    default_provider: String,
    default_model: String,
    max_turns: u32,
    /// Filesystem methods the client advertised during `initialize`. Defaults
    /// to none until the client tells us otherwise.
    client_fs: Mutex<FileSystemCapabilities>,
}

pub async fn run_server(_cli: Cli, config: AppConfig) -> anyhow::Result<()> {
    // Fail fast if the configured default provider/model can't be resolved
    // (missing api_key etc.), before the protocol starts.
    let (default_provider, default_model) =
        providers::default_selection(&config).context("no usable provider/model configured")?;
    let (effective_provider, effective_model) =
        providers::effective_selection(&config, &default_provider, &default_model)
            .context("failed to resolve default provider/model")?;
    providers::resolve(&config, &effective_provider, &effective_model)
        .context("failed to resolve default provider/model (check api_key settings)")?;
    let max_turns = config.max_turns;

    let (connection, writer_task) = AcpConnection::new();
    let server = Arc::new(AcpServer {
        connection,
        sessions: Mutex::new(HashMap::new()),
        config,
        default_provider,
        default_model,
        max_turns,
        client_fs: Mutex::new(FileSystemCapabilities::default()),
    });

    let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                server.connection.error(Value::Null, -32700, format!("Parse error: {e}"));
                continue;
            }
        };
        let server = Arc::clone(&server);
        tokio::spawn(async move {
            server.dispatch(msg).await;
        });
    }

    // stdin closed: the client is gone, tear down.
    writer_task.abort();
    Ok(())
}

impl AcpServer {
    async fn dispatch(self: &Arc<Self>, msg: Value) {
        let rpc: RpcMessage = match serde_json::from_value(msg.clone()) {
            Ok(rpc) => rpc,
            Err(e) => {
                let id = msg.get("id").cloned().unwrap_or(Value::Null);
                self.connection.error(id, -32700, format!("Parse error: {e}"));
                return;
            }
        };

        let id = rpc.id.unwrap_or(Value::Null);

        // A message with no `method` and a present `id` is a response to one
        // of our outbound requests (`fs/*` etc.). Route it to the waiter.
        if rpc.method.is_none() && !id.is_null() {
            self.connection.deliver_response(&id, rpc.result, rpc.error);
            return;
        }

        let method = match rpc.method {
            Some(m) => m,
            None => return, // not a request or notification
        };
        let params = rpc.params.unwrap_or(Value::Null);

        match method.as_str() {
            "initialize" => {
                let init_params: InitializeParams = serde_json::from_value(params).unwrap_or_default();
                if let Some(fs) = init_params.client_capabilities.as_ref().and_then(|c| c.fs) {
                    *self.client_fs.lock() = fs;
                }
                self.connection.response(id, self.initialize());
            }
            "authenticate" | "logout" => {
                // Auth is handled out-of-band via the API key; accept and no-op.
                self.connection.response(id, json!({}));
            }
            "session/new" => {
                let params: NewSessionParams = match serde_json::from_value(params) {
                    Ok(p) => p,
                    Err(e) => {
                        self.connection.error(id, -32602, format!("Invalid params: {e}"));
                        return;
                    }
                };
                self.handle_new_session(id, params);
            }
            "session/load" => {
                let params: SessionIdParams = match serde_json::from_value(params) {
                    Ok(p) => p,
                    Err(e) => {
                        self.connection.error(id, -32602, format!("Invalid params: {e}"));
                        return;
                    }
                };
                self.handle_load_session(id, params).await;
            }
            "session/prompt" => {
                let params: PromptParams = match serde_json::from_value(params) {
                    Ok(p) => p,
                    Err(e) => {
                        self.connection.error(id, -32602, format!("Invalid params: {e}"));
                        return;
                    }
                };
                let session_id = match params.session_id {
                    Some(s) => s,
                    None => {
                        self.connection.error(id, -32602, "Missing sessionId");
                        return;
                    }
                };
                let server = Arc::clone(self);
                tokio::spawn(async move {
                    server.handle_prompt(id, session_id, params.prompt).await;
                });
            }
            "session/cancel" => {
                let params: SessionIdParams = match serde_json::from_value(params) {
                    Ok(p) => p,
                    Err(e) => {
                        self.connection.error(id, -32602, format!("Invalid params: {e}"));
                        return;
                    }
                };
                if let Some(session_id) = params.session_id
                    && let Some(session) = self.sessions.lock().get(&session_id).cloned()
                {
                    let token = session.lock().pending_cancel.take().map(|(_, t)| t);
                    if let Some(token) = token {
                        token.cancel();
                    }
                }
                // session/cancel is a notification; reply only if the client sent an id.
                if !id.is_null() {
                    self.connection.response(id, json!({}));
                }
            }
            "session/set_config_option" => {
                let params: SetConfigOptionParams = match serde_json::from_value(params) {
                    Ok(p) => p,
                    Err(e) => {
                        self.connection.error(id, -32602, format!("Invalid params: {e}"));
                        return;
                    }
                };
                self.handle_set_config_option(id, params);
            }
            "session/set_mode" => {
                let params: SetModeParams = match serde_json::from_value(params) {
                    Ok(p) => p,
                    Err(e) => {
                        self.connection.error(id, -32602, format!("Invalid params: {e}"));
                        return;
                    }
                };
                self.handle_set_mode(id, params);
            }
            "unstable_set_session_model" => {
                let params: SetModeParams = match serde_json::from_value(params) {
                    Ok(p) => p,
                    Err(e) => {
                        self.connection.error(id, -32602, format!("Invalid params: {e}"));
                        return;
                    }
                };
                self.handle_set_model(id, params);
            }
            "session/close" => {
                let params: SessionIdParams = match serde_json::from_value(params) {
                    Ok(p) => p,
                    Err(e) => {
                        self.connection.error(id, -32602, format!("Invalid params: {e}"));
                        return;
                    }
                };
                if let Some(session_id) = params.session_id
                    && let Some(session) = self.sessions.lock().remove(&session_id)
                {
                    let token = session.lock().pending_cancel.take().map(|(_, t)| t);
                    if let Some(token) = token {
                        token.cancel();
                    }
                }
                self.connection.response(id, json!({}));
            }
            _ => {
                if !id.is_null() {
                    self.connection
                        .error(id, -32601, format!("Method not found: {method}"));
                }
            }
        }
    }

    fn initialize(&self) -> Value {
        json!({
            "protocolVersion": PROTOCOL_VERSION,
            "agentInfo": {
                "name": "abstract",
                "title": "Abstract",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "agentCapabilities": {
                "loadSession": true,
                "promptCapabilities": {
                    "image": false,
                    "audio": false,
                    "embeddedContext": true,
                },
                "mcpCapabilities": { "http": false, "sse": false },
            },
            "authMethods": [
                {
                    "id": "abstract_api_key",
                    "name": "Abstract API key",
                    "description": "Uses the POOLSIDE_API_KEY environment variable",
                },
            ],
        })
    }

    /// Flat list of all "provider/model" ids across configured providers.
    fn models_value(&self, session: &Arc<Mutex<AcpSession>>) -> Value {
        let (provider, model, effective_provider, effective_model) = {
            let guard = session.lock();
            (
                guard.provider.clone(),
                guard.model.clone(),
                guard.effective_provider.clone(),
                guard.effective_model.clone(),
            )
        };
        let available: Vec<Value> = providers::entries(&self.config)
            .into_iter()
            .map(|(p, m)| {
                let id = providers::display_model_id(&p, &m);
                json!({ "modelId": id, "name": id })
            })
            .collect();
        let current_id = providers::display_model_id(&provider, &model);
        let effective_id = providers::display_model_id(&effective_provider, &effective_model);
        // The concrete model only differs from the selection while a combo runs
        // on a fallback entry; expose it then, mirroring the TUI header.
        let mut models = json!({
            "availableModels": available,
            "currentModelId": current_id,
        });
        if effective_id != current_id {
            models["effectiveModelId"] = json!(effective_id);
        }
        models
    }

    /// Config options: provider + model (dependent) + mode.
    fn config_options(&self, session: &Arc<Mutex<AcpSession>>) -> Value {
        let (session_provider, session_model, session_mode) = {
            let guard = session.lock();
            (guard.provider.clone(), guard.model.clone(), guard.mode.clone())
        };
        let provider_names: Vec<Value> = providers::providers(&self.config)
            .into_iter()
            .map(|p| json!({ "value": p.name, "name": p.name }))
            .collect();
        let model_options: Vec<Value> = providers::provider(&self.config, &session_provider)
            .map(|p| {
                p.models
                    .into_iter()
                    .map(|m| {
                        let id = providers::display_model_id(&session_provider, &m);
                        json!({ "value": id, "name": id })
                    })
                    .collect()
            })
            .unwrap_or_default();
        json!([
            {
                "id": "provider",
                "name": "Provider",
                "type": "select",
                "currentValue": session_provider,
                "options": provider_names,
            },
            {
                "id": "model",
                "name": "Model",
                "category": "model",
                "type": "select",
                "currentValue": providers::display_model_id(&session_provider, &session_model),
                "options": model_options,
            },
            {
                "id": "mode",
                "name": "Mode",
                "description": "Tool permission mode",
                "category": "mode",
                "type": "select",
                "currentValue": session_mode,
                "options": [
                    { "value": "auto", "name": "Auto", "description": "Auto-approves all tools" },
                    { "value": "readonly", "name": "Read-only", "description": "Denies tools that modify files or run commands" },
                ],
            },
        ])
    }

    fn handle_new_session(&self, id: Value, params: NewSessionParams) {
        let session_id = uuid_short();
        let cwd = resolve_cwd(params.cwd);
        let mcp_servers: Vec<cersei::mcp::McpServerConfig> = params
            .mcp_servers
            .iter()
            .map(|s| cersei::mcp::McpServerConfig {
                name: s.name.clone(),
                command: s.command.clone(),
                args: s.args.clone(),
                env: s.env.iter().map(|e| (e.name.clone(), e.value.clone())).collect(),
                url: s.url.clone(),
                server_type: s.server_type.clone().unwrap_or_else(|| "stdio".to_string()),
            })
            .collect();

        let mut session_state = AcpSession {
            id: session_id.clone(),
            cwd,
            provider: self.default_provider.clone(),
            model: self.default_model.clone(),
            effective_provider: String::new(),
            effective_model: String::new(),
            mode: "auto".to_string(),
            messages: Vec::new(),
            run_seq: 0,
            pending_cancel: None,
            fallback: providers::fallback_for(
                &self.config,
                &self.default_provider,
                &self.default_model,
            ),
            _mcp_servers: mcp_servers,
            active_interview_id: None,
        };
        session_state.refresh_effective(&self.config);
        let session = Arc::new(Mutex::new(session_state));
        self.sessions.lock().insert(session_id.clone(), Arc::clone(&session));

        let guard = session.lock();
        let mode = guard.mode.clone();
        drop(guard);

        self.connection.response(
            id,
            json!({
                "sessionId": session_id,
                "modes": modes(&mode),
                "models": self.models_value(&session),
                "configOptions": self.config_options(&session),
            }),
        );

        // Advertise slash commands now that the session exists.
        self.connection.send_available_commands(&session_id);
    }

    async fn handle_load_session(&self, id: Value, params: SessionIdParams) {
        let session_id = match params.session_id {
            Some(s) => s,
            None => {
                self.connection.error(id, -32602, "Missing sessionId");
                return;
            }
        };
        let session = match self.sessions.lock().get(&session_id).cloned() {
            Some(s) => s,
            None => {
                self.connection.error(id, -32602, format!("Session not found: {session_id}"));
                return;
            }
        };

        // Replay the conversation via session/update notifications.
        let messages = session.lock().messages.clone();
        for msg in &messages {
            let text = msg.get_all_text();
            if text.trim().is_empty() {
                continue;
            }
            match msg.role {
                Role::User => self.connection.send_update(
                    &session_id,
                    SessionUpdate::UserMessageChunk {
                        content: TextContent::new(text),
                        message_id: None,
                    },
                ),
                Role::Assistant => self.connection.send_update(
                    &session_id,
                    SessionUpdate::AgentMessageChunk {
                        content: TextContent::new(text),
                        message_id: None,
                    },
                ),
                Role::System => {}
            }
        }

        self.connection.response(id, Value::Null);

        // Re-advertise slash commands so a client restoring this session still
        // knows what commands are available.
        self.connection.send_available_commands(&session_id);
    }

    fn handle_set_config_option(&self, id: Value, params: SetConfigOptionParams) {
        let session_id = match params.session_id {
            Some(s) => s,
            None => {
                self.connection.error(id, -32602, "Missing sessionId");
                return;
            }
        };
        let config_id = match params.config_id {
            Some(c) => c,
            None => {
                self.connection.error(id, -32602, "Missing configId");
                return;
            }
        };
        let value = match params.value {
            Some(v) => v,
            None => {
                self.connection.error(id, -32602, "Missing value");
                return;
            }
        };
        let session = match self.sessions.lock().get(&session_id).cloned() {
            Some(s) => s,
            None => {
                self.connection.error(id, -32602, format!("Session not found: {session_id}"));
                return;
            }
        };

        let mut guard = session.lock();
        match config_id.as_str() {
            "mode" => {
                let mode = match value.as_str() {
                    Some(m) if m == "auto" || m == "readonly" => m,
                    _ => {
                        self.connection
                            .error(id, -32602, "Invalid value for config option 'mode'");
                        return;
                    }
                };
                guard.mode = mode.to_string();
            }
            "provider" => {
                let name = match value.as_str() {
                    Some(n) => n,
                    None => {
                        self.connection
                            .error(id, -32602, "Invalid value for config option 'provider'");
                        return;
                    }
                };
                let model = match providers::default_model(&self.config, name) {
                    Ok(m) => m,
                    Err(_) => {
                        self.connection
                            .error(id, -32602, format!("Unknown provider: {name}"));
                        return;
                    }
                };
                guard.provider = name.to_string();
                guard.model = model;
                guard.fallback = providers::fallback_for(&self.config, &guard.provider, &guard.model);
                guard.refresh_effective(&self.config);
            }
            "model" => {
                let text = match value.as_str() {
                    Some(t) => t,
                    None => {
                        self.connection
                            .error(id, -32602, "Invalid value for config option 'model'");
                        return;
                    }
                };
                let (provider, model) = match providers::resolve_selection(&self.config, &guard.provider, text) {
                    Ok(sel) => sel,
                    Err(e) => {
                        self.connection.error(id, -32602, format!("Invalid model: {e}"));
                        return;
                    }
                };
                guard.provider = provider;
                guard.model = model;
                guard.fallback = providers::fallback_for(&self.config, &guard.provider, &guard.model);
                guard.refresh_effective(&self.config);
            }
            other => {
                self.connection.error(id, -32602, format!("Unknown config option: {other}"));
                return;
            }
        }
        drop(guard);

        self.connection
            .response(id, json!({ "configOptions": self.config_options(&session) }));
    }

    fn handle_set_mode(&self, id: Value, params: SetModeParams) {
        let session_id = match params.session_id {
            Some(s) => s,
            None => {
                self.connection.error(id, -32602, "Missing sessionId");
                return;
            }
        };
        let mode_id = match params.mode_id {
            Some(m) => m,
            None => {
                self.connection.error(id, -32602, "Missing modeId");
                return;
            }
        };
        let session = match self.sessions.lock().get(&session_id).cloned() {
            Some(s) => s,
            None => {
                self.connection.error(id, -32602, format!("Session not found: {session_id}"));
                return;
            }
        };
        if mode_id != "auto" && mode_id != "readonly" {
            self.connection.error(id, -32602, format!("Invalid or unavailable mode: {mode_id}"));
            return;
        }
        session.lock().mode = mode_id;
        self.connection.response(id, json!({}));
    }

    fn handle_set_model(&self, id: Value, params: SetModeParams) {
        let session_id = match params.session_id {
            Some(s) => s,
            None => {
                self.connection.error(id, -32602, "Missing sessionId");
                return;
            }
        };
        let model_id = match params.model_id {
            Some(m) => m,
            None => {
                self.connection.error(id, -32602, "Missing modelId");
                return;
            }
        };
        let session = match self.sessions.lock().get(&session_id).cloned() {
            Some(s) => s,
            None => {
                self.connection.error(id, -32602, format!("Session not found: {session_id}"));
                return;
            }
        };
        let current_provider = session.lock().provider.clone();
        match providers::resolve_selection(&self.config, &current_provider, &model_id) {
            Ok((provider, model)) => {
                let mut guard = session.lock();
                guard.provider = provider;
                guard.model = model;
                guard.fallback = providers::fallback_for(&self.config, &guard.provider, &guard.model);
                guard.refresh_effective(&self.config);
                drop(guard);
                self.connection.response(id, json!({}));
            }
            Err(e) => {
                self.connection.error(id, -32602, format!("Invalid or unavailable model: {e}"));
            }
        }
    }

    /// Build a fresh agent for one prompt run. A fresh agent per run lets each
    /// run carry its own cancellation token (cersei's cancel token is
    /// single-use), while conversation history is re-seeded via `with_messages`.
    /// `provider`/`model` are the concrete selection to run on — normally the
    /// session's, or a combo fallback entry during a transparent retry.
    fn build_agent(
        self: &Arc<Self>,
        session: &Arc<Mutex<AcpSession>>,
        cancel_token: CancellationToken,
        provider: &str,
        model: &str,
    ) -> anyhow::Result<Arc<Agent>> {
        let (cwd, mode, messages, session_id) = {
            let guard = session.lock();
            (
                guard.cwd.clone(),
                guard.mode.clone(),
                guard.messages.clone(),
                guard.id.clone(),
            )
        };
        let resolved = providers::resolve(&self.config, provider, model)
            .with_context(|| format!("failed to resolve provider '{provider}'"))?;
        // The ACP permission flow is not wired to cersei's InteractivePolicy
        // (permission responses never reach the runner), so only policies that
        // decide autonomously are offered.
        // The agent's Read tool consults this server (the ACP client's fs)
        // before the local disk, so unsaved editor buffers are visible.
        let fs_reader: Arc<dyn crate::subagents::AcpFs> = Arc::clone(self) as Arc<_>;
        providers::build_agent(
            &resolved,
            providers::BuildParams {
                working_dir: cwd,
                max_turns: self.max_turns,
                session_id: Some(session_id),
                messages,
                cancel_token,
                readonly: mode == "readonly",
                parent: Arc::new(parking_lot::Mutex::new(None)),
                followups: Arc::new(parking_lot::Mutex::new(Vec::new())),
                // No TUI consumer for sub-agent activity in ACP mode.
                subagent_events: None,
                fs_reader: Some(fs_reader),
                reasoning: crate::response_format::reasoning_field_for(
                    &self.config,
                    &resolved.model,
                ),
                ask_user_tool: None,
            },
        )
    }

    /// Maximum wait for a client `fs/*` response before giving up. The client
    /// may be slow (e.g. prompting the user for a write), but an unbounded
    /// wait would hang the agent run that triggered it.
    const FS_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

    /// Read a text file from the client's environment (including unsaved
    /// editor state) via `fs/read_text_file`. Requires the client to have
    /// advertised `fs.readTextFile`. `path` must be absolute.
    async fn read_text_file(
        &self,
        session_id: &str,
        path: &str,
        line: Option<u32>,
        limit: Option<u32>,
    ) -> anyhow::Result<String> {
        if !self.client_fs.lock().read_text_file {
            anyhow::bail!("client did not advertise fs.readTextFile capability");
        }
        let params = serde_json::to_value(ReadTextFileParams {
            session_id,
            path,
            line,
            limit,
        })
        .context("failed to serialize fs/read_text_file params")?;
        let result = self.fs_request("fs/read_text_file", params).await?;
        let parsed: ReadTextFileResult = serde_json::from_value(result)
            .context("invalid fs/read_text_file response")?;
        Ok(parsed.content)
    }

    /// Write or create a text file in the client's environment via
    /// `fs/write_text_file`. Requires the client to have advertised
    /// `fs.writeTextFile`. The client creates the file if it doesn't exist.
    async fn write_text_file(
        &self,
        session_id: &str,
        path: &str,
        content: &str,
    ) -> anyhow::Result<()> {
        if !self.client_fs.lock().write_text_file {
            anyhow::bail!("client did not advertise fs.writeTextFile capability");
        }
        let params = serde_json::to_value(WriteTextFileParams {
            session_id,
            path,
            content,
        })
        .context("failed to serialize fs/write_text_file params")?;
        self.fs_request("fs/write_text_file", params).await?;
        Ok(())
    }

    /// Send a `fs/*` request to the client and await the correlated response.
    /// Times out after [`FS_REQUEST_TIMEOUT`] to avoid hanging the run. Errors
    /// from the client (a JSON-RPC error response) propagate as `anyhow`.
    async fn fs_request(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        let (id, rx) = self.connection.request(method, params);
        match tokio::time::timeout(Self::FS_REQUEST_TIMEOUT, rx).await {
            Ok(Ok(RpcResponse::Result(value))) => Ok(value),
            Ok(Ok(RpcResponse::Error(err))) => {
                Err(anyhow::anyhow!("{method} failed: [{code}] {message}", code = err.code, message = err.message))
            }
            // Sender dropped without sending: the connection was torn down.
            Ok(Err(_)) => Err(anyhow::anyhow!("{method} response channel closed")),
            Err(_) => {
                self.connection.forget_request(id);
                Err(anyhow::anyhow!("{method} timed out after {FS_REQUEST_TIMEOUT:?}", FS_REQUEST_TIMEOUT = Self::FS_REQUEST_TIMEOUT))
            }
        }
    }

    async fn handle_prompt(
        self: &Arc<Self>,
        id: Value,
        session_id: String,
        prompt: Vec<PromptContentBlock>,
    ) {
        let session = match self.sessions.lock().get(&session_id).cloned() {
            Some(s) => s,
            None => {
                self.connection
                    .error(id, -32602, format!("Session not found: {session_id}"));
                return;
            }
        };

        let text = prompt_to_text(&prompt);
        if text.is_empty() {
            self.connection.error(id, -32602, "Prompt must not be empty");
            return;
        }

        // Check if this is an interview prompt (starts with /interview).
        let is_interview = text.starts_with("/interview");
        let mut run_text = text.clone();
        let interview_id: Option<String> = if is_interview {
            let tid = uuid_short();
            // Extract the target from the prompt.
            let target = if text.starts_with("/interview ") {
                text["/interview ".len()..].trim().to_string()
            } else {
                String::new()
            };
            if target.is_empty() {
                self.connection.error(id, -32602, crate::interview::EMPTY_TARGET_MESSAGE);
                return;
            }
            // The model runs the wrapped interview prompt (INTERVIEW_BASE_PROMPT
            // + target), not the raw `/interview ...` text — the same routed
            // prompt the TUI and freebuff send.
            run_text = crate::interview::build_interview_prompt(&target);
            let params = InterviewStartedParams {
                interview_id: tid.clone(),
                target,
                base_prompt: crate::interview::INTERVIEW_BASE_PROMPT.to_string(),
            };
            let sid = session_id.clone();
            // Send notification before starting the agent run.
            // (The session might be dropped if the agent run fails early,
            // but we send the notification regardless.)
            self.connection.send_interview_started(&sid, params);
            Some(tid)
        } else {
            None
        };

        // Store the interview ID in the session if this is an interview.
        if let Some(ref iid) = interview_id {
            session.lock().active_interview_id = Some(iid.clone());
        }

        // Cancel any turn still running for this session before starting a new one.
        let cancel_token = CancellationToken::new();
        let run_id = {
            let mut guard = session.lock();
            if let Some((_, prev)) = guard.pending_cancel.take() {
                prev.cancel();
            }
            guard.run_seq += 1;
            guard.pending_cancel = Some((guard.run_seq, cancel_token.clone()));
            guard.run_seq
        };

        // The concrete (provider, model) the run starts on: for a combo
        // selection that's the combo's first entry.
        let (start_provider, start_model) = {
            let guard = session.lock();
            match providers::effective_selection(&self.config, &guard.provider, &guard.model) {
                Ok(sel) => sel,
                Err(e) => {
                    self.clear_pending(&session, run_id);
                    self.connection.error(id, -32000, format!("Failed to start agent: {e}"));
                    return;
                }
            }
        };
        let mut agent =
            match self.build_agent(&session, cancel_token.clone(), &start_provider, &start_model) {
                Ok(a) => a,
                Err(e) => {
                    self.clear_pending(&session, run_id);
                    self.connection
                        .error(id, -32000, format!("Failed to start agent: {e}"));
                    return;
                }
            };

        let mut stream = agent.run_stream(&run_text);

        let mut cancelled = false;
        let mut terminal: Option<TerminalOutcome> = None;
        let mut current_provider = start_provider;
        let mut current_model = start_model;
        // Fallback state is shared across runs of this session (clones share
        // the cooldown map), so a failed entry stays cooled down on retries.
        let fallback = session.lock().fallback.clone();
        // Once the run has produced any output, a retry can't be transparent
        // (partial text/tool results would be duplicated), so fallback only
        // happens on failures before the first event.
        let mut produced_output = false;
        // Interview bookkeeping for the `interview/completed` notification:
        // the spec file written by the agent (from Write tool calls) and the
        // agent's final reply text.
        let mut spec_file_path: Option<String> = None;
        let mut final_reply = String::new();
        loop {
            let event = if cancelled {
                // Keep draining briefly after a cancel so in-flight tool results
                // and the terminal event can still be observed.
                match tokio::time::timeout(CANCEL_GRACE, stream.next()).await {
                    Ok(event) => event,
                    Err(_) => break,
                }
            } else {
                tokio::select! {
                    event = stream.next() => event,
                    _ = cancel_token.cancelled() => {
                        cancelled = true;
                        continue;
                    }
                }
            };

            match event {
                Some(AgentEvent::Complete(output)) => {
                    session.lock().messages = agent.messages();
                    terminal = Some(TerminalOutcome::Complete(output.stop_reason));
                    break;
                }
                Some(AgentEvent::Error(e)) => {
                    // Provider errors / rate limits usually hit on the first
                    // request, before any output — fall back to the next combo
                    // entry in the list (only combos fall back; a plain
                    // selection has a disabled FallbackManager).
                    let next = if produced_output || cancelled {
                        None
                    } else {
                        fallback.next_entry(&current_provider, &current_model)
                    };
                    if let Some(next) = next {
                        fallback.record_failure(&current_provider, &current_model);
                        // Drop the failed run's pushed prompt so the retry
                        // re-pushes it exactly once. The session's selection
                        // (the combo) stays untouched — only the conversation
                        // history is updated.
                        let mut messages = agent.messages();
                        providers::drop_trailing_user_message(&mut messages);
                        session.lock().messages = messages;
                        let rebuilt = self.build_agent(
                            &session,
                            cancel_token.clone(),
                            &next.provider,
                            &next.model,
                        );
                        if let Ok(new_agent) = rebuilt {
                            self.connection.send_update(
                                &session_id,
                                SessionUpdate::AgentMessageChunk {
                                    content: TextContent::new(format!(
                                        "⚠ {} failed ({e}) — retrying on {}",
                                        providers::display_model_id(
                                            &current_provider,
                                            &current_model,
                                        ),
                                        providers::display_model_id(&next.provider, &next.model),
                                    )),
                                    message_id: None,
                                },
                            );
                            // Keep the session metadata in sync: the selection
                            // (the combo) stays, the effective model moves to
                            // the fallback entry.
                            let selection_id = {
                                let guard = session.lock();
                                providers::display_model_id(&guard.provider, &guard.model)
                            };
                            {
                                let mut guard = session.lock();
                                guard.effective_provider = next.provider.clone();
                                guard.effective_model = next.model.clone();
                            }
                            self.connection.send_update(
                                &session_id,
                                SessionUpdate::ModelChanged {
                                    model_id: selection_id,
                                    effective_model_id: providers::display_model_id(
                                        &next.provider,
                                        &next.model,
                                    ),
                                },
                            );
                            stream = new_agent.run_stream(&run_text);
                            agent = new_agent;
                            current_provider = next.provider;
                            current_model = next.model;
                            continue;
                        }
                    }
                    session.lock().messages = agent.messages();
                    terminal = Some(TerminalOutcome::Error(e));
                    break;
                }
                Some(other) => {
                    // Only actual model/tool output blocks a retry — lifecycle
                    // events like TurnStart fire before the provider call.
                    produced_output |= matches!(
                        other,
                        AgentEvent::TextDelta(_)
                            | AgentEvent::ThinkingDelta(_)
                            | AgentEvent::ToolStart { .. }
                            | AgentEvent::ToolEnd { .. }
                    );
                    // Intercept tool calls during an interview: forward
                    // ask_user questions as notifications and remember the
                    // spec file the agent writes (Write tool, `file_path`
                    // input) for the completion notification.
                    if let Some(ref iid) = interview_id {
                        if let AgentEvent::ToolStart { name, id: _, input } = &other {
                            if name == "ask_user" {
                                self.send_interview_question_from_input(&session_id, iid, input);
                            } else if name == "Write"
                                && let Some(path) =
                                    input.get("file_path").and_then(Value::as_str)
                            {
                                spec_file_path = Some(path.to_string());
                            }
                        }
                    }
                    // Accumulate the agent's reply so the completion
                    // notification can report it.
                    if let AgentEvent::TextDelta(text) = &other {
                        final_reply.push_str(text);
                    }
                    self.handle_event(&session, other).await;
                }
                None => {
                    break;
                }
            }
        }
        // Clear our pending slot, unless a newer prompt has already replaced it.
        self.clear_pending(&session, run_id);

        // Send interview/completed notification if this was an interview.
        if let Some(ref iid) = interview_id {
            let status = if cancelled {
                "cancelled"
            } else {
                match &terminal {
                    Some(TerminalOutcome::Complete(_)) => "success",
                    Some(TerminalOutcome::Error(_)) => "failure",
                    None => "cancelled",
                }
            };
            let final_reply = (!final_reply.trim().is_empty()).then_some(final_reply.trim().to_string());
            let params = InterviewCompletedParams {
                interview_id: iid.clone(),
                status: status.to_string(),
                spec_file_path,
                summary: None,
                final_reply,
            };
            self.connection.send_interview_completed(&session_id, params);
            // Clear the interview ID from the session.
            session.lock().active_interview_id = None;
        }

        match terminal {
            Some(TerminalOutcome::Complete(stop_reason)) if !cancelled => {
                self.connection
                    .response(id, json!({ "stopReason": acp_stop_reason(&stop_reason) }));
            }
            Some(TerminalOutcome::Error(e)) if !cancelled => {
                self.connection.error(id, -32000, e);
            }
            _ => {
                self.connection.response(id, json!({ "stopReason": "cancelled" }));
            }
        }
    }

    fn clear_pending(&self, session: &Arc<Mutex<AcpSession>>, run_id: u64) {
        let mut guard = session.lock();
        if guard.pending_cancel.as_ref().is_some_and(|(id, _)| *id == run_id) {
            guard.pending_cancel = None;
        }
    }

    async fn handle_event(&self, session: &Arc<Mutex<AcpSession>>, event: AgentEvent) {
        let session_id = session.lock().id.clone();
        match event {
            AgentEvent::TextDelta(text) => {
                self.connection.send_update(
                    &session_id,
                    SessionUpdate::AgentMessageChunk {
                        content: TextContent::new(text),
                        message_id: None,
                    },
                );
            }
            AgentEvent::ThinkingDelta(text) => {
                self.connection.send_update(
                    &session_id,
                    SessionUpdate::AgentThoughtChunk {
                        content: TextContent::new(text),
                    },
                );
            }
            AgentEvent::ToolStart { name, id, input } => {
                self.connection.send_update(
                    &session_id,
                    SessionUpdate::ToolCall {
                        tool_call_id: id,
                        title: tool_title(&name, &input),
                        status: ToolStatus::InProgress,
                        kind: tool_kind(&name).to_string(),
                        content: Vec::new(),
                    },
                );
            }
            AgentEvent::ToolEnd {
                name,
                id,
                result,
                is_error,
                ..
            } => {
                let status = if is_error {
                    ToolStatus::Failed
                } else {
                    ToolStatus::Completed
                };
                self.connection.send_update(
                    &session_id,
                    SessionUpdate::ToolCallUpdate {
                        tool_call_id: id,
                        title: name.clone(),
                        status,
                        kind: tool_kind(&name).to_string(),
                        content: vec![ToolCallContent::text(result)],
                    },
                );
            }
            AgentEvent::CostUpdate {
                cumulative_cost,
                input_tokens,
                output_tokens,
                ..
            } => {
                self.connection.send_update(
                    &session_id,
                    SessionUpdate::UsageUpdate {
                        used: input_tokens + output_tokens,
                        size: CONTEXT_WINDOW,
                        cost: (cumulative_cost > 0.0).then_some(Cost {
                            amount: cumulative_cost,
                            currency: "USD",
                        }),
                    },
                );
            }
            _ => {}
        }
    }

    /// Convert an `ask_user` tool input (JSON Value) into an
    /// `InterviewQuestionParams` and send the `interview/question`
    /// notification.
    fn send_interview_question_from_input(
        &self,
        session_id: &str,
        interview_id: &str,
        input: &Value,
    ) {
        // The ask_user input has a "questions" array. Parse it into
        // InterviewQuestionParams.
        let questions: Vec<AskUserQuestionParams> = input
            .get("questions")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|q| {
                        let question = q.get("question")?.as_str()?.to_string();
                        let header = q.get("header")?.as_str()?.to_string();
                        let header = if header.is_empty() { None } else { Some(header) };
                        let options = q.get("options")?.as_array().map(|opts| {
                            opts.iter()
                                .filter_map(|o| {
                                    let label = o.get("label")?.as_str()?.to_string();
                                    let desc = o.get("description")?.as_str()?.to_string();
                                    let desc = if desc.is_empty() { None } else { Some(desc) };
                                    Some(AskUserOptionParams { label, description: desc })
                                })
                                .collect()
                        });
                        let multi_select = q.get("multiSelect")?.as_bool();
                        let multi_select = if multi_select == Some(true) { Some(true) } else { None };
                        let validation = if let Some(v) = q.get("validation").and_then(Value::as_object) {
                            let max_length = v.get("maxLength")?.as_u64().map(|n| n as u32);
                            let min_length = v.get("minLength")?.as_u64().map(|n| n as u32);
                            let pattern = v.get("pattern")?.as_str().map(String::from);
                            let pattern_error = v.get("patternError")?.as_str().map(String::from);
                            // Only include if at least one field is present.
                            if max_length.is_none()
                                && min_length.is_none()
                                && pattern.is_none()
                                && pattern_error.is_none()
                            {
                                None
                            } else {
                                Some(AskUserValidationParams {
                                    max_length,
                                    min_length,
                                    pattern,
                                    pattern_error,
                                })
                            }
                        } else {
                            None
                        };
                        Some(AskUserQuestionParams {
                            question,
                            header,
                            options,
                            multi_select,
                            validation,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        if questions.is_empty() {
            return;
        }

        let params = InterviewQuestionParams {
            interview_id: interview_id.to_string(),
            questions,
        };
        self.connection.send_interview_question(session_id, params);
    }
}

/// Lets the agent's Read tool reach the ACP client's filesystem (unsaved
/// editor buffers) via the same `fs/read_text_file` request path. The session
/// id is supplied by the `ToolContext` at execute time.
#[async_trait::async_trait]
impl crate::subagents::AcpFs for AcpServer {
    fn supports_read_text_file(&self) -> bool {
        self.client_fs.lock().read_text_file
    }

    async fn read_text_file(
        &self,
        session_id: &str,
        path: &str,
        line: Option<u32>,
        limit: Option<u32>,
    ) -> anyhow::Result<String> {
        self.read_text_file(session_id, path, line, limit).await
    }

    fn supports_write_text_file(&self) -> bool {
        self.client_fs.lock().write_text_file
    }

    async fn write_text_file(
        &self,
        session_id: &str,
        path: &str,
        content: &str,
    ) -> anyhow::Result<()> {
        self.write_text_file(session_id, path, content).await
    }
}

enum TerminalOutcome {
    Complete(StopReason),
    Error(String),
}

// ─── Helpers ────────────────────────────────────────────────────────────────

fn uuid_short() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn resolve_cwd(cwd: Option<String>) -> PathBuf {
    let cwd = match cwd {
        Some(c) if !c.trim().is_empty() => PathBuf::from(c),
        _ => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };
    if cwd.is_absolute() {
        cwd
    } else {
        std::env::current_dir()
            .map(|base| base.join(cwd.clone()))
            .unwrap_or(cwd)
    }
}

fn prompt_to_text(prompt: &[PromptContentBlock]) -> String {
    let mut out = String::new();
    for block in prompt {
        match block {
            PromptContentBlock::Text { text } => out.push_str(text),
            PromptContentBlock::Image { .. } => out.push_str("\n[image]"),
            PromptContentBlock::Audio { .. } => out.push_str("\n[audio]"),
            PromptContentBlock::Resource { resource } => {
                if let Some(text) = resource.get("text").and_then(Value::as_str) {
                    out.push_str(text);
                } else if let Some(uri) = resource.get("uri").and_then(Value::as_str) {
                    out.push_str(&format!("\n@{uri}"));
                }
            }
            PromptContentBlock::ResourceLink { uri, .. } => {
                out.push_str(&format!("\n@{uri}"));
            }
        }
        out.push('\n');
    }
    out.trim().to_string()
}

fn modes(mode: &str) -> Value {
    json!({
        "availableModes": [
            { "id": "auto", "name": "Auto", "description": "Auto-approves all tools" },
            { "id": "readonly", "name": "Read-only", "description": "Denies tools that modify files or run commands" },
        ],
        "currentModeId": mode,
    })
}

fn acp_stop_reason(stop_reason: &StopReason) -> &'static str {
    match stop_reason {
        StopReason::EndTurn | StopReason::ToolUse | StopReason::StopSequence => "end_turn",
        StopReason::MaxTokens => "max_tokens",
        StopReason::ContentFilter => "error",
    }
}

fn tool_kind(name: &str) -> &'static str {
    match name {
        "Bash" | "PowerShell" => "execute",
        "Write" | "Edit" | "ApplyPatch" | "NotebookEdit" => "edit",
        "Read" | "Grep" | "Glob" | "CodeSearch" | "WebFetch" | "WebSearch" | "ExaSearch" => {
            "read"
        }
        _ => "other",
    }
}

fn tool_title(name: &str, input: &Value) -> String {
    let field = match name {
        "Bash" | "PowerShell" => Some("command"),
        "Read" | "Write" | "Edit" | "ApplyPatch" => Some("file_path"),
        "Glob" | "Grep" | "CodeSearch" => Some("pattern"),
        _ => None,
    };
    match field.and_then(|f| input.get(f)).and_then(Value::as_str) {
        Some(v) if !v.is_empty() => v.to_string(),
        _ => name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ComboEntry, ProviderConfigEntry};

    /// A config with a `test` provider and one combo referencing it; builds
    /// offline (literal key, loopback base URL).
    fn config_with_combo() -> AppConfig {
        let mut config = AppConfig {
            provider: "test".into(),
            model: "test/test-model".into(),
            permissions_mode: "allow_all".into(),
            ..Default::default()
        };
        config.providers.insert(
            "test".into(),
            ProviderConfigEntry {
                base_url: Some("http://127.0.0.1:1".into()),
                api_key: Some("test-key".into()),
                models: vec!["test/test-model".into(), "test/test-2".into()],
                max_tokens: None,
                temperature: None,
                top_p: None,
                extra_body: None,
            },
        );
        config.combos.insert(
            "coding".into(),
            vec![
                ComboEntry {
                    provider: "test".into(),
                    model: "test/test-model".into(),
                },
                ComboEntry {
                    provider: "test".into(),
                    model: "test/test-2".into(),
                },
            ],
        );
        config
    }

    fn session_with(config: &AppConfig) -> AcpSession {
        let (provider, model) = providers::default_selection(config).unwrap();
        let mut session = AcpSession {
            id: "s1".into(),
            cwd: std::env::current_dir().unwrap(),
            provider: provider.clone(),
            model: model.clone(),
            effective_provider: String::new(),
            effective_model: String::new(),
            mode: "auto".into(),
            messages: Vec::new(),
            run_seq: 0,
            pending_cancel: None,
            fallback: providers::fallback_for(config, &provider, &model),
            _mcp_servers: Vec::new(),
            active_interview_id: None,
        };
        session.refresh_effective(config);
        session
    }

    #[test]
    fn refresh_effective_tracks_combo_first_entry() {
        let config = config_with_combo();
        let mut session = session_with(&config);

        // Plain selection: the effective model is the selection itself.
        assert_eq!((session.provider.as_str(), session.model.as_str()), ("test", "test/test-model"));
        assert_eq!(
            (session.effective_provider.as_str(), session.effective_model.as_str()),
            ("test", "test/test-model")
        );

        // Combo selection: the effective model is its first entry.
        session.provider = "combos".into();
        session.model = "coding".into();
        session.refresh_effective(&config);
        assert_eq!((session.provider.as_str(), session.model.as_str()), ("combos", "coding"));
        assert_eq!(
            (session.effective_provider.as_str(), session.effective_model.as_str()),
            ("test", "test/test-model")
        );
    }

    #[tokio::test]
    async fn models_value_exposes_effective_model_id_only_when_different() {
        let config = config_with_combo();
        let (connection, writer) = AcpConnection::new();
        let server = AcpServer {
            connection,
            sessions: Mutex::new(HashMap::new()),
            config: config.clone(),
            default_provider: "test".into(),
            default_model: "test/test-model".into(),
            max_turns: 10,
            client_fs: Mutex::new(FileSystemCapabilities::default()),
        };

        // Plain selection: no effectiveModelId (it equals currentModelId).
        let session = Arc::new(Mutex::new(session_with(&config)));
        let models = server.models_value(&session);
        assert_eq!(models["currentModelId"], "test/test-model");
        assert!(models.get("effectiveModelId").is_none());

        // Combo on a fallback entry: effectiveModelId shows the concrete model
        // while currentModelId keeps the combo selection.
        let mut state = session_with(&config);
        state.provider = "combos".into();
        state.model = "coding".into();
        state.refresh_effective(&config);
        // Simulate a fallback to the second entry.
        state.effective_provider = "test".into();
        state.effective_model = "test/test-2".into();
        let session = Arc::new(Mutex::new(state));
        let models = server.models_value(&session);
        assert_eq!(models["currentModelId"], "combos/coding");
        assert_eq!(models["effectiveModelId"], "test/test-2");

        writer.abort();
    }

    #[test]
    fn model_changed_update_serializes_selection_and_effective() {
        let line = serde_json::to_string(&SessionUpdate::ModelChanged {
            model_id: "combos/coding".into(),
            effective_model_id: "test/test-2".into(),
        })
        .unwrap();
        let value: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["sessionUpdate"], "model_changed");
        assert_eq!(value["modelId"], "combos/coding");
        assert_eq!(value["effectiveModelId"], "test/test-2");
    }

    #[test]
    fn available_commands_update_serializes_per_spec() {
        let line = serde_json::to_string(&SessionUpdate::AvailableCommandsUpdate {
            available_commands: vec![AvailableCommand {
                name: "interview".into(),
                description: "Interview you about a task or spec to produce requirements".into(),
                input: Some(AvailableCommandInput {
                    ty: "text",
                    hint: "what to interview about".into(),
                }),
            }],
        })
        .unwrap();
        let value: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["sessionUpdate"], "available_commands_update");
        assert_eq!(value["availableCommands"][0]["name"], "interview");
        assert_eq!(value["availableCommands"][0]["input"]["type"], "text");
        assert_eq!(value["availableCommands"][0]["input"]["hint"], "what to interview about");
    }

    #[test]
    fn initialize_params_parse_fs_capabilities() {
        // A client advertising both fs methods.
        let params: InitializeParams = serde_json::from_value(json!({
            "protocolVersion": 1,
            "clientCapabilities": {
                "fs": { "readTextFile": true, "writeTextFile": true }
            }
        }))
        .unwrap();
        let fs = params.client_capabilities.unwrap().fs.unwrap();
        assert!(fs.read_text_file);
        assert!(fs.write_text_file);

        // Omitted capabilities default to false (unsupported).
        let params: InitializeParams = serde_json::from_value(json!({
            "protocolVersion": 1,
            "clientCapabilities": {}
        }))
        .unwrap();
        let fs = params.client_capabilities.unwrap().fs.unwrap_or_default();
        assert!(!fs.read_text_file);
        assert!(!fs.write_text_file);

        // Missing clientCapabilities entirely is valid (defaults).
        let params: InitializeParams = serde_json::from_value(json!({ "protocolVersion": 1 }))
            .unwrap();
        assert!(params.client_capabilities.is_none());
    }

    #[tokio::test]
    async fn read_text_file_round_trip_through_client() {
        // Server whose client advertised fs.readTextFile support.
        let (connection, writer) = AcpConnection::new();
        let server = Arc::new(AcpServer {
            connection,
            sessions: Mutex::new(HashMap::new()),
            config: config_with_combo(),
            default_provider: "test".into(),
            default_model: "test/test-model".into(),
            max_turns: 10,
            client_fs: Mutex::new(FileSystemCapabilities {
                read_text_file: true,
                write_text_file: false,
            }),
        });

        // Simulated client: once read_text_file has registered its outbound
        // request (id 1, the first id minted), reply with a content payload.
        let server_for_client = Arc::clone(&server);
        let _client = tokio::spawn(async move {
            // read_text_file sends its request synchronously before awaiting,
            // so a single yield is enough for the registration to land.
            tokio::task::yield_now().await;
            server_for_client
                .connection
                .deliver_response(&json!(1), Some(json!({ "content": "hello\n" })), None);
        });

        let content = server.read_text_file("sess", "/abs/path", None, None).await;
        writer.abort();
        assert_eq!(content.unwrap(), "hello\n");
    }

    #[tokio::test]
    async fn read_text_file_rejected_without_capability() {
        let (connection, writer) = AcpConnection::new();
        let server = AcpServer {
            connection,
            sessions: Mutex::new(HashMap::new()),
            config: config_with_combo(),
            default_provider: "test".into(),
            default_model: "test/test-model".into(),
            max_turns: 10,
            client_fs: Mutex::new(FileSystemCapabilities::default()),
        };
        let err = server.read_text_file("sess", "/abs/path", None, None).await;
        assert!(err.is_err());
        assert!(format!("{:?}", err).contains("readTextFile"));
        writer.abort();
    }

    #[tokio::test]
    async fn write_text_file_rejected_without_capability() {
        let (connection, writer) = AcpConnection::new();
        let server = AcpServer {
            connection,
            sessions: Mutex::new(HashMap::new()),
            config: config_with_combo(),
            default_provider: "test".into(),
            default_model: "test/test-model".into(),
            max_turns: 10,
            client_fs: Mutex::new(FileSystemCapabilities::default()),
        };
        let err = server.write_text_file("sess", "/abs/path", "contents").await;
        assert!(err.is_err());
        assert!(format!("{:?}", err).contains("writeTextFile"));
        writer.abort();
    }

    #[tokio::test]
    async fn fs_request_propagates_client_error() {
        let (connection, writer) = AcpConnection::new();
        let server = Arc::new(AcpServer {
            connection,
            sessions: Mutex::new(HashMap::new()),
            config: config_with_combo(),
            default_provider: "test".into(),
            default_model: "test/test-model".into(),
            max_turns: 10,
            client_fs: Mutex::new(FileSystemCapabilities {
                read_text_file: true,
                write_text_file: true,
            }),
        });
        // Simulate a client that rejects the first fs/read_text_file request.
        let server_for_client = Arc::clone(&server);
        let _client = tokio::spawn(async move {
            tokio::task::yield_now().await;
            server_for_client.connection.deliver_response(
                &json!(1),
                None,
                Some(RpcErrorBody { code: -32603, message: "permission denied".into() }),
            );
        });

        let err = server.read_text_file("sess", "/abs/path", None, None).await;
        assert!(err.is_err());
        let msg = format!("{:?}", err);
        assert!(msg.contains("permission denied"));
        writer.abort();
    }
}
