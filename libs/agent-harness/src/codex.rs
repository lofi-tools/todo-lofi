//! Codex adapter over `codex app-server`, with a `codex exec --json` fallback
//! for builds that predate the subcommand.
//!
//! The protocol is newline-delimited JSON-RPC 2.0 over stdio with the `jsonrpc`
//! header omitted. `probe` answers which tier this build supports before the
//! core commits to it; the harness translates notifications and server requests
//! into [`HarnessEvent`]s and never writes to the store itself.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::{Mutex as AsyncMutex, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::{
    AgentDiagnostics, AgentHarness, ApprovalId, ApprovalRequest, Capabilities, Decision, DeltaKind,
    FailureKind, FileDiff, HarnessError, HarnessEvent, HarnessResult, ItemId, PlanRow, PromptInput,
    SessionId, SessionState, SpawnSpec, ToolCallId, ToolStatus, TurnFinishReason, TurnId,
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
    pub images: bool,
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
    let images = if handshake.supported {
        true
    } else {
        let output = timeout(
            PROBE_TIMEOUT,
            Command::new(program)
                .args(["exec", "--help"])
                .current_dir(cwd)
                .kill_on_drop(true)
                .output(),
        )
        .await
        .map_err(|_| HarnessError::Protocol("Codex exec help probe timed out".into()))?
        .map_err(|error| spawn_error(program, error))?;
        let help = String::from_utf8_lossy(&output.stdout);
        let has_flag = |name| {
            help.split_whitespace()
                .any(|flag| flag.trim_end_matches(',') == name)
        };
        if !output.status.success() || !has_flag("--json") {
            return Err(HarnessError::Protocol(
                "Codex did not expose app-server or support exec --json".into(),
            ));
        }
        has_flag("--image")
    };
    Ok(CodexProbe {
        tier: if handshake.supported {
            CodexTier::AppServer
        } else {
            CodexTier::Exec
        },
        version,
        methods: handshake.methods,
        images,
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
    let stderr_task = spawn_stderr_reader(stderr, Arc::clone(&stderr_state), None);

    let request = json!({ "method": "initialize", "id": 1, "params": initialize_params(false) });
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
                    Err(codex_rpc_error(error))
                }
            }
            None => Ok(Handshake {
                supported: true,
                methods: supported_methods(message.get("result")),
            }),
        },
    }
}

fn initialize_params(experimental_api: bool) -> Value {
    json!({
        "clientInfo": {
            "name": "shepherd",
            "title": "Shepherd",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "capabilities": if experimental_api { json!({"experimentalApi": true}) } else { json!({}) },
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

/// How a Codex executable was located. The kind is provenance, not capability:
/// the verified tier always comes from the probe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodexInstallKind {
    /// An explicit path from the profile or settings.
    ProfilePath,
    /// A `codex` entry on `PATH`.
    Path,
    /// The macOS app bundle (`com.openai.codex`), in either packaged layout.
    AppBundle,
}

impl CodexInstallKind {
    pub fn label(self) -> &'static str {
        match self {
            CodexInstallKind::ProfilePath => "configured path",
            CodexInstallKind::Path => "PATH",
            CodexInstallKind::AppBundle => "app bundle",
        }
    }
}

/// A location that verified by answering `--version` and the handshake.
#[derive(Clone, Debug)]
pub struct CodexInstall {
    pub program: PathBuf,
    pub kind: CodexInstallKind,
    pub probe: CodexProbe,
}

/// A location that was tried and did not verify, with the reason to show.
#[derive(Clone, Debug)]
pub struct CodexAttempt {
    pub program: PathBuf,
    pub kind: CodexInstallKind,
    pub failure: FailureKind,
    pub reason: String,
}

/// What detection found. `install` is `None` when Codex is not usable here;
/// `attempts` then names every path that was tried and why it failed, so
/// "not installed" is a report rather than a guess.
#[derive(Clone, Debug)]
pub struct CodexDetection {
    pub install: Option<CodexInstall>,
    pub attempts: Vec<CodexAttempt>,
}

impl CodexDetection {
    pub fn failure_kind(&self) -> FailureKind {
        self.attempts
            .iter()
            .find_map(|attempt| {
                (attempt.failure != FailureKind::MissingBinary).then_some(attempt.failure)
            })
            .unwrap_or(FailureKind::MissingBinary)
    }

    /// One line for diagnostics.
    pub fn summary(&self) -> String {
        match &self.install {
            Some(install) => {
                let version = install
                    .probe
                    .version
                    .as_deref()
                    .unwrap_or("version unknown");
                format!(
                    "codex found at {} ({}, {version}, {} tier)",
                    install.program.display(),
                    install.kind.label(),
                    tier_label(install.probe.tier),
                )
            }
            None => {
                if self.attempts.is_empty() {
                    "codex is not installed: no configured path, no `codex` on PATH, and no app bundle".to_string()
                } else {
                    let tried = self
                        .attempts
                        .iter()
                        .map(|attempt| {
                            format!("{} ({})", attempt.program.display(), attempt.reason)
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    if self.failure_kind() == FailureKind::MissingBinary {
                        format!("codex is not installed; tried {tried}")
                    } else {
                        format!("no usable Codex executable; tried {tried}")
                    }
                }
            }
        }
    }
}

fn tier_label(tier: CodexTier) -> &'static str {
    match tier {
        CodexTier::AppServer => "app-server",
        CodexTier::Exec => "exec",
    }
}

/// Current and older packaged layouts of the CLI inside the macOS app bundle.
const APP_BUNDLE_APPS: [&str; 2] = ["ChatGPT.app", "Codex.app"];
const APP_BUNDLE_LAYOUTS: [&str; 2] = [
    "Contents/Resources/codex-cli/bin/codex",
    "Contents/Resources/codex",
];

/// Find Codex where it is actually installed. The desktop app does not put
/// `codex` on `PATH`, so an app-bundled binary is a normal installation, and
/// "not installed" is a result rather than an error.
pub async fn detect(profile_path: Option<&Path>, cwd: &Path) -> CodexDetection {
    let path_env = std::env::var_os("PATH").unwrap_or_default();
    let application_roots = [PathBuf::from("/Applications")];
    detect_in(profile_path, cwd, &path_env, &application_roots).await
}

async fn detect_in(
    profile_path: Option<&Path>,
    cwd: &Path,
    path_env: &OsStr,
    application_roots: &[PathBuf],
) -> CodexDetection {
    let mut attempts = Vec::new();
    for (program, kind) in codex_candidates(profile_path, path_env, application_roots) {
        if !program.exists() {
            if kind == CodexInstallKind::ProfilePath {
                attempts.push(CodexAttempt {
                    program,
                    kind,
                    failure: FailureKind::MissingBinary,
                    reason: "not found".to_string(),
                });
            }
            continue;
        }
        if !is_executable(&program) {
            attempts.push(CodexAttempt {
                program,
                kind,
                failure: FailureKind::Protocol,
                reason: "not executable".to_string(),
            });
            continue;
        }
        match probe(&program.to_string_lossy(), cwd).await {
            Ok(probe) => {
                return CodexDetection {
                    install: Some(CodexInstall {
                        program,
                        kind,
                        probe,
                    }),
                    attempts,
                };
            }
            Err(error) => attempts.push(CodexAttempt {
                program,
                kind,
                failure: FailureKind::classify(&error).unwrap_or(FailureKind::Protocol),
                reason: error.to_string(),
            }),
        }
    }
    CodexDetection {
        install: None,
        attempts,
    }
}

fn codex_candidates(
    profile_path: Option<&Path>,
    path_env: &OsStr,
    application_roots: &[PathBuf],
) -> Vec<(PathBuf, CodexInstallKind)> {
    let mut candidates = Vec::new();
    if let Some(profile_path) = profile_path {
        candidates.push((profile_path.to_path_buf(), CodexInstallKind::ProfilePath));
    }
    if let Some(found) = find_on_path("codex", path_env) {
        candidates.push((found, CodexInstallKind::Path));
    }
    for root in application_roots {
        for app in APP_BUNDLE_APPS {
            for layout in APP_BUNDLE_LAYOUTS {
                candidates.push((root.join(app).join(layout), CodexInstallKind::AppBundle));
            }
        }
    }
    candidates
}

fn find_on_path(program: &str, path_env: &OsStr) -> Option<PathBuf> {
    std::env::split_paths(path_env)
        .filter(|directory| !directory.as_os_str().is_empty())
        .map(|directory| directory.join(program))
        .find(|candidate| candidate.is_file())
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    match std::fs::metadata(path) {
        Ok(metadata) => metadata.permissions().mode() & 0o111 != 0,
        Err(error) => {
            tracing::debug!(path = %path.display(), %error, "could not read the codex candidate's permissions");
            false
        }
    }
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

pub struct CodexProject {
    pub name: String,
    pub path: PathBuf,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProjectPage {
    data: Vec<Project>,
    next_cursor: Option<String>,
}

#[derive(Deserialize)]
struct Project {
    name: String,
    roots: Vec<ProjectRoot>,
}

#[derive(Deserialize)]
struct ProjectRoot {
    path: PathBuf,
}

pub fn desktop_project_program() -> Option<PathBuf> {
    let path_env = std::env::var_os("PATH").unwrap_or_default();
    let mut candidates = codex_candidates(None, &path_env, &[PathBuf::from("/Applications")]);
    // A CLI on PATH may predate project/list while the installed desktop build supports it.
    candidates.sort_by_key(|(_, kind)| *kind != CodexInstallKind::AppBundle);
    candidates
        .into_iter()
        .find(|(path, _)| path.is_file() && is_executable(path))
        .map(|(program, _)| program)
}

pub async fn desktop_projects() -> HarnessResult<Vec<CodexProject>> {
    let program = desktop_project_program().ok_or_else(|| HarnessError::MissingProgram {
        program: "codex".into(),
    })?;
    list_projects(SpawnSpec {
        program: program.to_string_lossy().into_owned(),
        args: vec!["app-server".into()],
        cwd: std::env::temp_dir(),
        ..SpawnSpec::default()
    })
    .await
}

async fn list_projects(spec: SpawnSpec) -> HarnessResult<Vec<CodexProject>> {
    let mut harness = CodexHarness::new();
    let result = async {
        harness.connect(spec, true).await?;
        let mut projects = Vec::new();
        let mut cursor = None;
        let mut seen_cursors = BTreeSet::new();
        loop {
            let response = harness
                .request("project/list", json!({ "cursor": cursor, "limit": 100 }))
                .await?;
            let page: ProjectPage = serde_json::from_value(response).map_err(|error| {
                HarnessError::Protocol(format!("invalid Codex project list: {error}"))
            })?;
            projects.extend(page.data.into_iter().filter_map(|project| {
                let root = project.roots.into_iter().next()?;
                Some(CodexProject {
                    name: project.name,
                    path: root.path,
                })
            }));
            let Some(next) = page.next_cursor else {
                return Ok(projects);
            };
            if !seen_cursors.insert(next.clone()) {
                return Err(HarnessError::Protocol(
                    "Codex repeated a project list cursor".into(),
                ));
            }
            cursor = Some(next);
        }
    }
    .await;
    let shutdown = harness.shutdown().await;
    let projects = result?;
    shutdown?;
    Ok(projects)
}

pub async fn project_model(spec: SpawnSpec) -> HarnessResult<Option<String>> {
    let mut harness = CodexHarness::new();
    let result = async {
        let directory = spec.cwd.display().to_string();
        harness.start(spec).await?;
        let config = harness
            .request("config/read", json!({ "cwd": directory }))
            .await?;
        if let Some(model) = config.pointer("/config/model").and_then(Value::as_str) {
            return Ok(Some(model.to_owned()));
        }

        let mut cursor = None;
        let mut seen_cursors = BTreeSet::new();
        loop {
            let response = harness
                .request("model/list", json!({ "cursor": cursor }))
                .await?;
            let models = response
                .get("data")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    HarnessError::Protocol("the Codex model list did not include data".into())
                })?;
            if let Some(model) = models.iter().find(|model| model["isDefault"] == true) {
                return Ok(model
                    .get("model")
                    .and_then(Value::as_str)
                    .map(str::to_owned));
            }
            let Some(next) = response.get("nextCursor").and_then(Value::as_str) else {
                return Ok(None);
            };
            if !seen_cursors.insert(next.to_owned()) {
                return Err(HarnessError::Protocol(
                    "Codex repeated a model list cursor".into(),
                ));
            }
            cursor = Some(next.to_owned());
        }
    }
    .await;
    let shutdown = harness.shutdown().await;
    let model = result?;
    shutdown?;
    Ok(model)
}

#[derive(Default)]
struct Pending {
    responses: HashMap<i64, oneshot::Sender<HarnessResult<Value>>>,
    approvals: HashMap<String, i64>,
    thread_id: Option<String>,
    turn_id: Option<String>,
    turn_error: Option<HarnessEvent>,
}

#[cfg(any(test, feature = "live-test-support"))]
use crate::live_codex::{WireCapture, WireEntry, without_api_credentials};

/// One `codex app-server` session.
pub struct CodexHarness {
    #[cfg(any(test, feature = "live-test-support"))]
    wire_capture: Option<WireCapture>,
    sender: mpsc::UnboundedSender<HarnessEvent>,
    receiver: mpsc::UnboundedReceiver<HarnessEvent>,
    outgoing: Option<mpsc::UnboundedSender<Value>>,
    child: Option<Arc<AsyncMutex<Child>>>,
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
    model: Option<String>,
    command: String,
    protocol: String,
    version: Option<String>,
    started: bool,
}

impl CodexHarness {
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        Self {
            #[cfg(any(test, feature = "live-test-support"))]
            wire_capture: None,
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
            model: None,
            command: String::new(),
            protocol: "app-server".to_string(),
            version: None,
            started: false,
        }
    }

    /// The version the probe read, so diagnostics can name it. Capabilities
    /// still come from the advertised methods, never from this string.
    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.version = Some(version.into());
        self
    }

    #[cfg(any(test, feature = "live-test-support"))]
    pub(crate) fn with_wire_capture(mut self, capture: WireCapture) -> Self {
        self.wire_capture = Some(capture);
        self
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
            patches_in_approvals: self.supports("item/fileChange/requestApproval"),
            resume: self.supports("thread/resume"),
            cancel: self.supports("turn/interrupt"),
            images: true,
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
        if let Err(error) = self.send(json!({ "method": method, "id": id, "params": params })) {
            self.pending.lock().await.responses.remove(&id);
            return Err(error);
        }

        match timeout(REQUEST_TIMEOUT, receiver).await {
            Err(_) => {
                self.pending.lock().await.responses.remove(&id);
                Err(HarnessError::Protocol(format!(
                    "codex did not answer {method}"
                )))
            }
            Ok(Err(_)) => {
                let stderr = stderr_tail(&self.stderr);
                let suffix = if stderr.is_empty() {
                    String::new()
                } else {
                    format!("\n{stderr}")
                };
                Err(HarnessError::TransportClosed(format!(
                    "Codex disconnected while answering {method}.{suffix}"
                )))
            }
            Ok(Ok(outcome)) => outcome,
        }
    }

    async fn connect(
        &mut self,
        spec: SpawnSpec,
        experimental_api: bool,
    ) -> HarnessResult<Capabilities> {
        if self.started {
            return Err(HarnessError::Protocol(
                "the codex session is already started".to_string(),
            ));
        }

        self.command = crate::command_line(&spec);
        let mut command = Command::new(&spec.program);
        command
            .args(&spec.args)
            .current_dir(&spec.cwd)
            .envs(&spec.environment)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        #[cfg(any(test, feature = "live-test-support"))]
        if let Some(capture) = self.wire_capture.as_mut() {
            without_api_credentials(&mut command);
            capture.set_cwd(&spec.cwd);
        }

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
        let child = Arc::new(AsyncMutex::new(child));
        self.child = Some(child.clone());

        let (outgoing, outgoing_receiver) = mpsc::unbounded_channel();
        self.writer_task = Some(spawn_writer(
            stdin,
            outgoing_receiver,
            #[cfg(any(test, feature = "live-test-support"))]
            self.wire_capture.clone(),
        ));
        let (stderr_done, stderr_drained) = oneshot::channel();
        self.stdout_task = Some(spawn_reader(
            stdout,
            self.sender.clone(),
            Arc::clone(&self.pending),
            outgoing.clone(),
            Arc::clone(&self.stderr),
            Arc::clone(&self.closed),
            Arc::clone(&self.closing),
            stderr_drained,
            Arc::downgrade(&child),
            #[cfg(any(test, feature = "live-test-support"))]
            self.wire_capture.clone(),
        ));
        self.stderr_task = Some(spawn_stderr_reader(
            stderr,
            Arc::clone(&self.stderr),
            Some(stderr_done),
        ));
        self.outgoing = Some(outgoing);
        self.started = true;

        let result = self
            .request("initialize", initialize_params(experimental_api))
            .await?;
        if let Some(advertised) = advertised_methods(&result) {
            self.supported = SUPPORTED_METHODS
                .iter()
                .filter(|method| advertised.iter().any(|value| value == *method))
                .map(|method| method.to_string())
                .collect();
        }
        self.protocol = match &self.version {
            Some(version) => format!("app-server ({version})"),
            None => "app-server".to_string(),
        };
        self.send(json!({ "method": "initialized", "params": {} }))?;
        self.sender
            .send(HarnessEvent::SessionState {
                state: SessionState::Ready,
            })
            .map_err(|_| HarnessError::ChannelClosed)?;

        Ok(self.capabilities())
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

    fn diagnostics(&self) -> AgentDiagnostics {
        AgentDiagnostics {
            command: self.command.clone(),
            protocol: self.protocol.clone(),
            fallback_reason: None,
            model: self.model.clone(),
            stderr_tail: stderr_tail(&self.stderr),
        }
    }

    async fn start(&mut self, spec: SpawnSpec) -> HarnessResult<Capabilities> {
        self.connect(spec, false).await
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
                            "sandbox": "workspace-write",
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
        self.model = response
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_owned);
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
        let mut items = vec![json!({ "type": "text", "text": input.text })];
        for attachment in input
            .attachments
            .iter()
            .filter(|file| file.image_mime_type.is_some())
        {
            let path = attachment
                .path
                .to_str()
                .ok_or_else(|| HarnessError::Protocol("a Codex image path must be UTF-8".into()))?;
            items.push(json!({ "type": "localImage", "path": path }));
        }
        let response = self
            .request(
                "turn/start",
                json!({
                    "threadId": session_id.as_str(),
                    "input": items,
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
        let answer = match decision {
            Decision::Allow => "accept",
            Decision::Deny => "decline",
            Decision::AllowSession | Decision::AllowProject => {
                return Err(HarnessError::ApprovalOptionUnavailable);
            }
        };
        let request_id = self
            .pending
            .lock()
            .await
            .approvals
            .remove(approval_id.as_str())
            .ok_or(HarnessError::NoPendingApproval)?;
        self.send(json!({ "id": request_id, "result": { "decision": answer } }))?;
        self.sender
            .send(HarnessEvent::ApprovalResolved {
                approval_id,
                decision,
                source: crate::ApprovalSource::User,
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
            Some(child) => {
                let mut child = child.lock().await;
                match child.kill().await.and_then(|()| child.try_wait()) {
                    Ok(status) => status.and_then(|status| status.code()),
                    Err(error) => {
                        tracing::debug!(%error, "could not stop the codex process");
                        None
                    }
                }
            }
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
    command: String,
    version: Option<String>,
    images: bool,
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
    reader: tokio::task::AbortHandle,
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
            command: String::new(),
            version: None,
            images: false,
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

    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.version = Some(version.into());
        self
    }

    pub fn with_images(mut self, enabled: bool) -> Self {
        self.images = enabled;
        self
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            streaming: false,
            thoughts: true,
            tool_calls: true,
            cancel: true,
            usage: true,
            images: self.images,
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

    fn diagnostics(&self) -> AgentDiagnostics {
        AgentDiagnostics {
            command: self.command.clone(),
            protocol: self.version.as_ref().map_or_else(|| "codex exec --json".into(), |version| format!("codex exec --json ({version})")),
            fallback_reason: Some("Codex app-server did not complete the initialize handshake. Each prompt starts a separate Codex process.".into()),
            model: None,
            stderr_tail: stderr_tail(&self.stderr),
        }
    }

    async fn start(&mut self, spec: SpawnSpec) -> HarnessResult<Capabilities> {
        if self.started {
            return Err(HarnessError::Protocol(
                "the codex exec session is already started".to_string(),
            ));
        }
        self.command = std::iter::once(spec.program.as_str())
            .chain(["exec", "--json"])
            .chain(spec.args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ");
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
        command.arg("exec").arg("--json").args(&self.args);
        for attachment in input
            .attachments
            .iter()
            .filter(|file| self.images && file.image_mime_type.is_some())
        {
            let mut option = std::ffi::OsString::from("--image=");
            option.push(&attachment.path);
            command.arg(option);
        }
        command
            .arg("--")
            .arg(&input.text)
            .current_dir(&self.cwd)
            .envs(&self.environment)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);

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
        let (stderr_done, stderr_drained) = oneshot::channel();
        self.stderr_task = Some(spawn_stderr_reader(
            stderr,
            Arc::clone(&self.stderr),
            Some(stderr_done),
        ));

        let turn_id = TurnId::new(format!("exec-turn-{}", self.next_turn));
        self.next_turn += 1;
        let (kill, kill_receiver) = oneshot::channel();
        let finished = Arc::new(AtomicBool::new(false));
        let reader = spawn_exec_reader(stdout, self.sender.clone());
        let reader_abort = reader.abort_handle();
        let waiter = spawn_exec_waiter(
            child,
            kill_receiver,
            self.sender.clone(),
            Arc::clone(&self.stderr),
            stderr_drained,
            Arc::clone(&finished),
            reader,
        );
        self.run = Some(ExecRun {
            waiter,
            reader: reader_abort,
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

        if let Some(mut run) = self.run.take() {
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
        }

        if let Some(task) = self.stderr_task.take() {
            task.abort();
        }
        self.started = false;
        self.session_id = None;

        self.sender
            .send(HarnessEvent::Exited {
                code: Some(0),
                stderr: stderr_tail(&self.stderr),
            })
            .map_err(|_| HarnessError::ChannelClosed)
    }
}

fn spawn_exec_waiter(
    mut child: Child,
    kill: oneshot::Receiver<()>,
    sender: mpsc::UnboundedSender<HarnessEvent>,
    stderr: Arc<Mutex<VecDeque<String>>>,
    stderr_drained: oneshot::Receiver<()>,
    finished: Arc<AtomicBool>,
    mut reader: JoinHandle<()>,
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
                #[cfg(unix)]
                if let Some(group) = child.id().and_then(|id| libc::pid_t::try_from(id).ok())
                    && unsafe { libc::killpg(group, libc::SIGKILL) } != 0
                {
                    let error = std::io::Error::last_os_error();
                    if error.raw_os_error() != Some(libc::ESRCH) {
                        tracing::warn!(%error, "could not stop the Codex exec process group");
                    }
                }
                if let Err(error) = child.kill().await {
                    tracing::debug!(%error, "could not kill the codex exec process");
                }
                child.wait().await
            }
        };

        match timeout(Duration::from_secs(1), &mut reader).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => tracing::debug!(%error, "the Codex exec reader stopped"),
            Err(_) => reader.abort(),
        }
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
        if reason == TurnFinishReason::Failed {
            if timeout(Duration::from_millis(150), stderr_drained)
                .await
                .is_err()
            {
                tracing::debug!("codex exec stderr did not close with the process");
            }
            let stderr = stderr_tail(&stderr);
            let status = code.map_or_else(
                || "exit status unavailable".into(),
                |code| format!("exit code {code}"),
            );
            let text = format!(
                "Codex exec failed ({status}){suffix}",
                suffix = if stderr.is_empty() {
                    String::new()
                } else {
                    format!(": {stderr}")
                }
            );
            let kind = FailureKind::classify(&HarnessError::Protocol(text.clone()))
                .unwrap_or(FailureKind::Protocol);
            if sender.send(HarnessEvent::TurnError { kind, text }).is_err() {
                tracing::debug!("the core stopped listening to codex errors");
            }
        }
        finished.store(true, Ordering::Relaxed);
        if sender.send(HarnessEvent::TurnFinished { reason }).is_err() {
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
    #[cfg(any(test, feature = "live-test-support"))] capture: Option<WireCapture>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(message) = outgoing.recv().await {
            #[cfg(any(test, feature = "live-test-support"))]
            if let Some(capture) = &capture {
                capture.push(WireEntry::Request(message.clone()));
            }
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
    stderr_drained: oneshot::Receiver<()>,
    child: std::sync::Weak<AsyncMutex<Child>>,
    #[cfg(any(test, feature = "live-test-support"))] capture: Option<WireCapture>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        let protocol_failure = loop {
            let line = match lines.next_line().await {
                Ok(Some(line)) => line,
                Ok(None) => break None,
                Err(error) => break Some(format!("Could not read Codex stdout: {error}")),
            };
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let message = match serde_json::from_str::<Value>(trimmed) {
                Ok(message) => message,
                Err(error) => break Some(format!("Codex sent malformed JSON: {error}")),
            };
            #[cfg(any(test, feature = "live-test-support"))]
            if let Some(capture) = &capture {
                capture.push(WireEntry::Response(message.clone()));
            }
            route_incoming(message, &sender, &pending, &outgoing).await;
        };

        closed.store(true, Ordering::Relaxed);
        if closing.load(Ordering::Relaxed) {
            return;
        }
        let Some(child) = child.upgrade() else {
            return;
        };
        let outcome = {
            let mut child = child.lock().await;
            if let Some(message) = protocol_failure {
                if let Err(error) = child.kill().await {
                    tracing::debug!(%error, "could not stop Codex after invalid protocol output");
                }
                Err(message)
            } else {
                match timeout(Duration::from_millis(250), child.wait()).await {
                    Ok(Ok(status)) => Ok(status.code()),
                    Ok(Err(error)) => Err(format!("Could not read the Codex exit status: {error}")),
                    Err(_) => {
                        if let Err(error) = child.kill().await {
                            tracing::debug!(%error, "could not stop Codex after stdout closed");
                        }
                        Err("Codex closed its protocol stream while still running.".into())
                    }
                }
            }
        };
        if timeout(Duration::from_millis(150), stderr_drained)
            .await
            .is_err()
        {
            tracing::debug!("codex stderr did not close with stdout");
        }
        let stderr = stderr_tail(&stderr);
        #[cfg(any(test, feature = "live-test-support"))]
        if let Some(capture) = &capture {
            capture.push(WireEntry::Exit(stderr.clone()));
        }
        let event = match outcome {
            Ok(code) => HarnessEvent::Exited { code, stderr },
            Err(text) => HarnessEvent::ConnectionFailed {
                kind: FailureKind::Protocol,
                text: if stderr.is_empty() {
                    text
                } else {
                    format!("{text}\n{stderr}")
                },
            },
        };
        if !closing.load(Ordering::Relaxed) && sender.send(event).is_err() {
            tracing::debug!("the core stopped listening to codex events");
        }
        pending.lock().await.responses.clear();
    })
}

fn spawn_stderr_reader(
    stderr: ChildStderr,
    tail: Arc<Mutex<VecDeque<String>>>,
    drained: Option<oneshot::Sender<()>>,
) -> JoinHandle<()> {
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
        if let Some(drained) = drained
            && drained.send(()).is_err()
        {
            tracing::debug!("codex stdout reader already stopped");
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
                Some(error) => Err(codex_rpc_error(error)),
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

fn codex_rpc_error(error: &Value) -> HarnessError {
    let message = error["message"]
        .as_str()
        .unwrap_or("The Codex server returned an error.");
    let kind = match error["code"].as_i64() {
        Some(401) => FailureKind::AuthRequired,
        Some(METHOD_NOT_FOUND) => FailureKind::UnsupportedVersion,
        _ => FailureKind::from_message(message, FailureKind::Protocol),
    };
    HarnessError::Reported {
        kind,
        message: message.to_owned(),
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
        patch: approval_patch(&method, &params),
        choices: None,
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
                let mut pending = pending.lock().await;
                pending.turn_id = Some(turn_id);
                pending.turn_error = None;
            }
        }
        "error" if params["willRetry"] == false => {
            let mut pending = pending.lock().await;
            if params["turnId"].as_str() == pending.turn_id.as_deref() {
                pending.turn_error = codex_turn_error(&params["error"]);
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
        "item/commandExecution/outputDelta" => {
            if let (Some(id), Some(delta)) = (
                params.get("itemId").and_then(Value::as_str),
                params.get("delta").and_then(Value::as_str),
            ) {
                events.push(HarnessEvent::CommandOutput {
                    tool_call_id: ToolCallId::new(id),
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
        "serverRequest/resolved" => {
            if let Some(request_id) = params.get("requestId").and_then(Value::as_i64) {
                let mut pending = pending.lock().await;
                let approval_id = pending
                    .approvals
                    .iter()
                    .find_map(|(id, server_id)| (*server_id == request_id).then(|| id.clone()));
                if let Some(approval_id) = approval_id {
                    pending.approvals.remove(&approval_id);
                    events.push(HarnessEvent::ApprovalWithdrawn {
                        approval_id: ApprovalId::new(approval_id),
                    });
                }
            }
        }
        "turn/completed" | "turn/failed" | "turn/cancelled" => {
            let reason = match method {
                "turn/failed" => Some(TurnFinishReason::Failed),
                "turn/cancelled" => Some(TurnFinishReason::Cancelled),
                _ => turn_finish_reason(params),
            };
            let mut pending = pending.lock().await;
            pending.turn_id = None;
            let reported_error = pending.turn_error.take();
            events.extend(pending.approvals.drain().map(|(id, _)| {
                HarnessEvent::ApprovalWithdrawn {
                    approval_id: ApprovalId::new(id),
                }
            }));
            if reason == Some(TurnFinishReason::Failed) {
                events.extend(
                    codex_turn_error(&params["turn"]["error"])
                        .or_else(|| codex_turn_error(&params["error"]))
                        .or(reported_error),
                );
            }
            let reason = reason.unwrap_or_else(|| {
                events.push(HarnessEvent::TurnError {
                    kind: FailureKind::Protocol,
                    text: "Codex completed the turn without a recognized status.".into(),
                });
                TurnFinishReason::Failed
            });
            events.push(HarnessEvent::TurnFinished { reason });
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

fn codex_turn_error(error: &Value) -> Option<HarnessEvent> {
    let message = error["message"].as_str()?;
    let info = &error["codexErrorInfo"];
    let unauthorized = info == "unauthorized"
        || info
            .as_object()
            .is_some_and(|fields| fields.values().any(|field| field["httpStatusCode"] == 401));
    let kind = if unauthorized {
        FailureKind::AuthRequired
    } else {
        FailureKind::from_message(message, FailureKind::AgentRejected)
    };
    let text = match error["additionalDetails"]
        .as_str()
        .filter(|details| !details.is_empty())
    {
        Some(details) => format!("{message}\n{details}"),
        None => message.to_string(),
    };
    Some(HarnessEvent::TurnError { kind, text })
}

fn turn_finish_reason(params: &Value) -> Option<TurnFinishReason> {
    match params
        .get("turn")
        .and_then(|turn| turn.get("status"))
        .and_then(Value::as_str)
    {
        Some("interrupted") => Some(TurnFinishReason::Cancelled),
        Some("failed") => Some(TurnFinishReason::Failed),
        Some("completed") => Some(TurnFinishReason::Completed),
        _ => None,
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

/// A file-change approval carries its changes inline, so the card can show a
/// diff instead of asking the user to imagine one.
fn approval_patch(method: &str, params: &Value) -> Option<FileDiff> {
    if method != "item/fileChange/requestApproval" {
        return None;
    }
    file_diffs(params).into_iter().next()
}

fn approval_paths(params: &Value) -> Vec<PathBuf> {
    let mut paths = params
        .get("paths")
        .and_then(Value::as_array)
        .map(|paths| {
            paths
                .iter()
                .filter_map(Value::as_str)
                .map(PathBuf::from)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for diff in file_diffs(params) {
        let path = PathBuf::from(diff.path);
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    paths
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
    use crate::{Entry, EntryKind, Transcript};

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
    *'"method":"config/read"'*)
      case "$line" in
        *'/configured"'*) model='"project-model"' ;;
        *) model=null ;;
      esac
      printf '{"id":%s,"result":{"config":{"model":%s}}}\n' "$id" "$model"
      ;;
    *'"method":"model/list"'*)
      case "$line" in
        *'"cursor":"next"'*) printf '{"id":%s,"result":{"data":[{"model":"catalog-default","isDefault":true}],"nextCursor":null}}\n' "$id" ;;
        *) printf '{"id":%s,"result":{"data":[{"model":"another-model","isDefault":false}],"nextCursor":"next"}}\n' "$id" ;;
      esac
      ;;
    *'"method":"thread/start"'*)
      printf '{"id":%s,"result":{"thread":{"id":"thr_1"},"model":"session-model"}}\n' "$id"
      ;;
    *'"method":"thread/resume"'*)
      printf '%s\n' "$line" >> "$directory/methods.txt"
      printf '{"id":%s,"result":{"thread":{"id":"thr_resumed"}}}\n' "$id"
      ;;
    *'"method":"turn/start"'*)
      printf '{"id":%s,"result":{"turn":{"id":"turn_1","status":"inProgress"}}}\n' "$id"
      printf '{"method":"turn/started","params":{"turn":{"id":"turn_1"}}}\n'
      printf '{"method":"item/agentMessage/delta","params":{"itemId":"item_1","delta":"working"}}\n'
      printf '{"method":"item/started","params":{"item":{"id":"item_2","type":"commandExecution","command":"cargo test","status":"inProgress"}}}\n'
      printf '%s\n' '{"method":"item/commandExecution/outputDelta","params":{"itemId":"item_2","delta":"running tests\n"}}'
      printf '%s\n' '{"method":"item/commandExecution/outputDelta","params":{"itemId":"item_2","delta":"warning: unused import\n"}}'
      printf '%s\n' '{"method":"item/completed","params":{"item":{"id":"item_2","type":"commandExecution","command":"cargo test","status":"completed","aggregatedOutput":"running tests\nwarning: unused import\nok\n"}}}'
      printf '{"method":"turn/completed","params":{"turn":{"id":"turn_1","status":"completed"}}}\n'
      ;;
    *) : ;;
  esac
done
"#;

    /// Waits for an interrupt while two approvals are unanswered.
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
      printf '{"id":80,"method":"item/commandExecution/requestApproval","params":{"itemId":"approval-80","command":"First command"}}\n'
      printf '{"id":81,"method":"item/commandExecution/requestApproval","params":{"itemId":"approval-81","command":"Second command"}}\n'
      ;;
    *'"method":"turn/interrupt"'*)
      printf '%s\n' "$line" >> "$directory/interrupt.txt"
      printf '{"id":%s,"result":{}}\n' "$id"
      printf '{"method":"serverRequest/resolved","params":{"requestId":80,"threadId":"thr_waiting"}}\n'
      printf '{"method":"turn/completed","params":{"turn":{"id":"turn_waiting","status":"interrupted"}}}\n'
      ;;
    *) : ;;
  esac
done
"#;

    /// Asks for approval of a file change that carries its own diff.
    const FILE_APPROVAL_STUB: &str = r#"#!/usr/bin/env bash
if [ "$1" = "--version" ]; then echo "codex-cli 9.9.9"; exit 0; fi
directory="$(cd "$(dirname "$0")" && pwd)"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '{"id":%s,"result":{"capabilities":{"methods":["thread/start","turn/start","item/fileChange/requestApproval"]}}}\n' "$id"
      ;;
    *'"method":"thread/start"'*)
      printf '{"id":%s,"result":{"thread":{"id":"thr_files"}}}\n' "$id"
      ;;
    *'"method":"turn/start"'*)
      printf '{"id":%s,"result":{"turn":{"id":"turn_files","status":"inProgress"}}}\n' "$id"
      printf '{"id":77,"method":"item/fileChange/requestApproval","params":{"itemId":"item_9","threadId":"thr_files","turnId":"turn_files","changes":[{"path":"src/main.rs","kind":"update","diff":"@@ -1 +1 @@\\n-const answer = 41;\\n+const answer = 42;"}]}}\n'
      ;;
    *'"id":77'*)
      printf '%s\n' "$line" >> "$directory/decision.txt"
      printf '{"method":"turn/completed","params":{"turn":{"id":"turn_files","status":"completed"}}}\n'
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
if [ "$1" = "exec" ] && [ "$2" = "--help" ]; then echo "  --json"; exit 0; fi
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
        write_executable(&path, contents);
        path.to_string_lossy().into_owned()
    }

    fn write_executable(path: &Path, contents: &str) {
        std::fs::write(path, contents).expect("the stub is writable");
        let permissions = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(path, permissions).expect("the stub is executable");
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

    #[tokio::test]
    async fn app_server_receives_the_cached_image_path_as_a_local_image()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TempDir::new()?;
        let script = PROTOCOL_STUB.replace(
            "    *'\"method\":\"turn/start\"'*)\n",
            "    *'\"method\":\"turn/start\"'*)\n      printf '%s\\n' \"$line\" > \"$directory/turn.json\"\n",
        );
        let stub = write_stub(&directory, "codex-images", &script);
        let (mut harness, capabilities) = start_session(&stub, directory.path()).await;
        assert!(capabilities.images);
        let path = directory.path().join("a screenshot.png");
        harness
            .prompt(PromptInput {
                text: "Review this screenshot.".into(),
                attachments: vec![crate::PromptAttachment {
                    path: path.clone(),
                    image_mime_type: Some("image/png".into()),
                    metadata: None,
                }],
            })
            .await?;
        let request: Value =
            serde_json::from_slice(&std::fs::read(directory.path().join("turn.json"))?)?;
        assert_eq!(
            request["params"]["input"],
            json!([
                {"type": "text", "text": "Review this screenshot."},
                {"type": "localImage", "path": path},
            ])
        );
        harness.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn failed_turn_preserves_the_typed_sign_in_error()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TempDir::new()?;
        let script = PROTOCOL_STUB.replace(
            r#""id":"turn_1","status":"completed""#,
            r#""id":"turn_1","status":"failed","error":{"message":"Session expired","codexErrorInfo":"unauthorized","additionalDetails":"Refresh the account"}"#,
        );
        let stub = write_stub(&directory, "codex-auth", &script);
        let (mut harness, _) = start_session(&stub, directory.path()).await;
        harness.prompt(PromptInput::text("go")).await?;
        let events = collect_until(&mut harness, |events| {
            events
                .iter()
                .any(|event| matches!(event, HarnessEvent::TurnFinished { .. }))
        })
        .await;
        assert!(
            matches!(events.as_slice(), [.., HarnessEvent::TurnError { kind: FailureKind::AuthRequired, text }, HarnessEvent::TurnFinished { reason: TurnFinishReason::Failed }] if text == "Session expired\nRefresh the account"),
            "{events:?}"
        );
        harness.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn process_exit_and_invalid_output_have_different_outcomes()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TempDir::new()?;
        for (name, action) in [
            ("codex-exit", "echo 'fixture crashed' >&2; exit 23"),
            ("codex-invalid", "echo 'not valid JSON'"),
        ] {
            let script = PROTOCOL_STUB.replace(
                r#"printf '{"method":"turn/completed","params":{"turn":{"id":"turn_1","status":"completed"}}}\n'"#,
                action,
            );
            let stub = write_stub(&directory, name, &script);
            let (mut harness, _) = start_session(&stub, directory.path()).await;
            harness.prompt(PromptInput::text("go")).await?;
            let events = collect_until(&mut harness, |events| {
                events.iter().any(|event| {
                    matches!(
                        event,
                        HarnessEvent::Exited { .. } | HarnessEvent::ConnectionFailed { .. }
                    )
                })
            })
            .await;
            match (name, events.last()) {
                (
                    "codex-exit",
                    Some(HarnessEvent::Exited {
                        code: Some(23),
                        stderr,
                    }),
                ) => assert_eq!(stderr, "fixture crashed"),
                (
                    "codex-invalid",
                    Some(HarnessEvent::ConnectionFailed {
                        kind: FailureKind::Protocol,
                        text,
                    }),
                ) => assert!(text.contains("malformed JSON")),
                _ => panic!("{name}: {events:?}"),
            }
            harness.shutdown().await?;
        }
        Ok(())
    }

    #[tokio::test]
    async fn lists_primary_folders_across_codex_project_pages()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TempDir::new()?;
        let stub = write_stub(
            &directory,
            "codex-projects",
            r#"#!/usr/bin/env bash
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*'"experimentalApi":true'*)
      printf '{"id":%s,"result":{}}\n' "$id" ;;
    *'"method":"initialized"'*)
      initialized=1 ;;
    *'"method":"project/list"'*'"cursor":null'*)
      [ "${initialized:-0}" = 1 ] || exit 1
      printf '{"id":%s,"result":{"data":[{"name":"App","roots":[{"path":"/tmp/app"},{"path":"/tmp/related"}]},{"name":"No folder","roots":[]}],"nextCursor":"page-2"}}\n' "$id" ;;
    *'"method":"project/list"'*'"cursor":"page-2"'*)
      printf '{"id":%s,"result":{"data":[{"name":"Docs","roots":[{"path":"/tmp/docs"}]}],"nextCursor":null}}\n' "$id" ;;
    *) exit 1 ;;
  esac
done
"#,
        );
        let projects = list_projects(spec(&stub, directory.path())).await?;
        let folders: Vec<_> = projects
            .iter()
            .map(|project| (project.name.as_str(), project.path.as_path()))
            .collect();
        assert_eq!(
            folders,
            [
                ("App", Path::new("/tmp/app")),
                ("Docs", Path::new("/tmp/docs"))
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn diagnostics_report_the_command_and_protocol() {
        let directory = TempDir::new().expect("temp dir");
        let stub = write_stub(&directory, "codex.sh", PROTOCOL_STUB);
        let mut harness = CodexHarness::new().with_version("codex-cli 9.9.9");
        harness
            .start(spec(&stub, directory.path()))
            .await
            .expect("the stub app-server starts");
        harness.open_session(None).await.expect("a thread opens");

        let diagnostics = harness.diagnostics();
        assert_eq!(diagnostics.command, stub);
        assert_eq!(diagnostics.protocol, "app-server (codex-cli 9.9.9)");

        harness.shutdown().await.expect("the session stops");
    }

    fn fixture_lines(name: &str) -> Vec<Value> {
        let path = format!("{}/fixtures/codex/{name}", env!("CARGO_MANIFEST_DIR"));
        let text = std::fs::read_to_string(&path).expect("the recorded fixture is readable");
        text.lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).expect("a recorded line is JSON"))
            .collect()
    }

    /// Replay a recorded app-server stream through the same translation the
    /// stdout reader uses, with no child process involved.
    async fn replay_app_server(name: &str) -> Transcript {
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let pending = Arc::new(AsyncMutex::new(Pending::default()));
        let (outgoing, _outgoing_receiver) = mpsc::unbounded_channel();
        for message in fixture_lines(name) {
            let params = message.get("params").cloned().unwrap_or(Value::Null);
            match (message.get("id"), message.get("method")) {
                (Some(id), Some(method)) => {
                    handle_server_request(
                        id.clone(),
                        method.as_str().unwrap_or_default().to_string(),
                        params,
                        &sender,
                        &pending,
                        &outgoing,
                    )
                    .await;
                }
                (None, Some(method)) => {
                    handle_notification(
                        method.as_str().unwrap_or_default(),
                        &params,
                        &sender,
                        &pending,
                    )
                    .await;
                }
                // Answers to Shepherd's own requests need a live waiter; the
                // mapping under test is the notification and request surface.
                (Some(_), None) | (None, None) => {}
            }
        }
        drop(sender);
        let mut transcript = Transcript::default();
        while let Ok(event) = receiver.try_recv() {
            transcript.apply(&event);
        }
        transcript
    }

    fn summaries(transcript: &Transcript) -> Vec<String> {
        transcript.entries().iter().map(Entry::summary).collect()
    }

    #[tokio::test]
    async fn a_recorded_app_server_session_replays_without_the_cli() {
        let transcript = replay_app_server("app-server-session.jsonl").await;
        assert_eq!(
            summaries(&transcript),
            vec![
                "agent: Reading the repo. Done.".to_string(),
                "cargo test (Completed)".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn a_recorded_approval_with_no_decision_is_withdrawn_when_the_turn_ends() {
        let transcript = replay_app_server("app-server-approval.jsonl").await;
        assert_eq!(
            summaries(&transcript),
            vec![
                "agent: About to run the tests.".to_string(),
                "approval: cargo test (withdrawn)".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn a_recorded_exec_stream_replays_without_the_cli() {
        let mut transcript = Transcript::default();
        let mut usage = None;
        for message in fixture_lines("exec-session.jsonl") {
            for event in exec_events(&message) {
                if let HarnessEvent::Usage { used, size, .. } = &event {
                    usage = Some((*used, *size));
                }
                transcript.apply(&event);
            }
        }
        assert_eq!(
            summaries(&transcript),
            vec![
                "agent: Running the suite.".to_string(),
                "cargo test (Completed)".to_string(),
                "usage: 1200/128000".to_string(),
            ]
        );
        assert_eq!(usage, Some((1200, 128_000)));
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
    async fn model_lookup_prefers_the_project_config_and_falls_back_to_the_catalog()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TempDir::new()?;
        let stub = write_stub(&directory, "codex.sh", PROTOCOL_STUB);
        let configured = directory.path().join("configured");
        std::fs::create_dir(&configured)?;

        assert_eq!(
            project_model(spec(&stub, &configured)).await?.as_deref(),
            Some("project-model")
        );
        assert_eq!(
            project_model(spec(&stub, directory.path()))
                .await?
                .as_deref(),
            Some("catalog-default")
        );
        Ok(())
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
        assert!(!probe.images);
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
        assert_eq!(
            harness.diagnostics().model.as_deref(),
            Some("session-model")
        );

        harness
            .prompt(PromptInput::text("run the tests"))
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
        let mut transcript = Transcript::default();
        let mut live_output = Vec::new();
        for event in &events {
            transcript.apply(event);
            if matches!(event, HarnessEvent::CommandOutput { .. })
                && let Some(EntryKind::ToolCall {
                    status: ToolStatus::Running,
                    output: Some(output),
                    ..
                }) = transcript.entry(1).map(|entry| &entry.kind)
            {
                live_output.push(output.clone());
            }
        }
        assert_eq!(
            live_output,
            ["running tests\n", "running tests\nwarning: unused import\n"]
        );
        assert_eq!(transcript.len(), 2);
        assert!(matches!(
            transcript.entry(1).map(|entry| &entry.kind),
            Some(EntryKind::ToolCall { status: ToolStatus::Completed, output: Some(output), .. })
                if output == "running tests\nwarning: unused import\nok\n"
        ));
        harness.shutdown().await.expect("the session stops");
    }

    #[tokio::test]
    async fn an_approval_is_answered_with_the_users_decision() {
        let directory = TempDir::new().expect("temp dir");
        let stub = write_stub(&directory, "codex-approval.sh", APPROVAL_STUB);
        let (mut harness, _capabilities) = start_session(&stub, directory.path()).await;

        harness
            .prompt(PromptInput::text("run the tests"))
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
    async fn a_denied_approval_is_reported_as_declined() {
        let directory = TempDir::new().expect("temp dir");
        let stub = write_stub(&directory, "codex-denial.sh", APPROVAL_STUB);
        let (mut harness, _capabilities) = start_session(&stub, directory.path()).await;

        harness
            .prompt(PromptInput::text("run the tests"))
            .await
            .expect("the turn starts");
        collect_until(&mut harness, |events| {
            events
                .iter()
                .any(|event| matches!(event, HarnessEvent::ApprovalRequested { .. }))
        })
        .await;

        harness
            .respond(ApprovalId::new("item_2"), Decision::Deny)
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
                decision: Decision::Deny,
                ..
            }
        )));

        let decision = std::fs::read_to_string(directory.path().join("decision.txt"))
            .expect("the stub recorded the decision");
        assert!(
            decision.contains("\"decline\""),
            "unexpected decision payload: {decision}"
        );
        harness.shutdown().await.expect("the session stops");
    }

    #[tokio::test]
    async fn a_file_change_approval_carries_its_diff() {
        let directory = TempDir::new().expect("temp dir");
        let stub = write_stub(&directory, "codex-files.sh", FILE_APPROVAL_STUB);
        let (mut harness, _capabilities) = start_session(&stub, directory.path()).await;

        harness
            .prompt(PromptInput::text("rename the answer"))
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
            .expect("the file approval arrives");

        let patch = request.patch.as_ref().expect("the card can show a diff");
        assert_eq!(patch.path, "src/main.rs");
        assert!(
            patch.new_text.contains("+const answer = 42;"),
            "unexpected patch: {}",
            patch.new_text
        );
        assert_eq!(request.paths, vec![PathBuf::from("src/main.rs")]);
        harness.shutdown().await.expect("the session stops");
    }

    #[tokio::test]
    async fn cancelling_interrupts_the_active_turn() {
        let directory = TempDir::new().expect("temp dir");
        let stub = write_stub(&directory, "codex-interrupt.sh", INTERRUPT_STUB);
        let (mut harness, _capabilities) = start_session(&stub, directory.path()).await;

        harness
            .prompt(PromptInput::text("keep going"))
            .await
            .expect("the turn starts");
        let requests = collect_until(&mut harness, |events| {
            events
                .iter()
                .filter(|event| matches!(event, HarnessEvent::ApprovalRequested { .. }))
                .count()
                == 2
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
        let mut transcript = crate::Transcript::default();
        for event in requests.iter().chain(&events) {
            transcript.apply(event);
        }
        for title in ["First command", "Second command"] {
            assert!(
                transcript
                    .entries()
                    .iter()
                    .any(|entry| entry.summary() == format!("approval: {title} (withdrawn)"))
            );
        }
        harness
            .prompt(PromptInput::text("a follow-up after cancellation"))
            .await
            .expect("no withdrawn approval blocks the follow-up");
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
        assert!(
            methods.contains("thread/resume") && methods.contains("\"threadId\":\"thr_stored\""),
            "the resume must name the stored thread: {methods}"
        );
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
        assert!(capabilities.cancel);

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
            .prompt(PromptInput::text("run the tests"))
            .await
            .expect("the exec turn starts");
        let events = collect_until(&mut harness, |events| {
            events
                .iter()
                .any(|event| matches!(event, HarnessEvent::TurnFinished { .. }))
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
            !events
                .iter()
                .any(|event| matches!(event, HarnessEvent::Exited { .. }))
        );

        let arguments = std::fs::read_to_string(directory.path().join("args.txt"))
            .expect("the stub recorded its arguments");
        assert!(arguments.contains("--json"));
        assert!(arguments.contains("run the tests"));
        harness
            .prompt(PromptInput::text("second prompt"))
            .await
            .expect("another command starts in the same adapter");
        let next = collect_until(&mut harness, |events| {
            events
                .iter()
                .any(|event| matches!(event, HarnessEvent::TurnFinished { .. }))
        })
        .await;
        assert!(next.iter().any(|event| matches!(
            event,
            HarnessEvent::TurnFinished {
                reason: TurnFinishReason::Completed
            }
        )));
        harness.shutdown().await.expect("the session stops");
    }

    #[tokio::test]
    async fn exec_image_option_depends_on_the_cli_help_probe()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TempDir::new()?;
        let script = UNSUPPORTED_STUB.replace(
            "echo \"  --json\"",
            "echo \"  --json -i, --image <FILE>...\"",
        );
        let stub = write_stub(&directory, "codex-probe-images", &script);
        let probe = probe(&stub, directory.path()).await?;
        assert_eq!(probe.tier, CodexTier::Exec);
        assert!(probe.images);
        let path = directory.path().join("a screenshot.png");
        let stub = write_stub(&directory, "codex-exec-images", EXEC_STUB);
        let mut harness = ExecHarness::new().with_images(probe.images);
        assert!(harness.start(spec(&stub, directory.path())).await?.images);
        harness.open_session(None).await?;
        harness
            .prompt(PromptInput {
                text: "Review the image.".into(),
                attachments: vec![crate::PromptAttachment {
                    path: path.clone(),
                    image_mime_type: Some("image/png".into()),
                    metadata: None,
                }],
            })
            .await?;
        collect_until(&mut harness, |events| {
            events
                .iter()
                .any(|event| matches!(event, HarnessEvent::TurnFinished { .. }))
        })
        .await;
        let arguments = std::fs::read_to_string(directory.path().join("args.txt"))?;
        assert!(
            arguments
                .lines()
                .any(|line| line == format!("--image={}", path.display()))
        );
        harness.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn a_failed_exec_turn_reports_its_exit_code_and_sign_in_error()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TempDir::new()?;
        let stub = write_stub(
            &directory,
            "codex-exec-fail",
            "#!/bin/sh\nprintf '%s\\n' 'authentication required for this account' >&2\nexit 23\n",
        );
        let (mut harness, _) = start_exec(&stub, directory.path()).await;
        harness.prompt(PromptInput::text("go")).await?;
        let events = collect_until(&mut harness, |events| {
            events
                .iter()
                .any(|event| matches!(event, HarnessEvent::TurnFinished { .. }))
        })
        .await;
        assert!(
            matches!(events.as_slice(), [..,
            HarnessEvent::TurnError { kind: FailureKind::AuthRequired, text },
            HarnessEvent::TurnFinished { reason: TurnFinishReason::Failed },
        ] if text.contains("exit code 23") && text.contains("authentication required for this account")),
            "{events:?}"
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, HarnessEvent::Exited { .. }))
        );
        harness.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn cancelling_exec_kills_the_run() {
        let directory = TempDir::new().expect("temp dir");
        let stub = write_stub(&directory, "codex-exec-slow.sh", EXEC_SLOW_STUB);
        let (mut harness, _capabilities) = start_exec(&stub, directory.path()).await;

        harness
            .prompt(PromptInput::text("keep going"))
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

    #[tokio::test]
    async fn a_restored_transcript_reattaches_a_codex_item() {
        let directory = TempDir::new().expect("temp dir");
        let stub = write_stub(&directory, "codex.sh", PROTOCOL_STUB);
        let (mut harness, _capabilities) = start_session(&stub, directory.path()).await;

        harness
            .prompt(PromptInput::text("run the tests"))
            .await
            .expect("the turn starts");
        let events = collect_until(&mut harness, |events| {
            events
                .iter()
                .any(|event| matches!(event, HarnessEvent::TurnFinished { .. }))
        })
        .await;

        let mut live = Transcript::default();
        for event in &events {
            live.apply(event);
        }
        let mut restored = Transcript::default();
        restored.restore(live.entries().to_vec());
        assert_eq!(restored.len(), live.len());
        for (restored, live) in restored.entries().iter().zip(live.entries()) {
            assert_eq!(restored.id, live.id);
            assert_eq!(restored.sequence, live.sequence);
        }

        let change = restored.apply(&HarnessEvent::Delta {
            item_id: ItemId::new("item_1"),
            kind: DeltaKind::Message,
            text: " again".to_string(),
        });
        assert!(
            !change.appended,
            "a known codex item must land on its original row"
        );
        assert_eq!(restored.len(), live.len());
        assert!(matches!(
            restored.entry(0).map(Entry::summary),
            Some(summary) if summary == "agent: working again"
        ));
        harness.shutdown().await.expect("the session stops");
    }

    #[tokio::test]
    async fn detection_prefers_the_configured_path_and_reports_its_kind() {
        let directory = TempDir::new().expect("temp dir");
        let stub = write_stub(&directory, "codex.sh", PROTOCOL_STUB);

        let detection = detect_in(
            Some(Path::new(&stub)),
            directory.path(),
            OsStr::new(""),
            &[],
        )
        .await;

        let install = detection
            .install
            .as_ref()
            .expect("the configured path verifies");
        assert_eq!(install.kind, CodexInstallKind::ProfilePath);
        assert_eq!(install.probe.tier, CodexTier::AppServer);
        assert_eq!(install.probe.version.as_deref(), Some("codex-cli 9.9.9"));
        let summary = detection.summary();
        assert!(summary.contains("configured path"), "summary: {summary}");
        assert!(summary.contains("app-server"), "summary: {summary}");
    }

    #[tokio::test]
    async fn detection_falls_back_to_path_and_records_the_miss() {
        let directory = TempDir::new().expect("temp dir");
        write_stub(&directory, "codex", PROTOCOL_STUB);
        let missing = Path::new("/nonexistent/codex-for-shepherd");

        let detection = detect_in(
            Some(missing),
            directory.path(),
            directory.path().as_os_str(),
            &[],
        )
        .await;

        let install = detection.install.as_ref().expect("the PATH entry verifies");
        assert_eq!(install.kind, CodexInstallKind::Path);
        assert_eq!(detection.attempts.len(), 1);
        assert_eq!(detection.attempts[0].program, missing);
        assert!(
            detection.attempts[0].reason.contains("not found"),
            "reason: {}",
            detection.attempts[0].reason
        );
    }

    #[tokio::test]
    async fn detection_verifies_the_app_bundle_layout() {
        let directory = TempDir::new().expect("temp dir");
        let bundle_binary = directory
            .path()
            .join("ChatGPT.app/Contents/Resources/codex-cli/bin/codex");
        let bundle_directory = bundle_binary
            .parent()
            .expect("the bundle path has a parent");
        std::fs::create_dir_all(bundle_directory).expect("the bundle directories are creatable");
        write_executable(&bundle_binary, PROTOCOL_STUB);
        let roots = [directory.path().to_path_buf()];

        let detection = detect_in(None, directory.path(), OsStr::new(""), &roots).await;

        let install = detection
            .install
            .as_ref()
            .expect("the bundle candidate verifies");
        assert_eq!(install.kind, CodexInstallKind::AppBundle);
        assert_eq!(install.program, bundle_binary);
    }

    #[tokio::test]
    async fn detection_reports_when_codex_is_not_installed() {
        let directory = TempDir::new().expect("temp dir");

        let detection = detect_in(None, directory.path(), OsStr::new(""), &[]).await;

        assert!(detection.install.is_none());
        assert!(detection.attempts.is_empty());
        let summary = detection.summary();
        assert!(summary.contains("not installed"), "summary: {summary}");
    }

    #[tokio::test]
    async fn detection_reports_the_exec_tier_for_an_old_build() {
        let directory = TempDir::new().expect("temp dir");
        let stub = write_stub(&directory, "codex-old.sh", UNSUPPORTED_STUB);

        let detection = detect_in(
            Some(Path::new(&stub)),
            directory.path(),
            OsStr::new(""),
            &[],
        )
        .await;

        let install = detection
            .install
            .as_ref()
            .expect("an old build is still an install");
        assert_eq!(install.probe.tier, CodexTier::Exec);
        let summary = detection.summary();
        assert!(summary.contains("exec"), "summary: {summary}");
    }
}

#[cfg(test)]
mod live {
    use std::path::Path;
    use std::process::Command;
    use std::time::Duration;

    use tempfile::TempDir;
    use tokio::time::timeout;

    use super::CodexHarness;
    use crate::live_codex::LiveCodexRun;
    use crate::{AgentHarness, Decision, HarnessEvent, PromptInput, ToolStatus, TurnFinishReason};

    const TURN_LIMIT: Duration = Duration::from_secs(180);

    /// Drives the installed, logged-in Codex through the same path the app uses.
    /// Ignored because CI has no CLI; replaying the recorded fixtures covers the
    /// translation without one. Run it with
    /// `cargo test -p agent-harness -- --ignored live_codex`.
    #[tokio::test]
    #[ignore]
    async fn live_codex_streams_a_tool_change_approves_resumes_and_cancels() {
        let Some(run) = LiveCodexRun::load("stream-resume-cancel")
            .await
            .expect("load the live test cache")
        else {
            return;
        };

        let directory = TempDir::new().expect("a temp repo");
        let repo = directory.path();
        git(repo, &["init", "--quiet"]);
        git(repo, &["config", "user.email", "shepherd@test.invalid"]);
        git(repo, &["config", "user.name", "Shepherd Test"]);
        std::fs::write(repo.join("note.txt"), "first line\n").expect("the seed file is writable");
        git(repo, &["add", "note.txt"]);
        git(repo, &["commit", "--quiet", "-m", "seed"]);

        let mut harness = start(&run, "initial", repo).await;
        let thread = harness
            .open_session(None)
            .await
            .expect("a new thread opens");
        eprintln!("thread: {}", thread.as_str());

        let first = drive_turn(
            &mut harness,
            "Append the line 'hello from shepherd' to note.txt. Change nothing else.",
            TURN_LIMIT,
        )
        .await;
        let deltas = first
            .iter()
            .filter(|event| matches!(event, HarnessEvent::Delta { .. }))
            .count();
        let tool_calls = first
            .iter()
            .filter(|event| matches!(event, HarnessEvent::ToolCall { .. }))
            .count();
        eprintln!("first turn: {deltas} deltas, {tool_calls} tool calls");
        assert!(deltas > 0, "the turn must stream text: {first:?}");
        assert!(
            tool_calls > 0,
            "the turn must report a tool call: {first:?}"
        );
        assert!(
            matches!(
                first.last(),
                Some(HarnessEvent::TurnFinished {
                    reason: TurnFinishReason::Completed
                })
            ),
            "the turn must finish: {first:?}"
        );
        if let Some(note) = run.cached_note() {
            std::fs::write(repo.join("note.txt"), note).expect("restore the recorded file change");
        }
        let changed = std::fs::read_to_string(repo.join("note.txt")).expect("note.txt is readable");
        assert!(
            changed.contains("hello from shepherd"),
            "codex changed note.txt: {changed}"
        );

        let diagnostics = harness.diagnostics();
        eprintln!("command: {}", diagnostics.command);
        eprintln!("protocol: {}", diagnostics.protocol);
        harness.shutdown().await.expect("the first session stops");

        let mut resumed = start(&run, "resumed", repo).await;
        let same = resumed
            .open_session(Some(thread.clone()))
            .await
            .expect("the stored thread resumes");
        assert_eq!(same, thread, "resume must reopen the same thread");
        let second = drive_turn(
            &mut resumed,
            "Reply with the single word: resumed.",
            TURN_LIMIT,
        )
        .await;
        assert!(
            matches!(
                second.last(),
                Some(HarnessEvent::TurnFinished {
                    reason: TurnFinishReason::Completed
                })
            ),
            "the resumed turn must finish: {second:?}"
        );

        // A turn that only replies with text ends before there is anything to
        // interrupt, so wait for a command that is actually running.
        let mut attempts = 0;
        loop {
            attempts += 1;
            resumed
                .prompt(PromptInput::text(
                    "Run the shell command `sleep 25` and wait for it to finish.",
                ))
                .await
                .expect("the long turn starts");
            let mut running = false;
            let mut finished = None;
            loop {
                match next_event(&mut resumed, TURN_LIMIT).await {
                    HarnessEvent::ToolCall {
                        status: ToolStatus::Running,
                        ..
                    } => {
                        running = true;
                        break;
                    }
                    HarnessEvent::TurnFinished { reason } => {
                        finished = Some(reason);
                        break;
                    }
                    _ => {}
                }
            }
            if running {
                break;
            }
            assert!(
                attempts < 3,
                "codex never ran a command to cancel: {finished:?}"
            );
            eprintln!("cancel attempt {attempts} ended early: {finished:?}");
        }
        resumed.cancel().await.expect("the turn is interrupted");
        let finished = loop {
            let event = next_event(&mut resumed, Duration::from_secs(60)).await;
            if matches!(event, HarnessEvent::TurnFinished { .. }) {
                break event;
            }
        };
        assert!(
            matches!(
                finished,
                HarnessEvent::TurnFinished {
                    reason: TurnFinishReason::Cancelled | TurnFinishReason::Interrupted
                }
            ),
            "the cancel must end the turn: {finished:?}"
        );
        resumed.shutdown().await.expect("the session stops");
        run.finish(Some(changed))
            .expect("cache the verified responses");
    }

    /// The real CLI decides when to ask; under `workspace-write` network access
    /// needs escalation, so a fetch is the cheapest prompt that raises a card.
    #[tokio::test]
    #[ignore]
    async fn live_codex_requests_approval_for_a_network_command() {
        let Some(run) = LiveCodexRun::load("network-approval")
            .await
            .expect("load the live test cache")
        else {
            return;
        };

        let directory = TempDir::new().expect("a temp repo");
        let repo = directory.path();
        git(repo, &["init", "--quiet"]);
        let mut harness = start(&run, "approval", repo).await;
        harness
            .open_session(None)
            .await
            .expect("a new thread opens");
        harness
            .prompt(PromptInput::text("Run this shell command and report only the status code: curl -s -o /dev/null -w '%{http_code}' https://example.com"))
            .await
            .expect("the turn starts");

        let mut requested = None;
        let mut finished = None;
        loop {
            let event = match timeout(TURN_LIMIT, harness.events().recv()).await {
                Ok(Some(event)) => event,
                Ok(None) => break,
                Err(_) => panic!("codex sent no event within {}s", TURN_LIMIT.as_secs()),
            };
            match event {
                HarnessEvent::ApprovalRequested { request } => {
                    requested = Some(request);
                    break;
                }
                HarnessEvent::TurnFinished { reason } => {
                    finished = Some(reason);
                    break;
                }
                _ => {}
            }
        }

        let Some(request) = requested else {
            harness.shutdown().await.expect("the session stops");
            panic!("codex finished without asking for approval: {finished:?}");
        };
        eprintln!("approval: {} {:?}", request.title, request.arguments);

        harness
            .respond(request.approval_id.clone(), Decision::Deny)
            .await
            .expect("the request is answered");
        let mut resolved = false;
        let mut finished = None;
        loop {
            let event = match timeout(Duration::from_secs(60), harness.events().recv()).await {
                Ok(Some(event)) => event,
                Ok(None) => break,
                Err(_) => panic!("codex sent no event after the denial"),
            };
            match event {
                HarnessEvent::ApprovalResolved {
                    approval_id,
                    decision,
                    ..
                } => {
                    assert_eq!(approval_id.as_str(), request.approval_id.as_str());
                    assert_eq!(decision, Decision::Deny);
                    resolved = true;
                }
                HarnessEvent::TurnFinished { reason } => {
                    finished = Some(reason);
                    break;
                }
                _ => {}
            }
        }
        assert!(resolved, "the denial must be recorded");
        assert!(finished.is_some(), "the denied turn should finish");
        eprintln!("after the denial: {finished:?}");
        harness.shutdown().await.expect("the session stops");
        run.finish(None)
            .expect("cache the verified approval responses");
    }

    async fn start(run: &LiveCodexRun, session: &str, repo: &Path) -> CodexHarness {
        let mut harness = run.harness(session, false);
        harness
            .start(run.spec(repo.to_path_buf(), session, None))
            .await
            .expect("codex app-server starts");
        harness
    }

    fn git(repo: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(repo)
            .status()
            .expect("git runs");
        assert!(status.success(), "git {args:?} failed: {status}");
    }

    async fn next_event(harness: &mut CodexHarness, limit: Duration) -> HarnessEvent {
        loop {
            let event = match timeout(limit, harness.events().recv()).await {
                Ok(Some(event)) => event,
                Ok(None) => panic!("codex closed its event stream"),
                Err(_) => panic!("codex sent no event within {}s", limit.as_secs()),
            };
            if let HarnessEvent::ApprovalRequested { request } = &event {
                eprintln!(
                    "approval requested: {} ({})",
                    request.title,
                    request.approval_id.as_str()
                );
                harness
                    .respond(request.approval_id.clone(), Decision::Allow)
                    .await
                    .expect("the approval is answered");
                continue;
            }
            return event;
        }
    }

    async fn drive_turn(
        harness: &mut CodexHarness,
        text: &str,
        limit: Duration,
    ) -> Vec<HarnessEvent> {
        harness
            .prompt(PromptInput::text(text))
            .await
            .expect("the prompt starts a turn");
        let mut events = Vec::new();
        loop {
            let event = next_event(harness, limit).await;
            let finished = matches!(event, HarnessEvent::TurnFinished { .. });
            events.push(event);
            if finished {
                return events;
            }
        }
    }
}
