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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use agent_harness::Decision;
use serde::Serialize;
use shepherd_core::harness::HarnessFactory;
use shepherd_core::store::TursoStore;
use shepherd_core::terminal::{
    TerminalEvent, TerminalId, TerminalRecord, TerminalService, TerminalSnapshot, TerminalSpec,
};
use shepherd_core::{
    Command, CommandResult, Core, CoreError, Event, ProjectId, ProjectList, QuitSummary, TaskId,
    TaskList, TaskWorkspace,
};
use tokio::sync::{Mutex, broadcast, mpsc, oneshot, watch};

use crate::pty_terminal::PtyTerminals;
use crate::terminal_screen::{Screen, TerminalFrame};

#[derive(Debug, thiserror::Error, Serialize)]
#[error("{message}")]
pub struct BridgeError {
    code: &'static str,
    message: String,
}

impl BridgeError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl From<CoreError> for BridgeError {
    fn from(error: CoreError) -> Self {
        Self::new("core", error.to_string())
    }
}

fn unexpected(result: &CommandResult) -> BridgeError {
    BridgeError::new("bridge", format!("unexpected command result: {result:?}"))
}

struct ScreenSlot {
    screen: Screen,
    frames: watch::Sender<TerminalFrame>,
}

/// The state Tauri manages for the window: one core, one event broadcaster, the
/// PTY engine, one parsed screen per live terminal, and the cached project scan
/// shared with the startup scan task.
pub struct ShellState {
    core: Arc<Mutex<Core>>,
    events: broadcast::Sender<Event>,
    terminal_subscriptions: Arc<Mutex<BTreeMap<String, oneshot::Sender<()>>>>,
    terminal_lifecycle: Mutex<()>,
    engine: Mutex<PtyTerminals>,
    screens: Arc<Mutex<BTreeMap<String, ScreenSlot>>>,
    scan: Arc<Mutex<Option<shepherd_core::project_scan::ProjectScan>>>,
}

impl ShellState {
    pub fn new(core: Core) -> Self {
        let (events, _) = broadcast::channel(512);
        Self {
            core: Arc::new(Mutex::new(core)),
            events,
            terminal_subscriptions: Arc::new(Mutex::new(BTreeMap::new())),
            terminal_lifecycle: Mutex::new(()),
            engine: Mutex::new(PtyTerminals::new()),
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
        if let Some(home) = home_directory() {
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
    ) -> Result<TaskWorkspace, BridgeError> {
        self.workspace(Command::CreateDraft {
            project_id: ProjectId::new(project_id),
            title,
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
        match self.execute(Command::Quit { force }).await? {
            CommandResult::Quit(summary) => Ok(summary),
            other => Err(unexpected(&other)),
        }
    }

    pub async fn project_list(&self) -> ProjectList {
        self.core.lock().await.project_list()
    }

    /// The cached home-directory scan. `rescan` walks the tree again; otherwise
    /// the first call after startup does the work and later calls reuse it.
    pub async fn project_scan(
        &self,
        rescan: bool,
    ) -> Result<shepherd_core::project_scan::ProjectScan, BridgeError> {
        let mut cache = self.scan.lock().await;
        if rescan || cache.is_none() {
            let home = home_directory()
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

    pub async fn terminal_list(&self, project_id: String) -> Vec<TerminalSnapshot> {
        self.core
            .lock()
            .await
            .terminals_for(&ProjectId::new(project_id))
    }

    pub async fn start_terminal(
        &self,
        project_id: String,
        task_id: Option<String>,
        columns: u16,
        rows: u16,
    ) -> Result<Vec<TerminalSnapshot>, BridgeError> {
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
    ) -> Result<Vec<TerminalSnapshot>, BridgeError> {
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

    async fn launch_terminal(
        &self,
        record: TerminalRecord,
        columns: u16,
        rows: u16,
    ) -> Result<Vec<TerminalSnapshot>, BridgeError> {
        let state = self.core.lock().await.shutdown_state();
        if state != shepherd_core::ShutdownState::Running {
            return Err(CoreError::NotRunning { state }.into());
        }
        let project_id = record.project_id.clone();
        let (output, mut receiver) = mpsc::unbounded_channel();
        let terminal_id = self
            .engine
            .lock()
            .await
            .spawn(
                TerminalSpec {
                    id: record.id.clone(),
                    columns,
                    rows,
                    cwd: record.cwd.clone(),
                    command: record.command.clone(),
                    args: Vec::new(),
                    environment: BTreeMap::new(),
                },
                output,
            )
            .map_err(|error| BridgeError::new("terminal", error.to_string()))?;

        let screen = Screen::new(usize::from(columns), usize::from(rows));
        let (frames, _) = watch::channel(screen.frame());
        self.screens.lock().await.insert(
            terminal_id.as_str().to_string(),
            ScreenSlot { screen, frames },
        );

        let saved = self.core.lock().await.record_terminal(&record).await;
        if let Err(error) = saved {
            self.screens.lock().await.remove(terminal_id.as_str());
            if let Err(close_error) = self.engine.lock().await.close(&terminal_id) {
                tracing::warn!(%close_error, "could not roll back terminal spawn");
            }
            return Err(error.into());
        }

        let screens = Arc::clone(&self.screens);
        let core = Arc::clone(&self.core);
        let shell_events = self.events.clone();
        let id = terminal_id.as_str().to_string();
        tauri::async_runtime::spawn(async move {
            while let Some(event) = receiver.recv().await {
                match event {
                    TerminalEvent::Output(bytes) => {
                        let mut screens = screens.lock().await;
                        if let Some(slot) = screens.get_mut(&id) {
                            slot.screen.process(&bytes);
                            slot.frames.send_replace(slot.screen.frame());
                        }
                    }
                    TerminalEvent::Exited { exit_code } => {
                        screens.lock().await.remove(&id);
                        let mut core = core.lock().await;
                        if let Err(error) = core
                            .end_terminal(&TerminalId::new(id.clone()), exit_code)
                            .await
                        {
                            tracing::warn!(%error, "could not mark the terminal ended");
                        }
                        for event in core.drain_events() {
                            if let Err(error) = shell_events.send(event) {
                                tracing::debug!(%error, "no shell is subscribed to terminal events");
                            }
                        }
                        break;
                    }
                }
            }
        });

        Ok(self.core.lock().await.terminals_for(&project_id))
    }

    pub async fn terminal_write(
        &self,
        terminal_id: String,
        data: String,
    ) -> Result<(), BridgeError> {
        self.engine
            .lock()
            .await
            .write(&TerminalId::new(terminal_id), data.as_bytes())
            .map_err(|error| BridgeError::new("terminal", error.to_string()))
    }

    pub async fn terminal_resize(
        &self,
        terminal_id: String,
        columns: u16,
        rows: u16,
    ) -> Result<(), BridgeError> {
        self.engine
            .lock()
            .await
            .resize(&TerminalId::new(terminal_id.clone()), columns, rows)
            .map_err(|error| BridgeError::new("terminal", error.to_string()))?;

        let mut screens = self.screens.lock().await;
        if let Some(slot) = screens.get_mut(&terminal_id) {
            slot.screen.resize(usize::from(columns), usize::from(rows));
            slot.frames.send_replace(slot.screen.frame());
        }
        Ok(())
    }

    pub async fn close_terminal(&self, terminal_id: String) -> Result<(), BridgeError> {
        self.engine
            .lock()
            .await
            .close(&TerminalId::new(terminal_id.clone()))
            .map_err(|error| BridgeError::new("terminal", error.to_string()))
    }

    pub async fn subscribe_terminal(
        &self,
        terminal_id: &str,
        channel: tauri::ipc::Channel<TerminalFrame>,
    ) -> Result<String, BridgeError> {
        let mut receiver = self
            .screens
            .lock()
            .await
            .get(terminal_id)
            .map(|slot| slot.frames.subscribe())
            .ok_or_else(|| {
                BridgeError::new("terminal", format!("no live terminal {terminal_id}"))
            })?;
        let (cancel, mut cancelled) = oneshot::channel();
        let subscription_id = uuid::Uuid::new_v4().to_string();
        self.terminal_subscriptions
            .lock()
            .await
            .insert(subscription_id.clone(), cancel);
        let subscriptions = self.terminal_subscriptions.clone();
        let id = subscription_id.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                let frame = receiver.borrow_and_update().clone();
                if let Err(error) = channel.send(frame) {
                    tracing::debug!(%error, "terminal view detached");
                    break;
                }
                tokio::select! {
                    _ = &mut cancelled => break,
                    result = receiver.changed() => if result.is_err() { break },
                }
            }
            subscriptions.lock().await.remove(&id);
        });
        Ok(subscription_id)
    }

    pub async fn unsubscribe_terminal(&self, subscription_id: &str) {
        self.terminal_subscriptions
            .lock()
            .await
            .remove(subscription_id);
    }
}

fn home_directory() -> Option<PathBuf> {
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
    ) -> Result<TaskWorkspace, BridgeError> {
        state.inner().create_draft(project_id, title).await
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
    pub async fn project_scan(
        state: State<'_, ShellState>,
        rescan: bool,
    ) -> Result<shepherd_core::project_scan::ProjectScan, BridgeError> {
        state.inner().project_scan(rescan).await
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
    pub async fn terminal_list(
        state: State<'_, ShellState>,
        project_id: String,
    ) -> Result<Vec<TerminalSnapshot>, BridgeError> {
        Ok(state.inner().terminal_list(project_id).await)
    }

    #[tauri::command]
    pub async fn start_terminal(
        state: State<'_, ShellState>,
        project_id: String,
        task_id: Option<String>,
        columns: u16,
        rows: u16,
    ) -> Result<Vec<TerminalSnapshot>, BridgeError> {
        state
            .inner()
            .start_terminal(project_id, task_id, columns, rows)
            .await
    }

    #[tauri::command]
    pub async fn restart_terminal(
        state: State<'_, ShellState>,
        terminal_id: String,
        columns: u16,
        rows: u16,
    ) -> Result<Vec<TerminalSnapshot>, BridgeError> {
        state
            .inner()
            .restart_terminal(terminal_id, columns, rows)
            .await
    }

    #[tauri::command]
    pub async fn terminal_write(
        state: State<'_, ShellState>,
        terminal_id: String,
        data: String,
    ) -> Result<(), BridgeError> {
        state.inner().terminal_write(terminal_id, data).await
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
        channel: tauri::ipc::Channel<TerminalFrame>,
    ) -> Result<String, BridgeError> {
        state
            .inner()
            .subscribe_terminal(&terminal_id, channel)
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
            .create_draft(project_id.clone(), "Bridge draft".to_string())
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
    async fn snapshots_serialize_as_tagged_camel_case_json()
    -> Result<(), Box<dyn std::error::Error>> {
        let event = Event::TaskListChanged(TaskList {
            project_id: ProjectId::new("project-1"),
            tasks: vec![TaskSummary {
                id: "task-1".to_string(),
                project_id: "project-1".to_string(),
                title: "Bridge".to_string(),
                state: TaskState::Waiting,
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
            .create_draft(projects.projects[0].id.clone(), "Approval".to_string())
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
        assert!(!projects.projects[0].name.is_empty());
        Ok(())
    }
}
