//! The coding-workflow MCP server.
//!
//! todo-2 hosts a loopback Model Context Protocol endpoint so the agent can
//! attach the spec it interviewed out, create and interview sub-tasks, log
//! annotations, propose the feature branch, and report phase completion. The
//! transport is a minimal HTTP/1.1 listener bound to `127.0.0.1:0` with a
//! per-process bearer token; only the `initialize`, `tools/list`, and
//! `tools/call` methods are implemented, which is the subset the coding
//! workflow uses.
//!
//! Tool calls mutate the same `TodoStore` the UI uses, and each mutating call
//! signals the foreground app so the stepper reloads.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};

use serde_json::{Value, json};
use storage::prelude::*;
use storage::task::TaskCreate;

use crate::store::Store;

/// A running loopback MCP endpoint. Dropping it leaves the listener thread
/// running for the life of the process (there is nothing to release).
pub struct CodingMcpServer {
    url: String,
    token: String,
}

impl CodingMcpServer {
    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn token(&self) -> &str {
        &self.token
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
    let token = random_token();
    let server_token = token.clone();
    std::thread::Builder::new()
        .name("coding-mcp".to_string())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else {
                    continue;
                };
                if let Err(error) = serve(stream, &store, &handle, &server_token, &notify) {
                    tracing::warn!("coding MCP request failed: {error}");
                }
            }
        })?;
    Ok(CodingMcpServer {
        url: format!("http://{address}/mcp"),
        token,
    })
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
    token: &str,
    notify: &Option<tokio::sync::mpsc::UnboundedSender<()>>,
) -> anyhow::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(());
    }
    let mut content_length = 0usize;
    let mut authorized = false;
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
            "authorization" => authorized = value == format!("Bearer {token}"),
            _ => {}
        }
    }
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body)?;
    let mut stream = stream;

    if !request_line.starts_with("POST ") {
        return write_response(&mut stream, 405, None);
    }
    if !authorized {
        return write_response(&mut stream, 401, None);
    }
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
            let outcome = handle.block_on(async {
                let mut store = store.0.lock().await;
                dispatch(&mut store, name, arguments).await
            });
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

/// The coding tools exposed to the agent, per §7.2 of the spec.
pub fn tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "get_coding_context",
            "description": "Read the current coding run: phase, spec, cycle, branch, annotation log and steps. `phases[].task_id` is the task id of each step (and `open_task_id` the one being worked on); pass it as `parent_task_id` to nest work under that step. Call this before acting on a phase.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "task_id": { "type": "integer", "description": "The feature task id." },
                    "run_id": { "type": "integer", "description": "The workflow run id, when known." }
                }
            }
        }),
        json!({
            "name": "save_spec",
            "description": "Save the spec you interviewed out: the umbrella spec on the feature task, plus a spec for each subtask that needs its own, plus the ids of the subtasks the umbrella covers. Completes the interview phase. Every subtask id must be a direct subtask (never a workflow step).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "task_id": { "type": "integer" },
                    "content": { "type": "string", "description": "The umbrella spec markdown." },
                    "path": { "type": "string", "description": "Where the spec was written on disk." },
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
                "required": ["task_id", "content"]
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
                    "task_id": { "type": "integer" },
                    "kind": { "type": "string", "description": "annotation | finding | decision" },
                    "body": { "type": "string" }
                },
                "required": ["task_id", "body"]
            }
        }),
        json!({
            "name": "propose_branch",
            "description": "Propose the feature branch name recorded on the run. The app creates it once the spec is approved.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "task_id": { "type": "integer" },
                    "name": { "type": "string" },
                    "summary": { "type": "string" }
                },
                "required": ["task_id", "name"]
            }
        }),
        json!({
            "name": "complete_phase",
            "description": "Report that the current phase's work is done. The interview advances automatically; implementation waits for the user's confirmation.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "task_id": { "type": "integer" },
                    "phase": { "type": "string", "description": "interview | spec | implement | review | merge" },
                    "summary": { "type": "string" }
                },
                "required": ["task_id", "phase"]
            }
        }),
        json!({
            "name": "propose_summary",
            "description": "Record a commit message and optional summary for the merge step.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "task_id": { "type": "integer" },
                    "commit_message": { "type": "string" },
                    "pr_summary": { "type": "string" }
                },
                "required": ["task_id", "commit_message"]
            }
        }),
    ]
}

/// Dispatch one JSON-RPC `tools/call` into the store. Errors are returned as
/// strings so the server can answer with `isError: true` instead of failing
/// the transport.
pub async fn dispatch(
    store: &mut TodoStore,
    tool: &str,
    arguments: Value,
) -> Result<Value, String> {
    match tool {
        "get_coding_context" => get_coding_context(store, &arguments).await,
        "save_spec" => save_spec(store, &arguments).await,
        "create_sub_task" => create_sub_task(store, &arguments).await,
        "request_sub_task_interview" => request_sub_task_interview(store, &arguments).await,
        "append_note" => append_note(store, &arguments).await,
        "propose_branch" => propose_branch(store, &arguments).await,
        "complete_phase" => complete_phase(store, &arguments).await,
        "propose_summary" => propose_summary(store, &arguments).await,
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

/// Resolve the run a tool call targets: by explicit `run_id`, else by the
/// root feature task. A task in no run is an error rather than a silent no-op.
async fn resolve(
    store: &mut TodoStore,
    arguments: &Value,
) -> Result<(WorkflowRun, RunView), String> {
    let run = match arguments.get("run_id").and_then(Value::as_u64) {
        Some(run_id) => Some(
            store
                .find_run(run_id)
                .await
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("no workflow run {run_id}"))?,
        ),
        None => {
            let task_id = arg_u64(arguments, "task_id")?;
            store
                .find_run_by_root_task(task_id)
                .await
                .map_err(|e| e.to_string())?
        }
    };
    let run = run.ok_or_else(|| "this task has no coding run".to_string())?;
    let view = store
        .workflow_run_view(run.id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("workflow run {} is not visible", run.id))?;
    Ok((run, view))
}

async fn get_coding_context(store: &mut TodoStore, arguments: &Value) -> Result<Value, String> {
    let (run, view) = resolve(store, arguments).await?;
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
            .get_task(task_id)
            .await
            .map_err(|e| e.to_string())?
            .spec,
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

async fn save_spec(store: &mut TodoStore, arguments: &Value) -> Result<Value, String> {
    let (_run, view) = resolve(store, arguments).await?;
    let task_id = view
        .run
        .root_task_id
        .ok_or_else(|| "this run has no feature task".to_string())?;
    let content = arg_str(arguments, "content").ok_or_else(|| "`content` is required".to_string())?;
    let path = arg_str(arguments, "path");
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
        .save_subtask_specs(task_id, Some(content), path, subtask_specs, covered)
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

async fn append_note(store: &mut TodoStore, arguments: &Value) -> Result<Value, String> {
    let (run, _) = resolve(store, arguments).await?;
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

async fn propose_branch(store: &mut TodoStore, arguments: &Value) -> Result<Value, String> {
    let (run, _) = resolve(store, arguments).await?;
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

async fn complete_phase(store: &mut TodoStore, arguments: &Value) -> Result<Value, String> {
    let (run, view) = resolve(store, arguments).await?;
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

async fn propose_summary(store: &mut TodoStore, arguments: &Value) -> Result<Value, String> {
    let (run, _) = resolve(store, arguments).await?;
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

    #[tokio::test]
    async fn tools_are_declared_with_schemas() {
        let tools = tool_definitions();
        assert_eq!(tools.len(), 8);
        for tool in &tools {
            assert!(tool.get("name").and_then(Value::as_str).is_some());
            assert!(tool.pointer("/inputSchema/type").is_some());
        }
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
    async fn save_spec_advances_to_the_spec_gate() {
        let mut store = store().await;
        let (task_id, _) = started_run(&mut store).await;
        let saved = dispatch(
            &mut store,
            "save_spec",
            json!({ "task_id": task_id, "content": "# Spec", "path": "docs/spec/x.md" }),
        )
        .await
        .expect("save spec");
        assert_eq!(saved["phase_advanced"], "spec");
        let task = store.get_task(task_id).await.expect("task");
        assert_eq!(task.spec.as_deref(), Some("# Spec"));
        assert_eq!(task.spec_path.as_deref(), Some("docs/spec/x.md"));
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
            "save_spec",
            json!({
                "task_id": task_id,
                "content": "# Umbrella",
                "subtasks": [{ "task_id": owned, "spec": "Refresh hourly" }],
                "covered": [covered]
            }),
        )
        .await
        .expect("save spec");
        let context = dispatch(&mut store, "get_coding_context", json!({ "task_id": task_id }))
            .await
            .expect("context");
        let sub_tasks = context["sub_tasks"].as_array().expect("sub tasks");
        assert_eq!(sub_tasks[0]["spec"].as_str(), Some("own"));
        assert_eq!(sub_tasks[1]["spec"].as_str(), Some("covered"));
        // The whole payload advanced the run to the spec gate.
        assert_eq!(context["phase"], "spec");
        assert_eq!(
            store.get_task(owned).await.expect("task").spec.as_deref(),
            Some("Refresh hourly")
        );
    }

    #[tokio::test]
    async fn save_spec_refuses_a_foreign_or_step_id_without_writing() {
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
            "save_spec",
            json!({
                "task_id": task_id,
                "content": "# Umbrella",
                "subtasks": [{ "task_id": owned, "spec": "Refresh hourly" }],
                "covered": [step_id]
            }),
        )
        .await;
        assert!(refused.is_err(), "a step id is refused");
        assert!(store.get_task(task_id).await.expect("task").spec.is_none());
        assert!(store.get_task(owned).await.expect("task").spec.is_none());
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

        let initialize = request(server.url(), server.token(), &json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}
        }));
        assert_eq!(
            initialize.pointer("/result/protocolVersion").and_then(Value::as_str),
            Some("2024-11-05")
        );

        let tools = request(server.url(), server.token(), &json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}
        }));
        assert_eq!(
            tools.pointer("/result/tools").and_then(Value::as_array).map(Vec::len),
            Some(8)
        );

        let call = request(server.url(), server.token(), &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": { "name": "get_coding_context", "arguments": { "task_id": task_id } }
        }));
        assert!(call.get("result").is_some(), "{call}");
        let text = call.pointer("/result/content/0/text").and_then(Value::as_str).expect("text");
        assert!(text.contains("\"interview\""), "{text}");

        // A wrong token is rejected before any tool runs.
        let unauthorized = raw_request(server.url(), "not-the-token", &json!({
            "jsonrpc": "2.0", "id": 4, "method": "tools/list", "params": {}
        }));
        assert!(unauthorized.starts_with("HTTP/1.1 401"), "{unauthorized}");
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
