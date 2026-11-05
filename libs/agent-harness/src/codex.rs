//! Codex adapter over `codex app-server`, with a `codex exec --json` fallback
//! for builds that predate the subcommand.
//!
//! The protocol is newline-delimited JSON-RPC 2.0 over stdio with the `jsonrpc`
//! header omitted. `probe` answers which tier this build supports before the
//! core commits to it; the harness translates notifications and server requests
//! into [`HarnessEvent`]s and never writes to the store itself.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::{Mutex as AsyncMutex, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::{
    AgentHarness, ApprovalId, ApprovalRequest, Capabilities, Decision, DeltaKind, FileDiff,
    HarnessError, HarnessEvent, HarnessResult, ItemId, PlanRow, PromptInput, SessionId,
    SessionState, SpawnSpec, ToolCallId, ToolStatus, TurnFinishReason, TurnId,
};

/// Protocol methods Shepherd knows how to drive. A build that advertises fewer
/// methods gets the intersection, so a control is never shown for a method the
/// server does not implement.
pub const SUPPORTED_METHODS: [&str; 6] = [
    "thread/start",
    "thread/resume",
    "turn/start",
    "turn/interrupt",
    "item/commandExecution/requestApproval",
    "item/fileChange/requestApproval",
];

/// JSON-RPC code for an unsupported request. Answering with it keeps the
/// session alive instead of leaving the server waiting on Shepherd.
const METHOD_NOT_FOUND: i64 = -32601;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);
const STDERR_TAIL_LINES: usize = 20;

/// Which Codex integration this build can drive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodexTier {
    /// `codex app-server` answered the handshake.
    AppServer,
    /// The app-server subcommand is absent; the `exec --json` tier is the fallback.
    Exec,
}

/// What a probe learned about one Codex installation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodexProbe {
    pub tier: CodexTier,
    pub version: Option<String>,
    pub methods: Vec<String>,
}

impl CodexProbe {
    pub fn supports(&self, method: &str) -> bool {
        self.methods.iter().any(|supported| supported == method)
    }
}

/// Probe `program` without leaving a process behind. A missing binary and a
/// broken handshake are errors; a build without the subcommand is the `Exec`
/// tier, which the caller offers as the fallback.
pub async fn probe(program: &str, cwd: &Path) -> HarnessResult<CodexProbe> {
    let version = read_version(program).await;
    let handshake = run_handshake(program, cwd).await?;
    Ok(CodexProbe {
        tier: if handshake.supported {
            CodexTier::AppServer
        } else {
            CodexTier::Exec
        },
        version,
        methods: handshake.methods,
    })
}

struct Handshake {
    supported: bool,
    methods: Vec<String>,
}

async fn read_version(program: &str) -> Option<String> {
    let output = match Command::new(program).arg("--version").output().await {
        Ok(output) => output,
        Err(error) => {
            tracing::debug!(program, %error, "could not read the codex version");
            return None;
        }
    };
    if !output.status.success() {
        tracing::debug!(program, status = ?output.status, "the version command failed");
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}

async fn run_handshake(program: &str, cwd: &Path) -> HarnessResult<Handshake> {
    let mut command = Command::new(program);
    command
        .arg("app-server")
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = command
        .spawn()
        .map_err(|source| spawn_error(program, source))?;
    let Some(mut stdin) = child.stdin.take() else {
        return Err(HarnessError::Protocol(
            "codex app-server stdin was not captured".to_string(),
        ));
    };
    let Some(stdout) = child.stdout.take() else {
        return Err(HarnessError::Protocol(
            "codex app-server stdout was not captured".to_string(),
        ));
    };
    let Some(stderr) = child.stderr.take() else {
        return Err(HarnessError::Protocol(
            "codex app-server stderr was not captured".to_string(),
        ));
    };
    let stderr_state = Arc::new(Mutex::new(VecDeque::new()));
    let stderr_task = spawn_stderr_reader(stderr, Arc::clone(&stderr_state));

    let request = json!({ "method": "initialize", "id": 1, "params": initialize_params() });
    let mut line = serde_json::to_string(&request)
        .map_err(|error| HarnessError::Protocol(error.to_string()))?;
    line.push('\n');
    if let Err(error) = stdin.write_all(line.as_bytes()).await {
        stderr_task.abort();
        return Err(HarnessError::Protocol(format!(
            "could not write the initialize handshake: {error}"
        )));
    }
    if let Err(error) = stdin.flush().await {
        stderr_task.abort();
        return Err(HarnessError::Protocol(format!(
            "could not flush the initialize handshake: {error}"
        )));
    }

    let mut lines = BufReader::new(stdout).lines();
    let outcome = tokio::time::timeout(PROBE_TIMEOUT, async {
        for _ in 0..50 {
            let line = match lines.next_line().await {
                Ok(Some(line)) => line,
                Ok(None) => return Ok(None),
                Err(error) => return Err(error),
            };
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let Ok(message) = serde_json::from_str::<Value>(trimmed) else {
                continue;
            };
            if message.get("id").and_then(Value::as_i64) == Some(1) {
                return Ok(Some(message));
            }
        }
        Ok(None)
    })
    .await;

    // Dropping the child kills it; the probe never leaves an app-server running.
    drop(stdin);
    stderr_task.abort();

    match outcome {
        Err(_) => Err(HarnessError::Protocol(format!(
            "{program} app-server did not answer the initialize handshake within {}s",
            PROBE_TIMEOUT.as_secs()
        ))),
        Ok(Err(error)) => Err(HarnessError::Protocol(format!(
            "reading the {program} handshake failed: {error}"
        ))),
        Ok(Ok(None)) => Ok(Handshake {
            supported: false,
            methods: Vec::new(),
        }),
        Ok(Ok(Some(message))) => match message.get("error") {
            Some(error) => {
                let code = error.get("code").and_then(Value::as_i64);
                if code == Some(METHOD_NOT_FOUND) {
                    Ok(Handshake {
                        supported: false,
                        methods: Vec::new(),
                    })
                } else {
                    let message = error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("the codex server rejected initialize");
                    Err(HarnessError::Protocol(message.to_string()))
                }
            }
            None => Ok(Handshake {
                supported: true,
                methods: supported_methods(message.get("result")),
            }),
        },
    }
}

fn initialize_params() -> Value {
    json!({
        "clientInfo": {
            "name": "shepherd",
            "title": "Shepherd",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "capabilities": {},
    })
}

fn spawn_error(program: &str, source: std::io::Error) -> HarnessError {
    if source.kind() == std::io::ErrorKind::NotFound {
        HarnessError::MissingProgram {
            program: program.to_string(),
        }
    } else {
        HarnessError::Spawn {
            program: program.to_string(),
            source,
        }
    }
}

/// Keep only methods both the server and Shepherd implement. A result that
/// advertises nothing is treated as the documented baseline.
fn supported_methods(result: Option<&Value>) -> Vec<String> {
    let advertised = result.and_then(advertised_methods);
    SUPPORTED_METHODS
        .iter()
        .filter(|method| match &advertised {
            Some(methods) => methods.iter().any(|value| value == *method),
            None => true,
        })
        .map(|method| method.to_string())
        .collect()
}

fn advertised_methods(result: &Value) -> Option<Vec<String>> {
    let methods = result
        .get("methods")
        .and_then(Value::as_array)
        .or_else(|| {
            result
                .get("capabilities")
                .and_then(|capabilities| capabilities.get("methods"))
                .and_then(Value::as_array)
        })?;
    Some(
        methods
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
    )
}

#[derive(Default)]
struct Pending {
    responses: HashMap<i64, oneshot::Sender<Result<Value, String>>>,
    approvals: HashMap<String, i64>,
    thread_id: Option<String>,
    turn_id: Option<String>,
}

/// One `codex app-server` session.
pub struct CodexHarness {
    sender: mpsc::UnboundedSender<HarnessEvent>,
    receiver: mpsc::UnboundedReceiver<HarnessEvent>,
    outgoing: Option<mpsc::UnboundedSender<Value>>,
    child: Option<Child>,
    pending: Arc<AsyncMutex<Pending>>,
    stderr: Arc<Mutex<VecDeque<String>>>,
    closed: Arc<AtomicBool>,
    closing: Arc<AtomicBool>,
    stdout_task: Option<JoinHandle<()>>,
    stderr_task: Option<JoinHandle<()>>,
    writer_task: Option<JoinHandle<()>>,
    next_request_id: i64,
    supported: BTreeSet<String>,
    cwd: PathBuf,
    session_id: Option<SessionId>,
    started: bool,
}

impl CodexHarness {
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        Self {
            sender,
            receiver,
            outgoing: None,
            child: None,
            pending: Arc::new(AsyncMutex::new(Pending::default())),
            stderr: Arc::new(Mutex::new(VecDeque::new())),
            closed: Arc::new(AtomicBool::new(false)),
            closing: Arc::new(AtomicBool::new(false)),
            stdout_task: None,
            stderr_task: None,
            writer_task: None,
            next_request_id: 1,
            supported: SUPPORTED_METHODS
                .iter()
                .map(|method| method.to_string())
                .collect(),
            cwd: PathBuf::new(),
            session_id: None,
            started: false,
        }
    }

    fn supports(&self, method: &str) -> bool {
        self.supported.contains(method)
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            streaming: true,
            thoughts: true,
            tool_calls: true,
            approvals: self.supports("item/commandExecution/requestApproval")
                || self.supports("item/fileChange/requestApproval"),
            resume: self.supports("thread/resume"),
            ..Capabilities::default()
        }
    }

    fn send(&self, message: Value) -> HarnessResult<()> {
        let outgoing = self.outgoing.as_ref().ok_or(HarnessError::NotStarted)?;
        outgoing
            .send(message)
            .map_err(|_| HarnessError::ChannelClosed)
    }

    async fn request(&mut self, method: &str, params: Value) -> HarnessResult<Value> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(HarnessError::Protocol(format!(
                "codex is no longer running; cannot call {method}"
            )));
        }

        let id = self.next_request_id;
        self.next_request_id += 1;
        let (sender, receiver) = oneshot::channel();
        self.pending.lock().await.responses.insert(id, sender);
        self.send(json!({ "method": method, "id": id, "params": params }))?;

        match timeout(REQUEST_TIMEOUT, receiver).await {
            Err(_) => {
                self.pending.lock().await.responses.remove(&id);
                Err(HarnessError::Protocol(format!(
                    "codex did not answer {method}"
                )))
            }
            Ok(Err(_)) => Err(HarnessError::Protocol(format!(
                "codex dropped the answer to {method}"
            ))),
            Ok(Ok(Err(message))) => Err(HarnessError::Protocol(format!("{method}: {message}"))),
            Ok(Ok(Ok(value))) => Ok(value),
        }
    }
}

impl Default for CodexHarness {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentHarness for CodexHarness {
    fn events(&mut self) -> &mut mpsc::UnboundedReceiver<HarnessEvent> {
        &mut self.receiver
    }

    async fn start(&mut self, spec: SpawnSpec) -> HarnessResult<Capabilities> {
        if self.started {
            return Err(HarnessError::Protocol(
                "the codex session is already started".to_string(),
            ));
        }

        let mut command = Command::new(&spec.program);
        command
            .args(&spec.args)
            .current_dir(&spec.cwd)
            .envs(&spec.environment)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let mut child = command
            .spawn()
            .map_err(|source| spawn_error(&spec.program, source))?;
        let Some(stdin) = child.stdin.take() else {
            return Err(HarnessError::Protocol(
                "codex stdin was not captured".to_string(),
            ));
        };
        let Some(stdout) = child.stdout.take() else {
            return Err(HarnessError::Protocol(
                "codex stdout was not captured".to_string(),
            ));
        };
        let Some(stderr) = child.stderr.take() else {
            return Err(HarnessError::Protocol(
                "codex stderr was not captured".to_string(),
            ));
        };

        self.cwd = spec.cwd;
        self.child = Some(child);

        let (outgoing, outgoing_receiver) = mpsc::unbounded_channel();
        self.writer_task = Some(spawn_writer(stdin, outgoing_receiver));
        self.stdout_task = Some(spawn_reader(
            stdout,
            self.sender.clone(),
            Arc::clone(&self.pending),
            outgoing.clone(),
            Arc::clone(&self.stderr),
            Arc::clone(&self.closed),
            Arc::clone(&self.closing),
        ));
        self.stderr_task = Some(spawn_stderr_reader(stderr, Arc::clone(&self.stderr)));
        self.outgoing = Some(outgoing);
        self.started = true;

        let result = self.request("initialize", initialize_params()).await?;
        if let Some(advertised) = advertised_methods(&result) {
            self.supported = SUPPORTED_METHODS
                .iter()
                .filter(|method| advertised.iter().any(|value| value == *method))
                .map(|method| method.to_string())
                .collect();
        }
        self.send(json!({ "method": "initialized", "params": {} }))?;
        self.sender
            .send(HarnessEvent::SessionState {
                state: SessionState::Ready,
            })
            .map_err(|_| HarnessError::ChannelClosed)?;

        Ok(self.capabilities())
    }

    async fn open_session(&mut self, resume: Option<SessionId>) -> HarnessResult<SessionId> {
        if !self.started {
            return Err(HarnessError::NotStarted);
        }

        let cwd = self.cwd.display().to_string();
        let response = match &resume {
            Some(session) => {
                if !self.supports("thread/resume") {
                    return Err(HarnessError::Protocol(
                        "this codex build does not support thread/resume".to_string(),
                    ));
                }
                self.request("thread/resume", json!({ "threadId": session.as_str() }))
                    .await?
            }
            None => {
                let response = self
                    .request(
                        "thread/start",
                        json!({
                            "cwd": cwd,
                            "approvalPolicy": "on-request",
                            "sandbox": "workspaceWrite",
                        }),
                    )
                    .await?;
                response
            }
        };

        let thread_id = read_thread_id(&response)
            .or_else(|| resume.as_ref().map(|session| session.as_str().to_string()))
            .ok_or_else(|| {
                HarnessError::Protocol("the thread response did not include thread.id".to_string())
            })?;

        self.pending.lock().await.thread_id = Some(thread_id.clone());
        let session_id = SessionId::new(thread_id);
        self.session_id = Some(session_id.clone());
        Ok(session_id)
    }

    async fn prompt(&mut self, input: PromptInput) -> HarnessResult<()> {
        let session_id = self
            .session_id
            .clone()
            .ok_or(HarnessError::SessionMissing)?;
        if !self.pending.lock().await.approvals.is_empty() {
            return Err(HarnessError::ApprovalPending);
        }

        let cwd = self.cwd.display().to_string();
        let response = self
            .request(
                "turn/start",
                json!({
                    "threadId": session_id.as_str(),
                    "input": [ { "type": "text", "text": input.text } ],
                    "cwd": cwd,
                    "approvalPolicy": "on-request",
                    "sandboxPolicy": {
                        "type": "workspaceWrite",
                        "writableRoots": [cwd],
                        "networkAccess": false,
                    },
                }),
            )
            .await?;

        let turn_id = read_turn_id(&response).ok_or_else(|| {
            HarnessError::Protocol("the turn response did not include turn.id".to_string())
        })?;
        self.pending.lock().await.turn_id = Some(turn_id.clone());
        self.sender
            .send(HarnessEvent::TurnStarted {
                turn_id: TurnId::new(turn_id),
            })
            .map_err(|_| HarnessError::ChannelClosed)?;
        Ok(())
    }

    async fn cancel(&mut self) -> HarnessResult<()> {
        let (thread_id, turn_id) = {
            let pending = self.pending.lock().await;
            match (pending.thread_id.clone(), pending.turn_id.clone()) {
                (Some(thread_id), Some(turn_id)) => (thread_id, turn_id),
                _ => return Ok(()),
            }
        };
        self.request(
            "turn/interrupt",
            json!({ "threadId": thread_id, "turnId": turn_id }),
        )
        .await?;
        Ok(())
    }

    async fn respond(&mut self, approval_id: ApprovalId, decision: Decision) -> HarnessResult<()> {
        let request_id = self
            .pending
            .lock()
            .await
            .approvals
            .remove(approval_id.as_str())
            .ok_or(HarnessError::NoPendingApproval)?;
        let answer = match decision {
            Decision::Allow => "accept",
            Decision::Deny => "decline",
        };
        self.send(json!({ "id": request_id, "result": { "decision": answer } }))?;
        self.sender
            .send(HarnessEvent::ApprovalResolved {
                approval_id,
                decision,
            })
            .map_err(|_| HarnessError::ChannelClosed)?;
        Ok(())
    }

    async fn shutdown(&mut self) -> HarnessResult<()> {
        if !self.started {
            return Ok(());
        }
        self.closing.store(true, Ordering::Relaxed);
        self.outgoing = None;

        let code = match self.child.take() {
            Some(mut child) => match child.kill().await {
                Ok(()) => child
                    .try_wait()
                    .ok()
                    .flatten()
                    .and_then(|status| status.code()),
                Err(error) => {
                    tracing::debug!(%error, "could not kill the codex process");
                    None
                }
            },
            None => None,
        };

        if let Some(task) = self.stdout_task.take() {
            task.abort();
        }
        if let Some(task) = self.stderr_task.take() {
            task.abort();
        }
        if let Some(task) = self.writer_task.take() {
            task.abort();
        }
        self.started = false;
        self.session_id = None;
        self.pending.lock().await.approvals.clear();

        self.sender
            .send(HarnessEvent::Exited {
                code,
                stderr: stderr_tail(&self.stderr),
            })
            .map_err(|_| HarnessError::ChannelClosed)?;
        Ok(())
    }
}

/// The `codex exec --json` tier, for builds without `app-server`.
///
/// Exec mode runs one process per turn, prints its stream as
/// `{"id":"…","msg":{"type":"…"}}` lines, suppresses delta events, reports a
/// turn's result only by exiting, and cannot answer approvals. Its capability
/// set says so, so the core never offers the controls it lacks.
pub struct ExecHarness {
    sender: mpsc::UnboundedSender<HarnessEvent>,
    receiver: mpsc::UnboundedReceiver<HarnessEvent>,
    program: String,
    args: Vec<String>,
    cwd: PathBuf,
    environment: BTreeMap<String, String>,
    stderr: Arc<Mutex<VecDeque<String>>>,
    stderr_task: Option<JoinHandle<()>>,
    run: Option<ExecRun>,
    session_id: Option<SessionId>,
    next_turn: u64,
    started: bool,
}

/// One `codex exec --json` process. The waiter owns the child, so a cancel and
/// a natural exit cannot both reap the same process.
struct ExecRun {
    waiter: JoinHandle<()>,
    reader: JoinHandle<()>,
    kill: Option<oneshot::Sender<()>>,
    finished: Arc<AtomicBool>,
}

static EXEC_SESSION_COUNTER: AtomicU64 = AtomicU64::new(1);

impl ExecHarness {
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        Self {
            sender,
            receiver,
            program: String::new(),
            args: Vec::new(),
            cwd: PathBuf::new(),
            environment: BTreeMap::new(),
            stderr: Arc::new(Mutex::new(VecDeque::new())),
            stderr_task: None,
            run: None,
            session_id: None,
            next_turn: 0,
            started: false,
        }
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            streaming: false,
            thoughts: true,
            tool_calls: true,
            ..Capabilities::default()
        }
    }

    /// Exec mode reports no thread handle before a run, so the session carries
    /// a local identifier. Resume is not advertised for the same reason.
    fn local_session_id() -> SessionId {
        let sequence = EXEC_SESSION_COUNTER.fetch_add(1, Ordering::Relaxed);
        SessionId::new(format!("exec-{}-{sequence}", std::process::id()))
    }
}

impl Default for ExecHarness {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentHarness for ExecHarness {
    fn events(&mut self) -> &mut mpsc::UnboundedReceiver<HarnessEvent> {
        &mut self.receiver
    }

    async fn start(&mut self, spec: SpawnSpec) -> HarnessResult<Capabilities> {
        if self.started {
            return Err(HarnessError::Protocol(
                "the codex exec session is already started".to_string(),
            ));
        }
        self.program = spec.program;
        self.args = spec.args;
        self.cwd = spec.cwd;
        self.environment = spec.environment;
        self.started = true;
        self.sender
            .send(HarnessEvent::SessionState {
                state: SessionState::Ready,
            })
            .map_err(|_| HarnessError::ChannelClosed)?;
        Ok(self.capabilities())
    }

    async fn open_session(&mut self, resume: Option<SessionId>) -> HarnessResult<SessionId> {
        if !self.started {
            return Err(HarnessError::NotStarted);
        }
        if let Some(session) = resume {
            return Err(HarnessError::Protocol(format!(
                "the codex exec tier cannot resume thread {}; this build has no app-server",
                session.as_str()
            )));
        }
        let session_id = Self::local_session_id();
        self.session_id = Some(session_id.clone());
        Ok(session_id)
    }

    async fn prompt(&mut self, input: PromptInput) -> HarnessResult<()> {
        if !self.started {
            return Err(HarnessError::NotStarted);
        }
        if self.session_id.is_none() {
            return Err(HarnessError::SessionMissing);
        }
        if self
            .run
            .as_ref()
            .is_some_and(|run| !run.finished.load(Ordering::Relaxed))
        {
            return Err(HarnessError::Protocol(
                "a codex exec turn is still running".to_string(),
            ));
        }
        self.run = None;

        let mut command = Command::new(&self.program);
        command
            .arg("exec")
            .arg("--json")
            .args(&self.args)
            .arg(&input.text)
            .current_dir(&self.cwd)
            .envs(&self.environment)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let mut child = command
            .spawn()
            .map_err(|source| spawn_error(&self.program, source))?;
        let Some(stdout) = child.stdout.take() else {
            return Err(HarnessError::Protocol(
                "codex exec stdout was not captured".to_string(),
            ));
        };
        let Some(stderr) = child.stderr.take() else {
            return Err(HarnessError::Protocol(
                "codex exec stderr was not captured".to_string(),
            ));
        };

        self.stderr = Arc::new(Mutex::new(VecDeque::new()));
        self.stderr_task = Some(spawn_stderr_reader(stderr, Arc::clone(&self.stderr)));

        let turn_id = TurnId::new(format!("exec-turn-{}", self.next_turn));
        self.next_turn += 1;
        let (kill, kill_receiver) = oneshot::channel();
        let finished = Arc::new(AtomicBool::new(false));
        let waiter = spawn_exec_waiter(
            child,
            kill_receiver,
            self.sender.clone(),
            Arc::clone(&self.stderr),
            Arc::clone(&finished),
        );
        let reader = spawn_exec_reader(stdout, self.sender.clone());
        self.run = Some(ExecRun {
            waiter,
            reader,
            kill: Some(kill),
            finished,
        });

        self.sender
            .send(HarnessEvent::SessionState {
                state: SessionState::Working,
            })
            .map_err(|_| HarnessError::ChannelClosed)?;
        self.sender
            .send(HarnessEvent::TurnStarted { turn_id })
            .map_err(|_| HarnessError::ChannelClosed)?;
        Ok(())
    }

    async fn cancel(&mut self) -> HarnessResult<()> {
        let Some(run) = self.run.as_mut() else {
            return Err(HarnessError::Protocol(
                "there is no codex exec turn to cancel".to_string(),
            ));
        };
        if run.finished.load(Ordering::Relaxed) {
            return Err(HarnessError::Protocol(
                "the codex exec turn already finished".to_string(),
            ));
        }
        let Some(kill) = run.kill.take() else {
            return Err(HarnessError::Protocol(
                "the codex exec turn is already stopping".to_string(),
            ));
        };
        kill.send(()).map_err(|_| {
            HarnessError::Protocol("the codex exec process already exited".to_string())
        })?;
        Ok(())
    }

    async fn respond(
        &mut self,
        _approval_id: ApprovalId,
        _decision: Decision,
    ) -> HarnessResult<()> {
        Err(HarnessError::Protocol(
            "the codex exec tier cannot answer approval requests".to_string(),
        ))
    }

    async fn shutdown(&mut self) -> HarnessResult<()> {
        if !self.started {
            return Ok(());
        }

        let reported = match self.run.take() {
            Some(mut run) => {
                if !run.finished.load(Ordering::Relaxed)
                    && let Some(kill) = run.kill.take()
                    && kill.send(()).is_err()
                {
                    tracing::debug!("the codex exec process was already gone");
                }
                let joined = timeout(Duration::from_secs(5), &mut run.waiter)
                    .await
                    .is_ok();
                if !joined {
                    tracing::warn!("the codex exec process did not stop within 5s");
                    run.waiter.abort();
                }
                run.reader.abort();
                joined
            }
            None => false,
        };

        if let Some(task) = self.stderr_task.take() {
            task.abort();
        }
        self.started = false;
        self.session_id = None;

        if !reported {
            self.sender
                .send(HarnessEvent::Exited {
                    code: None,
                    stderr: stderr_tail(&self.stderr),
                })
                .map_err(|_| HarnessError::ChannelClosed)?;
        }
        Ok(())
    }
}

fn spawn_exec_waiter(
    mut child: Child,
    kill: oneshot::Receiver<()>,
    sender: mpsc::UnboundedSender<HarnessEvent>,
    stderr: Arc<Mutex<VecDeque<String>>>,
    finished: Arc<AtomicBool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let outcome = tokio::select! {
            status = child.wait() => Some(status),
            _ = kill => None,
        };
        let cancelled = outcome.is_none();
        let status = match outcome {
            Some(status) => status,
            None => {
                if let Err(error) = child.kill().await {
                    tracing::debug!(%error, "could not kill the codex exec process");
                }
                child.wait().await
            }
        };

        finished.store(true, Ordering::Relaxed);
        let code = match &status {
            Ok(status) => status.code(),
            Err(error) => {
                tracing::debug!(%error, "waiting for the codex exec process failed");
                None
            }
        };
        let reason = if cancelled {
            TurnFinishReason::Cancelled
        } else if matches!(&status, Ok(status) if status.success()) {
            TurnFinishReason::Completed
        } else {
            TurnFinishReason::Failed
        };
        if sender.send(HarnessEvent::TurnFinished { reason }).is_err() {
            tracing::debug!("the core stopped listening to codex events");
        }
        if sender
            .send(HarnessEvent::Exited {
                code,
                stderr: stderr_tail(&stderr),
            })
            .is_err()
        {
            tracing::debug!("the core stopped listening to codex events");
        }
    })
}

fn spawn_exec_reader(
    stdout: ChildStdout,
    sender: mpsc::UnboundedSender<HarnessEvent>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        loop {
            let line = match lines.next_line().await {
                Ok(Some(line)) => line,
                Ok(None) => break,
                Err(error) => {
                    tracing::debug!(%error, "reading codex exec stdout failed");
                    break;
                }
            };
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let message = match serde_json::from_str::<Value>(trimmed) {
                Ok(message) => message,
                Err(error) => {
                    tracing::warn!(%error, "codex exec wrote a line that is not JSON");
                    continue;
                }
            };
            for event in exec_events(&message) {
                if sender.send(event).is_err() {
                    tracing::debug!("the core stopped listening to codex events");
                    return;
                }
            }
        }
    })
}

/// Map one `codex exec --json` line to events. Exec prints a config summary and
/// a prompt echo before the stream, so a line without a `msg` object is not an
/// event.
fn exec_events(message: &Value) -> Vec<HarnessEvent> {
    let Some(event) = message.get("msg").filter(|value| value.is_object()) else {
        return Vec::new();
    };
    let Some(kind) = event.get("type").and_then(Value::as_str) else {
        return Vec::new();
    };
    let event_id = message
        .get("id")
        .map(id_label)
        .unwrap_or_else(|| "event".to_string());

    match kind {
        "agent_message" => exec_delta(event, &event_id, "message", DeltaKind::Message)
            .into_iter()
            .collect(),
        "agent_reasoning" => exec_delta(event, &event_id, "reasoning", DeltaKind::Thought)
            .into_iter()
            .collect(),
        "exec_command_begin" | "exec_command_end" => {
            exec_command_event(event, kind).into_iter().collect()
        }
        "patch_apply_begin" | "patch_apply_end" => {
            exec_patch_event(event, kind).into_iter().collect()
        }
        "mcp_tool_call_begin" | "mcp_tool_call_end" => {
            exec_mcp_event(event, kind).into_iter().collect()
        }
        "token_count" => exec_usage(event).into_iter().collect(),
        "plan_update" => exec_plan(event).into_iter().collect(),
        "error" | "stream_error" => {
            let text = event
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("codex exec reported an error");
            tracing::warn!(kind, "{text}");
            Vec::new()
        }
        _ => Vec::new(),
    }
}

fn exec_delta(
    event: &Value,
    event_id: &str,
    prefix: &str,
    kind: DeltaKind,
) -> Option<HarnessEvent> {
    let field = match kind {
        DeltaKind::Message => "message",
        DeltaKind::Thought => "text",
    };
    let text = event.get(field).and_then(Value::as_str)?;
    Some(HarnessEvent::Delta {
        item_id: ItemId::new(format!("{prefix}-{event_id}")),
        kind,
        text: text.to_string(),
    })
}

fn exec_command_event(event: &Value, kind: &str) -> Option<HarnessEvent> {
    let tool_call_id = event.get("call_id").and_then(Value::as_str)?;
    let title = exec_command_title(event);
    let (status, output) = if kind == "exec_command_end" {
        let failed = event
            .get("exit_code")
            .and_then(Value::as_i64)
            .is_some_and(|code| code != 0);
        let status = if failed {
            ToolStatus::Failed
        } else {
            ToolStatus::Completed
        };
        (status, exec_command_output(event))
    } else {
        (ToolStatus::Running, None)
    };
    Some(exec_tool_event(
        tool_call_id,
        title,
        "command",
        status,
        output,
    ))
}

fn exec_command_title(event: &Value) -> String {
    match event.get("command") {
        Some(Value::Array(parts)) => {
            let joined = parts
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" ");
            if joined.is_empty() {
                "Run a command".to_string()
            } else {
                joined
            }
        }
        Some(Value::String(command)) => command.clone(),
        _ => "Run a command".to_string(),
    }
}

fn exec_command_output(event: &Value) -> Option<String> {
    event
        .get("aggregated_output")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .or_else(|| {
            event
                .get("stdout")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
        })
        .or_else(|| {
            event
                .get("stderr")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
        })
        .map(str::to_string)
}

fn exec_patch_event(event: &Value, kind: &str) -> Option<HarnessEvent> {
    let tool_call_id = event.get("call_id").and_then(Value::as_str)?;
    let (status, output) = if kind == "patch_apply_end" {
        let succeeded = event
            .get("success")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let status = if succeeded {
            ToolStatus::Completed
        } else {
            ToolStatus::Failed
        };
        (status, exec_command_output(event))
    } else {
        (ToolStatus::Running, None)
    };
    Some(exec_tool_event(
        tool_call_id,
        "Apply file changes".to_string(),
        "edit",
        status,
        output,
    ))
}

fn exec_mcp_event(event: &Value, kind: &str) -> Option<HarnessEvent> {
    let tool_call_id = event.get("call_id").and_then(Value::as_str)?;
    let title = event
        .get("invocation")
        .and_then(|invocation| invocation.get("tool"))
        .and_then(Value::as_str)
        .unwrap_or("Call a tool")
        .to_string();
    let (status, output) = if kind == "mcp_tool_call_end" {
        (
            ToolStatus::Completed,
            event.get("result").map(Value::to_string),
        )
    } else {
        (ToolStatus::Running, None)
    };
    Some(exec_tool_event(tool_call_id, title, "tool", status, output))
}

fn exec_usage(event: &Value) -> Option<HarnessEvent> {
    let info = event.get("info")?;
    let used = info
        .get("total_token_usage")
        .and_then(|usage| usage.get("total_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let size = info
        .get("model_context_window")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    Some(HarnessEvent::Usage {
        used,
        size,
        cost: None,
    })
}

fn exec_plan(event: &Value) -> Option<HarnessEvent> {
    let rows = event
        .get("plan")
        .and_then(Value::as_array)?
        .iter()
        .filter_map(|row| {
            let text = row.get("step").and_then(Value::as_str)?;
            let status = row
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("pending");
            Some(PlanRow {
                text: text.to_string(),
                status: status.to_string(),
            })
        })
        .collect::<Vec<_>>();
    if rows.is_empty() {
        None
    } else {
        Some(HarnessEvent::Plan { rows })
    }
}

fn exec_tool_event(
    tool_call_id: &str,
    title: String,
    kind: &str,
    status: ToolStatus,
    output: Option<String>,
) -> HarnessEvent {
    HarnessEvent::ToolCall {
        tool_call_id: ToolCallId::new(tool_call_id),
        title,
        kind: kind.to_string(),
        status,
        output,
        diffs: Vec::new(),
        terminal_id: None,
    }
}

fn spawn_writer(
    mut stdin: ChildStdin,
    mut outgoing: mpsc::UnboundedReceiver<Value>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(message) = outgoing.recv().await {
            let mut line = match serde_json::to_string(&message) {
                Ok(line) => line,
                Err(error) => {
                    tracing::error!(%error, "could not encode a codex message");
                    continue;
                }
            };
            line.push('\n');
            if let Err(error) = stdin.write_all(line.as_bytes()).await {
                tracing::debug!(%error, "codex stopped accepting input");
                return;
            }
            if let Err(error) = stdin.flush().await {
                tracing::debug!(%error, "could not flush codex input");
                return;
            }
        }
    })
}

fn spawn_reader(
    stdout: ChildStdout,
    sender: mpsc::UnboundedSender<HarnessEvent>,
    pending: Arc<AsyncMutex<Pending>>,
    outgoing: mpsc::UnboundedSender<Value>,
    stderr: Arc<Mutex<VecDeque<String>>>,
    closed: Arc<AtomicBool>,
    closing: Arc<AtomicBool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        loop {
            let line = match lines.next_line().await {
                Ok(Some(line)) => line,
                Ok(None) => break,
                Err(error) => {
                    tracing::debug!(%error, "reading codex stdout failed");
                    break;
                }
            };
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let message = match serde_json::from_str::<Value>(trimmed) {
                Ok(message) => message,
                Err(error) => {
                    tracing::warn!(%error, "codex sent a line that is not JSON");
                    continue;
                }
            };
            route_incoming(message, &sender, &pending, &outgoing).await;
        }

        closed.store(true, Ordering::Relaxed);
        if closing.load(Ordering::Relaxed) {
            return;
        }
        if sender
            .send(HarnessEvent::Exited {
                code: None,
                stderr: stderr_tail(&stderr),
            })
            .is_err()
        {
            tracing::debug!("the core stopped listening to codex events");
        }
    })
}

fn spawn_stderr_reader(stderr: ChildStderr, tail: Arc<Mutex<VecDeque<String>>>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::debug!(stream = "codex-stderr", "{line}");
            match tail.lock() {
                Ok(mut tail) => {
                    if tail.len() == STDERR_TAIL_LINES {
                        tail.pop_front();
                    }
                    tail.push_back(line);
                }
                Err(poisoned) => {
                    let mut tail = poisoned.into_inner();
                    if tail.len() == STDERR_TAIL_LINES {
                        tail.pop_front();
                    }
                    tail.push_back(line);
                }
            }
        }
    })
}

fn stderr_tail(tail: &Arc<Mutex<VecDeque<String>>>) -> String {
    match tail.lock() {
        Ok(tail) => tail.iter().cloned().collect::<Vec<_>>().join("\n"),
        Err(poisoned) => poisoned
            .into_inner()
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

async fn route_incoming(
    message: Value,
    sender: &mpsc::UnboundedSender<HarnessEvent>,
    pending: &Arc<AsyncMutex<Pending>>,
    outgoing: &mpsc::UnboundedSender<Value>,
) {
    match (message.get("id"), message.get("method")) {
        (Some(id), Some(method)) => {
            let method = method.as_str().unwrap_or_default().to_string();
            let params = message.get("params").cloned().unwrap_or(Value::Null);
            handle_server_request(id.clone(), method, params, sender, pending, outgoing).await;
        }
        (Some(id), None) => {
            let Some(request_id) = id.as_i64() else {
                tracing::warn!("codex answered with a non-numeric id");
                return;
            };
            let waiter = pending.lock().await.responses.remove(&request_id);
            let Some(waiter) = waiter else {
                tracing::debug!(request_id, "codex answered a request nobody is waiting for");
                return;
            };
            let outcome = match message.get("error") {
                Some(error) => Err(error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("the codex server returned an error")
                    .to_string()),
                None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
            };
            if waiter.send(outcome).is_err() {
                tracing::debug!(request_id, "the answer arrived after its caller gave up");
            }
        }
        (None, Some(method)) => {
            let method = method.as_str().unwrap_or_default().to_string();
            let params = message.get("params").cloned().unwrap_or(Value::Null);
            handle_notification(&method, &params, sender, pending).await;
        }
        (None, None) => tracing::warn!("codex sent a message with neither id nor method"),
    }
}

async fn handle_server_request(
    id: Value,
    method: String,
    params: Value,
    sender: &mpsc::UnboundedSender<HarnessEvent>,
    pending: &Arc<AsyncMutex<Pending>>,
    outgoing: &mpsc::UnboundedSender<Value>,
) {
    let wants_approval = matches!(
        method.as_str(),
        "item/commandExecution/requestApproval"
            | "item/fileChange/requestApproval"
            | "item/permissions/requestApproval"
    );
    if !wants_approval {
        let response = json!({
            "id": id,
            "error": {
                "code": METHOD_NOT_FOUND,
                "message": format!("shepherd does not support `{method}`"),
            },
        });
        if outgoing.send(response).is_err() {
            tracing::debug!("codex request could not be answered");
        }
        return;
    }

    let Some(server_id) = id.as_i64() else {
        tracing::warn!(%method, "an approval arrived with a non-numeric id");
        return;
    };
    let approval_id = params
        .get("itemId")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| format!("approval-{}", id_label(&id)));
    let tool_call_id = params
        .get("itemId")
        .and_then(Value::as_str)
        .unwrap_or(approval_id.as_str())
        .to_string();

    let request = ApprovalRequest {
        approval_id: ApprovalId::new(approval_id.clone()),
        tool_call_id: ToolCallId::new(tool_call_id),
        title: approval_title(&method, &params),
        arguments: params.clone(),
        paths: approval_paths(&params),
        patch: None,
    };
    pending
        .lock()
        .await
        .approvals
        .insert(approval_id, server_id);
    if sender
        .send(HarnessEvent::ApprovalRequested { request })
        .is_err()
    {
        tracing::debug!("the core stopped listening to codex events");
    }
}

async fn handle_notification(
    method: &str,
    params: &Value,
    sender: &mpsc::UnboundedSender<HarnessEvent>,
    pending: &Arc<AsyncMutex<Pending>>,
) {
    let mut events = Vec::new();

    match method {
        "thread/started" => {
            if let Some(thread_id) = read_thread_id(params) {
                pending.lock().await.thread_id = Some(thread_id);
            }
        }
        "turn/started" => {
            if let Some(turn_id) = read_turn_id(params) {
                pending.lock().await.turn_id = Some(turn_id);
            }
        }
        "item/agentMessage/delta" => {
            if let Some(delta) = params.get("delta").and_then(Value::as_str) {
                events.push(HarnessEvent::Delta {
                    item_id: ItemId::new(item_id(params)),
                    kind: DeltaKind::Message,
                    text: delta.to_string(),
                });
            }
        }
        "item/reasoning/delta" | "item/agentThought/delta" => {
            if let Some(delta) = params.get("delta").and_then(Value::as_str) {
                events.push(HarnessEvent::Delta {
                    item_id: ItemId::new(item_id(params)),
                    kind: DeltaKind::Thought,
                    text: delta.to_string(),
                });
            }
        }
        "item/started" | "item/completed" => {
            if let Some(item) = params.get("item")
                && let Some(event) = tool_call_from_item(item)
            {
                events.push(event);
            }
        }
        "turn/completed" => {
            let reason = turn_finish_reason(params);
            pending.lock().await.turn_id = None;
            events.push(HarnessEvent::TurnFinished { reason });
        }
        "turn/failed" => {
            pending.lock().await.turn_id = None;
            events.push(HarnessEvent::TurnFinished {
                reason: TurnFinishReason::Failed,
            });
        }
        "turn/cancelled" => {
            pending.lock().await.turn_id = None;
            events.push(HarnessEvent::TurnFinished {
                reason: TurnFinishReason::Cancelled,
            });
        }
        _ => {}
    }

    for event in events {
        if sender.send(event).is_err() {
            tracing::debug!("the core stopped listening to codex events");
            return;
        }
    }
}

fn tool_call_from_item(item: &Value) -> Option<HarnessEvent> {
    let tool_call_id = item.get("id").and_then(Value::as_str)?.to_string();
    let kind = item.get("type").and_then(Value::as_str)?;
    let status = tool_status(item.get("status").and_then(Value::as_str));

    let (title, kind_name, output, diffs) = match kind {
        "commandExecution" => (
            item.get("command")
                .and_then(Value::as_str)
                .unwrap_or("Run a command")
                .to_string(),
            "command".to_string(),
            item.get("aggregatedOutput")
                .and_then(Value::as_str)
                .map(str::to_string),
            Vec::new(),
        ),
        "fileChange" => (
            "Edit files".to_string(),
            "edit".to_string(),
            None,
            file_diffs(item),
        ),
        "mcpToolCall" => (
            item.get("tool")
                .and_then(Value::as_str)
                .unwrap_or("Call a tool")
                .to_string(),
            "tool".to_string(),
            item.get("result").map(Value::to_string),
            Vec::new(),
        ),
        "webSearch" => (
            "Search the web".to_string(),
            "search".to_string(),
            None,
            Vec::new(),
        ),
        _ => return None,
    };

    Some(HarnessEvent::ToolCall {
        tool_call_id: ToolCallId::new(tool_call_id),
        title,
        kind: kind_name,
        status,
        output,
        diffs,
        terminal_id: None,
    })
}

fn file_diffs(item: &Value) -> Vec<FileDiff> {
    let Some(changes) = item.get("changes").and_then(Value::as_array) else {
        return Vec::new();
    };
    changes
        .iter()
        .filter_map(|change| {
            let path = change.get("path").and_then(Value::as_str)?;
            let diff = change
                .get("diff")
                .and_then(Value::as_str)
                .unwrap_or_default();
            Some(FileDiff {
                path: path.to_string(),
                old_text: None,
                new_text: diff.to_string(),
            })
        })
        .collect()
}

fn tool_status(status: Option<&str>) -> ToolStatus {
    match status {
        Some("inProgress") => ToolStatus::Running,
        Some("completed") => ToolStatus::Completed,
        Some("failed") | Some("declined") => ToolStatus::Failed,
        _ => ToolStatus::Pending,
    }
}

fn turn_finish_reason(params: &Value) -> TurnFinishReason {
    match params
        .get("turn")
        .and_then(|turn| turn.get("status"))
        .and_then(Value::as_str)
    {
        Some("interrupted") => TurnFinishReason::Cancelled,
        Some("failed") => TurnFinishReason::Failed,
        _ => TurnFinishReason::Completed,
    }
}

fn approval_title(method: &str, params: &Value) -> String {
    match method {
        "item/commandExecution/requestApproval" => params
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or("Run a command")
            .to_string(),
        "item/fileChange/requestApproval" => "Apply file changes".to_string(),
        _ => "Grant permissions".to_string(),
    }
}

fn approval_paths(params: &Value) -> Vec<PathBuf> {
    params
        .get("paths")
        .and_then(Value::as_array)
        .map(|paths| {
            paths
                .iter()
                .filter_map(Value::as_str)
                .map(PathBuf::from)
                .collect()
        })
        .unwrap_or_default()
}

fn item_id(params: &Value) -> String {
    params
        .get("itemId")
        .and_then(Value::as_str)
        .unwrap_or("item")
        .to_string()
}

fn read_thread_id(value: &Value) -> Option<String> {
    value
        .get("thread")
        .and_then(|thread| thread.get("id"))
        .and_then(Value::as_str)
        .or_else(|| value.get("threadId").and_then(Value::as_str))
        .map(str::to_string)
}

fn read_turn_id(value: &Value) -> Option<String> {
    value
        .get("turn")
        .and_then(|turn| turn.get("id"))
        .and_then(Value::as_str)
        .or_else(|| value.get("turnId").and_then(Value::as_str))
        .map(str::to_string)
}

fn id_label(id: &Value) -> String {
    match id {
        Value::String(label) => label.clone(),
        other => other.to_string(),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use tempfile::TempDir;

    use super::*;

    /// Answers the handshake, starts a thread, streams a delta and a tool item,
    /// completes the turn, and records a resume so the test can see the method.
    const PROTOCOL_STUB: &str = r#"#!/usr/bin/env bash
if [ "$1" = "--version" ]; then echo "codex-cli 9.9.9"; exit 0; fi
directory="$(cd "$(dirname "$0")" && pwd)"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '{"id":%s,"result":{"capabilities":{"methods":["thread/start","thread/resume","turn/start","turn/interrupt","item/commandExecution/requestApproval"]}}}\n' "$id"
      ;;
    *'"method":"thread/start"'*)
      printf '{"id":%s,"result":{"thread":{"id":"thr_1"}}}\n' "$id"
      ;;
    *'"method":"thread/resume"'*)
      printf 'thread/resume\n' >> "$directory/methods.txt"
      printf '{"id":%s,"result":{"thread":{"id":"thr_resumed"}}}\n' "$id"
      ;;
    *'"method":"turn/start"'*)
      printf '{"id":%s,"result":{"turn":{"id":"turn_1","status":"inProgress"}}}\n' "$id"
      printf '{"method":"turn/started","params":{"turn":{"id":"turn_1"}}}\n'
      printf '{"method":"item/agentMessage/delta","params":{"itemId":"item_1","delta":"working"}}\n'
      printf '{"method":"item/completed","params":{"item":{"id":"item_2","type":"commandExecution","command":"cargo test","status":"completed","aggregatedOutput":"ok"}}}\n'
      printf '{"method":"turn/completed","params":{"turn":{"id":"turn_1","status":"completed"}}}\n'
      ;;
    *) : ;;
  esac
done
"#;

    /// Streams nothing and waits for an interrupt.
    const INTERRUPT_STUB: &str = r#"#!/usr/bin/env bash
if [ "$1" = "--version" ]; then echo "codex-cli 9.9.9"; exit 0; fi
directory="$(cd "$(dirname "$0")" && pwd)"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '{"id":%s,"result":{}}\n' "$id"
      ;;
    *'"method":"thread/start"'*)
      printf '{"id":%s,"result":{"thread":{"id":"thr_waiting"}}}\n' "$id"
      ;;
    *'"method":"turn/start"'*)
      printf '{"id":%s,"result":{"turn":{"id":"turn_waiting","status":"inProgress"}}}\n' "$id"
      printf '{"method":"item/agentMessage/delta","params":{"itemId":"item_1","delta":"waiting"}}\n'
      ;;
    *'"method":"turn/interrupt"'*)
      printf '%s\n' "$line" >> "$directory/interrupt.txt"
      printf '{"id":%s,"result":{}}\n' "$id"
      printf '{"method":"turn/completed","params":{"turn":{"id":"turn_waiting","status":"interrupted"}}}\n'
      ;;
    *) : ;;
  esac
done
"#;

    /// Asks for approval of a command and finishes once the decision arrives.
    const APPROVAL_STUB: &str = r#"#!/usr/bin/env bash
if [ "$1" = "--version" ]; then echo "codex-cli 9.9.9"; exit 0; fi
directory="$(cd "$(dirname "$0")" && pwd)"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '{"id":%s,"result":{}}\n' "$id"
      ;;
    *'"method":"thread/start"'*)
      printf '{"id":%s,"result":{"thread":{"id":"thr_approval"}}}\n' "$id"
      ;;
    *'"method":"turn/start"'*)
      printf '{"id":%s,"result":{"turn":{"id":"turn_approval","status":"inProgress"}}}\n' "$id"
      printf '{"method":"item/agentMessage/delta","params":{"itemId":"item_1","delta":"about to run"}}\n'
      printf '{"id":42,"method":"item/commandExecution/requestApproval","params":{"itemId":"item_2","threadId":"thr_approval","turnId":"turn_approval","command":"cargo test"}}\n'
      ;;
    *'"id":42'*)
      printf '%s\n' "$line" >> "$directory/decision.txt"
      printf '{"method":"item/agentMessage/delta","params":{"itemId":"item_1","delta":" ran"}}\n'
      printf '{"method":"turn/completed","params":{"turn":{"id":"turn_approval","status":"completed"}}}\n'
      ;;
    *) : ;;
  esac
done
"#;

    /// Answers initialize with "method not found".
    const UNSUPPORTED_STUB: &str = r#"#!/usr/bin/env bash
if [ "$1" = "--version" ]; then echo "codex-cli 0.9.0"; exit 0; fi
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '{"id":%s,"error":{"code":-32601,"message":"unknown method"}}\n' "$id"
      ;;
    *) : ;;
  esac
done
"#;

    fn write_stub(directory: &TempDir, name: &str, contents: &str) -> String {
        let path = directory.path().join(name);
        std::fs::write(&path, contents).expect("the stub is writable");
        let permissions = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(&path, permissions).expect("the stub is executable");
        path.to_string_lossy().into_owned()
    }

    fn spec(program: &str, cwd: &Path) -> SpawnSpec {
        SpawnSpec {
            program: program.to_string(),
            cwd: cwd.to_path_buf(),
            ..SpawnSpec::default()
        }
    }

    async fn start_session(stub: &str, cwd: &Path) -> (CodexHarness, Capabilities) {
        let mut harness = CodexHarness::new();
        let capabilities = harness
            .start(spec(stub, cwd))
            .await
            .expect("the stub app-server starts");
        harness.open_session(None).await.expect("a thread opens");
        (harness, capabilities)
    }

    async fn collect_until<H, F>(harness: &mut H, reached: F) -> Vec<HarnessEvent>
    where
        H: AgentHarness,
        F: Fn(&[HarnessEvent]) -> bool,
    {
        let mut events = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !reached(&events) {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            match timeout(remaining, harness.events().recv()).await {
                Ok(Some(event)) => events.push(event),
                Ok(None) => break,
                Err(_) => break,
            }
        }
        events
    }

    #[tokio::test]
    async fn the_probe_reports_only_the_methods_the_server_advertises() {
        let directory = TempDir::new().expect("temp dir");
        let stub = write_stub(&directory, "codex.sh", PROTOCOL_STUB);

        let probe = probe(&stub, directory.path())
            .await
            .expect("the app-server probe succeeds");

        assert_eq!(probe.tier, CodexTier::AppServer);
        assert_eq!(probe.version.as_deref(), Some("codex-cli 9.9.9"));
        assert!(probe.supports("thread/start"));
        assert!(probe.supports("thread/resume"));
        assert!(
            !probe.supports("item/fileChange/requestApproval"),
            "the server did not advertise file-change approvals: {:?}",
            probe.methods
        );
        assert!(!probe.supports("thread/archive"));
    }

    #[tokio::test]
    async fn a_build_without_app_server_offers_the_exec_tier() {
        let directory = TempDir::new().expect("temp dir");
        let stub = write_stub(&directory, "codex-old.sh", UNSUPPORTED_STUB);

        let probe = probe(&stub, directory.path())
            .await
            .expect("an unsupported app-server is a tier, not a crash");

        assert_eq!(probe.tier, CodexTier::Exec);
        assert!(probe.methods.is_empty());
        assert!(!probe.supports("turn/start"));
    }

    #[tokio::test]
    async fn a_missing_binary_names_the_program() {
        let directory = TempDir::new().expect("temp dir");
        let error = probe("/nonexistent/codex-for-shepherd", directory.path())
            .await
            .expect_err("a missing binary is an error");
        assert!(
            matches!(error, HarnessError::MissingProgram { ref program } if program.contains("codex-for-shepherd"))
        );
    }

    #[tokio::test]
    async fn a_turn_maps_messages_and_tool_items_into_events() {
        let directory = TempDir::new().expect("temp dir");
        let stub = write_stub(&directory, "codex.sh", PROTOCOL_STUB);
        let (mut harness, capabilities) = start_session(&stub, directory.path()).await;
        assert!(capabilities.streaming);
        assert!(capabilities.resume);
        assert!(capabilities.approvals);

        harness
            .prompt(PromptInput {
                text: "run the tests".to_string(),
            })
            .await
            .expect("the turn starts");
        let events = collect_until(&mut harness, |events| {
            events
                .iter()
                .any(|event| matches!(event, HarnessEvent::TurnFinished { .. }))
        })
        .await;

        assert!(events.iter().any(|event| matches!(
            event,
            HarnessEvent::Delta { text, kind: DeltaKind::Message, .. } if text == "working"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            HarnessEvent::ToolCall { title, status: ToolStatus::Completed, .. } if title == "cargo test"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            HarnessEvent::TurnFinished {
                reason: TurnFinishReason::Completed
            }
        )));
        harness.shutdown().await.expect("the session stops");
    }

    #[tokio::test]
    async fn an_approval_is_answered_with_the_users_decision() {
        let directory = TempDir::new().expect("temp dir");
        let stub = write_stub(&directory, "codex-approval.sh", APPROVAL_STUB);
        let (mut harness, _capabilities) = start_session(&stub, directory.path()).await;

        harness
            .prompt(PromptInput {
                text: "run the tests".to_string(),
            })
            .await
            .expect("the turn starts");
        let events = collect_until(&mut harness, |events| {
            events
                .iter()
                .any(|event| matches!(event, HarnessEvent::ApprovalRequested { .. }))
        })
        .await;
        let request = events
            .iter()
            .find_map(|event| match event {
                HarnessEvent::ApprovalRequested { request } => Some(request),
                _ => None,
            })
            .expect("the approval request arrives");
        assert_eq!(request.approval_id.as_str(), "item_2");
        assert_eq!(request.title, "cargo test");

        harness
            .respond(ApprovalId::new("item_2"), Decision::Allow)
            .await
            .expect("the decision is sent");
        let events = collect_until(&mut harness, |events| {
            events
                .iter()
                .any(|event| matches!(event, HarnessEvent::TurnFinished { .. }))
        })
        .await;
        assert!(events.iter().any(|event| matches!(
            event,
            HarnessEvent::ApprovalResolved {
                decision: Decision::Allow,
                ..
            }
        )));

        let decision = std::fs::read_to_string(directory.path().join("decision.txt"))
            .expect("the stub recorded the decision");
        assert!(
            decision.contains("\"accept\""),
            "unexpected decision payload: {decision}"
        );
        harness.shutdown().await.expect("the session stops");
    }

    #[tokio::test]
    async fn cancelling_interrupts_the_active_turn() {
        let directory = TempDir::new().expect("temp dir");
        let stub = write_stub(&directory, "codex-interrupt.sh", INTERRUPT_STUB);
        let (mut harness, _capabilities) = start_session(&stub, directory.path()).await;

        harness
            .prompt(PromptInput {
                text: "keep going".to_string(),
            })
            .await
            .expect("the turn starts");
        collect_until(&mut harness, |events| {
            events
                .iter()
                .any(|event| matches!(event, HarnessEvent::Delta { .. }))
        })
        .await;

        harness.cancel().await.expect("the turn is interrupted");
        let events = collect_until(&mut harness, |events| {
            events
                .iter()
                .any(|event| matches!(event, HarnessEvent::TurnFinished { .. }))
        })
        .await;
        assert!(matches!(
            events
                .iter()
                .find(|event| matches!(event, HarnessEvent::TurnFinished { .. })),
            Some(HarnessEvent::TurnFinished {
                reason: TurnFinishReason::Cancelled
            })
        ));

        let interrupt = std::fs::read_to_string(directory.path().join("interrupt.txt"))
            .expect("the stub recorded the interrupt");
        assert!(interrupt.contains("turn/interrupt"));
        harness.shutdown().await.expect("the session stops");
    }

    #[tokio::test]
    async fn resuming_reopens_the_stored_thread() {
        let directory = TempDir::new().expect("temp dir");
        let stub = write_stub(&directory, "codex.sh", PROTOCOL_STUB);
        let mut harness = CodexHarness::new();
        harness
            .start(spec(&stub, directory.path()))
            .await
            .expect("the stub app-server starts");

        let session = harness
            .open_session(Some(SessionId::new("thr_stored")))
            .await
            .expect("the thread resumes");
        assert_eq!(session.as_str(), "thr_resumed");

        let methods = std::fs::read_to_string(directory.path().join("methods.txt"))
            .expect("the stub recorded the resume");
        assert!(methods.contains("thread/resume"));
        harness.shutdown().await.expect("the session stops");
    }

    #[tokio::test]
    async fn resume_is_refused_when_the_build_does_not_advertise_it() {
        let directory = TempDir::new().expect("temp dir");
        let stub = write_stub(&directory, "codex-narrow.sh", NARROW_STUB);
        let mut harness = CodexHarness::new();
        let capabilities = harness
            .start(spec(&stub, directory.path()))
            .await
            .expect("the stub app-server starts");
        assert!(!capabilities.resume);

        let error = harness
            .open_session(Some(SessionId::new("thr_stored")))
            .await
            .expect_err("resume must not be attempted");
        assert!(
            matches!(error, HarnessError::Protocol(message) if message.contains("thread/resume"))
        );
        harness.shutdown().await.expect("the session stops");
    }

    /// Advertises only the methods needed to start a fresh thread.
    const NARROW_STUB: &str = r#"#!/usr/bin/env bash
if [ "$1" = "--version" ]; then echo "codex-cli 9.9.9"; exit 0; fi
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '{"id":%s,"result":{"capabilities":{"methods":["thread/start","turn/start"]}}}\n' "$id"
      ;;
    *'"method":"thread/start"'*)
      printf '{"id":%s,"result":{"thread":{"id":"thr_narrow"}}}\n' "$id"
      ;;
    *) : ;;
  esac
done
"#;

    /// Prints the exec-mode preamble and a short event stream, then exits 0.
    const EXEC_STUB: &str = r#"#!/usr/bin/env bash
if [ "$1" = "--version" ]; then echo "codex-cli 0.9.0"; exit 0; fi
directory="$(cd "$(dirname "$0")" && pwd)"
printf '%s\n' "$@" > "$directory/args.txt"
printf '{"model":"gpt-5"}\n'
printf '{"prompt":"run the tests"}\n'
printf '{"id":"0","msg":{"type":"session_configured","session_id":"sess_1"}}\n'
printf '{"id":"1","msg":{"type":"task_started"}}\n'
printf '{"id":"2","msg":{"type":"agent_message","message":"running the tests"}}\n'
printf '{"id":"3","msg":{"type":"exec_command_begin","call_id":"call_1","command":["bash","-lc","cargo test"]}}\n'
printf '{"id":"4","msg":{"type":"exec_command_end","call_id":"call_1","command":["bash","-lc","cargo test"],"stdout":"ok","stderr":"","aggregated_output":"ok","exit_code":0}}\n'
printf '{"id":"5","msg":{"type":"token_count","info":{"total_token_usage":{"total_tokens":15},"model_context_window":200000}}}\n'
exit 0
"#;

    /// Stays alive so a cancel has a running process to kill.
    const EXEC_SLOW_STUB: &str = r#"#!/usr/bin/env bash
printf '{"id":"0","msg":{"type":"task_started"}}\n'
sleep 30
"#;

    async fn start_exec(stub: &str, cwd: &Path) -> (ExecHarness, Capabilities) {
        let mut harness = ExecHarness::new();
        let capabilities = harness
            .start(spec(stub, cwd))
            .await
            .expect("the stub exec tier starts");
        harness
            .open_session(None)
            .await
            .expect("the session handle opens");
        (harness, capabilities)
    }

    #[tokio::test]
    async fn the_exec_tier_advertises_only_what_it_can_do() {
        let directory = TempDir::new().expect("temp dir");
        let stub = write_stub(&directory, "codex-exec.sh", EXEC_STUB);
        let (mut harness, capabilities) = start_exec(&stub, directory.path()).await;

        assert!(capabilities.tool_calls);
        assert!(!capabilities.streaming);
        assert!(!capabilities.approvals);
        assert!(!capabilities.resume);

        let error = harness
            .open_session(Some(SessionId::new("sess_1")))
            .await
            .expect_err("exec cannot resume a stored thread");
        assert!(matches!(error, HarnessError::Protocol(message) if message.contains("resume")));

        harness.shutdown().await.expect("the session stops");
    }

    #[tokio::test]
    async fn exec_events_map_to_messages_tools_and_usage() {
        let directory = TempDir::new().expect("temp dir");
        let stub = write_stub(&directory, "codex-exec.sh", EXEC_STUB);
        let (mut harness, _capabilities) = start_exec(&stub, directory.path()).await;

        harness
            .prompt(PromptInput {
                text: "run the tests".to_string(),
            })
            .await
            .expect("the exec turn starts");
        let events = collect_until(&mut harness, |events| {
            events
                .iter()
                .any(|event| matches!(event, HarnessEvent::Exited { .. }))
        })
        .await;

        assert!(events.iter().any(|event| matches!(
            event,
            HarnessEvent::Delta { text, kind: DeltaKind::Message, .. } if text == "running the tests"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            HarnessEvent::ToolCall {
                title,
                status: ToolStatus::Completed,
                output: Some(output),
                ..
            } if title == "bash -lc cargo test" && output == "ok"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            HarnessEvent::Usage {
                used: 15,
                size: 200000,
                ..
            }
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            HarnessEvent::TurnFinished {
                reason: TurnFinishReason::Completed
            }
        )));
        assert!(
            events
                .iter()
                .any(|event| matches!(event, HarnessEvent::Exited { code: Some(0), .. }))
        );

        let arguments = std::fs::read_to_string(directory.path().join("args.txt"))
            .expect("the stub recorded its arguments");
        assert!(arguments.contains("--json"));
        assert!(arguments.contains("run the tests"));
        harness.shutdown().await.expect("the session stops");
    }

    #[tokio::test]
    async fn cancelling_exec_kills_the_run() {
        let directory = TempDir::new().expect("temp dir");
        let stub = write_stub(&directory, "codex-exec-slow.sh", EXEC_SLOW_STUB);
        let (mut harness, _capabilities) = start_exec(&stub, directory.path()).await;

        harness
            .prompt(PromptInput {
                text: "keep going".to_string(),
            })
            .await
            .expect("the exec turn starts");
        harness.cancel().await.expect("the run is killed");
        let events = collect_until(&mut harness, |events| {
            events
                .iter()
                .any(|event| matches!(event, HarnessEvent::TurnFinished { .. }))
        })
        .await;
        assert!(matches!(
            events
                .iter()
                .find(|event| matches!(event, HarnessEvent::TurnFinished { .. })),
            Some(HarnessEvent::TurnFinished {
                reason: TurnFinishReason::Cancelled
            })
        ));
        harness.shutdown().await.expect("the session stops");
    }
}
