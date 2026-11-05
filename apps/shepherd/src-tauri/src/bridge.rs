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
use tokio::sync::{Mutex, broadcast, mpsc};

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
    frames: broadcast::Sender<TerminalFrame>,
}

/// The state Tauri manages for the window: one core, one event broadcaster, the
/// PTY engine, one parsed screen per live terminal, and the cached project scan
/// shared with the startup scan task.
pub struct ShellState {
    core: Arc<Mutex<Core>>,
    events: broadcast::Sender<Event>,
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
        self.core
            .lock()
            .await
            .task_workspace(&TaskId::new(task_id))
            .map_err(BridgeError::from)
    }

    pub async fn terminal_list(&self, project_id: String) -> Vec<TerminalSnapshot> {
        self.core
            .lock()
            .await
            .terminals_for(&ProjectId::new(project_id))
    }

    /// Spawn one PTY in the project's directory, record it, and start parsing
    /// its output. The returned list includes the new live terminal.
    pub async fn start_terminal(
        &self,
        project_id: String,
        columns: u16,
        rows: u16,
    ) -> Result<Vec<TerminalSnapshot>, BridgeError> {
        let project_id = ProjectId::new(project_id);
        let project = {
            let core = self.core.lock().await;
            core.project_list()
                .projects
                .into_iter()
                .find(|project| project.id == project_id.as_str())
        }
        .ok_or_else(|| BridgeError::new("project", "project not found"))?;

        let cwd = PathBuf::from(&project.path);
        let command = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
        let (output, mut receiver) = mpsc::unbounded_channel();
        let terminal_id = self
            .engine
            .lock()
            .await
            .spawn(
                TerminalSpec {
                    cwd: cwd.clone(),
                    command: command.clone(),
                    args: Vec::new(),
                    environment: BTreeMap::new(),
                },
                output,
            )
            .map_err(|error| BridgeError::new("terminal", error.to_string()))?;

        let title = {
            let core = self.core.lock().await;
            format!("Terminal {}", core.terminals_for(&project_id).len() + 1)
        };
        let record = TerminalRecord::project_shared(
            terminal_id.clone(),
            project_id.clone(),
            title,
            cwd,
            command,
            now_millis(),
        );
        {
            let mut core = self.core.lock().await;
            core.record_terminal(&record)
                .await
                .map_err(BridgeError::from)?;
        }

        let (frames, _) = broadcast::channel(64);
        self.screens.lock().await.insert(
            terminal_id.as_str().to_string(),
            ScreenSlot {
                screen: Screen::new(usize::from(columns), usize::from(rows)),
                frames: frames.clone(),
            },
        );

        let screens = Arc::clone(&self.screens);
        let core = Arc::clone(&self.core);
        let id = terminal_id.as_str().to_string();
        tauri::async_runtime::spawn(async move {
            while let Some(event) = receiver.recv().await {
                match event {
                    TerminalEvent::Output(bytes) => {
                        let mut screens = screens.lock().await;
                        if let Some(slot) = screens.get_mut(&id) {
                            slot.screen.process(&bytes);
                            let frame = slot.screen.frame();
                            if let Err(error) = slot.frames.send(frame) {
                                tracing::debug!(%error, "no terminal view is attached");
                            }
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
            let frame = slot.screen.frame();
            if let Err(error) = slot.frames.send(frame) {
                tracing::debug!(%error, "no terminal view is attached");
            }
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

    pub fn subscribe_terminal(
        &self,
        terminal_id: &str,
    ) -> Result<broadcast::Receiver<TerminalFrame>, BridgeError> {
        let screens = self
            .screens
            .try_lock()
            .map_err(|_| BridgeError::new("terminal", "the terminal table is busy; retry"))?;
        screens
            .get(terminal_id)
            .map(|slot| slot.frames.subscribe())
            .ok_or_else(|| BridgeError::new("terminal", format!("no live terminal {terminal_id}")))
    }

    /// A snapshot of the visible grid, so a fresh view paints before the next
    /// output arrives.
    pub async fn terminal_frame(&self, terminal_id: &str) -> Option<TerminalFrame> {
        self.screens
            .lock()
            .await
            .get(terminal_id)
            .map(|slot| slot.screen.frame())
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
            .mutate_git(&TaskId::new(task_id), mutation)?)
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
        columns: u16,
        rows: u16,
    ) -> Result<Vec<TerminalSnapshot>, BridgeError> {
        state
            .inner()
            .start_terminal(project_id, columns, rows)
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
    pub async fn terminal_frame(
        state: State<'_, ShellState>,
        terminal_id: String,
    ) -> Result<Option<TerminalFrame>, BridgeError> {
        Ok(state.inner().terminal_frame(&terminal_id).await)
    }

    #[tauri::command]
    pub async fn subscribe_terminal(
        state: State<'_, ShellState>,
        terminal_id: String,
        channel: tauri::ipc::Channel<TerminalFrame>,
    ) -> Result<(), BridgeError> {
        let mut receiver = state.inner().subscribe_terminal(&terminal_id)?;
        tauri::async_runtime::spawn(async move {
            loop {
                match receiver.recv().await {
                    Ok(frame) => {
                        if let Err(error) = channel.send(frame) {
                            tracing::debug!(%error, "the terminal view closed");
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(skipped, "the terminal view missed frames");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
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
