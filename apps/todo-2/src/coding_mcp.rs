//! The coding-workflow MCP server.
//!
//! todo-2 hosts a loopback Model Context Protocol endpoint so the agent can
//! attach the spec it interviewed out, create and interview sub-tasks, log
//! annotations, propose the feature branch, and report phase completion. The
//! transport is a minimal HTTP/1.1 listener bound to `127.0.0.1:0` with a
//! per-session bearer token, bound to the task that session was launched for;
//! only the `initialize`, `tools/list`, and `tools/call` methods are
//! implemented, which is the subset the coding workflow uses.
//!
//! Tool calls mutate the same `TodoStore` the UI uses, and each mutating call
//! signals the foreground app so the stepper reloads.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use storage::prelude::*;
use storage::task::TaskCreate;

use crate::store::Store;

/// The read-only interview process's profile.
pub const INTERVIEW_PROFILE: &str = "interview";
/// The full-tools coding process's profile.
pub const CODING_PROFILE: &str = "coding";

/// What a session's bearer token is bound to: the profile it may call tools
/// as, and the task its process was launched for.
///
/// A tool call names the task it means when it can, but the pane knows which
/// task it launched the session for, so the binding is what a call falls back
/// on. That is what lets a model act on a coding run at all: the feature task's
/// id is nowhere in the prompt it was given, so a call that has to invent one
/// invents it wrong.
#[derive(Clone, Debug)]
pub struct SessionScope {
    /// The agent profile that owns this session ([`allows_tool`] scopes by it).
    pub profile: String,
    /// The task the session was launched for.
    pub task_id: u64,
}

/// A running loopback MCP endpoint. Dropping it leaves the listener thread
/// running for the life of the process (there is nothing to release).
pub struct CodingMcpServer {
    url: String,
    /// Bearer token per session, bound to the task it was launched for. Each
    /// process gets its own, so a tool call can be attributed — and resolved —
    /// to the session that made it (spec decision #11).
    sessions: Arc<Mutex<BTreeMap<String, SessionScope>>>,
}

impl CodingMcpServer {
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Mint the token for a process launched for `task_id` under `profile`.
    /// The caller gives this token to that process alone, and retires it with
    /// [`Self::close_session`] when the session is replaced or torn down.
    pub fn open_session(&self, profile: &str, task_id: u64) -> String {
        let token = random_token();
        self.sessions().insert(
            token.clone(),
            SessionScope {
                profile: profile.to_string(),
                task_id,
            },
        );
        token
    }

    /// Retire a token: its process is gone or was replaced. Retiring a token
    /// that is already gone is not an error — the app retires sessions it may
    /// have replaced already.
    pub fn close_session(&self, token: &str) -> bool {
        self.sessions().remove(token).is_some()
    }

    /// The registry, taken even if a previous holder panicked: the map is still
    /// consistent, only the lock guard was abandoned mid-update.
    fn sessions(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, SessionScope>> {
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Bind the endpoint and serve it on a dedicated thread. `handle` is the app's
/// Tokio runtime, used to lock the store and run async storage calls; `notify`
/// is signalled after every mutating tool call so the UI can refresh.
pub fn start(
    store: Store,
    handle: tokio::runtime::Handle,
    notify: Option<tokio::sync::mpsc::UnboundedSender<()>>,
) -> anyhow::Result<CodingMcpServer> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let sessions: Arc<Mutex<BTreeMap<String, SessionScope>>> =
        Arc::new(Mutex::new(BTreeMap::new()));
    let server_sessions = sessions.clone();
    std::thread::Builder::new()
        .name("coding-mcp".to_string())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else {
                    continue;
                };
                if let Err(error) = serve(stream, &store, &handle, &server_sessions, &notify) {
                    tracing::warn!("coding MCP request failed: {error}");
                }
            }
        })?;
    Ok(CodingMcpServer {
        url: format!("http://{address}/mcp"),
        sessions,
    })
}

/// The session a bearer token belongs to: what scopes the `tools/call`, and the
/// task the call falls back on when it names none.
fn session_for_token(
    sessions: &Mutex<BTreeMap<String, SessionScope>>,
    header: &str,
) -> Option<SessionScope> {
    let token = header.strip_prefix("Bearer ")?;
    let sessions = sessions
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    sessions.get(token).cloned()
}

/// The tools a profile may call. Both profiles expose the whole surface today
/// (spec decision #10): the difference between them is opencode's own
/// filesystem/shell tools, not the MCP surface. Narrowing one profile later is
/// a change to this single function.
fn allows_tool(_profile: &str, tool: &str) -> bool {
    tool_definitions()
        .iter()
        .any(|definition| definition.get("name").and_then(Value::as_str) == Some(tool))
}

/// A token long enough that a local process cannot guess it, without pulling
/// in a random-number dependency (`sha2` is already a dependency).
fn random_token() -> String {
    use sha2::{Digest, Sha256};
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut hasher = Sha256::new();
    hasher.update(nanos.to_le_bytes());
    hasher.update(std::process::id().to_le_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Serve one request per connection and close it, which keeps the HTTP subset
/// we need (a JSON POST per tool call) simple and stateless.
fn serve(
    stream: TcpStream,
    store: &Store,
    handle: &tokio::runtime::Handle,
    sessions: &Mutex<BTreeMap<String, SessionScope>>,
    notify: &Option<tokio::sync::mpsc::UnboundedSender<()>>,
) -> anyhow::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(());
    }
    let mut content_length = 0usize;
    let mut authorization = String::new();
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 {
            break;
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        let Some((name, value)) = header.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match name.to_ascii_lowercase().as_str() {
            "content-length" => content_length = value.parse().unwrap_or(0),
            "authorization" => authorization = value.to_string(),
            _ => {}
        }
    }
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body)?;
    let mut stream = stream;

    if !request_line.starts_with("POST ") {
        return write_response(&mut stream, 405, None);
    }
    let Some(scope) = session_for_token(sessions, &authorization) else {
        return write_response(&mut stream, 401, None);
    };
    let Ok(request) = serde_json::from_slice::<Value>(&body) else {
        return write_response(&mut stream, 400, Some(error_response(Value::Null, -32700, "parse error")));
    };
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    // Notifications carry no id and get no response body.
    if request.get("id").is_none() {
        return write_response(&mut stream, 202, None);
    }

    let response = match method.as_str() {
        "initialize" => success_response(
            id,
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "todo-2-coding", "version": "0.1.0" },
            }),
        ),
        "ping" => success_response(id, json!({})),
        "tools/list" => success_response(id, json!({ "tools": tool_definitions() })),
        "tools/call" => {
            let name = request
                .pointer("/params/name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let arguments = request
                .pointer("/params/arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let outcome = if allows_tool(&scope.profile, name) {
                handle.block_on(async {
                    let mut store = store.0.lock().await;
                    dispatch(&mut store, name, arguments, Some(&scope)).await
                })
            } else {
                Err(format!(
                    "the {} profile may not call `{name}`",
                    scope.profile
                ))
            };
            match outcome {
                Ok(value) => {
                    if let Some(notify) = notify {
                        // A dropped receiver only means the window is gone.
                        let _ = notify.send(());
                    }
                    success_response(id, tool_result(&value, false))
                }
                Err(message) => success_response(id, tool_result(&json!(message), true)),
            }
        }
        other => error_response(id, -32601, &format!("unknown method `{other}`")),
    };
    write_response(&mut stream, 200, Some(response))
}

fn success_response(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error_response(id: Value, code: i32, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// MCP tool results wrap the payload in a text content block.
fn tool_result(payload: &Value, is_error: bool) -> Value {
    let text = serde_json::to_string_pretty(payload).unwrap_or_else(|_| payload.to_string());
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error,
    })
}

fn write_response(stream: &mut TcpStream, status: u16, body: Option<Value>) -> anyhow::Result<()> {
    let reason = match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        401 => "Unauthorized",
        405 => "Method Not Allowed",
        _ => "Error",
    };
    let body = body.map(|value| value.to_string()).unwrap_or_default();
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body.as_bytes())?;
    stream.flush()?;
    Ok(())
}

/// The `task_id` property every run-resolving tool shares.
///
/// One wording for all of them, because it is one rule: a call may name any
/// task of the run and the run that owns it is found by walking up parents, and
/// a call that names none is taken to mean the task this session was launched
/// for.
fn task_id_property() -> Value {
    json!({
        "type": "integer",
        "description": "Any task of the run — the feature task, one of its steps, or a sub-task; the run that owns it is found by walking up its parents. Optional: the task this session was launched for is used when it is omitted."
    })
}

/// The coding tools exposed to the agent, per §7.2 of the spec.
pub fn tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "get_coding_context",
            "description": "Read the current coding run: the task it is about, that task's parent chain up to the run's root, phase, spec, cycle, branch, annotation log and steps. `task_id` may be omitted (the task this session was launched for is used) and may name any task of the run — a step or a sub-task resolves to the run that owns it. `phases[].task_id` is the task id of each step (and `open_task_id` the one being worked on); pass it as `parent_task_id` to nest work under that step. Call this before acting on a phase.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "task_id": task_id_property(),
                    "run_id": { "type": "integer", "description": "The workflow run id, when known." }
                }
            }
        }),
        json!({
            "name": "get_spec",
            "description": "Read the spec currently stored for a task (the umbrella spec on the feature task, or a subtask's own spec). Returns null when nothing has been saved yet; call this before revising a spec on a later interview round.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "task_id": { "type": "integer", "description": "The task whose spec to read." }
                },
                "required": ["task_id"]
            }
        }),
        json!({
            "name": "set_spec",
            "description": "Save the spec you interviewed out: the umbrella spec on the feature task, plus a spec for each subtask that needs its own, plus the ids of the subtasks the umbrella covers. This is how the spec reaches the app — do not write a spec file. Completes the interview phase. Every subtask id must be a direct subtask (never a workflow step).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "task_id": task_id_property(),
                    "content": { "type": "string", "description": "The umbrella spec markdown." },
                    "subtasks": {
                        "type": "array",
                        "description": "Subtask specs: one entry per subtask you specced individually.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "task_id": { "type": "integer", "description": "The subtask's id from get_coding_context.sub_tasks." },
                                "spec": { "type": "string", "description": "That subtask's spec markdown." }
                            },
                            "required": ["task_id", "spec"]
                        }
                    },
                    "covered": {
                        "type": "array",
                        "description": "Ids of the subtasks the umbrella spec covers, with nothing of their own to add.",
                        "items": { "type": "integer" }
                    }
                },
                "required": ["content"]
            }
        }),
        json!({
            "name": "create_sub_task",
            "description": "Add a subtask to the feature: the new task always hangs off the feature task itself, never off a workflow step, so it appears in the feature's subtask list. Set nested=true when the subtask is complex enough to deserve its own coding run.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "parent_task_id": { "type": "integer", "description": "The feature task or a step's task id; a step id resolves to the feature task." },
                    "title": { "type": "string" },
                    "description": { "type": "string" },
                    "nested": { "type": "boolean", "default": false }
                },
                "required": ["parent_task_id", "title"]
            }
        }),
        json!({
            "name": "request_sub_task_interview",
            "description": "Flag an under-specified sub-task for its own interview round. The user launches it from the app.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "sub_task_id": { "type": "integer" },
                    "reason": { "type": "string", "description": "What is unclear." }
                },
                "required": ["sub_task_id"]
            }
        }),
        json!({
            "name": "append_note",
            "description": "Append an entry to the run's annotation log (a finding, a decision, a question).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "task_id": task_id_property(),
                    "kind": { "type": "string", "description": "annotation | finding | decision" },
                    "body": { "type": "string" }
                },
                "required": ["body"]
            }
        }),
        json!({
            "name": "propose_branch",
            "description": "Propose the feature branch name recorded on the run. The app creates it once the spec is approved.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "task_id": task_id_property(),
                    "name": { "type": "string" },
                    "summary": { "type": "string" }
                },
                "required": ["name"]
            }
        }),
        json!({
            "name": "complete_phase",
            "description": "Report that the current phase's work is done. The interview advances automatically; implementation waits for the user's confirmation.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "task_id": task_id_property(),
                    "phase": { "type": "string", "description": "interview | spec | implement | review | merge" },
                    "summary": { "type": "string" }
                },
                "required": ["phase"]
            }
        }),
        json!({
            "name": "propose_summary",
            "description": "Record a commit message and optional summary for the merge step.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "task_id": task_id_property(),
                    "commit_message": { "type": "string" },
                    "pr_summary": { "type": "string" }
                },
                "required": ["commit_message"]
            }
        }),
    ]
}

/// Dispatch one JSON-RPC `tools/call` into the store. Errors are returned as
/// strings so the server can answer with `isError: true` instead of failing
/// the transport. `session` is the calling process's scope, which every tool
/// that resolves a run falls back on when the call names no task.
///
/// `get_spec`, `create_sub_task` and `request_sub_task_interview` take ids of
/// their own (a task to read, a parent, a sub-task) and resolve no run.
pub async fn dispatch(
    store: &mut TodoStore,
    tool: &str,
    arguments: Value,
    session: Option<&SessionScope>,
) -> Result<Value, String> {
    match tool {
        "get_coding_context" => get_coding_context(store, &arguments, session).await,
        "get_spec" => get_spec(store, &arguments).await,
        "set_spec" => set_spec(store, &arguments, session).await,
        "create_sub_task" => create_sub_task(store, &arguments).await,
        "request_sub_task_interview" => request_sub_task_interview(store, &arguments).await,
        "append_note" => append_note(store, &arguments, session).await,
        "propose_branch" => propose_branch(store, &arguments, session).await,
        "complete_phase" => complete_phase(store, &arguments, session).await,
        "propose_summary" => propose_summary(store, &arguments, session).await,
        other => Err(format!("unknown tool `{other}`")),
    }
}

fn arg_u64(arguments: &Value, key: &str) -> Result<u64, String> {
    arguments
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("`{key}` is required and must be an integer"))
}

fn arg_str(arguments: &Value, key: &str) -> Option<String> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// A resolved run, with the tasks the resolution passed through: the task the
/// call named first (or the run's root, when the call named a run directly),
/// the task the run is rooted at last.
struct Resolved {
    run: WorkflowRun,
    view: RunView,
    chain: Vec<Task>,
}

/// Resolve the run a tool call targets: an explicit `run_id` first, then an
/// explicit `task_id`, then the task the session was launched for.
///
/// A task names the run that owns it, found by walking up its parents, so a
/// phase step or a sub-task is as good an id as the feature task itself. The
/// argument wins when it disagrees with the session's task: a session outlives
/// one call, and a call that names what it means is taken at its word. A task
/// in no run — and with no run above it — is an error rather than a silent
/// no-op.
async fn resolve(
    store: &mut TodoStore,
    arguments: &Value,
    session: Option<&SessionScope>,
) -> Result<Resolved, String> {
    if let Some(run_id) = arguments.get("run_id").and_then(Value::as_u64) {
        let run = store
            .find_run(run_id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("no workflow run {run_id}"))?;
        let view = store
            .workflow_run_view(run.id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("workflow run {} is not visible", run.id))?;
        // The call named a run, so the task it is about is that run's root.
        let chain = match run.root_task_id {
            Some(root) => vec![store.get_task(root).await.map_err(|e| e.to_string())?],
            None => Vec::new(),
        };
        return Ok(Resolved { run, view, chain });
    }
    let task_id = arguments
        .get("task_id")
        .and_then(Value::as_u64)
        .or_else(|| session.map(|session| session.task_id));
    let Some(task_id) = task_id else {
        return Err("no `task_id` was given and this session is not bound to a task".to_string());
    };
    let (run, chain) = store
        .find_run_in_task_chain(task_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("task {task_id} has no coding run (nor does any parent of it)"))?;
    let view = store
        .workflow_run_view(run.id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("workflow run {} is not visible", run.id))?;
    Ok(Resolved { run, view, chain })
}

/// One task of the chain as the model reads it: enough to tell which task it
/// is, and where it sits.
fn task_json(task: &Task) -> Value {
    json!({
        "id": task.id,
        "title": task.title,
        "done": task.done,
        "role": task.role,
        "parent_id": task.parent_id,
    })
}

async fn get_coding_context(
    store: &mut TodoStore,
    arguments: &Value,
    session: Option<&SessionScope>,
) -> Result<Value, String> {
    let Resolved { run, view, chain } = resolve(store, arguments, session).await?;
    let notes: Vec<Value> = run_notes(&run.step_results.0)
        .into_iter()
        .map(|note| note.to_json())
        .collect();
    let phases: Vec<Value> = view
        .steps
        .iter()
        .map(|step| {
            json!({
                "node_id": step.node.id,
                "phase": step.node.phase,
                "task_id": step.task.id,
                "title": step.task.title,
                "done": step.task.done,
            })
        })
        .collect();
    let open = view.steps.iter().find(|step| !step.task.done);
    let root_task_id = run.root_task_id;
    let spec = match root_task_id {
        Some(task_id) => store
            .get_task_spec(task_id)
            .await
            .map_err(|e| e.to_string())?,
        None => None,
    };
    // The run's real subtasks (never steps), with the same coverage state the
    // pane shows, so the model reads exactly what the user sees (§6.1).
    let sub_tasks: Vec<Value> = match root_task_id {
        Some(task_id) => store
            .run_subtasks(task_id)
            .await
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|task| {
                let coverage = subtask_coverage(&task);
                json!({
                    "id": task.id,
                    "title": task.title,
                    "done": task.done,
                    "spec": coverage.label(),
                    "description": task.description,
                    "nested_run": task.workflow_run_id.is_some(),
                })
            })
            .collect(),
        None => Vec::new(),
    };
    Ok(json!({
        "run_id": run.id,
        "status": run.status,
        "root_task_id": root_task_id,
        // The task this call is about, and the road it took: the named task (or
        // the run's root) first, the run's root last.
        "task": chain.first().map(task_json),
        "task_chain": chain.iter().map(task_json).collect::<Vec<Value>>(),
        "phase": open.and_then(|step| step.node.phase.clone()),
        "open_task_id": open.map(|step| step.task.id),
        "round": 1 + notes.iter().filter(|note| note.get("kind").and_then(Value::as_str) == Some("reject")).count(),
        "branch": run.branch,
        "base_branch": run.base_branch,
        "branch_status": run.branch_status,
        "spec": spec,
        "annotations": notes,
        "sub_tasks": sub_tasks,
        "phases": phases,
    }))
}

/// The spec currently stored for a task, or `null`. A read-only interview
/// round calls this first to revise what is already there.
async fn get_spec(store: &mut TodoStore, arguments: &Value) -> Result<Value, String> {
    let task_id = arg_u64(arguments, "task_id")?;
    // A missing task is an error the model must see, not a silent `null`.
    store.get_task(task_id).await.map_err(|e| e.to_string())?;
    let spec = store
        .get_task_spec(task_id)
        .await
        .map_err(|e| e.to_string())?;
    Ok(json!({ "task_id": task_id, "spec": spec }))
}

async fn set_spec(
    store: &mut TodoStore,
    arguments: &Value,
    session: Option<&SessionScope>,
) -> Result<Value, String> {
    let Resolved { view, .. } = resolve(store, arguments, session).await?;
    let task_id = view
        .run
        .root_task_id
        .ok_or_else(|| "this run has no feature task".to_string())?;
    let content = arg_str(arguments, "content").ok_or_else(|| "`content` is required".to_string())?;
    let subtask_specs = arg_subtask_specs(arguments)?;
    let covered = arg_covered_ids(arguments);
    let awaiting_interview = view
        .steps
        .iter()
        .any(|step| step.node.id == "interview" && !step.task.done);
    // One transaction: umbrella, subtask specs, coverage marks, the run note
    // and the completed interview step. An id that is not a direct, non-step
    // subtask of this run is refused before anything is written (§6.2).
    store
        .save_subtask_specs(task_id, Some(content), subtask_specs, covered)
        .await
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "saved": true,
        "phase_advanced": awaiting_interview.then_some("spec"),
    }))
}

/// Parse the optional `subtasks` payload. A malformed entry is an error rather
/// than a silently dropped subtask: the model must say what it meant.
fn arg_subtask_specs(arguments: &Value) -> Result<Vec<SubtaskSpecInput>, String> {
    let Some(items) = arguments.get("subtasks").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let mut specs = Vec::with_capacity(items.len());
    for item in items {
        let task_id = item
            .get("task_id")
            .and_then(Value::as_u64)
            .ok_or_else(|| "each `subtasks` entry needs an integer `task_id`".to_string())?;
        let spec = item
            .get("spec")
            .and_then(Value::as_str)
            .ok_or_else(|| "each `subtasks` entry needs a `spec`".to_string())?;
        specs.push(SubtaskSpecInput {
            task_id,
            spec: spec.to_string(),
        });
    }
    Ok(specs)
}

/// Parse the optional `covered` id list.
fn arg_covered_ids(arguments: &Value) -> Vec<u64> {
    arguments
        .get("covered")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_u64).collect())
        .unwrap_or_default()
}

async fn create_sub_task(store: &mut TodoStore, arguments: &Value) -> Result<Value, String> {
    let requested_parent = arg_u64(arguments, "parent_task_id")?;
    let title =
        arg_str(arguments, "title").ok_or_else(|| "`title` is required".to_string())?;
    let description = arg_str(arguments, "description");
    let nested = arguments
        .get("nested")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    // In a coding run every subtask is a direct child of the run root
    // (decision #15): a step id passed as the parent resolves to the root, so
    // steps never grow children and the new work is visible as a subtask.
    let parent_task_id = resolve_subtask_parent(store, requested_parent).await?;
    let sub_task = store
        .create_task(
            TaskCreate::default()
                .title(title)
                .description(description)
                .parent_id(Some(parent_task_id)),
        )
        .await
        .map_err(|e| e.to_string())?;
    let mut run_id = None;
    if nested {
        let recipe_id = store
            .recipe_id_by_slug("coding-task")
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "the coding-task recipe is missing".to_string())?;
        let run = store
            .create_task_run(sub_task.id, recipe_id, json!({}))
            .await
            .map_err(|e| e.to_string())?;
        run_id = Some(run.id);
    }
    Ok(json!({
        "sub_task_id": sub_task.id,
        "run_id": run_id,
        // Echo the parent actually used, so a corrected step id is visible.
        "parent_task_id": parent_task_id,
    }))
}

/// The parent a new subtask lands on: the run root when the requested parent
/// belongs to a coding run (a step, or the root itself), the requested task
/// otherwise.
async fn resolve_subtask_parent(store: &mut TodoStore, requested: u64) -> Result<u64, String> {
    let task = store.get_task(requested).await.map_err(|e| e.to_string())?;
    let Some(run_id) = task.workflow_run_id else {
        return Ok(requested);
    };
    Ok(store
        .find_run(run_id)
        .await
        .map_err(|e| e.to_string())?
        .and_then(|run| run.root_task_id)
        .unwrap_or(requested))
}

async fn request_sub_task_interview(store: &mut TodoStore, arguments: &Value) -> Result<Value, String> {
    let sub_task_id = arg_u64(arguments, "sub_task_id")?;
    let reason = arg_str(arguments, "reason").unwrap_or_default();
    let recipe_id = store
        .recipe_id_by_slug("coding-sub-interview")
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "the coding-sub-interview recipe is missing".to_string())?;
    let run = store
        .create_task_run(sub_task_id, recipe_id, json!({}))
        .await
        .map_err(|e| e.to_string())?;
    if !reason.trim().is_empty() {
        store
            .append_run_note(run.id, "annotation", "interview", "interview", &reason)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(json!({ "run_id": run.id, "awaiting_user": true }))
}

async fn append_note(
    store: &mut TodoStore,
    arguments: &Value,
    session: Option<&SessionScope>,
) -> Result<Value, String> {
    let Resolved { run, .. } = resolve(store, arguments, session).await?;
    let body = arg_str(arguments, "body").ok_or_else(|| "`body` is required".to_string())?;
    let kind = arg_str(arguments, "kind").unwrap_or_else(|| "annotation".to_string());
    // The phase the run is in, so the log reads correctly when it is grouped
    // into cycles by the details panel.
    let phase = (store
        .workflow_run_view(run.id)
        .await
        .map_err(|e| e.to_string())?)
    .and_then(|view| {
        view.steps
            .iter()
            .find(|step| !step.task.done)
            .and_then(|step| step.node.phase.clone())
    })
    .unwrap_or_else(|| "annotation".to_string());
    store
        .append_run_note(run.id, &kind, &phase, &phase, &body)
        .await
        .map_err(|e| e.to_string())?;
    Ok(json!({ "appended": true }))
}

async fn propose_branch(
    store: &mut TodoStore,
    arguments: &Value,
    session: Option<&SessionScope>,
) -> Result<Value, String> {
    let Resolved { run, .. } = resolve(store, arguments, session).await?;
    let name = arg_str(arguments, "name").ok_or_else(|| "`name` is required".to_string())?;
    let kept = store
        .propose_run_branch(run.id, &name)
        .await
        .map_err(|e| e.to_string())?;
    if let Some(summary) = arg_str(arguments, "summary") {
        store
            .append_run_note(run.id, "annotation", "spec", "spec", &summary)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(json!({ "branch": kept }))
}

async fn complete_phase(
    store: &mut TodoStore,
    arguments: &Value,
    session: Option<&SessionScope>,
) -> Result<Value, String> {
    let Resolved { run, view, .. } = resolve(store, arguments, session).await?;
    let phase = arg_str(arguments, "phase").ok_or_else(|| "`phase` is required".to_string())?;
    let summary = arg_str(arguments, "summary");
    let Some(step) = view
        .steps
        .iter()
        .find(|step| !step.task.done && step.node.phase.as_deref() == Some(phase.as_str()))
    else {
        return Err(format!("the run is not waiting on the {phase} phase"));
    };
    if !CODING_PHASES.contains(&phase.as_str()) {
        return Err(format!("`{phase}` is not a coding phase"));
    }
    if let Some(summary) = summary.as_deref().filter(|summary| !summary.trim().is_empty()) {
        store
            .append_run_note(run.id, "annotation", &phase, &step.node.id, summary)
            .await
            .map_err(|e| e.to_string())?;
    }
    match phase.as_str() {
        "interview" => {
            store
                .complete_workflow_step(step.task.id, json!({}))
                .await
                .map_err(|e| e.to_string())?;
            Ok(json!({ "advanced": true }))
        }
        // Implementation needs the user's confirmation, and review/merge are
        // the user's gates: the report is recorded for the stepper instead.
        other => {
            store
                .append_run_note(
                    run.id,
                    "annotation",
                    other,
                    &step.node.id,
                    "Agent reports this phase is done",
                )
                .await
                .map_err(|e| e.to_string())?;
            Ok(json!({ "advanced": false, "awaiting": "user confirmation" }))
        }
    }
}

async fn propose_summary(
    store: &mut TodoStore,
    arguments: &Value,
    session: Option<&SessionScope>,
) -> Result<Value, String> {
    let Resolved { run, .. } = resolve(store, arguments, session).await?;
    let commit_message = arg_str(arguments, "commit_message")
        .ok_or_else(|| "`commit_message` is required".to_string())?;
    let mut body = commit_message;
    if let Some(summary) = arg_str(arguments, "pr_summary") {
        body.push_str(&format!("\n\n{summary}"));
    }
    store
        .append_run_note(run.id, "annotation", "review", "review", &body)
        .await
        .map_err(|e| e.to_string())?;
    Ok(json!({ "recorded": true }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use storage::StorageConfig;

    async fn store() -> TodoStore {
        let mut store = TodoStore::new(&StorageConfig {
            db_uri: "turso::memory:".to_string(),
        })
        .await
        .expect("in-memory store");
        store.ensure_coding_recipes().await.expect("seed recipes");
        store
    }

    /// A feature task with a started coding run, ready for tool calls.
    async fn started_run(store: &mut TodoStore) -> (u64, u64) {
        let task = store
            .create_task(TaskCreate::default().title("Add OAuth".to_string()))
            .await
            .expect("create feature task");
        let recipe_id = store
            .recipe_id_by_slug("coding-task")
            .await
            .expect("recipe lookup")
            .expect("coding-task recipe");
        let run = store
            .create_task_run(task.id, recipe_id, json!({}))
            .await
            .expect("start run");
        (task.id, run.id)
    }

    /// `dispatch` in these tests: a call from a session that is not bound to a
    /// task, so each test names its own `task_id` and proves the tools still
    /// work unbound. The bound-session tests call [`super::dispatch`] with a
    /// scope instead.
    async fn dispatch(
        store: &mut TodoStore,
        tool: &str,
        arguments: Value,
    ) -> Result<Value, String> {
        super::dispatch(store, tool, arguments, None).await
    }

    /// The scope the pane hands a process it launched for a task.
    fn bound(task_id: u64) -> SessionScope {
        SessionScope {
            profile: CODING_PROFILE.to_string(),
            task_id,
        }
    }

    #[tokio::test]
    async fn tools_are_declared_with_schemas() {
        let tools = tool_definitions();
        assert_eq!(tools.len(), 9);
        for tool in &tools {
            assert!(tool.get("name").and_then(Value::as_str).is_some());
            assert!(tool.pointer("/inputSchema/type").is_some());
        }
        // The old name is gone: `set_spec` is the write half of the pair.
        assert!(
            tools
                .iter()
                .all(|tool| tool["name"].as_str() != Some("save_spec"))
        );
    }

    #[tokio::test]
    async fn get_spec_reads_what_set_spec_wrote() {
        let mut store = store().await;
        let (task_id, _) = started_run(&mut store).await;
        let empty = dispatch(&mut store, "get_spec", json!({ "task_id": task_id }))
            .await
            .expect("get spec");
        assert!(empty["spec"].is_null());
        dispatch(
            &mut store,
            "set_spec",
            json!({ "task_id": task_id, "content": "# Spec" }),
        )
        .await
        .expect("set spec");
        let read = dispatch(&mut store, "get_spec", json!({ "task_id": task_id }))
            .await
            .expect("get spec");
        assert_eq!(read["spec"], "# Spec");
        // An unknown task is a tool error, never a silent null.
        assert!(dispatch(&mut store, "get_spec", json!({ "task_id": 9999 })).await.is_err());
    }

    #[tokio::test]
    async fn context_reports_the_open_phase() {
        let mut store = store().await;
        let (task_id, _) = started_run(&mut store).await;
        let context = dispatch(&mut store, "get_coding_context", json!({ "task_id": task_id }))
            .await
            .expect("context");
        assert_eq!(context["phase"], "interview");
        assert_eq!(context["round"], 1);
        assert!(context["spec"].is_null());
    }

    #[tokio::test]
    async fn set_spec_advances_to_the_spec_gate() {
        let mut store = store().await;
        let (task_id, _) = started_run(&mut store).await;
        let saved = dispatch(
            &mut store,
            "set_spec",
            json!({ "task_id": task_id, "content": "# Spec" }),
        )
        .await
        .expect("set spec");
        assert_eq!(saved["phase_advanced"], "spec");
        let spec = store.get_task_spec(task_id).await.expect("spec");
        assert_eq!(spec.as_deref(), Some("# Spec"));
        let context = dispatch(&mut store, "get_coding_context", json!({ "task_id": task_id }))
            .await
            .expect("context");
        assert_eq!(context["phase"], "spec");
    }

    #[tokio::test]
    async fn nested_sub_task_starts_its_own_run() {
        let mut store = store().await;
        let (task_id, _) = started_run(&mut store).await;
        let plain = dispatch(
            &mut store,
            "create_sub_task",
            json!({ "parent_task_id": task_id, "title": "Refresh tokens" }),
        )
        .await
        .expect("create sub-task");
        assert!(plain["run_id"].is_null());
        let nested = dispatch(
            &mut store,
            "create_sub_task",
            json!({ "parent_task_id": task_id, "title": "Session storage", "nested": true }),
        )
        .await
        .expect("create nested sub-task");
        let nested_run_id = nested["run_id"].as_u64().expect("nested run id");
        let nested_task_id = nested["sub_task_id"].as_u64().expect("nested task id");
        // The parent run keeps working; the nested run is rooted at the sub-task.
        assert_eq!(
            store
                .find_run_by_root_task(nested_task_id)
                .await
                .expect("nested lookup")
                .map(|run| run.id),
            Some(nested_run_id)
        );
        let context = dispatch(&mut store, "get_coding_context", json!({ "task_id": task_id }))
            .await
            .expect("context");
        let sub_tasks = context["sub_tasks"].as_array().expect("sub tasks");
        assert_eq!(sub_tasks.len(), 2);
    }

    #[tokio::test]
    async fn sub_task_interview_waits_for_the_user() {
        let mut store = store().await;
        let (task_id, _) = started_run(&mut store).await;
        let sub = dispatch(
            &mut store,
            "create_sub_task",
            json!({ "parent_task_id": task_id, "title": "Refresh tokens" }),
        )
        .await
        .expect("create sub-task");
        let sub_task_id = sub["sub_task_id"].as_u64().expect("sub-task id");
        let response = dispatch(
            &mut store,
            "request_sub_task_interview",
            json!({ "sub_task_id": sub_task_id, "reason": "unclear token lifetime" }),
        )
        .await
        .expect("request interview");
        assert_eq!(response["awaiting_user"], true);
        let run = store
            .find_run_by_root_task(sub_task_id)
            .await
            .expect("run lookup")
            .expect("interview run");
        let notes = run_notes(&run.step_results.0);
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].body, "unclear token lifetime");
    }

    #[tokio::test]
    async fn notes_and_branch_land_on_the_run() {
        let mut store = store().await;
        let (task_id, run_id) = started_run(&mut store).await;
        dispatch(
            &mut store,
            "append_note",
            json!({ "task_id": task_id, "kind": "finding", "body": "Guard empty input" }),
        )
        .await
        .expect("append note");
        let proposed = dispatch(
            &mut store,
            "propose_branch",
            json!({ "task_id": task_id, "name": "feature/42 add-oauth" }),
        )
        .await
        .expect("propose branch");
        assert_eq!(proposed["branch"], "feature/42-add-oauth");
        let run = store.get_run(run_id).await.expect("run");
        assert_eq!(run.branch.as_deref(), Some("feature/42-add-oauth"));
        assert_eq!(run.branch_status.as_deref(), Some("proposed"));
        let notes = run_notes(&run.step_results.0);
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].kind, "finding");
    }

    /// A feature run with two real subtasks added after the interview started.
    async fn started_run_with_subtasks(store: &mut TodoStore) -> (u64, u64, u64, u64) {
        let (task_id, run_id) = started_run(store).await;
        let mut ids = Vec::new();
        for title in ["Add token refresh", "Wire the callback URL"] {
            let sub = dispatch(
                store,
                "create_sub_task",
                json!({ "parent_task_id": task_id, "title": title }),
            )
            .await
            .expect("create sub-task");
            ids.push(sub["sub_task_id"].as_u64().expect("sub-task id"));
        }
        (task_id, run_id, ids[0], ids[1])
    }

    #[tokio::test]
    async fn context_reports_each_subtask_state() {
        let mut store = store().await;
        let (task_id, _, owned, covered) = started_run_with_subtasks(&mut store).await;
        let context = dispatch(&mut store, "get_coding_context", json!({ "task_id": task_id }))
            .await
            .expect("context");
        let sub_tasks = context["sub_tasks"].as_array().expect("sub tasks");
        assert_eq!(sub_tasks.len(), 2);
        // Both start unspecced, so the model is told to decide.
        assert!(
            sub_tasks
                .iter()
                .all(|sub| sub["spec"].as_str() == Some("none"))
        );
        assert!(sub_tasks[0].get("description").is_some());

        dispatch(
            &mut store,
            "set_spec",
            json!({
                "task_id": task_id,
                "content": "# Umbrella",
                "subtasks": [{ "task_id": owned, "spec": "Refresh hourly" }],
                "covered": [covered]
            }),
        )
        .await
        .expect("set spec");
        let context = dispatch(&mut store, "get_coding_context", json!({ "task_id": task_id }))
            .await
            .expect("context");
        let sub_tasks = context["sub_tasks"].as_array().expect("sub tasks");
        assert_eq!(sub_tasks[0]["spec"].as_str(), Some("own"));
        assert_eq!(sub_tasks[1]["spec"].as_str(), Some("covered"));
        // The whole payload advanced the run to the spec gate.
        assert_eq!(context["phase"], "spec");
        assert_eq!(
            store.get_task_spec(owned).await.expect("spec").as_deref(),
            Some("Refresh hourly")
        );
    }

    #[tokio::test]
    async fn set_spec_refuses_a_foreign_or_step_id_without_writing() {
        let mut store = store().await;
        let (task_id, _, owned, _) = started_run_with_subtasks(&mut store).await;
        // A step id is not a subtask, so the whole call fails and nothing is
        // written (decision #31).
        let context = dispatch(&mut store, "get_coding_context", json!({ "task_id": task_id }))
            .await
            .expect("context");
        let step_id = context["phases"][0]["task_id"].as_u64().expect("step id");
        let refused = dispatch(
            &mut store,
            "set_spec",
            json!({
                "task_id": task_id,
                "content": "# Umbrella",
                "subtasks": [{ "task_id": owned, "spec": "Refresh hourly" }],
                "covered": [step_id]
            }),
        )
        .await;
        assert!(refused.is_err(), "a step id is refused");
        assert!(store.get_task_spec(task_id).await.expect("spec").is_none());
        assert!(store.get_task_spec(owned).await.expect("spec").is_none());
    }

    /// A step id passed as the parent of a new subtask resolves to the run
    /// root, so steps never grow children (decision #15).
    #[tokio::test]
    async fn create_sub_task_under_a_step_lands_on_the_run_root() {
        let mut store = store().await;
        let (task_id, _) = started_run(&mut store).await;
        let context = dispatch(&mut store, "get_coding_context", json!({ "task_id": task_id }))
            .await
            .expect("context");
        let step_id = context["phases"][0]["task_id"].as_u64().expect("step id");
        let created = dispatch(
            &mut store,
            "create_sub_task",
            json!({ "parent_task_id": step_id, "title": "Bump the changelog" }),
        )
        .await
        .expect("create sub-task");
        assert_eq!(created["parent_task_id"].as_u64(), Some(task_id));
        let sub_task_id = created["sub_task_id"].as_u64().expect("sub-task id");
        assert_eq!(
            store.get_task(sub_task_id).await.expect("task").parent_id,
            Some(task_id)
        );
    }

    #[tokio::test]
    async fn complete_phase_only_advances_the_interview() {
        let mut store = store().await;
        let (task_id, _) = started_run(&mut store).await;
        let premature = dispatch(
            &mut store,
            "complete_phase",
            json!({ "task_id": task_id, "phase": "review" }),
        )
        .await;
        assert!(premature.is_err(), "the run is not in the review phase");
        let interview = dispatch(
            &mut store,
            "complete_phase",
            json!({ "task_id": task_id, "phase": "interview", "summary": "Asked 3 rounds" }),
        )
        .await
        .expect("complete interview");
        assert_eq!(interview["advanced"], true);
        let context = dispatch(&mut store, "get_coding_context", json!({ "task_id": task_id }))
            .await
            .expect("context");
        assert_eq!(context["phase"], "spec");
    }

    /// The session knows the task it was launched for, so a call that names no
    /// task still resolves. This is the fix for a model that cannot know the
    /// feature task's id: it no longer has to guess one.
    #[tokio::test]
    async fn a_bound_session_resolves_without_a_task_id() {
        let mut store = store().await;
        let (task_id, run_id) = started_run(&mut store).await;
        let context = super::dispatch(
            &mut store,
            "get_coding_context",
            json!({}),
            Some(&bound(task_id)),
        )
        .await
        .expect("context");
        assert_eq!(context["run_id"].as_u64(), Some(run_id));
        assert_eq!(context["phase"], "interview");
        assert_eq!(context["task"]["id"].as_u64(), Some(task_id));
        assert_eq!(
            context["task_chain"].as_array().map(Vec::len),
            Some(1),
            "the feature task is its own chain"
        );
    }

    /// A step's id (what `phases[].task_id` hands out) and a sub-task's id
    /// resolve to the run that owns them, and the reply names the task the call
    /// was about plus the road it took to the run's root.
    #[tokio::test]
    async fn a_step_or_subtask_id_resolves_to_the_run_above_it() {
        let mut store = store().await;
        let (task_id, run_id) = started_run(&mut store).await;
        let context = dispatch(
            &mut store,
            "get_coding_context",
            json!({ "task_id": task_id }),
        )
        .await
        .expect("context");
        let step_id = context["phases"][0]["task_id"].as_u64().expect("step id");

        let from_step = dispatch(
            &mut store,
            "get_coding_context",
            json!({ "task_id": step_id }),
        )
        .await
        .expect("a step id is a task of the run");
        assert_eq!(from_step["run_id"].as_u64(), Some(run_id));
        assert_eq!(from_step["task"]["id"].as_u64(), Some(step_id));
        assert_eq!(from_step["task"]["role"].as_str(), Some("step"));
        let chain: Vec<u64> = from_step["task_chain"]
            .as_array()
            .expect("chain")
            .iter()
            .map(|task| task["id"].as_u64().expect("id"))
            .collect();
        assert_eq!(
            chain,
            vec![step_id, task_id],
            "the chain runs from the named task to the run's root"
        );

        let created = dispatch(
            &mut store,
            "create_sub_task",
            json!({ "parent_task_id": task_id, "title": "Token refresh" }),
        )
        .await
        .expect("create sub-task");
        let sub_task_id = created["sub_task_id"].as_u64().expect("sub-task id");
        let from_sub = dispatch(
            &mut store,
            "get_coding_context",
            json!({ "task_id": sub_task_id }),
        )
        .await
        .expect("a sub-task id is a task of the run");
        assert_eq!(from_sub["run_id"].as_u64(), Some(run_id));
        assert_eq!(from_sub["task"]["id"].as_u64(), Some(sub_task_id));
        assert_eq!(from_sub["task"]["role"], Value::Null);

        // A finished run is an answer: the model is told the run is over, not
        // that it does not exist. The branch is what keeps its view on screen.
        store
            .set_run_branch(run_id, "feature/1-add-oauth", Some("main"))
            .await
            .expect("branch");
        store.cancel_run(run_id).await.expect("cancel the run");
        let finished = dispatch(
            &mut store,
            "get_coding_context",
            json!({ "task_id": step_id }),
        )
        .await
        .expect("a finished run answers");
        assert_eq!(finished["status"], "cancelled");
    }

    /// An explicit id wins over the session's task: a call that names a task is
    /// taken at its word, and an id in no run anywhere is refused rather than
    /// quietly resolved to the session's.
    #[tokio::test]
    async fn the_named_task_wins_over_the_session() {
        let mut store = store().await;
        let (first, _) = started_run(&mut store).await;
        let recipe_id = store
            .recipe_id_by_slug("coding-task")
            .await
            .expect("recipe lookup")
            .expect("coding-task recipe");
        let second = store
            .create_task(TaskCreate::default().title("Add billing".to_string()))
            .await
            .expect("second feature task");
        let second_run = store
            .create_task_run(second.id, recipe_id, json!({}))
            .await
            .expect("second run");

        let named = super::dispatch(
            &mut store,
            "get_coding_context",
            json!({ "task_id": second.id }),
            Some(&bound(first)),
        )
        .await
        .expect("context");
        assert_eq!(named["run_id"].as_u64(), Some(second_run.id));
        assert_eq!(named["task"]["id"].as_u64(), Some(second.id));

        let lonely = store
            .create_task(TaskCreate::default().title("Loose".to_string()))
            .await
            .expect("lonely task");
        let refused = super::dispatch(
            &mut store,
            "get_coding_context",
            json!({ "task_id": lonely.id }),
            Some(&bound(first)),
        )
        .await
        .expect_err("the named task is answered, not the session's");
        assert!(refused.contains("has no coding run"), "{refused}");
    }

    /// A call that names no task and comes from an unbound session says which
    /// is missing rather than resolving to something arbitrary.
    #[tokio::test]
    async fn an_unbound_call_with_no_task_id_says_so() {
        let mut store = store().await;
        // A run exists; the call simply names nothing and has no session.
        started_run(&mut store).await;
        let error = dispatch(&mut store, "get_coding_context", json!({}))
            .await
            .expect_err("nothing to resolve");
        assert!(error.contains("not bound"), "{error}");
    }

    #[tokio::test]
    async fn unknown_tool_is_an_error() {
        let mut store = store().await;
        let error = dispatch(&mut store, "nope", json!({})).await;
        assert!(error.is_err());
    }

    /// The whole transport, over a real socket: initialize, tools/list, then a
    /// tool call that moves the run. Multi-threaded because the test blocks a
    /// worker on synchronous socket I/O while the server thread drives the
    /// store through `Handle::block_on`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn serves_json_rpc_over_loopback_http() {
        let mut store = store().await;
        let (task_id, _) = started_run(&mut store).await;
        let handle = tokio::runtime::Handle::current();
        let server = start(Store::new(store), handle, None).expect("start server");
        let coding_token = server.open_session(CODING_PROFILE, task_id);

        let initialize = request(server.url(), &coding_token, &json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}
        }));
        assert_eq!(
            initialize.pointer("/result/protocolVersion").and_then(Value::as_str),
            Some("2024-11-05")
        );

        let tools = request(server.url(), &coding_token, &json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}
        }));
        assert_eq!(
            tools.pointer("/result/tools").and_then(Value::as_array).map(Vec::len),
            Some(9)
        );

        // No `task_id`: the token this session was handed names the task.
        let call = request(server.url(), &coding_token, &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": { "name": "get_coding_context", "arguments": {} }
        }));
        assert!(call.get("result").is_some(), "{call}");
        let text = call.pointer("/result/content/0/text").and_then(Value::as_str).expect("text");
        let context: Value = serde_json::from_str(text).expect("the payload is JSON");
        assert_eq!(context["phase"], "interview");
        assert_eq!(
            context["task"]["id"].as_u64(),
            Some(task_id),
            "the session's task is what the call is about"
        );

        // A wrong token is rejected before any tool runs.
        let unauthorized = raw_request(server.url(), "not-the-token", &json!({
            "jsonrpc": "2.0", "id": 4, "method": "tools/list", "params": {}
        }));
        assert!(unauthorized.starts_with("HTTP/1.1 401"), "{unauthorized}");

        // Every session gets its own token, and each is accepted.
        let interview_token = server.open_session(INTERVIEW_PROFILE, task_id);
        assert_ne!(interview_token, coding_token);
        let interview_tools = request(server.url(), &interview_token, &json!({
            "jsonrpc": "2.0", "id": 5, "method": "tools/list", "params": {}
        }));
        assert!(interview_tools.get("result").is_some());
        assert!(
            !server.close_session("nope"),
            "retiring a token that was never handed out is not an error"
        );

        // Retiring a session takes its token with it: a replaced or torn-down
        // process cannot call tools afterwards.
        assert!(server.close_session(&coding_token));
        let retired = raw_request(server.url(), &coding_token, &json!({
            "jsonrpc": "2.0", "id": 6, "method": "tools/list", "params": {}
        }));
        assert!(retired.starts_with("HTTP/1.1 401"), "{retired}");
    }

    /// One POST with the bearer token, returning the parsed response body.
    fn request(url: &str, token: &str, body: &Value) -> Value {
        let response = raw_request(url, token, body);
        let (_, payload) = response
            .split_once("\r\n\r\n")
            .expect("headers and body are separated");
        serde_json::from_str(payload).expect("a JSON-RPC response")
    }

    fn raw_request(url: &str, token: &str, body: &Value) -> String {
        let body = body.to_string();
        let address = url
            .trim_start_matches("http://")
            .trim_end_matches("/mcp")
            .to_string();
        let mut stream = TcpStream::connect(&address).expect("connect");
        let request = format!(
            "POST /mcp HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(request.as_bytes()).expect("write request");
        let mut response = String::new();
        stream.read_to_string(&mut response).expect("read response");
        response
    }
}
