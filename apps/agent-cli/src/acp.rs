//! ACP (Agent Client Protocol) server over stdio, enabled with `--acp`.
//!
//! Implements the same v1 method surface as `gemini-cli --acp`:
//! - `initialize` (capabilities + auth method advertisement)
//! - `session/new` (with cwd + optional MCP servers)
//! - `session/load` (resume: replays the conversation via `session/update`)
//! - `session/prompt` (streams `session/update` notifications and replies with a stop reason)
//! - `session/set_config_option` (plus `session/set_mode` and `unstable_set_session_model`)
//! - `session/cancel` (notification)
//!
//! Requests are newline-delimited JSON-RPC 2.0 on stdin; responses and
//! `session/update` notifications are newline-delimited JSON-RPC 2.0 on stdout.

use crate::cli_commands::Cli;
use crate::config::AppConfig;
use anyhow::Context as _;
use cersei::events::AgentEvent;
use cersei::tools::permissions::{AllowAll, AllowReadOnly};
use cersei::types::{Message, Role, StopReason};
use cersei::{Agent, OpenAi};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

const PROTOCOL_VERSION: i64 = 1;
/// Approximate provider context window, reported in `usage_update`.
const CONTEXT_WINDOW: u64 = 128_000;
/// How long to keep draining the agent stream after a cancel request before
/// replying anyway (tool executions in cersei cannot always be aborted).
const CANCEL_GRACE: Duration = Duration::from_secs(2);
const POOLSIDE_BASE_URL: &str = "https://inference.poolside.ai/v1";
const DEFAULT_MODEL: &str = "poolside/laguna-xs-2.1";

// ─── JSON-RPC 2.0 wire types ────────────────────────────────────────────────

#[derive(Deserialize)]
struct RpcMessage {
    method: Option<String>,
    id: Option<Value>,
    #[serde(default)]
    params: Option<Value>,
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

#[derive(Serialize)]
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

/// Serializes all writes to stdout so responses and streaming notifications
/// from concurrent tasks stay ordered and never interleave mid-line.
struct AcpConnection {
    tx: tokio::sync::mpsc::UnboundedSender<String>,
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
        (Self { tx }, writer)
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
}

#[derive(Serialize)]
struct Cost {
    amount: f64,
    currency: &'static str,
}

// ─── Request params ─────────────────────────────────────────────────────────

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
    model: String,
    mode: String,
    messages: Vec<Message>,
    /// Monotonic run counter, used to disambiguate pending prompts.
    run_seq: u64,
    /// (run id, cancellation token) for the prompt currently streaming, if any.
    pending_cancel: Option<(u64, CancellationToken)>,
    _mcp_servers: Vec<cersei::mcp::McpServerConfig>,
}

// ─── Server ─────────────────────────────────────────────────────────────────

struct AcpServer {
    connection: AcpConnection,
    sessions: Mutex<HashMap<String, Arc<Mutex<AcpSession>>>>,
    api_key: String,
    default_model: String,
    max_turns: u32,
}

pub async fn run_server(_cli: Cli, config: AppConfig) -> anyhow::Result<()> {
    // Prefer the env var so no subprocess (`pool-key-file`) is spawned, which
    // would print to stdout and corrupt the NDJSON stream. Only fall back to
    // the subprocess at startup, before the protocol begins.
    let api_key = std::env::var("POOLSIDE_API_KEY").unwrap_or_else(|_| {
        ai_providers::poolside::load_key().unwrap_or_default()
    });
    if api_key.is_empty() {
        anyhow::bail!("POOLSIDE_API_KEY is not set and could not be loaded");
    }

    let default_model = if config.model.is_empty() || config.model == "auto" {
        DEFAULT_MODEL.to_string()
    } else {
        config.model.clone()
    };

    let (connection, writer_task) = AcpConnection::new();
    let server = Arc::new(AcpServer {
        connection,
        sessions: Mutex::new(HashMap::new()),
        api_key,
        default_model,
        max_turns: config.max_turns,
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
        let method = match rpc.method {
            Some(m) => m,
            None => return, // not a request or notification
        };
        let params = rpc.params.unwrap_or(Value::Null);

        match method.as_str() {
            "initialize" => {
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

        let session = Arc::new(Mutex::new(AcpSession {
            id: session_id.clone(),
            cwd,
            model: self.default_model.clone(),
            mode: "auto".to_string(),
            messages: Vec::new(),
            run_seq: 0,
            pending_cancel: None,
            _mcp_servers: mcp_servers,
        }));
        self.sessions.lock().insert(session_id.clone(), Arc::clone(&session));

        let guard = session.lock();
        let model = guard.model.clone();
        let mode = guard.mode.clone();
        drop(guard);

        self.connection.response(
            id,
            json!({
                "sessionId": session_id,
                "modes": modes(&mode),
                "models": models(&model),
                "configOptions": config_options(&model, &mode),
            }),
        );
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
            "model" => {
                let model = match value.as_str() {
                    Some(m) if m == guard.model => m,
                    _ => {
                        self.connection
                            .error(id, -32602, "Invalid value for config option 'model'");
                        return;
                    }
                };
                guard.model = model.to_string();
            }
            other => {
                self.connection.error(id, -32602, format!("Unknown config option: {other}"));
                return;
            }
        }
        let model = guard.model.clone();
        let mode = guard.mode.clone();
        drop(guard);

        self.connection
            .response(id, json!({ "configOptions": config_options(&model, &mode) }));
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
        if model_id != session.lock().model {
            self.connection.error(id, -32602, format!("Invalid or unavailable model: {model_id}"));
            return;
        }
        session.lock().model = model_id;
        self.connection.response(id, json!({}));
    }

    /// Build a fresh agent for one prompt run. A fresh agent per run lets each
    /// run carry its own cancellation token (cersei's cancel token is
    /// single-use), while conversation history is re-seeded via `with_messages`.
    fn build_agent(
        &self,
        model: &str,
        mode: &str,
        cwd: &PathBuf,
        session_id: &str,
        messages: Vec<Message>,
        cancel_token: CancellationToken,
    ) -> anyhow::Result<Arc<Agent>> {
        let provider = OpenAi::builder()
            .base_url(POOLSIDE_BASE_URL)
            .api_key(self.api_key.clone())
            .model(model)
            .build()
            .context("failed to build provider")?;

        let mut builder = Agent::builder()
            .provider(provider)
            .model(model)
            .tools(cersei::tools::coding())
            .max_turns(self.max_turns)
            .working_dir(cwd)
            .session_id(session_id)
            .cancel_token(cancel_token)
            .with_messages(messages);
        // The ACP permission flow is not wired to cersei's InteractivePolicy
        // (permission responses never reach the runner), so only policies that
        // decide autonomously are offered.
        builder = match mode {
            "readonly" => builder.permission_policy(AllowReadOnly),
            _ => builder.permission_policy(AllowAll),
        };

        Ok(Arc::new(
            builder.build().context("failed to build agent")?,
        ))
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

        let (messages, mode, model, cwd) = {
            let guard = session.lock();
            (guard.messages.clone(), guard.mode.clone(), guard.model.clone(), guard.cwd.clone())
        };

        let agent = match self.build_agent(
            &model,
            &mode,
            &cwd,
            &session_id,
            messages,
            cancel_token.clone(),
        ) {
            Ok(a) => a,
            Err(e) => {
                self.clear_pending(&session, run_id);
                self.connection.error(id, -32000, format!("Failed to start agent: {e}"));
                return;
            }
        };

        let mut stream = agent.run_stream(&text);

        let mut cancelled = false;
        let mut terminal: Option<TerminalOutcome> = None;
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
                    session.lock().messages = agent.messages();
                    terminal = Some(TerminalOutcome::Error(e));
                    break;
                }
                Some(other) => {
                    self.handle_event(&session, other).await;
                }
                None => {
                    break;
                }
            }
        }
        // Clear our pending slot, unless a newer prompt has already replaced it.
        self.clear_pending(&session, run_id);

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

fn models(model: &str) -> Value {
    json!({
        "availableModels": [
            { "modelId": model, "name": model },
        ],
        "currentModelId": model,
    })
}

fn config_options(model: &str, mode: &str) -> Value {
    json!([
        {
            "id": "mode",
            "name": "Mode",
            "description": "Tool permission mode",
            "category": "mode",
            "type": "select",
            "currentValue": mode,
            "options": [
                { "value": "auto", "name": "Auto", "description": "Auto-approves all tools" },
                { "value": "readonly", "name": "Read-only", "description": "Denies tools that modify files or run commands" },
            ],
        },
        {
            "id": "model",
            "name": "Model",
            "category": "model",
            "type": "select",
            "currentValue": model,
            "options": [
                { "value": model, "name": model },
            ],
        },
    ])
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
