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
use std::pin::Pin;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
    ReadBuf,
};
use tokio::process::{Child, ChildStdout, Command};
use tokio::sync::{Mutex as AsyncMutex, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{Instant, timeout, timeout_at};

use crate::{
    AgentDiagnostics, AgentHarness, ApprovalId, ApprovalRequest, Capabilities, Decision, DeltaKind,
    FailureKind, FileDiff, HarnessError, HarnessEvent, HarnessResult, ItemId, PlanRow, PromptInput,
    SessionId, SessionState, SpawnSpec, ToolCallId, ToolStatus, TurnFinishReason, TurnId,
};

/// Protocol methods Shepherd knows how to drive. A build that advertises fewer
/// methods gets the intersection, so a control is never shown for a method the
/// server does not implement.
pub const SUPPORTED_METHODS: [&str; 7] = [
    "thread/start",
    "thread/resume",
    "turn/start",
    "turn/interrupt",
    "model/list",
    "item/commandExecution/requestApproval",
    "item/fileChange/requestApproval",
];

/// JSON-RPC code for an unsupported request. Answering with it keeps the
/// session alive instead of leaving the server waiting on Shepherd.
const METHOD_NOT_FOUND: i64 = -32601;
const MAX_PENDING_APPROVALS: usize = 64;
const MAX_OUTBOUND_MESSAGES: usize = 128;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);
const STDERR_TAIL_LINES: usize = 20;
const MAX_PROTOCOL_LINE_BYTES: usize = 8 * 1024 * 1024;
const MAX_PROBE_OUTPUT_BYTES: usize = 256 * 1024;
const EXEC_STREAM_CHUNKS: usize = 64;
const EXEC_STREAM_CHUNK_BYTES: usize = 8 * 1024;
const APP_STREAM_UNIT_BYTES: usize = 16 * 1024;
const APP_STREAM_CAPACITY_UNITS: usize = 4096;

#[derive(Default)]
struct ByteCounter(usize);

impl std::io::Write for ByteCounter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self.0.saturating_add(bytes.len());
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn reserve_app_stream<T: serde::Serialize + ?Sized>(
    capacity: &Arc<tokio::sync::Semaphore>,
    value: &T,
) -> HarnessResult<Arc<tokio::sync::OwnedSemaphorePermit>> {
    let mut bytes = ByteCounter::default();
    serde_json::to_writer(&mut bytes, value)
        .map_err(|_| HarnessError::Protocol("could not measure Codex stream event".into()))?;
    let units = u32::try_from(bytes.0.div_ceil(APP_STREAM_UNIT_BYTES).max(1))
        .map_err(|_| HarnessError::Protocol("Codex stream event exceeds queue capacity".into()))?;
    capacity
        .clone()
        .try_acquire_many_owned(units)
        .map(Arc::new)
        .map_err(|_| HarnessError::Protocol("Codex stream event queue exceeded its limit".into()))
}

async fn read_protocol_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
) -> std::io::Result<Option<String>> {
    let mut bytes = Vec::new();
    let count = reader
        .take((MAX_PROTOCOL_LINE_BYTES + 2) as u64)
        .read_until(b'\n', &mut bytes)
        .await?;
    if count == 0 {
        return Ok(None);
    }
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
    if bytes.len() > MAX_PROTOCOL_LINE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("Codex protocol message exceeds {MAX_PROTOCOL_LINE_BYTES} bytes"),
        ));
    }
    String::from_utf8(bytes).map(Some).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("Codex protocol message is not UTF-8: {error}"),
        )
    })
}

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
    let version = read_version(program, cwd).await;
    let handshake = run_handshake(program, cwd).await?;
    let images = if handshake.supported {
        true
    } else {
        let output = capture_probe_output(
            program,
            cwd,
            &["exec", "--help"],
            "exec help",
            PROBE_TIMEOUT,
        )
        .await?;
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

async fn read_version(program: &str, cwd: &Path) -> Option<String> {
    let output =
        match capture_probe_output(program, cwd, &["--version"], "version", PROBE_TIMEOUT).await {
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

async fn capture_probe_output(
    program: &str,
    cwd: &Path,
    args: &[&str],
    description: &str,
    deadline: Duration,
) -> HarnessResult<std::process::Output> {
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let mut child = ProbeProcess(
        command
            .spawn()
            .map_err(|error| spawn_error(program, error))?,
    );
    let stdout = child.0.stdout.take().ok_or_else(|| {
        HarnessError::Protocol(format!("Codex {description} stdout was not captured"))
    })?;
    let stderr = child.0.stderr.take().ok_or_else(|| {
        HarnessError::Protocol(format!("Codex {description} stderr was not captured"))
    })?;
    let capture = async {
        let (stdout, stderr) = tokio::try_join!(
            read_probe_stream(stdout, description, "stdout"),
            read_probe_stream(stderr, description, "stderr"),
        )?;
        let status = child.0.wait().await.map_err(|error| {
            HarnessError::Protocol(format!("Could not wait for Codex {description}: {error}"))
        })?;
        Ok(std::process::Output {
            status,
            stdout,
            stderr,
        })
    };
    timeout(deadline, capture)
        .await
        .map_err(|_| HarnessError::Protocol(format!("Codex {description} probe timed out")))?
}

async fn read_probe_stream(
    stream: impl AsyncRead + Unpin,
    description: &str,
    name: &str,
) -> HarnessResult<Vec<u8>> {
    let mut bytes = Vec::new();
    stream
        .take((MAX_PROBE_OUTPUT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| {
            HarnessError::Protocol(format!(
                "Could not read Codex {description} {name}: {error}"
            ))
        })?;
    if bytes.len() > MAX_PROBE_OUTPUT_BYTES {
        return Err(HarnessError::Protocol(format!(
            "Codex {description} {name} exceeds {MAX_PROBE_OUTPUT_BYTES} bytes"
        )));
    }
    Ok(bytes)
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
    #[cfg(unix)]
    command.process_group(0);

    let mut child = ProbeProcess(
        command
            .spawn()
            .map_err(|source| spawn_error(program, source))?,
    );
    let Some(mut stdin) = child.0.stdin.take() else {
        return Err(HarnessError::Protocol(
            "codex app-server stdin was not captured".to_string(),
        ));
    };
    let Some(stdout) = child.0.stdout.take() else {
        return Err(HarnessError::Protocol(
            "codex app-server stdout was not captured".to_string(),
        ));
    };
    let Some(stderr) = child.0.stderr.take() else {
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

    let mut lines = BufReader::new(stdout);
    let outcome = tokio::time::timeout(PROBE_TIMEOUT, async {
        for _ in 0..50 {
            let line = match read_protocol_line(&mut lines).await {
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

struct ProbeProcess(Child);

impl Drop for ProbeProcess {
    fn drop(&mut self) {
        if let Err(error) = stop_codex_process_group(&self.0) {
            tracing::warn!(%error, "could not stop the Codex probe process group");
        }
        if self.0.id().is_some()
            && let Err(error) = self.0.start_kill()
        {
            tracing::debug!(%error, "could not stop the Codex probe process");
        }
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
    let mut discovery_group = None;
    let result = async {
        harness.connect(spec, true).await?;
        discovery_group = harness
            .child
            .as_ref()
            .map(|child| AppServerStartupGroup(Some(Arc::downgrade(child))));
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
    if let Some(group) = &mut discovery_group {
        group.disarm();
    }
    let projects = result?;
    shutdown?;
    Ok(projects)
}

#[derive(Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexProjectModels {
    pub model: Option<String>,
    pub effort: Option<String>,
    pub models: Vec<CodexModel>,
}

#[derive(Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexModel {
    pub model: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    hidden: bool,
    #[serde(default)]
    is_default: bool,
    #[serde(default)]
    supported_reasoning_efforts: Vec<CodexEffort>,
    default_reasoning_effort: Option<String>,
}

#[derive(Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct CodexEffort {
    reasoning_effort: String,
    #[serde(default)]
    description: String,
}

pub async fn project_models(spec: SpawnSpec) -> HarnessResult<CodexProjectModels> {
    let hide_diagnostics = !spec.environment.is_empty();
    let mut harness = CodexHarness::new();
    let mut discovery_group = None;
    let result = async {
        let directory = spec.cwd.display().to_string();
        let capabilities = harness.start(spec).await?;
        discovery_group = harness.child.as_ref().map(|child| AppServerStartupGroup(Some(Arc::downgrade(child))));
        let config = harness
            .request("config/read", json!({ "cwd": directory }))
            .await;
        let mut settings = CodexProjectModels::default();
        match config {
            Ok(config) => {
                settings.model = config
                    .pointer("/config/model")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                settings.effort = config
                    .pointer("/config/model_reasoning_effort")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
            }
            Err(error) => tracing::debug!(error = %crate::protect_discovery_error(error, hide_diagnostics), "could not read Codex project defaults"),
        }
        if capabilities.model_picker {
            match harness.model_catalog().await {
                Ok(models) => settings.models = models,
                Err(error) => tracing::warn!(error = %crate::protect_discovery_error(error, hide_diagnostics), "could not list Codex models"),
            }
        }
        if settings.model.is_none() {
            settings.model = settings
                .models
                .iter()
                .find(|model| model.is_default)
                .map(|model| model.model.clone());
        }
        settings.models.retain(|model| !model.hidden);
        Ok::<_, HarnessError>(settings)
    }
    .await;
    let shutdown = harness.shutdown().await;
    if let Some(group) = &mut discovery_group {
        group.disarm();
    }
    let settings =
        result.map_err(|error| crate::protect_discovery_error(error, hide_diagnostics))?;
    shutdown.map_err(|error| crate::protect_discovery_error(error, hide_diagnostics))?;
    Ok(settings)
}

struct Pending {
    responses: HashMap<i64, oneshot::Sender<HarnessResult<Value>>>,
    start_request: Option<i64>,
    announced_turn: Option<String>,
    approvals: HashMap<String, i64>,
    approval_writes: BTreeSet<String>,
    thread_id: Option<String>,
    turn_id: Option<String>,
    turn_error: Option<HarnessEvent>,
    stream_capacity: Arc<tokio::sync::Semaphore>,
}

impl Default for Pending {
    fn default() -> Self {
        Self {
            responses: HashMap::new(),
            start_request: None,
            announced_turn: None,
            approvals: HashMap::new(),
            approval_writes: BTreeSet::new(),
            thread_id: None,
            turn_id: None,
            turn_error: None,
            stream_capacity: Arc::new(tokio::sync::Semaphore::new(APP_STREAM_CAPACITY_UNITS)),
        }
    }
}

#[derive(Debug)]
struct OutboundMessage {
    value: Value,
    written: Option<oneshot::Sender<std::io::Result<()>>>,
}

impl From<Value> for OutboundMessage {
    fn from(value: Value) -> Self {
        Self {
            value,
            written: None,
        }
    }
}

fn outbound_error(error: mpsc::error::TrySendError<OutboundMessage>) -> HarnessError {
    match error {
        mpsc::error::TrySendError::Full(_) => {
            HarnessError::Protocol("Codex outbound queue is full".into())
        }
        mpsc::error::TrySendError::Closed(_) => HarnessError::ChannelClosed,
    }
}

#[cfg(any(test, feature = "live-test-support"))]
use crate::live_codex::{WireCapture, WireEntry, without_api_credentials};

/// One `codex app-server` session.
pub struct CodexHarness {
    #[cfg(any(test, feature = "live-test-support"))]
    wire_capture: Option<WireCapture>,
    sender: mpsc::UnboundedSender<HarnessEvent>,
    receiver: mpsc::UnboundedReceiver<HarnessEvent>,
    outgoing: Option<mpsc::Sender<OutboundMessage>>,
    child: Option<Arc<AsyncMutex<Child>>>,
    process_tracker: Option<Arc<dyn crate::AgentProcessTracker>>,
    process_record: Option<String>,
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
    reasoning_effort: Option<String>,
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
            process_tracker: None,
            process_record: None,
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
            reasoning_effort: None,
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
            model_picker: self.supports("model/list") && self.supports("turn/start"),
            images: true,
            ..Capabilities::default()
        }
    }

    async fn model_catalog(&mut self) -> HarnessResult<Vec<CodexModel>> {
        let mut models = Vec::new();
        let mut cursor = None;
        let mut seen_cursors = BTreeSet::new();
        loop {
            let response = self
                .request("model/list", json!({ "cursor": cursor }))
                .await?;
            let page: Vec<CodexModel> =
                serde_json::from_value(response["data"].clone()).map_err(|error| {
                    HarnessError::Protocol(format!("invalid Codex model list: {error}"))
                })?;
            models.extend(page);
            let Some(next) = response.get("nextCursor").and_then(Value::as_str) else {
                return Ok(models);
            };
            if !seen_cursors.insert(next.to_owned()) {
                return Err(HarnessError::Protocol(
                    "Codex repeated a model list cursor".into(),
                ));
            }
            cursor = Some(next.to_owned());
        }
    }

    fn send(&self, message: Value) -> HarnessResult<()> {
        let outgoing = self.outgoing.as_ref().ok_or(HarnessError::NotStarted)?;
        outgoing.try_send(message.into()).map_err(outbound_error)
    }

    async fn request(&mut self, method: &str, params: Value) -> HarnessResult<Value> {
        let id = self.next_request_id;
        self.next_request_id += 1;
        let (sender, receiver) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            if self.closed.load(Ordering::Relaxed) {
                return Err(HarnessError::Protocol(format!(
                    "codex is no longer running; cannot call {method}"
                )));
            }
            pending.responses.insert(id, sender);
            if method == "turn/start" {
                pending.start_request = Some(id);
                pending.announced_turn = None;
            }
        }
        if let Err(error) = self.send(json!({ "method": method, "id": id, "params": params })) {
            let mut pending = self.pending.lock().await;
            pending.responses.remove(&id);
            if pending.start_request == Some(id) {
                pending.start_request = None;
            }
            return Err(error);
        }

        match timeout(REQUEST_TIMEOUT, receiver).await {
            Err(_) => {
                let mut pending = self.pending.lock().await;
                pending.responses.remove(&id);
                if pending.start_request == Some(id) {
                    pending.start_request = None;
                }
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

        #[cfg(unix)]
        command.process_group(0);

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
        let mut startup_group = AppServerStartupGroup(Some(Arc::downgrade(&child)));
        self.child = Some(child.clone());
        if let Some(tracker) = &self.process_tracker {
            let pid = child.lock().await.id().ok_or_else(|| {
                HarnessError::Protocol("Codex app-server has no process ID".into())
            })?;
            self.process_record = Some(tracker.started(pid).await.map_err(|error| {
                HarnessError::Protocol(format!(
                    "Could not record Codex app-server process: {error}"
                ))
            })?);
        }

        let (outgoing, outgoing_receiver) = mpsc::channel(MAX_OUTBOUND_MESSAGES);
        let (writer_failure, writer_failed) = oneshot::channel();
        self.writer_task = Some(spawn_writer(
            stdin,
            outgoing_receiver,
            Some(writer_failure),
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
            writer_failed,
            Arc::downgrade(&child),
            stop_codex_process_group,
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

        startup_group.disarm();
        Ok(self.capabilities())
    }
}

impl CodexHarness {
    async fn shutdown_with_signal(
        &mut self,
        signal_group: impl FnOnce(&Child) -> std::io::Result<()> + Send,
    ) -> HarnessResult<()> {
        self.shutdown_with_signals(signal_group, Child::start_kill)
            .await
    }

    async fn shutdown_with_signals(
        &mut self,
        signal_group: impl FnOnce(&Child) -> std::io::Result<()> + Send,
        signal_child: impl FnOnce(&mut Child) -> std::io::Result<()> + Send,
    ) -> HarnessResult<()> {
        if !self.started {
            return Ok(());
        }
        self.closing.store(true, Ordering::Relaxed);
        self.outgoing = None;

        let code = match self.child.as_ref() {
            Some(child) => {
                let mut child = child.lock().await;
                signal_group(&child).map_err(|error| {
                    HarnessError::Protocol(format!(
                        "could not stop the Codex app-server process group: {error}"
                    ))
                })?;
                signal_child(&mut child).map_err(|error| {
                    HarnessError::Protocol(format!(
                        "could not stop the Codex app-server process: {error}"
                    ))
                })?;
                child
                    .wait()
                    .await
                    .map_err(|error| {
                        HarnessError::Protocol(format!(
                            "could not stop the Codex app-server process: {error}"
                        ))
                    })?
                    .code()
            }
            None => None,
        };
        self.child = None;

        if let (Some(tracker), Some(record)) = (&self.process_tracker, &self.process_record) {
            tracker.stopped(record).await.map_err(|error| {
                HarnessError::Protocol(format!(
                    "Could not clear Codex app-server process record: {error}"
                ))
            })?;
            self.process_record = None;
        }

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
        {
            let mut pending = self.pending.lock().await;
            pending.approvals.clear();
            pending.approval_writes.clear();
        }

        self.sender
            .send(HarnessEvent::Exited {
                code,
                stderr: stderr_tail(&self.stderr),
            })
            .map_err(|_| HarnessError::ChannelClosed)?;
        Ok(())
    }
}

impl Default for CodexHarness {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentHarness for CodexHarness {
    fn set_process_tracker(&mut self, tracker: Arc<dyn crate::AgentProcessTracker>) {
        self.process_tracker = Some(tracker);
    }

    fn events(&mut self) -> &mut mpsc::UnboundedReceiver<HarnessEvent> {
        &mut self.receiver
    }

    fn diagnostics(&self) -> AgentDiagnostics {
        AgentDiagnostics {
            command: self.command.clone(),
            protocol: self.protocol.clone(),
            fallback_reason: None,
            model: self.model.clone(),
            reasoning_effort: self.reasoning_effort.clone(),
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
                self.request(
                    "thread/start",
                    json!({
                        "cwd": cwd,
                        "approvalPolicy": "on-request",
                        "sandbox": "workspace-write",
                    }),
                )
                .await?
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
        self.reasoning_effort = response
            .get("reasoningEffort")
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
        if input.codex != Default::default() && !self.capabilities().model_picker {
            return Err(HarnessError::Protocol(
                "This Codex session does not support model or effort selection.".into(),
            ));
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
        let mut params = json!({
                    "threadId": session_id.as_str(),
                    "input": items,
                    "cwd": cwd,
                    "approvalPolicy": "on-request",
                    "sandboxPolicy": {
                        "type": "workspaceWrite",
                        "writableRoots": [cwd],
                        "networkAccess": false,
                    },
        });
        if let Some(model) = &input.codex.model {
            params["model"] = json!(model);
        }
        if let Some(effort) = &input.codex.effort {
            params["effort"] = json!(effort);
        }
        let response = self.request("turn/start", params).await?;

        read_turn_id(&response).ok_or_else(|| {
            HarnessError::Protocol("the turn response did not include turn.id".to_string())
        })?;
        self.model = input.codex.model.or(self.model.take());
        self.reasoning_effort = input.codex.effort.or(self.reasoning_effort.take());
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
        let outgoing = self.outgoing.as_ref().ok_or(HarnessError::NotStarted)?;
        let request_id = {
            let mut pending = self.pending.lock().await;
            let request_id = pending
                .approvals
                .get(approval_id.as_str())
                .copied()
                .ok_or(HarnessError::NoPendingApproval)?;
            if !pending
                .approval_writes
                .insert(approval_id.as_str().to_owned())
            {
                return Err(HarnessError::Protocol("the Codex approval response may already have been sent; wait for the agent or restart the session".into()));
            }
            request_id
        };
        let (written, completed) = oneshot::channel();
        if let Err(error) = outgoing.try_send(OutboundMessage {
            value: json!({ "id": request_id, "result": { "decision": answer } }),
            written: Some(written),
        }) {
            self.pending
                .lock()
                .await
                .approval_writes
                .remove(approval_id.as_str());
            return Err(outbound_error(error));
        }
        timeout(REQUEST_TIMEOUT, completed)
            .await
            .map_err(|_| {
                HarnessError::Protocol(
                    "timed out writing the Codex approval response; delivery is unknown".into(),
                )
            })?
            .map_err(|_| {
                HarnessError::TransportClosed(
                    "Codex writer stopped before confirming the approval response".into(),
                )
            })?
            .map_err(|error| {
                HarnessError::TransportClosed(format!(
                    "could not write the Codex approval response: {error}"
                ))
            })?;
        {
            let mut pending = self.pending.lock().await;
            pending.approval_writes.remove(approval_id.as_str());
            if pending.approvals.get(approval_id.as_str()) != Some(&request_id) {
                return Err(HarnessError::NoPendingApproval);
            }
            pending.approvals.remove(approval_id.as_str());
        }
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
        self.shutdown_with_signal(stop_codex_process_group).await
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
    stream_capacity: Arc<tokio::sync::Semaphore>,
    program: String,
    command: String,
    version: Option<String>,
    images: bool,
    args: Vec<String>,
    cwd: PathBuf,
    environment: BTreeMap<String, String>,
    stderr: Arc<Mutex<VecDeque<String>>>,
    stderr_task: Option<JoinHandle<()>>,
    process_tracker: Option<Arc<dyn crate::AgentProcessTracker>>,
    run: Option<ExecRun>,
    session_id: Option<SessionId>,
    next_turn: u64,
    started: bool,
}

/// One `codex exec --json` process. The waiter owns the child, so a cancel and
/// a natural exit cannot both reap the same process.
struct ExecRun {
    waiter: JoinHandle<()>,
    waiter_failure: Option<String>,
    reader: tokio::task::AbortHandle,
    kill: Option<mpsc::UnboundedSender<oneshot::Sender<Result<(), String>>>>,
    wait_errors: mpsc::UnboundedReceiver<String>,
    process_record: ExecProcessRecord,
    stopping: bool,
    finished: Arc<AtomicBool>,
}

type ExecProcessRecord = Arc<AsyncMutex<Option<(Arc<dyn crate::AgentProcessTracker>, String)>>>;

async fn clear_exec_process_record(record: &ExecProcessRecord) -> HarnessResult<()> {
    let mut record = record.lock().await;
    if let Some((tracker, id)) = &*record {
        tracker.stopped(id).await.map_err(|error| {
            HarnessError::Protocol(format!(
                "Could not clear Codex exec process record: {error}"
            ))
        })?;
        *record = None;
    }
    Ok(())
}

static EXEC_SESSION_COUNTER: AtomicU64 = AtomicU64::new(1);

struct ExecStartupGroup(#[cfg(unix)] Option<libc::pid_t>);

impl ExecStartupGroup {
    fn new(child: &Child) -> Self {
        #[cfg(unix)]
        {
            Self(child.id().and_then(|id| libc::pid_t::try_from(id).ok()))
        }
        #[cfg(not(unix))]
        {
            let _child = child;
            Self()
        }
    }
    fn disarm(&mut self) {
        #[cfg(unix)]
        {
            self.0 = None;
        }
    }
}

#[cfg(unix)]
impl Drop for ExecStartupGroup {
    fn drop(&mut self) {
        if let Some(group) = self.0.take()
            && unsafe { libc::killpg(group, libc::SIGKILL) } != 0
        {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                tracing::warn!(%error, "could not stop Codex exec during startup");
            }
        }
    }
}

impl ExecHarness {
    async fn shutdown_with_timeout(&mut self, deadline: Duration) -> HarnessResult<()> {
        if !self.started {
            return Ok(());
        }
        if let Some(run) = self.run.as_mut() {
            if let Some(error) = &run.waiter_failure {
                return Err(HarnessError::Protocol(error.clone()));
            }
            let deadline = Instant::now() + deadline;
            if let Ok(error) = run.wait_errors.try_recv() {
                run.stopping = false;
                return Err(HarnessError::Protocol(error));
            }
            if !run.finished.load(Ordering::Relaxed)
                && !run.stopping
                && let Some(kill) = &run.kill
            {
                let (reply, result) = oneshot::channel();
                if kill.send(reply).is_ok() {
                    match timeout_at(deadline, result).await {
                        Ok(Ok(Ok(()))) => run.stopping = true,
                        Ok(Ok(Err(error))) => return Err(HarnessError::Protocol(error)),
                        Ok(Err(_)) => {}
                        Err(_) => {
                            return Err(HarnessError::Protocol(
                                "Codex exec did not stop before the shutdown deadline".into(),
                            ));
                        }
                    }
                }
            }
            let outcome = tokio::select! {
                biased;
                Some(error) = run.wait_errors.recv() => {
                    run.stopping = false;
                    return Err(HarnessError::Protocol(error));
                }
                outcome = timeout_at(deadline, &mut run.waiter) => outcome,
            };
            match outcome {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    let message = format!("Codex exec shutdown failed: {error}");
                    run.waiter_failure = Some(message.clone());
                    return Err(HarnessError::Protocol(message));
                }
                Err(_) => {
                    return Err(HarnessError::Protocol(
                        "Codex exec did not stop before the shutdown deadline".into(),
                    ));
                }
            }
            clear_exec_process_record(&run.process_record).await?;
        }
        if let Some(run) = self.run.take() {
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

    pub fn new() -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        Self {
            sender,
            receiver,
            stream_capacity: Arc::new(tokio::sync::Semaphore::new(EXEC_STREAM_CHUNKS)),
            program: String::new(),
            command: String::new(),
            version: None,
            images: false,
            args: Vec::new(),
            cwd: PathBuf::new(),
            environment: BTreeMap::new(),
            stderr: Arc::new(Mutex::new(VecDeque::new())),
            stderr_task: None,
            process_tracker: None,
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
    fn set_process_tracker(&mut self, tracker: Arc<dyn crate::AgentProcessTracker>) {
        self.process_tracker = Some(tracker);
    }

    fn events(&mut self) -> &mut mpsc::UnboundedReceiver<HarnessEvent> {
        &mut self.receiver
    }

    fn diagnostics(&self) -> AgentDiagnostics {
        AgentDiagnostics {
            command: self.command.clone(),
            protocol: self.version.as_ref().map_or_else(|| "codex exec --json".into(), |version| format!("codex exec --json ({version})")),
            fallback_reason: Some("Codex app-server did not complete the initialize handshake. Each prompt starts a separate Codex process.".into()),
            model: None,
            reasoning_effort: None,
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
        if input.codex != Default::default() {
            return Err(HarnessError::Protocol(
                "This Codex build does not support model or effort selection through Shepherd."
                    .into(),
            ));
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
        if let Some(run) = &self.run {
            clear_exec_process_record(&run.process_record).await?;
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
        let mut startup_group = ExecStartupGroup::new(&child);
        let process_record: ExecProcessRecord = Arc::new(AsyncMutex::new(None));
        if let Some(tracker) = &self.process_tracker {
            let pid = child
                .id()
                .ok_or_else(|| HarnessError::Protocol("Codex exec has no process ID".into()))?;
            let id = tracker.started(pid).await.map_err(|error| {
                HarnessError::Protocol(format!("Could not record Codex exec process: {error}"))
            })?;
            *process_record.lock().await = Some((tracker.clone(), id));
        }
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
        let (kill, kill_receiver) = mpsc::unbounded_channel();
        let (wait_errors, wait_error_receiver) = mpsc::unbounded_channel();
        let finished = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(AtomicU64::new(0));
        let backpressured = Arc::new(AtomicBool::new(false));
        let reader = spawn_exec_reader(
            stdout,
            self.sender.clone(),
            progress.clone(),
            backpressured.clone(),
            self.stream_capacity.clone(),
        );
        let reader_abort = reader.abort_handle();
        let waiter = spawn_exec_waiter(
            ExecWaiter {
                child,
                kill: kill_receiver,
                sender: self.sender.clone(),
                stderr: Arc::clone(&self.stderr),
                stderr_drained,
                finished: Arc::clone(&finished),
                reader,
                progress,
                backpressured,
                wait_errors,
                process_record: process_record.clone(),
            },
            stop_exec_child,
            || None,
        );
        self.run = Some(ExecRun {
            waiter,
            waiter_failure: None,
            reader: reader_abort,
            kill: Some(kill),
            wait_errors: wait_error_receiver,
            process_record,
            stopping: false,
            finished,
        });
        startup_group.disarm();

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
        if let Ok(error) = run.wait_errors.try_recv() {
            run.stopping = false;
            return Err(HarnessError::Protocol(error));
        }
        if run.stopping {
            return Err(HarnessError::Protocol(
                "the codex exec turn is already stopping".to_string(),
            ));
        }
        let kill = run.kill.as_ref().ok_or_else(|| {
            HarnessError::Protocol("the codex exec process already exited".into())
        })?;
        let (reply, result) = oneshot::channel();
        kill.send(reply).map_err(|_| {
            HarnessError::Protocol("the codex exec process already exited".to_string())
        })?;
        result
            .await
            .map_err(|_| {
                HarnessError::Protocol(
                    "the codex exec process stopped before confirming cancellation".into(),
                )
            })?
            .map_err(HarnessError::Protocol)?;
        run.stopping = true;
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
        self.shutdown_with_timeout(Duration::from_secs(5)).await
    }
}

struct ExecWaiter {
    child: Child,
    kill: mpsc::UnboundedReceiver<oneshot::Sender<Result<(), String>>>,
    sender: mpsc::UnboundedSender<HarnessEvent>,
    stderr: Arc<Mutex<VecDeque<String>>>,
    stderr_drained: oneshot::Receiver<()>,
    finished: Arc<AtomicBool>,
    reader: JoinHandle<Result<(), String>>,
    progress: Arc<AtomicU64>,
    backpressured: Arc<AtomicBool>,
    wait_errors: mpsc::UnboundedSender<String>,
    process_record: ExecProcessRecord,
}

fn spawn_exec_waiter(
    waiter: ExecWaiter,
    mut stop: impl FnMut(&mut Child) -> Result<(), String> + Send + 'static,
    mut wait_failure: impl FnMut() -> Option<std::io::Error> + Send + 'static,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let ExecWaiter {
            mut child,
            mut kill,
            sender,
            stderr,
            stderr_drained,
            finished,
            mut reader,
            progress,
            backpressured,
            wait_errors,
            process_record,
        } = waiter;
        let mut reader_finished = false;
        let mut protocol_error = None;
        let mut cancelled = false;
        let mut kill_open = true;
        let mut wait_enabled = true;
        let mut stop_requested = false;
        let status = loop {
            tokio::select! {
                status = async {
                    match wait_failure() {
                        Some(error) => Err(error),
                        None => child.wait().await,
                    }
                }, if wait_enabled => match status {
                    Ok(status) => break status,
                    Err(error) => {
                        let text = format!("Could not wait for Codex exec to exit: {error}");
                        if sender.send(HarnessEvent::TurnError { kind: FailureKind::Protocol, text: text.clone(), stream_permit: None }).is_err() {
                            tracing::debug!("the core stopped listening to codex errors");
                        }
                        if wait_errors.send(text).is_err() {
                            tracing::debug!("the Codex exec shutdown receiver disconnected");
                        }
                        wait_enabled = false;
                    }
                },
                request = kill.recv(), if kill_open => {
                    let result = if stop_requested { Ok(()) } else { stop(&mut child) };
                    if result.is_ok() && protocol_error.is_none() {
                        cancelled = true;
                    }
                    if result.is_ok() {
                        stop_requested = true;
                        wait_enabled = true;
                    }
                    match request {
                        Some(reply) => {
                            if reply.send(result).is_err() {
                                tracing::debug!("the codex exec stop requester disconnected");
                            }
                        }
                        None => {
                            kill_open = false;
                            if let Err(error) = result {
                                tracing::warn!(%error, "could not stop the abandoned Codex exec process");
                                if let Err(error) = child.start_kill() {
                                    tracing::warn!(%error, "could not stop the abandoned Codex exec child");
                                }
                            }
                        }
                    }
                }
                result = &mut reader, if !reader_finished => {
                    reader_finished = true;
                    let error = match result {
                        Ok(Ok(())) => None,
                        Ok(Err(error)) => Some(error),
                        Err(error) => Some(format!("Codex exec reader failed: {error}")),
                    };
                    if let Some(error) = error {
                        protocol_error = Some(error);
                        if let Err(error) = stop(&mut child) {
                            let text = format!("Could not stop Codex exec after an output failure: {error}");
                            if sender.send(HarnessEvent::TurnError { kind: FailureKind::Protocol, text, stream_permit: None }).is_err() {
                                tracing::debug!("the core stopped listening to codex errors");
                            }
                        } else {
                            stop_requested = true;
                        }
                    }
                }
            }
            if !kill_open && !wait_enabled {
                tracing::warn!("could not wait for the abandoned Codex exec process");
                reader.abort();
                return;
            }
        };

        if !reader_finished {
            match drain_exec_reader(
                &mut reader,
                &progress,
                &backpressured,
                &mut kill,
                cancelled,
                Duration::from_secs(1),
                Duration::from_secs(10),
            )
            .await
            {
                Ok(interrupted) => cancelled |= interrupted,
                Err(error) if !cancelled => protocol_error = Some(error),
                Err(error) => {
                    tracing::debug!(%error, "Codex exec output stopped during cancellation")
                }
            }
        }
        let code = status.code();
        let reason = if cancelled {
            TurnFinishReason::Cancelled
        } else if protocol_error.is_none() && status.success() {
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
            let explanation =
                protocol_error.unwrap_or_else(|| format!("Codex exec failed ({status})"));
            let text = format!(
                "{explanation}{suffix}",
                suffix = if stderr.is_empty() {
                    String::new()
                } else {
                    format!(": {stderr}")
                }
            );
            let kind = FailureKind::classify(&HarnessError::Protocol(text.clone()))
                .unwrap_or(FailureKind::Protocol);
            if sender
                .send(HarnessEvent::TurnError {
                    kind,
                    text,
                    stream_permit: None,
                })
                .is_err()
            {
                tracing::debug!("the core stopped listening to codex errors");
            }
        }
        if let Err(error) = clear_exec_process_record(&process_record).await
            && sender
                .send(HarnessEvent::TurnError {
                    kind: FailureKind::Protocol,
                    text: error.to_string(),
                    stream_permit: None,
                })
                .is_err()
        {
            tracing::debug!("the core stopped listening to codex errors");
        }
        finished.store(true, Ordering::Relaxed);
        if sender.send(HarnessEvent::TurnFinished { reason }).is_err() {
            tracing::debug!("the core stopped listening to codex events");
        }
    })
}

async fn drain_exec_reader(
    reader: &mut JoinHandle<Result<(), String>>,
    progress: &AtomicU64,
    backpressured: &AtomicBool,
    kill: &mut mpsc::UnboundedReceiver<oneshot::Sender<Result<(), String>>>,
    cancelled: bool,
    idle_timeout: Duration,
    total_timeout: Duration,
) -> Result<bool, String> {
    let mut deadline = Instant::now() + total_timeout;
    loop {
        let previous = progress.load(Ordering::Relaxed);
        let started = Instant::now();
        let next_check = (Instant::now() + idle_timeout).min(deadline);
        let result = tokio::select! {
            result = timeout_at(next_check, &mut *reader) => result,
            Some(reply) = kill.recv(), if !cancelled => {
                reader.abort();
                if reply.send(Ok(())).is_err() {
                    tracing::debug!("the codex exec stop requester disconnected");
                }
                return Ok(true);
            }
        };
        match result {
            Ok(Ok(result)) => return result.map(|()| false),
            Ok(Err(error)) => return Err(format!("Codex exec reader failed: {error}")),
            Err(_) if !cancelled && backpressured.load(Ordering::Acquire) => {
                let now = Instant::now();
                deadline =
                    (deadline + now.saturating_duration_since(started)).max(now + idle_timeout);
                continue;
            }
            Err(_) if Instant::now() >= deadline => {
                reader.abort();
                return Err("Codex exec output exceeded the post-exit drain limit".into());
            }
            Err(_) if !cancelled && progress.load(Ordering::Relaxed) != previous => continue,
            Err(_) => {
                reader.abort();
                return Err("Codex exec output did not close after the process exited".into());
            }
        }
    }
}

fn stop_exec_child(child: &mut Child) -> Result<(), String> {
    stop_exec_child_with(child, stop_codex_process_group, Child::start_kill)
}

fn stop_exec_child_with(
    child: &mut Child,
    stop_group: impl FnOnce(&Child) -> std::io::Result<()>,
    stop_child: impl FnOnce(&mut Child) -> std::io::Result<()>,
) -> Result<(), String> {
    stop_group(child)
        .map_err(|error| format!("Could not stop the Codex exec process group: {error}"))?;
    stop_child(child).map_err(|error| format!("Could not stop the Codex exec process: {error}"))
}

fn spawn_exec_reader(
    stdout: ChildStdout,
    sender: mpsc::UnboundedSender<HarnessEvent>,
    progress: Arc<AtomicU64>,
    backpressured: Arc<AtomicBool>,
    capacity: Arc<tokio::sync::Semaphore>,
) -> JoinHandle<Result<(), String>> {
    tokio::spawn(async move {
        let mut lines = BufReader::new(ProgressReader {
            inner: stdout,
            progress: progress.clone(),
        });
        loop {
            let line = match read_protocol_line(&mut lines).await {
                Ok(Some(line)) => line,
                Ok(None) => break,
                Err(error) => return Err(format!("Could not read Codex exec stdout: {error}")),
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
                if publish_exec_event(&sender, &capacity, &backpressured, &progress, event)
                    .await
                    .is_err()
                {
                    tracing::debug!("the core stopped listening to codex events");
                    return Ok(());
                }
            }
        }
        Ok(())
    })
}

async fn publish_exec_event(
    sender: &mpsc::UnboundedSender<HarnessEvent>,
    capacity: &Arc<tokio::sync::Semaphore>,
    backpressured: &AtomicBool,
    progress: &AtomicU64,
    mut event: HarnessEvent,
) -> Result<(), ()> {
    if let HarnessEvent::ToolCall { stream_permit, .. }
    | HarnessEvent::Plan { stream_permit, .. }
    | HarnessEvent::Usage { stream_permit, .. } = &mut event
    {
        backpressured.store(true, Ordering::Release);
        let permit = capacity.clone().acquire_owned().await.map_err(|_| ())?;
        backpressured.store(false, Ordering::Release);
        *stream_permit = Some(Arc::new(permit));
        sender.send(event).map_err(|_| ())?;
        progress.fetch_add(1, Ordering::Relaxed);
        return Ok(());
    }
    let HarnessEvent::Delta {
        item_id,
        kind,
        text,
        ..
    } = event
    else {
        return sender.send(event).map_err(|_| ());
    };
    if text.is_empty() {
        backpressured.store(true, Ordering::Release);
        let permit = capacity.clone().acquire_owned().await.map_err(|_| ())?;
        backpressured.store(false, Ordering::Release);
        sender
            .send(HarnessEvent::Delta {
                item_id,
                kind,
                text,
                stream_permit: Some(Arc::new(permit)),
            })
            .map_err(|_| ())?;
        progress.fetch_add(1, Ordering::Relaxed);
        return Ok(());
    }
    let mut remaining = text.as_str();
    while !remaining.is_empty() {
        let end = remaining.floor_char_boundary(remaining.len().min(EXEC_STREAM_CHUNK_BYTES));
        let (chunk, rest) = remaining.split_at(end);
        backpressured.store(true, Ordering::Release);
        let permit = capacity.clone().acquire_owned().await.map_err(|_| ())?;
        backpressured.store(false, Ordering::Release);
        sender
            .send(HarnessEvent::Delta {
                item_id: item_id.clone(),
                kind,
                text: chunk.to_owned(),
                stream_permit: Some(Arc::new(permit)),
            })
            .map_err(|_| ())?;
        progress.fetch_add(1, Ordering::Relaxed);
        remaining = rest;
    }
    Ok(())
}

struct ProgressReader<R> {
    inner: R,
    progress: Arc<AtomicU64>,
}

impl<R: AsyncRead + Unpin> AsyncRead for ProgressReader<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buffer.filled().len();
        match Pin::new(&mut self.inner).poll_read(cx, buffer) {
            Poll::Ready(Ok(())) => {
                if buffer.filled().len() > before {
                    self.progress.fetch_add(1, Ordering::Relaxed);
                }
                Poll::Ready(Ok(()))
            }
            other => other,
        }
    }
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
            tracing::warn!(kind, "codex exec reported an error");
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
        stream_permit: None,
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
        let output = exec_command_output_text(event).map(crate::bounded_command_output);
        (status, output)
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
    exec_command_output_text(event).map(str::to_string)
}

fn exec_command_output_text(event: &Value) -> Option<&str> {
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
        stream_permit: None,
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
        Some(HarnessEvent::Plan {
            rows,
            stream_permit: None,
        })
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
        stream_permit: None,
    }
}

fn spawn_writer(
    mut stdin: impl AsyncWrite + Unpin + Send + 'static,
    mut outgoing: mpsc::Receiver<OutboundMessage>,
    mut failure: Option<oneshot::Sender<()>>,
    #[cfg(any(test, feature = "live-test-support"))] capture: Option<WireCapture>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(OutboundMessage {
            value: message,
            written,
        }) = outgoing.recv().await
        {
            #[cfg(any(test, feature = "live-test-support"))]
            if let Some(capture) = &capture {
                capture.push(WireEntry::Request(message.clone()));
            }
            let mut line = match serde_json::to_string(&message) {
                Ok(line) => line,
                Err(error) => {
                    tracing::error!(%error, "could not encode a codex message");
                    if let Some(failure) = failure.take()
                        && failure.send(()).is_err()
                    {
                        tracing::debug!("codex writer supervisor disconnected");
                    }
                    if let Some(written) = written
                        && written.send(Err(std::io::Error::other(error))).is_err()
                    {
                        tracing::debug!("codex writer confirmation receiver dropped");
                    }
                    return;
                }
            };
            line.push('\n');
            if let Err(error) = stdin.write_all(line.as_bytes()).await {
                tracing::debug!(%error, "codex stopped accepting input");
                if let Some(failure) = failure.take()
                    && failure.send(()).is_err()
                {
                    tracing::debug!("codex writer supervisor disconnected");
                }
                if let Some(written) = written
                    && written.send(Err(error)).is_err()
                {
                    tracing::debug!("codex writer confirmation receiver dropped");
                }
                return;
            }
            if let Err(error) = stdin.flush().await {
                tracing::debug!(%error, "could not flush codex input");
                if let Some(failure) = failure.take()
                    && failure.send(()).is_err()
                {
                    tracing::debug!("codex writer supervisor disconnected");
                }
                if let Some(written) = written
                    && written.send(Err(error)).is_err()
                {
                    tracing::debug!("codex writer confirmation receiver dropped");
                }
                return;
            }
            if let Some(written) = written
                && written.send(Ok(())).is_err()
            {
                tracing::debug!("codex writer confirmation receiver dropped");
            }
        }
    })
}

#[allow(
    clippy::too_many_arguments,
    reason = "the reader supervises these shared resources"
)]
fn spawn_reader(
    stdout: ChildStdout,
    sender: mpsc::UnboundedSender<HarnessEvent>,
    pending: Arc<AsyncMutex<Pending>>,
    outgoing: mpsc::Sender<OutboundMessage>,
    stderr: Arc<Mutex<VecDeque<String>>>,
    closed: Arc<AtomicBool>,
    closing: Arc<AtomicBool>,
    stderr_drained: oneshot::Receiver<()>,
    mut writer_failed: oneshot::Receiver<()>,
    child: std::sync::Weak<AsyncMutex<Child>>,
    stop_group: fn(&Child) -> std::io::Result<()>,
    #[cfg(any(test, feature = "live-test-support"))] capture: Option<WireCapture>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout);
        let protocol_failure = loop {
            let result = tokio::select! {
                result = read_protocol_line(&mut lines) => result,
                result = &mut writer_failed => {
                    break Some(match result {
                        Ok(()) => "Codex stopped accepting protocol input.",
                        Err(_) => "The Codex protocol writer stopped unexpectedly.",
                    }.into());
                }
            };
            let line = match result {
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
            if let Err(error) = route_incoming(message, &sender, &pending, &outgoing).await {
                break Some(format!("Could not answer Codex request: {error}"));
            }
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
                Err(stop_failed_app_server(&mut child, stop_group, message).await)
            } else {
                match timeout(Duration::from_millis(250), child.wait()).await {
                    Ok(Ok(status)) => Ok(status.code()),
                    Ok(Err(error)) => Err(format!("Could not read the Codex exit status: {error}")),
                    Err(_) => Err(stop_failed_app_server(
                        &mut child,
                        stop_group,
                        "Codex closed its protocol stream while still running.".into(),
                    )
                    .await),
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

async fn stop_failed_app_server(
    child: &mut Child,
    stop_group: fn(&Child) -> std::io::Result<()>,
    message: String,
) -> String {
    if let Err(error) = stop_group(child) {
        return format!("{message} Could not stop the Codex process group: {error}");
    }
    if let Err(error) = child.kill().await {
        return format!("{message} Could not stop the Codex process: {error}");
    }
    message
}

struct AppServerStartupGroup(Option<std::sync::Weak<AsyncMutex<Child>>>);

impl AppServerStartupGroup {
    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for AppServerStartupGroup {
    fn drop(&mut self) {
        if let Some(child) = self.0.take().and_then(|child| child.upgrade()) {
            match child.try_lock() {
                Ok(mut child) => {
                    if let Err(error) = stop_codex_process_group(&child) {
                        tracing::warn!(%error, "could not stop the Codex startup process group");
                    }
                    if let Err(error) = child.start_kill() {
                        tracing::debug!(%error, "could not stop Codex during startup");
                    }
                }
                Err(_) => tracing::debug!("Codex startup process is being stopped by its reader"),
            }
        }
    }
}

fn stop_codex_process_group(child: &Child) -> std::io::Result<()> {
    #[cfg(unix)]
    if let Some(group) = child.id().and_then(|id| libc::pid_t::try_from(id).ok())
        && unsafe { libc::killpg(group, libc::SIGKILL) } != 0
    {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(error);
        }
    }
    #[cfg(not(unix))]
    let _child = child;
    Ok(())
}

fn spawn_stderr_reader(
    mut stderr: impl AsyncRead + Unpin + Send + 'static,
    tail: Arc<Mutex<VecDeque<String>>>,
    drained: Option<oneshot::Sender<()>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut lines = acp_client::bounded_lines::BoundedLines::default();
        let mut bytes = [0u8; 4096];
        let record = |line: acp_client::bounded_lines::BoundedLine| match tail.lock() {
            Ok(mut tail) => {
                if tail.len() == STDERR_TAIL_LINES {
                    tail.pop_front();
                }
                tail.push_back(line.text);
            }
            Err(poisoned) => {
                let mut tail = poisoned.into_inner();
                if tail.len() == STDERR_TAIL_LINES {
                    tail.pop_front();
                }
                tail.push_back(line.text);
            }
        };
        loop {
            match stderr.read(&mut bytes).await {
                Ok(0) => break,
                Ok(count) => {
                    for line in lines.push(&bytes[..count]) {
                        record(line);
                    }
                }
                Err(error) => {
                    tracing::debug!(%error, "could not read Codex stderr");
                    break;
                }
            }
        }
        if let Some(line) = lines.finish() {
            record(line);
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
    outgoing: &mpsc::Sender<OutboundMessage>,
) -> HarnessResult<()> {
    match (message.get("id"), message.get("method")) {
        (Some(id), Some(method)) => {
            let method = method.as_str().unwrap_or_default().to_string();
            let params = message.get("params").cloned().unwrap_or(Value::Null);
            handle_server_request(id.clone(), method, params, sender, pending, outgoing).await?;
        }
        (Some(id), None) => {
            let Some(request_id) = id.as_i64() else {
                tracing::warn!("codex answered with a non-numeric id");
                return Ok(());
            };
            let mut state = pending.lock().await;
            let waiter = state.responses.remove(&request_id);
            let Some(waiter) = waiter else {
                tracing::debug!("codex answered a request nobody is waiting for");
                return Ok(());
            };
            let outcome = match message.get("error") {
                Some(error) => Err(codex_rpc_error(error)),
                None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
            };
            if state.start_request == Some(request_id) {
                if let Ok(response) = &outcome
                    && let Some(turn_id) = read_turn_id(response)
                {
                    announce_app_turn(&mut state, turn_id, sender)?;
                }
                state.start_request = None;
            }
            drop(state);
            if waiter.send(outcome).is_err() {
                tracing::debug!("the answer arrived after its caller gave up");
            }
        }
        (None, Some(method)) => {
            let method = method.as_str().unwrap_or_default().to_string();
            let params = message.get("params").cloned().unwrap_or(Value::Null);
            handle_notification(&method, &params, sender, pending).await?;
        }
        (None, None) => tracing::warn!("codex sent a message with neither id nor method"),
    }
    Ok(())
}

fn announce_app_turn(
    pending: &mut Pending,
    turn_id: String,
    sender: &mpsc::UnboundedSender<HarnessEvent>,
) -> HarnessResult<()> {
    if let Some(announced) = &pending.announced_turn {
        if announced != &turn_id {
            return Err(HarnessError::Protocol(
                "Codex reported conflicting turn IDs.".into(),
            ));
        }
        return Ok(());
    }
    pending.turn_id = Some(turn_id.clone());
    pending.announced_turn = Some(turn_id.clone());
    sender
        .send(HarnessEvent::TurnStarted {
            turn_id: TurnId::new(turn_id),
        })
        .map_err(|_| HarnessError::ChannelClosed)
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
    outgoing: &mpsc::Sender<OutboundMessage>,
) -> HarnessResult<()> {
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
                "message": "shepherd does not support this request",
            },
        });
        outgoing.try_send(response.into()).map_err(outbound_error)?;
        return Ok(());
    }

    let Some(server_id) = id.as_i64() else {
        tracing::warn!(%method, "an approval arrived with a non-numeric id");
        return Ok(());
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

    let mut approvals = pending.lock().await;
    if approvals
        .approvals
        .values()
        .any(|existing| *existing == server_id)
    {
        tracing::debug!("codex repeated a pending server request ID");
        return Ok(());
    }
    let rejection = if approvals.approvals.contains_key(&approval_id) {
        Some("an approval for this item is already pending")
    } else if approvals.approvals.len() >= MAX_PENDING_APPROVALS {
        Some("too many pending approvals")
    } else {
        None
    };
    if let Some(message) = rejection {
        drop(approvals);
        outgoing
            .try_send(json!({"id": id, "error": {"code": -32000, "message": message}}).into())
            .map_err(outbound_error)?;
        return Ok(());
    }
    let permit = reserve_app_stream(&approvals.stream_capacity, &params)?;
    let request = ApprovalRequest {
        approval_id: ApprovalId::new(approval_id.clone()),
        tool_call_id: ToolCallId::new(tool_call_id),
        title: approval_title(&method, &params),
        arguments: params.clone(),
        paths: approval_paths(&params),
        patch: approval_patch(&method, &params),
        choices: None,
        stream_permit: Some(crate::ApprovalCapacity::Stream(permit)),
    };
    approvals.approvals.insert(approval_id.clone(), server_id);
    drop(approvals);
    if sender
        .send(HarnessEvent::ApprovalRequested { request })
        .is_err()
    {
        tracing::debug!("the core stopped listening to codex events");
        pending.lock().await.approvals.remove(&approval_id);
        outgoing.try_send(json!({"id": id, "error": {"code": -32000, "message": "approval handler unavailable"}}).into()).map_err(outbound_error)?;
    }
    Ok(())
}

async fn handle_notification(
    method: &str,
    params: &Value,
    sender: &mpsc::UnboundedSender<HarnessEvent>,
    pending: &Arc<AsyncMutex<Pending>>,
) -> HarnessResult<()> {
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
                if pending.start_request.is_some() {
                    let first_start = pending.announced_turn.is_none();
                    announce_app_turn(&mut pending, turn_id, sender)?;
                    if first_start {
                        pending.turn_error = None;
                    }
                } else {
                    tracing::debug!("ignoring an unsolicited or duplicate Codex turn start");
                }
            }
        }
        "error" if params["willRetry"] == false => {
            let mut pending = pending.lock().await;
            if pending
                .turn_id
                .as_deref()
                .is_some_and(|turn_id| params["turnId"].as_str() == Some(turn_id))
            {
                pending.turn_error = codex_turn_error(&params["error"]);
            }
        }
        "item/agentMessage/delta" => {
            if let Some(delta) = params.get("delta").and_then(Value::as_str) {
                events.push(HarnessEvent::Delta {
                    item_id: ItemId::new(item_id(params)),
                    kind: DeltaKind::Message,
                    text: delta.to_string(),
                    stream_permit: None,
                });
            }
        }
        "item/reasoning/delta" | "item/agentThought/delta" => {
            if let Some(delta) = params.get("delta").and_then(Value::as_str) {
                events.push(HarnessEvent::Delta {
                    item_id: ItemId::new(item_id(params)),
                    kind: DeltaKind::Thought,
                    text: delta.to_string(),
                    stream_permit: None,
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
                    output_permit: None,
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
                    pending.approval_writes.remove(&approval_id);
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
            if pending.start_request.is_none() && pending.turn_id.is_none() {
                tracing::debug!("ignoring an unsolicited Codex turn completion");
                return Ok(());
            }
            let completed = read_turn_id(params).ok_or_else(|| {
                HarnessError::Protocol("Codex turn completion is missing its turn ID.".into())
            })?;
            let active = pending
                .turn_id
                .as_deref()
                .or_else(|| pending.start_request.and(pending.announced_turn.as_deref()));
            if active.is_some_and(|active| active != completed) {
                tracing::warn!("ignoring completion for a different Codex turn");
                return Ok(());
            }
            if pending.start_request.is_some()
                && pending.turn_id.is_none()
                && pending.announced_turn.is_some()
                && pending.announced_turn.as_deref() == Some(completed.as_str())
            {
                tracing::debug!("ignoring a duplicate Codex turn completion");
                return Ok(());
            }
            if pending.start_request.is_some() {
                announce_app_turn(&mut pending, completed, sender)?;
            }
            pending.turn_id = None;
            pending.approval_writes.clear();
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
                    stream_permit: None,
                });
                TurnFinishReason::Failed
            });
            events.push(HarnessEvent::TurnFinished { reason });
        }
        _ => {}
    }

    let permit = if events.iter().any(|event| {
        matches!(
            event,
            HarnessEvent::Delta { .. }
                | HarnessEvent::ToolCall { .. }
                | HarnessEvent::CommandOutput { .. }
        )
    }) {
        let capacity = pending.lock().await.stream_capacity.clone();
        Some(reserve_app_stream(&capacity, params)?)
    } else {
        None
    };
    for mut event in events {
        match &mut event {
            HarnessEvent::Delta { stream_permit, .. }
            | HarnessEvent::ToolCall { stream_permit, .. } => *stream_permit = permit.clone(),
            HarnessEvent::CommandOutput { output_permit, .. } => *output_permit = permit.clone(),
            HarnessEvent::TurnError {
                text,
                stream_permit,
                ..
            } => {
                let capacity = pending.lock().await.stream_capacity.clone();
                *stream_permit = Some(reserve_app_stream(&capacity, text)?);
            }
            _ => {}
        }
        if sender.send(event).is_err() {
            tracing::debug!("the core stopped listening to codex events");
            return Ok(());
        }
    }
    Ok(())
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
                .map(|output| {
                    let mut output = output.to_string();
                    crate::limit_command_output(&mut output);
                    output
                }),
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
        stream_permit: None,
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
    Some(HarnessEvent::TurnError {
        kind,
        text,
        stream_permit: None,
    })
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

    #[tokio::test]
    async fn stale_turn_completion_preserves_active_turn_and_approvals()
    -> Result<(), Box<dyn std::error::Error>> {
        let pending = Arc::new(AsyncMutex::new(Pending::default()));
        {
            let mut state = pending.lock().await;
            state.turn_id = Some("current".into());
            state.start_request = Some(1);
            state.announced_turn = Some("current".into());
            state.approvals.insert("approval".into(), 7);
        }
        let (sender, mut receiver) = mpsc::unbounded_channel();
        handle_notification(
            "turn/completed",
            &json!({"turn": {"id": "previous", "status": "completed"}}),
            &sender,
            &pending,
        )
        .await?;
        assert_eq!(pending.lock().await.turn_id.as_deref(), Some("current"));
        assert_eq!(pending.lock().await.approvals.get("approval"), Some(&7));
        assert!(receiver.try_recv().is_err());
        handle_notification(
            "turn/completed",
            &json!({"turn": {"id": "current", "status": "completed"}}),
            &sender,
            &pending,
        )
        .await?;
        assert!(matches!(
            receiver.try_recv(),
            Ok(HarnessEvent::ApprovalWithdrawn { .. })
        ));
        assert!(matches!(
            receiver.try_recv(),
            Ok(HarnessEvent::TurnFinished { .. })
        ));
        assert!(pending.lock().await.turn_id.is_none());
        handle_notification(
            "turn/completed",
            &json!({"turn": {"id": "previous", "status": "completed"}}),
            &sender,
            &pending,
        )
        .await?;
        assert!(receiver.try_recv().is_err());
        handle_notification(
            "turn/completed",
            &json!({"turn": {"id": "current", "status": "completed"}}),
            &sender,
            &pending,
        )
        .await?;
        assert!(receiver.try_recv().is_err());
        Ok(())
    }

    use super::*;

    #[tokio::test]
    async fn app_server_saved_turn_error_retains_its_byte_capacity() -> HarnessResult<()> {
        let pending = Arc::new(AsyncMutex::new(Pending::default()));
        let capacity = Arc::new(tokio::sync::Semaphore::new(2));
        {
            let mut state = pending.lock().await;
            state.stream_capacity = capacity.clone();
            state.turn_id = Some("turn".into());
        }
        let (sender, mut events) = mpsc::unbounded_channel();
        handle_notification(
            "error",
            &json!({"willRetry": false, "turnId": "turn", "error": {"message": "x".repeat(APP_STREAM_UNIT_BYTES + 1)}}),
            &sender,
            &pending,
        ).await?;
        handle_notification("turn/failed", &json!({"turnId": "turn"}), &sender, &pending).await?;
        assert_eq!(capacity.available_permits(), 0);
        let error = events.try_recv().map_err(|_| HarnessError::ChannelClosed)?;
        assert!(matches!(
            &error,
            HarnessEvent::TurnError {
                stream_permit: Some(_),
                ..
            }
        ));
        assert!(matches!(
            events.try_recv(),
            Ok(HarnessEvent::TurnFinished { .. })
        ));
        let (reply, response) = oneshot::channel();
        pending.lock().await.responses.insert(9, reply);
        let (outgoing, _requests) = mpsc::channel(1);
        route_incoming(json!({"id":9, "result":{}}), &sender, &pending, &outgoing).await?;
        assert!(
            response
                .await
                .map_err(|_| HarnessError::ChannelClosed)?
                .is_ok()
        );
        drop(error);
        assert_eq!(capacity.available_permits(), 2);
        Ok(())
    }

    #[tokio::test]
    async fn app_server_orders_start_and_completion_around_start_reply() -> HarnessResult<()> {
        for order in 0..4 {
            let pending = Arc::new(AsyncMutex::new(Pending::default()));
            let (reply, response) = oneshot::channel();
            {
                let mut state = pending.lock().await;
                state.responses.insert(1, reply);
                state.start_request = Some(1);
            }
            let (sender, mut events) = mpsc::unbounded_channel();
            let (outgoing, _requests) = mpsc::channel(1);
            let start = json!({"method":"turn/started", "params":{"turn":{"id":"turn"}}});
            let finish = json!({"method":"turn/completed", "params":{"turn":{"id":"turn", "status":"completed"}}});
            let answer = json!({"id":1, "result":{"turn":{"id":"turn"}}});
            let messages = match order {
                0 => vec![answer, start, finish],
                1 => vec![start, finish, answer],
                2 => vec![finish, answer, start],
                _ => vec![answer, finish, start],
            };
            for message in messages {
                route_incoming(message, &sender, &pending, &outgoing).await?;
            }
            assert!(
                response
                    .await
                    .map_err(|_| HarnessError::ChannelClosed)?
                    .is_ok()
            );
            assert!(matches!(
                events.try_recv(),
                Ok(HarnessEvent::TurnStarted { .. })
            ));
            assert!(matches!(
                events.try_recv(),
                Ok(HarnessEvent::TurnFinished { .. })
            ));
            assert!(events.try_recv().is_err());
            assert!(pending.lock().await.turn_id.is_none());
        }
        Ok(())
    }

    #[tokio::test]
    async fn unsolicited_turn_notifications_do_not_create_events_or_change_active_turn()
    -> HarnessResult<()> {
        let pending = Arc::new(AsyncMutex::new(Pending::default()));
        let (sender, mut events) = mpsc::unbounded_channel();
        let start = json!({"turn": {"id": "unsolicited"}});
        let completed = json!({"turn": {"id": "unsolicited", "status": "completed"}});
        for _ in 0..512 {
            handle_notification("turn/started", &start, &sender, &pending).await?;
            handle_notification("turn/completed", &completed, &sender, &pending).await?;
        }
        assert!(events.try_recv().is_err());
        assert!(pending.lock().await.turn_id.is_none());
        {
            let mut state = pending.lock().await;
            state.turn_id = Some("current".into());
            state.turn_error = Some(HarnessEvent::TurnError {
                kind: FailureKind::Protocol,
                text: "original failure".into(),
                stream_permit: None,
            });
        }
        handle_notification("turn/started", &start, &sender, &pending).await?;
        handle_notification("turn/completed", &completed, &sender, &pending).await?;
        let state = pending.lock().await;
        assert_eq!(state.turn_id.as_deref(), Some("current"));
        assert!(
            matches!(state.turn_error.as_ref(), Some(HarnessEvent::TurnError { text, .. }) if text == "original failure")
        );
        assert!(events.try_recv().is_err());
        drop(state);
        {
            let mut state = pending.lock().await;
            state.start_request = Some(1);
            state.announced_turn = Some("current".into());
        }
        handle_notification(
            "turn/started",
            &json!({"turn": {"id": "current"}}),
            &sender,
            &pending,
        )
        .await?;
        assert!(matches!(pending.lock().await.turn_error.as_ref(),
            Some(HarnessEvent::TurnError { text, .. }) if text == "original failure"));
        assert!(events.try_recv().is_err());
        Ok(())
    }

    #[tokio::test]
    async fn unidentified_turn_completion_fails_without_mutating_active_state() -> HarnessResult<()>
    {
        for active in [false, true] {
            let pending = Arc::new(AsyncMutex::new(Pending::default()));
            {
                let mut state = pending.lock().await;
                state.start_request = Some(1);
                if active {
                    state.turn_id = Some("current".into());
                    state.announced_turn = Some("current".into());
                }
                state.approvals.insert("approval".into(), 7);
            }
            let (sender, mut events) = mpsc::unbounded_channel();
            let result = handle_notification(
                "turn/completed",
                &json!({"turn": {"status": "completed"}}),
                &sender,
                &pending,
            )
            .await;
            assert!(
                matches!(result, Err(HarnessError::Protocol(message)) if message.contains("missing its turn ID"))
            );
            let state = pending.lock().await;
            assert_eq!(state.turn_id.as_deref(), active.then_some("current"));
            assert_eq!(state.approvals.get("approval"), Some(&7));
            assert!(events.try_recv().is_err());
        }
        Ok(())
    }

    #[tokio::test]
    async fn turn_errors_require_a_matching_active_turn() -> HarnessResult<()> {
        let pending = Arc::new(AsyncMutex::new(Pending::default()));
        let (sender, _) = mpsc::unbounded_channel();
        let error = json!({"willRetry": false, "error": {"message": "orphan"}});
        handle_notification("error", &error, &sender, &pending).await?;
        assert!(pending.lock().await.turn_error.is_none());
        pending.lock().await.turn_id = Some("current".into());
        handle_notification("error", &error, &sender, &pending).await?;
        assert!(pending.lock().await.turn_error.is_none());
        let matching = json!({"willRetry": false, "turnId": "current", "error": {"message": "current failure"}});
        handle_notification("error", &matching, &sender, &pending).await?;
        assert!(
            matches!(pending.lock().await.turn_error.as_ref(), Some(HarnessEvent::TurnError { text, .. }) if text == "current failure")
        );
        Ok(())
    }

    #[tokio::test]
    async fn reader_reports_group_stop_failure_and_retains_the_child_for_retry()
    -> Result<(), Box<dyn std::error::Error>> {
        for script in [
            "printf 'invalid\\n'; exec sleep 30",
            "exec 1>&-; exec sleep 30",
        ] {
            let mut command = Command::new("/bin/sh");
            command
                .args(["-c", script])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true);
            command.process_group(0);
            let mut child = command.spawn()?;
            let stdout = child.stdout.take().ok_or("missing stdout")?;
            let child = Arc::new(AsyncMutex::new(child));
            let (sender, mut events) = mpsc::unbounded_channel();
            let (outgoing, _requests) = mpsc::channel(1);
            let (_failure, writer_failed) = oneshot::channel();
            let (drained, stderr_drained) = oneshot::channel();
            drained.send(()).map_err(|_| "stderr receiver closed")?;
            let reader = spawn_reader(
                stdout,
                sender,
                Arc::new(AsyncMutex::new(Pending::default())),
                outgoing,
                Arc::new(Mutex::new(VecDeque::new())),
                Arc::new(AtomicBool::new(false)),
                Arc::new(AtomicBool::new(false)),
                stderr_drained,
                writer_failed,
                Arc::downgrade(&child),
                |_| Err(std::io::Error::other("fixture group failure")),
                None,
            );
            assert!(matches!(
                timeout(Duration::from_secs(5), events.recv()).await?,
                Some(HarnessEvent::ConnectionFailed { text, .. }) if text.contains("fixture group failure")
            ));
            reader.await?;
            {
                let mut child = child.lock().await;
                assert!(child.id().is_some());
                assert!(child.try_wait()?.is_none());
            }
            let mut harness = CodexHarness::new();
            harness.started = true;
            harness.child = Some(child.clone());
            harness.shutdown().await?;
            assert!(harness.child.is_none());
        }
        Ok(())
    }

    #[tokio::test]
    async fn closed_app_server_input_stops_child_and_fails_pending_requests()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TempDir::new()?;
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "exec 0<&-; printf ready > ready; exec sleep 30"])
            .current_dir(directory.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        command.process_group(0);
        let mut child = command.spawn()?;
        let stdin = child.stdin.take().ok_or("missing stdin")?;
        let stdout = child.stdout.take().ok_or("missing stdout")?;
        let child = Arc::new(AsyncMutex::new(child));
        timeout(Duration::from_secs(5), async {
            while !directory.path().join("ready").is_file() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await?;
        let (sender, mut events) = mpsc::unbounded_channel();
        let pending = Arc::new(AsyncMutex::new(Pending::default()));
        let (response, response_waiter) = oneshot::channel();
        pending.lock().await.responses.insert(1, response);
        let (outgoing, requests) = mpsc::channel(MAX_OUTBOUND_MESSAGES);
        let (writer_failure, writer_failed) = oneshot::channel();
        let writer = spawn_writer(stdin, requests, Some(writer_failure), None);
        let (drained, stderr_drained) = oneshot::channel();
        drained.send(()).map_err(|_| "stderr receiver closed")?;
        let reader = spawn_reader(
            stdout,
            sender,
            pending,
            outgoing.clone(),
            Arc::new(Mutex::new(VecDeque::new())),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            stderr_drained,
            writer_failed,
            Arc::downgrade(&child),
            stop_codex_process_group,
            None,
        );
        outgoing.try_send(json!({"id": 1, "method": "fixture"}).into())?;
        assert!(matches!(
            timeout(Duration::from_secs(5), events.recv()).await?,
            Some(HarnessEvent::ConnectionFailed { kind: FailureKind::Protocol, text }) if text.contains("stopped accepting protocol input")
        ));
        assert!(
            timeout(Duration::from_secs(1), response_waiter)
                .await?
                .is_err()
        );
        reader.await?;
        writer.await?;
        assert!(child.lock().await.try_wait()?.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn unexpected_writer_exit_stops_reader_with_partial_stdout()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "printf '{\\\"partial\\\":'; exec sleep 30"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        command.process_group(0);
        let mut child = command.spawn()?;
        let stdout = child.stdout.take().ok_or("missing stdout")?;
        let child = Arc::new(AsyncMutex::new(child));
        let (sender, mut events) = mpsc::unbounded_channel();
        let (outgoing, _requests) = mpsc::channel(1);
        let (failure, writer_failed) = oneshot::channel();
        let (drained, stderr_drained) = oneshot::channel();
        drained.send(()).map_err(|_| "stderr receiver closed")?;
        let reader = spawn_reader(
            stdout,
            sender,
            Arc::new(AsyncMutex::new(Pending::default())),
            outgoing,
            Arc::new(Mutex::new(VecDeque::new())),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            stderr_drained,
            writer_failed,
            Arc::downgrade(&child),
            stop_codex_process_group,
            None,
        );
        drop(failure);
        assert!(matches!(
            timeout(Duration::from_secs(5), events.recv()).await?,
            Some(HarnessEvent::ConnectionFailed { kind: FailureKind::Protocol, text }) if text.contains("writer stopped unexpectedly")
        ));
        reader.await?;
        assert!(child.lock().await.try_wait()?.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn full_outbound_queue_fails_the_app_server_reader()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut command = Command::new("/bin/sh");
        command
            .args([
                "-c",
                "printf '%s\\n' '{\"id\":9,\"method\":\"unsupported\"}'; exec sleep 30",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        command.process_group(0);
        let mut child = command.spawn()?;
        let stdout = child.stdout.take().ok_or("missing stdout")?;
        let child = Arc::new(AsyncMutex::new(child));
        let (sender, mut events) = mpsc::unbounded_channel();
        let (outgoing, _requests) = mpsc::channel(1);
        outgoing.try_send(json!({"id": 1}).into())?;
        let (drained, stderr_drained) = oneshot::channel();
        drained.send(()).map_err(|_| "stderr receiver closed")?;
        let (_failure, writer_failed) = oneshot::channel();
        let reader = spawn_reader(
            stdout,
            sender,
            Arc::new(AsyncMutex::new(Pending::default())),
            outgoing,
            Arc::new(Mutex::new(VecDeque::new())),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            stderr_drained,
            writer_failed,
            Arc::downgrade(&child),
            stop_codex_process_group,
            None,
        );
        assert!(matches!(
            timeout(Duration::from_secs(5), events.recv()).await?,
            Some(HarnessEvent::ConnectionFailed { kind: FailureKind::Protocol, text }) if text.contains("outbound queue is full")
        ));
        reader.await?;
        assert!(child.lock().await.try_wait()?.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn full_outbound_queue_reports_error_and_allows_approval_retry()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut harness = CodexHarness::new();
        harness
            .pending
            .lock()
            .await
            .approvals
            .insert("item".into(), 42);
        let (outgoing, mut requests) = mpsc::channel(MAX_OUTBOUND_MESSAGES);
        for number in 0..MAX_OUTBOUND_MESSAGES {
            outgoing.try_send(json!({"id": number}).into())?;
        }
        harness.outgoing = Some(outgoing.clone());
        assert!(matches!(
            harness.respond(ApprovalId::new("item"), Decision::Allow).await,
            Err(HarnessError::Protocol(message)) if message.contains("queue is full")
        ));
        let pending = harness.pending.lock().await;
        assert_eq!(pending.approvals.get("item"), Some(&42));
        assert!(pending.approval_writes.is_empty());
        drop(pending);
        assert!(matches!(
            route_incoming(json!({"id": 900, "method": "unsupported", "params": {}}), &harness.sender, &harness.pending, &outgoing).await,
            Err(HarnessError::Protocol(message)) if message.contains("queue is full")
        ));
        requests.recv().await.ok_or("missing queued message")?;
        route_incoming(
            json!({"id": 901, "method": "unsupported", "params": {}}),
            &harness.sender,
            &harness.pending,
            &outgoing,
        )
        .await?;
        let mut response = None;
        while let Ok(message) = requests.try_recv() {
            if message.value["id"] == 901 {
                response = Some(message.value);
            }
        }
        assert_eq!(
            response.ok_or("missing rejection")?["error"]["code"],
            METHOD_NOT_FOUND
        );
        let (writer, reader) = tokio::io::duplex(4096);
        let writer_task = spawn_writer(writer, requests, None, None);
        harness
            .respond(ApprovalId::new("item"), Decision::Allow)
            .await?;
        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        let message: Value = serde_json::from_str(&line)?;
        assert_eq!(message["id"], 42);
        assert_eq!(message["result"]["decision"], "accept");
        harness.outgoing = None;
        drop(outgoing);
        writer_task.await?;
        Ok(())
    }

    #[tokio::test]
    async fn approval_requests_are_bounded_and_duplicate_items_do_not_replace_pending_requests()
    -> Result<(), Box<dyn std::error::Error>> {
        let pending = Arc::new(AsyncMutex::new(Pending::default()));
        let (events, mut received) = mpsc::unbounded_channel();
        let (outgoing, mut responses) = mpsc::channel(MAX_OUTBOUND_MESSAGES);
        let method = "item/commandExecution/requestApproval";
        for number in 0..MAX_PENDING_APPROVALS {
            handle_server_request(
                json!(number),
                method.into(),
                json!({"itemId": format!("item-{number}")}),
                &events,
                &pending,
                &outgoing,
            )
            .await?;
        }
        assert_eq!(pending.lock().await.approvals.len(), MAX_PENDING_APPROVALS);
        for _ in 0..MAX_PENDING_APPROVALS {
            assert!(matches!(
                received.try_recv(),
                Ok(HarnessEvent::ApprovalRequested { .. })
            ));
        }
        handle_server_request(
            json!(100),
            method.into(),
            json!({"itemId": "item-0"}),
            &events,
            &pending,
            &outgoing,
        )
        .await?;
        let duplicate = responses.try_recv()?.value;
        assert_eq!(duplicate["id"], 100);
        assert!(
            duplicate["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("already pending"))
        );
        assert_eq!(pending.lock().await.approvals.get("item-0"), Some(&0));
        handle_server_request(
            json!(0),
            method.into(),
            json!({"itemId": "item-0"}),
            &events,
            &pending,
            &outgoing,
        )
        .await?;
        assert!(responses.try_recv().is_err());
        handle_server_request(
            json!(101),
            method.into(),
            json!({"itemId": "extra"}),
            &events,
            &pending,
            &outgoing,
        )
        .await?;
        let overflow = responses.try_recv()?.value;
        assert_eq!(overflow["id"], 101);
        assert!(
            overflow["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("too many"))
        );

        handle_notification(
            "serverRequest/resolved",
            &json!({"requestId": 0}),
            &events,
            &pending,
        )
        .await?;
        assert!(matches!(
            received.try_recv(),
            Ok(HarnessEvent::ApprovalWithdrawn { .. })
        ));
        handle_server_request(
            json!(102),
            method.into(),
            json!({"itemId": "extra"}),
            &events,
            &pending,
            &outgoing,
        )
        .await?;
        assert!(matches!(
            received.try_recv(),
            Ok(HarnessEvent::ApprovalRequested { .. })
        ));
        assert_eq!(pending.lock().await.approvals.len(), MAX_PENDING_APPROVALS);

        drop(received);
        handle_notification(
            "serverRequest/resolved",
            &json!({"requestId": 1}),
            &events,
            &pending,
        )
        .await?;
        handle_server_request(
            json!(103),
            method.into(),
            json!({"itemId": "undeliverable"}),
            &events,
            &pending,
            &outgoing,
        )
        .await?;
        assert_eq!(responses.try_recv()?.value["id"], 103);
        assert!(!pending.lock().await.approvals.contains_key("undeliverable"));
        Ok(())
    }

    #[tokio::test]
    async fn stderr_reader_bounds_long_lines_and_drains_following_output()
    -> Result<(), Box<dyn std::error::Error>> {
        let (mut writer, reader) = tokio::io::duplex(8192);
        let tail = Arc::new(Mutex::new(VecDeque::new()));
        let task = spawn_stderr_reader(reader, tail.clone(), None);
        writer.write_all(&vec![b'x'; 256 * 1024]).await?;
        writer.write_all(b" end\nnext\npartial").await?;
        drop(writer);
        task.await?;
        let tail = tail.lock().map_err(|_| "stderr lock poisoned")?;
        assert_eq!(tail.len(), 3);
        assert!(tail[0].len() < 5000 && tail[0].ends_with(" end"));
        assert_eq!(tail[1], "next");
        assert_eq!(tail[2], "partial");
        Ok(())
    }

    #[tokio::test]
    async fn abandoned_reply_logs_omit_response_ids() -> Result<(), Box<dyn std::error::Error>> {
        #[derive(Clone)]
        struct Writer(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Writer {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0
                    .lock()
                    .map_err(|error| std::io::Error::other(error.to_string()))?
                    .extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let logs = Arc::new(Mutex::new(Vec::new()));
        let writer = Writer(logs.clone());
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .without_time()
            .with_max_level(tracing::Level::DEBUG)
            .with_writer(move || writer.clone())
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        let (events, _event_receiver) = mpsc::unbounded_channel();
        let (outgoing, _outgoing_receiver) = mpsc::channel(1);
        let pending = Arc::new(AsyncMutex::new(Pending::default()));
        route_incoming(
            json!({"id": 72931486152_i64, "result": {}}),
            &events,
            &pending,
            &outgoing,
        )
        .await?;
        let (reply, receiver) = oneshot::channel();
        drop(receiver);
        pending.lock().await.responses.insert(72931486153, reply);
        route_incoming(
            json!({"id": 72931486153_i64, "result": {}}),
            &events,
            &pending,
            &outgoing,
        )
        .await?;
        let output = String::from_utf8(logs.lock().map_err(|error| error.to_string())?.clone())?;
        assert!(output.contains("codex answered a request nobody is waiting for"));
        assert!(output.contains("the answer arrived after its caller gave up"));
        assert!(!output.contains("72931486152"));
        assert!(!output.contains("72931486153"));
        Ok(())
    }

    #[tokio::test]
    async fn stderr_reader_does_not_log_agent_output() -> Result<(), Box<dyn std::error::Error>> {
        #[derive(Clone)]
        struct Writer(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Writer {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0
                    .lock()
                    .map_err(|error| std::io::Error::other(error.to_string()))?
                    .extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let logs = Arc::new(Mutex::new(Vec::new()));
        let writer = Writer(logs.clone());
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .without_time()
            .with_max_level(tracing::Level::DEBUG)
            .with_writer(move || writer.clone())
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        tracing::debug!("fixture subscriber active");
        let tail = Arc::new(Mutex::new(VecDeque::new()));
        let task = spawn_stderr_reader(&b"fixture-private-value\n"[..], tail.clone(), None);
        task.await?;
        assert_eq!(
            tail.lock()
                .map_err(|error| error.to_string())?
                .front()
                .map(String::as_str),
            Some("fixture-private-value")
        );
        let output = String::from_utf8(logs.lock().map_err(|error| error.to_string())?.clone())?;
        assert!(output.contains("fixture subscriber active"));
        assert!(!output.contains("fixture-private-value"));
        Ok(())
    }
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
      printf '{"id":%s,"result":{"capabilities":{"methods":["thread/start","thread/resume","turn/start","turn/interrupt","model/list","item/commandExecution/requestApproval"]}}}\n' "$id"
      ;;
    *'"method":"config/read"'*)
      case "$line" in
        *'/configured"'*) model='"project-model"' ;;
        *) model=null ;;
      esac
      printf '{"id":%s,"result":{"config":{"model":%s,"model_reasoning_effort":"low"}}}\n' "$id" "$model"
      ;;
    *'"method":"model/list"'*)
      case "$line" in
        *'"cursor":"next"'*) printf '{"id":%s,"result":{"data":[{"model":"catalog-default","isDefault":true,"defaultReasoningEffort":"low","supportedReasoningEfforts":[{"reasoningEffort":"low","description":"Fixture"}]},{"model":"hidden","hidden":true}],"nextCursor":null}}\n' "$id" ;;
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
    async fn app_server_receives_the_cached_image_and_selected_turn_options()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TempDir::new()?;
        let script = PROTOCOL_STUB.replace(
            "    *'\"method\":\"turn/start\"'*)\n",
            "    *'\"method\":\"turn/start\"'*)\n      printf '%s\\n' \"$line\" > \"$directory/turn.json\"\n",
        );
        let stub = write_stub(&directory, "codex-images", &script);
        let (mut harness, capabilities) = start_session(&stub, directory.path()).await;
        assert!(capabilities.images);
        assert!(capabilities.model_picker);
        let path = directory.path().join("a screenshot.png");
        harness
            .prompt(PromptInput {
                text: "Review this screenshot.".into(),
                attachments: vec![crate::PromptAttachment {
                    path: path.clone(),
                    image_mime_type: Some("image/png".into()),
                    metadata: None,
                }],
                codex: crate::CodexTurnOptions {
                    model: Some("catalog-default".into()),
                    effort: Some("low".into()),
                },
                ..PromptInput::default()
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
        assert_eq!(request["params"]["model"], "catalog-default");
        assert_eq!(request["params"]["effort"], "low");
        assert_eq!(
            harness.diagnostics().model.as_deref(),
            Some("catalog-default")
        );
        assert_eq!(
            harness.diagnostics().reasoning_effort.as_deref(),
            Some("low")
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
            matches!(events.as_slice(), [.., HarnessEvent::TurnError { kind: FailureKind::AuthRequired, text, .. }, HarnessEvent::TurnFinished { reason: TurnFinishReason::Failed }] if text == "Session expired\nRefresh the account"),
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
            (
                "codex-oversized",
                "python3 -c 'import sys; sys.stdout.write(\"x\" * (8 * 1024 * 1024 + 2)); sys.stdout.flush()'; sleep 30",
            ),
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
                (
                    "codex-oversized",
                    Some(HarnessEvent::ConnectionFailed {
                        kind: FailureKind::Protocol,
                        text,
                    }),
                ) => assert!(text.contains("exceeds 8388608 bytes"), "{text}"),
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
    async fn replay_app_server(name: &str) -> HarnessResult<Transcript> {
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let pending = Arc::new(AsyncMutex::new(Pending {
            turn_id: Some("turn_1".into()),
            ..Pending::default()
        }));
        let (outgoing, _outgoing_receiver) = mpsc::channel(MAX_OUTBOUND_MESSAGES);
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
                    .await?;
                }
                (None, Some(method)) => {
                    handle_notification(
                        method.as_str().unwrap_or_default(),
                        &params,
                        &sender,
                        &pending,
                    )
                    .await?;
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
        Ok(transcript)
    }

    fn summaries(transcript: &Transcript) -> Vec<String> {
        transcript.entries().iter().map(Entry::summary).collect()
    }

    #[tokio::test]
    async fn a_recorded_app_server_session_replays_without_the_cli() -> HarnessResult<()> {
        let transcript = replay_app_server("app-server-session.jsonl").await?;
        assert_eq!(
            summaries(&transcript),
            vec![
                "agent: Reading the repo. Done.".to_string(),
                "cargo test (Completed)".to_string(),
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_recorded_approval_with_no_decision_is_withdrawn_when_the_turn_ends()
    -> HarnessResult<()> {
        let transcript = replay_app_server("app-server-approval.jsonl").await?;
        assert_eq!(
            summaries(&transcript),
            vec![
                "agent: About to run the tests.".to_string(),
                "approval: cargo test (withdrawn)".to_string(),
            ]
        );
        Ok(())
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

    #[test]
    fn bounded_exec_command_output_matches_transcript_truncation() {
        for length in [0, 1, 21_845, 21_846, 50_000] {
            let original = format!("{}tail", "界".repeat(length));
            let mut expected = original.clone();
            crate::limit_command_output(&mut expected);
            assert_eq!(crate::bounded_command_output(&original), expected);
        }
    }

    #[test]
    fn exec_command_events_bound_output_before_queueing() -> Result<(), Box<dyn std::error::Error>>
    {
        let output = format!("{}tail", "界".repeat(crate::MAX_COMMAND_OUTPUT_BYTES));
        let events = exec_events(
            &json!({"msg": {"type": "exec_command_end", "call_id": "call", "aggregated_output": output, "exit_code": 0}}),
        );
        let [
            HarnessEvent::ToolCall {
                output: Some(output),
                ..
            },
        ] = events.as_slice()
        else {
            return Err("missing command output".into());
        };
        assert!(output.len() <= crate::MAX_COMMAND_OUTPUT_BYTES);
        assert!(output.starts_with(crate::COMMAND_OUTPUT_OMITTED));
        assert!(output.ends_with("tail"));
        Ok(())
    }

    #[test]
    fn app_server_command_events_bound_output_before_queueing()
    -> Result<(), Box<dyn std::error::Error>> {
        let output = format!("{}tail", "界".repeat(crate::MAX_COMMAND_OUTPUT_BYTES));
        let event = tool_call_from_item(&json!({
            "id": "call", "type": "commandExecution", "status": "completed", "aggregatedOutput": output
        })).ok_or("missing command event")?;
        let HarnessEvent::ToolCall {
            output: Some(output),
            ..
        } = event
        else {
            return Err("missing command output".into());
        };
        assert!(output.len() <= crate::MAX_COMMAND_OUTPUT_BYTES);
        assert!(output.starts_with(crate::COMMAND_OUTPUT_OMITTED));
        assert!(output.ends_with("tail"));
        Ok(())
    }

    #[test]
    fn exec_error_log_omits_agent_text() -> Result<(), Box<dyn std::error::Error>> {
        #[derive(Clone)]
        struct Writer(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Writer {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0
                    .lock()
                    .map_err(|error| std::io::Error::other(error.to_string()))?
                    .extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let writer = Writer(buffer.clone());
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .without_time()
            .with_writer(move || writer.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            exec_events(&json!({"msg": {"type": "error", "message": "fixture-private-value"}}));
        });
        let output = String::from_utf8(buffer.lock().map_err(|error| error.to_string())?.clone())?;
        assert!(output.contains("codex exec reported an error"));
        assert!(!output.contains("fixture-private-value"));
        Ok(())
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
    async fn probes_stop_descendants_after_handshake_or_timeout()
    -> Result<(), Box<dyn std::error::Error>> {
        for handshake in [true, false] {
            let directory = TempDir::new()?;
            let response = if handshake {
                "read -r request\nprintf '{\"id\":1,\"result\":{}}\\n'"
            } else {
                ":"
            };
            let script =
                format!("#!/bin/sh\n(sleep 1; printf escaped > escaped) &\n{response}\nsleep 30\n");
            let stub = write_stub(&directory, "codex-probe.sh", &script);
            if handshake {
                assert!(run_handshake(&stub, directory.path()).await?.supported);
            } else {
                assert!(
                    capture_probe_output(
                        &stub,
                        directory.path(),
                        &[],
                        "fixture",
                        Duration::from_millis(300)
                    )
                    .await
                    .is_err()
                );
            }
            tokio::time::sleep(Duration::from_millis(1300)).await;
            assert!(!directory.path().join("escaped").exists());
        }
        Ok(())
    }

    #[tokio::test]
    async fn probe_timeout_stops_pipe_holding_descendant_after_leader_exits()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TempDir::new()?;
        let stub = write_stub(
            &directory,
            "codex-probe-exit.sh",
            "#!/bin/sh\n(sleep 1; printf escaped > escaped) &\nexit 0\n",
        );
        assert!(
            capture_probe_output(
                &stub,
                directory.path(),
                &[],
                "fixture",
                Duration::from_millis(300)
            )
            .await
            .is_err()
        );
        tokio::time::sleep(Duration::from_millis(1300)).await;
        assert!(!directory.path().join("escaped").exists());
        Ok(())
    }

    #[tokio::test]
    async fn probe_capture_limits_both_streams_and_times_out()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TempDir::new()?;
        let stub = write_stub(
            &directory,
            "probe-limits",
            r##"#!/usr/bin/env bash
case "$1" in
  stdout) exec python3 -c 'import sys; sys.stdout.write("x" * (256 * 1024 + 1))' ;;
  stderr) exec python3 -c 'import sys; sys.stderr.write("x" * (256 * 1024 + 1))' ;;
  slow) exec sleep 30 ;;
  normal) printf 'ok'; printf 'note' >&2 ;;
esac
"##,
        );
        let output = capture_probe_output(
            &stub,
            directory.path(),
            &["normal"],
            "fixture",
            Duration::from_secs(5),
        )
        .await?;
        assert_eq!(output.stdout, b"ok");
        assert_eq!(output.stderr, b"note");
        for stream in ["stdout", "stderr"] {
            let Err(error) = capture_probe_output(
                &stub,
                directory.path(),
                &[stream],
                "fixture",
                Duration::from_secs(5),
            )
            .await
            else {
                return Err(format!("oversized {stream} was accepted").into());
            };
            assert!(
                error
                    .to_string()
                    .contains(&format!("{stream} exceeds {MAX_PROBE_OUTPUT_BYTES} bytes")),
                "{error}"
            );
        }
        let Err(error) = capture_probe_output(
            &stub,
            directory.path(),
            &["slow"],
            "fixture",
            Duration::from_millis(100),
        )
        .await
        else {
            return Err("slow probe succeeded".into());
        };
        assert!(error.to_string().contains("timed out"), "{error}");
        Ok(())
    }

    #[tokio::test]
    async fn probe_rejects_oversized_help_and_skips_oversized_version()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TempDir::new()?;
        let flood = "python3 -c 'import sys; sys.stdout.write(\"x\" * (256 * 1024 + 1))'";
        let version_script = PROTOCOL_STUB.replace("echo \"codex-cli 9.9.9\"", flood);
        let version_stub = write_stub(&directory, "oversized-version", &version_script);
        let result = probe(&version_stub, directory.path()).await?;
        assert!(result.version.is_none());
        assert_eq!(result.tier, CodexTier::AppServer);

        let help_script = UNSUPPORTED_STUB.replace("echo \"  --json\"", flood);
        let help_stub = write_stub(&directory, "oversized-help", &help_script);
        let Err(error) = probe(&help_stub, directory.path()).await else {
            return Err("oversized help was accepted".into());
        };
        assert!(
            error.to_string().contains("exec help stdout exceeds"),
            "{error}"
        );
        Ok(())
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
    async fn app_server_process_record_is_retained_until_cleanup_succeeds()
    -> Result<(), Box<dyn std::error::Error>> {
        struct Tracker {
            pid: Mutex<Option<u32>>,
            fail_start: bool,
            fail_clear: AtomicBool,
        }
        #[async_trait]
        impl crate::AgentProcessTracker for Tracker {
            async fn started(&self, pid: u32) -> Result<String, String> {
                *self.pid.lock().map_err(|error| error.to_string())? = Some(pid);
                if self.fail_start {
                    return Err("fixture save failure".into());
                }
                Ok("record".into())
            }

            async fn stopped(&self, record_id: &str) -> Result<(), String> {
                assert_eq!(record_id, "record");
                if self.fail_clear.swap(false, Ordering::SeqCst) {
                    return Err("fixture storage failure".into());
                }
                *self.pid.lock().map_err(|error| error.to_string())? = None;
                Ok(())
            }
        }
        let directory = TempDir::new()?;
        let stub = write_stub(&directory, "codex-tracked.sh", PROTOCOL_STUB);
        let tracker = Arc::new(Tracker {
            pid: Mutex::new(None),
            fail_start: false,
            fail_clear: AtomicBool::new(true),
        });
        let mut harness = CodexHarness::new();
        harness.set_process_tracker(tracker.clone());
        harness.start(spec(&stub, directory.path())).await?;
        assert!(
            tracker
                .pid
                .lock()
                .map_err(|error| error.to_string())?
                .is_some()
        );
        let error = harness
            .shutdown()
            .await
            .expect_err("fixture cleanup must fail");
        assert!(error.to_string().contains("fixture storage failure"));
        assert!(harness.started && harness.process_record.is_some());
        harness.shutdown().await?;
        assert!(
            tracker
                .pid
                .lock()
                .map_err(|error| error.to_string())?
                .is_none()
        );
        let failing = Arc::new(Tracker {
            pid: Mutex::new(None),
            fail_start: true,
            fail_clear: AtomicBool::new(false),
        });
        let mut harness = CodexHarness::new();
        harness.set_process_tracker(failing);
        let error = harness
            .start(spec(&stub, directory.path()))
            .await
            .expect_err("recording must fail");
        assert!(error.to_string().contains("fixture save failure"));
        let child = harness.child.as_ref().ok_or("missing child")?;
        tokio::time::timeout(Duration::from_secs(3), child.lock().await.wait()).await??;
        Ok(())
    }

    #[tokio::test]
    async fn app_server_shutdown_stops_its_process_group() -> Result<(), Box<dyn std::error::Error>>
    {
        let directory = TempDir::new()?;
        let script = PROTOCOL_STUB.replace(
            "while IFS= read -r line; do",
            "(sleep 1; printf escaped > escaped) &\nwhile IFS= read -r line; do",
        );
        let stub = write_stub(&directory, "codex-group.sh", &script);
        let mut harness = CodexHarness::new();
        harness.start(spec(&stub, directory.path())).await?;
        let child = harness.child.as_ref().ok_or("missing child")?.lock().await;
        let pid = libc::pid_t::try_from(child.id().ok_or("missing process id")?)?;
        assert_eq!(unsafe { libc::getpgid(pid) }, pid);
        assert_ne!(unsafe { libc::getpgrp() }, pid);
        drop(child);
        harness.shutdown().await?;
        tokio::time::sleep(Duration::from_millis(1300)).await;
        assert!(!directory.path().join("escaped").exists());
        Ok(())
    }

    #[tokio::test]
    async fn failed_app_server_child_signal_retains_process_for_retry()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TempDir::new()?;
        let stub = write_stub(&directory, "codex-child-failure.sh", PROTOCOL_STUB);
        let mut harness = CodexHarness::new();
        harness.start(spec(&stub, directory.path())).await?;
        let failure = harness
            .shutdown_with_signals(
                |_| Ok(()),
                |_| Err(std::io::Error::other("fixture child failure")),
            )
            .await;
        assert!(failure.is_err());
        assert!(harness.started);
        let mut child = harness
            .child
            .as_ref()
            .ok_or("process handle was discarded")?
            .lock()
            .await;
        assert!(child.id().is_some());
        assert!(child.try_wait()?.is_none());
        drop(child);
        harness.shutdown().await?;
        assert!(!harness.started && harness.child.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn failed_app_server_group_signal_retains_process_for_retry()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TempDir::new()?;
        let script = PROTOCOL_STUB.replace(
            "while IFS= read -r line; do",
            "(sleep 1; printf escaped > escaped) &\nwhile IFS= read -r line; do",
        );
        let stub = write_stub(&directory, "codex-signal-failure.sh", &script);
        let mut harness = CodexHarness::new();
        harness.start(spec(&stub, directory.path())).await?;
        let failure = harness
            .shutdown_with_signal(|_| Err(std::io::Error::other("fixture signal failure")))
            .await;
        assert!(failure.is_err());
        assert!(harness.started);
        let mut child = harness
            .child
            .as_ref()
            .ok_or("process handle was discarded")?
            .lock()
            .await;
        assert!(child.id().is_some());
        assert!(child.try_wait()?.is_none());
        drop(child);
        harness.shutdown().await?;
        assert!(!harness.started && harness.child.is_none());
        tokio::time::sleep(Duration::from_millis(1300)).await;
        assert!(!directory.path().join("escaped").exists());
        Ok(())
    }

    #[tokio::test]
    async fn malformed_app_server_output_stops_descendants()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TempDir::new()?;
        let stub = write_stub(
            &directory,
            "codex-invalid.sh",
            "#!/bin/sh\n(sleep 1; printf escaped > escaped) &\nprintf 'not-json\\n'\nsleep 30\n",
        );
        let mut harness = CodexHarness::new();
        assert!(
            timeout(
                Duration::from_secs(5),
                harness.start(spec(&stub, directory.path()))
            )
            .await?
            .is_err()
        );
        tokio::time::sleep(Duration::from_millis(1300)).await;
        assert!(!directory.path().join("escaped").exists());
        Ok(())
    }

    #[tokio::test]
    async fn request_does_not_register_after_reader_closes()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut harness = CodexHarness::new();
        let (outgoing, _receiver) = mpsc::channel(MAX_OUTBOUND_MESSAGES);
        harness.outgoing = Some(outgoing);
        let pending = Arc::clone(&harness.pending);
        let closed = Arc::clone(&harness.closed);
        let mut guard = pending.lock().await;
        let mut request = Box::pin(harness.request("initialize", json!({})));
        std::future::poll_fn(|cx| {
            assert!(std::future::Future::poll(request.as_mut(), cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        closed.store(true, Ordering::Relaxed);
        guard.responses.clear();
        drop(guard);
        let result = timeout(Duration::from_secs(1), request).await?;
        assert!(
            matches!(result, Err(HarnessError::Protocol(message)) if message.contains("no longer running"))
        );
        assert!(pending.lock().await.responses.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn closed_app_server_output_stops_descendants() -> Result<(), Box<dyn std::error::Error>>
    {
        let directory = TempDir::new()?;
        let stub = write_stub(
            &directory,
            "codex-closed.sh",
            "#!/bin/sh\n(sleep 1; printf escaped > escaped) >/dev/null &\nexec 1>&-\nsleep 30\n",
        );
        let mut harness = CodexHarness::new();
        assert!(
            timeout(
                Duration::from_secs(5),
                harness.start(spec(&stub, directory.path()))
            )
            .await?
            .is_err()
        );
        tokio::time::sleep(Duration::from_millis(1300)).await;
        assert!(!directory.path().join("escaped").exists());
        Ok(())
    }

    #[tokio::test]
    async fn failed_and_cancelled_app_server_initialization_stop_descendants()
    -> Result<(), Box<dyn std::error::Error>> {
        for respond in [true, false] {
            let directory = TempDir::new()?;
            let response = if respond {
                "printf '{\"id\":1,\"error\":{\"code\":-1,\"message\":\"failed\"}}\\n'"
            } else {
                ":"
            };
            let script = format!(
                "#!/bin/sh\n(sleep 1; printf escaped > escaped) &\nread -r request\n{response}\nsleep 30\n"
            );
            let stub = write_stub(&directory, "codex-start.sh", &script);
            let mut harness = CodexHarness::new();
            let start = timeout(
                Duration::from_millis(300),
                harness.start(spec(&stub, directory.path())),
            )
            .await;
            if respond {
                assert!(start?.is_err());
            } else {
                assert!(start.is_err());
            }
            tokio::time::sleep(Duration::from_millis(1300)).await;
            assert!(!directory.path().join("escaped").exists());
        }
        Ok(())
    }

    #[tokio::test]
    async fn model_discovery_hides_agent_failure_with_environment()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TempDir::new()?;
        let stub = write_stub(
            &directory,
            "codex-secret",
            "#!/bin/sh\nprintf '%s\\n' \"$TOKEN\" >&2\nexit 1\n",
        );
        let mut launch = spec(&stub, directory.path());
        launch
            .environment
            .insert("TOKEN".into(), "fixture-private-value".into());
        let error = match project_models(launch).await {
            Ok(_) => return Err("expected discovery failure".into()),
            Err(error) => error,
        };
        assert!(!error.to_string().contains("fixture-private-value"));
        assert!(error.to_string().contains("hidden"));
        Ok(())
    }

    #[tokio::test]
    async fn cancelled_discovery_stops_app_server_descendants()
    -> Result<(), Box<dyn std::error::Error>> {
        for projects in [false, true] {
            let directory = TempDir::new()?;
            let stub = write_stub(
                &directory,
                "codex-discovery.sh",
                r##"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*) printf '{"id":1,"result":{}}\n' ;;
    *'"method":"config/read"'*|*'"method":"project/list"'*)
      (sleep 2; printf escaped > escaped) &
      printf ready > ready
      sleep 30 ;;
  esac
done
"##,
            );
            let launch = spec(&stub, directory.path());
            let discovery = tokio::spawn(async move {
                if projects {
                    list_projects(launch).await.map(|_| ())
                } else {
                    project_models(launch).await.map(|_| ())
                }
            });
            timeout(Duration::from_secs(5), async {
                while !directory.path().join("ready").exists() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await?;
            discovery.abort();
            assert!(discovery.await.is_err_and(|error| error.is_cancelled()));
            tokio::time::sleep(Duration::from_millis(2300)).await;
            assert!(!directory.path().join("escaped").exists());
        }
        Ok(())
    }

    #[tokio::test]
    async fn model_lookup_prefers_the_project_config_and_falls_back_to_the_catalog()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TempDir::new()?;
        let stub = write_stub(&directory, "codex.sh", PROTOCOL_STUB);
        let configured = directory.path().join("configured");
        std::fs::create_dir(&configured)?;

        let settings = project_models(spec(&stub, &configured)).await?;
        assert_eq!(settings.model.as_deref(), Some("project-model"));
        assert_eq!(settings.effort.as_deref(), Some("low"));
        assert_eq!(
            settings
                .models
                .iter()
                .map(|model| model.model.as_str())
                .collect::<Vec<_>>(),
            ["another-model", "catalog-default"]
        );
        let model = settings.models.last().ok_or("missing catalog default")?;
        assert_eq!(model.default_reasoning_effort.as_deref(), Some("low"));
        assert_eq!(
            model
                .supported_reasoning_efforts
                .first()
                .ok_or("missing advertised effort")?
                .reasoning_effort,
            "low"
        );
        assert_eq!(
            project_models(spec(&stub, directory.path()))
                .await?
                .model
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
    async fn failed_approval_delivery_preserves_the_request_for_retry()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut harness = CodexHarness::new();
        harness
            .pending
            .lock()
            .await
            .approvals
            .insert("item".into(), 42);
        let (disconnected, receiver) = mpsc::channel(MAX_OUTBOUND_MESSAGES);
        drop(receiver);
        harness.outgoing = Some(disconnected);
        assert!(matches!(
            harness
                .respond(ApprovalId::new("item"), Decision::Allow)
                .await,
            Err(HarnessError::ChannelClosed)
        ));
        assert_eq!(
            harness.pending.lock().await.approvals.get("item"),
            Some(&42)
        );
        assert!(harness.receiver.try_recv().is_err());

        let (connected, requests) = mpsc::channel(MAX_OUTBOUND_MESSAGES);
        let (writer, reader) = tokio::io::duplex(4096);
        drop(reader);
        let writer_task = spawn_writer(writer, requests, None, None);
        harness.outgoing = Some(connected);
        assert!(matches!(
            harness
                .respond(ApprovalId::new("item"), Decision::Allow)
                .await,
            Err(HarnessError::TransportClosed(_))
        ));
        writer_task.await?;
        assert_eq!(
            harness.pending.lock().await.approvals.get("item"),
            Some(&42)
        );
        assert!(harness.receiver.try_recv().is_err());

        assert!(matches!(
            harness.respond(ApprovalId::new("item"), Decision::Allow).await,
            Err(HarnessError::Protocol(message)) if message.contains("may already have been sent")
        ));
        handle_notification(
            "serverRequest/resolved",
            &json!({"requestId": 42}),
            &harness.sender,
            &harness.pending,
        )
        .await?;
        assert!(matches!(
            harness.receiver.try_recv(),
            Ok(HarnessEvent::ApprovalWithdrawn { .. })
        ));
        harness
            .pending
            .lock()
            .await
            .approvals
            .insert("item".into(), 43);

        let (connected, requests) = mpsc::channel(MAX_OUTBOUND_MESSAGES);
        let (writer, reader) = tokio::io::duplex(4096);
        let writer_task = spawn_writer(writer, requests, None, None);
        harness.outgoing = Some(connected);
        harness
            .respond(ApprovalId::new("item"), Decision::Allow)
            .await?;
        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        let request: Value = serde_json::from_str(&line)?;
        assert_eq!(request["id"], 43);
        assert_eq!(request["result"]["decision"], "accept");
        assert!(!harness.pending.lock().await.approvals.contains_key("item"));
        assert!(matches!(
            harness.receiver.try_recv(),
            Ok(HarnessEvent::ApprovalResolved {
                decision: Decision::Allow,
                ..
            })
        ));
        harness.outgoing = None;
        writer_task.await?;
        Ok(())
    }

    #[tokio::test]
    async fn approval_waits_for_writer_and_does_not_resolve_a_withdrawn_request()
    -> Result<(), Box<dyn std::error::Error>> {
        for withdraw in [false, true] {
            let mut harness = CodexHarness::new();
            harness
                .pending
                .lock()
                .await
                .approvals
                .insert("item".into(), 42);
            let pending = harness.pending.clone();
            let (connected, requests) = mpsc::channel(MAX_OUTBOUND_MESSAGES);
            let (writer, reader) = tokio::io::duplex(1);
            let writer_task = spawn_writer(writer, requests, None, None);
            harness.outgoing = Some(connected);
            let mut response = tokio::spawn(async move {
                let result = harness
                    .respond(ApprovalId::new("item"), Decision::Allow)
                    .await;
                (harness, result)
            });
            assert!(
                timeout(Duration::from_millis(50), &mut response)
                    .await
                    .is_err()
            );
            assert_eq!(pending.lock().await.approvals.get("item"), Some(&42));
            if withdraw {
                pending.lock().await.approvals.remove("item");
            }
            let reader_task = tokio::spawn(async move {
                let mut reader = BufReader::new(reader);
                let mut line = String::new();
                reader.read_line(&mut line).await?;
                Ok::<_, std::io::Error>(line)
            });
            let (mut harness, result) = response.await?;
            let line = reader_task.await??;
            let message: Value = serde_json::from_str(&line)?;
            assert_eq!(message["id"], 42);
            if withdraw {
                assert!(matches!(result, Err(HarnessError::NoPendingApproval)));
                assert!(harness.receiver.try_recv().is_err());
            } else {
                result?;
                assert!(matches!(
                    harness.receiver.try_recv(),
                    Ok(HarnessEvent::ApprovalResolved { .. })
                ));
            }
            harness.outgoing = None;
            writer_task.await?;
        }
        Ok(())
    }

    #[tokio::test]
    async fn cancelled_approval_write_cannot_be_retried_while_delivery_is_unknown()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut harness = CodexHarness::new();
        harness
            .pending
            .lock()
            .await
            .approvals
            .insert("item".into(), 42);
        let (connected, requests) = mpsc::channel(MAX_OUTBOUND_MESSAGES);
        let (writer, reader) = tokio::io::duplex(1);
        let writer_task = spawn_writer(writer, requests, None, None);
        harness.outgoing = Some(connected);
        let mut response = Box::pin(harness.respond(ApprovalId::new("item"), Decision::Allow));
        assert!(
            timeout(Duration::from_millis(50), &mut response)
                .await
                .is_err()
        );
        drop(response);
        assert!(matches!(
            harness.respond(ApprovalId::new("item"), Decision::Deny).await,
            Err(HarnessError::Protocol(message)) if message.contains("may already have been sent")
        ));
        let reader_task = tokio::spawn(async move {
            let mut reader = BufReader::new(reader);
            let mut line = String::new();
            reader.read_line(&mut line).await?;
            Ok::<_, std::io::Error>(line)
        });
        let message: Value = serde_json::from_str(&reader_task.await??)?;
        assert_eq!(message["result"]["decision"], "accept");
        handle_notification(
            "serverRequest/resolved",
            &json!({"requestId": 42}),
            &harness.sender,
            &harness.pending,
        )
        .await?;
        assert!(harness.pending.lock().await.approval_writes.is_empty());
        harness.outgoing = None;
        writer_task.await?;
        Ok(())
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
    async fn exec_rejects_oversized_output_and_stops_the_process()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TempDir::new()?;
        let stub = write_stub(
            &directory,
            "oversized-exec",
            r##"#!/usr/bin/env bash
(sleep 3; printf escaped > escaped) &
python3 -c 'import sys; sys.stdout.write("x" * (8 * 1024 * 1024 + 2)); sys.stdout.flush()'
sleep 30
"##,
        );
        let (mut harness, _) = start_exec(&stub, directory.path()).await;
        harness.prompt(PromptInput::text("go")).await?;
        let events = collect_until(&mut harness, |events| {
            events
                .iter()
                .any(|event| matches!(event, HarnessEvent::TurnFinished { .. }))
        })
        .await;
        assert!(events.iter().any(|event| matches!(event, HarnessEvent::TurnError { kind: FailureKind::Protocol, text, .. } if text.contains("exceeds 8388608 bytes"))), "{events:?}");
        assert!(
            events.iter().any(|event| matches!(
                event,
                HarnessEvent::TurnFinished {
                    reason: TurnFinishReason::Failed
                }
            )),
            "{events:?}"
        );
        harness.shutdown().await?;
        tokio::time::sleep(Duration::from_millis(3300)).await;
        assert!(!directory.path().join("escaped").exists());
        Ok(())
    }

    #[tokio::test]
    async fn exec_keeps_draining_output_while_it_is_progressing()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TempDir::new()?;
        let stub = write_stub(
            &directory,
            "codex-progress",
            r##"#!/usr/bin/env bash
(
  sleep 0.4
  printf '{"id":"1","msg":{"type":"agent_message","message":"fi'
  sleep 0.4
  printf 'rs'
  sleep 0.4
  printf 't"}}\n'
  printf '{"id":"2","msg":{"type":"agent_message","message":"second"}}\n'
) &
exit 0
"##,
        );
        let (mut harness, _) = start_exec(&stub, directory.path()).await;
        harness.prompt(PromptInput::text("go")).await?;
        let events = collect_until(&mut harness, |events| {
            events
                .iter()
                .any(|event| matches!(event, HarnessEvent::TurnFinished { .. }))
        })
        .await;
        let messages: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                HarnessEvent::Delta { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(messages, ["first", "second"]);
        assert!(events.iter().any(|event| matches!(
            event,
            HarnessEvent::TurnFinished {
                reason: TurnFinishReason::Completed
            }
        )));
        harness.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn exec_reports_an_output_pipe_that_never_closes()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TempDir::new()?;
        let stub = write_stub(
            &directory,
            "codex-open-pipe",
            "#!/usr/bin/env bash\nsleep 2 &\nexit 0\n",
        );
        let (mut harness, _) = start_exec(&stub, directory.path()).await;
        harness.prompt(PromptInput::text("go")).await?;
        let events = collect_until(&mut harness, |events| {
            events
                .iter()
                .any(|event| matches!(event, HarnessEvent::TurnFinished { .. }))
        })
        .await;
        assert!(events.iter().any(|event| matches!(event, HarnessEvent::TurnError { text, .. } if text.contains("output did not close"))));
        assert!(events.iter().any(|event| matches!(
            event,
            HarnessEvent::TurnFinished {
                reason: TurnFinishReason::Failed
            }
        )));
        harness.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn app_server_approval_retains_capacity_after_resolution()
    -> Result<(), Box<dyn std::error::Error>> {
        let (sender, mut events) = mpsc::unbounded_channel();
        let (outgoing, _receiver) = mpsc::channel(MAX_OUTBOUND_MESSAGES);
        let pending = Arc::new(AsyncMutex::new(Pending::default()));
        let capacity = Arc::new(tokio::sync::Semaphore::new(1));
        pending.lock().await.stream_capacity = capacity.clone();
        let approval = |id, item: &str| json!({"id":id,"method":"item/commandExecution/requestApproval","params":{"itemId":item}});
        route_incoming(approval(1, "one"), &sender, &pending, &outgoing).await?;
        let first = events.recv().await.ok_or("missing approval")?;
        let copy = first.clone();
        drop(first);
        handle_notification(
            "serverRequest/resolved",
            &json!({"requestId":1}),
            &sender,
            &pending,
        )
        .await?;
        assert!(pending.lock().await.approvals.is_empty());
        let error = route_incoming(approval(2, "two"), &sender, &pending, &outgoing).await;
        assert!(
            matches!(error, Err(HarnessError::Protocol(message)) if message.contains("queue exceeded"))
        );
        assert!(pending.lock().await.approvals.is_empty());
        let (reply, response) = oneshot::channel();
        pending.lock().await.responses.insert(99, reply);
        route_incoming(
            json!({"id":99,"result":{"ok":true}}),
            &sender,
            &pending,
            &outgoing,
        )
        .await?;
        assert_eq!(response.await??["ok"], true);
        drop(copy);
        assert_eq!(capacity.available_permits(), 1);
        route_incoming(approval(2, "two"), &sender, &pending, &outgoing).await?;
        assert_eq!(pending.lock().await.approvals.len(), 1);
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unidentified_completion_stops_app_server_and_releases_waiters()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TempDir::new()?;
        let line = r#"{"method":"turn/completed","params":{"turn":{"status":"completed"}}}"#;
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", &format!("(sleep 1; printf escaped > escaped) >/dev/null 2>&1 & printf '%s\\n' '{line}'; exec sleep 30")])
            .current_dir(directory.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .process_group(0);
        let mut child = command.spawn()?;
        let stdout = child.stdout.take().ok_or("missing stdout")?;
        let child = Arc::new(AsyncMutex::new(child));
        let (sender, mut events) = mpsc::unbounded_channel();
        let pending = Arc::new(AsyncMutex::new(Pending::default()));
        let (response, response_waiter) = oneshot::channel();
        {
            let mut state = pending.lock().await;
            state.start_request = Some(1);
            state.responses.insert(1, response);
        }
        let (outgoing, _requests) = mpsc::channel(1);
        let (_writer, writer_failed) = oneshot::channel();
        let (drained, stderr_drained) = oneshot::channel();
        drained.send(()).map_err(|_| "stderr receiver closed")?;
        let reader = spawn_reader(
            stdout,
            sender,
            pending,
            outgoing,
            Arc::new(Mutex::new(VecDeque::new())),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            stderr_drained,
            writer_failed,
            Arc::downgrade(&child),
            stop_codex_process_group,
            None,
        );
        assert!(
            matches!(timeout(Duration::from_secs(5), events.recv()).await?,
            Some(HarnessEvent::ConnectionFailed { kind: FailureKind::Protocol, text }) if text.contains("missing its turn ID"))
        );
        assert!(
            timeout(Duration::from_secs(1), response_waiter)
                .await?
                .is_err()
        );
        reader.await?;
        assert!(child.lock().await.try_wait()?.is_some());
        tokio::time::sleep(Duration::from_millis(1300)).await;
        assert!(!directory.path().join("escaped").exists());
        Ok(())
    }

    #[tokio::test]
    async fn app_server_stream_overflow_stops_child_and_releases_waiters()
    -> Result<(), Box<dyn std::error::Error>> {
        let line = r#"{"method":"item/agentMessage/delta","params":{"itemId":"message","delta":"fixture"}}"#;
        let mut command = Command::new("/bin/sh");
        command
            .args([
                "-c",
                &format!("printf '%s\\n' '{line}' '{line}'; exec sleep 30"),
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .process_group(0);
        let mut child = command.spawn()?;
        let stdout = child.stdout.take().ok_or("missing stdout")?;
        let child = Arc::new(AsyncMutex::new(child));
        let (sender, mut events) = mpsc::unbounded_channel();
        let pending = Arc::new(AsyncMutex::new(Pending::default()));
        pending.lock().await.stream_capacity = Arc::new(tokio::sync::Semaphore::new(1));
        let (response, response_waiter) = oneshot::channel();
        pending.lock().await.responses.insert(1, response);
        let (outgoing, _requests) = mpsc::channel(1);
        let (_writer, writer_failed) = oneshot::channel();
        let (drained, stderr_drained) = oneshot::channel();
        drained.send(()).map_err(|_| "stderr receiver closed")?;
        let reader = spawn_reader(
            stdout,
            sender,
            pending,
            outgoing,
            Arc::new(Mutex::new(VecDeque::new())),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            stderr_drained,
            writer_failed,
            Arc::downgrade(&child),
            stop_codex_process_group,
            None,
        );
        let first = timeout(Duration::from_secs(5), events.recv())
            .await?
            .ok_or("missing first event")?;
        assert!(matches!(&first, HarnessEvent::Delta { .. }));
        let failure = timeout(Duration::from_secs(5), events.recv())
            .await?
            .ok_or("missing failure")?;
        assert!(
            matches!(failure, HarnessEvent::ConnectionFailed { text, .. } if text.contains("queue exceeded"))
        );
        reader.await?;
        assert!(
            timeout(Duration::from_secs(1), response_waiter)
                .await?
                .is_err()
        );
        assert!(child.lock().await.try_wait()?.is_some());
        drop(first);
        Ok(())
    }

    #[tokio::test]
    async fn app_server_stream_saturation_preserves_control_replies()
    -> Result<(), Box<dyn std::error::Error>> {
        let (sender, mut events) = mpsc::unbounded_channel();
        let (outgoing, _receiver) = mpsc::channel(MAX_OUTBOUND_MESSAGES);
        let pending = Arc::new(AsyncMutex::new(Pending::default()));
        let capacity = Arc::new(tokio::sync::Semaphore::new(1));
        pending.lock().await.stream_capacity = capacity.clone();
        route_incoming(json!({"method":"item/agentMessage/delta","params":{"itemId":"message","delta":"first"}}), &sender, &pending, &outgoing).await?;
        let first = events.recv().await.ok_or("missing stream event")?;
        let copy = first.clone();
        drop(first);
        let (reply, result) = oneshot::channel();
        pending.lock().await.responses.insert(99, reply);
        route_incoming(
            json!({"id":99,"result":{"ok":true}}),
            &sender,
            &pending,
            &outgoing,
        )
        .await?;
        assert_eq!(result.await??["ok"], true);
        let error = route_incoming(json!({"method":"item/agentMessage/delta","params":{"itemId":"message","delta":"second"}}), &sender, &pending, &outgoing).await;
        assert!(
            matches!(error, Err(HarnessError::Protocol(message)) if message.contains("queue exceeded"))
        );
        drop(copy);
        assert_eq!(capacity.available_permits(), 1);
        route_incoming(json!({"method":"item/commandExecution/outputDelta","params":{"itemId":"tool","delta":"output"}}), &sender, &pending, &outgoing).await?;
        let output = events.recv().await.ok_or("missing command output")?;
        assert!(matches!(
            &output,
            HarnessEvent::CommandOutput {
                output_permit: Some(_),
                ..
            }
        ));
        assert_eq!(capacity.available_permits(), 0);
        drop(output);
        assert_eq!(capacity.available_permits(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn exec_plan_and_usage_share_stream_capacity() -> Result<(), Box<dyn std::error::Error>> {
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let capacity = Arc::new(tokio::sync::Semaphore::new(1));
        let blocked = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(AtomicU64::new(0));
        let producer = tokio::spawn({
            let capacity = capacity.clone();
            let blocked = blocked.clone();
            let progress = progress.clone();
            async move {
                for event in [
                    HarnessEvent::Plan {
                        rows: vec![PlanRow {
                            text: "step".into(),
                            status: "pending".into(),
                        }],
                        stream_permit: None,
                    },
                    HarnessEvent::Usage {
                        used: 1,
                        size: 100,
                        cost: None,
                        stream_permit: None,
                    },
                ] {
                    publish_exec_event(&sender, &capacity, &blocked, &progress, event).await?;
                }
                Ok::<_, ()>(())
            }
        });
        let plan = receiver.recv().await.ok_or("missing plan")?;
        assert!(matches!(plan, HarnessEvent::Plan { .. }));
        let retained = plan.clone();
        drop(plan);
        assert!(
            timeout(Duration::from_millis(30), receiver.recv())
                .await
                .is_err()
        );
        drop(retained);
        let usage = receiver.recv().await.ok_or("missing usage")?;
        assert!(matches!(usage, HarnessEvent::Usage { .. }));
        producer.await?.map_err(|()| "stream closed")?;
        assert_eq!(capacity.available_permits(), 0);
        drop(usage);
        assert_eq!(capacity.available_permits(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn exec_tool_events_share_stream_capacity_and_release_on_cancel()
    -> Result<(), Box<dyn std::error::Error>> {
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let capacity = Arc::new(tokio::sync::Semaphore::new(2));
        let blocked = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(AtomicU64::new(0));
        let producer = tokio::spawn({
            let capacity = capacity.clone();
            let blocked = blocked.clone();
            let progress = progress.clone();
            async move {
                for id in ["one", "two", "three"] {
                    publish_exec_event(
                        &sender,
                        &capacity,
                        &blocked,
                        &progress,
                        exec_tool_event(id, id.into(), "command", ToolStatus::Running, None),
                    )
                    .await?;
                }
                Ok::<_, ()>(())
            }
        });
        let first = receiver.recv().await.ok_or("missing first tool")?;
        let second = receiver.recv().await.ok_or("missing second tool")?;
        assert_eq!(capacity.available_permits(), 0);
        assert!(
            timeout(Duration::from_millis(30), receiver.recv())
                .await
                .is_err()
        );
        let retained = first.clone();
        drop(first);
        assert!(
            timeout(Duration::from_millis(30), receiver.recv())
                .await
                .is_err()
        );
        drop(retained);
        let third = receiver.recv().await.ok_or("missing third tool")?;
        producer.await?.map_err(|()| "stream closed")?;
        assert!(
            matches!(third, HarnessEvent::ToolCall { ref tool_call_id, .. } if tool_call_id.as_str() == "three")
        );
        drop(second);
        drop(third);
        assert_eq!(capacity.available_permits(), 2);
        let held = capacity.clone().acquire_many_owned(2).await?;
        let stalled = tokio::spawn({
            let capacity = capacity.clone();
            let blocked = blocked.clone();
            let progress = progress.clone();
            async move {
                let (sender, _receiver) = mpsc::unbounded_channel();
                publish_exec_event(
                    &sender,
                    &capacity,
                    &blocked,
                    &progress,
                    exec_tool_event("four", "four".into(), "command", ToolStatus::Running, None),
                )
                .await
            }
        });
        tokio::task::yield_now().await;
        stalled.abort();
        assert!(stalled.await.is_err());
        drop(held);
        assert_eq!(capacity.available_permits(), 2);
        Ok(())
    }

    #[tokio::test]
    async fn exec_stream_backpressure_bounds_chunks_and_preserves_unicode()
    -> Result<(), Box<dyn std::error::Error>> {
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let capacity = Arc::new(tokio::sync::Semaphore::new(2));
        let blocked = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(AtomicU64::new(0));
        let expected = "界".repeat(7_000);
        let task = tokio::spawn({
            let capacity = capacity.clone();
            let blocked = blocked.clone();
            let progress = progress.clone();
            let text = expected.clone();
            async move {
                publish_exec_event(
                    &sender,
                    &capacity,
                    &blocked,
                    &progress,
                    HarnessEvent::Delta {
                        item_id: ItemId::new("message"),
                        kind: DeltaKind::Message,
                        text,
                        stream_permit: None,
                    },
                )
                .await
            }
        });
        let first = receiver.recv().await.ok_or("missing first chunk")?;
        let second = receiver.recv().await.ok_or("missing second chunk")?;
        assert_eq!(capacity.available_permits(), 0);
        assert!(
            timeout(Duration::from_millis(30), receiver.recv())
                .await
                .is_err()
        );
        let HarnessEvent::Delta {
            text: first_text, ..
        } = &first
        else {
            return Err("unexpected event".into());
        };
        let mut output = first_text.clone();
        drop(first);
        let third = receiver.recv().await.ok_or("missing third chunk")?;
        for event in [&second, &third] {
            let HarnessEvent::Delta { text, .. } = event else {
                return Err("unexpected event".into());
            };
            assert!(text.len() <= EXEC_STREAM_CHUNK_BYTES);
            output.push_str(text);
        }
        task.await?.map_err(|()| "stream closed")?;
        assert_eq!(output, expected);
        drop(second);
        drop(third);
        assert_eq!(capacity.available_permits(), 2);
        Ok(())
    }

    #[tokio::test]
    async fn exec_backpressure_does_not_fail_post_exit_drain()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TempDir::new()?;
        let stub = write_stub(
            &directory,
            "exec-many.sh",
            r##"#!/bin/sh
number=0
while [ "$number" -lt 100 ]; do
    printf '{"id":"%s","msg":{"type":"agent_message","message":"message"}}\n' "$number"
    number=$((number + 1))
done
"##,
        );
        let (mut harness, _) = start_exec(&stub, directory.path()).await;
        harness.prompt(PromptInput::text("go")).await?;
        tokio::time::sleep(Duration::from_millis(1300)).await;
        let mut count = 0;
        let reason = timeout(Duration::from_secs(5), async {
            loop {
                match harness.events().recv().await {
                    Some(HarnessEvent::Delta { .. }) => count += 1,
                    Some(HarnessEvent::TurnFinished { reason }) => break Ok(reason),
                    Some(HarnessEvent::TurnError { text, .. }) => break Err(text),
                    Some(_) => {}
                    None => break Err("event stream closed".into()),
                }
            }
        })
        .await??;
        assert_eq!(count, 100);
        assert_eq!(reason, TurnFinishReason::Completed);
        harness.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn exec_stream_capacity_is_shared_across_turns() -> Result<(), Box<dyn std::error::Error>>
    {
        let directory = TempDir::new()?;
        let stub = write_stub(
            &directory,
            "exec-one.sh",
            "#!/bin/sh\nprintf '{\"id\":\"one\",\"msg\":{\"type\":\"agent_message\",\"message\":\"hello\"}}\\n'\n",
        );
        let (mut harness, _) = start_exec(&stub, directory.path()).await;
        harness.stream_capacity = Arc::new(tokio::sync::Semaphore::new(1));
        harness.prompt(PromptInput::text("first")).await?;
        timeout(Duration::from_secs(3), async {
            while !harness
                .run
                .as_ref()
                .is_some_and(|run| run.finished.load(Ordering::Relaxed))
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await?;
        harness.prompt(PromptInput::text("second")).await?;
        assert_eq!(harness.stream_capacity.available_permits(), 0);
        timeout(Duration::from_secs(3), async {
            loop {
                match harness.receiver.recv().await {
                    Some(HarnessEvent::Delta { .. }) => break Ok::<(), &'static str>(()),
                    Some(_) => {}
                    None => break Err("event stream closed"),
                }
            }
        })
        .await??;
        timeout(Duration::from_secs(3), async {
            while !harness
                .run
                .as_ref()
                .is_some_and(|run| run.finished.load(Ordering::Relaxed))
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await?;
        harness.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn cancelling_exec_unblocks_backpressured_output()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TempDir::new()?;
        let stub = write_stub(
            &directory,
            "exec-blocked.sh",
            r##"#!/bin/sh
number=0
while [ "$number" -lt 100 ]; do
    printf '{"id":"%s","msg":{"type":"agent_message","message":"message"}}\n' "$number"
    number=$((number + 1))
done
sleep 30
"##,
        );
        let (mut harness, _) = start_exec(&stub, directory.path()).await;
        harness.prompt(PromptInput::text("go")).await?;
        timeout(Duration::from_secs(3), async {
            while harness.receiver.len() < EXEC_STREAM_CHUNKS + 3 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await?;
        timeout(Duration::from_secs(3), harness.cancel()).await??;
        harness
            .shutdown_with_timeout(Duration::from_secs(3))
            .await?;
        Ok(())
    }

    #[tokio::test]
    async fn backpressure_pauses_the_post_exit_total_deadline()
    -> Result<(), Box<dyn std::error::Error>> {
        let progress = AtomicU64::new(0);
        let blocked = AtomicBool::new(true);
        let (sender, mut kill) = mpsc::unbounded_channel();
        let mut reader = tokio::spawn(std::future::pending::<Result<(), String>>());
        let request = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            let (reply, response) = oneshot::channel::<Result<(), String>>();
            sender.send(reply).map_err(|_| "receiver closed")?;
            response
                .await
                .map_err(|_| "reply closed")?
                .map_err(|_| "cancellation failed")
        });
        assert!(
            drain_exec_reader(
                &mut reader,
                &progress,
                &blocked,
                &mut kill,
                false,
                Duration::from_millis(20),
                Duration::from_millis(60)
            )
            .await?
        );
        request.await??;
        assert!(reader.await.is_err_and(|error| error.is_cancelled()));
        Ok(())
    }

    #[tokio::test]
    async fn exec_post_exit_drain_has_a_limit_even_with_continuous_progress()
    -> Result<(), Box<dyn std::error::Error>> {
        let progress = Arc::new(AtomicU64::new(0));
        let (_kill_sender, mut kill) = mpsc::unbounded_channel();
        let mut reader: JoinHandle<Result<(), String>> = tokio::spawn({
            let progress = progress.clone();
            async move {
                loop {
                    progress.fetch_add(1, Ordering::Relaxed);
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
        });
        let result = drain_exec_reader(
            &mut reader,
            &progress,
            &AtomicBool::new(false),
            &mut kill,
            false,
            Duration::from_millis(80),
            Duration::from_millis(250),
        )
        .await;
        assert!(matches!(result, Err(error) if error.contains("post-exit drain limit")));
        assert!(reader.await.is_err_and(|error| error.is_cancelled()));
        Ok(())
    }

    #[tokio::test]
    async fn cancellation_interrupts_the_post_exit_drain() -> Result<(), Box<dyn std::error::Error>>
    {
        let progress = AtomicU64::new(0);
        let (sender, mut kill) = mpsc::unbounded_channel();
        let mut reader = tokio::spawn(std::future::pending::<Result<(), String>>());
        let (reply, response) = oneshot::channel();
        sender
            .send(reply)
            .map_err(|_| "cancellation receiver closed")?;
        assert!(
            drain_exec_reader(
                &mut reader,
                &progress,
                &AtomicBool::new(false),
                &mut kill,
                false,
                Duration::from_secs(1),
                Duration::from_secs(10)
            )
            .await?
        );
        response.await??;
        assert!(reader.await.is_err_and(|error| error.is_cancelled()));
        Ok(())
    }

    #[tokio::test]
    async fn handshake_rejects_an_oversized_message() -> Result<(), Box<dyn std::error::Error>> {
        let directory = TempDir::new()?;
        let stub = write_stub(
            &directory,
            "oversized-handshake",
            r##"#!/usr/bin/env bash
python3 -c 'import sys; sys.stdout.write("x" * (8 * 1024 * 1024 + 2)); sys.stdout.flush()'
sleep 30
"##,
        );
        let result = timeout(
            Duration::from_secs(5),
            run_handshake(&stub, directory.path()),
        )
        .await?;
        let Err(error) = result else {
            return Err("oversized handshake succeeded".into());
        };
        assert!(
            error.to_string().contains("exceeds 8388608 bytes"),
            "{error}"
        );
        Ok(())
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
    async fn exec_tracks_each_run_and_retries_record_removal()
    -> Result<(), Box<dyn std::error::Error>> {
        struct Tracker {
            pid: Mutex<Option<u32>>,
            failures: std::sync::atomic::AtomicUsize,
            fail_start: bool,
        }
        #[async_trait]
        impl crate::AgentProcessTracker for Tracker {
            async fn started(&self, pid: u32) -> Result<String, String> {
                *self.pid.lock().map_err(|error| error.to_string())? = Some(pid);
                if self.fail_start {
                    return Err("fixture save failure".into());
                }
                Ok("exec-record".into())
            }
            async fn stopped(&self, id: &str) -> Result<(), String> {
                assert_eq!(id, "exec-record");
                if self
                    .failures
                    .try_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                        value.checked_sub(1)
                    })
                    .is_ok()
                {
                    return Err("fixture clear failure".into());
                }
                *self.pid.lock().map_err(|error| error.to_string())? = None;
                Ok(())
            }
        }
        for cancel in [false, true] {
            let directory = TempDir::new()?;
            let stub = write_stub(
                &directory,
                "tracked-exec.sh",
                if cancel { EXEC_SLOW_STUB } else { EXEC_STUB },
            );
            let tracker = Arc::new(Tracker {
                pid: Mutex::new(None),
                failures: std::sync::atomic::AtomicUsize::new(2),
                fail_start: false,
            });
            let mut harness = ExecHarness::new();
            harness.set_process_tracker(tracker.clone());
            harness.start(spec(&stub, directory.path())).await?;
            harness.open_session(None).await?;
            harness.prompt(PromptInput::text("go")).await?;
            assert!(
                tracker
                    .pid
                    .lock()
                    .map_err(|error| error.to_string())?
                    .is_some()
            );
            if cancel {
                harness.cancel().await?;
            }
            let events = collect_until(&mut harness, |events| {
                events
                    .iter()
                    .any(|event| matches!(event, HarnessEvent::TurnFinished { .. }))
            })
            .await;
            assert!(events.iter().any(|event| matches!(event, HarnessEvent::TurnError { text, .. } if text.contains("fixture clear failure"))));
            assert!(
                tracker
                    .pid
                    .lock()
                    .map_err(|error| error.to_string())?
                    .is_some()
            );
            let error = harness
                .prompt(PromptInput::text("next"))
                .await
                .expect_err("record removal must succeed before the next run");
            assert!(error.to_string().contains("fixture clear failure"));
            harness.shutdown().await?;
            assert!(
                tracker
                    .pid
                    .lock()
                    .map_err(|error| error.to_string())?
                    .is_none()
            );
        }
        #[cfg(unix)]
        {
            let directory = TempDir::new()?;
            let stub = write_stub(&directory, "failed-tracked-exec.sh", EXEC_SLOW_STUB);
            let tracker = Arc::new(Tracker {
                pid: Mutex::new(None),
                failures: std::sync::atomic::AtomicUsize::new(0),
                fail_start: true,
            });
            let mut harness = ExecHarness::new();
            harness.set_process_tracker(tracker.clone());
            harness.start(spec(&stub, directory.path())).await?;
            harness.open_session(None).await?;
            let error = harness
                .prompt(PromptInput::text("go"))
                .await
                .expect_err("tracking must fail");
            assert!(error.to_string().contains("fixture save failure"));
            let pid = tracker
                .pid
                .lock()
                .map_err(|error| error.to_string())?
                .ok_or("missing PID")?;
            timeout(Duration::from_secs(3), async {
                loop {
                    if unsafe { libc::kill(libc::pid_t::try_from(pid)?, 0) } != 0 {
                        break Ok::<(), Box<dyn std::error::Error>>(());
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await??;
            harness.shutdown().await?;
        }
        Ok(())
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
        assert!(
            events.iter().any(|event| matches!(
                event,
                HarnessEvent::TurnFinished {
                    reason: TurnFinishReason::Completed
                }
            )),
            "unexpected exec events: {events:?}"
        );
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
                ..PromptInput::default()
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
            HarnessEvent::TurnError { kind: FailureKind::AuthRequired, text, .. },
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
    async fn exec_shutdown_stops_its_process_group() -> Result<(), Box<dyn std::error::Error>> {
        let directory = TempDir::new()?;
        let stub = write_stub(
            &directory,
            "codex-exec-group.sh",
            "#!/bin/sh\n(sleep 1; printf escaped > escaped) &\nprintf ready > ready\nsleep 30\n",
        );
        let (mut harness, _) = start_exec(&stub, directory.path()).await;
        harness.prompt(PromptInput::text("go")).await?;
        timeout(Duration::from_secs(5), async {
            while !directory.path().join("ready").exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await?;
        harness.shutdown().await?;
        tokio::time::sleep(Duration::from_millis(1300)).await;
        assert!(!directory.path().join("escaped").exists());
        Ok(())
    }

    #[tokio::test]
    async fn exec_shutdown_timeout_can_be_retried() -> Result<(), Box<dyn std::error::Error>> {
        let mut harness = ExecHarness::new();
        harness.started = true;
        let (release, wait) = oneshot::channel::<()>();
        let waiter = tokio::spawn(async move { assert!(wait.await.is_ok()) });
        let reader = tokio::spawn(std::future::pending::<()>()).abort_handle();
        harness.run = Some(ExecRun {
            waiter,
            waiter_failure: None,
            reader,
            kill: None,
            wait_errors: mpsc::unbounded_channel().1,
            process_record: Arc::new(AsyncMutex::new(None)),
            stopping: false,
            finished: Arc::new(AtomicBool::new(false)),
        });

        assert!(
            harness
                .shutdown_with_timeout(Duration::from_millis(30))
                .await
                .is_err()
        );
        assert!(harness.started && harness.run.is_some());
        assert!(matches!(
            harness.receiver.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        release.send(()).map_err(|_| "waiter already stopped")?;
        harness
            .shutdown_with_timeout(Duration::from_secs(1))
            .await?;
        assert!(!harness.started && harness.run.is_none());
        assert!(matches!(
            harness.receiver.try_recv(),
            Ok(HarnessEvent::Exited { .. })
        ));
        Ok(())
    }

    #[tokio::test]
    async fn failed_exec_stop_retains_the_child_for_shutdown_retry()
    -> Result<(), Box<dyn std::error::Error>> {
        for (cancel_first, child_failure) in
            [(false, false), (true, false), (false, true), (true, true)]
        {
            let mut command = Command::new("/bin/sh");
            command.args(["-c", "sleep 30"]).kill_on_drop(true);
            #[cfg(unix)]
            command.process_group(0);
            let child = command.spawn()?;
            #[cfg(unix)]
            let pid = libc::pid_t::try_from(child.id().ok_or("missing child PID")?)?;
            let mut harness = ExecHarness::new();
            harness.started = true;
            let (kill, requests) = mpsc::unbounded_channel();
            let (wait_errors, wait_error_receiver) = mpsc::unbounded_channel();
            let (_stderr_done, stderr_drained) = oneshot::channel();
            let finished = Arc::new(AtomicBool::new(false));
            let reader = tokio::spawn(std::future::pending::<Result<(), String>>());
            let reader_abort = reader.abort_handle();
            let mut attempts = 0;
            let waiter = spawn_exec_waiter(
                ExecWaiter {
                    child,
                    kill: requests,
                    sender: harness.sender.clone(),
                    stderr: Arc::clone(&harness.stderr),
                    stderr_drained,
                    finished: Arc::clone(&finished),
                    reader,
                    progress: Arc::new(AtomicU64::new(0)),
                    backpressured: Arc::new(AtomicBool::new(false)),
                    wait_errors,
                    process_record: Arc::new(AsyncMutex::new(None)),
                },
                move |child| {
                    attempts += 1;
                    if attempts == 1 {
                        stop_exec_child_with(
                            child,
                            |_| {
                                if child_failure {
                                    Ok(())
                                } else {
                                    Err(std::io::Error::other("fixture stop failure"))
                                }
                            },
                            |_| Err(std::io::Error::other("fixture stop failure")),
                        )
                    } else {
                        stop_exec_child(child)
                    }
                },
                || None,
            );
            harness.run = Some(ExecRun {
                waiter,
                waiter_failure: None,
                reader: reader_abort,
                kill: Some(kill),
                wait_errors: wait_error_receiver,
                process_record: Arc::new(AsyncMutex::new(None)),
                stopping: false,
                finished,
            });
            let failure = if cancel_first {
                harness.cancel().await
            } else {
                harness.shutdown_with_timeout(Duration::from_secs(3)).await
            };
            let error = failure.expect_err("the injected stop must fail");
            assert!(error.to_string().contains("fixture stop failure"));
            assert!(harness.started && harness.run.is_some());
            #[cfg(unix)]
            assert_eq!(unsafe { libc::kill(pid, 0) }, 0);
            assert!(matches!(
                harness.receiver.try_recv(),
                Err(mpsc::error::TryRecvError::Empty)
            ));
            harness
                .shutdown_with_timeout(Duration::from_secs(3))
                .await?;
            assert!(!harness.started && harness.run.is_none());
        }
        Ok(())
    }

    #[tokio::test]
    async fn exec_wait_failure_retains_child_for_cancel_or_shutdown_retry()
    -> Result<(), Box<dyn std::error::Error>> {
        for (cancel_first, retry_cancel) in
            [(false, false), (true, false), (false, true), (true, true)]
        {
            let mut command = Command::new("/bin/sh");
            command.args(["-c", "sleep 30"]).kill_on_drop(true);
            #[cfg(unix)]
            command.process_group(0);
            let child = command.spawn()?;
            let mut harness = ExecHarness::new();
            harness.started = true;
            let (kill, requests) = mpsc::unbounded_channel();
            let (wait_errors, wait_error_receiver) = mpsc::unbounded_channel();
            let (_stderr_done, stderr_drained) = oneshot::channel();
            let finished = Arc::new(AtomicBool::new(false));
            let reader = tokio::spawn(std::future::pending::<Result<(), String>>());
            let reader_abort = reader.abort_handle();
            let inject = Arc::new(AtomicBool::new(!cancel_first));
            let stop_inject = inject.clone();
            let waiter = spawn_exec_waiter(
                ExecWaiter {
                    child,
                    kill: requests,
                    sender: harness.sender.clone(),
                    stderr: Arc::clone(&harness.stderr),
                    stderr_drained,
                    finished: finished.clone(),
                    reader,
                    progress: Arc::new(AtomicU64::new(0)),
                    backpressured: Arc::new(AtomicBool::new(false)),
                    wait_errors,
                    process_record: Arc::new(AsyncMutex::new(None)),
                },
                move |child| {
                    let result = stop_exec_child(child);
                    if cancel_first {
                        stop_inject.store(true, Ordering::SeqCst);
                    }
                    result
                },
                move || {
                    inject
                        .swap(false, Ordering::SeqCst)
                        .then(|| std::io::Error::other("fixture wait failure"))
                },
            );
            harness.run = Some(ExecRun {
                waiter,
                waiter_failure: None,
                reader: reader_abort,
                kill: Some(kill),
                wait_errors: wait_error_receiver,
                process_record: Arc::new(AsyncMutex::new(None)),
                stopping: false,
                finished,
            });
            if cancel_first {
                harness.cancel().await?;
            }
            let event = timeout(Duration::from_secs(3), harness.receiver.recv()).await?;
            assert!(
                matches!(event, Some(HarnessEvent::TurnError { text, .. }) if text.contains("fixture wait failure"))
            );
            let result = if retry_cancel {
                harness.cancel().await
            } else {
                harness.shutdown_with_timeout(Duration::from_secs(3)).await
            };
            let error = result.expect_err("wait failure must be reported");
            assert!(error.to_string().contains("fixture wait failure"));
            assert!(harness.started && harness.run.is_some());
            assert!(matches!(
                harness.receiver.try_recv(),
                Err(mpsc::error::TryRecvError::Empty)
            ));
            if retry_cancel {
                harness.cancel().await?;
            }
            harness
                .shutdown_with_timeout(Duration::from_secs(3))
                .await?;
            assert!(!harness.started && harness.run.is_none());
        }
        Ok(())
    }

    #[tokio::test]
    async fn exec_shutdown_waiter_failure_is_stable_on_retry() {
        let mut harness = ExecHarness::new();
        harness.started = true;
        let waiter = tokio::spawn(async { panic!("fixture waiter failed") });
        let reader = tokio::spawn(std::future::pending::<()>()).abort_handle();
        harness.run = Some(ExecRun {
            waiter,
            waiter_failure: None,
            reader,
            kill: None,
            wait_errors: mpsc::unbounded_channel().1,
            process_record: Arc::new(AsyncMutex::new(None)),
            stopping: false,
            finished: Arc::new(AtomicBool::new(false)),
        });

        let first = harness.shutdown_with_timeout(Duration::from_secs(1)).await;
        let second = harness.shutdown_with_timeout(Duration::from_secs(1)).await;
        assert!(first.is_err() && second.is_err());
        assert!(harness.started && harness.run.is_some());
        if let Some(run) = harness.run.take() {
            run.reader.abort();
        }
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
            stream_permit: None,
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
    async fn command_reply_is_delivered_after_unconsumed_stream_events()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TempDir::new()?;
        let stub = write_stub(
            &directory,
            "codex-events-before-reply.sh",
            r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s\n' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*) printf '{"id":%s,"result":{}}\n' "$id" ;;
    *'"method":"thread/start"'*) printf '{"id":%s,"result":{"thread":{"id":"fixture"}}}\n' "$id" ;;
    *'"method":"turn/start"'*)
      i=0
      while [ "$i" -lt 100 ]; do
        printf '%s\n' '{"method":"item/agentMessage/delta","params":{"itemId":"message","delta":"chunk"}}'
        i=$((i+1))
      done
      printf '{"id":%s,"result":{"turn":{"id":"turn","status":"inProgress"}}}\n' "$id"
      ;;
  esac
done
"#,
        );
        let (mut harness, _) = start_session(&stub, directory.path()).await;
        timeout(
            Duration::from_secs(5),
            harness.prompt(PromptInput::text("go")),
        )
        .await??;
        let mut chunks = 0;
        while let Ok(event) = harness.receiver.try_recv() {
            if matches!(event, HarnessEvent::Delta { .. }) {
                chunks += 1;
            }
        }
        assert_eq!(chunks, 100);
        harness.shutdown().await?;
        Ok(())
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
    /// Run with `cargo test -p agent-harness -- --ignored live_codex` to replay.
    /// Add `--features live-codex` before `--` to record a missing fixture.
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
