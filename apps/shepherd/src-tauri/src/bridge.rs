//! The shell's command and event boundary.
//!
//! One method per core [`Command`], plus snapshot readers for a fresh window and
//! a single event stream. Methods return the core's own typed snapshots; the
//! shell does not keep a second copy of the state model.
//!
//! Core events are drained after every command and broadcast to whatever window
//! is subscribed. Adapter events that arrive between commands are folded by a
//! event wakeup ([`ShellState::start_event_pump`]) and broadcast the same way,
//! so a streaming turn paints without waiting for the next invoke.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use agent_harness::acp::{
    TerminalBackend, TerminalExitStatus, TerminalLaunch, TerminalProcessEvent,
};
use agent_harness::codex::CodexProjectModels;
use agent_harness::{AgentMode, CodexTurnOptions, Decision, PromptInput, SpawnSpec};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use shepherd_core::files::{self, DirectoryListing, FileSearchQuery, FileSearchResult, TextFile};
use shepherd_core::git::GitService;
use shepherd_core::harness::HarnessFactory;
use shepherd_core::store::{
    PoolCatalogAdvisory, PoolCatalogOutcome, PoolCooldownClearRequest, PoolQuotaCorrectionOutcome,
    PoolQuotaCorrectionRequest, PoolReorderOutcome, PoolReorderRequest, PoolSlot,
    PoolSlotEnabledOutcome, PoolSlotEnabledRequest, PoolSlotId, PoolStatus, Store, TursoStore,
};
use shepherd_core::terminal::{
    InspectorTab, ProcessRecord, TerminalEvent, TerminalId, TerminalList, TerminalRecord,
    TerminalService, TerminalSpec,
};
use shepherd_core::{
    Agent, Command, CommandResult, Core, CoreError, Event, PoolForceOutcome, PoolForceRequest,
    ProjectApprovalsSnapshot, ProjectId, ProjectList, QuitSummary, TaskId, TaskList, TaskState,
    TaskWorkspace, WorkspaceLayout,
};
use tokio::sync::{Mutex, Semaphore, broadcast, mpsc, oneshot, watch};
use tokio::time::Instant;

use crate::attachments::{self, AttachmentLimit};
use crate::codex_harness::codex_spawn_spec;
use crate::file_actions::{self, FileAction};
use crate::file_watch::{FileWatchUpdate, WatchError, WorkspaceWatch};
use crate::preview_annotations::{AnnotationBatch, validate_batch};
use crate::pty_terminal::PtyTerminals;
use crate::settings::{DEFAULT_ATTACHMENT_BYTES, DEFAULT_SCROLLBACK, SettingKey, Settings};
use crate::terminal_screen::{Screen, SearchDirection, TerminalAppearance, TerminalFrame};

const FILE_WATCH_START_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_PENDING_FILE_WATCH_STARTS: usize = 4;
const FILE_WATCH_RETRY_DELAY: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error, Serialize)]
#[error("{message}")]
pub struct BridgeError {
    code: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    causes: Vec<String>,
    #[serde(rename = "quitConfirmation", skip_serializing_if = "Option::is_none")]
    quit_confirmation: Option<QuitConfirmation>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct QuitConfirmation {
    live_tasks: usize,
    live_terminals: usize,
}

#[derive(Serialize)]
pub struct LocalCodexProject {
    name: String,
    path: String,
}

impl BridgeError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            causes: Vec::new(),
            quit_confirmation: None,
        }
    }
}

fn error_causes(error: &(dyn std::error::Error + 'static)) -> Vec<String> {
    let mut causes = Vec::new();
    let mut source = error.source();
    while let Some(error) = source {
        causes.push(error.to_string());
        source = error.source();
    }
    causes
}

impl From<CoreError> for BridgeError {
    fn from(error: CoreError) -> Self {
        let mut bridge_error = Self::new("core", error.to_string());
        bridge_error.causes = error_causes(&error);
        if let CoreError::QuitConfirmationRequired {
            live_tasks,
            live_terminals,
        } = error
        {
            bridge_error.quit_confirmation = Some(QuitConfirmation {
                live_tasks,
                live_terminals,
            });
        }
        bridge_error
    }
}

impl From<files::FileError> for BridgeError {
    fn from(error: files::FileError) -> Self {
        let mut bridge_error = Self::new("files", error.to_string());
        bridge_error.causes = error_causes(&error);
        bridge_error
    }
}

#[derive(Serialize)]
pub struct ProjectRemote {
    label: String,
    forge: Option<RemoteForge>,
}

#[derive(Serialize)]
#[serde(rename_all = "lowercase")]
enum RemoteForge {
    GitHub,
    GitLab,
    HuggingFace,
}

impl ProjectRemote {
    fn from_url(remote: &str) -> Self {
        let (host, label) = if let Ok(url) = tauri::Url::parse(remote)
            && let Some(host) = url.host_str()
        {
            let authority = url
                .port()
                .map_or_else(|| host.to_string(), |port| format!("{host}:{port}"));
            (Some(host.to_string()), format!("{authority}{}", url.path()))
        } else if let Some((authority, path)) = remote.split_once(':')
            && !authority.contains(['/', '\\'])
            && !path.starts_with("//")
        {
            let host = authority.rsplit('@').next().unwrap_or(authority);
            (Some(host.to_ascii_lowercase()), format!("{host}/{path}"))
        } else {
            (None, remote.to_string())
        };
        let forge = match host.as_deref() {
            Some("github.com" | "ssh.github.com") => Some(RemoteForge::GitHub),
            Some("gitlab.com" | "altssh.gitlab.com") => Some(RemoteForge::GitLab),
            Some("huggingface.co" | "hf.co") => Some(RemoteForge::HuggingFace),
            _ => None,
        };
        let label = label.split(['?', '#']).next().unwrap_or_default();
        let label = if forge.is_some() {
            label
                .split_once('/')
                .map_or(label, |(_, path)| path.trim_end_matches('/'))
        } else {
            label
        };
        Self {
            label: label.strip_suffix(".git").unwrap_or(label).to_string(),
            forge,
        }
    }
}

fn unexpected(result: &CommandResult) -> BridgeError {
    BridgeError::new("bridge", format!("unexpected command result: {result:?}"))
}

#[derive(Deserialize)]
#[serde(untagged)]
pub enum TerminalInput {
    Text(String),
    Bytes(Vec<u8>),
}

impl TerminalInput {
    fn into_bytes(self) -> Vec<u8> {
        match self {
            Self::Text(text) => text.into_bytes(),
            Self::Bytes(bytes) => bytes,
        }
    }
}

struct ScreenSlot {
    revision: u32,
    screen: Screen,
    frames: watch::Sender<Arc<TerminalFrame>>,
    task_id: Option<TaskId>,
    last_worktree_touch: Option<Instant>,
}

impl ScreenSlot {
    fn publish(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        if self.frames.receiver_count() > 0 {
            let mut frame = self.screen.frame();
            frame.revision = self.revision;
            self.frames.send_replace(Arc::new(frame));
        }
    }

    fn follow_output(&mut self) {
        if self.screen.follow_output() {
            self.publish();
        }
    }

    fn subscribe(&mut self) -> watch::Receiver<Arc<TerminalFrame>> {
        let first = self.frames.receiver_count() == 0;
        let receiver = self.frames.subscribe();
        if first {
            self.publish();
        }
        receiver
    }
}

#[derive(Clone)]
pub(crate) struct TerminalHost {
    engine: Arc<Mutex<PtyTerminals>>,
    screens: Arc<Mutex<BTreeMap<String, Arc<Mutex<ScreenSlot>>>>>,
    appearance: Arc<Mutex<TerminalAppearance>>,
    process_store: Arc<OnceLock<Arc<dyn Store>>>,
    process_leases: Arc<OnceLock<Arc<crate::orphan_process::ProcessLeases>>>,
    core: Arc<OnceLock<std::sync::Weak<Mutex<Core>>>>,
}

struct AgentProcessStore {
    store: Arc<OnceLock<Arc<dyn Store>>>,
    task_id: String,
    leases: Option<Arc<crate::orphan_process::ProcessLeases>>,
    token: String,
}

#[async_trait]
impl agent_harness::AgentProcessTracker for AgentProcessStore {
    async fn started(&self, pid: u32) -> Result<String, String> {
        let store = self.store.get().ok_or("process storage is unavailable")?;
        #[cfg(unix)]
        let record = {
            let pid = libc::pid_t::try_from(pid).map_err(|error| error.to_string())?;
            crate::pty_terminal::capture_process_record(pid, "agent", &self.task_id, &self.token)
                .ok_or("could not capture agent process identity")?
        };
        #[cfg(not(unix))]
        return Err(format!(
            "agent process identity is unavailable for PID {pid}"
        ));
        #[cfg(unix)]
        {
            self.leases
                .as_ref()
                .ok_or("agent process leases are unavailable")?
                .claim(&record.id)
                .map_err(|error| format!("could not acquire agent process lease: {error}"))?;
            store
                .save_process(&record)
                .await
                .map_err(|error| error.to_string())?;
            Ok(record.id)
        }
    }

    async fn stopped(&self, record_id: &str) -> Result<(), String> {
        let store = self.store.get().ok_or("process storage is unavailable")?;
        let record = store
            .load_processes()
            .await
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|record| record.id == record_id);
        let Some(record) = record else {
            return self
                .leases
                .as_ref()
                .ok_or("agent process leases are unavailable")?
                .release(record_id)
                .map_err(|error| error.to_string());
        };
        if record.kind != "agent" || record.owner_id != self.task_id {
            return Err("agent process record ownership changed".into());
        }
        #[cfg(unix)]
        {
            let has_members = tokio::task::spawn_blocking(move || {
                crate::pty_terminal::process_group_has_other_members(&record)
            })
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| format!("could not inspect agent process group: {error}"))?;
            if has_members {
                return Ok(());
            }
        }
        store
            .remove_process(record_id)
            .await
            .map_err(|error| error.to_string())?;
        self.leases
            .as_ref()
            .ok_or("agent process leases are unavailable")?
            .release(record_id)
            .map_err(|error| error.to_string())
    }
}

impl TerminalHost {
    pub(crate) fn agent_process_tracker(
        &self,
        task_id: &TaskId,
        token: String,
    ) -> Arc<dyn agent_harness::AgentProcessTracker> {
        Arc::new(AgentProcessStore {
            store: self.process_store.clone(),
            task_id: task_id.to_string(),
            leases: self.process_leases.get().cloned(),
            token,
        })
    }

    pub(crate) fn process_leases(&self) -> Option<Arc<crate::orphan_process::ProcessLeases>> {
        self.process_leases.get().cloned()
    }
}

impl Default for TerminalHost {
    fn default() -> Self {
        Self {
            engine: Arc::new(Mutex::new(PtyTerminals::new())),
            screens: Arc::new(Mutex::new(BTreeMap::new())),
            appearance: Arc::new(Mutex::new(TerminalAppearance::default())),
            process_store: Arc::new(OnceLock::new()),
            process_leases: Arc::new(OnceLock::new()),
            core: Arc::new(OnceLock::new()),
        }
    }
}

struct PreparedTerminal {
    id: TerminalId,
    process: Option<ProcessRecord>,
    slot: Arc<Mutex<ScreenSlot>>,
    output: mpsc::Receiver<TerminalEvent>,
}

struct RecordedTerminal {
    record: TerminalRecord,
    process_id: Option<String>,
    core: Arc<Mutex<Core>>,
    events: broadcast::Sender<Event>,
}

struct TrackedProcess {
    id: Option<String>,
    store: Arc<dyn Store>,
}

impl TerminalHost {
    async fn prepare(
        &self,
        spec: TerminalSpec,
        scrollback: usize,
    ) -> Result<PreparedTerminal, BridgeError> {
        let (columns, rows) = (spec.columns, spec.rows);
        let (events, output) = mpsc::channel(32);
        let mut screens = self.screens.lock().await;
        let id = self
            .engine
            .lock()
            .await
            .spawn(spec, events)
            .map_err(|error| BridgeError::new("terminal", error.to_string()))?;
        let process = self
            .engine
            .lock()
            .await
            .process_record(&id)
            .map_err(|error| BridgeError::new("terminal", error.to_string()))?;
        let mut screen =
            Screen::with_scrollback(usize::from(columns), usize::from(rows), scrollback);
        screen.set_appearance(*self.appearance.lock().await);
        let (frames, _) = watch::channel(Arc::new(screen.frame()));
        let slot = Arc::new(Mutex::new(ScreenSlot {
            revision: 0,
            screen,
            frames,
            task_id: None,
            last_worktree_touch: None,
        }));
        screens.insert(id.as_str().to_string(), slot.clone());
        Ok(PreparedTerminal {
            id,
            process,
            slot,
            output,
        })
    }

    async fn set_appearance(&self, appearance: TerminalAppearance) {
        let screens = self.screens.lock().await;
        *self.appearance.lock().await = appearance;
        for slot in screens.values() {
            let mut slot = slot.lock().await;
            if slot.screen.set_appearance(appearance) {
                slot.publish();
            }
        }
    }

    fn observe(
        &self,
        terminal: PreparedTerminal,
        recorded: Option<RecordedTerminal>,
        protocol: Option<mpsc::Sender<TerminalProcessEvent>>,
        tracked: Option<TrackedProcess>,
    ) {
        let engine = self.engine.clone();
        let screens = self.screens.clone();
        let core = self.core.get().cloned();
        let PreparedTerminal {
            id,
            process: _,
            slot,
            mut output,
        } = terminal;
        tauri::async_runtime::spawn(async move {
            let mut pending = None;
            let mut deadline = None;
            let mut protocol = protocol;
            let mut last_activity = None::<Instant>;
            loop {
                let event = if let Some(event) = pending.take() {
                    Ok(Some(event))
                } else if let Some(deadline) = deadline {
                    tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), output.recv())
                        .await
                } else {
                    Ok(output.recv().await)
                };
                let mut batch = Vec::new();
                match event {
                    Ok(Some(TerminalEvent::Output(bytes))) => {
                        batch.push(bytes);
                        for _ in 0..31 {
                            match output.try_recv() {
                                Ok(TerminalEvent::Output(bytes)) => batch.push(bytes),
                                Ok(event) => {
                                    pending = Some(event);
                                    break;
                                }
                                Err(_) => break,
                            }
                        }
                    }
                    Ok(None) => break,
                    Err(_) => {}
                    Ok(Some(TerminalEvent::Exited { exit_code, signal })) => {
                        screens.lock().await.remove(id.as_str());
                        if let Some(protocol) = &protocol
                            && protocol
                                .send(TerminalProcessEvent::Exited(
                                    TerminalExitStatus::new()
                                        .exit_code(
                                            exit_code
                                                .filter(|_| signal.is_none())
                                                .and_then(|code| u32::try_from(code).ok()),
                                        )
                                        .signal(signal),
                                ))
                                .await
                                .is_err()
                        {
                            tracing::debug!("ACP terminal output listener closed before exit");
                        }
                        if let Some(tracked) = &tracked {
                            let lingering = engine.lock().await.lingering_process_records(&id);
                            let mut captured = true;
                            match lingering {
                                Ok(processes) => {
                                    for process in processes {
                                        if let Err(error) =
                                            tracked.store.save_process(&process).await
                                        {
                                            captured = false;
                                            tracing::warn!(%error, "could not record remaining terminal process");
                                        }
                                    }
                                }
                                Err(error) => {
                                    captured = false;
                                    tracing::warn!(%error, "could not inspect remaining terminal processes");
                                }
                            }
                            if captured
                                && let Some(id) = &tracked.id
                                && let Err(error) = tracked.store.remove_process(id).await
                            {
                                tracing::warn!(%error, "could not clear terminal process record");
                            }
                            if let Some(recorded) = &recorded {
                                let mut core = recorded.core.lock().await;
                                if let Err(error) = core.end_terminal(&id, exit_code).await
                                    && !matches!(error, CoreError::TerminalNotFound { .. })
                                {
                                    tracing::warn!(%error, "could not mark the terminal ended");
                                }
                                for event in core.drain_events() {
                                    if let Err(error) = recorded.events.send(event) {
                                        tracing::debug!(%error, "no shell is subscribed to terminal events");
                                    }
                                }
                            }
                        }
                        break;
                    }
                }
                if protocol.is_some()
                    && !batch.is_empty()
                    && last_activity.is_none_or(|last| last.elapsed() >= Duration::from_secs(60))
                    && let Some(core) = core.as_ref().and_then(std::sync::Weak::upgrade)
                    && let Some(task_id) = slot.lock().await.task_id.clone()
                {
                    last_activity = Some(Instant::now());
                    tauri::async_runtime::spawn(async move {
                        if let Err(error) = core
                            .lock()
                            .await
                            .touch_archived_worktree_if_due(&task_id)
                            .await
                        {
                            tracing::warn!(%error, "could not record agent terminal activity");
                        }
                    });
                }
                let (replies, bell) = {
                    let mut slot = slot.lock().await;
                    if batch.is_empty() {
                        slot.screen.finish_sync();
                    } else {
                        for bytes in &batch {
                            slot.screen.process(bytes);
                        }
                    }
                    deadline = slot.screen.sync_deadline();
                    slot.publish();
                    (slot.screen.take_replies(), slot.screen.take_bell())
                };
                if let Some(sender) = &protocol {
                    for bytes in batch {
                        if sender
                            .send(TerminalProcessEvent::Output(bytes))
                            .await
                            .is_err()
                        {
                            protocol = None;
                            break;
                        }
                    }
                }
                if bell
                    && let Some(recorded) = &recorded
                    && let Err(error) = recorded.events.send(Event::TerminalBell {
                        project_id: recorded.record.project_id.clone(),
                        task_id: recorded
                            .record
                            .task_id
                            .as_ref()
                            .map(|id| id.as_str().to_owned()),
                        terminal_id: id.as_str().to_owned(),
                    })
                {
                    tracing::debug!(%error, "no shell is subscribed to terminal bells");
                }
                if !replies.is_empty()
                    && let Err(error) = engine.lock().await.write_reply(&id, replies)
                {
                    tracing::debug!(%error, "could not send terminal replies");
                }
            }
        });
    }

    async fn stop_terminal_process(&self, terminal_id: TerminalId) -> Result<(), BridgeError> {
        let engine = Arc::clone(&self.engine);
        tokio::task::spawn_blocking(move || engine.blocking_lock().close(&terminal_id))
            .await
            .map_err(|error| BridgeError::new("terminal", error.to_string()))?
            .map_err(|error| BridgeError::new("terminal", error.to_string()))
    }
}

impl TerminalHost {
    pub(crate) fn for_task(&self, task_id: TaskId) -> Arc<dyn TerminalBackend> {
        Arc::new(TaskTerminalHost {
            host: self.clone(),
            task_id,
        })
    }

    async fn spawn_for_task(
        &self,
        launch: TerminalLaunch,
        events: mpsc::Sender<TerminalProcessEvent>,
        task_id: Option<TaskId>,
    ) -> Result<(), String> {
        let terminal = self
            .prepare(
                TerminalSpec {
                    id: TerminalId::new(launch.id),
                    columns: 80,
                    rows: 24,
                    cwd: launch.cwd,
                    command: launch.command,
                    args: launch.args,
                    environment: launch.environment,
                },
                DEFAULT_SCROLLBACK,
            )
            .await
            .map_err(|error| error.to_string())?;
        terminal.slot.lock().await.task_id = task_id;
        let store = self
            .process_store
            .get()
            .cloned()
            .ok_or_else(|| "terminal process storage is unavailable".to_string());
        let result = match store {
            Ok(store) => {
                if let Some(process) = &terminal.process {
                    let mut process = process.clone();
                    process.kind = "acp_terminal".into();
                    match store.save_process(&process).await {
                        Ok(()) => Ok(TrackedProcess {
                            id: Some(process.id),
                            store: store.clone(),
                        }),
                        Err(error) => Err(error.to_string()),
                    }
                } else {
                    tracing::warn!(
                        terminal_id = terminal.id.as_str(),
                        "could not capture ACP terminal process identity"
                    );
                    Ok(TrackedProcess { id: None, store })
                }
            }
            Err(error) => Err(error),
        };
        let tracked = match result {
            Ok(tracked) => tracked,
            Err(error) => {
                self.screens.lock().await.remove(terminal.id.as_str());
                if let Err(close_error) = self.stop_terminal_process(terminal.id.clone()).await {
                    return Err(format!(
                        "{error}; could not stop ACP terminal: {close_error}"
                    ));
                }
                return Err(error);
            }
        };
        self.observe(terminal, None, Some(events), Some(tracked));
        Ok(())
    }
}

struct TaskTerminalHost {
    host: TerminalHost,
    task_id: TaskId,
}

#[async_trait]
impl TerminalBackend for TaskTerminalHost {
    async fn spawn(
        &self,
        launch: TerminalLaunch,
        events: mpsc::Sender<TerminalProcessEvent>,
    ) -> Result<(), String> {
        self.host
            .spawn_for_task(launch, events, Some(self.task_id.clone()))
            .await
    }

    async fn stop(&self, id: &str) -> Result<(), String> {
        self.host.stop(id).await
    }
}

#[async_trait]
impl TerminalBackend for TerminalHost {
    async fn spawn(
        &self,
        launch: TerminalLaunch,
        events: mpsc::Sender<TerminalProcessEvent>,
    ) -> Result<(), String> {
        self.spawn_for_task(launch, events, None).await
    }

    async fn stop(&self, id: &str) -> Result<(), String> {
        self.stop_terminal_process(TerminalId::new(id))
            .await
            .map_err(|error| error.to_string())
    }
}

/// The state Tauri manages for the window: one core, one event broadcaster, the
/// PTY engine, one parsed screen per live terminal, and the cached project scan
/// shared with the startup scan task.
struct TerminalSubscription {
    _cancel: oneshot::Sender<()>,
    acknowledgements: watch::Sender<u32>,
}

pub struct ShellState {
    core: Arc<Mutex<Core>>,
    agent_events: Arc<tokio::sync::Notify>,
    pump_lifetime: watch::Sender<()>,
    attachment_root: Option<PathBuf>,
    pub(crate) preferences: Settings,
    pub(crate) codex_program: Option<PathBuf>,
    pub(crate) codex_detected: bool,
    pub(crate) opencode_launch: Option<SpawnSpec>,
    pub(crate) pi_configured: bool,
    pub(crate) claude_configured: bool,
    events: broadcast::Sender<Event>,
    terminal_subscriptions: Arc<Mutex<BTreeMap<String, TerminalSubscription>>>,
    terminal_lifecycle: Mutex<()>,
    pending_quit: Mutex<Option<QuitSummary>>,
    terminals: TerminalHost,
    preview_server: Arc<crate::preview_server::PreviewServer>,
    previous_processes: Mutex<Vec<ProcessRecord>>,
    scan: Arc<Mutex<ProjectScanCache>>,
    file_searches: Mutex<BTreeMap<String, FileSearch>>,
    task_searches: Mutex<BTreeMap<String, TaskSearchRequest>>,
    file_watches: Arc<Mutex<BTreeMap<String, oneshot::Sender<()>>>>,
    file_watch_starts: Arc<Semaphore>,
}

struct ProjectScanCache {
    value: Option<shepherd_core::project_scan::ProjectScan>,
    startup_valid: bool,
}

impl Default for ProjectScanCache {
    fn default() -> Self {
        Self {
            value: None,
            startup_valid: true,
        }
    }
}

#[derive(Default)]
struct FileSearch {
    cancelled: Arc<AtomicBool>,
    started: bool,
}

struct TaskSearchRequest {
    _cancel: oneshot::Sender<()>,
    cancellation: Option<oneshot::Receiver<()>>,
}

async fn pump_workspace_watch(
    watcher: &mut WorkspaceWatch,
    cancelled: &mut oneshot::Receiver<()>,
    mut send: impl FnMut(FileWatchUpdate) -> Result<(), String>,
) {
    loop {
        let update = tokio::select! {
            _ = &mut *cancelled => break,
            snapshot = watcher.snapshot() => match snapshot {
                Ok(update) => update,
                Err(error) => FileWatchUpdate::Error { message: format!("Could not refresh Git changes: {error}") },
            }
        };
        if let Err(error) = send(update) {
            tracing::debug!(%error, "the workspace file watch detached");
            break;
        }
        tokio::select! {
            _ = &mut *cancelled => break,
            refresh = watcher.next_refresh() => if let Err(error) = refresh {
                let recoverable = matches!(error, WatchError::Notification(_));
                let update = FileWatchUpdate::Error { message: format!("Could not watch workspace files: {error}") };
                if let Err(error) = send(update) {
                    tracing::debug!(%error, "the workspace file watch detached");
                    break;
                }
                if !recoverable {
                    break;
                }
            },
        }
    }
}

async fn start_file_watcher<T: Send + 'static>(
    capacity: Arc<Semaphore>,
    deadline: Duration,
    start: impl FnOnce() -> Result<T, WatchError> + Send + 'static,
) -> Result<T, BridgeError> {
    let permit = capacity
        .try_acquire_owned()
        .map_err(|_| BridgeError::new("files", "Too many file watchers are still starting."))?;
    let worker = tokio::task::spawn_blocking(move || (start(), permit));
    let (result, _permit) = tokio::time::timeout(deadline, worker)
        .await
        .map_err(|_| {
            BridgeError::new(
                "files",
                "The file watcher took too long to start. Please try again.",
            )
        })?
        .map_err(|error| {
            BridgeError::new(
                "files",
                format!("Could not start the file watcher: {error}"),
            )
        })?;
    result.map_err(|error| {
        BridgeError::new("files", format!("Could not watch workspace files: {error}"))
    })
}

async fn retry_file_watcher<T, F, Fut>(
    cancelled: &mut oneshot::Receiver<()>,
    retry_delay: Duration,
    mut start: F,
    mut report: impl FnMut(String) -> Result<(), String>,
) -> Option<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, BridgeError>>,
{
    let mut reported = false;
    loop {
        let result = tokio::select! {
            biased;
            _ = &mut *cancelled => return None,
            result = start() => result,
        };
        match result {
            Ok(watcher) => return Some(watcher),
            Err(error) => {
                let message = error.to_string();
                if !reported {
                    if let Err(error) = report(message.clone()) {
                        tracing::debug!(%error, "the workspace file watch detached");
                        return None;
                    }
                    reported = true;
                }
            }
        }
        tokio::select! {
            _ = &mut *cancelled => return None,
            _ = tokio::time::sleep(retry_delay) => {}
        }
    }
}

impl ShellState {
    #[cfg(test)]
    pub fn new(core: Core) -> Self {
        Self::with_terminals(core, TerminalHost::default())
    }

    fn with_terminals(core: Core, terminals: TerminalHost) -> Self {
        let (events, _) = broadcast::channel(512);
        if terminals.process_store.set(core.process_store()).is_err() {
            tracing::error!("terminal host is already bound to a core");
        }
        let core = Arc::new(Mutex::new(core));
        if terminals.core.set(Arc::downgrade(&core)).is_err() {
            tracing::error!("terminal host is already bound to a core");
        }
        let (pump_lifetime, _) = watch::channel(());
        Self {
            core,
            agent_events: Arc::new(tokio::sync::Notify::new()),
            pump_lifetime,
            attachment_root: None,
            preferences: Settings::default(),
            codex_program: None,
            codex_detected: false,
            opencode_launch: None,
            pi_configured: false,
            claude_configured: false,
            events,
            terminal_subscriptions: Arc::new(Mutex::new(BTreeMap::new())),
            terminal_lifecycle: Mutex::new(()),
            pending_quit: Mutex::new(None),
            terminals,
            preview_server: Arc::new(crate::preview_server::PreviewServer::default()),
            previous_processes: Mutex::new(Vec::new()),
            scan: Arc::new(Mutex::new(ProjectScanCache::default())),
            file_searches: Mutex::new(BTreeMap::new()),
            task_searches: Mutex::new(BTreeMap::new()),
            file_watches: Arc::new(Mutex::new(BTreeMap::new())),
            file_watch_starts: Arc::new(Semaphore::new(MAX_PENDING_FILE_WATCH_STARTS)),
        }
    }

    /// Open the durable core in the app data directory. A first run creates the
    /// database and schema; a later run marks every previously live task
    /// `interrupted` because its agent is no longer attached to this instance.
    pub async fn load(
        data_directory: &Path,
        harness_factory: Box<dyn HarnessFactory>,
        terminals: TerminalHost,
    ) -> Result<Self, BridgeError> {
        std::fs::create_dir_all(data_directory).map_err(|source| {
            BridgeError::new(
                "storage",
                format!(
                    "could not create the data directory {}: {source}",
                    data_directory.display()
                ),
            )
        })?;
        let leases =
            crate::orphan_process::ProcessLeases::new(data_directory.join(".agent-process-leases"))
                .map_err(|error| {
                    BridgeError::new(
                        "storage",
                        format!("could not initialize agent process leases: {error}"),
                    )
                })?;
        terminals
            .process_leases
            .set(Arc::new(leases))
            .map_err(|_| BridgeError::new("storage", "agent process leases already initialized"))?;
        let attachment_lock = attachments::lock_deletions(data_directory).map_err(|error| {
            BridgeError::new(
                "attachments",
                format!("Could not lock attachment recovery: {error}"),
            )
        })?;
        let database_path = data_directory.join("shepherd.db");
        let store = TursoStore::open_at(&database_path)
            .await
            .map_err(|error| BridgeError::new("storage", error.to_string()))?;
        let mut core = Core::load(Box::new(store), harness_factory)
            .await
            .map_err(BridgeError::from)?;
        let tasks = core
            .process_store()
            .load_tasks()
            .await
            .map_err(|error| BridgeError::new("storage", error.to_string()))?
            .into_iter()
            .map(|task| task.id.to_string())
            .collect();
        let attachment_root = data_directory.join("attachments");
        tokio::task::spawn_blocking(move || {
            let _lock = attachment_lock;
            attachments::recover_staged(&attachment_root, &tasks)
        })
        .await
        .map_err(|error| {
            BridgeError::new(
                "attachments",
                format!("Could not finish attachment recovery: {error}"),
            )
        })?
        .map_err(|error| {
            BridgeError::new(
                "attachments",
                format!("Could not recover staged attachments: {error}"),
            )
        })?;
        let previous_processes = core.recorded_processes().await.map_err(BridgeError::from)?;
        let mut state = Self::with_terminals(core, terminals);
        state.previous_processes = Mutex::new(previous_processes);
        state.attachment_root = Some(data_directory.join("attachments"));

        // Walking the home directory can take seconds, so the scan starts with
        // the app and the picker reads whatever has landed by the time it opens.
        if let Some(home) = project_scan_root() {
            let scan = state.scan.clone();
            tauri::async_runtime::spawn(async move {
                match scan_project_root(home).await {
                    Ok(found) => install_startup_scan(&scan, found).await,
                    Err(error) => tracing::warn!(%error, "the startup project scan failed"),
                }
            });
        }

        Ok(state)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    /// Fold agent activity between commands. A streaming turn must reach the
    /// window without waiting for the next invoke.
    pub fn start_event_pump(&self) {
        let core = Arc::downgrade(&self.core);
        let events = self.events.clone();
        let agent_events = self.agent_events.clone();
        let mut lifetime = self.pump_lifetime.subscribe();
        tauri::async_runtime::spawn(async move {
            agent_events.notify_one();
            loop {
                tokio::select! {
                    _ = agent_events.notified() => {}
                    _ = lifetime.changed() => break,
                }
                let Some(core) = core.upgrade() else {
                    break;
                };
                let mut core = core.lock().await;
                let retry = if let Err(error) = core.pump().await {
                    tracing::warn!(%error, "could not fold agent activity");
                    true
                } else {
                    false
                };
                Self::broadcast_core_events(&mut core, &events);
                if core.shutdown_state() == shepherd_core::ShutdownState::Closed {
                    break;
                }
                core.arm_agent_events(&agent_events);
                drop(core);
                if retry {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    agent_events.notify_one();
                }
            }
        });
    }

    async fn execute(&self, command: Command) -> Result<CommandResult, BridgeError> {
        let mut core = self.core.lock().await;
        let result = core.execute(command).await;
        Self::broadcast_core_events(&mut core, &self.events);
        core.arm_agent_events(&self.agent_events);
        if core.shutdown_state() == shepherd_core::ShutdownState::Closed {
            self.agent_events.notify_one();
        }
        result.map_err(BridgeError::from)
    }

    fn broadcast_core_events(core: &mut Core, events: &broadcast::Sender<Event>) {
        // Keep snapshots in core mutation order across the pump and commands.
        for event in core.drain_events() {
            if let Err(error) = events.send(event) {
                tracing::debug!(%error, "no shell is subscribed to core events");
            }
        }
    }

    async fn workspace(&self, command: Command) -> Result<TaskWorkspace, BridgeError> {
        match self.execute(command).await? {
            CommandResult::TaskWorkspace(workspace) => Ok(workspace),
            other => Err(unexpected(&other)),
        }
    }

    async fn git_read<T: Send + 'static>(
        &self,
        task_id: TaskId,
        read: impl FnOnce(shepherd_core::TaskGitRead) -> Result<T, CoreError> + Send + 'static,
    ) -> Result<T, BridgeError> {
        let context = self.core.lock().await.task_git_read(&task_id)?;
        let result = tokio::task::spawn_blocking(move || read(context))
            .await
            .map_err(|error| {
                BridgeError::new("git", format!("Could not finish the Git read: {error}"))
            })??;
        self.core
            .lock()
            .await
            .touch_archived_worktree_if_due(&task_id)
            .await?;
        Ok(result)
    }

    pub async fn add_project(&self, path: String) -> Result<ProjectList, BridgeError> {
        match self
            .execute(Command::AddProject { path: path.into() })
            .await?
        {
            CommandResult::ProjectList(projects) => Ok(projects),
            other => Err(unexpected(&other)),
        }
    }

    pub async fn set_project_preview_url(
        &self,
        project_id: String,
        url: String,
    ) -> Result<ProjectList, BridgeError> {
        let projects = match self
            .execute(Command::SetProjectPreviewUrl {
                project_id: ProjectId::new(project_id.clone()),
                url,
            })
            .await?
        {
            CommandResult::ProjectList(projects) => projects,
            other => return Err(unexpected(&other)),
        };
        self.preview_server.forget_project(&project_id).await;
        Ok(projects)
    }

    pub async fn open_project_preview(&self, project_id: String) -> Result<(), BridgeError> {
        let url = self
            .core
            .lock()
            .await
            .project_preview_url(&ProjectId::new(project_id))?
            .ok_or_else(|| BridgeError::new("preview", "Set the project preview URL first."))?;
        open_in_browser(url).await.map_err(|error| {
            BridgeError::new(
                "preview",
                format!("Could not open the preview in a browser. {error}"),
            )
        })
    }

    pub async fn annotation_wrapper_url(
        &self,
        project_id: String,
        task_id: Option<String>,
    ) -> Result<tauri::Url, BridgeError> {
        if let Some(task_id) = &task_id {
            let workspace = self
                .core
                .lock()
                .await
                .task_workspace(&TaskId::new(task_id.clone()))?;
            if workspace.task.project_id != project_id {
                return Err(BridgeError::new(
                    "preview",
                    "The selected task does not belong to this project.",
                ));
            }
        }
        let target = self
            .core
            .lock()
            .await
            .project_preview_url(&ProjectId::new(project_id.clone()))?
            .ok_or_else(|| BridgeError::new("preview", "Set the project preview URL first."))?;
        self.preview_server
            .open(&project_id, task_id.as_deref(), &target)
            .await
            .map_err(|error| BridgeError::new("preview", error.to_string()))
    }

    pub async fn open_project_annotation_wrapper(
        &self,
        project_id: String,
        task_id: Option<String>,
    ) -> Result<String, BridgeError> {
        let wrapper = self.annotation_wrapper_url(project_id, task_id).await?;
        let address = wrapper.clone();
        open_in_browser(wrapper).await.map_err(|error| {
            BridgeError::new(
                "preview",
                format!("Could not open the annotation wrapper in a browser. {error}"),
            )
        })?;
        Ok(address.to_string())
    }

    pub async fn validate_preview_annotations(
        &self,
        batch: AnnotationBatch,
    ) -> Result<AnnotationBatch, BridgeError> {
        let project_id = batch.project_id.clone();
        let task_id = batch.task_id.clone();
        let target = {
            let core = self.core.lock().await;
            let workspace = core.task_workspace(&TaskId::new(task_id.clone()))?;
            if workspace.task.project_id != project_id {
                return Err(BridgeError::new(
                    "preview",
                    "The selected task does not belong to this project.",
                ));
            }
            core.project_preview_url(&ProjectId::new(project_id.clone()))?
                .ok_or_else(|| BridgeError::new("preview", "Set the project preview URL first."))?
        };
        let attachment_root = self.task_attachment_directory(&task_id);
        let origin = target.origin().ascii_serialization();
        validate_batch(
            batch,
            &project_id,
            &task_id,
            &origin,
            attachment_root.as_deref(),
        )
        .map_err(|error| BridgeError::new("preview", error.to_string()))
    }

    pub async fn save_preview_screenshot(
        &self,
        task_id: String,
        bytes: &[u8],
    ) -> Result<PathBuf, BridgeError> {
        let directory = self.task_attachment_directory(&task_id).ok_or_else(|| {
            BridgeError::new(
                "preview",
                "Could not locate this task's attachment directory.",
            )
        })?;
        let attachment_lock = self.lock_attachment_deletions()?;
        // A draft cannot be discarded until its screenshot write finishes.
        let core = self.core.lock().await;
        let task_id = TaskId::new(task_id);
        Self::ensure_durable_attachment_task(core.process_store(), &task_id).await?;
        let limit = self
            .attachment_limit_for(&core.task_project_path(&task_id)?)
            .await;
        limit
            .check(bytes.len() as u64)
            .map_err(|error| BridgeError::new("preview", error.to_string()))?;
        let bytes = bytes.to_vec();
        let result = tokio::task::spawn_blocking(move || {
            let _lock = attachment_lock;
            attachments::persist_preview_screenshot(&directory, &bytes)
        })
        .await
        .map_err(|error| BridgeError::new("preview", error.to_string()))?;
        drop(core);
        result.map_err(|error| {
            BridgeError::new("preview", format!("Could not save the screenshot. {error}"))
        })
    }

    fn task_attachment_directory(&self, task_id: &str) -> Option<PathBuf> {
        let task_id = uuid::Uuid::parse_str(task_id).ok()?;
        Some(self.attachment_root.as_ref()?.join(task_id.to_string()))
    }

    fn lock_attachment_deletions(&self) -> Result<Option<attachments::DeletionLock>, BridgeError> {
        match &self.attachment_root {
            Some(root) => {
                let data_directory = root.parent().ok_or_else(|| {
                    BridgeError::new(
                        "attachments",
                        "Could not locate the attachment data directory.",
                    )
                })?;
                attachments::lock_deletions(data_directory)
                    .map(Some)
                    .map_err(|error| {
                        BridgeError::new(
                            "attachments",
                            format!("Could not lock attachment access: {error}"),
                        )
                    })
            }
            None => Ok(None),
        }
    }

    async fn ensure_durable_attachment_task(
        store: Arc<dyn Store>,
        task_id: &TaskId,
    ) -> Result<(), BridgeError> {
        if store
            .load_tasks()
            .await
            .map_err(|error| BridgeError::new("storage", error.to_string()))?
            .iter()
            .any(|task| &task.id == task_id)
        {
            Ok(())
        } else {
            Err(BridgeError::new(
                "attachments",
                "The task no longer exists.",
            ))
        }
    }

    async fn attachment_limit_for(&self, path: &Path) -> AttachmentLimit {
        let value = self
            .preferences
            .value(Some(path), SettingKey::AttachmentBytes)
            .await;
        let bytes = value
            .as_u64()
            .and_then(|bytes| usize::try_from(bytes).ok())
            .unwrap_or(DEFAULT_ATTACHMENT_BYTES);
        AttachmentLimit::from_bytes(bytes)
    }

    pub(crate) async fn settings_project_path(&self, id: &str) -> Result<PathBuf, BridgeError> {
        Ok(self.core.lock().await.project_path(&ProjectId::new(id))?)
    }

    async fn attachment_limit(&self, project_id: String) -> Result<AttachmentLimit, BridgeError> {
        let path = self
            .core
            .lock()
            .await
            .project_path(&ProjectId::new(project_id))?;
        Ok(self.attachment_limit_for(&path).await)
    }

    pub async fn save_prompt_file(
        &self,
        task_id: String,
        name: String,
        bytes: &[u8],
    ) -> Result<String, BridgeError> {
        let directory = self.task_attachment_directory(&task_id).ok_or_else(|| {
            BridgeError::new(
                "attachments",
                "Could not locate this task's attachment directory.",
            )
        })?;
        let attachment_lock = self.lock_attachment_deletions()?;
        let core = self.core.lock().await;
        let task_id = TaskId::new(task_id);
        Self::ensure_durable_attachment_task(core.process_store(), &task_id).await?;
        let limit = self
            .attachment_limit_for(&core.task_project_path(&task_id)?)
            .await;
        limit
            .check(bytes.len() as u64)
            .map_err(|error| BridgeError::new("attachments", error.to_string()))?;
        let bytes = bytes.to_vec();
        let result = tokio::task::spawn_blocking(move || {
            let _lock = attachment_lock;
            attachments::persist_prompt_file(&directory, &name, &bytes)
        })
        .await
        .map_err(|error| BridgeError::new("attachments", error.to_string()))?;
        drop(core);
        result.map_err(|error| {
            BridgeError::new(
                "attachments",
                format!("Could not save the attachment. {error}"),
            )
        })
    }

    pub async fn create_draft(
        &self,
        project_id: String,
        title: String,
        agent: Agent,
    ) -> Result<TaskWorkspace, BridgeError> {
        self.workspace(Command::CreateDraft {
            project_id: ProjectId::new(project_id),
            title,
            agent,
        })
        .await
    }

    pub async fn set_draft_agent(
        &self,
        task_id: String,
        agent: Agent,
    ) -> Result<TaskWorkspace, BridgeError> {
        self.workspace(Command::SetDraftAgent {
            task_id: TaskId::new(task_id),
            agent,
        })
        .await
    }

    pub async fn discard_draft(&self, task_id: String) -> Result<(), BridgeError> {
        self.remove_task(task_id, true, false).await
    }

    pub async fn delete_task(
        &self,
        task_id: String,
        remove_worktree: bool,
    ) -> Result<(), BridgeError> {
        self.remove_task(task_id, false, remove_worktree).await
    }

    async fn remove_task(
        &self,
        task_id: String,
        draft_only: bool,
        remove_worktree: bool,
    ) -> Result<(), BridgeError> {
        let attachments = self.task_attachment_directory(&task_id);
        let _guard = self.terminal_lifecycle.lock().await;
        let mut attachment_lock = self.lock_attachment_deletions()?;
        let task_id = TaskId::new(task_id);
        let mut core = self.core.lock().await;
        let workspace = core.task_workspace(&task_id)?;
        if draft_only && workspace.task.state != TaskState::Draft {
            return Err(CoreError::NotADraft {
                id: task_id.to_string(),
            }
            .into());
        }
        if !draft_only
            && matches!(
                workspace.task.state,
                TaskState::Starting | TaskState::Working | TaskState::Waiting | TaskState::Queued
            )
        {
            return Err(CoreError::TaskDeletionBusy {
                id: task_id.to_string(),
                state: workspace.task.state.as_str(),
            }
            .into());
        }
        if remove_worktree {
            core.check_task_worktree_removal(&task_id)?;
        }
        let project_id = ProjectId::new(workspace.task.project_id);
        let running = core
            .terminals_for(&project_id)
            .into_iter()
            .filter(|terminal| {
                terminal.task_id.as_deref() == Some(task_id.as_str()) && terminal.ended_at.is_none()
            })
            .map(|terminal| TerminalId::new(terminal.id))
            .collect::<Vec<_>>();
        for terminal in running {
            self.terminals
                .stop_terminal_process(terminal.clone())
                .await?;
            core.end_terminal(&terminal, None).await?;
        }
        let staged = if let Some(path) = attachments.as_ref() {
            let path = path.clone();
            let lock = attachment_lock.take();
            let (result, lock) =
                tokio::task::spawn_blocking(move || (attachments::stage_task(&path), lock))
                    .await
                    .map_err(|error| {
                        BridgeError::new(
                            "attachments",
                            format!("Could not prepare attachment removal: {error}"),
                        )
                    })?;
            attachment_lock = lock;
            result.map_err(|error| {
                BridgeError::new(
                    "attachments",
                    format!("Could not prepare attachment removal: {error}"),
                )
            })?
        } else {
            None
        };
        #[cfg(test)]
        let fixture_task_id = task_id.clone();
        #[cfg(test)]
        pause_attachment_deletion_fixture("staged", &fixture_task_id).await?;
        let command = if draft_only {
            Command::DiscardDraft { task_id }
        } else {
            Command::DeleteTask {
                task_id,
                remove_worktree,
            }
        };
        let result = core.execute(command).await;
        #[cfg(test)]
        if result.is_ok() {
            pause_attachment_deletion_fixture("committed", &fixture_task_id).await?;
        }
        let committed = matches!(&result, Err(CoreError::TaskDeletedCleanupPending { .. }));
        let result = result
            .map_err(BridgeError::from)
            .and_then(|result| match result {
                CommandResult::Ack => Ok(()),
                other => Err(unexpected(&other)),
            });
        Self::broadcast_core_events(&mut core, &self.events);
        drop(core);
        match result {
            Err(error) if !committed => {
                if let (Some(staged), Some(original)) = (staged, attachments) {
                    let display = staged.display().to_string();
                    let lock = attachment_lock.take();
                    tokio::task::spawn_blocking(move || {
                        let _lock = lock;
                        attachments::restore_task(&staged, &original)
                    })
                    .await
                    .map_err(|restore| {
                        BridgeError::new(
                            "attachments",
                            format!(
                                "{error}; could not restore attachments from {display}: {restore}"
                            ),
                        )
                    })?
                    .map_err(|restore| {
                        BridgeError::new(
                            "attachments",
                            format!(
                                "{error}; could not restore attachments from {display}: {restore}"
                            ),
                        )
                    })?;
                }
                Err(error)
            }
            outcome => {
                if let Some(path) = staged {
                    let display = path.display().to_string();
                    let lock = attachment_lock.take();
                    tokio::task::spawn_blocking(move || {
                        let _lock = lock;
                        attachments::remove_staged_task(&path)
                    })
                    .await
                    .map_err(|error| {
                        BridgeError::new(
                            "attachments",
                            format!(
                                "Task deleted, but attachment cleanup failed at {display}: {error}"
                            ),
                        )
                    })?
                    .map_err(|error| {
                        BridgeError::new(
                            "attachments",
                            format!(
                                "Task deleted, but attachment cleanup failed at {display}: {error}"
                            ),
                        )
                    })?;
                }
                outcome
            }
        }
    }

    #[cfg(test)]
    async fn send_prompt(
        &self,
        task_id: String,
        text: String,
    ) -> Result<TaskWorkspace, BridgeError> {
        self.send_prompt_with_files(task_id, text, Vec::new(), CodexTurnOptions::default(), None)
            .await
    }

    pub async fn send_prompt_with_files(
        &self,
        task_id: String,
        text: String,
        ids: Vec<String>,
        codex: CodexTurnOptions,
        mode: Option<String>,
    ) -> Result<TaskWorkspace, BridgeError> {
        let mut input = PromptInput {
            text,
            codex,
            mode,
            ..PromptInput::default()
        };
        if !ids.is_empty() {
            let attachment_lock = self.lock_attachment_deletions()?;
            let directory = self.task_attachment_directory(&task_id).ok_or_else(|| {
                BridgeError::new(
                    "attachments",
                    "Could not locate this task's attachment directory.",
                )
            })?;
            let core = self.core.lock().await;
            let id = TaskId::new(&task_id);
            Self::ensure_durable_attachment_task(core.process_store(), &id).await?;
            let path = core.task_project_path(&id)?;
            drop(core);
            let limit = self.attachment_limit_for(&path).await;
            let attachments = tokio::task::spawn_blocking(move || {
                let _lock = attachment_lock;
                ids.iter()
                    .map(|id| attachments::resolve_prompt_file(&directory, id, &limit))
                    .collect::<std::io::Result<Vec<_>>>()
            })
            .await
            .map_err(|error| BridgeError::new("attachments", error.to_string()))?
            .map_err(|error| {
                BridgeError::new(
                    "attachments",
                    format!("Could not read an attached file. {error}"),
                )
            })?;
            let paths = attachments
                .iter()
                .map(|file| &file.path)
                .collect::<Vec<_>>();
            let references = serde_json::to_string_pretty(&paths).map_err(|error| {
                BridgeError::new(
                    "attachments",
                    format!("Could not encode the attached paths. {error}"),
                )
            })?;
            let text = if input.text.trim().is_empty() {
                "Use the attached files."
            } else {
                input.text.trim_end()
            };
            input.text = format!("{text}\n\nAttached files\n{references}");
            input.attachments = attachments;
        }
        self.workspace(Command::SendPrompt {
            task_id: TaskId::new(task_id),
            input,
        })
        .await
    }

    pub async fn resolve_approval(
        &self,
        task_id: String,
        approval_id: String,
        decision: Decision,
    ) -> Result<TaskWorkspace, BridgeError> {
        self.workspace(Command::ResolveApproval {
            task_id: TaskId::new(task_id),
            approval_id: agent_harness::ApprovalId::new(approval_id),
            decision,
        })
        .await
    }

    pub async fn project_approvals(
        &self,
        project_id: String,
    ) -> Result<ProjectApprovalsSnapshot, BridgeError> {
        Ok(self
            .core
            .lock()
            .await
            .project_approvals(&ProjectId::new(project_id))
            .await?)
    }

    pub async fn reset_approval_grant(
        &self,
        project_id: String,
        approval_id: String,
        task_id: Option<String>,
    ) -> Result<(), BridgeError> {
        Ok(self
            .core
            .lock()
            .await
            .reset_approval_grant(
                &ProjectId::new(project_id),
                &approval_id,
                task_id.map(TaskId::new).as_ref(),
            )
            .await?)
    }

    pub async fn cancel_task(&self, task_id: String) -> Result<TaskWorkspace, BridgeError> {
        self.workspace(Command::CancelTask {
            task_id: TaskId::new(task_id),
        })
        .await
    }

    pub async fn resume_task(&self, task_id: String) -> Result<TaskWorkspace, BridgeError> {
        self.workspace(Command::ResumeTask {
            task_id: TaskId::new(task_id),
        })
        .await
    }

    pub async fn quit(&self, force: bool) -> Result<QuitSummary, BridgeError> {
        let engine = self.terminals.engine.clone();
        let preview_server = self.preview_server.clone();
        self.quit_with_cleanup(force, || async move {
            preview_server
                .stop()
                .await
                .map_err(|error| BridgeError::new("preview", error.to_string()))?;
            tokio::task::spawn_blocking(move || engine.blocking_lock().terminate_all())
                .await
                .map_err(|error| {
                    BridgeError::new(
                        "terminal",
                        format!("Terminal shutdown worker failed: {error}"),
                    )
                })?
                .map_err(|error| {
                    BridgeError::new("terminal", format!("Could not stop terminals: {error}"))
                })
        })
        .await
    }

    async fn quit_with_cleanup<F, Fut>(
        &self,
        force: bool,
        cleanup: F,
    ) -> Result<QuitSummary, BridgeError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<(), BridgeError>>,
    {
        let _guard = self.terminal_lifecycle.lock().await;
        let mut pending = self.pending_quit.lock().await;
        let summary = match pending.as_ref() {
            Some(summary) => summary.clone(),
            None => match self.execute(Command::Quit { force }).await? {
                CommandResult::Quit(summary) => {
                    *pending = Some(summary.clone());
                    summary
                }
                other => return Err(unexpected(&other)),
            },
        };
        cleanup().await?;
        *pending = None;
        Ok(summary)
    }

    pub async fn project_list(&self) -> ProjectList {
        self.core.lock().await.project_list()
    }

    pub async fn waiting_task_title(&self, task_id: TaskId) -> Result<Option<String>, BridgeError> {
        Ok(self
            .core
            .lock()
            .await
            .waiting_task_title(&task_id)?
            .map(str::to_owned))
    }

    pub async fn codex_project_models(
        &self,
        project_id: String,
    ) -> Result<CodexProjectModels, BridgeError> {
        let Some(program) = &self.codex_program else {
            return Ok(CodexProjectModels::default());
        };
        let spec = self
            .discovery_spec(
                project_id,
                Agent::Codex,
                codex_spawn_spec(program, PathBuf::new()),
            )
            .await?;
        agent_harness::codex::project_models(spec)
            .await
            .map_err(|error| BridgeError::new("codex_model", error.to_string()))
    }

    async fn opencode_project_modes(
        &self,
        project_id: String,
    ) -> Result<Vec<AgentMode>, BridgeError> {
        let Some(launch) = &self.opencode_launch else {
            return Ok(Vec::new());
        };
        let spec = self
            .discovery_spec(project_id, Agent::OpenCode, launch.clone())
            .await?;
        agent_harness::opencode::project_modes(&spec)
            .await
            .map_err(|error| BridgeError::new("opencode_modes", error.to_string()))
    }

    async fn discovery_spec(
        &self,
        project_id: String,
        agent: Agent,
        mut spec: SpawnSpec,
    ) -> Result<SpawnSpec, BridgeError> {
        let (cwd, environment) = self
            .core
            .lock()
            .await
            .project_launch_environment(&ProjectId::new(project_id), agent)?;
        spec.environment.extend(environment);
        agent_harness::settings::resolve_environment(&mut spec.environment, &cwd)
            .await
            .map_err(|error| BridgeError::new("agent_environment", error.to_string()))?;
        spec.cwd = cwd;
        Ok(spec)
    }

    pub async fn project_remote(
        &self,
        project_id: String,
    ) -> Result<Option<ProjectRemote>, BridgeError> {
        let path = self
            .core
            .lock()
            .await
            .project_path(&ProjectId::new(project_id))?;
        tokio::task::spawn_blocking(move || {
            GitService::primary_remote(&path)
                .map(|remote| remote.as_deref().map(ProjectRemote::from_url))
        })
        .await
        .map_err(|error| BridgeError::new("git", error.to_string()))?
        .map_err(|error| BridgeError::new("git", error.to_string()))
    }

    /// The cached home-directory scan. `rescan` walks the tree again; otherwise
    /// the first call after startup does the work and later calls reuse it.
    pub async fn project_scan(
        &self,
        rescan: bool,
    ) -> Result<shepherd_core::project_scan::ProjectScan, BridgeError> {
        request_project_scan(&self.scan, rescan, async {
            let home = project_scan_root()
                .ok_or_else(|| BridgeError::new("scan", "no home directory to scan"))?;
            scan_project_root(home).await
        })
        .await
    }

    pub async fn task_list(&self, project_id: String) -> Result<TaskList, BridgeError> {
        self.core
            .lock()
            .await
            .task_list(&ProjectId::new(project_id))
            .map_err(BridgeError::from)
    }

    pub async fn pool_status(&self) -> Result<PoolStatus, BridgeError> {
        let mut core = self.core.lock().await;
        core.pool_status(now_millis())
            .await
            .map_err(BridgeError::from)
    }

    pub async fn force_pool_slot(&self, slot_id: String) -> Result<PoolForceOutcome, BridgeError> {
        let request = PoolForceRequest {
            slot_id: PoolSlotId::new(slot_id),
            now: now_millis(),
        };
        let mut core = self.core.lock().await;
        core.force_pool_slot(&request)
            .await
            .map_err(BridgeError::from)
    }

    pub async fn clear_pool_force(&self) -> Result<PoolForceOutcome, BridgeError> {
        let mut core = self.core.lock().await;
        Ok(core.clear_pool_force())
    }

    pub async fn apply_pool_catalog(
        &self,
        snapshot: String,
    ) -> Result<PoolCatalogOutcome, BridgeError> {
        let advisory: PoolCatalogAdvisory = serde_json::from_str(&snapshot).map_err(|error| {
            BridgeError::new(
                "pool_catalog",
                format!("Could not read the catalog snapshot. {error}"),
            )
        })?;
        let mut core = self.core.lock().await;
        core.apply_pool_catalog(&advisory, now_millis())
            .await
            .map_err(BridgeError::from)
    }

    pub async fn add_pool_slot(
        &self,
        agent: String,
        provider_id: String,
        provider_name: String,
        model_id: String,
        model_name: String,
    ) -> Result<PoolSlot, BridgeError> {
        let agent = Agent::parse(&agent)
            .ok_or_else(|| BridgeError::new("pool_slot", format!("Unknown agent: {agent}")))?;
        let mut core = self.core.lock().await;
        core.add_pool_slot(agent, &provider_id, &provider_name, &model_id, &model_name)
            .await
            .map_err(BridgeError::from)
    }

    pub async fn generated_pool_catalog(&self) -> Result<Option<String>, BridgeError> {
        let path = std::env::var_os("SHEPHERD_POOL_CATALOG")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../../.cache/shepherd/pool-catalog-advisory.v2.json")
            });
        match std::fs::read_to_string(&path) {
            Ok(text) => Ok(Some(text)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(BridgeError::new(
                "pool_catalog",
                format!("Could not read {}: {error}", path.display()),
            )),
        }
    }

    pub async fn reorder_pool_slots(
        &self,
        slot_ids: Vec<String>,
    ) -> Result<PoolReorderOutcome, BridgeError> {
        let request = PoolReorderRequest {
            slot_ids: slot_ids.into_iter().map(PoolSlotId::new).collect(),
            now: now_millis(),
        };
        let mut core = self.core.lock().await;
        core.reorder_pool_slots(&request)
            .await
            .map_err(BridgeError::from)
    }

    pub async fn set_pool_slot_enabled(
        &self,
        slot_id: String,
        enabled: bool,
    ) -> Result<PoolSlotEnabledOutcome, BridgeError> {
        let request = PoolSlotEnabledRequest {
            slot_id: PoolSlotId::new(slot_id),
            enabled,
            now: now_millis(),
        };
        let mut core = self.core.lock().await;
        core.set_pool_slot_enabled(&request)
            .await
            .map_err(BridgeError::from)
    }

    pub async fn correct_pool_quota(
        &self,
        slot_id: String,
        limit: Option<u64>,
        limit_unit: Option<String>,
        reset_at: Option<u64>,
        window_key: Option<String>,
    ) -> Result<PoolQuotaCorrectionOutcome, BridgeError> {
        let request = PoolQuotaCorrectionRequest {
            slot_id: PoolSlotId::new(slot_id),
            limit,
            limit_unit,
            reset_at,
            now: now_millis(),
            window_key,
        };
        let mut core = self.core.lock().await;
        core.correct_pool_quota(&request)
            .await
            .map_err(BridgeError::from)
    }

    pub async fn clear_pool_slot_cooldown(
        &self,
        slot_id: String,
    ) -> Result<PoolQuotaCorrectionOutcome, BridgeError> {
        let request = PoolCooldownClearRequest {
            slot_id: PoolSlotId::new(slot_id),
            now: now_millis(),
        };
        let mut core = self.core.lock().await;
        core.clear_pool_slot_cooldown(&request)
            .await
            .map_err(BridgeError::from)
    }

    pub async fn task_workspace(&self, task_id: String) -> Result<TaskWorkspace, BridgeError> {
        let task_id = TaskId::new(task_id);
        Ok(self.core.lock().await.open_task_workspace(&task_id).await?)
    }

    async fn reserve_task_search(&self) -> Result<String, BridgeError> {
        let mut searches = self.task_searches.lock().await;
        if searches.len() >= 8 {
            return Err(BridgeError::new(
                "search",
                "Too many task searches are pending.",
            ));
        }
        let id = uuid::Uuid::new_v4().to_string();
        let (cancel, cancellation) = oneshot::channel();
        searches.insert(
            id.clone(),
            TaskSearchRequest {
                _cancel: cancel,
                cancellation: Some(cancellation),
            },
        );
        Ok(id)
    }

    async fn cancel_task_search(&self, id: &str) {
        // Dropping the sender also wakes a search waiting for the core lock.
        self.task_searches.lock().await.remove(id);
    }

    async fn search_tasks(
        &self,
        id: &str,
        query: &str,
    ) -> Result<Vec<shepherd_core::search::SearchHit>, BridgeError> {
        let cancelled = || BridgeError::new("search_cancelled", "Task search was cancelled.");
        let cancellation = {
            let mut searches = self.task_searches.lock().await;
            searches
                .get_mut(id)
                .ok_or_else(cancelled)?
                .cancellation
                .take()
                .ok_or_else(|| BridgeError::new("search", "This task search is already running."))?
        };
        let result = tokio::select! {
            biased;
            _ = cancellation => Err(cancelled()),
            result = async {
                let search = self.core.lock().await.task_search();
                search.search(query).await
            } => result.map_err(|error| BridgeError::new("search", format!("Could not search tasks. {error}"))),
        };
        self.task_searches.lock().await.remove(id);
        result
    }

    async fn files_directory(
        &self,
        project_id: String,
        task_id: Option<String>,
    ) -> Result<PathBuf, BridgeError> {
        let task_id = task_id.map(TaskId::new);
        let mut core = self.core.lock().await;
        let directory = core.workspace_directory(&ProjectId::new(project_id), task_id.as_ref())?;
        if let Some(task_id) = &task_id {
            core.touch_archived_worktree_if_due(task_id).await?;
        }
        Ok(directory)
    }

    async fn watch_workspace_files(
        &self,
        project_id: String,
        task_id: Option<String>,
        channel: tauri::ipc::Channel<FileWatchUpdate>,
    ) -> Result<String, BridgeError> {
        let root = self.files_directory(project_id, task_id).await?;
        let (cancel, mut cancelled) = oneshot::channel();
        let watch_id = uuid::Uuid::new_v4().to_string();
        self.file_watches
            .lock()
            .await
            .insert(watch_id.clone(), cancel);
        let watches = self.file_watches.clone();
        let capacity = self.file_watch_starts.clone();
        let id = watch_id.clone();
        tauri::async_runtime::spawn(async move {
            let watcher = retry_file_watcher(
                &mut cancelled,
                FILE_WATCH_RETRY_DELAY,
                || {
                    let capacity = capacity.clone();
                    let root = root.clone();
                    start_file_watcher(capacity, FILE_WATCH_START_TIMEOUT, move || {
                        WorkspaceWatch::start(root)
                    })
                },
                |message| {
                    channel
                        .send(FileWatchUpdate::Error { message })
                        .map_err(|error| error.to_string())
                },
            )
            .await;
            if let Some(mut watcher) = watcher {
                pump_workspace_watch(&mut watcher, &mut cancelled, |update| {
                    channel.send(update).map_err(|error| error.to_string())
                })
                .await;
            }
            watches.lock().await.remove(&id);
        });
        Ok(watch_id)
    }

    async fn reserve_file_search(&self) -> Result<String, BridgeError> {
        let mut searches = self.file_searches.lock().await;
        if searches.len() >= 8 {
            return Err(BridgeError::new(
                "files",
                "Too many file searches are pending.",
            ));
        }
        let search_id = uuid::Uuid::new_v4().to_string();
        searches.insert(search_id.clone(), FileSearch::default());
        Ok(search_id)
    }

    async fn cancel_file_search(&self, search_id: &str) {
        let mut searches = self.file_searches.lock().await;
        if let Some(search) = searches.get(search_id).filter(|search| search.started) {
            search.cancelled.store(true, Ordering::Relaxed);
        } else {
            searches.remove(search_id);
        }
    }

    async fn search_workspace_files(
        &self,
        search_id: &str,
        project_id: String,
        task_id: Option<String>,
        query: FileSearchQuery,
    ) -> Result<FileSearchResult, BridgeError> {
        let cancelled = {
            let mut searches = self.file_searches.lock().await;
            let search = searches
                .get_mut(search_id)
                .ok_or(files::FileError::Cancelled)?;
            if search.started {
                return Err(BridgeError::new(
                    "files",
                    "This file search is already running.",
                ));
            }
            search.started = true;
            search.cancelled.clone()
        };
        let result = async {
            let root = self.files_directory(project_id, task_id).await?;
            Ok(
                tokio::task::spawn_blocking(move || files::search_files(&root, query, &cancelled))
                    .await
                    .map_err(|error| {
                        BridgeError::new(
                            "files",
                            format!("Could not finish the file search: {error}"),
                        )
                    })??,
            )
        }
        .await;
        self.file_searches.lock().await.remove(search_id);
        result
    }

    pub async fn terminal_list(&self, project_id: String) -> TerminalList {
        self.core
            .lock()
            .await
            .terminal_list(&ProjectId::new(project_id))
    }

    pub async fn start_terminal(
        &self,
        project_id: String,
        task_id: Option<String>,
        columns: u16,
        rows: u16,
    ) -> Result<TerminalList, BridgeError> {
        let _guard = self.terminal_lifecycle.lock().await;
        let project_id = ProjectId::new(project_id);
        let task_id = task_id.map(TaskId::new);
        let (cwd, title) = {
            let core = self.core.lock().await;
            let cwd = core.workspace_directory(&project_id, task_id.as_ref())?;
            let title = format!("Terminal {}", core.terminals_for(&project_id).len() + 1);
            (cwd, title)
        };
        let record = TerminalRecord::project_shared(
            TerminalId::new(uuid::Uuid::new_v4().to_string()),
            project_id,
            title,
            cwd,
            std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string()),
            now_millis(),
        );
        let record = match task_id {
            Some(task_id) => record.for_task(task_id),
            None => record,
        };
        self.launch_terminal(record, columns, rows).await
    }

    pub async fn restart_terminal(
        &self,
        terminal_id: String,
        columns: u16,
        rows: u16,
    ) -> Result<TerminalList, BridgeError> {
        let _guard = self.terminal_lifecycle.lock().await;
        let mut record = self
            .core
            .lock()
            .await
            .terminal_record(&TerminalId::new(terminal_id))?;
        if record.ended_at.is_none() {
            return Err(BridgeError::new("terminal", "terminal is already running"));
        }
        if !record.cwd.is_dir() {
            return Err(BridgeError::new(
                "terminal",
                format!("terminal directory is missing: {}", record.cwd.display()),
            ));
        }
        record.ended_at = None;
        record.exit_code = None;
        self.launch_terminal(record, columns, rows).await
    }

    pub async fn start_freebuff_terminal(&self, project_id: String) -> Result<String, BridgeError> {
        let _guard = self.terminal_lifecycle.lock().await;
        let cwd = self
            .core
            .lock()
            .await
            .project_path(&ProjectId::new(project_id))?;
        let id = self
            .spawn_terminal(
                TerminalSpec {
                    id: TerminalId::new(uuid::Uuid::new_v4().to_string()),
                    columns: 80,
                    rows: 24,
                    cwd,
                    command: "freebuff".into(),
                    args: Vec::new(),
                    environment: BTreeMap::new(),
                },
                None,
            )
            .await?;
        Ok(id.as_str().to_owned())
    }

    async fn launch_terminal(
        &self,
        record: TerminalRecord,
        columns: u16,
        rows: u16,
    ) -> Result<TerminalList, BridgeError> {
        let project_id = record.project_id.clone();
        if let Some(task_id) = &record.task_id {
            self.core
                .lock()
                .await
                .touch_archived_worktree(task_id)
                .await?;
        }
        self.spawn_terminal(
            TerminalSpec {
                id: record.id.clone(),
                columns,
                rows,
                cwd: record.cwd.clone(),
                command: record.command.clone(),
                args: Vec::new(),
                environment: BTreeMap::new(),
            },
            Some(record),
        )
        .await?;
        Ok(self.core.lock().await.terminal_list(&project_id))
    }

    async fn spawn_terminal(
        &self,
        spec: TerminalSpec,
        record: Option<TerminalRecord>,
    ) -> Result<TerminalId, BridgeError> {
        let (state, project, process_store) = {
            let core = self.core.lock().await;
            let project = record
                .as_ref()
                .map(|record| core.project_path(&record.project_id))
                .transpose()?;
            (core.shutdown_state(), project, core.process_store())
        };
        if state != shepherd_core::ShutdownState::Running {
            return Err(CoreError::NotRunning { state }.into());
        }
        let scrollback = if let Some(project) = project {
            self.preferences
                .value(Some(&project), SettingKey::Scrollback)
                .await
                .as_u64()
                .and_then(|lines| usize::try_from(lines).ok())
                .unwrap_or(DEFAULT_SCROLLBACK)
        } else {
            DEFAULT_SCROLLBACK
        };
        let terminal = self.terminals.prepare(spec, scrollback).await?;
        let terminal_id = terminal.id.clone();
        terminal.slot.lock().await.task_id =
            record.as_ref().and_then(|record| record.task_id.clone());
        if record.is_some() {
            if let Some(process) = &terminal.process {
                let result = { self.core.lock().await.record_process(process).await };
                if let Err(error) = result {
                    self.terminals
                        .screens
                        .lock()
                        .await
                        .remove(terminal_id.as_str());
                    if let Err(close_error) = self
                        .terminals
                        .stop_terminal_process(terminal_id.clone())
                        .await
                    {
                        return Err(BridgeError::new(
                            "terminal",
                            format!(
                                "{error}; could not stop terminal after recording failed: {close_error}"
                            ),
                        ));
                    }
                    return Err(error.into());
                }
            } else {
                tracing::warn!(
                    terminal_id = terminal_id.as_str(),
                    "could not capture terminal process identity"
                );
            }
        }
        if let Some(record) = &record {
            let result = { self.core.lock().await.record_terminal(record).await };
            if let Err(error) = result {
                self.terminals
                    .screens
                    .lock()
                    .await
                    .remove(terminal_id.as_str());
                let close = self.terminals.stop_terminal_process(terminal_id).await;
                let recorded = RecordedTerminal {
                    record: record.clone(),
                    process_id: terminal.process.as_ref().map(|process| process.id.clone()),
                    core: Arc::clone(&self.core),
                    events: self.events.clone(),
                };
                let tracked = TrackedProcess {
                    id: recorded.process_id.clone(),
                    store: process_store.clone(),
                };
                self.terminals
                    .observe(terminal, Some(recorded), None, Some(tracked));
                if let Err(close_error) = close {
                    return Err(BridgeError::new(
                        "terminal",
                        format!(
                            "{error}; could not stop terminal after recording failed: {close_error}"
                        ),
                    ));
                }
                return Err(error.into());
            }
        }
        let recorded = record.map(|record| RecordedTerminal {
            record,
            process_id: terminal.process.as_ref().map(|process| process.id.clone()),
            core: Arc::clone(&self.core),
            events: self.events.clone(),
        });
        let tracked = recorded.as_ref().map(|recorded| TrackedProcess {
            id: recorded.process_id.clone(),
            store: process_store,
        });
        self.terminals.observe(terminal, recorded, None, tracked);
        Ok(terminal_id)
    }

    async fn screen_slot(&self, terminal_id: &str) -> Result<Arc<Mutex<ScreenSlot>>, BridgeError> {
        self.terminals
            .screens
            .lock()
            .await
            .get(terminal_id)
            .cloned()
            .ok_or_else(|| BridgeError::new("terminal", format!("no live terminal {terminal_id}")))
    }

    pub async fn search_terminal(
        &self,
        terminal_id: &str,
        query: &str,
        anchor: Option<u32>,
        direction: SearchDirection,
    ) -> Result<Option<u32>, BridgeError> {
        if query.len() > 1024 {
            return Err(BridgeError::new(
                "terminal",
                "search is limited to 1024 bytes",
            ));
        }
        let slot = self.screen_slot(terminal_id).await?;
        let mut slot = slot.lock().await;
        let found = slot
            .screen
            .search(query, anchor, direction)
            .map_err(|error| BridgeError::new("terminal", error.to_string()))?;
        if found.is_some() {
            slot.publish();
        }
        Ok(found)
    }

    pub async fn terminal_scroll(&self, terminal_id: &str, lines: i32) -> Result<(), BridgeError> {
        let slot = self.screen_slot(terminal_id).await?;
        let mut slot = slot.lock().await;
        slot.screen.scroll(lines);
        slot.publish();
        Ok(())
    }

    pub async fn terminal_follow_output(&self, terminal_id: &str) -> Result<(), BridgeError> {
        self.screen_slot(terminal_id)
            .await?
            .lock()
            .await
            .follow_output();
        Ok(())
    }

    pub async fn terminal_write(
        &self,
        terminal_id: String,
        data: TerminalInput,
        follow_output: bool,
    ) -> Result<(), BridgeError> {
        let slot = self.screen_slot(&terminal_id).await?;
        let task_id = {
            let slot = slot.lock().await;
            slot.task_id.clone().filter(|_| {
                slot.last_worktree_touch
                    .is_none_or(|last| last.elapsed() >= Duration::from_secs(60))
            })
        };
        if let Some(task_id) = task_id {
            self.core
                .lock()
                .await
                .touch_archived_worktree(&task_id)
                .await?;
            slot.lock().await.last_worktree_touch = Some(Instant::now());
        }
        let completion = self
            .terminals
            .engine
            .lock()
            .await
            .write(&TerminalId::new(terminal_id.clone()), data.into_bytes())
            .map_err(|error| BridgeError::new("terminal", error.to_string()))?;
        completion
            .await
            .map_err(|_| BridgeError::new("terminal", "the terminal writer stopped"))?
            .map_err(|error| BridgeError::new("terminal", error.to_string()))?;
        if follow_output {
            slot.lock().await.follow_output();
        }
        Ok(())
    }

    pub async fn open_terminal_link(
        &self,
        terminal_id: &str,
        row: usize,
        column: usize,
        revision: u32,
    ) -> Result<(), BridgeError> {
        let slot = self.screen_slot(terminal_id).await?;
        let link = {
            let slot = slot.lock().await;
            if slot.revision != revision {
                return Err(BridgeError::new(
                    "terminal",
                    "terminal output changed before the link could open",
                ));
            }
            slot.screen.link(row, column)
        }
        .ok_or_else(|| BridgeError::new("terminal", "no link at this position"))?;
        let url = tauri::Url::parse(&link)
            .ok()
            .filter(|url| matches!(url.scheme(), "http" | "https") && url.host().is_some())
            .ok_or_else(|| BridgeError::new("terminal", "terminal links must use HTTP or HTTPS"))?;
        open_in_browser(url).await.map_err(|error| {
            BridgeError::new(
                "terminal",
                format!("could not open the terminal link: {error}"),
            )
        })
    }

    pub async fn terminal_resize(
        &self,
        terminal_id: String,
        columns: u16,
        rows: u16,
    ) -> Result<(), BridgeError> {
        let slot = self.screen_slot(&terminal_id).await?;
        let mut slot = slot.lock().await;
        self.terminals
            .engine
            .lock()
            .await
            .resize(&TerminalId::new(terminal_id.clone()), columns, rows)
            .map_err(|error| BridgeError::new("terminal", error.to_string()))?;

        slot.screen.resize(usize::from(columns), usize::from(rows));
        slot.publish();
        Ok(())
    }

    pub async fn close_terminal(&self, terminal_id: String) -> Result<(), BridgeError> {
        let _guard = self.terminal_lifecycle.lock().await;
        self.terminals
            .stop_terminal_process(TerminalId::new(terminal_id))
            .await
    }

    pub async fn subscribe_terminal(
        &self,
        terminal_id: &str,
        channel: tauri::ipc::Channel<tauri::ipc::Response>,
    ) -> Result<String, BridgeError> {
        let slot = self.screen_slot(terminal_id).await?;
        let mut receiver = slot.lock().await.subscribe();
        let (cancel, mut cancelled) = oneshot::channel();
        let (acknowledgements, mut acknowledged) = watch::channel(0);
        let subscription_id = uuid::Uuid::new_v4().to_string();
        self.terminal_subscriptions.lock().await.insert(
            subscription_id.clone(),
            TerminalSubscription {
                _cancel: cancel,
                acknowledgements,
            },
        );
        let subscriptions = self.terminal_subscriptions.clone();
        let id = subscription_id.clone();
        tauri::async_runtime::spawn(async move {
            let mut previous = None;
            let mut sequence = 0u32;
            let mut next_send_at = Instant::now();
            loop {
                let frame = Arc::clone(&receiver.borrow_and_update());
                if previous.as_deref() != Some(frame.as_ref()) {
                    sequence = sequence.wrapping_add(1);
                    let mut payload = frame.encode(previous.as_deref());
                    payload.extend(frame.revision.to_le_bytes());
                    payload.extend(sequence.to_le_bytes());
                    let response = tauri::ipc::Response::new(payload);
                    if let Err(error) = channel.send(response) {
                        tracing::debug!(%error, "terminal view detached");
                        break;
                    }
                    next_send_at = Instant::now() + Duration::from_millis(16);
                    let received = tokio::select! {
                        _ = &mut cancelled => false,
                        result = tokio::time::timeout(
                            Duration::from_secs(30),
                            acknowledged.wait_for(|value| *value == sequence),
                        ) => matches!(result, Ok(Ok(_))),
                    };
                    if !received {
                        let mut active = subscriptions.lock().await;
                        // An acknowledgement can arrive just after the timer fires.
                        if !active.contains_key(&id) || *acknowledged.borrow() != sequence {
                            active.remove(&id);
                            tracing::debug!(%id, sequence, "terminal frame was not acknowledged");
                            return;
                        }
                    }
                    previous = Some(frame);
                }
                tokio::select! {
                    _ = &mut cancelled => break,
                    result = receiver.changed() => if result.is_err() { break },
                }
                // Coalesce bursts without delaying the next frame after idle time.
                if next_send_at > Instant::now() {
                    tokio::select! {
                        _ = &mut cancelled => break,
                        _ = tokio::time::sleep_until(next_send_at) => {}
                    }
                }
            }
            subscriptions.lock().await.remove(&id);
        });
        Ok(subscription_id)
    }

    pub async fn acknowledge_terminal_frame(
        &self,
        subscription_id: &str,
        sequence: u32,
    ) -> Result<(), BridgeError> {
        let subscriptions = self.terminal_subscriptions.lock().await;
        let subscription = subscriptions.get(subscription_id).ok_or_else(|| {
            BridgeError::new(
                "terminal_subscription_ended",
                "terminal frame subscription ended",
            )
        })?;
        subscription.acknowledgements.send_replace(sequence);
        Ok(())
    }

    pub async fn unsubscribe_terminal(&self, subscription_id: &str) {
        self.terminal_subscriptions
            .lock()
            .await
            .remove(subscription_id);
    }
}

async fn install_startup_scan(
    cache: &Mutex<ProjectScanCache>,
    found: shepherd_core::project_scan::ProjectScan,
) {
    let mut cache = cache.lock().await;
    if cache.startup_valid && cache.value.is_none() {
        cache.value = Some(found);
    }
}

async fn request_project_scan(
    cache: &Mutex<ProjectScanCache>,
    rescan: bool,
    scan: impl std::future::Future<
        Output = Result<shepherd_core::project_scan::ProjectScan, BridgeError>,
    >,
) -> Result<shepherd_core::project_scan::ProjectScan, BridgeError> {
    let mut cache = cache.lock().await;
    if rescan || cache.value.is_none() {
        cache.startup_valid = false;
        cache.value = Some(scan.await?);
    }
    cache
        .value
        .clone()
        .ok_or_else(|| BridgeError::new("scan", "the project scan produced no result"))
}

async fn scan_project_root(
    root: PathBuf,
) -> Result<shepherd_core::project_scan::ProjectScan, BridgeError> {
    tokio::task::spawn_blocking(move || shepherd_core::project_scan::scan(&root))
        .await
        .map_err(|error| {
            BridgeError::new("scan", format!("the project scan did not finish: {error}"))
        })?
        .map_err(|error| BridgeError::new("scan", error.to_string()))
}

async fn open_in_browser(url: tauri::Url) -> std::io::Result<()> {
    let program = match std::env::consts::OS {
        "macos" => "open",
        "linux" => "xdg-open",
        _ => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "Opening a browser is unavailable on this platform.",
            ));
        }
    };
    let status = tokio::process::Command::new(program)
        .arg(url.as_str())
        .status()
        .await?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(status.to_string()))
    }
}

fn project_scan_root() -> Option<PathBuf> {
    if cfg!(debug_assertions)
        && let Some(path) = std::env::var_os("SHEPHERD_DEV_PROJECT_ROOT")
        && !path.is_empty()
    {
        return Some(path.into());
    }
    std::env::var_os("HOME").map(PathBuf::from)
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// The invoke commands. Each one delegates to [`ShellState`] so the behaviour is
/// testable without a webview.
pub mod commands {
    use tauri::State;

    use super::*;

    #[tauri::command]
    pub async fn archived_worktree_issues(
        state: State<'_, ShellState>,
    ) -> Result<Vec<shepherd_core::ArchivedWorktreeIssue>, BridgeError> {
        Ok(state.core.lock().await.archived_worktree_issues().to_vec())
    }

    #[tauri::command]
    pub async fn inspect_previous_processes(
        state: State<'_, ShellState>,
    ) -> Result<Vec<crate::orphan_process::ProcessInspection>, BridgeError> {
        let records = state.previous_processes.lock().await.clone();
        tokio::task::spawn_blocking(move || crate::orphan_process::inspect(records))
            .await
            .map_err(|error| BridgeError::new("process_inspection", error.to_string()))
    }

    #[tauri::command]
    pub async fn stop_previous_process(
        state: State<'_, ShellState>,
        id: String,
    ) -> Result<Vec<crate::orphan_process::ProcessInspection>, BridgeError> {
        let record = state
            .previous_processes
            .lock()
            .await
            .iter()
            .find(|record| record.id == id)
            .cloned()
            .ok_or_else(|| {
                BridgeError::new(
                    "process_inspection",
                    "The recorded process is no longer known.",
                )
            })?;
        let leases = state.terminals.process_leases().ok_or_else(|| {
            BridgeError::new(
                "process_inspection",
                "Agent process leases are unavailable.",
            )
        })?;
        let stopping = record.clone();
        tokio::task::spawn_blocking(move || crate::orphan_process::stop(&leases, &stopping))
            .await
            .map_err(|error| BridgeError::new("process_inspection", error.to_string()))?
            .map_err(|error| {
                BridgeError::new(
                    "process_inspection",
                    format!("Could not stop the recorded process: {error}"),
                )
            })?;
        state
            .core
            .lock()
            .await
            .remove_process(&record.id)
            .await
            .map_err(BridgeError::from)?;
        let remaining = {
            let mut records = state.previous_processes.lock().await;
            records.retain(|seen| seen.id != record.id);
            records.clone()
        };
        tokio::task::spawn_blocking(move || crate::orphan_process::inspect(remaining))
            .await
            .map_err(|error| BridgeError::new("process_inspection", error.to_string()))
    }

    #[tauri::command]
    pub async fn set_terminal_appearance(
        state: State<'_, ShellState>,
        appearance: TerminalAppearance,
    ) -> Result<(), BridgeError> {
        state.terminals.set_appearance(appearance).await;
        Ok(())
    }

    #[tauri::command]
    pub fn agent_availability(state: State<'_, ShellState>) -> BTreeMap<&'static str, bool> {
        BTreeMap::from([
            ("codex", state.codex_detected),
            ("opencode", state.opencode_launch.is_some()),
            ("pi", state.pi_configured),
            ("claude", state.claude_configured),
        ])
    }

    #[tauri::command]
    pub async fn add_project(
        state: State<'_, ShellState>,
        path: String,
    ) -> Result<ProjectList, BridgeError> {
        state.inner().add_project(path).await
    }

    #[tauri::command]
    pub async fn set_project_preview_url(
        state: State<'_, ShellState>,
        project_id: String,
        url: String,
    ) -> Result<ProjectList, BridgeError> {
        state.inner().set_project_preview_url(project_id, url).await
    }

    #[tauri::command]
    pub async fn open_project_preview(
        state: State<'_, ShellState>,
        project_id: String,
    ) -> Result<(), BridgeError> {
        state.inner().open_project_preview(project_id).await
    }

    #[tauri::command]
    pub async fn open_project_annotation_wrapper(
        state: State<'_, ShellState>,
        project_id: String,
        task_id: Option<String>,
    ) -> Result<String, BridgeError> {
        state
            .inner()
            .open_project_annotation_wrapper(project_id, task_id)
            .await
    }

    #[tauri::command]
    pub async fn validate_preview_annotations(
        state: State<'_, ShellState>,
        batch: AnnotationBatch,
    ) -> Result<AnnotationBatch, BridgeError> {
        state.inner().validate_preview_annotations(batch).await
    }

    #[tauri::command]
    pub async fn attachment_limit(
        state: State<'_, ShellState>,
        project_id: String,
    ) -> Result<AttachmentLimit, BridgeError> {
        state.inner().attachment_limit(project_id).await
    }

    #[tauri::command]
    pub async fn save_preview_screenshot(
        state: State<'_, ShellState>,
        request: tauri::ipc::Request<'_>,
    ) -> Result<PathBuf, BridgeError> {
        let task_id = request
            .headers()
            .get("x-shepherd-task")
            .and_then(|header| header.to_str().ok())
            .ok_or_else(|| BridgeError::new("preview", "Select a task for the screenshot."))?;
        let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else {
            return Err(BridgeError::new(
                "preview",
                "The screenshot did not contain image bytes.",
            ));
        };
        state
            .inner()
            .save_preview_screenshot(task_id.to_owned(), bytes)
            .await
    }

    #[tauri::command]
    pub async fn save_prompt_file(
        state: State<'_, ShellState>,
        request: tauri::ipc::Request<'_>,
    ) -> Result<String, BridgeError> {
        let task_id = request
            .headers()
            .get("x-shepherd-task")
            .and_then(|header| header.to_str().ok())
            .ok_or_else(|| BridgeError::new("attachments", "Select a task for the attachment."))?;
        let name = request
            .headers()
            .get("x-shepherd-filename")
            .and_then(|header| header.to_str().ok())
            .ok_or_else(|| {
                BridgeError::new("attachments", "The attachment is missing its file name.")
            })?;
        let name = percent_encoding::percent_decode_str(name)
            .decode_utf8()
            .map_err(|error| {
                BridgeError::new("attachments", format!("Invalid attachment name. {error}"))
            })?;
        let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else {
            return Err(BridgeError::new(
                "attachments",
                "The attachment did not contain file bytes.",
            ));
        };
        state
            .save_prompt_file(task_id.to_owned(), name.into_owned(), bytes)
            .await
    }

    #[tauri::command]
    pub async fn create_draft(
        state: State<'_, ShellState>,
        project_id: String,
        title: String,
        agent: Option<Agent>,
    ) -> Result<TaskWorkspace, BridgeError> {
        state
            .inner()
            .create_draft(project_id, title, agent.unwrap_or_default())
            .await
    }

    #[tauri::command]
    pub async fn set_draft_agent(
        state: State<'_, ShellState>,
        task_id: String,
        agent: Agent,
    ) -> Result<TaskWorkspace, BridgeError> {
        state.inner().set_draft_agent(task_id, agent).await
    }

    #[tauri::command]
    pub fn opencode_configured(state: State<'_, ShellState>) -> bool {
        state.opencode_launch.is_some()
    }

    #[tauri::command]
    pub fn pi_configured(state: State<'_, ShellState>) -> bool {
        state.pi_configured
    }

    #[tauri::command]
    pub fn claude_configured(state: State<'_, ShellState>) -> bool {
        state.claude_configured
    }

    #[tauri::command]
    pub async fn discard_draft(
        state: State<'_, ShellState>,
        task_id: String,
    ) -> Result<(), BridgeError> {
        state.inner().discard_draft(task_id).await
    }

    #[tauri::command]
    pub async fn delete_task(
        state: State<'_, ShellState>,
        task_id: String,
        remove_worktree: bool,
    ) -> Result<(), BridgeError> {
        state.inner().delete_task(task_id, remove_worktree).await
    }

    #[tauri::command]
    pub async fn send_prompt(
        state: State<'_, ShellState>,
        task_id: String,
        text: String,
        attachment_ids: Option<Vec<String>>,
        codex: Option<CodexTurnOptions>,
        mode: Option<String>,
    ) -> Result<TaskWorkspace, BridgeError> {
        state
            .inner()
            .send_prompt_with_files(
                task_id,
                text,
                attachment_ids.unwrap_or_default(),
                codex.unwrap_or_default(),
                mode,
            )
            .await
    }

    #[tauri::command]
    pub async fn set_session_mode(
        state: State<'_, ShellState>,
        task_id: String,
        mode: String,
    ) -> Result<TaskWorkspace, BridgeError> {
        state
            .workspace(Command::SetSessionMode {
                task_id: TaskId::new(task_id),
                mode,
            })
            .await
    }

    #[tauri::command]
    pub async fn resolve_approval(
        state: State<'_, ShellState>,
        task_id: String,
        approval_id: String,
        decision: Decision,
    ) -> Result<TaskWorkspace, BridgeError> {
        state
            .inner()
            .resolve_approval(task_id, approval_id, decision)
            .await
    }

    #[tauri::command]
    pub async fn project_approvals(
        state: State<'_, ShellState>,
        project_id: String,
    ) -> Result<ProjectApprovalsSnapshot, BridgeError> {
        state.inner().project_approvals(project_id).await
    }

    #[tauri::command]
    pub async fn reset_approval_grant(
        state: State<'_, ShellState>,
        project_id: String,
        approval_id: String,
        task_id: Option<String>,
    ) -> Result<(), BridgeError> {
        state
            .inner()
            .reset_approval_grant(project_id, approval_id, task_id)
            .await
    }

    #[tauri::command]
    pub async fn cancel_task(
        state: State<'_, ShellState>,
        task_id: String,
    ) -> Result<TaskWorkspace, BridgeError> {
        state.inner().cancel_task(task_id).await
    }

    #[tauri::command]
    pub async fn resume_task(
        state: State<'_, ShellState>,
        task_id: String,
    ) -> Result<TaskWorkspace, BridgeError> {
        state.inner().resume_task(task_id).await
    }

    #[tauri::command]
    pub async fn quit(
        state: State<'_, ShellState>,
        force: bool,
    ) -> Result<QuitSummary, BridgeError> {
        state.inner().quit(force).await
    }

    #[tauri::command]
    pub async fn project_list(state: State<'_, ShellState>) -> Result<ProjectList, BridgeError> {
        Ok(state.inner().project_list().await)
    }

    #[tauri::command]
    pub async fn project_remote(
        state: State<'_, ShellState>,
        project_id: String,
    ) -> Result<Option<ProjectRemote>, BridgeError> {
        state.inner().project_remote(project_id).await
    }

    #[tauri::command]
    pub async fn project_scan(
        state: State<'_, ShellState>,
        rescan: bool,
    ) -> Result<shepherd_core::project_scan::ProjectScan, BridgeError> {
        state.inner().project_scan(rescan).await
    }

    #[tauri::command]
    pub fn local_codex_available() -> bool {
        agent_harness::codex::desktop_project_program().is_some()
    }

    #[tauri::command]
    pub async fn codex_project_models(
        state: State<'_, ShellState>,
        project_id: String,
    ) -> Result<CodexProjectModels, BridgeError> {
        state.codex_project_models(project_id).await
    }

    #[tauri::command]
    pub async fn opencode_project_modes(
        state: State<'_, ShellState>,
        project_id: String,
    ) -> Result<Vec<AgentMode>, BridgeError> {
        state.opencode_project_modes(project_id).await
    }

    #[tauri::command]
    pub async fn local_codex_projects() -> Result<Vec<LocalCodexProject>, BridgeError> {
        let projects = agent_harness::codex::desktop_projects()
            .await
            .map_err(|error| {
                BridgeError::new(
                    "project_import",
                    format!("Could not read projects from the ChatGPT app: {error}"),
                )
            })?;
        let mut paths = BTreeSet::new();
        let mut folders = Vec::new();
        for project in projects {
            if !project.path.is_absolute() {
                continue;
            }
            let path = match tokio::fs::canonicalize(&project.path).await {
                Ok(path) => path,
                Err(error) => {
                    tracing::debug!(%error, "could not resolve a Codex project folder");
                    continue;
                }
            };
            match tokio::fs::metadata(&path).await {
                Ok(metadata) if metadata.is_dir() => {}
                Ok(_) => continue,
                Err(error) => {
                    tracing::debug!(%error, "could not inspect a Codex project folder");
                    continue;
                }
            }
            if let Ok(path) = path.into_os_string().into_string()
                && paths.insert(path.clone())
            {
                folders.push(LocalCodexProject {
                    name: project.name,
                    path,
                });
            }
        }
        Ok(folders)
    }

    #[tauri::command]
    pub async fn task_list(
        state: State<'_, ShellState>,
        project_id: String,
    ) -> Result<TaskList, BridgeError> {
        state.inner().task_list(project_id).await
    }

    #[tauri::command]
    pub async fn pool_status(state: State<'_, ShellState>) -> Result<PoolStatus, BridgeError> {
        state.inner().pool_status().await
    }

    #[tauri::command]
    pub async fn pool_reorder_slots(
        state: State<'_, ShellState>,
        slot_ids: Vec<String>,
    ) -> Result<PoolReorderOutcome, BridgeError> {
        state.inner().reorder_pool_slots(slot_ids).await
    }

    #[tauri::command]
    pub async fn pool_set_slot_enabled(
        state: State<'_, ShellState>,
        slot_id: String,
        enabled: bool,
    ) -> Result<PoolSlotEnabledOutcome, BridgeError> {
        state.inner().set_pool_slot_enabled(slot_id, enabled).await
    }

    #[tauri::command]
    pub async fn pool_correct_quota(
        state: State<'_, ShellState>,
        slot_id: String,
        limit: Option<u64>,
        limit_unit: Option<String>,
        reset_at: Option<u64>,
        window_key: Option<String>,
    ) -> Result<PoolQuotaCorrectionOutcome, BridgeError> {
        state
            .inner()
            .correct_pool_quota(slot_id, limit, limit_unit, reset_at, window_key)
            .await
    }

    #[tauri::command]
    pub async fn pool_clear_cooldown(
        state: State<'_, ShellState>,
        slot_id: String,
    ) -> Result<PoolQuotaCorrectionOutcome, BridgeError> {
        state.inner().clear_pool_slot_cooldown(slot_id).await
    }

    #[tauri::command]
    pub async fn pool_force_slot(
        state: State<'_, ShellState>,
        slot_id: String,
    ) -> Result<PoolForceOutcome, BridgeError> {
        state.inner().force_pool_slot(slot_id).await
    }

    #[tauri::command]
    pub async fn pool_clear_force(
        state: State<'_, ShellState>,
    ) -> Result<PoolForceOutcome, BridgeError> {
        state.inner().clear_pool_force().await
    }

    #[tauri::command]
    pub async fn pool_apply_catalog(
        state: State<'_, ShellState>,
        snapshot: String,
    ) -> Result<PoolCatalogOutcome, BridgeError> {
        state.inner().apply_pool_catalog(snapshot).await
    }

    #[tauri::command]
    pub async fn pool_generated_catalog(
        state: State<'_, ShellState>,
    ) -> Result<Option<String>, BridgeError> {
        state.inner().generated_pool_catalog().await
    }

    #[tauri::command]
    pub async fn pool_add_slot(
        state: State<'_, ShellState>,
        agent: String,
        provider_id: String,
        provider_name: String,
        model_id: String,
        model_name: String,
    ) -> Result<PoolSlot, BridgeError> {
        state
            .inner()
            .add_pool_slot(agent, provider_id, provider_name, model_id, model_name)
            .await
    }

    #[tauri::command]
    pub async fn review_task_lists(
        state: State<'_, ShellState>,
    ) -> Result<Vec<TaskList>, BridgeError> {
        Ok(state.core.lock().await.review_task_lists())
    }

    #[tauri::command]
    pub async fn archive_reviewed(
        state: State<'_, ShellState>,
        task_ids: Vec<String>,
    ) -> Result<(), BridgeError> {
        state
            .execute(Command::ArchiveReviewed {
                task_ids: task_ids.into_iter().map(TaskId::new).collect(),
            })
            .await?;
        Ok(())
    }

    #[tauri::command]
    pub async fn mutate_git(
        state: State<'_, ShellState>,
        task_id: String,
        mutation: shepherd_core::git::GitMutation,
    ) -> Result<shepherd_core::git::GitSnapshot, BridgeError> {
        Ok(state
            .core
            .lock()
            .await
            .mutate_git(&TaskId::new(task_id), mutation)
            .await?)
    }

    #[tauri::command]
    pub async fn git_file_diff(
        state: State<'_, ShellState>,
        task_id: String,
        file: String,
    ) -> Result<shepherd_core::git::GitFileDiff, BridgeError> {
        state
            .git_read(TaskId::new(task_id), move |context| {
                context.git_file_diff(&file)
            })
            .await
    }

    #[tauri::command]
    pub async fn git_snapshot(
        state: State<'_, ShellState>,
        task_id: String,
    ) -> Result<shepherd_core::git::GitSnapshot, BridgeError> {
        state
            .git_read(TaskId::new(task_id), |context| context.git_snapshot())
            .await
    }

    #[tauri::command]
    pub async fn list_files(
        state: State<'_, ShellState>,
        project_id: String,
        task_id: Option<String>,
        path: String,
        show_dotfiles: bool,
    ) -> Result<DirectoryListing, BridgeError> {
        let root = state.files_directory(project_id, task_id).await?;
        tokio::task::spawn_blocking(move || {
            files::list_directory(&root, Path::new(&path), show_dotfiles).map_err(|error| {
                let mut error = BridgeError::from(error);
                error.message = format!(
                    "Could not list {}: {}",
                    root.join(&path).display(),
                    error.message
                );
                error
            })
        })
        .await
        .map_err(|error| {
            BridgeError::new(
                "files",
                format!("Could not finish reading the folder: {error}"),
            )
        })?
    }

    #[tauri::command]
    pub async fn read_workspace_file(
        state: State<'_, ShellState>,
        project_id: String,
        task_id: Option<String>,
        path: String,
        line: Option<usize>,
    ) -> Result<TextFile, BridgeError> {
        let root = state.files_directory(project_id, task_id).await?;
        Ok(
            tokio::task::spawn_blocking(move || files::read_file(&root, Path::new(&path), line))
                .await
                .map_err(|error| {
                    BridgeError::new(
                        "files",
                        format!("Could not finish reading the file: {error}"),
                    )
                })??,
        )
    }

    #[tauri::command]
    pub async fn act_on_workspace_file(
        state: State<'_, ShellState>,
        project_id: String,
        task_id: Option<String>,
        path: String,
        action: FileAction,
    ) -> Result<(), BridgeError> {
        let root = state.files_directory(project_id, task_id).await?;
        let failure = move |error: &dyn std::fmt::Display| {
            BridgeError::new("files", format!("{}: {error}", action.error_context()))
        };
        tokio::task::spawn_blocking(move || {
            let path = files::external_file_path(&root, Path::new(&path))
                .map_err(|error| failure(&error))?;
            file_actions::perform(&path, action).map_err(|error| failure(&error))
        })
        .await
        .map_err(|error| failure(&error))?
    }

    #[tauri::command]
    pub async fn watch_workspace_files(
        state: State<'_, ShellState>,
        project_id: String,
        task_id: Option<String>,
        channel: tauri::ipc::Channel<FileWatchUpdate>,
    ) -> Result<String, BridgeError> {
        state
            .watch_workspace_files(project_id, task_id, channel)
            .await
    }

    #[tauri::command]
    pub async fn unwatch_workspace_files(
        state: State<'_, ShellState>,
        watch_id: String,
    ) -> Result<(), BridgeError> {
        state.file_watches.lock().await.remove(&watch_id);
        Ok(())
    }

    #[tauri::command]
    pub async fn reserve_file_search(state: State<'_, ShellState>) -> Result<String, BridgeError> {
        state.reserve_file_search().await
    }

    #[tauri::command]
    pub async fn search_workspace_files(
        state: State<'_, ShellState>,
        search_id: String,
        project_id: String,
        task_id: Option<String>,
        query: FileSearchQuery,
    ) -> Result<FileSearchResult, BridgeError> {
        state
            .search_workspace_files(&search_id, project_id, task_id, query)
            .await
    }

    #[tauri::command]
    pub async fn cancel_file_search(
        state: State<'_, ShellState>,
        search_id: String,
    ) -> Result<(), BridgeError> {
        state.cancel_file_search(&search_id).await;
        Ok(())
    }

    #[tauri::command]
    pub async fn review_task(
        state: State<'_, ShellState>,
        task_id: String,
        action: shepherd_core::ReviewAction,
    ) -> Result<TaskWorkspace, BridgeError> {
        state
            .workspace(Command::ReviewTask {
                task_id: TaskId::new(task_id),
                action,
            })
            .await
    }

    #[tauri::command]
    pub async fn review_file_diff(
        state: State<'_, ShellState>,
        task_id: String,
        file: String,
    ) -> Result<String, BridgeError> {
        state
            .git_read(TaskId::new(task_id), move |context| {
                context.review_file_diff(&file)
            })
            .await
    }

    #[tauri::command]
    pub async fn review_snapshot(
        state: State<'_, ShellState>,
        task_id: String,
    ) -> Result<shepherd_core::git::ReviewSnapshot, BridgeError> {
        state
            .git_read(TaskId::new(task_id), |context| context.review_snapshot())
            .await
    }

    #[tauri::command]
    pub async fn task_workspace(
        state: State<'_, ShellState>,
        task_id: String,
    ) -> Result<TaskWorkspace, BridgeError> {
        state.inner().task_workspace(task_id).await
    }

    #[tauri::command]
    pub async fn reserve_task_search(state: State<'_, ShellState>) -> Result<String, BridgeError> {
        state.reserve_task_search().await
    }

    #[tauri::command]
    pub async fn search_tasks(
        state: State<'_, ShellState>,
        search_id: String,
        query: String,
    ) -> Result<Vec<shepherd_core::search::SearchHit>, BridgeError> {
        state.search_tasks(&search_id, &query).await
    }

    #[tauri::command]
    pub async fn cancel_task_search(
        state: State<'_, ShellState>,
        search_id: String,
    ) -> Result<(), BridgeError> {
        state.cancel_task_search(&search_id).await;
        Ok(())
    }

    #[tauri::command]
    pub async fn save_workspace_layout(
        state: State<'_, ShellState>,
        task_id: String,
        layout: WorkspaceLayout,
    ) -> Result<WorkspaceLayout, BridgeError> {
        Ok(state
            .core
            .lock()
            .await
            .save_workspace_layout(&TaskId::new(task_id), layout)
            .await?)
    }

    #[tauri::command]
    pub async fn terminal_layout(
        state: State<'_, ShellState>,
        project_id: String,
    ) -> Result<shepherd_core::terminal::TerminalLayout, BridgeError> {
        Ok(state
            .core
            .lock()
            .await
            .terminal_layout(&ProjectId::new(project_id))?)
    }

    #[tauri::command]
    pub async fn save_terminal_layout(
        state: State<'_, ShellState>,
        project_id: String,
        layout: shepherd_core::terminal::TerminalLayout,
    ) -> Result<shepherd_core::terminal::TerminalLayout, BridgeError> {
        Ok(state
            .core
            .lock()
            .await
            .save_terminal_layout(&ProjectId::new(project_id), layout)
            .await?)
    }

    #[tauri::command]
    pub async fn save_inspector_tab(
        state: State<'_, ShellState>,
        project_id: String,
        tab: InspectorTab,
    ) -> Result<(), BridgeError> {
        Ok(state
            .core
            .lock()
            .await
            .save_inspector_tab(&ProjectId::new(project_id), tab)
            .await?)
    }

    #[tauri::command]
    pub async fn terminal_list(
        state: State<'_, ShellState>,
        project_id: String,
    ) -> Result<TerminalList, BridgeError> {
        Ok(state.inner().terminal_list(project_id).await)
    }

    #[tauri::command]
    pub async fn start_terminal(
        state: State<'_, ShellState>,
        project_id: String,
        task_id: Option<String>,
        columns: u16,
        rows: u16,
    ) -> Result<TerminalList, BridgeError> {
        state
            .inner()
            .start_terminal(project_id, task_id, columns, rows)
            .await
    }

    #[tauri::command]
    pub async fn start_freebuff_terminal(
        state: State<'_, ShellState>,
        project_id: String,
    ) -> Result<String, BridgeError> {
        state.start_freebuff_terminal(project_id).await
    }

    #[tauri::command]
    pub async fn restart_terminal(
        state: State<'_, ShellState>,
        terminal_id: String,
        columns: u16,
        rows: u16,
    ) -> Result<TerminalList, BridgeError> {
        state
            .inner()
            .restart_terminal(terminal_id, columns, rows)
            .await
    }

    #[tauri::command]
    pub async fn search_terminal(
        state: State<'_, ShellState>,
        terminal_id: String,
        query: String,
        anchor: Option<u32>,
        direction: SearchDirection,
    ) -> Result<Option<u32>, BridgeError> {
        state
            .inner()
            .search_terminal(&terminal_id, &query, anchor, direction)
            .await
    }

    #[tauri::command]
    pub async fn terminal_scroll(
        state: State<'_, ShellState>,
        terminal_id: String,
        lines: i32,
    ) -> Result<(), BridgeError> {
        state.inner().terminal_scroll(&terminal_id, lines).await
    }

    #[tauri::command]
    pub async fn terminal_follow_output(
        state: State<'_, ShellState>,
        terminal_id: String,
    ) -> Result<(), BridgeError> {
        state.inner().terminal_follow_output(&terminal_id).await
    }

    #[tauri::command]
    pub async fn terminal_write(
        state: State<'_, ShellState>,
        terminal_id: String,
        data: TerminalInput,
        follow_output: bool,
    ) -> Result<(), BridgeError> {
        state
            .inner()
            .terminal_write(terminal_id, data, follow_output)
            .await
    }

    #[tauri::command]
    pub async fn open_terminal_link(
        state: State<'_, ShellState>,
        terminal_id: String,
        row: usize,
        column: usize,
        revision: u32,
    ) -> Result<(), BridgeError> {
        state
            .inner()
            .open_terminal_link(&terminal_id, row, column, revision)
            .await
    }

    #[tauri::command]
    pub async fn terminal_resize(
        state: State<'_, ShellState>,
        terminal_id: String,
        columns: u16,
        rows: u16,
    ) -> Result<(), BridgeError> {
        state
            .inner()
            .terminal_resize(terminal_id, columns, rows)
            .await
    }

    #[tauri::command]
    pub async fn close_terminal(
        state: State<'_, ShellState>,
        terminal_id: String,
    ) -> Result<(), BridgeError> {
        state.inner().close_terminal(terminal_id).await
    }

    #[tauri::command]
    pub async fn subscribe_terminal(
        state: State<'_, ShellState>,
        terminal_id: String,
        channel: tauri::ipc::Channel<tauri::ipc::Response>,
    ) -> Result<String, BridgeError> {
        state
            .inner()
            .subscribe_terminal(&terminal_id, channel)
            .await
    }

    #[tauri::command]
    pub async fn acknowledge_terminal_frame(
        state: State<'_, ShellState>,
        subscription_id: String,
        sequence: u32,
    ) -> Result<(), BridgeError> {
        state
            .inner()
            .acknowledge_terminal_frame(&subscription_id, sequence)
            .await
    }

    #[tauri::command]
    pub async fn unsubscribe_terminal(
        state: State<'_, ShellState>,
        subscription_id: String,
    ) -> Result<(), BridgeError> {
        state.inner().unsubscribe_terminal(&subscription_id).await;
        Ok(())
    }

    /// The window's single event stream. The channel stays open for the life of
    /// this task; a closed channel ends the forwarder.
    #[tauri::command]
    pub async fn subscribe(
        state: State<'_, ShellState>,
        channel: tauri::ipc::Channel<Event>,
    ) -> Result<(), BridgeError> {
        let mut receiver = state.inner().subscribe();
        tauri::async_runtime::spawn(async move {
            while let Some(event) = next_core_event(&mut receiver).await {
                if let Err(error) = channel.send(event) {
                    tracing::debug!(%error, "the shell event channel closed");
                    break;
                }
            }
        });
        Ok(())
    }
}

async fn next_core_event(receiver: &mut broadcast::Receiver<Event>) -> Option<Event> {
    match receiver.recv().await {
        Ok(event) => Some(event),
        Err(broadcast::error::RecvError::Lagged(skipped)) => {
            tracing::warn!(skipped, "the shell missed core events; reload snapshots");
            *receiver = receiver.resubscribe();
            Some(Event::ResyncRequired)
        }
        Err(broadcast::error::RecvError::Closed) => None,
    }
}

#[cfg(test)]
async fn pause_attachment_deletion_fixture(
    phase: &str,
    task_id: &TaskId,
) -> Result<(), BridgeError> {
    if std::env::var("SHEPHERD_ATTACHMENT_CRASH_PHASE")
        .ok()
        .as_deref()
        == Some(phase)
    {
        let path = std::env::var_os("SHEPHERD_ATTACHMENT_CRASH_READY")
            .map(PathBuf::from)
            .ok_or_else(|| BridgeError::new("attachments", "missing fixture ready path"))?;
        std::fs::write(&path, task_id.as_str())
            .map_err(|error| BridgeError::new("attachments", error.to_string()))?;
        std::future::pending::<()>().await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn lagged_core_events_request_a_snapshot_reload_and_skip_stale_events() {
        let (sender, mut receiver) = tokio::sync::broadcast::channel(2);
        for _ in 0..3 {
            assert!(
                sender
                    .send(shepherd_core::events::Event::TaskNeedsInput {
                        task_id: "old".into(),
                    })
                    .is_ok()
            );
        }
        assert!(matches!(
            super::next_core_event(&mut receiver).await,
            Some(shepherd_core::events::Event::ResyncRequired)
        ));
        assert!(
            sender
                .send(shepherd_core::events::Event::TaskNeedsInput {
                    task_id: "new".into(),
                })
                .is_ok()
        );
        assert!(matches!(super::next_core_event(&mut receiver).await,
            Some(shepherd_core::events::Event::TaskNeedsInput { task_id }) if task_id == "new"));
    }
    use shepherd_core::store::MemoryStore;
    use shepherd_core::test_support::{TestDirectory, TestRepository};
    use shepherd_core::{TaskState, TaskSummary};

    use super::*;

    #[tokio::test]
    async fn file_watcher_retries_once_reported_failure_and_stops_on_cancel()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::sync::atomic::AtomicUsize;

        let attempts = Arc::new(AtomicUsize::new(0));
        let (_keep_open, mut cancelled) = oneshot::channel();
        let mut errors = Vec::new();
        let result = retry_file_watcher(
            &mut cancelled,
            Duration::from_millis(5),
            || {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                async move {
                    if attempt < 2 {
                        Err(BridgeError::new("files", "fixture unavailable"))
                    } else {
                        Ok(7)
                    }
                }
            },
            |message| {
                errors.push(message);
                Ok(())
            },
        )
        .await;
        assert_eq!(result, Some(7));
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
        assert_eq!(errors, ["fixture unavailable"]);

        let attempts = Arc::new(AtomicUsize::new(0));
        let (cancel, mut cancelled) = oneshot::channel();
        let (reported, received) = oneshot::channel();
        let worker = tokio::spawn({
            let attempts = attempts.clone();
            async move {
                let mut reported = Some(reported);
                retry_file_watcher(
                    &mut cancelled,
                    Duration::from_secs(30),
                    || {
                        attempts.fetch_add(1, Ordering::SeqCst);
                        async { Err::<(), _>(BridgeError::new("files", "fixture unavailable")) }
                    },
                    |message| {
                        if let Some(reported) = reported.take() {
                            reported
                                .send(message)
                                .map_err(|_| "test listener closed".to_string())?;
                        }
                        Ok(())
                    },
                )
                .await
            }
        });
        assert_eq!(received.await?, "fixture unavailable");
        cancel.send(()).map_err(|_| "watcher already stopped")?;
        assert!(
            tokio::time::timeout(Duration::from_secs(2), worker)
                .await??
                .is_none()
        );
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        Ok(())
    }

    #[tokio::test]
    async fn closing_file_watch_cancels_an_in_flight_start()
    -> Result<(), Box<dyn std::error::Error>> {
        struct DropMarker(Arc<AtomicBool>);
        impl Drop for DropMarker {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let dropped = Arc::new(AtomicBool::new(false));
        let (cancel, mut cancelled) = oneshot::channel();
        let (started, ready) = oneshot::channel();
        let worker = tokio::spawn({
            let dropped = dropped.clone();
            async move {
                let mut started = Some(started);
                retry_file_watcher(
                    &mut cancelled,
                    Duration::from_secs(30),
                    || {
                        let marker = DropMarker(dropped.clone());
                        let started = started.take();
                        async move {
                            if let Some(started) = started {
                                started.send(()).map_err(|_| {
                                    BridgeError::new("files", "test listener closed")
                                })?;
                            }
                            let _marker = marker;
                            std::future::pending::<Result<(), BridgeError>>().await
                        }
                    },
                    |_| Ok(()),
                )
                .await
            }
        });
        ready.await?;
        cancel.send(()).map_err(|_| "watcher already stopped")?;
        assert!(
            tokio::time::timeout(Duration::from_secs(2), worker)
                .await??
                .is_none()
        );
        assert!(dropped.load(Ordering::SeqCst));
        Ok(())
    }

    #[tokio::test]
    async fn timed_out_file_watcher_start_holds_capacity_until_worker_finishes()
    -> Result<(), Box<dyn std::error::Error>> {
        struct DropMarker(Arc<AtomicBool>, Arc<Semaphore>);
        impl Drop for DropMarker {
            fn drop(&mut self) {
                self.0
                    .store(self.1.available_permits() == 0, Ordering::SeqCst);
            }
        }
        let capacity = Arc::new(Semaphore::new(1));
        let dropped = Arc::new(AtomicBool::new(false));
        let (release, blocked) = std::sync::mpsc::channel();
        let (entered, started) = oneshot::channel();
        let waiting = tokio::spawn({
            let capacity = capacity.clone();
            let worker_capacity = capacity.clone();
            let dropped = dropped.clone();
            async move {
                start_file_watcher(capacity, Duration::from_millis(100), move || {
                    entered.send(()).map_err(|_| WatchError::Closed)?;
                    blocked
                        .recv()
                        .map_err(|error| WatchError::Io(std::io::Error::other(error)))?;
                    Ok(DropMarker(dropped, worker_capacity))
                })
                .await
            }
        });
        started.await?;
        let error = match waiting.await? {
            Ok(_) => return Err("watcher unexpectedly started".into()),
            Err(error) => error,
        };
        assert!(error.to_string().contains("too long"));
        assert_eq!(capacity.available_permits(), 0);
        let saturated =
            start_file_watcher(capacity.clone(), Duration::from_secs(1), || Ok(())).await;
        assert!(matches!(saturated, Err(error) if error.to_string().contains("still starting")));
        release.send(())?;
        tokio::time::timeout(Duration::from_secs(2), async {
            while !dropped.load(Ordering::SeqCst) || capacity.available_permits() != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await?;
        start_file_watcher(capacity, Duration::from_secs(1), || Ok(())).await?;
        Ok(())
    }

    #[tokio::test]
    async fn startup_scan_cannot_overwrite_a_later_scan_or_its_failure()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = |root: &str| shepherd_core::project_scan::ProjectScan {
            root: root.into(),
            repositories: 0,
            tree: Vec::new(),
        };
        for fail in [false, true] {
            let cache = Arc::new(Mutex::new(ProjectScanCache::default()));
            let (started, waiting) = oneshot::channel();
            let (release, released) = oneshot::channel();
            let request = tokio::spawn({
                let cache = cache.clone();
                async move {
                    request_project_scan(&cache, true, async move {
                        started
                            .send(())
                            .map_err(|_| BridgeError::new("scan", "test receiver closed"))?;
                        released
                            .await
                            .map_err(|_| BridgeError::new("scan", "test sender closed"))?;
                        if fail {
                            Err(BridgeError::new("scan", "fixture failure"))
                        } else {
                            Ok(fixture("fresh"))
                        }
                    })
                    .await
                }
            });
            waiting.await?;
            let startup = tokio::spawn({
                let cache = cache.clone();
                async move { install_startup_scan(&cache, fixture("stale")).await }
            });
            release.send(()).map_err(|_| "request ended early")?;
            let result = request.await?;
            startup.await?;
            if fail {
                assert!(result.is_err());
                assert!(cache.lock().await.value.is_none());
                assert_eq!(
                    request_project_scan(&cache, false, async { Ok(fixture("retry")) })
                        .await?
                        .root,
                    "retry"
                );
            } else {
                assert_eq!(result?.root, "fresh");
                assert_eq!(
                    cache
                        .lock()
                        .await
                        .value
                        .as_ref()
                        .map(|scan| scan.root.as_str()),
                    Some("fresh")
                );
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn project_scan_reports_an_unavailable_root() -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;
        let missing = directory.join("missing");
        let error = scan_project_root(missing.clone())
            .await
            .err()
            .ok_or("missing root was accepted")?;
        assert_eq!(error.code, "scan");
        assert!(error.message.contains(&missing.display().to_string()));
        std::fs::create_dir(&missing)?;
        assert_eq!(scan_project_root(missing).await?.repositories, 0);
        Ok(())
    }

    #[cfg(all(feature = "live-codex", any(target_os = "linux", target_os = "macos")))]
    #[tokio::test]
    #[ignore]
    async fn live_codex_recovers_after_shell_sigkill() -> Result<(), Box<dyn std::error::Error>> {
        use agent_harness::AgentHarness as _;
        use agent_harness::live_codex::LiveCodexRun;
        use std::os::unix::process::ExitStatusExt as _;

        struct Factory {
            run: LiveCodexRun,
            host: TerminalHost,
        }
        impl HarnessFactory for Factory {
            fn create(&self) -> Result<Box<dyn agent_harness::AgentHarness>, CoreError> {
                Ok(Box::new(self.run.harness("desktop", false)))
            }
            fn launch_for_task(
                &self,
                task: &TaskId,
                agent: Agent,
                cwd: PathBuf,
            ) -> Result<
                (
                    Box<dyn agent_harness::AgentHarness>,
                    agent_harness::SpawnSpec,
                ),
                CoreError,
            > {
                if agent != Agent::Codex {
                    return Err(CoreError::HarnessUnavailable);
                }
                let mut harness = self.run.harness("desktop", false);
                harness.set_process_tracker(self.host.agent_process_tracker(task));
                Ok((Box::new(harness), self.run.spec(cwd, "desktop", None)))
            }
        }

        if let Some(data) = std::env::var_os("SHEPHERD_LIVE_CRASH_DATA") {
            let data = PathBuf::from(data);
            let repository = PathBuf::from(
                std::env::var_os("SHEPHERD_LIVE_CRASH_REPOSITORY").ok_or("missing repository")?,
            );
            let ready = PathBuf::from(
                std::env::var_os("SHEPHERD_LIVE_CRASH_READY").ok_or("missing ready path")?,
            );
            let scenario = std::env::var("SHEPHERD_LIVE_CRASH_SCENARIO")?;
            let run = LiveCodexRun::load(&scenario)
                .await?
                .ok_or("Codex is not installed")?;
            if run.is_replay() {
                return Err("live crash fixture unexpectedly replayed a cache".into());
            }
            let host = TerminalHost::default();
            let state = ShellState::load(
                &data,
                Box::new(Factory {
                    run: run.clone(),
                    host: host.clone(),
                }),
                host,
            )
            .await?;
            state.start_event_pump();
            let projects = state.add_project(repository.display().to_string()).await?;
            let project = projects.projects.first().ok_or("missing project")?;
            let draft = state
                .create_draft(project.id.clone(), "Live crash".into(), Agent::Codex)
                .await?;
            let task_id = draft.task.id;
            state
                .send_prompt(
                    task_id.clone(),
                    "Explain in detail how a compiler works, without running any tools.".into(),
                )
                .await?;
            let process = state
                .core
                .lock()
                .await
                .recorded_processes()
                .await?
                .into_iter()
                .find(|record| record.kind == "agent" && record.owner_id == task_id)
                .ok_or("missing agent process record")?;
            let identity = data.join("live-agent-identity");
            let temporary_identity = identity.with_extension("tmp");
            std::fs::write(
                &temporary_identity,
                format!("{} {}", process.pid, process.started_at),
            )?;
            std::fs::rename(temporary_identity, identity)?;
            let process = tokio::time::timeout(Duration::from_secs(90), async {
                loop {
                    let workspace = state.task_workspace(task_id.clone()).await?;
                    if workspace.task.state.is_live() && workspace.transcript.len() > 1 {
                        let records = state.core.lock().await.recorded_processes().await?;
                        if let Some(record) = records
                            .into_iter()
                            .find(|record| record.kind == "agent" && record.owner_id == task_id)
                        {
                            return Ok::<_, BridgeError>(record);
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            })
            .await??;
            run.finish(None)?;
            let temporary = ready.with_extension("tmp");
            std::fs::write(
                &temporary,
                format!("{} {} {}", task_id, process.pid, process.started_at),
            )?;
            std::fs::rename(temporary, ready)?;
            std::future::pending::<()>().await;
            return Ok(());
        }

        struct Guard {
            identity: PathBuf,
            stopped: bool,
        }
        impl Guard {
            fn stop(&mut self) -> std::io::Result<()> {
                if self.stopped {
                    return Ok(());
                }
                let contents = match std::fs::read_to_string(&self.identity) {
                    Ok(contents) => contents,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                    Err(error) => return Err(error),
                };
                let mut fields = contents.split_whitespace();
                let pid: libc::pid_t = fields
                    .next()
                    .ok_or_else(|| std::io::Error::other("missing fixture pid"))?
                    .parse()
                    .map_err(std::io::Error::other)?;
                let started_at: u64 = fields
                    .next()
                    .ok_or_else(|| std::io::Error::other("missing fixture start time"))?
                    .parse()
                    .map_err(std::io::Error::other)?;
                if pid <= 1 || u32::try_from(pid).ok() == Some(std::process::id()) {
                    return Err(std::io::Error::other("invalid fixture pid"));
                }
                if crate::pty_terminal::process_start_time(pid) == Some(started_at)
                    && unsafe { libc::getpgid(pid) } == pid
                    && unsafe { libc::killpg(pid, libc::SIGKILL) } != 0
                {
                    let error = std::io::Error::last_os_error();
                    if error.raw_os_error() != Some(libc::ESRCH) {
                        return Err(error);
                    }
                }
                self.stopped = true;
                Ok(())
            }
        }
        impl Drop for Guard {
            fn drop(&mut self) {
                if let Err(error) = self.stop() {
                    eprintln!("could not stop live crash fixture: {error}");
                }
            }
        }
        let data = TestDirectory::new()?;
        let repository = TestRepository::new()?;
        let ready = data.join("live-crash-ready");
        let mut guard = Guard {
            identity: data.join("live-agent-identity"),
            stopped: false,
        };
        let scenario = format!("live-crash-{}", uuid::Uuid::new_v4().simple());
        let mut child = tokio::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "bridge::tests::live_codex_recovers_after_shell_sigkill",
                "--ignored",
                "--nocapture",
            ])
            .env("SHEPHERD_LIVE_CRASH_DATA", data.path())
            .env("SHEPHERD_LIVE_CRASH_REPOSITORY", repository.path())
            .env("SHEPHERD_LIVE_CRASH_READY", &ready)
            .env("SHEPHERD_LIVE_CRASH_SCENARIO", &scenario)
            .env("SHEPHERD_DEV_PROJECT_ROOT", repository.path())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let contents = tokio::time::timeout(Duration::from_secs(120), async {
            loop {
                match std::fs::read_to_string(&ready) {
                    Ok(contents) => break Ok::<_, Box<dyn std::error::Error>>(contents),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => break Err(error.into()),
                }
                if let Some(status) = child.try_wait()? {
                    return Err(format!("live crash fixture exited before ready: {status}").into());
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await??;
        let mut fields = contents.split_whitespace();
        let task_id = fields.next().ok_or("missing task id")?.to_owned();
        let pid: libc::pid_t = fields.next().ok_or("missing pid")?.parse()?;
        let started_at: u64 = fields.next().ok_or("missing start time")?.parse()?;
        if pid <= 1 || u32::try_from(pid).ok() == Some(std::process::id()) {
            return Err("invalid fixture pid".into());
        }
        let identity = std::fs::read_to_string(&guard.identity)?;
        assert_eq!(identity, format!("{pid} {started_at}"));
        child.kill().await?;
        assert_eq!(child.wait().await?.signal(), Some(libc::SIGKILL));
        guard.stop()?;

        let resume_scenario = format!("live-crash-resume-{}", uuid::Uuid::new_v4().simple());
        let run = LiveCodexRun::load(&resume_scenario)
            .await?
            .ok_or("Codex is not installed")?;
        if run.is_replay() {
            return Err("live resume unexpectedly replayed a cache".into());
        }
        let host = TerminalHost::default();
        let state = ShellState::load(
            data.path(),
            Box::new(Factory {
                run: run.clone(),
                host: host.clone(),
            }),
            host,
        )
        .await?;
        state.start_event_pump();
        let interrupted = state.task_workspace(task_id.clone()).await?;
        assert_eq!(interrupted.task.state, TaskState::Interrupted);
        let recorded = state
            .core
            .lock()
            .await
            .process_store()
            .load_sessions()
            .await?
            .into_iter()
            .filter(|session| session.task_id.as_str() == task_id)
            .max_by_key(|session| session.started_at)
            .and_then(|session| session.adapter_session_id)
            .ok_or("missing persisted agent session")?;
        let resumed = state.resume_task(task_id.clone()).await?;
        assert_eq!(resumed.task.state, TaskState::Ready);
        assert!(
            resumed.transcript.iter().any(|entry| entry
                .summary()
                .contains(&format!("resumed the previous agent session {recorded}"))),
            "resume did not use the recorded agent session"
        );
        state
            .send_prompt(task_id.clone(), "Reply with exactly: resumed".into())
            .await?;
        let finished = tokio::time::timeout(Duration::from_secs(90), async {
            loop {
                let workspace = state.task_workspace(task_id.clone()).await?;
                if matches!(
                    workspace.task.state,
                    TaskState::Ready | TaskState::Review | TaskState::Failed
                ) {
                    return Ok::<_, BridgeError>(workspace);
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await??;
        assert_ne!(
            finished.task.state,
            TaskState::Failed,
            "{:?}",
            finished.task.error
        );
        assert!(
            finished
                .transcript
                .iter()
                .any(|entry| matches!(&entry.kind, agent_harness::EntryKind::AgentText { text } if text.to_lowercase().contains("resumed")))
        );
        run.finish(None)?;
        state.quit(true).await?;
        Ok(())
    }

    #[tokio::test]
    async fn discovery_uses_project_environment_at_spawn() -> Result<(), Box<dyn std::error::Error>>
    {
        struct Factory;
        impl HarnessFactory for Factory {
            fn create(&self) -> Result<Box<dyn agent_harness::AgentHarness>, CoreError> {
                Err(CoreError::HarnessUnavailable)
            }
            fn environment_for_project(
                &self,
                agent: Agent,
                _project: &Path,
            ) -> Result<BTreeMap<String, String>, CoreError> {
                Ok(BTreeMap::from([(
                    "TOKEN".into(),
                    format!("!cat {}-token", agent.as_str()),
                )]))
            }
        }
        let directory = TestDirectory::new()?;
        std::fs::write(directory.path().join("codex-token"), "codex-secret\n")?;
        std::fs::write(directory.path().join("opencode-token"), "open-secret\n")?;
        let state = ShellState::new(Core::with_services(
            Box::new(MemoryStore::new()),
            Box::new(Factory),
        ));
        let projects = state
            .add_project(directory.path().display().to_string())
            .await?;
        let id = projects
            .projects
            .first()
            .ok_or("missing project")?
            .id
            .clone();
        for (agent, expected) in [
            (Agent::Codex, "codex-secret"),
            (Agent::OpenCode, "open-secret"),
        ] {
            let spec = state
                .discovery_spec(
                    id.clone(),
                    agent,
                    SpawnSpec {
                        environment: BTreeMap::from([("EXISTING".into(), "preserved".into())]),
                        ..SpawnSpec::default()
                    },
                )
                .await?;
            assert_eq!(spec.cwd, directory.path().canonicalize()?);
            assert_eq!(
                spec.environment.get("TOKEN").map(String::as_str),
                Some(expected)
            );
            assert_eq!(
                spec.environment.get("EXISTING").map(String::as_str),
                Some("preserved")
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn quit_retries_terminal_cleanup_after_core_has_closed()
    -> Result<(), Box<dyn std::error::Error>> {
        let state = ShellState::new(Core::new());
        let failure = state
            .quit_with_cleanup(true, || async {
                Err(BridgeError::new("terminal", "fixture cleanup failure"))
            })
            .await;
        assert!(failure.is_err());
        assert!(state.pending_quit.lock().await.is_some());
        let summary = state.quit_with_cleanup(true, || async { Ok(()) }).await?;
        assert_eq!(summary.interrupted_tasks, 0);
        assert!(state.pending_quit.lock().await.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn dropping_shell_releases_the_event_pump_and_core()
    -> Result<(), Box<dyn std::error::Error>> {
        let state = ShellState::new(Core::new());
        let core = Arc::downgrade(&state.core);
        let mut events = state.subscribe();
        state.start_event_pump();
        tokio::task::yield_now().await;
        drop(state);
        let result = tokio::time::timeout(Duration::from_secs(2), events.recv()).await?;
        assert!(matches!(
            result,
            Err(tokio::sync::broadcast::error::RecvError::Closed)
        ));
        assert!(core.upgrade().is_none());
        Ok(())
    }

    #[tokio::test]
    async fn completed_shutdown_stops_the_event_pump() -> Result<(), Box<dyn std::error::Error>> {
        let state = ShellState::new(Core::new());
        state.start_event_pump();
        assert_eq!(state.events.strong_count(), 2);
        state.quit(true).await?;
        tokio::time::timeout(Duration::from_secs(2), async {
            while state.events.strong_count() > 1 {
                tokio::task::yield_now().await;
            }
        })
        .await?;
        assert_eq!(state.events.strong_count(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn agent_events_reach_subscribers_without_a_followup_command()
    -> Result<(), Box<dyn std::error::Error>> {
        use agent_harness::{
            AgentHarness, ApprovalId, Capabilities, DeltaKind, HarnessEvent, HarnessResult, ItemId,
            SessionId, TurnFinishReason,
        };
        use shepherd_core::store::MemoryStore;

        struct Factory {
            receiver: std::sync::Mutex<Option<mpsc::UnboundedReceiver<HarnessEvent>>>,
            sender: mpsc::UnboundedSender<HarnessEvent>,
        }
        impl HarnessFactory for Factory {
            fn create(&self) -> Result<Box<dyn AgentHarness>, CoreError> {
                let events = self
                    .receiver
                    .lock()
                    .map_err(|_| CoreError::HarnessUnavailable)?
                    .take()
                    .ok_or(CoreError::HarnessUnavailable)?;
                Ok(Box::new(DelayedHarness {
                    events,
                    sender: self.sender.clone(),
                }))
            }
        }
        struct DelayedHarness {
            events: mpsc::UnboundedReceiver<HarnessEvent>,
            sender: mpsc::UnboundedSender<HarnessEvent>,
        }
        #[async_trait]
        impl AgentHarness for DelayedHarness {
            fn events(&mut self) -> &mut mpsc::UnboundedReceiver<HarnessEvent> {
                &mut self.events
            }
            async fn start(&mut self, _: SpawnSpec) -> HarnessResult<Capabilities> {
                Ok(Capabilities::default())
            }
            async fn open_session(&mut self, _: Option<SessionId>) -> HarnessResult<SessionId> {
                Ok(SessionId::new("fixture"))
            }
            async fn prompt(&mut self, _: PromptInput) -> HarnessResult<()> {
                for _ in 0..300 {
                    self.sender
                        .send(HarnessEvent::Delta {
                            item_id: ItemId::new("answer"),
                            kind: DeltaKind::Message,
                            text: "p".into(),
                            stream_permit: None,
                        })
                        .map_err(|_| agent_harness::HarnessError::ChannelClosed)?;
                }
                Ok(())
            }
            async fn cancel(&mut self) -> HarnessResult<()> {
                Ok(())
            }
            async fn respond(&mut self, _: ApprovalId, _: Decision) -> HarnessResult<()> {
                Ok(())
            }
            async fn shutdown(&mut self) -> HarnessResult<()> {
                Ok(())
            }
        }

        let repository = TestRepository::new()?;
        let (sender, receiver) = mpsc::unbounded_channel();
        let state = ShellState::new(Core::with_services(
            Box::new(MemoryStore::new()),
            Box::new(Factory {
                receiver: std::sync::Mutex::new(Some(receiver)),
                sender: sender.clone(),
            }),
        ));
        state.start_event_pump();
        let projects = state
            .add_project(repository.path().display().to_string())
            .await?;
        let project = projects.projects.first().ok_or("missing project")?;
        let draft = state
            .execute(Command::CreateDraft {
                project_id: ProjectId::new(project.id.clone()),
                title: "Wakeup".into(),
                agent: shepherd_core::Agent::Codex,
            })
            .await?;
        let CommandResult::TaskWorkspace(draft) = draft else {
            return Err("missing draft".into());
        };
        let task_id = TaskId::new(draft.task.id);
        state
            .execute(Command::SendPrompt {
                task_id: task_id.clone(),
                input: PromptInput::text("hello"),
            })
            .await?;
        let mut events = state.subscribe();
        for _ in 0..600 {
            sender.send(HarnessEvent::Delta {
                item_id: ItemId::new("answer"),
                kind: DeltaKind::Message,
                text: "x".into(),
                stream_permit: None,
            })?;
        }
        sender.send(HarnessEvent::Delta {
            item_id: ItemId::new("answer"),
            kind: DeltaKind::Message,
            text: "delayed answer".into(),
            stream_permit: None,
        })?;
        sender.send(HarnessEvent::TurnFinished {
            reason: TurnFinishReason::Completed,
        })?;
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Event::TaskWorkspaceChanged(workspace) = events.recv().await?
                    && workspace.task.id == task_id.as_str()
                    && workspace.transcript.iter().any(|entry| {
                        entry.summary().contains(&format!(
                            "{}{}delayed answer",
                            "p".repeat(300),
                            "x".repeat(600)
                        ))
                    })
                    && workspace.task.state == TaskState::Ready
                {
                    return Ok::<_, tokio::sync::broadcast::error::RecvError>(());
                }
            }
        })
        .await??;
        Ok(())
    }

    #[tokio::test]
    async fn task_search_can_be_cancelled_before_start_or_while_waiting_for_core()
    -> Result<(), Box<dyn std::error::Error>> {
        let state = ShellState::new(Core::new());
        let id = state.reserve_task_search().await?;
        state.cancel_task_search(&id).await;
        assert_eq!(
            state
                .search_tasks(&id, "preview")
                .await
                .err()
                .map(|error| error.code),
            Some("search_cancelled")
        );

        let id = state.reserve_task_search().await?;
        let core = state.core.lock().await;
        let (result, ()) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(state.search_tasks(&id, "preview"), async {
                loop {
                    if state
                        .task_searches
                        .lock()
                        .await
                        .get(&id)
                        .is_some_and(|request| request.cancellation.is_none())
                    {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                state.cancel_task_search(&id).await;
            })
        })
        .await?;
        drop(core);
        assert_eq!(
            result.err().map(|error| error.code),
            Some("search_cancelled")
        );
        assert!(state.task_searches.lock().await.is_empty());
        Ok(())
    }
    use crate::dev_harness::ScriptedHarnessFactory;

    fn state_with(core: Core) -> ShellState {
        ShellState::new(core)
    }

    #[tokio::test]
    async fn workspace_watch_continues_after_a_notification_error()
    -> Result<(), Box<dyn std::error::Error>> {
        let repository = TestRepository::new()?;
        let mut watcher = WorkspaceWatch::start(repository.path().to_path_buf())?;
        watcher.inject_error("fixture error");
        let (cancel, mut cancelled) = oneshot::channel();
        let (updates, mut received) = mpsc::unbounded_channel();
        let pump = tokio::spawn(async move {
            pump_workspace_watch(&mut watcher, &mut cancelled, |update| {
                updates.send(update).map_err(|error| error.to_string())
            })
            .await;
        });
        let mut saw_error = false;
        let mut saw_refresh = false;
        tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(update) = received.recv().await {
                match update {
                    FileWatchUpdate::Error { message } if message.contains("fixture error") => {
                        saw_error = true;
                        std::fs::write(repository.path().join("after-error.txt"), "fixture")?;
                    }
                    FileWatchUpdate::Changes { files, .. } if saw_error => {
                        let files = serde_json::to_value(files)?;
                        if files.as_array().is_some_and(|files| {
                            files.iter().any(|file| file["path"] == "after-error.txt")
                        }) {
                            saw_refresh = true;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            Ok::<(), Box<dyn std::error::Error>>(())
        })
        .await??;
        assert!(saw_error && saw_refresh);
        cancel.send(()).map_err(|_| "watcher stopped early")?;
        pump.await?;
        Ok(())
    }

    #[tokio::test]
    async fn large_git_read_does_not_block_agent_events() -> Result<(), Box<dyn std::error::Error>>
    {
        let repository = TestRepository::new()?;
        let state = std::sync::Arc::new(state_with(Core::with_services(
            Box::new(MemoryStore::new()),
            Box::new(ScriptedHarnessFactory),
        )));
        let projects = state
            .add_project(repository.path().display().to_string())
            .await?;
        let project_id = projects
            .projects
            .first()
            .ok_or("project not added")?
            .id
            .clone();
        let draft = state
            .create_draft(project_id, "Large diff".into(), Agent::Codex)
            .await?;
        let task_id = draft.task.id;
        let workspace = state.send_prompt(task_id.clone(), "hello".into()).await?;
        let worktree = workspace.task.worktree_path.ok_or("missing worktree")?;
        let content = format!(
            "first marker\n{}last marker\n",
            "large diff line\n".repeat(12_000)
        );
        std::fs::write(std::path::Path::new(&worktree).join("large.txt"), content)?;

        let (started_sender, started_receiver) = tokio::sync::oneshot::channel();
        let (release_sender, release_receiver) = std::sync::mpsc::channel();
        let read = tokio::spawn({
            let state = state.clone();
            let task_id = task_id.clone();
            async move {
                state
                    .git_read(TaskId::new(task_id), move |context| {
                        started_sender.send(()).map_err(|_| CoreError::Io {
                            path: "test Git read".into(),
                            source: std::io::Error::other("start receiver dropped"),
                        })?;
                        release_receiver.recv().map_err(|error| CoreError::Io {
                            path: "test Git read".into(),
                            source: std::io::Error::other(error),
                        })?;
                        context.review_file_diff("large.txt")
                    })
                    .await
            }
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), started_receiver).await??;
        let workspace = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            state.resolve_approval(task_id, "approval-1".into(), Decision::Allow),
        )
        .await??;
        assert!(
            workspace
                .transcript
                .iter()
                .any(|entry| entry.summary().contains("Scripted turn complete."))
        );
        release_sender.send(())?;
        let diff = tokio::time::timeout(std::time::Duration::from_secs(15), read).await???;
        assert!(diff.contains("first marker"));
        assert!(diff.contains("last marker"));
        Ok(())
    }

    #[tokio::test]
    async fn file_search_reservations_are_cancellable_and_used_once()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;
        std::fs::write(directory.join("notes.txt"), "searchable text")?;
        let state = state_with(Core::new());
        let projects = state
            .add_project(directory.path().display().to_string())
            .await?;
        let project_id = &projects.projects.first().ok_or("project not added")?.id;
        let query = || {
            serde_json::from_value(serde_json::json!({
                "query": "notes", "mode": "name", "showDotfiles": false,
            }))
        };
        let cancelled = state.reserve_file_search().await?;
        state.cancel_file_search(&cancelled).await;
        assert!(
            matches!(state.search_workspace_files(&cancelled, project_id.clone(), None, query()?).await,
            Err(error) if error.message.contains("cancelled"))
        );

        let search = state.reserve_file_search().await?;
        let result = state
            .search_workspace_files(&search, project_id.clone(), None, query()?)
            .await?;
        assert_eq!(
            serde_json::to_value(result)?,
            serde_json::json!({
                "matches": [{"path": "notes.txt", "line": null, "excerpt": null}], "incomplete": false,
            })
        );
        assert!(
            state
                .search_workspace_files(&search, project_id.clone(), None, query()?)
                .await
                .is_err()
        );
        assert!(state.file_searches.lock().await.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn project_remote_uses_origin_and_hides_credentials()
    -> Result<(), Box<dyn std::error::Error>> {
        let repository = TestRepository::new()?;
        let core = Core::with_services(
            Box::new(MemoryStore::new()),
            Box::new(ScriptedHarnessFactory),
        );
        let state = state_with(core);
        let projects = state
            .add_project(repository.path().display().to_string())
            .await?;
        let project_id = &projects.projects.first().ok_or("project not added")?.id;
        assert!(state.project_remote(project_id.clone()).await?.is_none());

        for (name, url) in [
            ("mirror", "git@GitLab.com:example/mirror.git/"),
            (
                "origin",
                "https://user:secret@github.com/example/project.git?token=secret",
            ),
        ] {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(repository.path())
                .args(["remote", "add", name, url])
                .status()?;
            assert!(status.success());
            let remote = serde_json::to_value(state.project_remote(project_id.clone()).await?)?;
            let expected = if name == "origin" {
                serde_json::json!({ "label": "example/project", "forge": "github" })
            } else {
                serde_json::json!({ "label": "example/mirror", "forge": "gitlab" })
            };
            assert_eq!(remote, expected);
        }
        Ok(())
    }

    #[test]
    fn identifies_hugging_face_https_and_ssh_remotes() -> Result<(), serde_json::Error> {
        for url in [
            "https://huggingface.co/datasets/example/data.git",
            "git@hf.co:datasets/example/data",
        ] {
            assert_eq!(
                serde_json::to_value(ProjectRemote::from_url(url))?,
                serde_json::json!({ "label": "datasets/example/data", "forge": "huggingface" })
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn commands_return_typed_snapshots_and_broadcast_events()
    -> Result<(), Box<dyn std::error::Error>> {
        let repository = TestRepository::new()?;
        let core = Core::with_services(
            Box::new(MemoryStore::new()),
            Box::new(ScriptedHarnessFactory),
        );
        let state = state_with(core);
        let mut events = state.subscribe();

        assert!(state.project_list().await.projects.is_empty());

        let projects = state
            .add_project(repository.path().display().to_string())
            .await?;
        assert_eq!(projects.projects.len(), 1);
        assert!(projects.projects[0].is_git_repository);
        let project_id = projects.projects[0].id.clone();

        let mut saw_project_list = false;
        for _ in 0..4 {
            if matches!(events.recv().await?, Event::ProjectListChanged(_)) {
                saw_project_list = true;
                break;
            }
        }
        assert!(
            saw_project_list,
            "the project list event should reach the shell"
        );

        let draft = state
            .create_draft(project_id.clone(), "Bridge draft".to_string(), Agent::Codex)
            .await?;
        assert_eq!(draft.task.state, TaskState::Draft);
        assert!(draft.task.worktree_path.is_none());
        let task_id = draft.task.id.clone();

        let tasks = state.task_list(project_id).await?;
        assert_eq!(tasks.tasks.len(), 1);
        assert_eq!(tasks.tasks[0].id, task_id);

        let workspace = state
            .send_prompt(task_id.clone(), "hello".to_string())
            .await?;
        assert_eq!(workspace.task.state, TaskState::Waiting);
        assert_eq!(
            workspace.transcript.first().map(|entry| entry.summary()),
            Some("you: hello".to_string())
        );

        let workspace = state
            .resolve_approval(task_id.clone(), "approval-1".to_string(), Decision::Allow)
            .await?;
        assert_eq!(workspace.task.state, TaskState::Ready);

        let summary: QuitSummary = state.quit(true).await?;
        assert_eq!(summary.interrupted_tasks, 0);
        Ok(())
    }

    #[tokio::test]
    async fn terminal_links_reject_stale_frame_revisions() {
        let core = Core::with_services(
            Box::new(MemoryStore::new()),
            Box::new(ScriptedHarnessFactory),
        );
        let state = state_with(core);
        let screen = Screen::new(20, 2);
        let (frames, _) = watch::channel(Arc::new(screen.frame()));
        let mut slot = ScreenSlot {
            revision: 0,
            screen,
            frames,
            task_id: None,
            last_worktree_touch: None,
        };
        let mut receiver = slot.subscribe();
        let old = receiver.borrow_and_update().revision;
        slot.screen
            .process(b"\x1b]8;;file:///tmp/fixture\x1b\\link\x1b]8;;\x1b\\");
        slot.publish();
        let current = receiver.borrow_and_update().revision;
        state
            .terminals
            .screens
            .lock()
            .await
            .insert("fixture".into(), Arc::new(Mutex::new(slot)));

        assert!(matches!(
            state.open_terminal_link("fixture", 0, 0, old).await,
            Err(error) if error.message == "terminal output changed before the link could open"
        ));
        assert!(matches!(
            state.open_terminal_link("fixture", 0, 0, current).await,
            Err(error) if error.message == "terminal links must use HTTP or HTTPS"
        ));
    }

    #[tokio::test]
    async fn snapshots_serialize_as_tagged_camel_case_json()
    -> Result<(), Box<dyn std::error::Error>> {
        let event = Event::TaskListChanged(TaskList {
            project_id: ProjectId::new("project-1"),
            tasks: vec![TaskSummary {
                id: "task-1".to_string(),
                project_id: "project-1".to_string(),
                title: "Bridge".to_string(),
                state: TaskState::Waiting,
                agent: Agent::OpenCode,
                tier: shepherd_core::AgentTier::Native,
                branch: Some("shepherd/bridge".to_string()),
                worktree_path: None,
                error: None,
                created_at: 1,
            }],
        });
        let value = serde_json::to_value(&event)?;
        assert_eq!(value["type"], "taskListChanged");
        assert_eq!(value["data"]["projectId"], "project-1");
        assert_eq!(
            value["data"]["tasks"][0]["worktreePath"],
            serde_json::Value::Null
        );
        assert_eq!(value["data"]["tasks"][0]["state"], "waiting");
        assert_eq!(value["data"]["tasks"][0]["agent"], "opencode");
        Ok(())
    }

    #[tokio::test]
    async fn an_unknown_approval_is_a_bridge_error() -> Result<(), Box<dyn std::error::Error>> {
        let repository = TestRepository::new()?;
        let core = Core::with_services(
            Box::new(MemoryStore::new()),
            Box::new(ScriptedHarnessFactory),
        );
        let state = state_with(core);
        let projects = state
            .add_project(repository.path().display().to_string())
            .await?;
        let draft = state
            .create_draft(
                projects.projects[0].id.clone(),
                "Approval".to_string(),
                Agent::Codex,
            )
            .await?;
        let task_id = draft.task.id.clone();
        state.send_prompt(task_id.clone(), "go".to_string()).await?;

        let error = state
            .resolve_approval(task_id, "approval-9".to_string(), Decision::Allow)
            .await
            .expect_err("a mismatched approval must fail");
        assert!(error.to_string().contains("approval"));
        Ok(())
    }

    #[tokio::test]
    async fn a_standalone_pty_has_a_screen_but_no_project_terminal_record()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;
        let state = state_with(Core::new());
        let projects = state
            .add_project(directory.path().display().to_string())
            .await?;
        let project = projects.projects.first().ok_or("missing project")?;
        let terminal = state
            .spawn_terminal(
                TerminalSpec {
                    id: TerminalId::new("standalone-fixture"),
                    columns: 80,
                    rows: 24,
                    cwd: directory.path().to_path_buf(),
                    command: "/bin/sh".into(),
                    args: vec![
                        "-c".into(),
                        "printf 'standalone-ready\\n'; read line".into(),
                    ],
                    environment: BTreeMap::new(),
                },
                None,
            )
            .await?;
        tokio::time::timeout(Duration::from_secs(4), async {
            while state
                .search_terminal(
                    terminal.as_str(),
                    "standalone-ready",
                    None,
                    SearchDirection::Previous,
                )
                .await?
                .is_none()
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Ok::<(), BridgeError>(())
        })
        .await??;
        assert!(
            state
                .terminal_list(project.id.clone())
                .await
                .terminals
                .is_empty()
        );
        state.close_terminal(terminal.as_str().to_owned()).await?;
        Ok(())
    }
    /// Spawns through the ACP transport's own command builder, so the launch
    /// token and process group are the ones a real ACP agent would receive.
    #[cfg(unix)]
    #[tokio::test]
    async fn acp_launched_tree_is_stopped_after_its_leader_exits()
    -> Result<(), Box<dyn std::error::Error>> {
        if crate::pty_terminal::boot_id().is_none() {
            eprintln!("Skipping process stop check: the OS boot identifier is unavailable");
            return Ok(());
        }
        let directory = TestDirectory::new()?;
        let members_file = directory.path().join("stoppable-members");
        let token_file = directory.path().join("stoppable-token");
        let token = "acp-launch-token";
        let script = format!(
            "printf %s \"$SHEPHERD_PROCESS_TOKEN\" > '{}'; sleep 30 & echo $! > '{}'; SHEPHERD_PROCESS_TOKEN=other-launch sleep 30 & echo $! >> '{}'; sleep 0.5; exit 0",
            token_file.display(),
            members_file.display(),
            members_file.display()
        );
        let leader_pid =
            acp_client::connection::ProjectAgent::spawn_for_test(acp_client::SpawnSpec {
                program: "/bin/sh".into(),
                args: vec!["-c".into(), script],
                cwd: directory.path().to_path_buf(),
                env: BTreeMap::from([(agent_harness::PROCESS_TOKEN_ENV.into(), token.into())]),
            })?;
        let record = crate::pty_terminal::capture_process_record(
            libc::pid_t::try_from(leader_pid)?,
            "agent",
            "acp-task",
            token,
        )
        .ok_or("could not capture the ACP process")?;
        let members = {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Ok(contents) = std::fs::read_to_string(&members_file) {
                    let pids: Vec<libc::pid_t> = contents
                        .lines()
                        .filter_map(|line| line.trim().parse().ok())
                        .collect();
                    if pids.len() >= 2 {
                        break pids;
                    }
                }
                if Instant::now() >= deadline {
                    return Err("the ACP fixture did not record its members".into());
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        assert_eq!(std::fs::read_to_string(&token_file)?, token);
        let mut unrelated = std::process::Command::new("sleep")
            .arg("30")
            .env(agent_harness::PROCESS_TOKEN_ENV, token)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        let unrelated_pid = libc::pid_t::try_from(unrelated.id())?;
        let mut status = 0;
        let reaped = unsafe { libc::waitpid(libc::pid_t::try_from(leader_pid)?, &mut status, 0) };
        if reaped < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let leases_directory = directory.path().join("leases");
        let leases = crate::orphan_process::ProcessLeases::new(leases_directory.clone())?;
        drop(crate::orphan_process::ProcessLease::create(
            &leases_directory,
            &record.id,
        )?);
        let stopping = record.clone();
        tokio::task::spawn_blocking(move || crate::orphan_process::stop(&leases, &stopping))
            .await??;
        let alive = |pid: libc::pid_t| (unsafe { libc::kill(pid, 0) }) == 0;
        assert!(
            !alive(members[0]),
            "a descendant of the ACP launch should be stopped"
        );
        assert!(alive(members[1]), "a member of another launch must survive");
        assert!(alive(unrelated_pid), "an unrelated process must survive");
        unsafe {
            libc::kill(members[1], libc::SIGKILL);
            libc::kill(unrelated_pid, libc::SIGKILL);
        }
        unrelated.wait()?;
        Ok(())
    }

    #[tokio::test]
    async fn acp_terminal_identity_is_recorded_and_removed_on_exit()
    -> Result<(), Box<dyn std::error::Error>> {
        if crate::pty_terminal::boot_id().is_none() {
            eprintln!("Skipping process identity check: the OS boot identifier is unavailable");
            return Ok(());
        }
        let directory = TestDirectory::new()?;
        let state = state_with(Core::new());
        let (events, mut output) = mpsc::channel(32);
        let core_guard = state.core.lock().await;
        tokio::time::timeout(
            Duration::from_secs(4),
            state.terminals.spawn(
                TerminalLaunch {
                    id: "acp-fixture".into(),
                    cwd: directory.path().to_path_buf(),
                    command: "/bin/sh".into(),
                    args: vec!["-c".into(), "read line".into()],
                    environment: BTreeMap::new(),
                },
                events,
            ),
        )
        .await??;
        drop(core_guard);
        let records = state.core.lock().await.recorded_processes().await?;
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].kind, "acp_terminal");
        assert_eq!(records[0].owner_id, "acp-fixture");
        state
            .terminal_write(
                "acp-fixture".into(),
                TerminalInput::Text("done\n".into()),
                true,
            )
            .await?;
        tokio::time::timeout(Duration::from_secs(4), async {
            while let Some(event) = output.recv().await {
                if matches!(event, TerminalProcessEvent::Exited(_)) {
                    break;
                }
            }
            loop {
                if state
                    .core
                    .lock()
                    .await
                    .recorded_processes()
                    .await?
                    .is_empty()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Ok::<(), CoreError>(())
        })
        .await??;
        Ok(())
    }

    #[tokio::test]
    async fn acp_wait_and_output_follow_the_same_pty_as_the_pane()
    -> Result<(), Box<dyn std::error::Error>> {
        use acp_client::fake_agent::{FakeScript, connect_fake_agent_with_terminals};
        use acp_client::{AcpEvent, ConnectOptions, SessionRoots, ToolPermissions};

        async fn next_terminal(
            events: &mut mpsc::UnboundedReceiver<AcpEvent>,
        ) -> Result<String, Box<dyn std::error::Error>> {
            Ok(tokio::time::timeout(Duration::from_secs(4), async {
                while let Some(event) = events.recv().await {
                    if let AcpEvent::TerminalStarted { terminal_id, .. } = event {
                        return Ok::<_, &str>(terminal_id);
                    }
                }
                Err("ACP terminal never started")
            })
            .await??)
        }

        let directory = TestDirectory::new()?;
        let state = state_with(Core::new());
        let cwd = directory.path().canonicalize()?;
        let (mut connection, observed) = connect_fake_agent_with_terminals(
            FakeScript {
                terminal: Some(("/bin/sh".into(), vec![
                    "-c".into(),
                    "test -t 0 && test -t 1 || exit 91; printf 'acp-ready\n'; IFS= read -r line; stty size; printf 'received=%s\n' \"$line\"; exit 7".into(),
                ])),
                release_terminal: true,
                ..FakeScript::default()
            },
            ConnectOptions {
                roots: SessionRoots::new([cwd.clone()]),
                tool_permissions: ToolPermissions::default(),
            },
            Arc::new(state.terminals.clone()),
        ).await?;
        let (session, _, _) = connection
            .requester
            .new_session(cwd, Vec::new(), Vec::new())
            .await?;
        let mut events = connection.take_events();
        let prompt = tokio::spawn({
            let requester = connection.requester.clone();
            let session = session.clone();
            async move { requester.prompt(&session, "start the fixture".into()).await }
        });
        let terminal_id = next_terminal(&mut events).await?;
        tokio::time::timeout(Duration::from_secs(4), async {
            while state
                .search_terminal(&terminal_id, "acp-ready", None, SearchDirection::Previous)
                .await?
                .is_none()
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Ok::<(), BridgeError>(())
        })
        .await??;
        state.terminal_resize(terminal_id.clone(), 93, 32).await?;
        state
            .terminal_write(
                terminal_id.clone(),
                TerminalInput::Text("from-the-pane\n".into()),
                true,
            )
            .await?;
        tokio::time::timeout(Duration::from_secs(4), prompt).await???;
        let mut released = false;
        while let Ok(event) = events.try_recv() {
            if let AcpEvent::TerminalReleased {
                terminal_id: released_id,
                ..
            } = event
            {
                released |= released_id == terminal_id;
            }
        }
        assert!(released);
        assert!(state.screen_slot(&terminal_id).await.is_err());
        {
            let observed = observed.lock().map_err(|_| "fake observations poisoned")?;
            let output = observed
                .terminal_output
                .first()
                .ok_or("ACP terminal output was empty")?;
            assert!(output.contains("32 93"), "{output:?}");
            assert!(output.contains("received=from-the-pane"), "{output:?}");
            assert_eq!(observed.terminal_exit, [Some(7)]);
        }

        let requester = connection.requester.clone();
        let prompt = tokio::spawn(async move {
            requester
                .prompt(&session, "leave the terminal open".into())
                .await
        });
        let terminal_id = next_terminal(&mut events).await?;
        connection.close();
        tokio::time::timeout(Duration::from_secs(4), async {
            while state.screen_slot(&terminal_id).await.is_ok() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await?;
        assert!(
            tokio::time::timeout(Duration::from_secs(4), prompt)
                .await??
                .is_err()
        );
        Ok(())
    }

    #[tokio::test]
    async fn archived_task_terminal_activity_refreshes_worktree_usage()
    -> Result<(), Box<dyn std::error::Error>> {
        use shepherd_core::store::Store;
        use shepherd_core::tasks::Task;

        let directory = TestDirectory::new()?;
        let repository = TestRepository::new()?;
        let state = ShellState::load(
            directory.path(),
            Box::new(ScriptedHarnessFactory),
            TerminalHost::default(),
        )
        .await?;
        let projects = state
            .add_project(repository.path().display().to_string())
            .await?;
        let project_id = projects
            .projects
            .first()
            .ok_or("missing project")?
            .id
            .clone();
        let mut task = Task::draft(
            ProjectId::new(project_id.clone()),
            "Archived task",
            &repository.repository().git_dir,
        );
        GitService.create_worktree(
            repository.repository(),
            &task.proposed_worktree_path,
            &task.proposed_branch,
            "main",
        )?;
        task.branch = Some(task.proposed_branch.clone());
        task.worktree_path = Some(task.proposed_worktree_path.clone());
        task.state = TaskState::Done;
        task.updated_at = now_millis().saturating_sub(29 * 24 * 60 * 60 * 1000);
        task.worktree_last_used_at = task.updated_at;
        let store = TursoStore::open_at(&directory.path().join("shepherd.db")).await?;
        store.save_task(&task).await?;
        drop(state);

        let state = ShellState::load(
            directory.path(),
            Box::new(ScriptedHarnessFactory),
            TerminalHost::default(),
        )
        .await?;
        let agent_terminal = state.terminals.for_task(task.id.clone());
        let (events, mut output) = mpsc::channel(32);
        agent_terminal
            .spawn(
                TerminalLaunch {
                    id: "archived-agent-terminal".into(),
                    cwd: task.proposed_worktree_path.clone(),
                    command: "/bin/sh".into(),
                    args: vec!["-c".into(), "printf ready; sleep 30".into()],
                    environment: BTreeMap::new(),
                },
                events,
            )
            .await
            .map_err(std::io::Error::other)?;
        tokio::time::timeout(Duration::from_secs(4), async {
            while let Some(event) = output.recv().await {
                if matches!(event, TerminalProcessEvent::Output(_)) {
                    return Ok::<_, std::io::Error>(());
                }
            }
            Err(std::io::Error::other("agent terminal ended before output"))
        })
        .await??;
        let after_agent = tokio::time::timeout(Duration::from_secs(4), async {
            loop {
                let saved = store
                    .load_tasks()
                    .await?
                    .into_iter()
                    .next()
                    .ok_or("missing task")?;
                if saved.worktree_last_used_at > task.worktree_last_used_at {
                    return Ok::<_, Box<dyn std::error::Error>>(saved.worktree_last_used_at);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await??;
        assert_eq!(
            state
                .screen_slot("archived-agent-terminal")
                .await?
                .lock()
                .await
                .task_id
                .as_ref(),
            Some(&task.id)
        );
        agent_terminal
            .stop("archived-agent-terminal")
            .await
            .map_err(std::io::Error::other)?;
        assert_eq!(
            state
                .files_directory(project_id.clone(), Some(task.id.to_string()))
                .await?,
            task.proposed_worktree_path
        );
        let after_file_access = store
            .load_tasks()
            .await?
            .first()
            .ok_or("missing task")?
            .worktree_last_used_at;
        assert!(after_file_access > task.worktree_last_used_at);
        assert_eq!(after_file_access, after_agent);
        state
            .files_directory(project_id.clone(), Some(task.id.to_string()))
            .await?;
        assert_eq!(
            store
                .load_tasks()
                .await?
                .first()
                .ok_or("missing task")?
                .worktree_last_used_at,
            after_file_access
        );
        let terminals = state
            .start_terminal(project_id, Some(task.id.to_string()), 80, 24)
            .await?;
        let terminal_id = terminals
            .terminals
            .first()
            .ok_or("missing terminal")?
            .id
            .clone();
        let after_launch = store
            .load_tasks()
            .await?
            .first()
            .ok_or("missing task")?
            .worktree_last_used_at;
        assert!(after_launch > task.worktree_last_used_at);
        tokio::time::sleep(Duration::from_millis(20)).await;
        state
            .terminal_write(terminal_id.clone(), TerminalInput::Text(" ".into()), true)
            .await?;
        let after_input = store
            .load_tasks()
            .await?
            .first()
            .ok_or("missing task")?
            .worktree_last_used_at;
        assert!(after_input > after_launch);
        tokio::time::sleep(Duration::from_millis(20)).await;
        state
            .terminal_write(terminal_id.clone(), TerminalInput::Text(" ".into()), true)
            .await?;
        assert_eq!(
            store
                .load_tasks()
                .await?
                .first()
                .ok_or("missing task")?
                .worktree_last_used_at,
            after_input
        );
        assert_eq!(
            store
                .load_tasks()
                .await?
                .first()
                .ok_or("missing task")?
                .updated_at,
            task.updated_at
        );
        state.close_terminal(terminal_id.clone()).await?;
        tokio::time::timeout(Duration::from_secs(4), async {
            loop {
                if state
                    .core
                    .lock()
                    .await
                    .terminal_record(&TerminalId::new(&terminal_id))?
                    .ended_at
                    .is_some()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Ok::<(), CoreError>(())
        })
        .await??;
        Ok(())
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[tokio::test]
    async fn agent_record_is_retained_when_group_inspection_fails()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;
        let store = Arc::new(TursoStore::open_at(&directory.path().join("shepherd.db")).await?);
        let slot = Arc::new(OnceLock::new());
        slot.set(store.clone() as Arc<dyn Store>)
            .map_err(|_| "store already set")?;
        let tracker = AgentProcessStore {
            store: slot,
            task_id: "task-fixture".into(),
            token: String::new(),
            leases: Some(Arc::new(crate::orphan_process::ProcessLeases::new(
                directory.path().join("leases"),
            )?)),
        };
        let pid = libc::pid_t::try_from(std::process::id())?;
        let mut record =
            crate::pty_terminal::capture_process_record(pid, "agent", "task-fixture", "")
                .ok_or("identity unavailable")?;
        record.process_group = 1;
        store.save_process(&record).await?;
        let error = agent_harness::AgentProcessTracker::stopped(&tracker, &record.id)
            .await
            .expect_err("invalid group must fail inspection");
        assert!(error.contains("could not inspect agent process group"));
        assert_eq!(store.load_processes().await?, vec![record]);
        Ok(())
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[tokio::test]
    async fn agent_record_remains_when_a_group_member_outlives_the_leader()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;
        let store = Arc::new(TursoStore::open_at(&directory.path().join("shepherd.db")).await?);
        let slot = Arc::new(OnceLock::new());
        slot.set(store.clone() as Arc<dyn Store>)
            .map_err(|_| "store already set")?;
        let tracker = AgentProcessStore {
            store: slot,
            task_id: "task-fixture".into(),
            token: String::new(),
            leases: Some(Arc::new(crate::orphan_process::ProcessLeases::new(
                directory.path().join("leases"),
            )?)),
        };
        let ready = directory.path().join("child-pid");
        let release = directory.path().join("release");
        let mut command = tokio::process::Command::new("/bin/sh");
        command
            .args([
                "-c",
                "sleep 30 & echo $! > \"$1\"; while [ ! -e \"$2\" ]; do sleep 0.02; done",
                "fixture",
            ])
            .arg(&ready)
            .arg(&release)
            .process_group(0)
            .kill_on_drop(true);
        let mut leader = command.spawn()?;
        let id = agent_harness::AgentProcessTracker::started(
            &tracker,
            leader.id().ok_or("missing leader PID")?,
        )
        .await?;
        let contents = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                match std::fs::read_to_string(&ready) {
                    Ok(contents) => break Ok::<_, std::io::Error>(contents),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        tokio::time::sleep(Duration::from_millis(10)).await
                    }
                    Err(error) => break Err(error),
                }
            }
        })
        .await??;
        let child_pid: libc::pid_t = contents.trim().parse()?;
        let started_at =
            crate::pty_terminal::process_start_time(child_pid).ok_or("missing child start time")?;
        struct Guard {
            pid: libc::pid_t,
            started_at: u64,
        }
        impl Drop for Guard {
            fn drop(&mut self) {
                if crate::pty_terminal::process_start_time(self.pid) == Some(self.started_at)
                    && unsafe { libc::kill(self.pid, libc::SIGKILL) } != 0
                {
                    let error = std::io::Error::last_os_error();
                    if error.raw_os_error() != Some(libc::ESRCH) {
                        eprintln!("could not stop group fixture: {error}");
                    }
                }
            }
        }
        let guard = Guard {
            pid: child_pid,
            started_at,
        };
        std::fs::write(release, "release")?;
        assert!(leader.wait().await?.success());
        agent_harness::AgentProcessTracker::stopped(&tracker, &id).await?;
        let records = store.load_processes().await?;
        assert_eq!(records.len(), 1);
        assert!(
            crate::orphan_process::ProcessLease::recover(&directory.path().join("leases"), &id)
                .is_err()
        );
        let inspection = serde_json::to_value(crate::orphan_process::inspect(records))?;
        assert_eq!(
            inspection
                .get(0)
                .and_then(|item| item.get("status"))
                .and_then(serde_json::Value::as_str),
            Some("groupUnverified")
        );
        drop(guard);
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let records = store.load_processes().await?;
                let record = records.first().ok_or("missing record")?;
                if !crate::pty_terminal::process_group_has_other_members(record)? {
                    break Ok::<(), Box<dyn std::error::Error>>(());
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await??;
        agent_harness::AgentProcessTracker::stopped(&tracker, &id).await?;
        assert!(store.load_processes().await?.is_empty());
        let recovered =
            crate::orphan_process::ProcessLease::recover(&directory.path().join("leases"), &id)?;
        drop(recovered);
        Ok(())
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[tokio::test]
    async fn agent_process_tracker_persists_and_removes_identity()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;
        let database = directory.path().join("shepherd.db");
        let store = Arc::new(TursoStore::open_at(&database).await?);
        let slot = Arc::new(OnceLock::new());
        slot.set(store.clone() as Arc<dyn Store>)
            .map_err(|_| "store already set")?;
        let tracker = Arc::new(AgentProcessStore {
            store: slot,
            task_id: "task-fixture".into(),
            token: String::new(),
            leases: Some(Arc::new(crate::orphan_process::ProcessLeases::new(
                directory.path().join("leases"),
            )?)),
        });
        use agent_harness::AgentHarness;
        let mut harness = agent_harness::codex::CodexHarness::new();
        harness.set_process_tracker(tracker.clone());
        harness.start(agent_harness::SpawnSpec {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "while IFS= read -r line; do case \"$line\" in *initialize*) printf '{\"id\":1,\"result\":{}}\\n';; esac; done".into()],
            cwd: directory.path().to_path_buf(),
            ..Default::default()
        }).await?;
        let records = TursoStore::open_at(&database)
            .await?
            .load_processes()
            .await?;
        assert_eq!(records.len(), 1);
        assert!(records[0].pid > 1);
        assert_eq!(records[0].kind, "agent");
        assert_eq!(records[0].owner_id, "task-fixture");
        harness.shutdown().await?;
        assert!(store.load_processes().await?.is_empty());
        let mut harness = agent_harness::acp::AcpHarness::default();
        harness.set_process_tracker(tracker.clone());
        harness.start(agent_harness::SpawnSpec {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), r#"while IFS= read -r request; do case "$request" in *'"method":"initialize"'*) id=$(printf '%s\n' "$request" | sed -nE 's/.*"id":("[^"]+"|[0-9]+).*/\1/p'); printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":1}}\n' "$id";; esac; done"#.into()],
            cwd: directory.path().to_path_buf(),
            ..Default::default()
        }).await?;
        let records = TursoStore::open_at(&database)
            .await?
            .load_processes()
            .await?;
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].kind, "agent");
        assert_eq!(records[0].owner_id, "task-fixture");
        harness.shutdown().await?;
        assert!(store.load_processes().await?.is_empty());
        let mut harness = agent_harness::codex::ExecHarness::new();
        harness.set_process_tracker(tracker);
        use std::os::unix::fs::PermissionsExt as _;
        let launcher = directory.path().join("exec-fixture");
        std::fs::write(&launcher, "#!/bin/sh\nexec sleep 30\n")?;
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o700))?;
        harness
            .start(agent_harness::SpawnSpec {
                program: launcher.to_string_lossy().into_owned(),
                cwd: directory.path().to_path_buf(),
                ..Default::default()
            })
            .await?;
        harness.open_session(None).await?;
        harness
            .prompt(agent_harness::PromptInput::text("fixture"))
            .await?;
        let records = TursoStore::open_at(&database)
            .await?
            .load_processes()
            .await?;
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].kind, "agent");
        assert_eq!(records[0].owner_id, "task-fixture");
        let record = records.first().ok_or("missing exec process record")?;
        assert_eq!(
            unsafe { libc::kill(libc::pid_t::try_from(record.pid)?, 0) },
            0
        );
        harness.shutdown().await?;
        assert!(store.load_processes().await?.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn terminal_process_identity_is_durable_until_exit()
    -> Result<(), Box<dyn std::error::Error>> {
        if crate::pty_terminal::boot_id().is_none() {
            eprintln!("Skipping process identity check: the OS boot identifier is unavailable");
            return Ok(());
        }
        let directory = TestDirectory::new()?;
        let repository = TestRepository::new()?;
        let state = ShellState::load(
            directory.path(),
            Box::new(ScriptedHarnessFactory),
            TerminalHost::default(),
        )
        .await?;
        let projects = state
            .add_project(repository.path().display().to_string())
            .await?;
        let project = projects.projects.first().ok_or("missing project")?;
        let list = state
            .start_terminal(project.id.clone(), None, 80, 24)
            .await?;
        let terminal = list.terminals.first().ok_or("missing terminal")?;
        let recorded = state.core.lock().await.recorded_processes().await?;
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].owner_id, terminal.id);
        assert!(recorded[0].pid > 1);
        assert!(recorded[0].process_group > 1);
        assert!(recorded[0].session_id > 1);
        assert!(!recorded[0].boot_id.is_empty());
        let path = directory.path().join("shepherd.db");
        let reopened = TursoStore::open_at(&path).await?;
        use shepherd_core::store::Store;
        assert_eq!(reopened.load_processes().await?, recorded);
        state.close_terminal(terminal.id.clone()).await?;
        tokio::time::timeout(Duration::from_secs(4), async {
            loop {
                if state
                    .core
                    .lock()
                    .await
                    .recorded_processes()
                    .await?
                    .is_empty()
                {
                    return Ok::<(), CoreError>(());
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await??;
        state.quit(true).await?;
        Ok(())
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    async fn abrupt_shell_death_leaves_a_verifiable_agent_record()
    -> Result<(), Box<dyn std::error::Error>> {
        use agent_harness::AgentHarness;
        if let Some(data) = std::env::var_os("SHEPHERD_AGENT_ORPHAN_DATA") {
            let data = PathBuf::from(data);
            let ready = data.join("agent-ready");
            let store = Arc::new(TursoStore::open_at(&data.join("shepherd.db")).await?);
            let slot = Arc::new(OnceLock::new());
            slot.set(store.clone() as Arc<dyn Store>)
                .map_err(|_| "store already set")?;
            let tracker = Arc::new(AgentProcessStore {
                store: slot,
                task_id: "crash-task".into(),
                token: String::new(),
                leases: Some(Arc::new(crate::orphan_process::ProcessLeases::new(
                    data.join("leases"),
                )?)),
            });
            let kind = std::env::var("SHEPHERD_AGENT_ORPHAN_KIND")?;
            let (mut harness, script, prompt): (Box<dyn AgentHarness>, &str, bool) = match kind
                .as_str()
            {
                "app-server" => (
                    Box::new(agent_harness::codex::CodexHarness::new()),
                    "while IFS= read -r line; do case \"$line\" in *initialize*) printf '{\"id\":1,\"result\":{}}\\n';; esac; done; exec sleep 30",
                    false,
                ),
                "exec" => (
                    Box::new(agent_harness::codex::ExecHarness::new()),
                    "exec sleep 30",
                    true,
                ),
                "acp" => (
                    Box::new(agent_harness::acp::AcpHarness::default()),
                    r#"while IFS= read -r request; do case "$request" in *'"method":"initialize"'*) id=$(printf '%s\n' "$request" | sed -nE 's/.*"id":("[^"]+"|[0-9]+).*/\1/p'); printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":1}}\n' "$id";; esac; done; exec sleep 30"#,
                    false,
                ),
                _ => return Err("unknown agent fixture".into()),
            };
            harness.set_process_tracker(tracker);
            let (program, args) = if prompt {
                use std::os::unix::fs::PermissionsExt as _;
                let launcher = data.join("exec-fixture");
                std::fs::write(&launcher, "#!/bin/sh\nexec sleep 30\n")?;
                std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o700))?;
                (launcher.to_string_lossy().into_owned(), Vec::new())
            } else {
                ("/bin/sh".into(), vec!["-c".into(), script.into()])
            };
            harness
                .start(agent_harness::SpawnSpec {
                    program,
                    args,
                    cwd: data.clone(),
                    ..Default::default()
                })
                .await?;
            if prompt {
                harness.open_session(None).await?;
                harness
                    .prompt(agent_harness::PromptInput::text("fixture"))
                    .await?;
            }
            let record = store
                .load_processes()
                .await?
                .into_iter()
                .next()
                .ok_or("missing agent record")?;
            let temporary = ready.with_extension("tmp");
            std::fs::write(
                &temporary,
                format!("{} {} {}", record.pid, record.started_at, record.id),
            )?;
            std::fs::rename(temporary, ready)?;
            std::future::pending::<()>().await;
            return Ok(());
        }
        struct ProcessGuard {
            pid: libc::pid_t,
            started_at: u64,
        }
        impl Drop for ProcessGuard {
            fn drop(&mut self) {
                if crate::pty_terminal::process_start_time(self.pid) == Some(self.started_at)
                    && unsafe { libc::kill(self.pid, libc::SIGKILL) } != 0
                {
                    let error = std::io::Error::last_os_error();
                    if error.raw_os_error() != Some(libc::ESRCH) {
                        eprintln!("could not stop agent fixture: {error}");
                    }
                }
            }
        }
        if crate::pty_terminal::boot_id().is_none() {
            return Ok(());
        }
        for kind in ["app-server", "exec", "acp"] {
            let data = TestDirectory::new()?;
            let ready = data.join("agent-ready");
            let mut child = tokio::process::Command::new(std::env::current_exe()?)
                .args([
                    "--exact",
                    "bridge::tests::abrupt_shell_death_leaves_a_verifiable_agent_record",
                    "--nocapture",
                ])
                .env("SHEPHERD_AGENT_ORPHAN_DATA", data.path())
                .env("SHEPHERD_AGENT_ORPHAN_KIND", kind)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .kill_on_drop(true)
                .spawn()?;
            let contents = tokio::time::timeout(Duration::from_secs(20), async {
                loop {
                    match std::fs::read_to_string(&ready) {
                        Ok(contents) => break Ok::<_, Box<dyn std::error::Error>>(contents),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => break Err(error.into()),
                    }
                    if let Some(status) = child.try_wait()? {
                        break Err(format!("fixture exited before ready: {status}").into());
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await??;
            let mut fields = contents.split_whitespace();
            let pid: libc::pid_t = fields.next().ok_or("missing pid")?.parse()?;
            let started_at = fields.next().ok_or("missing start time")?.parse()?;
            let record_id = fields.next().ok_or("missing record ID")?;
            if pid <= 1 || u32::try_from(pid).ok() == Some(std::process::id()) {
                return Err("invalid fixture PID".into());
            }
            let _guard = ProcessGuard { pid, started_at };
            assert!(
                crate::orphan_process::ProcessLease::recover(&data.join("leases"), record_id)
                    .is_err()
            );
            child.kill().await?;
            use std::os::unix::process::ExitStatusExt as _;
            assert_eq!(child.wait().await?.signal(), Some(libc::SIGKILL));
            let recovered =
                crate::orphan_process::ProcessLease::recover(&data.join("leases"), record_id)?;
            drop(recovered);
            let store = TursoStore::open_at(&data.join("shepherd.db")).await?;
            let records = store.load_processes().await?;
            assert_eq!(records.len(), 1);
            let record = records.first().ok_or("missing agent record")?;
            assert_eq!(record.kind, "agent");
            assert_eq!(record.owner_id, "crash-task");
            assert_eq!(record.pid, u32::try_from(pid)?);
            let inspection = serde_json::to_value(crate::orphan_process::inspect(records))?;
            assert_eq!(
                inspection
                    .get(0)
                    .and_then(|item| item.get("status"))
                    .and_then(serde_json::Value::as_str),
                Some("matching"),
                "fixture: {kind}"
            );
        }
        Ok(())
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    async fn abrupt_shell_death_leaves_a_verifiable_terminal_record()
    -> Result<(), Box<dyn std::error::Error>> {
        if let Some(data) = std::env::var_os("SHEPHERD_ORPHAN_FIXTURE_DATA") {
            let data = PathBuf::from(data);
            let repository = PathBuf::from(
                std::env::var_os("SHEPHERD_ORPHAN_FIXTURE_REPOSITORY")
                    .ok_or("missing repository")?,
            );
            let ready = PathBuf::from(
                std::env::var_os("SHEPHERD_ORPHAN_FIXTURE_READY").ok_or("missing ready path")?,
            );
            let terminal_ready = data.join("terminal-ready");
            let state = ShellState::load(
                &data,
                Box::new(ScriptedHarnessFactory),
                TerminalHost::default(),
            )
            .await?;
            let projects = state.add_project(repository.display().to_string()).await?;
            let project = projects.projects.first().ok_or("missing project")?;
            let id = TerminalId::new("crash-terminal");
            let record = TerminalRecord::project_shared(
                id.clone(),
                ProjectId::new(project.id.clone()),
                "Crash terminal",
                repository.clone(),
                "/bin/sh",
                1,
            );
            state
                .spawn_terminal(
                    TerminalSpec {
                        id,
                        columns: 80,
                        rows: 24,
                        cwd: repository,
                        command: "/bin/sh".into(),
                        args: vec![
                            "-c".into(),
                            "trap '' HUP; printf x > \"$1\"; exec sleep 30".into(),
                            "sh".into(),
                            terminal_ready.display().to_string(),
                        ],
                        environment: BTreeMap::new(),
                    },
                    Some(record),
                )
                .await?;
            tokio::time::timeout(Duration::from_secs(5), async {
                while !terminal_ready.exists() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await?;
            let process = state
                .core
                .lock()
                .await
                .recorded_processes()
                .await?
                .into_iter()
                .next()
                .ok_or("missing process record")?;
            let temporary = ready.with_extension("tmp");
            std::fs::write(
                &temporary,
                format!("{} {}", process.pid, process.started_at),
            )?;
            std::fs::rename(temporary, ready)?;
            std::future::pending::<()>().await;
            return Ok(());
        }

        struct ProcessGuard {
            pid: libc::pid_t,
            started_at: u64,
            stopped: bool,
        }
        impl ProcessGuard {
            fn stop(&mut self) -> std::io::Result<()> {
                if !self.stopped
                    && crate::pty_terminal::process_start_time(self.pid) == Some(self.started_at)
                    && unsafe { libc::kill(self.pid, libc::SIGKILL) } != 0
                {
                    let error = std::io::Error::last_os_error();
                    if error.raw_os_error() != Some(libc::ESRCH) {
                        return Err(error);
                    }
                }
                self.stopped = true;
                Ok(())
            }
        }
        impl Drop for ProcessGuard {
            fn drop(&mut self) {
                if let Err(error) = self.stop() {
                    eprintln!("could not stop crash fixture process: {error}");
                }
            }
        }

        if crate::pty_terminal::boot_id().is_none() {
            return Ok(());
        }
        let data = TestDirectory::new()?;
        let repository = TestRepository::new()?;
        let ready = data.join("crash-ready");
        let mut child = tokio::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "bridge::tests::abrupt_shell_death_leaves_a_verifiable_terminal_record",
                "--nocapture",
            ])
            .env("SHEPHERD_ORPHAN_FIXTURE_DATA", data.path())
            .env("SHEPHERD_ORPHAN_FIXTURE_REPOSITORY", repository.path())
            .env("SHEPHERD_ORPHAN_FIXTURE_READY", &ready)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let contents = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                match std::fs::read_to_string(&ready) {
                    Ok(contents) => break Ok::<_, Box<dyn std::error::Error>>(contents),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => break Err(error.into()),
                }
                if let Some(status) = child.try_wait()? {
                    break Err(format!("fixture exited before ready: {status}").into());
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await??;
        let mut fields = contents.split_whitespace();
        let pid: libc::pid_t = fields.next().ok_or("missing pid")?.parse()?;
        let started_at = fields.next().ok_or("missing start time")?.parse()?;
        if pid <= 1 || u32::try_from(pid).ok() == Some(std::process::id()) {
            return Err("invalid crash fixture process id".into());
        }
        let mut guard = ProcessGuard {
            pid,
            started_at,
            stopped: false,
        };
        child.kill().await?;
        use std::os::unix::process::ExitStatusExt as _;
        assert_eq!(child.wait().await?.signal(), Some(libc::SIGKILL));
        let reopened = ShellState::load(
            data.path(),
            Box::new(ScriptedHarnessFactory),
            TerminalHost::default(),
        )
        .await?;
        let records = reopened.previous_processes.lock().await.clone();
        assert_eq!(records.len(), 1);
        assert_eq!(
            records.first().map(|record| record.pid),
            u32::try_from(pid).ok()
        );
        let inspection = serde_json::to_value(crate::orphan_process::inspect(records))?;
        assert_eq!(
            inspection
                .get(0)
                .and_then(|item| item.get("status"))
                .and_then(serde_json::Value::as_str),
            Some("matching")
        );
        guard.stop()?;
        Ok(())
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    async fn terminal_exit_records_a_surviving_background_job()
    -> Result<(), Box<dyn std::error::Error>> {
        if crate::pty_terminal::boot_id().is_none() {
            eprintln!("Skipping process identity check: the OS boot identifier is unavailable");
            return Ok(());
        }
        let directory = TestDirectory::new()?;
        let repository = TestRepository::new()?;
        let state = ShellState::load(
            directory.path(),
            Box::new(ScriptedHarnessFactory),
            TerminalHost::default(),
        )
        .await?;
        let projects = state
            .add_project(repository.path().display().to_string())
            .await?;
        let project = projects.projects.first().ok_or("missing project")?;
        let id = TerminalId::new("background-record-test");
        let record = TerminalRecord::project_shared(
            id.clone(),
            ProjectId::new(project.id.clone()),
            "Background",
            repository.path().to_path_buf(),
            "/bin/sh",
            1,
        );
        state.spawn_terminal(TerminalSpec {
            id: id.clone(), columns: 80, rows: 24, cwd: repository.path().to_path_buf(), command: "/bin/sh".into(),
            args: vec!["-c".into(), "python3 -c 'import os,signal,time; os.setpgid(0,0); signal.signal(signal.SIGHUP,signal.SIG_IGN); time.sleep(10)' & sleep 1; exit".into()],
            environment: BTreeMap::new(),
        }, Some(record)).await?;
        let leader = state
            .core
            .lock()
            .await
            .recorded_processes()
            .await?
            .into_iter()
            .next()
            .ok_or("missing leader")?;
        let remaining = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let mut core = state.core.lock().await;
                let terminal = core.terminal_record(&id)?;
                let processes = core.recorded_processes().await?;
                if terminal.ended_at.is_some()
                    && processes.iter().any(|process| process.pid != leader.pid)
                {
                    return Ok::<_, CoreError>(processes);
                }
                drop(core);
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await??;
        assert!(
            remaining
                .iter()
                .all(|process| process.owner_id == id.as_str())
        );
        assert!(remaining.iter().all(|process| process.pid != leader.pid));
        state.quit(true).await?;
        Ok(())
    }

    #[tokio::test]
    async fn discarding_draft_stops_only_its_terminal() -> Result<(), Box<dyn std::error::Error>> {
        let repository = TestRepository::new()?;
        let state = state_with(Core::with_services(
            Box::new(MemoryStore::new()),
            Box::new(ScriptedHarnessFactory),
        ));
        let projects = state
            .add_project(repository.path().display().to_string())
            .await?;
        let project = projects.projects.first().ok_or("missing project")?;
        let draft = state
            .create_draft(project.id.clone(), "Draft".into(), Agent::Codex)
            .await?;
        let task = draft.task.id;
        let shared = TerminalRecord::project_shared(
            TerminalId::new("shared-draft-test"),
            ProjectId::new(project.id.clone()),
            "Shared",
            repository.path().to_path_buf(),
            "/bin/sh",
            1,
        );
        let scoped = TerminalRecord::project_shared(
            TerminalId::new("scoped-draft-test"),
            ProjectId::new(project.id.clone()),
            "Draft",
            repository.path().to_path_buf(),
            "/bin/sh",
            2,
        )
        .for_task(TaskId::new(task.clone()));
        state.launch_terminal(shared.clone(), 80, 24).await?;
        state.launch_terminal(scoped.clone(), 80, 24).await?;
        state.discard_draft(task).await?;
        let remaining = state.terminal_list(project.id.clone()).await;
        assert_eq!(remaining.terminals.len(), 1);
        assert_eq!(remaining.terminals[0].id, shared.id.as_str());
        tokio::time::timeout(Duration::from_secs(4), async {
            while state.screen_slot(scoped.id.as_str()).await.is_ok() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await?;
        assert!(state.screen_slot(shared.id.as_str()).await.is_ok());
        state.quit(true).await?;
        Ok(())
    }

    #[tokio::test]
    async fn restarting_a_task_terminal_reuses_its_record_and_worktree()
    -> Result<(), Box<dyn std::error::Error>> {
        let repository = TestRepository::new()?;
        let core = Core::with_services(
            Box::new(MemoryStore::new()),
            Box::new(ScriptedHarnessFactory),
        );
        let state = state_with(core);
        let projects = state
            .add_project(repository.path().display().to_string())
            .await?;
        let project = projects.projects.first().ok_or("project was not added")?;
        let task = state
            .create_draft(project.id.clone(), "terminal scope".into(), Agent::Codex)
            .await?;
        let task_id = task.task.id;
        let workspace = state
            .send_prompt(task_id.clone(), "create a worktree".into())
            .await?;
        let worktree = workspace
            .task
            .worktree_path
            .ok_or("worktree was not created")?;
        let mut saved = TerminalRecord::project_shared(
            TerminalId::new("task-terminal"),
            ProjectId::new(project.id.clone()),
            "Task terminal",
            PathBuf::from(&worktree),
            "/bin/sh",
            1,
        )
        .for_task(TaskId::new(task_id.clone()));
        saved.ended_at = Some(2);
        saved.exit_code = Some(7);
        state.core.lock().await.record_terminal(&saved).await?;

        let list = state
            .restart_terminal(saved.id.as_str().into(), 80, 24)
            .await?;
        let [running] = list.terminals.as_slice() else {
            return Err("restart should reuse one terminal record".into());
        };
        assert_eq!(running.id, saved.id.as_str());
        assert_eq!(running.task_id.as_deref(), Some(task_id.as_str()));
        assert_eq!(running.scope, shepherd_core::terminal::TerminalScope::Task);
        assert_eq!(running.cwd, worktree);
        assert_eq!(running.command, saved.command);
        assert_eq!((running.ended_at, running.exit_code), (None, None));
        assert!(
            state
                .terminals
                .screens
                .lock()
                .await
                .contains_key(saved.id.as_str())
        );

        let mut events = state.subscribe();
        state
            .terminal_write(
                saved.id.as_str().into(),
                TerminalInput::Text("exit 23\r".into()),
                true,
            )
            .await?;
        let ended = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Event::TerminalListChanged(snapshot) = events.recv().await?
                    && snapshot.revision > list.revision
                {
                    return Ok::<_, broadcast::error::RecvError>(snapshot);
                }
            }
        })
        .await??;
        assert_eq!(ended.project_id.as_str(), project.id);
        assert_eq!(
            ended
                .terminals
                .first()
                .and_then(|terminal| terminal.exit_code),
            Some(23)
        );
        assert_eq!(
            state.terminal_list(project.id.clone()).await.revision,
            ended.revision
        );
        state.quit(true).await?;
        Ok(())
    }

    #[tokio::test]
    async fn deleting_saved_task_removes_its_cache_and_preserves_other_work()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;
        let repository = TestRepository::new()?;
        let state = ShellState::load(
            directory.path(),
            Box::new(ScriptedHarnessFactory),
            TerminalHost::default(),
        )
        .await?;
        let projects = state
            .add_project(repository.path().display().to_string())
            .await?;
        let project = projects.projects.first().ok_or("missing project")?;
        let draft = state
            .create_draft(project.id.clone(), "Saved".into(), Agent::Codex)
            .await?;
        let task = draft.task.id;
        let other = state
            .create_draft(project.id.clone(), "Other".into(), Agent::Codex)
            .await?
            .task
            .id;
        let id = state
            .save_prompt_file(task.clone(), "notes.txt".into(), b"task notes")
            .await?;
        let other_id = state
            .save_prompt_file(other.clone(), "notes.txt".into(), b"other notes")
            .await?;
        let screenshot = state
            .save_preview_screenshot(task.clone(), include_bytes!("../icons/128x128.png"))
            .await?;
        let sent = state
            .send_prompt_with_files(
                task.clone(),
                "Read this".into(),
                vec![id],
                CodexTurnOptions::default(),
                None,
            )
            .await?;
        let worktree = PathBuf::from(sent.task.worktree_path.ok_or("missing worktree")?);
        let task_directory = directory.path().join("attachments").join(&task);
        let other_directory = directory.path().join("attachments").join(&other);
        let other_file = attachments::prompt_file(&other_directory, &other_id)?;
        assert!(state.delete_task(task.clone(), false).await.is_err());
        assert!(task_directory.is_dir());
        assert!(screenshot.is_file());
        state
            .resolve_approval(task.clone(), "approval-1".into(), Decision::Allow)
            .await?;
        std::fs::write(worktree.join("uncommitted.txt"), "preserve")?;
        assert!(state.delete_task(task.clone(), true).await.is_err());
        assert!(task_directory.is_dir());
        assert!(worktree.join("uncommitted.txt").is_file());
        state.delete_task(task.clone(), false).await?;
        assert!(!task_directory.exists());
        assert!(!screenshot.exists());
        assert_eq!(std::fs::read(other_file)?, b"other notes");
        assert!(worktree.is_dir());
        assert!(
            state
                .core
                .lock()
                .await
                .task_workspace(&TaskId::new(task))
                .is_err()
        );
        assert!(
            state
                .core
                .lock()
                .await
                .task_workspace(&TaskId::new(other))
                .is_ok()
        );
        state.quit(true).await?;
        Ok(())
    }

    #[tokio::test]
    async fn committed_deletion_cleanup_error_does_not_restore_attachments()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;
        let repository = TestRepository::new()?;
        let state = ShellState::load(
            directory.path(),
            Box::new(ScriptedHarnessFactory),
            TerminalHost::default(),
        )
        .await?;
        let projects = state
            .add_project(repository.path().display().to_string())
            .await?;
        let project = projects.projects.first().ok_or("missing project")?;
        let task = state
            .create_draft(project.id.clone(), "Cleanup".into(), Agent::Codex)
            .await?
            .task
            .id;
        let attachment = state
            .save_prompt_file(task.clone(), "notes.txt".into(), b"notes")
            .await?;
        state
            .send_prompt_with_files(
                task.clone(),
                "Read".into(),
                vec![attachment],
                CodexTurnOptions::default(),
                None,
            )
            .await?;
        state
            .resolve_approval(task.clone(), "approval-1".into(), Decision::Allow)
            .await?;
        let attachment_dir = directory.path().join("attachments").join(&task);
        assert!(attachment_dir.is_dir());
        let uri = format!("turso:{}", directory.path().join("shepherd.db").display());
        let mut database = toasty::Db::builder()
            .build(toasty_driver_turso::Turso::new(&uri)?.experimental_index_method(true))
            .await?;
        toasty::sql::statement("CREATE TRIGGER block_deletion_cleanup BEFORE DELETE ON worktree_deletions BEGIN SELECT RAISE(ABORT, 'fixture cleanup failure'); END").exec(&mut database).await?;
        let error = state
            .delete_task(task.clone(), true)
            .await
            .err()
            .ok_or("expected cleanup error")?;
        assert!(error.to_string().contains("was deleted"));
        assert!(!attachment_dir.exists());
        assert!(
            state
                .core
                .lock()
                .await
                .task_workspace(&TaskId::new(&task))
                .is_err()
        );
        toasty::sql::statement("DROP TRIGGER block_deletion_cleanup")
            .exec(&mut database)
            .await?;
        state.quit(true).await?;
        drop(state);
        let reopened = Core::load(
            Box::new(TursoStore::open(&uri).await?),
            Box::new(shepherd_core::harness::UnavailableHarnessFactory),
        )
        .await?;
        assert!(
            reopened
                .process_store()
                .load_worktree_deletions()
                .await?
                .is_empty()
        );
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn killed_attachment_deletion_recovers_before_and_after_commit()
    -> Result<(), Box<dyn std::error::Error>> {
        if let Some(data) = std::env::var_os("SHEPHERD_ATTACHMENT_CRASH_DATA") {
            let data = PathBuf::from(data);
            let repository = PathBuf::from(
                std::env::var_os("SHEPHERD_ATTACHMENT_CRASH_REPOSITORY")
                    .ok_or("missing repository")?,
            );
            let state = ShellState::load(
                &data,
                Box::new(ScriptedHarnessFactory),
                TerminalHost::default(),
            )
            .await?;
            let projects = state.add_project(repository.display().to_string()).await?;
            let project = projects.projects.first().ok_or("missing project")?;
            let task = state
                .create_draft(project.id.clone(), "Crash attachment".into(), Agent::Codex)
                .await?
                .task
                .id;
            let id = state
                .save_prompt_file(task.clone(), "notes.txt".into(), b"important attachment")
                .await?;
            std::fs::write(data.join("fixture-attachment-id"), id)?;
            state.discard_draft(task).await?;
            return Err("attachment deletion fixture unexpectedly finished".into());
        }
        use std::os::unix::process::ExitStatusExt as _;
        for phase in ["staged", "committed"] {
            let directory = TestDirectory::new()?;
            let repository = TestRepository::new()?;
            let ready = directory.path().join("attachment-crash-ready");
            let mut child = tokio::process::Command::new(std::env::current_exe()?)
                .args([
                    "--exact",
                    "bridge::tests::killed_attachment_deletion_recovers_before_and_after_commit",
                    "--nocapture",
                ])
                .env("SHEPHERD_ATTACHMENT_CRASH_DATA", directory.path())
                .env("SHEPHERD_ATTACHMENT_CRASH_REPOSITORY", repository.path())
                .env("SHEPHERD_ATTACHMENT_CRASH_READY", &ready)
                .env("SHEPHERD_ATTACHMENT_CRASH_PHASE", phase)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .kill_on_drop(true)
                .spawn()?;
            let task = tokio::time::timeout(Duration::from_secs(20), async {
                loop {
                    match std::fs::read_to_string(&ready) {
                        Ok(id) if !id.is_empty() => break Ok::<_, Box<dyn std::error::Error>>(id),
                        Ok(_) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => break Err(error.into()),
                    }
                    if let Some(status) = child.try_wait()? {
                        break Err(
                            format!("attachment deletion fixture exited early: {status}").into(),
                        );
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await??;
            child.kill().await?;
            assert_eq!(child.wait().await?.signal(), Some(9));
            let id = std::fs::read_to_string(directory.path().join("fixture-attachment-id"))?;
            let root = directory.path().join("attachments");
            let staged = root.join(format!(".shepherd-deleting-task-{task}"));
            assert_eq!(
                std::fs::read(attachments::prompt_file(&staged, &id)?)?,
                b"important attachment"
            );
            let state = ShellState::load(
                directory.path(),
                Box::new(ScriptedHarnessFactory),
                TerminalHost::default(),
            )
            .await?;
            assert!(!staged.exists());
            let original = root.join(&task);
            if phase == "staged" {
                assert_eq!(
                    std::fs::read(attachments::prompt_file(&original, &id)?)?,
                    b"important attachment"
                );
                assert!(
                    state
                        .core
                        .lock()
                        .await
                        .task_workspace(&TaskId::new(&task))
                        .is_ok()
                );
            } else {
                assert!(!original.exists());
                assert!(
                    state
                        .core
                        .lock()
                        .await
                        .task_workspace(&TaskId::new(&task))
                        .is_err()
                );
            }
            state.quit(true).await?;
        }
        Ok(())
    }

    #[tokio::test]
    async fn startup_recovers_staged_attachments_from_task_state()
    -> Result<(), Box<dyn std::error::Error>> {
        for deleted in [false, true] {
            let directory = TestDirectory::new()?;
            let repository = TestRepository::new()?;
            let state = ShellState::load(
                directory.path(),
                Box::new(ScriptedHarnessFactory),
                TerminalHost::default(),
            )
            .await?;
            let projects = state
                .add_project(repository.path().display().to_string())
                .await?;
            let project = projects.projects.first().ok_or("missing project")?;
            let task = state
                .create_draft(project.id.clone(), "Recovery".into(), Agent::Codex)
                .await?
                .task
                .id;
            let id = state
                .save_prompt_file(task.clone(), "notes.txt".into(), b"recover me")
                .await?;
            let original = directory.path().join("attachments").join(&task);
            let staged = attachments::stage_task(&original)?.ok_or("missing staged attachments")?;
            if deleted {
                state
                    .core
                    .lock()
                    .await
                    .execute(Command::DiscardDraft {
                        task_id: TaskId::new(&task),
                    })
                    .await?;
            }
            state.quit(true).await?;
            drop(state);
            let reopened = ShellState::load(
                directory.path(),
                Box::new(ScriptedHarnessFactory),
                TerminalHost::default(),
            )
            .await?;
            assert!(!staged.exists());
            if deleted {
                assert!(!original.exists());
            } else {
                assert_eq!(
                    std::fs::read(attachments::prompt_file(&original, &id)?)?,
                    b"recover me"
                );
            }
            reopened.quit(true).await?;
        }
        Ok(())
    }

    #[tokio::test]
    async fn attachment_writes_respect_deletion_lock_and_durable_task_state()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;
        let repository = TestRepository::new()?;
        let state = ShellState::load(
            directory.path(),
            Box::new(ScriptedHarnessFactory),
            TerminalHost::default(),
        )
        .await?;
        let projects = state
            .add_project(repository.path().display().to_string())
            .await?;
        let project = projects.projects.first().ok_or("missing project")?;
        let task = state
            .create_draft(project.id.clone(), "Concurrent".into(), Agent::Codex)
            .await?
            .task
            .id;
        let lock = attachments::lock_deletions(directory.path())?;
        assert!(
            state
                .save_prompt_file(task.clone(), "notes.txt".into(), b"blocked")
                .await
                .is_err()
        );
        drop(lock);
        state
            .save_prompt_file(task.clone(), "notes.txt".into(), b"allowed")
            .await?;
        let store = state.core.lock().await.process_store();
        store.delete_task(&TaskId::new(&task), None).await?;
        assert!(
            state
                .save_prompt_file(task.clone(), "stale.txt".into(), b"blocked")
                .await
                .is_err()
        );
        assert!(
            state
                .save_preview_screenshot(task.clone(), include_bytes!("../icons/128x128.png"))
                .await
                .is_err()
        );
        Ok(())
    }

    #[tokio::test]
    async fn screenshots_are_task_scoped_and_removed_with_a_discarded_draft()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;
        let repository = TestRepository::new()?;
        let state = ShellState::load(
            directory.path(),
            Box::new(ScriptedHarnessFactory),
            TerminalHost::default(),
        )
        .await?;
        let list = state
            .add_project(repository.path().display().to_string())
            .await?;
        let project = list.projects.first().ok_or("missing project")?;
        let workspace = state
            .create_draft(project.id.clone(), "Preview".into(), Agent::Codex)
            .await?;
        let task = workspace.task.id.as_str().to_owned();
        let other = state
            .create_draft(project.id.clone(), "Other".into(), Agent::Codex)
            .await?
            .task
            .id
            .as_str()
            .to_owned();
        let image = include_bytes!("../icons/128x128.png");
        assert!(
            state
                .save_preview_screenshot(task.clone(), b"invalid image")
                .await
                .is_err()
        );
        assert!(
            state
                .save_preview_screenshot(uuid::Uuid::new_v4().to_string(), image)
                .await
                .is_err()
        );
        let path = state.save_preview_screenshot(task.clone(), image).await?;
        assert_eq!(
            path.parent(),
            Some(
                directory
                    .path()
                    .canonicalize()?
                    .join("attachments")
                    .join(&task)
                    .as_path()
            )
        );
        assert_eq!(std::fs::read(&path)?, image);
        assert_eq!(
            state.save_preview_screenshot(task.clone(), image).await?,
            path
        );
        let file_id = state
            .save_prompt_file(task.clone(), "notes.txt".into(), b"draft notes")
            .await?;
        let other_file_id = state
            .save_prompt_file(other.clone(), "notes.txt".into(), b"other notes")
            .await?;
        let task_directory = directory.path().join("attachments").join(&task);
        let other_directory = directory.path().join("attachments").join(&other);
        let file = attachments::prompt_file(&task_directory, &file_id)?;
        let other_file = attachments::prompt_file(&other_directory, &other_file_id)?;
        state.discard_draft(task.clone()).await?;
        assert!(!path.exists());
        assert!(!file.exists());
        assert!(!task_directory.exists());
        assert_eq!(std::fs::read(&other_file)?, b"other notes");
        assert_eq!(
            attachments::prompt_file(&other_directory, &other_file_id)?,
            other_file
        );
        assert!(state.save_preview_screenshot(task, image).await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn project_attachment_limit_applies_to_uploads_screenshots_and_cached_files()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;
        let restricted = TestRepository::new()?;
        let inherited = TestRepository::new()?;
        let mut state = ShellState::load(
            directory.path(),
            Box::new(ScriptedHarnessFactory),
            TerminalHost::default(),
        )
        .await?;
        state
            .add_project(restricted.path().display().to_string())
            .await?;
        let projects = state
            .add_project(inherited.path().display().to_string())
            .await?;
        let restricted_path = restricted.path().canonicalize()?;
        let inherited_path = inherited.path().canonicalize()?;
        let restricted = projects
            .projects
            .iter()
            .find(|project| Path::new(&project.path) == restricted_path)
            .ok_or("missing restricted project")?;
        let inherited = projects
            .projects
            .iter()
            .find(|project| Path::new(&project.path) == inherited_path)
            .ok_or("missing inherited project")?;
        let restricted_task = state
            .create_draft(restricted.id.clone(), "Restricted".into(), Agent::Codex)
            .await?
            .task
            .id;
        let inherited_task = state
            .create_draft(inherited.id.clone(), "Inherited".into(), Agent::Codex)
            .await?
            .task
            .id;
        let oversized = b"nine-byte";
        let old_id = state
            .save_prompt_file(restricted_task.clone(), "old.txt".into(), oversized)
            .await?;

        let settings_path = directory.path().join("settings.json");
        std::fs::write(
            &settings_path,
            serde_json::to_vec(&serde_json::json!({
                "version": 1,
                "app": {"attachments.max_file_bytes": 24},
                "projects": {(restricted.path.clone()): {"attachments.max_file_bytes": 8}}
            }))?,
        )?;
        state.preferences = Settings::new(settings_path);
        state.preferences.reload().await;
        let limit = state.attachment_limit(restricted.id.clone()).await?;
        assert_eq!(limit.bytes, 8);
        assert_eq!(limit.label, "8 bytes");
        assert_eq!(
            state.attachment_limit(inherited.id.clone()).await?.bytes,
            24
        );

        assert!(
            state
                .save_prompt_file(restricted_task.clone(), "ok.txt".into(), b"12345678")
                .await
                .is_ok()
        );
        let error = state
            .save_prompt_file(restricted_task.clone(), "large.txt".into(), oversized)
            .await
            .err()
            .ok_or("oversized upload was accepted")?;
        assert!(error.message.contains("8 bytes"));
        assert!(
            state
                .save_prompt_file(inherited_task.clone(), "ok.txt".into(), oversized)
                .await
                .is_ok()
        );
        let image = b"\x89PNG\r\n\x1a\n\0";
        assert!(
            state
                .save_preview_screenshot(restricted_task.clone(), image)
                .await
                .is_err()
        );
        assert!(
            state
                .save_preview_screenshot(inherited_task, image)
                .await
                .is_ok()
        );
        assert!(
            state
                .send_prompt_with_files(
                    restricted_task,
                    "Read the old attachment".into(),
                    vec![old_id],
                    CodexTurnOptions::default(),
                    None,
                )
                .await
                .err()
                .is_some_and(|error| error.message.contains("8 bytes"))
        );
        Ok(())
    }

    #[tokio::test]
    async fn prompt_attachments_are_resolved_and_restored_for_the_selected_task()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;
        let repository = TestRepository::new()?;
        let state = ShellState::load(
            directory.path(),
            Box::new(ScriptedHarnessFactory),
            TerminalHost::default(),
        )
        .await?;
        let projects = state
            .add_project(repository.path().display().to_string())
            .await?;
        let project = projects.projects.first().ok_or("missing project")?;
        let draft = state
            .create_draft(
                project.id.clone(),
                "Attachment fixture".into(),
                Agent::Codex,
            )
            .await?;
        let task = draft.task.id.as_str().to_owned();
        let other = state
            .create_draft(project.id.clone(), "Other task".into(), Agent::Codex)
            .await?;
        let contents = b"file contents";
        let id = state
            .save_prompt_file(task.clone(), "fixture.txt".into(), contents)
            .await?;
        assert_eq!(
            state
                .send_prompt_with_files(
                    other.task.id.as_str().into(),
                    String::new(),
                    vec![id.clone()],
                    CodexTurnOptions::default(),
                    None,
                )
                .await
                .err()
                .map(|error| error.code),
            Some("attachments")
        );
        let sent = state
            .send_prompt_with_files(
                task.clone(),
                String::new(),
                vec![id.clone()],
                CodexTurnOptions::default(),
                None,
            )
            .await?;
        let entry = sent
            .transcript
            .iter()
            .find(|entry| matches!(entry.kind, agent_harness::EntryKind::UserMessage { .. }))
            .ok_or("missing prompt")?;
        let agent_harness::EntryKind::UserMessage { text } = &entry.kind else {
            return Err("expected a user prompt".into());
        };
        let references = text
            .strip_prefix("Use the attached files.\n\nAttached files\n")
            .ok_or("missing file references in prompt")?;
        let paths = serde_json::from_str::<Vec<PathBuf>>(references)?;
        let [file] = paths.as_slice() else {
            return Err("expected one attachment".into());
        };
        assert!(file.ends_with("fixture.txt"));
        assert_eq!(std::fs::read(file)?, contents);
        assert_eq!(
            entry.attachments,
            vec![agent_harness::AttachmentMetadata {
                relative_path: PathBuf::from(format!("file-{id}")).join("fixture.txt"),
                id,
                mime: "text/plain".into(),
                bytes: contents.len() as u64,
            }]
        );
        state.quit(true).await?;
        drop(state);
        let reopened = ShellState::load(
            directory.path(),
            Box::new(ScriptedHarnessFactory),
            TerminalHost::default(),
        )
        .await?;
        let restored = reopened.task_workspace(task).await?;
        let restored_entry = restored
            .transcript
            .iter()
            .find(|row| row.id == entry.id)
            .ok_or("missing restored prompt")?;
        assert_eq!(restored_entry, entry);
        Ok(())
    }

    #[tokio::test]
    async fn startup_snapshots_only_previously_recorded_processes()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;
        let state = ShellState::load(
            directory.path(),
            Box::new(ScriptedHarnessFactory),
            TerminalHost::default(),
        )
        .await?;
        let record = ProcessRecord {
            id: "old-terminal".into(),
            kind: "terminal".into(),
            owner_id: "project".into(),
            pid: 123,
            started_at: 456,
            process_group: 123,
            session_id: 123,
            boot_id: "boot".into(),
            token: "token".into(),
        };
        state.core.lock().await.record_process(&record).await?;
        assert!(state.previous_processes.lock().await.is_empty());
        drop(state);

        let reopened = ShellState::load(
            directory.path(),
            Box::new(ScriptedHarnessFactory),
            TerminalHost::default(),
        )
        .await?;
        assert_eq!(*reopened.previous_processes.lock().await, vec![record]);
        Ok(())
    }

    #[tokio::test]
    async fn cold_start_loads_the_shell_within_the_warm_budget()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;
        let started = std::time::Instant::now();
        let cold = ShellState::load(
            directory.path(),
            Box::new(ScriptedHarnessFactory),
            TerminalHost::default(),
        )
        .await?;
        let cold_elapsed = started.elapsed();
        drop(cold);
        let started = std::time::Instant::now();
        let warm = ShellState::load(
            directory.path(),
            Box::new(ScriptedHarnessFactory),
            TerminalHost::default(),
        )
        .await?;
        let warm_elapsed = started.elapsed();
        eprintln!("headless cold start {cold_elapsed:.1?}, warm start {warm_elapsed:.1?}");
        assert!(
            cold_elapsed < std::time::Duration::from_secs(4),
            "headless cold start took {cold_elapsed:.1?}, over the 4 s budget"
        );
        assert!(
            warm_elapsed < std::time::Duration::from_millis(1200),
            "headless warm start took {warm_elapsed:.1?}, over the 1.2 s budget"
        );
        drop(warm);
        Ok(())
    }

    #[tokio::test]
    async fn load_opens_the_durable_core_and_survives_a_reopen()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;

        let state = ShellState::load(
            directory.path(),
            Box::new(ScriptedHarnessFactory),
            TerminalHost::default(),
        )
        .await?;
        let projects = state
            .add_project(directory.path().display().to_string())
            .await?;
        assert_eq!(projects.projects.len(), 1);
        let project_id = projects
            .projects
            .first()
            .ok_or("missing project")?
            .id
            .clone();
        let preview = "http://[::1]:3000/demo?view=compact#header";
        state
            .set_project_preview_url(project_id.clone(), format!(" {preview} "))
            .await?;
        for invalid in [
            "file:///tmp/demo.html",
            "https://example.com/",
            "http://localhost.example.com/",
            "http://secret@localhost:3000/",
        ] {
            assert!(
                state
                    .set_project_preview_url(project_id.clone(), invalid.into())
                    .await
                    .is_err()
            );
        }
        assert!(
            directory.join("shepherd.db").is_file(),
            "the shell should create its database in the data directory"
        );
        drop(state);

        let reopened = ShellState::load(
            directory.path(),
            Box::new(ScriptedHarnessFactory),
            TerminalHost::default(),
        )
        .await?;
        let projects = reopened.project_list().await;
        assert_eq!(projects.projects.len(), 1);
        assert!(!projects.projects[0].is_git_repository);
        assert!(!projects.projects[0].name.is_empty());
        assert_eq!(projects.projects[0].preview_url.as_deref(), Some(preview));
        let cleared = reopened
            .set_project_preview_url(project_id.clone(), String::new())
            .await?;
        assert_eq!(
            cleared
                .projects
                .first()
                .and_then(|project| project.preview_url.as_deref()),
            None
        );
        assert!(reopened.open_project_preview(project_id).await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn annotation_wrapper_stops_when_the_shell_quits()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;
        let state = ShellState::new(Core::new());
        let projects = state
            .add_project(directory.path().display().to_string())
            .await?;
        let project = projects.projects.first().ok_or("missing project")?;
        state
            .set_project_preview_url(project.id.clone(), "http://localhost:3000/demo".into())
            .await?;
        let wrapper = state
            .annotation_wrapper_url(project.id.clone(), None)
            .await?;
        assert_eq!(wrapper.host_str(), Some("127.0.0.1"));
        assert!(wrapper.path().starts_with("/preview/"));
        let address = format!(
            "{}:{}",
            wrapper.host_str().ok_or("missing host")?,
            wrapper.port().ok_or("missing port")?
        );
        assert!(tokio::net::TcpStream::connect(&address).await.is_ok());
        assert_eq!(
            state
                .annotation_wrapper_url(project.id.clone(), None)
                .await?,
            wrapper
        );
        state
            .set_project_preview_url(project.id.clone(), "http://localhost:3001/demo".into())
            .await?;
        assert_ne!(
            state
                .annotation_wrapper_url(project.id.clone(), None)
                .await?,
            wrapper
        );
        state.quit(true).await?;
        assert!(tokio::net::TcpStream::connect(address).await.is_err());
        Ok(())
    }
}
