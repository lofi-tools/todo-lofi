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
use std::time::Duration;

use agent_harness::Decision;
use serde::{Deserialize, Serialize};
use shepherd_core::git::GitService;
use shepherd_core::harness::HarnessFactory;
use shepherd_core::store::TursoStore;
use shepherd_core::terminal::{
    InspectorTab, TerminalEvent, TerminalId, TerminalList, TerminalRecord, TerminalService,
    TerminalSpec,
};
use shepherd_core::{
    Agent, Command, CommandResult, Core, CoreError, Event, ProjectId, ProjectList, QuitSummary,
    TaskId, TaskList, TaskWorkspace, WorkspaceLayout,
};
use tokio::sync::{Mutex, broadcast, mpsc, oneshot, watch};
use tokio::time::Instant;

use crate::codex_harness::codex_spawn_spec;
use crate::pty_terminal::PtyTerminals;
use crate::terminal_screen::{Screen, SearchDirection, TerminalFrame};

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

/// The state Tauri manages for the window: one core, one event broadcaster, the
/// PTY engine, one parsed screen per live terminal, and the cached project scan
/// shared with the startup scan task.
struct TerminalSubscription {
    _cancel: oneshot::Sender<()>,
    acknowledgements: watch::Sender<u32>,
}

pub struct ShellState {
    core: Arc<Mutex<Core>>,
    pub(crate) codex_program: Option<PathBuf>,
    pub(crate) opencode_configured: bool,
    events: broadcast::Sender<Event>,
    terminal_subscriptions: Arc<Mutex<BTreeMap<String, TerminalSubscription>>>,
    terminal_lifecycle: Mutex<()>,
    engine: Arc<Mutex<PtyTerminals>>,
    screens: Arc<Mutex<BTreeMap<String, Arc<Mutex<ScreenSlot>>>>>,
    scan: Arc<Mutex<Option<shepherd_core::project_scan::ProjectScan>>>,
}

impl ShellState {
    pub fn new(core: Core) -> Self {
        let (events, _) = broadcast::channel(512);
        Self {
            core: Arc::new(Mutex::new(core)),
            codex_program: None,
            opencode_configured: false,
            events,
            terminal_subscriptions: Arc::new(Mutex::new(BTreeMap::new())),
            terminal_lifecycle: Mutex::new(()),
            engine: Arc::new(Mutex::new(PtyTerminals::new())),
            screens: Arc::new(Mutex::new(BTreeMap::new())),
            scan: Arc::new(Mutex::new(None)),
        }
    }

    /// Open the durable core in the app data directory. A first run creates the
    /// database and schema; a later run marks every live task `interrupted`,
    /// because no agent process survives the previous shutdown.
    pub async fn load(
        data_directory: &Path,
        harness_factory: Box<dyn HarnessFactory>,
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
        let state = Self::new(core);

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
                let drained = {
                    let mut core = core.lock().await;
                    if let Err(error) = core.pump().await {
                        tracing::warn!(%error, "could not fold agent activity");
                    }
                    core.drain_events()
                };
                for event in drained {
                    if let Err(error) = events.send(event) {
                        tracing::debug!(%error, "no shell is subscribed to core events");
                    }
                }
            }
        });
    }

    async fn execute(&self, command: Command) -> Result<CommandResult, BridgeError> {
        let (result, events) = {
            let mut core = self.core.lock().await;
            let result = core.execute(command).await;
            let events = core.drain_events();
            (result, events)
        };

        for event in events {
            if let Err(error) = self.events.send(event) {
                // A window may not have subscribed yet; the snapshot returned by
                // the command is enough to paint a fresh shell.
                tracing::debug!(%error, "no shell is subscribed to core events");
            }
        }

        result.map_err(BridgeError::from)
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
        match self
            .execute(Command::DiscardDraft {
                task_id: TaskId::new(task_id),
            })
            .await?
        {
            CommandResult::Ack => Ok(()),
            other => Err(unexpected(&other)),
        }
    }

    pub async fn send_prompt(
        &self,
        task_id: String,
        text: String,
    ) -> Result<TaskWorkspace, BridgeError> {
        self.workspace(Command::SendPrompt {
            task_id: TaskId::new(task_id),
            text,
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
                let mut engine = std::mem::take(&mut *self.engine.lock().await);
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

    pub async fn codex_project_model(
        &self,
        project_id: String,
    ) -> Result<Option<String>, BridgeError> {
        let Some(program) = &self.codex_program else {
            return Ok(None);
        };
        let directory = self
            .core
            .lock()
            .await
            .project_path(&ProjectId::new(project_id))?;
        agent_harness::codex::project_model(codex_spawn_spec(program, directory))
            .await
            .map_err(|error| BridgeError::new("codex_model", error.to_string()))
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
        let mut core = self.core.lock().await;
        core.touch_archived_worktree(&task_id).await?;
        Ok(core.task_workspace(&task_id)?)
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
            let cwd = core.terminal_directory(&project_id, task_id.as_ref())?;
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
        let state = self.core.lock().await.shutdown_state();
        if state != shepherd_core::ShutdownState::Running {
            return Err(CoreError::NotRunning { state }.into());
        }
        let (columns, rows) = (spec.columns, spec.rows);
        let (output, mut receiver) = mpsc::channel(32);
        let terminal_id = self
            .engine
            .lock()
            .await
            .spawn(spec, output)
            .map_err(|error| BridgeError::new("terminal", error.to_string()))?;

        let screen = Screen::new(usize::from(columns), usize::from(rows));
        let (frames, _) = watch::channel(Arc::new(screen.frame()));
        let slot = Arc::new(Mutex::new(ScreenSlot {
            revision: 0,
            screen,
            frames,
        }));
        self.screens
            .lock()
            .await
            .insert(terminal_id.as_str().to_string(), Arc::clone(&slot));

        if let Some(record) = &record
            && let Err(error) = self.core.lock().await.record_terminal(record).await
        {
            self.screens.lock().await.remove(terminal_id.as_str());
            if let Err(close_error) = self.stop_terminal_process(terminal_id).await {
                tracing::warn!(%close_error, "could not roll back terminal spawn");
            }
            return Err(error.into());
        }

        let engine = Arc::clone(&self.engine);
        let screens = Arc::clone(&self.screens);
        let recorded = record.map(|record| (record, Arc::clone(&self.core), self.events.clone()));
        let running_id = terminal_id.clone();
        let id = terminal_id.as_str().to_string();
        tauri::async_runtime::spawn(async move {
            let mut pending = None;
            let mut deadline = None;
            loop {
                let event = if let Some(event) = pending.take() {
                    Ok(Some(event))
                } else if let Some(deadline) = deadline {
                    tokio::time::timeout_at(
                        tokio::time::Instant::from_std(deadline),
                        receiver.recv(),
                    )
                    .await
                } else {
                    Ok(receiver.recv().await)
                };
                let output = match event {
                    Ok(Some(TerminalEvent::Output(bytes))) => Some(bytes),
                    Ok(None) => break,
                    Err(_) => None,
                    Ok(Some(TerminalEvent::Exited { exit_code })) => {
                        screens.lock().await.remove(&id);
                        if let Some((_, core, shell_events)) = &recorded {
                            let mut core = core.lock().await;
                            if let Err(error) = core.end_terminal(&running_id, exit_code).await {
                                tracing::warn!(%error, "could not mark the terminal ended");
                            }
                            for event in core.drain_events() {
                                if let Err(error) = shell_events.send(event) {
                                    tracing::debug!(%error, "no shell is subscribed to terminal events");
                                }
                            }
                        }
                        break;
                    }
                };
                let (replies, bell) = {
                    let mut slot = slot.lock().await;
                    if let Some(bytes) = output {
                        slot.screen.process(&bytes);
                        for _ in 0..31 {
                            match receiver.try_recv() {
                                Ok(TerminalEvent::Output(bytes)) => slot.screen.process(&bytes),
                                Ok(event) => {
                                    pending = Some(event);
                                    break;
                                }
                                Err(_) => break,
                            }
                        }
                    } else {
                        slot.screen.finish_sync();
                    }
                    deadline = slot.screen.sync_deadline();
                    slot.publish();
                    (slot.screen.take_replies(), slot.screen.take_bell())
                };
                if bell
                    && let Some((record, _, shell_events)) = &recorded
                    && let Err(error) = shell_events.send(Event::TerminalBell {
                        project_id: record.project_id.clone(),
                        task_id: record.task_id.as_ref().map(|id| id.as_str().to_owned()),
                        terminal_id: id.clone(),
                    })
                {
                    tracing::debug!(%error, "no shell is subscribed to terminal bells");
                }
                if !replies.is_empty()
                    && let Err(error) = engine.lock().await.write_reply(&running_id, replies)
                {
                    tracing::debug!(%error, "could not send terminal replies");
                }
            }
        });

        Ok(terminal_id)
    }

    async fn screen_slot(&self, terminal_id: &str) -> Result<Arc<Mutex<ScreenSlot>>, BridgeError> {
        self.screens
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
        let program = match std::env::consts::OS {
            "macos" => "open",
            "linux" => "xdg-open",
            _ => {
                return Err(BridgeError::new(
                    "terminal",
                    "opening terminal links is unavailable",
                ));
            }
        };
        let status = tokio::task::spawn_blocking(move || {
            std::process::Command::new(program)
                .arg(url.as_str())
                .status()
        })
        .await
        .map_err(|error| BridgeError::new("terminal", error.to_string()))?
        .map_err(|error| BridgeError::new("terminal", error.to_string()))?;
        if status.success() {
            Ok(())
        } else {
            Err(BridgeError::new(
                "terminal",
                format!("could not open the terminal link: {status}"),
            ))
        }
    }

    pub async fn terminal_resize(
        &self,
        terminal_id: String,
        columns: u16,
        rows: u16,
    ) -> Result<(), BridgeError> {
        let slot = self.screen_slot(&terminal_id).await?;
        let mut slot = slot.lock().await;
        self.engine
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
        self.stop_terminal_process(TerminalId::new(terminal_id))
            .await
    }

    async fn stop_terminal_process(&self, terminal_id: TerminalId) -> Result<(), BridgeError> {
        let engine = Arc::clone(&self.engine);
        tokio::task::spawn_blocking(move || engine.blocking_lock().close(&terminal_id))
            .await
            .map_err(|error| BridgeError::new("terminal", error.to_string()))?
            .map_err(|error| BridgeError::new("terminal", error.to_string()))
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
    pub async fn add_project(
        state: State<'_, ShellState>,
        path: String,
    ) -> Result<ProjectList, BridgeError> {
        state.inner().add_project(path).await
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
        state.opencode_configured
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
    ) -> Result<TaskWorkspace, BridgeError> {
        state.inner().send_prompt(task_id, text).await
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
    pub async fn codex_project_model(
        state: State<'_, ShellState>,
        project_id: String,
    ) -> Result<Option<String>, BridgeError> {
        state.codex_project_model(project_id).await
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
    use crate::dev_harness::ScriptedHarnessFactory;

    fn state_with(core: Core) -> ShellState {
        ShellState::new(core)
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
        assert!(state.screens.lock().await.contains_key(saved.id.as_str()));

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
    async fn load_opens_the_durable_core_and_survives_a_reopen()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;

        let state = ShellState::load(directory.path(), Box::new(ScriptedHarnessFactory)).await?;
        let projects = state
            .add_project(directory.path().display().to_string())
            .await?;
        assert_eq!(projects.projects.len(), 1);
        assert!(
            directory.join("shepherd.db").is_file(),
            "the shell should create its database in the data directory"
        );
        drop(state);

        let reopened = ShellState::load(directory.path(), Box::new(ScriptedHarnessFactory)).await?;
        let projects = reopened.project_list().await;
        assert_eq!(projects.projects.len(), 1);
        assert!(!projects.projects[0].is_git_repository);
        assert!(!projects.projects[0].name.is_empty());
        Ok(())
    }
}
