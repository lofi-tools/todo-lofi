//! The shell's command and event boundary.
//!
//! One method per core [`Command`], plus snapshot readers for a fresh window and
//! a single event stream. Methods return the core's own typed snapshots; the
//! shell does not keep a second copy of the state model.
//!
//! Core events are drained after every command and broadcast to whatever window
//! is subscribed. Adapter events that arrive between commands are folded by a
//! short pump tick ([`ShellState::start_event_pump`]) and broadcast the same way,
//! so a streaming turn paints without waiting for the next invoke.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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
use shepherd_core::store::TursoStore;
use shepherd_core::terminal::{
    InspectorTab, TerminalEvent, TerminalId, TerminalList, TerminalRecord, TerminalService,
    TerminalSpec,
};
use shepherd_core::{
    Agent, Command, CommandResult, Core, CoreError, Event, ProjectApprovalsSnapshot, ProjectId,
    ProjectList, QuitSummary, TaskId, TaskList, TaskWorkspace, WorkspaceLayout,
};
use tokio::sync::{Mutex, broadcast, mpsc, oneshot, watch};
use tokio::time::Instant;

use crate::attachments::{self, AttachmentLimit};
use crate::codex_harness::codex_spawn_spec;
use crate::file_actions::{self, FileAction};
use crate::file_watch::{FileWatchUpdate, WorkspaceWatch};
use crate::pty_terminal::PtyTerminals;
use crate::settings::{DEFAULT_ATTACHMENT_BYTES, DEFAULT_SCROLLBACK, SettingKey, Settings};
use crate::terminal_screen::{Screen, SearchDirection, TerminalAppearance, TerminalFrame};

#[derive(Debug, thiserror::Error, Serialize)]
#[error("{message}")]
pub struct BridgeError {
    code: &'static str,
    message: String,
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
            quit_confirmation: None,
        }
    }
}

impl From<CoreError> for BridgeError {
    fn from(error: CoreError) -> Self {
        let mut bridge_error = Self::new("core", error.to_string());
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
        Self::new("files", error.to_string())
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
}

impl Default for TerminalHost {
    fn default() -> Self {
        Self {
            engine: Arc::new(Mutex::new(PtyTerminals::new())),
            screens: Arc::new(Mutex::new(BTreeMap::new())),
            appearance: Arc::new(Mutex::new(TerminalAppearance::default())),
        }
    }
}

struct PreparedTerminal {
    id: TerminalId,
    slot: Arc<Mutex<ScreenSlot>>,
    output: mpsc::Receiver<TerminalEvent>,
}

struct RecordedTerminal {
    record: TerminalRecord,
    core: Arc<Mutex<Core>>,
    events: broadcast::Sender<Event>,
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
        let mut screen =
            Screen::with_scrollback(usize::from(columns), usize::from(rows), scrollback);
        screen.set_appearance(*self.appearance.lock().await);
        let (frames, _) = watch::channel(Arc::new(screen.frame()));
        let slot = Arc::new(Mutex::new(ScreenSlot {
            revision: 0,
            screen,
            frames,
        }));
        screens.insert(id.as_str().to_string(), slot.clone());
        Ok(PreparedTerminal { id, slot, output })
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
    ) {
        let engine = self.engine.clone();
        let screens = self.screens.clone();
        let PreparedTerminal {
            id,
            slot,
            mut output,
        } = terminal;
        tauri::async_runtime::spawn(async move {
            let mut pending = None;
            let mut deadline = None;
            let mut protocol = protocol;
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
                        if let Some(recorded) = &recorded {
                            let mut core = recorded.core.lock().await;
                            if let Err(error) = core.end_terminal(&id, exit_code).await {
                                tracing::warn!(%error, "could not mark the terminal ended");
                            }
                            for event in core.drain_events() {
                                if let Err(error) = recorded.events.send(event) {
                                    tracing::debug!(%error, "no shell is subscribed to terminal events");
                                }
                            }
                        }
                        break;
                    }
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

#[async_trait]
impl TerminalBackend for TerminalHost {
    async fn spawn(
        &self,
        launch: TerminalLaunch,
        events: mpsc::Sender<TerminalProcessEvent>,
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
        self.observe(terminal, None, Some(events));
        Ok(())
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
    attachment_root: Option<PathBuf>,
    pub(crate) preferences: Settings,
    pub(crate) codex_program: Option<PathBuf>,
    pub(crate) codex_detected: bool,
    pub(crate) opencode_launch: Option<SpawnSpec>,
    pub(crate) claude_configured: bool,
    events: broadcast::Sender<Event>,
    terminal_subscriptions: Arc<Mutex<BTreeMap<String, TerminalSubscription>>>,
    terminal_lifecycle: Mutex<()>,
    terminals: TerminalHost,
    scan: Arc<Mutex<Option<shepherd_core::project_scan::ProjectScan>>>,
    file_searches: Mutex<BTreeMap<String, FileSearch>>,
    task_searches: Mutex<BTreeMap<String, TaskSearchRequest>>,
    file_watches: Arc<Mutex<BTreeMap<String, oneshot::Sender<()>>>>,
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

impl ShellState {
    #[cfg(test)]
    pub fn new(core: Core) -> Self {
        Self::with_terminals(core, TerminalHost::default())
    }

    fn with_terminals(core: Core, terminals: TerminalHost) -> Self {
        let (events, _) = broadcast::channel(512);
        Self {
            core: Arc::new(Mutex::new(core)),
            attachment_root: None,
            preferences: Settings::default(),
            codex_program: None,
            codex_detected: false,
            opencode_launch: None,
            claude_configured: false,
            events,
            terminal_subscriptions: Arc::new(Mutex::new(BTreeMap::new())),
            terminal_lifecycle: Mutex::new(()),
            terminals,
            scan: Arc::new(Mutex::new(None)),
            file_searches: Mutex::new(BTreeMap::new()),
            task_searches: Mutex::new(BTreeMap::new()),
            file_watches: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    /// Open the durable core in the app data directory. A first run creates the
    /// database and schema; a later run marks every live task `interrupted`,
    /// because no agent process survives the previous shutdown.
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
        let database_path = data_directory.join("shepherd.db");
        let store = TursoStore::open_at(&database_path)
            .await
            .map_err(|error| BridgeError::new("storage", error.to_string()))?;
        let core = Core::load(Box::new(store), harness_factory)
            .await
            .map_err(BridgeError::from)?;
        let mut state = Self::with_terminals(core, terminals);
        state.attachment_root = Some(data_directory.join("attachments"));

        // Walking the home directory can take seconds, so the scan starts with
        // the app and the picker reads whatever has landed by the time it opens.
        if let Some(home) = project_scan_root() {
            let scan = state.scan.clone();
            tauri::async_runtime::spawn(async move {
                match tokio::task::spawn_blocking(move || shepherd_core::project_scan::scan(&home))
                    .await
                {
                    Ok(found) => *scan.lock().await = Some(found),
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
    /// window without waiting for the next invoke, so this runs on a short tick
    /// and broadcasts whatever the core emits.
    pub fn start_event_pump(&self) {
        let core = self.core.clone();
        let events = self.events.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(100)).await;
                let mut core = core.lock().await;
                if let Err(error) = core.pump().await {
                    tracing::warn!(%error, "could not fold agent activity");
                }
                Self::broadcast_core_events(&mut core, &events);
            }
        });
    }

    async fn execute(&self, command: Command) -> Result<CommandResult, BridgeError> {
        let mut core = self.core.lock().await;
        let result = core.execute(command).await;
        Self::broadcast_core_events(&mut core, &self.events);
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
        match self
            .execute(Command::SetProjectPreviewUrl {
                project_id: ProjectId::new(project_id),
                url,
            })
            .await?
        {
            CommandResult::ProjectList(projects) => Ok(projects),
            other => Err(unexpected(&other)),
        }
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
        // A draft cannot be discarded until its screenshot write finishes.
        let core = self.core.lock().await;
        let limit = self
            .attachment_limit_for(&core.task_project_path(&TaskId::new(task_id))?)
            .await;
        limit
            .check(bytes.len() as u64)
            .map_err(|error| BridgeError::new("preview", error.to_string()))?;
        let bytes = bytes.to_vec();
        let result = tokio::task::spawn_blocking(move || {
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
        let core = self.core.lock().await;
        let limit = self
            .attachment_limit_for(&core.task_project_path(&TaskId::new(task_id))?)
            .await;
        limit
            .check(bytes.len() as u64)
            .map_err(|error| BridgeError::new("attachments", error.to_string()))?;
        let bytes = bytes.to_vec();
        let result = tokio::task::spawn_blocking(move || {
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
        let attachments = self.task_attachment_directory(&task_id);
        match self
            .execute(Command::DiscardDraft {
                task_id: TaskId::new(task_id),
            })
            .await?
        {
            CommandResult::Ack => {
                if let Some(path) = attachments {
                    match tokio::task::spawn_blocking(move || attachments::remove_task(&path)).await
                    {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => {
                            tracing::warn!(%error, "could not remove discarded draft attachments")
                        }
                        Err(error) => tracing::warn!(%error, "attachment removal worker failed"),
                    }
                }
                Ok(())
            }
            other => Err(unexpected(&other)),
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
            let directory = self.task_attachment_directory(&task_id).ok_or_else(|| {
                BridgeError::new(
                    "attachments",
                    "Could not locate this task's attachment directory.",
                )
            })?;
            let path = self
                .core
                .lock()
                .await
                .task_project_path(&TaskId::new(&task_id))?;
            let limit = self.attachment_limit_for(&path).await;
            let attachments = tokio::task::spawn_blocking(move || {
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
        let _guard = self.terminal_lifecycle.lock().await;
        match self.execute(Command::Quit { force }).await? {
            CommandResult::Quit(summary) => {
                let mut engine = std::mem::take(&mut *self.terminals.engine.lock().await);
                if let Err(error) =
                    tokio::task::spawn_blocking(move || engine.terminate_all()).await
                {
                    tracing::error!(%error, "terminal shutdown worker failed");
                }
                Ok(summary)
            }

            other => Err(unexpected(&other)),
        }
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
        let directory = self
            .core
            .lock()
            .await
            .project_path(&ProjectId::new(project_id))?;
        agent_harness::codex::project_models(codex_spawn_spec(program, directory))
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
        let cwd = self
            .core
            .lock()
            .await
            .project_path(&ProjectId::new(project_id))?;
        agent_harness::opencode::project_modes(&SpawnSpec {
            cwd,
            ..launch.clone()
        })
        .await
        .map_err(|error| BridgeError::new("opencode_modes", error.to_string()))
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
        let mut cache = self.scan.lock().await;
        if rescan || cache.is_none() {
            let home = project_scan_root()
                .ok_or_else(|| BridgeError::new("scan", "no home directory to scan"))?;
            let found =
                tokio::task::spawn_blocking(move || shepherd_core::project_scan::scan(&home))
                    .await
                    .map_err(|error| {
                        BridgeError::new(
                            "scan",
                            format!("the project scan did not finish: {error}"),
                        )
                    })?;
            *cache = Some(found);
        }
        cache
            .clone()
            .ok_or_else(|| BridgeError::new("scan", "the project scan produced no result"))
    }

    pub async fn task_list(&self, project_id: String) -> Result<TaskList, BridgeError> {
        self.core
            .lock()
            .await
            .task_list(&ProjectId::new(project_id))
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
        Ok(self.core.lock().await.workspace_directory(
            &ProjectId::new(project_id),
            task_id.map(TaskId::new).as_ref(),
        )?)
    }

    async fn watch_workspace_files(
        &self,
        project_id: String,
        task_id: Option<String>,
        channel: tauri::ipc::Channel<FileWatchUpdate>,
    ) -> Result<String, BridgeError> {
        let root = self.files_directory(project_id, task_id).await?;
        let mut watcher = tokio::task::spawn_blocking(move || WorkspaceWatch::start(root))
            .await
            .map_err(|error| {
                BridgeError::new(
                    "files",
                    format!("Could not start the file watcher: {error}"),
                )
            })?
            .map_err(|error| {
                BridgeError::new("files", format!("Could not watch workspace files: {error}"))
            })?;
        let (cancel, mut cancelled) = oneshot::channel();
        let watch_id = uuid::Uuid::new_v4().to_string();
        self.file_watches
            .lock()
            .await
            .insert(watch_id.clone(), cancel);
        let watches = self.file_watches.clone();
        let id = watch_id.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                let update = tokio::select! {
                    _ = &mut cancelled => break,
                    snapshot = watcher.snapshot() => match snapshot {
                        Ok(update) => update,
                        Err(error) => FileWatchUpdate::Error { message: format!("Could not refresh Git changes: {error}") },
                    }
                };
                if let Err(error) = channel.send(update) {
                    tracing::debug!(%error, "the workspace file watch detached");
                    break;
                }
                tokio::select! {
                    _ = &mut cancelled => break,
                    refresh = watcher.next_refresh() => if let Err(error) = refresh {
                        let update = FileWatchUpdate::Error { message: format!("Could not watch workspace files: {error}") };
                        if let Err(error) = channel.send(update) {
                            tracing::debug!(%error, "the workspace file watch detached");
                        }
                        break;
                    },
                }
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
        let (state, project) = {
            let core = self.core.lock().await;
            let project = record
                .as_ref()
                .map(|record| core.project_path(&record.project_id))
                .transpose()?;
            (core.shutdown_state(), project)
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
        if let Some(record) = &record
            && let Err(error) = self.core.lock().await.record_terminal(record).await
        {
            self.terminals
                .screens
                .lock()
                .await
                .remove(terminal_id.as_str());
            if let Err(close_error) = self.terminals.stop_terminal_process(terminal_id).await {
                tracing::warn!(%close_error, "could not roll back terminal spawn");
            }
            return Err(error.into());
        }
        let recorded = record.map(|record| RecordedTerminal {
            record,
            core: Arc::clone(&self.core),
            events: self.events.clone(),
        });
        self.terminals.observe(terminal, recorded, None);
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
        if follow_output && let Ok(slot) = self.screen_slot(&terminal_id).await {
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
        Ok(state
            .core
            .lock()
            .await
            .git_file_diff(&TaskId::new(task_id), &file)?)
    }

    #[tauri::command]
    pub async fn git_snapshot(
        state: State<'_, ShellState>,
        task_id: String,
    ) -> Result<shepherd_core::git::GitSnapshot, BridgeError> {
        Ok(state
            .core
            .lock()
            .await
            .git_snapshot(&TaskId::new(task_id))?)
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
        Ok(tokio::task::spawn_blocking(move || {
            files::list_directory(&root, Path::new(&path), show_dotfiles)
        })
        .await
        .map_err(|error| {
            BridgeError::new(
                "files",
                format!("Could not finish reading the folder: {error}"),
            )
        })??)
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
        Ok(state
            .core
            .lock()
            .await
            .review_file_diff(&TaskId::new(task_id), &file)?)
    }

    #[tauri::command]
    pub async fn review_snapshot(
        state: State<'_, ShellState>,
        task_id: String,
    ) -> Result<shepherd_core::git::ReviewSnapshot, BridgeError> {
        Ok(state
            .core
            .lock()
            .await
            .review_snapshot(&TaskId::new(task_id))?)
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
            loop {
                match receiver.recv().await {
                    Ok(event) => {
                        if let Err(error) = channel.send(event) {
                            tracing::debug!(%error, "the shell event channel closed");
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(skipped, "the shell missed core events; reload snapshots");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use shepherd_core::store::MemoryStore;
    use shepherd_core::test_support::{TestDirectory, TestRepository};
    use shepherd_core::{TaskState, TaskSummary};

    use super::*;

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
        state.discard_draft(task.clone()).await?;
        assert!(!path.exists());
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
}
