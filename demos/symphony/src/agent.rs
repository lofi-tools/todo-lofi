//! Coding-agent app-server client (Section 10).
//!
//! The client launches `codex.command` via `bash -lc` inside the per-issue
//! workspace and speaks the Codex app-server protocol: newline-delimited
//! JSON-RPC 2.0 messages over stdio, with the `jsonrpc` header omitted.
//! Protocol shape is owned by the targeted Codex version; this module keeps the
//! Symphony-specific behaviour (workspace cwd, prompt construction, approval
//! policy, telemetry extraction) separate from transport parsing.

use crate::config::CodexConfig;
use crate::domain::{Issue, LiveSession, TokenUsage};
use crate::error::Result;
use crate::error::SymphonyError::*;
use log::{debug, info, warn};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

/// JSON-RPC error code used for unsupported requests, so the session keeps running.
const METHOD_NOT_FOUND: i64 = -32601;

/// Continuation guidance for later turns in the same worker session (Section 7.1).
pub const CONTINUATION_GUIDANCE: &str = "The previous turn ended. Re-check the issue state in the \
tracker. If it is still active, continue the work in this workspace until it reaches the workflow's \
handoff state. Do not repeat work that is already complete, and do not resend the original task \
prompt.";

/// A client-side tool advertised to the coding agent (Section 10.5).
#[async_trait::async_trait]
pub trait ClientTool: Send + Sync {
    /// Tool name advertised to the agent.
    fn name(&self) -> &str;
    /// Human-readable description.
    fn description(&self) -> &str;
    /// JSON Schema for the tool input.
    fn input_schema(&self) -> Value;
    /// Execute the tool call. Returns a JSON tool result or a failure message.
    async fn call(&self, arguments: Value) -> std::result::Result<Value, String>;
}

/// Structured events emitted upstream to the orchestrator (Section 10.4).
#[derive(Debug, Clone)]
pub enum AgentEvent {
    SessionStarted {
        session_id: String,
        thread_id: String,
        turn_id: String,
        pid: u32,
    },
    StartupFailed {
        error: String,
    },
    TurnStarted {
        turn_id: String,
    },
    TurnCompleted {
        turn_id: String,
    },
    TurnFailed {
        turn_id: String,
        error: String,
    },
    TurnCancelled {
        turn_id: String,
    },
    TurnEndedWithError {
        turn_id: String,
        error: String,
    },
    TurnInputRequired {
        turn_id: String,
        prompt: String,
    },
    ApprovalAutoApproved {
        turn_id: String,
        request_type: String,
    },
    UnsupportedToolCall {
        turn_id: String,
        tool_name: String,
    },
    Notification {
        message: String,
    },
    OtherMessage {
        message: String,
    },
    Malformed {
        message: String,
    },
    /// Absolute thread token totals. Delta-style payloads are ignored.
    TokenUsage { usage: TokenUsage },
    /// Latest rate-limit payload.
    RateLimits { payload: Value },
}

impl AgentEvent {
    /// Stable event name for logs and observability surfaces.
    pub fn name(&self) -> &'static str {
        match self {
            AgentEvent::SessionStarted { .. } => "session_started",
            AgentEvent::StartupFailed { .. } => "startup_failed",
            AgentEvent::TurnStarted { .. } => "turn_started",
            AgentEvent::TurnCompleted { .. } => "turn_completed",
            AgentEvent::TurnFailed { .. } => "turn_failed",
            AgentEvent::TurnCancelled { .. } => "turn_cancelled",
            AgentEvent::TurnEndedWithError { .. } => "turn_ended_with_error",
            AgentEvent::TurnInputRequired { .. } => "turn_input_required",
            AgentEvent::ApprovalAutoApproved { .. } => "approval_auto_approved",
            AgentEvent::UnsupportedToolCall { .. } => "unsupported_tool_call",
            AgentEvent::Notification { .. } => "notification",
            AgentEvent::OtherMessage { .. } => "other_message",
            AgentEvent::Malformed { .. } => "malformed",
            AgentEvent::TokenUsage { .. } => "token_usage",
            AgentEvent::RateLimits { .. } => "rate_limits",
        }
    }
}

/// How a streamed turn ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnTerminal {
    Completed,
    Cancelled,
    Failed(String),
}

/// Live view of a running coding-agent session.
#[derive(Debug, Clone)]
pub struct SessionIdentity {
    pub thread_id: String,
    pub turn_id: String,
    pub session_id: String,
    pub pid: u32,
}

enum PumpTarget {
    Response(Value),
    Turn(String),
}

enum PumpOutcome {
    Response(Value),
    TurnEnded(TurnTerminal),
}

enum Incoming {
    Response { id: Value, payload: Value },
    Notification { method: String, params: Value },
    Request { id: Value, method: String, params: Value },
    Malformed { line: String },
    Closed { message: String },
}

/// One step of the agent pump loop.
enum Step {
    Cancel(bool),
    Message(Option<Incoming>),
    TimedOut,
}

/// Client for a Codex app-server subprocess.
pub struct AgentClient {
    child: Child,
    stdin: ChildStdin,
    incoming: mpsc::Receiver<Incoming>,
    stdout_task: JoinHandle<()>,
    stderr_task: JoinHandle<()>,
    next_request_id: i64,
    thread_id: Option<String>,
    current_turn_id: Option<String>,
    cancel_requested: bool,
    /// Whether `session_started` has been emitted for this worker session.
    announced_session: bool,
    tools: Vec<Arc<dyn ClientTool>>,
    codex: CodexConfig,
    /// Absolute per-issue workspace used as the agent cwd.
    workspace: PathBuf,
}

impl AgentClient {
    /// Launch the app-server subprocess in `workspace` and initialize the session.
    pub async fn start(
        codex: &CodexConfig,
        workspace: &Path,
        cwd: &Path,
        issue: &Issue,
        tools: Vec<Arc<dyn ClientTool>>,
    ) -> Result<Self> {
        // Invariant 1: the agent runs only in the per-issue workspace directory.
        if crate::config::normalize_path(cwd) != crate::config::normalize_path(workspace) {
            return Err(InvalidWorkspaceCwd {
                path: cwd.to_string_lossy().into_owned(),
            });
        }
        if !workspace.is_absolute() {
            return Err(InvalidWorkspaceCwd {
                path: workspace.to_string_lossy().into_owned(),
            });
        }

        let mut command = Command::new("bash");
        command
            .arg("-lc")
            .arg(&codex.command)
            .current_dir(workspace)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        let mut child = command.spawn().map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                CodexNotFound {
                    command: codex.command.clone(),
                }
            } else {
                AgentLaunchError {
                    source: Box::new(error),
                }
            }
        })?;

        let pid = child.id().unwrap_or_default();
        let Some(stdin) = child.stdin.take() else {
            return Err(AgentLaunchError {
                source: Box::new(std::io::Error::other("agent stdin was not captured")),
            });
        };
        let Some(stdout) = child.stdout.take() else {
            return Err(AgentLaunchError {
                source: Box::new(std::io::Error::other("agent stdout was not captured")),
            });
        };
        let Some(stderr) = child.stderr.take() else {
            return Err(AgentLaunchError {
                source: Box::new(std::io::Error::other("agent stderr was not captured")),
            });
        };

        let (incoming_tx, incoming) = mpsc::channel(512);
        let stdout_task = spawn_stdout_reader(stdout, incoming_tx.clone());
        // Diagnostic stderr stays on its own stream, away from the protocol.
        let stderr_task = spawn_stderr_reader(stderr);

        let mut client = Self {
            child,
            stdin,
            incoming,
            stdout_task,
            stderr_task,
            next_request_id: 1,
            thread_id: None,
            current_turn_id: None,
            cancel_requested: false,
            announced_session: false,
            tools,
            codex: codex.clone(),
            workspace: workspace.to_path_buf(),
        };

        info!(
            target: "symphony",
            "issue_identifier={} outcome=started agent=codex_app_server pid={pid} cwd={}",
            issue.identifier,
            workspace.display()
        );

        client.initialize().await?;
        let thread_id = client.start_thread(workspace, issue).await?;
        client.thread_id = Some(thread_id);

        Ok(client)
    }

    /// The identity of the current thread and turn, when available.
    pub fn identity(&self) -> Option<SessionIdentity> {
        let thread_id = self.thread_id.clone()?;
        let turn_id = self.current_turn_id.clone()?;
        Some(SessionIdentity {
            session_id: LiveSession::compose_session_id(&thread_id, &turn_id),
            thread_id,
            turn_id,
            pid: self.child.id().unwrap_or_default(),
        })
    }

    /// Send the `initialize` handshake and its `initialized` acknowledgement.
    async fn initialize(&mut self) -> Result<()> {
        let mut capabilities = serde_json::Map::new();
        // Dynamic tools are gated behind the experimental capability.
        capabilities.insert("experimentalApi".into(), Value::Bool(!self.tools.is_empty()));

        let params = json!({
            "clientInfo": {
                "name": "symphony",
                "title": "Symphony",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "capabilities": Value::Object(capabilities),
        });

        self.request("initialize", params).await?;
        self.notify("initialized", json!({})).await
    }

    /// Create a new thread rooted at the per-issue workspace.
    async fn start_thread(&mut self, workspace: &Path, issue: &Issue) -> Result<String> {
        let mut params = serde_json::Map::new();
        params.insert(
            "cwd".into(),
            Value::String(workspace.to_string_lossy().into_owned()),
        );
        params.insert(
            "approvalPolicy".into(),
            Value::String(self.codex.approval_policy.clone()),
        );
        params.insert(
            "sandbox".into(),
            Value::String(map_sandbox_mode(&self.codex.thread_sandbox).to_string()),
        );
        params.insert("serviceName".into(), Value::String("symphony".into()));

        if !self.tools.is_empty() {
            params.insert(
                "dynamicTools".into(),
                Value::Array(
                    self.tools
                        .iter()
                        .map(|tool| {
                            json!({
                                "name": tool.name(),
                                "description": tool.description(),
                                "inputSchema": tool.input_schema(),
                            })
                        })
                        .collect(),
                ),
            );
        }

        let response = self.request("thread/start", Value::Object(params)).await?;
        let thread_id = response
            .get("thread")
            .and_then(|thread| thread.get("id"))
            .and_then(Value::as_str)
            .ok_or_else(|| ResponseError {
                method: "thread/start".to_string(),
                message: "response did not include thread.id".to_string(),
            })?
            .to_string();

        // Issue-identifying thread titles are best-effort; not all versions support them.
        let title = format!("{}: {}", issue.identifier, issue.title);
        if let Err(error) = self
            .request(
                "thread/name/set",
                json!({ "threadId": thread_id, "name": title }),
            )
            .await
        {
            debug!(
                target: "symphony",
                "issue_identifier={} outcome=failed method=thread/name/set error={error}",
                issue.identifier
            );
        }

        Ok(thread_id)
    }

    /// Start a turn and stream it to completion.
    pub async fn run_turn<F: FnMut(AgentEvent)>(
        &mut self,
        prompt: &str,
        cancel: &mut watch::Receiver<bool>,
        on_event: &mut F,
    ) -> Result<TurnTerminal> {
        let Some(thread_id) = self.thread_id.clone() else {
            return Err(ResponseError {
                method: "turn/start".to_string(),
                message: "no active thread".to_string(),
            });
        };
        // Cancellation can land between turns; do not start a turn we know is cancelled.
        if *cancel.borrow() {
            return Err(TurnCancelled);
        }
        self.cancel_requested = false;

        let workspace = self.workspace.to_string_lossy().into_owned();
        let params = json!({
            "threadId": thread_id,
            "input": [ { "type": "text", "text": prompt } ],
            "cwd": workspace,
            "approvalPolicy": self.codex.approval_policy,
            "sandboxPolicy": turn_sandbox_policy(&self.codex.turn_sandbox_policy, &workspace),
        });

        let request_id = self.next_id();
        self.send(json!({ "method": "turn/start", "id": request_id, "params": params }))
            .await?;

        let read_deadline = Instant::now() + Duration::from_millis(self.codex.read_timeout_ms);
        let response = match self
            .pump(
                PumpTarget::Response(json!(request_id)),
                read_deadline,
                cancel,
                on_event,
            )
            .await?
        {
            PumpOutcome::Response(response) => response,
            PumpOutcome::TurnEnded(_) => {
                return Err(ResponseError {
                    method: "turn/start".to_string(),
                    message: "agent ended the turn before acknowledging turn/start".to_string(),
                });
            }
        };

        let turn_id = response
            .get("turn")
            .and_then(|turn| turn.get("id"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| ResponseError {
                method: "turn/start".to_string(),
                message: "response did not include turn.id".to_string(),
            })?;
        self.current_turn_id = Some(turn_id.clone());

        // `session_id` needs both identities, so the session is announced on the
        // first turn rather than at process start (Section 10.2).
        if !self.announced_session {
            self.announced_session = true;
            on_event(AgentEvent::SessionStarted {
                session_id: LiveSession::compose_session_id(&thread_id, &turn_id),
                thread_id: thread_id.clone(),
                turn_id: turn_id.clone(),
                pid: self.child.id().unwrap_or_default(),
            });
        }

        let turn_deadline = Instant::now() + Duration::from_millis(self.codex.turn_timeout_ms);
        let terminal = match self
            .pump(
                PumpTarget::Turn(turn_id.clone()),
                turn_deadline,
                cancel,
                on_event,
            )
            .await?
        {
            PumpOutcome::TurnEnded(terminal) => terminal,
            PumpOutcome::Response(_) => {
                return Err(ResponseError {
                    method: "turn/start".to_string(),
                    message: "unexpected response while streaming a turn".to_string(),
                });
            }
        };

        let session_id = self
            .identity()
            .map(|identity| identity.session_id)
            .unwrap_or_else(|| thread_id.clone());
        match &terminal {
            TurnTerminal::Completed => info!(
                target: "symphony",
                "session_id={session_id} turn_id={turn_id} outcome=turn_completed"
            ),
            TurnTerminal::Cancelled => info!(
                target: "symphony",
                "session_id={session_id} turn_id={turn_id} outcome=turn_cancelled"
            ),
            TurnTerminal::Failed(message) => warn!(
                target: "symphony",
                "session_id={session_id} turn_id={turn_id} outcome=turn_failed error={message}"
            ),
        }

        Ok(terminal)
    }

    /// Terminate the app-server subprocess.
    pub async fn stop(&mut self) {
        if let Err(error) = self.child.start_kill() {
            debug!(target: "symphony", "outcome=failed action=kill_agent error={error}");
        }
        if let Err(error) = self.child.wait().await {
            debug!(target: "symphony", "outcome=failed action=wait_agent error={error}");
        }
        self.stdout_task.abort();
        self.stderr_task.abort();
    }

    fn next_id(&mut self) -> i64 {
        let id = self.next_request_id;
        self.next_request_id += 1;
        id
    }

    async fn send(&mut self, message: Value) -> Result<()> {
        let mut line = serde_json::to_string(&message).map_err(|error| AgentCommunicationError {
            source: Box::new(error),
        })?;
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|error| AgentCommunicationError {
                source: Box::new(error),
            })?;
        self.stdin
            .flush()
            .await
            .map_err(|error| AgentCommunicationError {
                source: Box::new(error),
            })
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        self.send(json!({ "method": method, "params": params })).await
    }

    /// Send a request and wait for its response, honouring `read_timeout_ms`.
    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id();
        self.send(json!({ "method": method, "id": id, "params": params }))
            .await?;

        let deadline = Instant::now() + Duration::from_millis(self.codex.read_timeout_ms);
        // The sender must outlive the pump: a dropped sender makes
        // `cancel.changed()` permanently ready, which would starve the
        // message branch of the biased `select!` below.
        let (_cancel_tx, mut cancel) = watch::channel(false);
        let mut on_event = |_event: AgentEvent| {};

        match self
            .pump(PumpTarget::Response(json!(id)), deadline, &mut cancel, &mut on_event)
            .await
        {
            Ok(PumpOutcome::Response(response)) => Ok(response),
            Ok(PumpOutcome::TurnEnded(_)) => Err(ResponseError {
                method: method.to_string(),
                message: "unexpected turn completion".to_string(),
            }),
            Err(error) => Err(error),
        }
    }

    /// Pump messages until the target is reached, the deadline passes, or the
    /// agent asks for input that this implementation refuses to provide.
    async fn pump<F: FnMut(AgentEvent)>(
        &mut self,
        target: PumpTarget,
        deadline: Instant,
        cancel: &mut watch::Receiver<bool>,
        on_event: &mut F,
    ) -> Result<PumpOutcome> {
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(response_timeout_error(&target, &self.codex));
            }

            // The awaited futures borrow only `self.incoming`/`cancel`, so the
            // borrow ends here and the handler below can take `&mut self`.
            let step = tokio::select! {
                biased;

                changed = cancel.changed() => {
                    // A dropped sender means no one can cancel us any more.
                    Step::Cancel(changed.is_ok() && *cancel.borrow())
                }

                message = self.incoming.recv() => Step::Message(message),

                _ = tokio::time::sleep(remaining) => Step::TimedOut,
            };

            match step {
                Step::Cancel(true) => {
                    if !self.cancel_requested {
                        self.cancel_requested = true;
                        self.interrupt_turn().await?;
                    }
                }
                Step::Cancel(false) => {}
                Step::TimedOut => {
                    return Err(response_timeout_error(&target, &self.codex));
                }
                Step::Message(None) => {
                    return Err(PortExit {
                        message: "agent subprocess closed its output stream".to_string(),
                    });
                }
                Step::Message(Some(Incoming::Closed { message })) => {
                    if let PumpTarget::Turn(turn_id) = &target {
                        on_event(AgentEvent::TurnEndedWithError {
                            turn_id: turn_id.clone(),
                            error: message.clone(),
                        });
                    }
                    return Err(PortExit { message });
                }
                Step::Message(Some(Incoming::Malformed { line })) => {
                    on_event(AgentEvent::Malformed { message: line });
                }
                Step::Message(Some(Incoming::Response { id, payload })) => {
                    match &target {
                        PumpTarget::Response(expected) if &id == expected => {
                            return response_outcome(payload).map(PumpOutcome::Response);
                        }
                        _ => {
                            debug!(
                                target: "symphony",
                                "outcome=ignored message=unexpected_response id={id}"
                            );
                        }
                    }
                }
                Step::Message(Some(Incoming::Notification { method, params })) => {
                    if let Some(terminal) = self
                        .handle_notification(&method, params, on_event)
                        .await?
                        && matches!(target, PumpTarget::Turn(_))
                    {
                        return Ok(PumpOutcome::TurnEnded(terminal));
                    }
                }
                Step::Message(Some(Incoming::Request { id, method, params })) => {
                    self.handle_request(id, method, params, on_event).await?;
                }
            }
        }
    }

    /// Ask the app-server to interrupt the active turn.
    async fn interrupt_turn(&mut self) -> Result<()> {
        let Some(thread_id) = self.thread_id.clone() else {
            return Ok(());
        };
        let params = match &self.current_turn_id {
            Some(turn_id) => json!({ "threadId": thread_id, "turnId": turn_id }),
            None => json!({ "threadId": thread_id }),
        };
        let id = self.next_id();
        self.send(json!({ "method": "turn/interrupt", "id": id, "params": params }))
            .await
    }

    async fn handle_notification<F: FnMut(AgentEvent)>(
        &mut self,
        method: &str,
        params: Value,
        on_event: &mut F,
    ) -> Result<Option<TurnTerminal>> {
        match method {
            "turn/started" => {
                if let Some(turn_id) = params
                    .get("turn")
                    .and_then(|turn| turn.get("id"))
                    .and_then(Value::as_str)
                {
                    self.current_turn_id = Some(turn_id.to_string());
                    on_event(AgentEvent::TurnStarted {
                        turn_id: turn_id.to_string(),
                    });
                }
                Ok(None)
            }
            "turn/completed" => {
                let turn_id = params
                    .get("turn")
                    .and_then(|turn| turn.get("id"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let status = params
                    .get("turn")
                    .and_then(|turn| turn.get("status"))
                    .and_then(Value::as_str)
                    .unwrap_or("completed")
                    .to_string();

                match status.as_str() {
                    "interrupted" => {
                        on_event(AgentEvent::TurnCancelled {
                            turn_id: turn_id.clone(),
                        });
                        Ok(Some(TurnTerminal::Cancelled))
                    }
                    "failed" => {
                        let message = turn_error_message(&params)
                            .unwrap_or_else(|| "turn failed".to_string());
                        on_event(AgentEvent::TurnFailed {
                            turn_id: turn_id.clone(),
                            error: message.clone(),
                        });
                        Ok(Some(TurnTerminal::Failed(message)))
                    }
                    _ => {
                        if self.cancel_requested {
                            on_event(AgentEvent::TurnCancelled {
                                turn_id: turn_id.clone(),
                            });
                            return Ok(Some(TurnTerminal::Cancelled));
                        }
                        on_event(AgentEvent::TurnCompleted {
                            turn_id: turn_id.clone(),
                        });
                        Ok(Some(TurnTerminal::Completed))
                    }
                }
            }
            "turn/failed" => {
                let turn_id = params
                    .get("turnId")
                    .and_then(Value::as_str)
                    .or_else(|| self.current_turn_id.as_deref())
                    .unwrap_or_default()
                    .to_string();
                let message =
                    turn_error_message(&params).unwrap_or_else(|| "turn failed".to_string());
                on_event(AgentEvent::TurnFailed {
                    turn_id,
                    error: message.clone(),
                });
                Ok(Some(TurnTerminal::Failed(message)))
            }
            "turn/cancelled" => {
                let turn_id = params
                    .get("turnId")
                    .and_then(Value::as_str)
                    .or_else(|| self.current_turn_id.as_deref())
                    .unwrap_or_default()
                    .to_string();
                on_event(AgentEvent::TurnCancelled {
                    turn_id,
                });
                Ok(Some(TurnTerminal::Cancelled))
            }
            "thread/tokenUsage/updated" => {
                if let Some(usage) = extract_token_usage(&params) {
                    on_event(AgentEvent::TokenUsage { usage });
                }
                Ok(None)
            }
            "account/rateLimits/updated" => {
                on_event(AgentEvent::RateLimits { payload: params });
                Ok(None)
            }
            "thread/started" => {
                if let Some(thread_id) = params
                    .get("thread")
                    .and_then(|thread| thread.get("id"))
                    .and_then(Value::as_str)
                {
                    self.thread_id = Some(thread_id.to_string());
                }
                Ok(None)
            }
            "error" => {
                let message = params
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("agent reported an error")
                    .to_string();
                on_event(AgentEvent::OtherMessage {
                    message: message.clone(),
                });
                Ok(None)
            }
            other => {
                on_event(AgentEvent::Notification {
                    message: summarize_notification(other, &params),
                });
                Ok(None)
            }
        }
    }

    async fn handle_request<F: FnMut(AgentEvent)>(
        &mut self,
        id: Value,
        method: String,
        params: Value,
        on_event: &mut F,
    ) -> Result<()> {
        let turn_id = self.current_turn_id.clone().unwrap_or_default();

        match method.as_str() {
            // Documented high-trust posture: auto-approve agent approvals so a
            // run never stalls waiting on an operator.
            "item/commandExecution/requestApproval" => {
                self.respond(&id, json!({ "decision": "acceptForSession" }))
                    .await?;
                on_event(AgentEvent::ApprovalAutoApproved {
                    turn_id,
                    request_type: "command_execution".to_string(),
                });
            }
            "item/fileChange/requestApproval" => {
                self.respond(&id, json!({ "decision": "acceptForSession" }))
                    .await?;
                on_event(AgentEvent::ApprovalAutoApproved {
                    turn_id,
                    request_type: "file_change".to_string(),
                });
            }
            // Permission escalation stays inside the configured sandbox.
            "item/permissions/requestApproval" => {
                self.respond(&id, json!({ "decision": "decline" })).await?;
                on_event(AgentEvent::Notification {
                    message: "declined agent permission escalation request".to_string(),
                });
            }
            // User input cannot be satisfied; fail the turn instead of stalling.
            "tool/requestUserInput" => {
                self.respond_error(&id, METHOD_NOT_FOUND, "user input is not supported")
                    .await?;
                let prompt = params
                    .get("questions")
                    .map(|questions| questions.to_string())
                    .unwrap_or_else(|| params.to_string());
                on_event(AgentEvent::TurnInputRequired {
                    turn_id: turn_id.clone(),
                    prompt: prompt.clone(),
                });
                return Err(TurnInputRequired { prompt });
            }
            "item/tool/call" => {
                self.handle_tool_call(id, params, on_event).await?;
            }
            other => {
                // Answer unknown server requests so the session never stalls.
                self.respond_error(
                    &id,
                    METHOD_NOT_FOUND,
                    &format!("unsupported agent request `{other}`"),
                )
                .await?;
                on_event(AgentEvent::Notification {
                    message: format!("unsupported agent request `{other}`"),
                });
            }
        }

        Ok(())
    }

    /// Handle a dynamic tool call, including the unsupported-tool failure path.
    ///
    /// The response envelope follows the targeted Codex version's
    /// `item/tool/call` contract (text content plus an error flag).
    async fn handle_tool_call<F: FnMut(AgentEvent)>(
        &mut self,
        id: Value,
        params: Value,
        on_event: &mut F,
    ) -> Result<()> {
        let turn_id = self.current_turn_id.clone().unwrap_or_default();
        let tool_name = params
            .get("tool")
            .or_else(|| params.get("toolName"))
            .or_else(|| params.get("name"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let arguments = params
            .get("arguments")
            .or_else(|| params.get("input"))
            .or_else(|| params.get("args"))
            .cloned()
            .unwrap_or_else(|| json!({}));

        let tool = self
            .tools
            .iter()
            .find(|tool| tool.name() == tool_name)
            .cloned();

        match tool {
            Some(tool) => match tool.call(arguments).await {
                Ok(value) => {
                    self.respond(&id, tool_result(&value.to_string(), false))
                        .await?;
                }
                Err(message) => {
                    warn!(
                        target: "symphony",
                        "tool={} outcome=failed error={message}",
                        tool_name
                    );
                    self.respond(&id, tool_result(&message, true)).await?;
                }
            },
            None => {
                on_event(AgentEvent::UnsupportedToolCall {
                    turn_id,
                    tool_name: tool_name.clone(),
                });
                self.respond(
                    &id,
                    tool_result(&format!("unsupported tool `{tool_name}`"), true),
                )
                .await?;
            }
        }

        Ok(())
    }

    async fn respond(&mut self, id: &Value, result: Value) -> Result<()> {
        self.send(json!({ "id": id, "result": result })).await
    }

    async fn respond_error(&mut self, id: &Value, code: i64, message: &str) -> Result<()> {
        self.send(json!({ "id": id, "error": { "code": code, "message": message } }))
            .await
    }
}

/// A dynamic tool result envelope.
fn tool_result(text: &str, is_error: bool) -> Value {
    json!({
        "content": [ { "type": "text", "text": text } ],
        "isError": is_error,
    })
}

fn response_timeout_error(target: &PumpTarget, codex: &CodexConfig) -> crate::error::SymphonyError {
    match target {
        PumpTarget::Response(_) => ResponseTimeout {
            method: "agent request".to_string(),
            timeout_ms: codex.read_timeout_ms,
        },
        PumpTarget::Turn(_) => TurnTimeout {
            timeout_ms: codex.turn_timeout_ms,
        },
    }
}

fn response_outcome(payload: Value) -> Result<Value> {
    if let Some(error) = payload.get("error") {
        return Err(ResponseError {
            method: "agent request".to_string(),
            message: error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("agent returned an error")
                .to_string(),
        });
    }
    Ok(payload.get("result").cloned().unwrap_or(Value::Null))
}

fn turn_error_message(params: &Value) -> Option<String> {
    params
        .get("turn")
        .and_then(|turn| turn.get("error"))
        .and_then(|error| {
            error
                .get("message")
                .and_then(Value::as_str)
                .or_else(|| error.as_str())
        })
        .or_else(|| params.get("error").and_then(Value::as_str))
        .map(str::to_string)
}

fn summarize_notification(method: &str, params: &Value) -> String {
    let summary = match method {
        "item/started" | "item/completed" => params
            .get("item")
            .and_then(|item| item.get("type"))
            .and_then(Value::as_str)
            .map(|item_type| format!("{method}: {item_type}")),
        "item/agentMessage/delta" => params
            .get("delta")
            .and_then(Value::as_str)
            .map(|delta| format!("agent message: {}", truncate(delta, 200))),
        _ => None,
    };

    summary.unwrap_or_else(|| method.to_string())
}

fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    text.chars().take(limit).collect::<String>() + "…"
}

/// Map the configured thread sandbox mode onto the protocol's `SandboxMode`.
fn map_sandbox_mode(configured: &str) -> &'static str {
    match configured.trim().to_ascii_lowercase().replace('_', "-").as_str() {
        "read-only" | "readonly" => "readOnly",
        "danger-full-access" | "dangerfullaccess" | "full-access" => "dangerFullAccess",
        _ => "workspaceWrite",
    }
}

/// Build the per-turn sandbox policy payload.
fn turn_sandbox_policy(configured: &str, workspace: &str) -> Value {
    match map_sandbox_mode(configured) {
        "readOnly" => json!({ "type": "readOnly" }),
        "dangerFullAccess" => json!({ "type": "dangerFullAccess" }),
        _ => json!({
            "type": "workspaceWrite",
            "writableRoots": [workspace],
            "networkAccess": false,
        }),
    }
}

/// Extract absolute token totals from an agent payload, ignoring delta-shaped fields.
fn extract_token_usage(params: &Value) -> Option<TokenUsage> {
    const ABSOLUTE_KEYS: [&str; 6] = [
        "total_token_usage",
        "totalTokenUsage",
        "total_token_usage_breakdown",
        "tokenUsage",
        "token_usage",
        "usage",
    ];
    const DELTA_KEYS: [&str; 3] = ["last_token_usage", "lastTokenUsage", "delta"];

    fn collect(value: &Value, keys: &[&str]) -> Option<TokenUsage> {
        match value {
            Value::Object(map) => {
                for (key, nested) in map {
                    if DELTA_KEYS.contains(&key.as_str()) {
                        continue;
                    }
                    if keys.contains(&key.as_str())
                        && let Some(usage) = read_usage(nested)
                    {
                        return Some(usage);
                    }
                }
                map.iter()
                    .filter(|(key, _)| !DELTA_KEYS.contains(&key.as_str()))
                    .find_map(|(_, nested)| collect(nested, keys))
            }
            Value::Array(items) => items.iter().find_map(|item| collect(item, keys)),
            _ => None,
        }
    }

    fn read_usage(value: &Value) -> Option<TokenUsage> {
        let map = value.as_object()?;
        let read = |names: &[&str]| -> Option<u64> {
            names
                .iter()
                .find_map(|name| map.get(*name).and_then(Value::as_u64))
        };

        let input = read(&[
            "input_tokens",
            "inputTokens",
            "prompt_tokens",
            "promptTokens",
            "input",
            "cached_input_tokens",
        ]);
        let output = read(&[
            "output_tokens",
            "outputTokens",
            "completion_tokens",
            "completionTokens",
            "output",
            "reasoning_output_tokens",
        ]);
        let total = read(&["total_tokens", "totalTokens", "total"]);

        let input = input.unwrap_or(0);
        let output = output.unwrap_or(0);
        let total = total.unwrap_or(input + output);
        if input == 0 && output == 0 && total == 0 {
            return None;
        }
        Some(TokenUsage {
            input_tokens: input,
            output_tokens: output,
            total_tokens: total,
        })
    }

    collect(params, &ABSOLUTE_KEYS)
}

fn spawn_stdout_reader(stdout: tokio::process::ChildStdout, tx: mpsc::Sender<Incoming>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    let message = match serde_json::from_str::<Value>(trimmed) {
                        Ok(message) => message,
                        Err(error) => {
                            if tx
                                .send(Incoming::Malformed {
                                    line: format!("{error}: {}", truncate(trimmed, 200)),
                                })
                                .await
                                .is_err()
                            {
                                return;
                            }
                            continue;
                        }
                    };

                    let incoming = match (message.get("id"), message.get("method")) {
                        (Some(id), Some(method)) => Incoming::Request {
                            id: id.clone(),
                            method: method.as_str().unwrap_or_default().to_string(),
                            params: message.get("params").cloned().unwrap_or(Value::Null),
                        },
                        (Some(id), None) => Incoming::Response {
                            id: id.clone(),
                            payload: message,
                        },
                        (None, Some(method)) => Incoming::Notification {
                            method: method.as_str().unwrap_or_default().to_string(),
                            params: message.get("params").cloned().unwrap_or(Value::Null),
                        },
                        (None, None) => Incoming::Malformed {
                            line: format!("message without id or method: {}", truncate(trimmed, 200)),
                        },
                    };

                    if tx.send(incoming).await.is_err() {
                        return;
                    }
                }
                Ok(None) => {
                    // The client may already be gone; either way this task is done.
                    if tx
                        .send(Incoming::Closed {
                            message: "agent subprocess exited".to_string(),
                        })
                        .await
                        .is_err()
                    {
                        debug!(target: "symphony", "outcome=ignored message=agent client channel closed");
                    }
                    return;
                }
                Err(error) => {
                    if tx
                        .send(Incoming::Closed {
                            message: format!("agent stdout read error: {error}"),
                        })
                        .await
                        .is_err()
                    {
                        debug!(target: "symphony", "outcome=ignored message=agent client channel closed");
                    }
                    return;
                }
            }
        }
    })
}

fn spawn_stderr_reader(stderr: tokio::process::ChildStderr) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            debug!(target: "symphony", "stream=stderr {line}");
        }
    })
}

#[cfg(all(test, unix))]
mod protocol_tests {
    use super::*;
    use crate::config::CodexConfig;
    use crate::domain::Issue;
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;

    const STUB_PROTOCOL: &str = r##"#!/usr/bin/env bash
printf '%s\n' "$PWD" > "$(dirname "$0")/cwd.txt"
while IFS= read -r line; do
  id() { printf '%s' "$1" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p'; }
  case "$line" in
    *'"method":"initialize"'*)
      printf '{"id":%s,"result":{"userAgent":"stub"}}\n' "$(id "$line")"
      ;;
    *'"method":"thread/start"'*)
      printf '{"id":%s,"result":{"thread":{"id":"thr_1"}}}\n' "$(id "$line")"
      ;;
    *'"method":"thread/name/set"'*)
      printf '{"id":%s,"result":{}}\n' "$(id "$line")"
      ;;
    *'"method":"turn/start"'*)
      printf '{"id":%s,"result":{"turn":{"id":"turn_1","status":"inProgress"}}}\n' "$(id "$line")"
      printf '{"method":"turn/started","params":{"turn":{"id":"turn_1"}}}\n'
      printf '{"id":900,"method":"item/commandExecution/requestApproval","params":{"itemId":"i1","threadId":"thr_1","turnId":"turn_1"}}\n'
      printf '{"id":901,"method":"item/tool/call","params":{"threadId":"thr_1","turnId":"turn_1","tool":"unknown_tool","arguments":{}}}\n'
      printf '{"method":"thread/tokenUsage/updated","params":{"tokenUsage":{"total_token_usage":{"input_tokens":1200,"output_tokens":800,"total_tokens":2000},"last_token_usage":{"input_tokens":10,"output_tokens":5,"total_tokens":15}}}}\n'
      printf '{"method":"item/agentMessage/delta","params":{"delta":"working"}}\n'
      printf '{"method":"turn/completed","params":{"turn":{"id":"turn_1","status":"completed"}}}\n'
      ;;
    *) : ;;
  esac
done
"##;

    const STUB_HANGING: &str = r##"#!/usr/bin/env bash
while IFS= read -r line; do
  id() { printf '%s' "$1" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p'; }
  case "$line" in
    *'"method":"initialize"'*)
      printf '{"id":%s,"result":{}}\n' "$(id "$line")"
      ;;
    *'"method":"thread/start"'*)
      printf '{"id":%s,"result":{"thread":{"id":"thr_1"}}}\n' "$(id "$line")"
      ;;
    *'"method":"thread/name/set"'*)
      printf '{"id":%s,"result":{}}\n' "$(id "$line")"
      ;;
    *'"method":"turn/start"'*)
      printf '{"id":%s,"result":{"turn":{"id":"turn_1","status":"inProgress"}}}\n' "$(id "$line")"
      ;;
    *) : ;;
  esac
done
"##;

    fn write_stub(directory: &TempDir, contents: &str) -> String {
        use std::os::unix::fs::PermissionsExt;

        let path = directory.path().join("stub_agent.sh");
        std::fs::write(&path, contents).expect("stub agent is writable");
        let permissions = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(&path, permissions).expect("stub agent is executable");
        path.to_string_lossy().into_owned()
    }

    fn codex_config(command: String) -> CodexConfig {
        CodexConfig {
            command,
            approval_policy: "never".to_string(),
            thread_sandbox: "workspace-write".to_string(),
            turn_sandbox_policy: "workspace-write".to_string(),
            turn_timeout_ms: 5_000,
            read_timeout_ms: 5_000,
            stall_timeout_ms: 0,
        }
    }

    fn issue() -> Issue {
        Issue {
            id: "abc123".to_string(),
            identifier: "MT-649".to_string(),
            title: "Fix the thing".to_string(),
            description: None,
            priority: Some(1),
            state: "Todo".to_string(),
            branch_name: None,
            url: None,
            labels: Vec::new(),
            blocked_by: Vec::new(),
            created_at: None,
            updated_at: None,
        }
    }

    fn workspace(directory: &TempDir) -> std::path::PathBuf {
        let path = directory.path().join("MT-649");
        std::fs::create_dir_all(&path).expect("workspace is creatable");
        path
    }

    fn events() -> (Arc<Mutex<Vec<AgentEvent>>>, impl FnMut(AgentEvent)) {
        let collected = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&collected);
        let emit = move |event: AgentEvent| {
            sink.lock().expect("event sink is not poisoned").push(event);
        };
        (collected, emit)
    }

    #[tokio::test]
    async fn runs_a_turn_end_to_end_over_stdio() {
        let directory = TempDir::new().expect("temp dir");
        let command = write_stub(&directory, STUB_PROTOCOL);
        let workspace = workspace(&directory);

        // Keep the sender alive so the pump's cancel branch stays pending.
        let (_cancel_tx, mut cancel) = watch::channel(false);
        let mut client = AgentClient::start(
            &codex_config(command),
            &workspace,
            &workspace,
            &issue(),
            Vec::new(),
        )
        .await
        .expect("agent session starts");

        let (collected, mut emit) = events();
        let terminal = client
            .run_turn("work on {{ issue.identifier }}", &mut cancel, &mut emit)
            .await
            .expect("turn completes");
        assert_eq!(terminal, TurnTerminal::Completed);

        client.stop().await;

        // The agent was launched with the per-issue workspace as its cwd.
        let cwd = std::fs::read_to_string(directory.path().join("cwd.txt"))
            .expect("the stub recorded its cwd");
        assert_eq!(
            std::fs::canonicalize(cwd.trim()).expect("cwd exists"),
            std::fs::canonicalize(&workspace).expect("workspace exists")
        );

        let collected = collected.lock().expect("events are readable").clone();
        let names: Vec<&'static str> = collected.iter().map(AgentEvent::name).collect();

        assert_eq!(
            names
                .iter()
                .filter(|name| **name == "session_started")
                .count(),
            1,
            "session_started must be emitted exactly once: {names:?}"
        );
        assert!(
            names.contains(&"turn_started"),
            "turn_started is derived from the notification: {names:?}"
        );
        assert!(names.contains(&"turn_completed"), "{names:?}");
        assert!(names.contains(&"approval_auto_approved"), "{names:?}");
        assert!(names.contains(&"unsupported_tool_call"), "{names:?}");
        assert!(names.contains(&"notification"), "{names:?}");

        let usage = collected
            .iter()
            .find_map(|event| match event {
                AgentEvent::TokenUsage { usage } => Some(*usage),
                _ => None,
            })
            .expect("absolute token usage is extracted");
        assert_eq!(usage.total_tokens, 2_000);

        let session_started = collected
            .iter()
            .find_map(|event| match event {
                AgentEvent::SessionStarted {
                    session_id,
                    thread_id,
                    turn_id,
                    ..
                } => Some((session_id.clone(), thread_id.clone(), turn_id.clone())),
                _ => None,
            })
            .expect("session identity is reported");
        assert_eq!(session_started.0, "thr_1-turn_1");
        assert_eq!(session_started.1, "thr_1");
        assert_eq!(session_started.2, "turn_1");
    }

    #[tokio::test]
    async fn enforces_the_turn_timeout() {
        let directory = TempDir::new().expect("temp dir");
        let command = write_stub(&directory, STUB_HANGING);
        let workspace = workspace(&directory);

        let mut config = codex_config(command);
        config.turn_timeout_ms = 300;

        // Keep the sender alive so the pump's cancel branch stays pending.
        let (_cancel_tx, mut cancel) = watch::channel(false);
        let mut client = AgentClient::start(&config, &workspace, &workspace, &issue(), Vec::new())
            .await
            .expect("agent session starts");

        let (_collected, mut emit) = events();
        let error = client
            .run_turn("never finishes", &mut cancel, &mut emit)
            .await
            .expect_err("the turn times out");
        assert!(matches!(error, TurnTimeout { .. }), "got {error}");

        client.stop().await;
    }

    #[tokio::test]
    async fn a_pre_cancelled_turn_is_not_started() {
        let directory = TempDir::new().expect("temp dir");
        let command = write_stub(&directory, STUB_PROTOCOL);
        let workspace = workspace(&directory);

        let (cancel_tx, mut cancel) = watch::channel(false);
        let mut client = AgentClient::start(
            &codex_config(command),
            &workspace,
            &workspace,
            &issue(),
            Vec::new(),
        )
        .await
        .expect("agent session starts");

        cancel_tx.send(true).expect("cancel can be signalled");
        let (_collected, mut emit) = events();
        let error = client
            .run_turn("cancelled", &mut cancel, &mut emit)
            .await
            .expect_err("the turn is cancelled");
        assert!(matches!(error, TurnCancelled), "got {error}");

        client.stop().await;
    }

    #[tokio::test]
    async fn rejects_a_workspace_mismatched_cwd() {
        let directory = TempDir::new().expect("temp dir");
        let command = write_stub(&directory, STUB_PROTOCOL);
        let workspace = workspace(&directory);

        let result = AgentClient::start(
            &codex_config(command),
            &workspace,
            directory.path(),
            &issue(),
            Vec::new(),
        )
        .await;
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("a cwd outside the per-issue workspace must be rejected"),
        };
        assert!(matches!(error, InvalidWorkspaceCwd { .. }), "got {error}");
        assert!(
            !Path::new(&workspace).join("cwd.txt").exists(),
            "no agent process should have been launched"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_sandbox_modes() {
        assert_eq!(map_sandbox_mode("workspace-write"), "workspaceWrite");
        assert_eq!(map_sandbox_mode("workspaceWrite"), "workspaceWrite");
        assert_eq!(map_sandbox_mode("read-only"), "readOnly");
        assert_eq!(map_sandbox_mode("danger-full-access"), "dangerFullAccess");
        assert_eq!(map_sandbox_mode("nonsense"), "workspaceWrite");
    }

    #[test]
    fn builds_turn_sandbox_policy() {
        let policy = turn_sandbox_policy("workspace-write", "/tmp/ws");
        assert_eq!(policy["type"], "workspaceWrite");
        assert_eq!(policy["writableRoots"][0], "/tmp/ws");
        assert_eq!(policy["networkAccess"], false);

        assert_eq!(turn_sandbox_policy("read-only", "/tmp/ws")["type"], "readOnly");
        assert_eq!(
            turn_sandbox_policy("danger-full-access", "/tmp/ws")["type"],
            "dangerFullAccess"
        );
    }

    #[test]
    fn extracts_absolute_token_usage_and_ignores_deltas() {
        let payload = json!({
            "threadId": "thr_1",
            "tokenUsage": {
                "total_token_usage": {
                    "input_tokens": 1200,
                    "output_tokens": 800,
                    "total_tokens": 2000
                },
                "last_token_usage": {
                    "input_tokens": 10,
                    "output_tokens": 5,
                    "total_tokens": 15
                }
            }
        });

        let usage = extract_token_usage(&payload).expect("usage is extracted");
        assert_eq!(
            usage,
            TokenUsage {
                input_tokens: 1200,
                output_tokens: 800,
                total_tokens: 2000,
            }
        );
    }

    #[test]
    fn derives_total_when_absent() {
        let payload = json!({
            "tokenUsage": { "promptTokens": 10, "completionTokens": 4 }
        });
        let usage = extract_token_usage(&payload).expect("usage is extracted");
        assert_eq!(usage.total_tokens, 14);
    }

    #[test]
    fn ignores_delta_only_payloads() {
        let payload = json!({
            "last_token_usage": { "input_tokens": 10, "output_tokens": 5, "total_tokens": 15 }
        });
        assert!(extract_token_usage(&payload).is_none());
    }

    #[test]
    fn tool_result_envelope_marks_errors() {
        let result = tool_result("nope", true);
        assert_eq!(result["isError"], true);
        assert_eq!(result["content"][0]["text"], "nope");
    }

    #[test]
    fn response_outcome_surfaces_jsonrpc_errors() {
        let error = response_outcome(json!({ "id": 1, "error": { "code": 1, "message": "boom" } }))
            .expect_err("error is surfaced");
        assert!(error.to_string().contains("boom"));

        let result = response_outcome(json!({ "id": 1, "result": { "thread": { "id": "t" } } }))
            .expect("result is returned");
        assert_eq!(result["thread"]["id"], "t");
    }
}
