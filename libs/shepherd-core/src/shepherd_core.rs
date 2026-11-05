//! Shepherd's headless core.
//!
//! Shells send [`Command`] values and receive [`Event`] values. Snapshots are
//! returned by commands so a fresh shell can paint without replaying history.
//! This crate does not depend on Tauri or GPUI.

pub mod commands;
pub mod events;
pub mod git;
pub mod harness;
pub mod input;
pub mod project_scan;
pub mod projects;
pub mod session;
pub mod snapshots;
pub mod store;
pub mod tasks;
pub mod terminal;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use agent_harness::{FailureKind, HarnessEvent, SessionId, TurnFinishReason};
use tokio::sync::mpsc;

pub use commands::{Command, CommandResult, QuitSummary};
pub use events::{Event, ShutdownState};
pub use input::{Focus, InputMode};
pub use project_scan::{ProjectNode, ProjectScan};
pub use projects::{Project, ProjectId, ProjectSnapshot};
pub use snapshots::{ProjectList, TaskList, TaskWorkspace};
pub use tasks::{AgentTier, Task, TaskDetail, TaskId, TaskState, TaskSummary};

use crate::git::{GitError, GitService};
use crate::harness::{HarnessFactory, HarnessSession, UnavailableHarnessFactory};
use crate::session::SessionRecord;
use crate::store::{MemoryStore, Store, StoreError};
use crate::terminal::{TerminalId, TerminalRecord, TerminalSnapshot};

/// A failure reported by the core. Errors are returned to the caller; the core
/// does not turn a failed command into a successful event.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("project path is empty")]
    EmptyProjectPath,
    #[error("project path is already registered: {path}")]
    DuplicateProject { path: String },
    #[error("project not found: {id}")]
    ProjectNotFound { id: String },
    #[error("project path is missing: {path}")]
    ProjectPathMissing { path: String },
    #[error("task not found: {id}")]
    TaskNotFound { id: String },
    #[error("terminal not found: {id}")]
    TerminalNotFound { id: String },
    #[error("task title cannot be empty")]
    EmptyTitle,
    #[error("the prompt is empty")]
    EmptyPrompt,
    #[error("task {id} is not a draft")]
    NotADraft { id: String },
    #[error("task {id} is {state}; it cannot accept a prompt")]
    TaskNotReady { id: String, state: &'static str },
    #[error("task {id} is {state}; it has nothing to resume")]
    TaskNotResumable { id: String, state: &'static str },
    #[error("task {id} is {state}; it cannot be cancelled")]
    TaskNotCancellable { id: String, state: &'static str },
    #[error("task {id} has no pending approval")]
    NoPendingApproval { id: String },
    #[error("approval mismatch for task {id}: expected {expected}, got {actual}")]
    ApprovalMismatch {
        id: String,
        expected: String,
        actual: String,
    },
    #[error("cannot transition task {id} from {from} to {to}")]
    InvalidTransition {
        id: String,
        from: &'static str,
        to: &'static str,
    },
    #[error("quit requires confirmation: {live_tasks} live task(s); retry with force to stop them")]
    QuitConfirmationRequired { live_tasks: usize },
    #[error("the core is not running; shutdown state is {state:?}")]
    NotRunning { state: ShutdownState },
    #[error("the core was built without an agent harness")]
    HarnessUnavailable,
    #[error("no usable agent is installed: {report}")]
    HarnessMissing { report: String },
    #[error("no agent session is open for task {id}")]
    SessionMissing { id: String },
    #[error("filesystem error at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("store error: {0}")]
    Store(#[from] StoreError),
    #[error("Git error: {0}")]
    Git(#[from] GitError),
    #[error("agent harness error: {0}")]
    Harness(#[from] agent_harness::HarnessError),
}

/// What opening an agent session actually did. Resume reports this to the user
/// instead of pretending a stored thread was picked up.
enum SessionOpened {
    Fresh,
    Resumed(String),
    ResumeFailed(String),
}

/// The core owns all mutable state. It is a library, not a daemon.
pub struct Core {
    projects: BTreeMap<ProjectId, Project>,
    project_order: Vec<ProjectId>,
    tasks: BTreeMap<TaskId, Task>,
    sessions: BTreeMap<TaskId, HarnessSession>,
    /// The newest durable session per task, kept across restarts so resume can
    /// ask the adapter for the thread the previous process opened.
    session_records: BTreeMap<TaskId, SessionRecord>,
    terminals: BTreeMap<TerminalId, TerminalRecord>,
    store: Box<dyn Store>,
    harness_factory: Box<dyn HarnessFactory>,
    shutdown: ShutdownState,
    events: mpsc::UnboundedSender<Event>,
    event_receiver: mpsc::UnboundedReceiver<Event>,
}

impl Default for Core {
    fn default() -> Self {
        Self::new()
    }
}

impl Core {
    /// A core with in-memory persistence and no agent adapter. Project and draft
    /// commands work; send and resume report [`CoreError::HarnessUnavailable`].
    pub fn new() -> Self {
        Self::with_services(
            Box::new(MemoryStore::new()),
            Box::new(UnavailableHarnessFactory),
        )
    }

    /// A core bound to a durable store and an adapter factory. Callers that want
    /// to continue a previous run should use [`Core::load`] instead.
    pub fn with_services(store: Box<dyn Store>, harness_factory: Box<dyn HarnessFactory>) -> Self {
        let (events, event_receiver) = mpsc::unbounded_channel();
        Self {
            projects: BTreeMap::new(),
            project_order: Vec::new(),
            tasks: BTreeMap::new(),
            sessions: BTreeMap::new(),
            session_records: BTreeMap::new(),
            terminals: BTreeMap::new(),
            store,
            harness_factory,
            shutdown: ShutdownState::Running,
            events,
            event_receiver,
        }
    }

    /// Open a core from durable records. No agent process survives a restart, so
    /// every task that was live when the previous run ended is loaded as
    /// `interrupted`: resuming starts a new session rather than pretending the
    /// old one is still attached.
    pub async fn load(
        store: Box<dyn Store>,
        harness_factory: Box<dyn HarnessFactory>,
    ) -> Result<Self, CoreError> {
        let mut core = Self::with_services(store, harness_factory);

        let mut projects = core.store.load_projects().await?;
        projects.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        for project in projects {
            core.project_order.push(project.id.clone());
            core.projects.insert(project.id.clone(), project);
        }

        for task in core.store.load_tasks().await? {
            core.tasks.insert(task.id.clone(), task);
        }

        for mut record in core.store.load_sessions().await? {
            // The process that owned this session is gone, so mark it ended while
            // keeping the adapter handle a later resume needs.
            if !record.is_ended() {
                record.end(None, None);
                core.store.save_session(&record).await?;
            }
            let replace = core
                .session_records
                .get(&record.task_id)
                .is_none_or(|seen| seen.started_at <= record.started_at);
            if replace {
                core.session_records.insert(record.task_id.clone(), record);
            }
        }

        let live_tasks: Vec<TaskId> = core
            .tasks
            .values()
            .filter(|task| task.state.is_live())
            .map(|task| task.id.clone())
            .collect();
        for task_id in live_tasks {
            core.set_task_state(&task_id, TaskState::Interrupted)?;
            core.save_task(&task_id).await?;
        }

        for mut terminal in core.store.load_terminals().await? {
            // No PTY outlives the process, so a record without an end time is
            // loaded as ended instead of being shown as still running.
            if terminal.ended_at.is_none() {
                terminal.ended_at = Some(crate::projects::now_millis());
                core.store.save_terminal(&terminal).await?;
            }
            core.terminals.insert(terminal.id.clone(), terminal);
        }

        Ok(core)
    }

    /// Terminal records for one project, oldest first. An ended terminal stays
    /// listed so the shell can show it as ended with its exit code.
    pub fn terminals_for(&self, project_id: &ProjectId) -> Vec<TerminalSnapshot> {
        let mut terminals: Vec<&TerminalRecord> = self
            .terminals
            .values()
            .filter(|terminal| &terminal.project_id == project_id)
            .collect();
        terminals.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        terminals
            .into_iter()
            .map(TerminalRecord::snapshot)
            .collect()
    }

    pub async fn record_terminal(&mut self, terminal: &TerminalRecord) -> Result<(), CoreError> {
        self.store.save_terminal(terminal).await?;
        self.terminals.insert(terminal.id.clone(), terminal.clone());
        self.emit(Event::TerminalListChanged {
            project_id: terminal.project_id.clone(),
            terminals: self.terminals_for(&terminal.project_id),
        });
        Ok(())
    }

    pub async fn end_terminal(
        &mut self,
        terminal_id: &TerminalId,
        exit_code: Option<i32>,
    ) -> Result<(), CoreError> {
        let Some(seen) = self.terminals.get(terminal_id) else {
            return Err(CoreError::TerminalNotFound {
                id: terminal_id.as_str().to_string(),
            });
        };
        if seen.ended_at.is_some() {
            return Ok(());
        }

        let mut terminal = seen.clone();
        terminal.ended_at = Some(crate::projects::now_millis());
        terminal.exit_code = exit_code;
        self.store.save_terminal(&terminal).await?;
        let project_id = terminal.project_id.clone();
        self.terminals.insert(terminal_id.clone(), terminal);
        self.emit(Event::TerminalListChanged {
            project_id: project_id.clone(),
            terminals: self.terminals_for(&project_id),
        });
        Ok(())
    }

    pub fn shutdown_state(&self) -> ShutdownState {
        self.shutdown
    }

    /// Execute one command. A shell should send exactly this enum over its
    /// bridge; it does not get a second copy of the state model.
    pub async fn execute(&mut self, command: Command) -> Result<CommandResult, CoreError> {
        if self.shutdown != ShutdownState::Running {
            return Err(CoreError::NotRunning {
                state: self.shutdown,
            });
        }

        match command {
            Command::AddProject { path } => self.add_project(path).await,
            Command::CreateDraft { project_id, title } => {
                self.create_draft(&project_id, title).await
            }
            Command::DiscardDraft { task_id } => self.discard_draft(&task_id).await,
            Command::SendPrompt { task_id, text } => self.send_prompt(&task_id, text).await,
            Command::ResolveApproval {
                task_id,
                approval_id,
                decision,
            } => self.resolve_approval(&task_id, approval_id, decision).await,
            Command::CancelTask { task_id } => self.cancel_task(&task_id).await,
            Command::ResumeTask { task_id } => self.resume_task(&task_id).await,
            Command::Quit { force } => self.quit(force).await,
        }
    }

    /// Events produced since the last drain. Events are never dropped; the
    /// channel is unbounded because task events must not be coalesced away.
    pub fn drain_events(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        while let Ok(event) = self.event_receiver.try_recv() {
            events.push(event);
        }
        events
    }

    /// Fold adapter activity that arrived since the last command. Without this,
    /// a streaming turn only appears when the user issues another command; the
    /// shell calls it on a short tick. Returns whether anything was folded.
    pub async fn pump(&mut self) -> Result<bool, CoreError> {
        let task_ids: Vec<TaskId> = self.sessions.keys().cloned().collect();
        let mut changed = false;
        for task_id in task_ids {
            let events = match self.sessions.get_mut(&task_id) {
                Some(session) => session.pump(),
                None => continue,
            };
            if events.is_empty() {
                continue;
            }
            let before = self.task(&task_id)?.state;
            self.apply_events(&task_id, &events)?;
            if self.task(&task_id)?.state != before {
                self.save_task(&task_id).await?;
            }
            self.publish_task(&task_id)?;
            changed = true;
        }
        Ok(changed)
    }

    pub fn project_list(&self) -> ProjectList {
        ProjectList {
            projects: self
                .project_order
                .iter()
                .filter_map(|project_id| self.projects.get(project_id))
                .map(|project| self.project_snapshot(project))
                .collect(),
        }
    }

    pub fn task_list(&self, project_id: &ProjectId) -> Result<TaskList, CoreError> {
        if !self.projects.contains_key(project_id) {
            return Err(CoreError::ProjectNotFound {
                id: project_id.as_str().to_string(),
            });
        }

        let mut tasks: Vec<TaskSummary> = self
            .tasks
            .values()
            .filter(|task| &task.project_id == project_id)
            .map(TaskSummary::from)
            .collect();
        tasks.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.id.cmp(&right.id))
        });

        Ok(TaskList {
            project_id: project_id.clone(),
            tasks,
        })
    }

    pub fn task_workspace(&self, task_id: &TaskId) -> Result<TaskWorkspace, CoreError> {
        let task = self.task(task_id)?;
        let project =
            self.projects
                .get(&task.project_id)
                .ok_or_else(|| CoreError::ProjectNotFound {
                    id: task.project_id.as_str().to_string(),
                })?;
        Ok(TaskWorkspace {
            project: self.project_snapshot(project),
            task: TaskDetail::from(task),
            transcript: self
                .sessions
                .get(task_id)
                .map(|session| session.transcript().entries().to_vec())
                .unwrap_or_default(),
            diagnostics: self
                .sessions
                .get(task_id)
                .map(|session| session.diagnostics()),
        })
    }

    async fn add_project(&mut self, path: PathBuf) -> Result<CommandResult, CoreError> {
        if path.as_os_str().is_empty() {
            return Err(CoreError::EmptyProjectPath);
        }
        let normalized = normalize_project_path(&path)?;
        if self
            .projects
            .values()
            .any(|project| project.path == normalized)
        {
            return Err(CoreError::DuplicateProject {
                path: normalized.display().to_string(),
            });
        }

        let project = Project::new(normalized);
        self.store.save_project(&project).await?;
        let project_id = project.id.clone();
        self.projects.insert(project_id.clone(), project);
        self.project_order.push(project_id.clone());

        let projects = self.project_list();
        self.emit(Event::ProjectListChanged(projects.clone()));
        self.emit(Event::TaskListChanged(TaskList {
            project_id,
            tasks: Vec::new(),
        }));
        Ok(CommandResult::ProjectList(projects))
    }

    async fn create_draft(
        &mut self,
        project_id: &ProjectId,
        title: String,
    ) -> Result<CommandResult, CoreError> {
        let title = title.trim().to_string();
        if title.is_empty() {
            return Err(CoreError::EmptyTitle);
        }
        let project = self
            .projects
            .get(project_id)
            .ok_or_else(|| CoreError::ProjectNotFound {
                id: project_id.as_str().to_string(),
            })?;
        if project.missing {
            return Err(CoreError::ProjectPathMissing {
                path: project.path.display().to_string(),
            });
        }

        let git_dir = match GitService::discover(&project.path) {
            Ok(repository) => repository.git_dir,
            Err(error) => {
                // The draft still appears; the first prompt reports the real Git
                // failure instead of refusing to name a task.
                tracing::debug!(
                    project = %project.path.display(),
                    %error,
                    "drafting without a resolvable Git directory"
                );
                project.path.join(".git")
            }
        };
        let task = Task::draft(project_id.clone(), &title, &git_dir);
        let task_id = task.id.clone();
        let mut updated_project = project.clone();
        updated_project.last_task_id = Some(task_id.clone());

        self.store.save_project(&updated_project).await?;
        self.store.save_task(&task).await?;

        self.projects.insert(project_id.clone(), updated_project);
        self.tasks.insert(task_id.clone(), task);

        let workspace = self.task_workspace(&task_id)?;
        self.emit(Event::TaskWorkspaceChanged(workspace.clone()));
        self.emit(Event::TaskListChanged(self.task_list(project_id)?));
        self.emit(Event::ProjectListChanged(self.project_list()));
        Ok(CommandResult::TaskWorkspace(workspace))
    }

    async fn discard_draft(&mut self, task_id: &TaskId) -> Result<CommandResult, CoreError> {
        let task = self.task(task_id)?;
        if task.state != TaskState::Draft {
            return Err(CoreError::NotADraft {
                id: task_id.as_str().to_string(),
            });
        }
        let project_id = task.project_id.clone();
        self.store.delete_task(task_id).await?;
        self.tasks.remove(task_id);
        self.sessions.remove(task_id);

        let mut updated_project = None;
        if let Some(project) = self.projects.get_mut(&project_id)
            && project.last_task_id.as_ref() == Some(task_id)
        {
            project.last_task_id = None;
            updated_project = Some(project.clone());
        }
        if let Some(project) = updated_project {
            self.store.save_project(&project).await?;
        }

        self.emit(Event::TaskListChanged(self.task_list(&project_id)?));
        self.emit(Event::ProjectListChanged(self.project_list()));
        Ok(CommandResult::Ack)
    }

    /// The first prompt creates the worktree and the agent session; later
    /// prompts reuse both.
    async fn send_prompt(
        &mut self,
        task_id: &TaskId,
        text: String,
    ) -> Result<CommandResult, CoreError> {
        let text = text.trim().to_string();
        if text.is_empty() {
            return Err(CoreError::EmptyPrompt);
        }

        match self.task(task_id)?.state {
            TaskState::Draft => self.create_worktree(task_id).await?,
            TaskState::Ready | TaskState::Review => {}
            state => {
                return Err(CoreError::TaskNotReady {
                    id: task_id.as_str().to_string(),
                    state: state.as_str(),
                });
            }
        }

        self.set_task_state(task_id, TaskState::Working)?;
        if let Err(error) = self.ensure_session(task_id, None).await {
            // A session that never started leaves the task ready to retry; the
            // failure row is already in its transcript.
            self.set_task_state(task_id, TaskState::Ready)?;
            self.save_task(task_id).await?;
            self.publish_task(task_id)?;
            return Err(error);
        }

        let outcome = {
            let session =
                self.sessions
                    .get_mut(task_id)
                    .ok_or_else(|| CoreError::SessionMissing {
                        id: task_id.as_str().to_string(),
                    })?;
            session.prompt(text).await
        };
        let events = match outcome {
            Ok(events) => events,
            Err(error) => {
                self.record_failure(task_id, &error);
                // A rejected prompt ends the turn but keeps the session, so the
                // user can sign in and send another prompt.
                self.set_task_state(task_id, TaskState::Ready)?;
                self.save_task(task_id).await?;
                self.publish_task(task_id)?;
                return Err(error);
            }
        };

        self.apply_events(task_id, &events)?;
        self.save_task(task_id).await?;
        let workspace = self.publish_task(task_id)?;
        Ok(CommandResult::TaskWorkspace(workspace))
    }

    async fn resolve_approval(
        &mut self,
        task_id: &TaskId,
        approval_id: agent_harness::ApprovalId,
        decision: agent_harness::Decision,
    ) -> Result<CommandResult, CoreError> {
        let events = {
            let session =
                self.sessions
                    .get_mut(task_id)
                    .ok_or_else(|| CoreError::SessionMissing {
                        id: task_id.as_str().to_string(),
                    })?;
            let pending = session.pending_approval().cloned().ok_or_else(|| {
                CoreError::NoPendingApproval {
                    id: task_id.as_str().to_string(),
                }
            })?;
            if pending != approval_id {
                return Err(CoreError::ApprovalMismatch {
                    id: task_id.as_str().to_string(),
                    expected: pending.as_str().to_string(),
                    actual: approval_id.as_str().to_string(),
                });
            }
            session.respond(approval_id, decision).await
        };
        let events = match events {
            Ok(events) => events,
            Err(error) => {
                self.record_failure(task_id, &error);
                self.set_task_state(task_id, TaskState::Ready)?;
                self.save_task(task_id).await?;
                self.publish_task(task_id)?;
                return Err(error);
            }
        };

        self.apply_events(task_id, &events)?;
        self.save_task(task_id).await?;
        let workspace = self.publish_task(task_id)?;
        Ok(CommandResult::TaskWorkspace(workspace))
    }

    /// Cancel stops the current turn but keeps the session and the worktree:
    /// the user can send a new prompt without paying for a fresh worktree.
    async fn cancel_task(&mut self, task_id: &TaskId) -> Result<CommandResult, CoreError> {
        let state = self.task(task_id)?.state;
        if !state.is_live() && state != TaskState::Review {
            return Err(CoreError::TaskNotCancellable {
                id: task_id.as_str().to_string(),
                state: state.as_str(),
            });
        }

        let events = match self.sessions.get_mut(task_id) {
            Some(session) => session.cancel().await?,
            None => Vec::new(),
        };
        self.apply_events(task_id, &events)?;

        let state = self.task(task_id)?.state;
        if state.is_live() {
            self.set_task_state(task_id, TaskState::Ready)?;
        }
        self.save_task(task_id).await?;
        let workspace = self.publish_task(task_id)?;
        Ok(CommandResult::TaskWorkspace(workspace))
    }

    /// Resume an interrupted task with the agent session the previous process
    /// left behind, and say plainly when that session is gone.
    async fn resume_task(&mut self, task_id: &TaskId) -> Result<CommandResult, CoreError> {
        let state = self.task(task_id)?.state;
        if state != TaskState::Interrupted {
            return Err(CoreError::TaskNotResumable {
                id: task_id.as_str().to_string(),
                state: state.as_str(),
            });
        }

        self.set_task_state(task_id, TaskState::Starting)?;
        self.sessions.remove(task_id);
        let stored_session_id = self
            .session_records
            .get(task_id)
            .and_then(|record| record.adapter_session_id.clone())
            .map(SessionId::new);
        let opened = match self.ensure_session(task_id, stored_session_id).await {
            Ok(opened) => opened,
            Err(error) => {
                // Stay resumable: the user can install or update the agent and
                // try again.
                self.set_task_state(task_id, TaskState::Interrupted)?;
                self.save_task(task_id).await?;
                self.publish_task(task_id)?;
                return Err(error);
            }
        };
        if let Some(session) = self.sessions.get_mut(task_id) {
            match opened {
                SessionOpened::Resumed(session_id) => session.note(
                    format!("resumed the previous agent session {session_id}"),
                    false,
                ),
                SessionOpened::ResumeFailed(error) => session.note(
                    format!(
                        "the previous agent session is gone (the adapter cannot resume it: {error}); started a new session"
                    ),
                    true,
                ),
                SessionOpened::Fresh => session.note("started a new agent session", false),
            }
        }
        self.set_task_state(task_id, TaskState::Ready)?;
        self.save_task(task_id).await?;
        let workspace = self.publish_task(task_id)?;
        Ok(CommandResult::TaskWorkspace(workspace))
    }

    async fn create_worktree(&mut self, task_id: &TaskId) -> Result<(), CoreError> {
        let task = self.task(task_id)?.clone();
        let project_path = self.project_path(&task.project_id)?;
        if task.proposed_worktree_path.exists() {
            return Err(CoreError::Io {
                path: task.proposed_worktree_path.display().to_string(),
                source: std::io::Error::other("task worktree path already exists"),
            });
        }

        let repository = GitService::discover(&project_path)?;
        self.set_task_state(task_id, TaskState::Starting)?;
        if let Err(error) = GitService.create_worktree(
            &repository,
            &task.proposed_worktree_path,
            &task.proposed_branch,
            &repository.default_branch,
        ) {
            self.fail_task(task_id, error.to_string()).await?;
            return Err(CoreError::Git(error));
        }

        let task = self
            .tasks
            .get_mut(task_id)
            .ok_or_else(|| CoreError::TaskNotFound {
                id: task_id.as_str().to_string(),
            })?;
        task.worktree_path = Some(task.proposed_worktree_path.clone());
        task.branch = Some(task.proposed_branch.clone());
        task.base_branch = Some(repository.default_branch);
        task.set_state(TaskState::Ready)?;
        self.save_task(task_id).await
    }

    /// Open an adapter session and record it durably. A resume that the adapter
    /// cannot honour falls back to a fresh session, and the caller reports it.
    async fn ensure_session(
        &mut self,
        task_id: &TaskId,
        resume: Option<SessionId>,
    ) -> Result<SessionOpened, CoreError> {
        let carried = match self.sessions.get(task_id) {
            Some(session) if !session.has_failed() => return Ok(SessionOpened::Fresh),
            Some(_) => self
                .sessions
                .remove(task_id)
                .map(|session| session.transcript().entries().to_vec())
                .unwrap_or_default(),
            None => Vec::new(),
        };

        let task = self.task(task_id)?.clone();
        let cwd = match task.worktree_path.clone() {
            Some(worktree_path) => worktree_path,
            None => self.project_path(&task.project_id)?,
        };
        let spec = self.harness_factory.spawn_spec(cwd);
        let harness = self.harness_factory.create()?;
        let mut session = HarnessSession::new(harness);
        session.restore_transcript(carried);
        let capabilities = match session.start(spec).await {
            Ok(capabilities) => capabilities,
            Err(error) => return Err(self.fail_session_start(task_id, session, error)),
        };
        let opened_session = match resume {
            Some(requested) if capabilities.resume => {
                match session.open_session(Some(requested)).await {
                    Ok(session_id) => {
                        let handle = session_id.as_str().to_string();
                        Ok((SessionOpened::Resumed(handle), session_id))
                    }
                    Err(error) => {
                        tracing::warn!(
                            task_id = task_id.as_str(),
                            %error,
                            "the adapter could not resume the stored session"
                        );
                        // The stored thread is gone; a fresh session is still
                        // better than no session, but the caller reports it.
                        session.open_session(None).await.map(|session_id| {
                            (SessionOpened::ResumeFailed(error.to_string()), session_id)
                        })
                    }
                }
            }
            Some(requested) => {
                let reason = format!("this adapter cannot resume session {}", requested.as_str());
                session
                    .open_session(None)
                    .await
                    .map(|session_id| (SessionOpened::ResumeFailed(reason), session_id))
            }
            None => session
                .open_session(None)
                .await
                .map(|session_id| (SessionOpened::Fresh, session_id)),
        };
        let (opened, session_id) = match opened_session {
            Ok(opened) => opened,
            Err(error) => return Err(self.fail_session_start(task_id, session, error)),
        };

        let record = SessionRecord::started(
            task_id.clone(),
            task.tier.as_str(),
            Some(session_id.as_str().to_string()),
        );
        self.store.save_session(&record).await?;
        self.session_records.insert(task_id.clone(), record);
        self.sessions.insert(task_id.clone(), session);
        Ok(opened)
    }

    /// Fold adapter activity into the task state. The transcript itself is
    /// updated by [`HarnessSession`] as it pumps the same events.
    fn apply_events(&mut self, task_id: &TaskId, events: &[HarnessEvent]) -> Result<(), CoreError> {
        let started_live = self.task(task_id)?.state.is_live();
        let mut next_state = None;
        let mut task_error = None;
        let mut failure = None;

        for event in events {
            match event {
                HarnessEvent::ApprovalRequested { .. } => {
                    next_state = Some(TaskState::Waiting);
                }
                HarnessEvent::TurnFinished { reason } => match reason {
                    TurnFinishReason::Completed => {
                        next_state = Some(self.state_after_completed_turn(task_id)?);
                    }
                    TurnFinishReason::Cancelled => next_state = Some(TaskState::Ready),
                    TurnFinishReason::Failed => {
                        next_state = Some(TaskState::Failed);
                        task_error
                            .get_or_insert_with(|| "the agent reported a failed turn".to_string());
                    }
                    TurnFinishReason::Interrupted => {
                        next_state = Some(TaskState::Interrupted);
                    }
                },
                HarnessEvent::Exited { code, stderr } => {
                    if *code != Some(0) {
                        // Only a turn that was still running when the process
                        // died is a mid-turn exit; quitting is a separate path.
                        if started_live {
                            let tail = stderr.trim().to_string();
                            failure = Some((
                                FailureKind::MidTurnExit,
                                format!("the agent exited with code {code:?}"),
                                (!tail.is_empty()).then_some(tail),
                            ));
                        }
                        next_state = Some(TaskState::Failed);
                        task_error = Some(format!("the agent exited with code {code:?}: {stderr}"));
                    }
                }
                _ => {}
            }
        }

        if let Some((kind, text, detail)) = failure
            && let Some(session) = self.sessions.get_mut(task_id)
        {
            session.push_failure(kind, text, detail);
        }

        if let Some(state) = next_state {
            let task = self
                .tasks
                .get_mut(task_id)
                .ok_or_else(|| CoreError::TaskNotFound {
                    id: task_id.as_str().to_string(),
                })?;
            if task.state != state {
                task.set_state(state)?;
            }
            if let Some(message) = task_error {
                task.error = Some(message);
            }
        }

        Ok(())
    }

    /// A completed turn with real changes enters review; a chat-only turn goes
    /// back to ready. S3 replaces the worktree check with the turn baseline.
    fn state_after_completed_turn(&self, task_id: &TaskId) -> Result<TaskState, CoreError> {
        let task = self.task(task_id)?;
        let Some(worktree_path) = task.worktree_path.as_ref() else {
            return Ok(TaskState::Ready);
        };
        if !worktree_path.exists() {
            return Ok(TaskState::Ready);
        }
        if GitService.has_changes(worktree_path)? {
            Ok(TaskState::Review)
        } else {
            Ok(TaskState::Ready)
        }
    }

    /// Keep a session that failed to start so its failure row stays visible, and
    /// hand the original error back to the caller.
    fn fail_session_start(
        &mut self,
        task_id: &TaskId,
        mut session: HarnessSession,
        error: CoreError,
    ) -> CoreError {
        if let CoreError::Harness(harness_error) = &error
            && let Some(kind) = FailureKind::classify(harness_error)
        {
            session.push_failure(kind, error.to_string(), Some(kind.hint().to_string()));
        }
        self.sessions.insert(task_id.clone(), session);
        error
    }

    /// Turn an adapter error into a visible failure row. Errors about Shepherd's
    /// own state get no row, because the caller already reports them.
    fn record_failure(&mut self, task_id: &TaskId, error: &CoreError) {
        let CoreError::Harness(harness_error) = error else {
            return;
        };
        let Some(kind) = FailureKind::classify(harness_error) else {
            return;
        };
        if let Some(session) = self.sessions.get_mut(task_id) {
            session.push_failure(
                kind,
                harness_error.to_string(),
                Some(kind.hint().to_string()),
            );
        }
    }

    async fn fail_task(&mut self, task_id: &TaskId, message: String) -> Result<(), CoreError> {
        let task = self
            .tasks
            .get_mut(task_id)
            .ok_or_else(|| CoreError::TaskNotFound {
                id: task_id.as_str().to_string(),
            })?;
        task.error = Some(message);
        if task.set_state(TaskState::Failed).is_err() {
            tracing::error!(
                task_id = task_id.as_str(),
                state = task.state.as_str(),
                "could not mark the task failed"
            );
        }
        self.save_task(task_id).await
    }

    /// Close the durable record so the next run can tell this process ended
    /// cleanly rather than still holding the session.
    async fn end_session_record(
        &mut self,
        task_id: &TaskId,
        events: &[HarnessEvent],
    ) -> Result<(), CoreError> {
        let Some(mut record) = self.session_records.remove(task_id) else {
            return Ok(());
        };
        let mut exit_code = None;
        let mut stderr = None;
        for event in events {
            if let HarnessEvent::Exited { code, stderr: tail } = event {
                exit_code = *code;
                stderr = Some(tail.clone());
            }
        }
        record.end(exit_code, stderr);
        self.store.save_session(&record).await?;
        self.session_records.insert(task_id.clone(), record);
        Ok(())
    }

    async fn quit(&mut self, force: bool) -> Result<CommandResult, CoreError> {
        let live_tasks: Vec<TaskId> = self
            .tasks
            .values()
            .filter(|task| task.state.is_live())
            .map(|task| task.id.clone())
            .collect();
        if !live_tasks.is_empty() && !force {
            return Err(CoreError::QuitConfirmationRequired {
                live_tasks: live_tasks.len(),
            });
        }

        self.set_shutdown(ShutdownState::Draining);

        let session_tasks: Vec<TaskId> = self.sessions.keys().cloned().collect();
        for task_id in session_tasks {
            let Some(mut session) = self.sessions.remove(&task_id) else {
                continue;
            };
            let events = match session.shutdown().await {
                Ok(events) => events,
                Err(error) => {
                    tracing::error!(
                        task_id = task_id.as_str(),
                        %error,
                        "failed to stop the agent session during shutdown"
                    );
                    Vec::new()
                }
            };
            self.end_session_record(&task_id, &events).await?;
        }

        let mut interrupted_tasks = 0;
        let mut affected_projects = Vec::new();
        for task_id in live_tasks {
            let (project_id, summary) = {
                let task = self
                    .tasks
                    .get_mut(&task_id)
                    .ok_or_else(|| CoreError::TaskNotFound {
                        id: task_id.as_str().to_string(),
                    })?;
                if let Err(error) = task.set_state(TaskState::Interrupted) {
                    tracing::error!(
                        task_id = task_id.as_str(),
                        %error,
                        "failed to mark live task interrupted during shutdown"
                    );
                    continue;
                }
                (task.project_id.clone(), TaskSummary::from(&*task))
            };

            self.save_task(&task_id).await?;
            interrupted_tasks += 1;
            if !affected_projects.contains(&project_id) {
                affected_projects.push(project_id);
            }
            self.emit(Event::TaskChanged(summary));
        }

        self.set_shutdown(ShutdownState::Terminating);
        self.set_shutdown(ShutdownState::Closed);

        for project_id in affected_projects {
            match self.task_list(&project_id) {
                Ok(tasks) => self.emit(Event::TaskListChanged(tasks)),
                Err(error) => tracing::error!(
                    project_id = project_id.as_str(),
                    %error,
                    "failed to snapshot tasks during shutdown"
                ),
            }
        }

        Ok(CommandResult::Quit(QuitSummary {
            interrupted_tasks,
            live_terminals: 0,
        }))
    }

    fn task(&self, task_id: &TaskId) -> Result<&Task, CoreError> {
        self.tasks
            .get(task_id)
            .ok_or_else(|| CoreError::TaskNotFound {
                id: task_id.as_str().to_string(),
            })
    }

    fn project_path(&self, project_id: &ProjectId) -> Result<PathBuf, CoreError> {
        self.projects
            .get(project_id)
            .map(|project| project.path.clone())
            .ok_or_else(|| CoreError::ProjectNotFound {
                id: project_id.as_str().to_string(),
            })
    }

    fn set_task_state(&mut self, task_id: &TaskId, state: TaskState) -> Result<(), CoreError> {
        let task = self
            .tasks
            .get_mut(task_id)
            .ok_or_else(|| CoreError::TaskNotFound {
                id: task_id.as_str().to_string(),
            })?;
        task.set_state(state)
    }

    /// Takes `&mut self` so the future holds an exclusive borrow rather than
    /// `&Core`. A shared borrow would demand `Core: Sync`, which agent adapters
    /// (only required to be `Send`) cannot promise.
    async fn save_task(&mut self, task_id: &TaskId) -> Result<(), CoreError> {
        let task = self.task(task_id)?.clone();
        self.store.save_task(&task).await?;
        Ok(())
    }

    /// Emit the task row, the task workspace and the project's task list after
    /// any state or transcript change.
    fn publish_task(&mut self, task_id: &TaskId) -> Result<TaskWorkspace, CoreError> {
        let task = self.task(task_id)?.clone();
        let workspace = self.task_workspace(task_id)?;
        let tasks = self.task_list(&task.project_id)?;
        self.emit(Event::TaskChanged(TaskSummary::from(&task)));
        self.emit(Event::TaskWorkspaceChanged(workspace.clone()));
        self.emit(Event::TaskListChanged(tasks));
        Ok(workspace)
    }

    fn project_snapshot(&self, project: &Project) -> ProjectSnapshot {
        let unfinished_task_count = self
            .tasks
            .values()
            .filter(|task| {
                task.project_id == project.id
                    && !matches!(task.state, TaskState::Done | TaskState::Cancelled)
            })
            .count();
        ProjectSnapshot {
            id: project.id.as_str().to_string(),
            path: project.path.display().to_string(),
            name: project.name.clone(),
            missing: project.missing,
            last_task_id: project
                .last_task_id
                .as_ref()
                .map(|task_id| task_id.as_str().to_string()),
            unfinished_task_count,
        }
    }

    fn set_shutdown(&mut self, state: ShutdownState) {
        self.shutdown = state;
        self.emit(Event::ShutdownStateChanged(state));
    }

    fn emit(&self, event: Event) {
        if let Err(error) = self.events.send(event) {
            tracing::debug!(%error, "core event receiver closed");
        }
    }
}

fn normalize_project_path(path: &Path) -> Result<PathBuf, CoreError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|source| CoreError::Io {
                path: path.display().to_string(),
                source,
            })?
            .join(path)
    };

    if absolute.exists() {
        absolute.canonicalize().map_err(|source| CoreError::Io {
            path: absolute.display().to_string(),
            source,
        })
    } else {
        Ok(absolute)
    }
}

#[cfg(test)]
mod live_exit {
    use std::path::PathBuf;
    use std::time::Duration;

    use agent_harness::codex::{CodexHarness, detect};
    use agent_harness::{AgentHarness, SpawnSpec};

    use crate::harness::HarnessFactory;
    use crate::store::TursoStore;
    use crate::test_support::{TestDatabase, TestRepository};
    use crate::{Command, CommandResult, Core, CoreError, ProjectId, TaskId, TaskState};

    /// Starts the real Codex, then kills it with SIGKILL once the test creates
    /// the marker file, so a mid-turn death is reproducible on demand.
    struct KilledCodexFactory {
        wrapper: PathBuf,
        version: Option<String>,
    }

    impl HarnessFactory for KilledCodexFactory {
        fn create(&self) -> Result<Box<dyn AgentHarness>, CoreError> {
            let harness = CodexHarness::new();
            let harness = match &self.version {
                Some(version) => harness.with_version(version.clone()),
                None => harness,
            };
            Ok(Box::new(harness))
        }

        fn spawn_spec(&self, cwd: PathBuf) -> SpawnSpec {
            SpawnSpec {
                program: self.wrapper.to_string_lossy().into_owned(),
                cwd,
                ..SpawnSpec::default()
            }
        }
    }

    #[tokio::test]
    #[ignore]
    async fn a_killed_codex_process_fails_the_task_with_its_stderr_tail()
    -> Result<(), Box<dyn std::error::Error>> {
        let here = std::env::current_dir()?;
        let detection = detect(None, &here).await;
        let Some(install) = &detection.install else {
            eprintln!("skipping the live kill: {}", detection.summary());
            return Ok(());
        };
        eprintln!("{}", detection.summary());

        let repository = TestRepository::new()?;
        let database = TestDatabase::new()?;
        let directory = tempfile::TempDir::new()?;
        let marker = directory.path().join("kill");
        let wrapper = directory.path().join("codex-killed.sh");
        // A backgrounded job gets /dev/null as stdin, so keep the harness pipe
        // on fd 3 and hand it to codex explicitly.
        let script = format!(
            "#!/bin/bash\nexec 3<&0\n{:?} app-server <&3 &\nchild=$!\nwhile [ ! -f {:?} ]; do sleep 0.1; done\nkill -9 \"$child\"\nprintf 'codex app-server killed mid-turn\\n' >&2\nwait \"$child\"\n",
            install.program, marker
        );
        std::fs::write(&wrapper, script)?;
        std::fs::set_permissions(
            &wrapper,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )?;

        let mut core = Core::with_services(
            Box::new(TursoStore::open(&database.uri()).await?),
            Box::new(KilledCodexFactory {
                wrapper,
                version: install.probe.version.clone(),
            }),
        );

        let CommandResult::ProjectList(projects) = core
            .execute(Command::AddProject {
                path: repository.path().to_path_buf(),
            })
            .await?
        else {
            return Err("expected a project list".into());
        };
        let project = projects.projects.first().ok_or("the project is listed")?;
        let CommandResult::TaskWorkspace(draft) = core
            .execute(Command::CreateDraft {
                project_id: ProjectId::new(project.id.clone()),
                title: "Kill a live turn".to_string(),
            })
            .await?
        else {
            return Err("expected a draft".into());
        };
        let task_id = TaskId::new(draft.task.id.clone());
        core.execute(Command::SendPrompt {
            task_id: task_id.clone(),
            text: "Run the shell command `sleep 30` in the repository and wait for it to finish, then say done.".to_string(),
        })
        .await?;

        let mut live = false;
        for _ in 0..900 {
            core.pump().await?;
            let workspace = core.task_workspace(&task_id)?;
            if workspace.transcript.len() > 1 || workspace.task.state == TaskState::Waiting {
                live = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(live, "the turn must stream something before the kill");
        std::fs::write(&marker, b"kill")?;

        let mut failed = None;
        for _ in 0..900 {
            core.pump().await?;
            let workspace = core.task_workspace(&task_id)?;
            if workspace.task.state == TaskState::Failed {
                failed = Some(workspace);
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let workspace = failed.ok_or("the killed process must fail the task")?;

        let error = workspace.task.error.clone().unwrap_or_default();
        assert!(error.contains("exited with code"), "task error: {error}");
        let rows: Vec<String> = workspace
            .transcript
            .iter()
            .map(|entry| entry.summary())
            .collect();
        assert!(
            rows.iter().any(|row| row.starts_with("failure:")),
            "rows: {rows:?}"
        );
        let diagnostics = workspace
            .diagnostics
            .as_ref()
            .ok_or("the workspace carries diagnostics")?;
        assert!(
            diagnostics.stderr_tail.contains("killed mid-turn"),
            "stderr tail: {}",
            diagnostics.stderr_tail
        );
        Ok(())
    }
}

#[cfg(test)]
mod live_restart {
    use std::path::PathBuf;
    use std::time::Duration;

    use agent_harness::codex::{CodexHarness, CodexInstall, detect};
    use agent_harness::{AgentHarness, SpawnSpec};

    use crate::harness::HarnessFactory;
    use crate::store::{Store as _, TursoStore};
    use crate::test_support::{TestDatabase, TestRepository};
    use crate::{Command, CommandResult, Core, CoreError, ProjectId, TaskId, TaskState};

    struct LiveCodexFactory {
        program: PathBuf,
        version: Option<String>,
    }

    impl HarnessFactory for LiveCodexFactory {
        fn create(&self) -> Result<Box<dyn AgentHarness>, CoreError> {
            let harness = CodexHarness::new();
            let harness = match &self.version {
                Some(version) => harness.with_version(version.clone()),
                None => harness,
            };
            Ok(Box::new(harness))
        }

        fn spawn_spec(&self, cwd: PathBuf) -> SpawnSpec {
            SpawnSpec {
                program: self.program.to_string_lossy().into_owned(),
                args: vec!["app-server".to_string()],
                cwd,
                ..SpawnSpec::default()
            }
        }
    }

    #[tokio::test]
    #[ignore]
    async fn a_restarted_core_resumes_the_recorded_thread() -> Result<(), Box<dyn std::error::Error>>
    {
        let here = std::env::current_dir()?;
        let detection = detect(None, &here).await;
        let Some(install) = &detection.install else {
            eprintln!("skipping the live restart: {}", detection.summary());
            return Ok(());
        };
        eprintln!("{}", detection.summary());

        let repository = TestRepository::new()?;
        let database = TestDatabase::new()?;
        let uri = database.uri();
        let factory = |install: &CodexInstall| LiveCodexFactory {
            program: install.program.clone(),
            version: install.probe.version.clone(),
        };

        let mut core = Core::with_services(
            Box::new(TursoStore::open(&uri).await?),
            Box::new(factory(install)),
        );
        let CommandResult::ProjectList(projects) = core
            .execute(Command::AddProject {
                path: repository.path().to_path_buf(),
            })
            .await?
        else {
            return Err("expected a project list".into());
        };
        let project = projects.projects.first().ok_or("the project is listed")?;
        let CommandResult::TaskWorkspace(draft) = core
            .execute(Command::CreateDraft {
                project_id: ProjectId::new(project.id.clone()),
                title: "Restart and resume".to_string(),
            })
            .await?
        else {
            return Err("expected a draft".into());
        };
        let task_id = TaskId::new(draft.task.id.clone());
        core.execute(Command::SendPrompt {
            task_id: task_id.clone(),
            text: "Run the shell command `sleep 30` in the repository, wait for it to finish, then say done.".to_string(),
        })
        .await?;

        let mut live = false;
        for _ in 0..900 {
            core.pump().await?;
            let workspace = core.task_workspace(&task_id)?;
            if workspace.transcript.len() > 1 || workspace.task.state == TaskState::Waiting {
                live = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(live, "the turn must stream before the app quits");

        // Quitting drops the core, which ends the agent process; nothing about
        // the thread survives except what the store recorded.
        drop(core);

        let store = TursoStore::open(&uri).await?;
        let sessions = store.load_sessions().await?;
        let recorded = sessions
            .iter()
            .filter(|session| session.task_id == task_id)
            .max_by_key(|session| session.started_at)
            .and_then(|session| session.adapter_session_id.clone())
            .ok_or("the adapter thread is recorded")?;

        let mut restarted = Core::load(Box::new(store), Box::new(factory(install))).await?;
        let workspace = restarted.task_workspace(&task_id)?;
        assert_eq!(
            workspace.task.state,
            TaskState::Interrupted,
            "a live task loads as interrupted"
        );

        let CommandResult::TaskWorkspace(resumed) = restarted
            .execute(Command::ResumeTask {
                task_id: task_id.clone(),
            })
            .await?
        else {
            return Err("expected a workspace".into());
        };
        assert_eq!(resumed.task.state, TaskState::Ready);
        let rows: Vec<String> = resumed
            .transcript
            .iter()
            .map(|entry| entry.summary())
            .collect();
        let note = format!("resumed the previous agent session {recorded}");
        assert!(rows.iter().any(|row| row.contains(&note)), "rows: {rows:?}");

        restarted
            .execute(Command::SendPrompt {
                task_id: task_id.clone(),
                text: "Reply with the single word: resumed.".to_string(),
            })
            .await?;

        let mut finished = None;
        for _ in 0..900 {
            restarted.pump().await?;
            let workspace = restarted.task_workspace(&task_id)?;
            if matches!(workspace.task.state, TaskState::Ready | TaskState::Review) {
                finished = Some(workspace);
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let workspace = finished.ok_or("the resumed turn must finish")?;
        assert!(
            workspace.task.error.is_none(),
            "task error: {:?}",
            workspace.task.error
        );
        assert!(
            workspace.transcript.len() > resumed.transcript.len(),
            "the resumed turn must add to the transcript"
        );
        Ok(())
    }
}

#[cfg(test)]
mod streaming {
    use std::sync::Mutex;

    use agent_harness::{
        AgentHarness, ApprovalId, Capabilities, Decision, DeltaKind, HarnessEvent, HarnessResult,
        ItemId, PromptInput, SessionId, SpawnSpec, TurnFinishReason,
    };
    use async_trait::async_trait;
    use tokio::sync::mpsc;

    use crate::harness::HarnessFactory;
    use crate::store::TursoStore;
    use crate::test_support::{TestDatabase, TestRepository};
    use crate::{Command, CommandResult, Core, CoreError, Event, ProjectId, TaskId, TaskState};

    struct LateHarnessFactory {
        receiver: Mutex<Option<mpsc::UnboundedReceiver<HarnessEvent>>>,
    }

    impl HarnessFactory for LateHarnessFactory {
        fn create(&self) -> Result<Box<dyn AgentHarness>, CoreError> {
            let receiver = self
                .receiver
                .lock()
                .expect("the test holds the receiver lock")
                .take()
                .expect("the factory creates one harness per test");
            Ok(Box::new(LateHarness { events: receiver }))
        }
    }

    /// Emits nothing on its own; the test sends events after the prompt returns,
    /// the way a real adapter streams them between commands.
    struct LateHarness {
        events: mpsc::UnboundedReceiver<HarnessEvent>,
    }

    #[async_trait]
    impl AgentHarness for LateHarness {
        fn events(&mut self) -> &mut mpsc::UnboundedReceiver<HarnessEvent> {
            &mut self.events
        }

        async fn start(&mut self, _spec: SpawnSpec) -> HarnessResult<Capabilities> {
            Ok(Capabilities::default())
        }

        async fn open_session(&mut self, resume: Option<SessionId>) -> HarnessResult<SessionId> {
            Ok(resume.unwrap_or_else(|| SessionId::new("late-thread")))
        }

        async fn prompt(&mut self, _input: PromptInput) -> HarnessResult<()> {
            Ok(())
        }

        async fn cancel(&mut self) -> HarnessResult<()> {
            Ok(())
        }

        async fn respond(
            &mut self,
            _approval_id: ApprovalId,
            _decision: Decision,
        ) -> HarnessResult<()> {
            Ok(())
        }

        async fn shutdown(&mut self) -> HarnessResult<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn pumping_folds_a_streaming_turn_into_the_workspace()
    -> Result<(), Box<dyn std::error::Error>> {
        let repository = TestRepository::new()?;
        let database = TestDatabase::new()?;
        let (sender, receiver) = mpsc::unbounded_channel();
        let mut core = Core::with_services(
            Box::new(TursoStore::open(&database.uri()).await?),
            Box::new(LateHarnessFactory {
                receiver: Mutex::new(Some(receiver)),
            }),
        );

        let CommandResult::ProjectList(projects) = core
            .execute(Command::AddProject {
                path: repository.path().to_path_buf(),
            })
            .await?
        else {
            return Err("expected a project list".into());
        };
        let project = projects.projects.first().ok_or("the project is listed")?;
        let CommandResult::TaskWorkspace(draft) = core
            .execute(Command::CreateDraft {
                project_id: ProjectId::new(project.id.clone()),
                title: "Stream a turn".to_string(),
            })
            .await?
        else {
            return Err("expected a draft".into());
        };
        let task_id = TaskId::new(draft.task.id.clone());
        let CommandResult::TaskWorkspace(workspace) = core
            .execute(Command::SendPrompt {
                task_id: task_id.clone(),
                text: "go".to_string(),
            })
            .await?
        else {
            return Err("expected a task workspace".into());
        };
        assert_eq!(workspace.task.state, TaskState::Working);
        assert_eq!(workspace.transcript.len(), 1);
        core.drain_events();

        sender.send(HarnessEvent::Delta {
            item_id: ItemId::new("item-1"),
            kind: DeltaKind::Message,
            text: "streamed".to_string(),
        })?;
        sender.send(HarnessEvent::TurnFinished {
            reason: TurnFinishReason::Completed,
        })?;

        assert!(core.pump().await?);

        let workspace = core.task_workspace(&task_id)?;
        assert_eq!(workspace.task.state, TaskState::Ready);
        assert_eq!(workspace.transcript.len(), 2);
        assert_eq!(workspace.transcript[1].summary(), "agent: streamed");
        assert!(
            core.drain_events()
                .iter()
                .any(|event| matches!(event, Event::TaskWorkspaceChanged(_))),
            "the pump must publish the changed workspace"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use agent_harness::fake_harness::{FakeStep, FakeTurn};
    use agent_harness::{ApprovalId, Decision, Entry};

    use super::*;
    use crate::store::TursoStore;
    use crate::test_support::{
        FakeHarnessFactory, TestCore, TestDatabase, TestDirectory, TestRepository,
    };

    async fn add_project(
        core: &mut Core,
        path: PathBuf,
    ) -> Result<ProjectId, Box<dyn std::error::Error>> {
        let result = core.execute(Command::AddProject { path }).await?;
        let CommandResult::ProjectList(projects) = result else {
            return Err("unexpected result".into());
        };
        let project = projects
            .projects
            .first()
            .ok_or("the project list should contain the added project")?;
        Ok(ProjectId::new(project.id.clone()))
    }

    fn workspace_of(result: CommandResult) -> TaskWorkspace {
        match result {
            CommandResult::TaskWorkspace(workspace) => workspace,
            other => panic!("expected a task workspace, got {other:?}"),
        }
    }

    fn summaries(workspace: &TaskWorkspace) -> Vec<String> {
        workspace.transcript.iter().map(Entry::summary).collect()
    }

    fn git_in(repository: &TestRepository, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(repository.path())
            .args(args)
            .output()
            .expect("git should run");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn worktree_count(repository: &TestRepository) -> usize {
        git_in(repository, &["worktree", "list", "--porcelain"])
            .lines()
            .filter(|line| line.starts_with("worktree "))
            .count()
    }

    #[tokio::test]
    async fn adding_a_project_marks_a_missing_path() {
        let directory = TestDirectory::new().expect("temp directory");
        let missing = directory.join("not-there");
        let mut core = Core::new();

        let result = core
            .execute(Command::AddProject {
                path: missing.clone(),
            })
            .await;
        let CommandResult::ProjectList(projects) = result.expect("project should be added") else {
            panic!("expected a project list");
        };

        assert_eq!(projects.projects.len(), 1);
        assert!(projects.projects[0].missing);
        assert_eq!(projects.projects[0].unfinished_task_count, 0);
    }

    #[tokio::test]
    async fn duplicate_project_paths_are_rejected() {
        let directory = TestDirectory::new().expect("temp directory");
        let mut core = Core::new();
        let first = core
            .execute(Command::AddProject {
                path: directory.path().to_path_buf(),
            })
            .await;
        assert!(first.is_ok());

        let second = core
            .execute(Command::AddProject {
                path: directory.path().to_path_buf(),
            })
            .await;
        assert!(matches!(second, Err(CoreError::DuplicateProject { .. })));
    }

    #[tokio::test]
    async fn a_draft_has_no_worktree_or_branch() {
        let repository = TestRepository::new().expect("temp repository");
        let mut core = Core::new();
        let project_id = add_project(&mut core, repository.path().to_path_buf())
            .await
            .expect("project should be added");

        let result = core
            .execute(Command::CreateDraft {
                project_id: project_id.clone(),
                title: "Fix the parser".to_string(),
            })
            .await;
        let CommandResult::TaskWorkspace(workspace) = result.expect("draft should be created")
        else {
            panic!("expected a task workspace");
        };

        assert_eq!(workspace.task.state, TaskState::Draft);
        assert!(workspace.task.worktree_path.is_none());
        assert!(workspace.task.branch.is_none());
        assert!(workspace.transcript.is_empty());
        assert_eq!(workspace.task.proposed_branch, "shepherd/fix-the-parser");
        // The project path is canonicalized when it is added, so compare the
        // tail of the path rather than the whole temp directory.
        assert!(
            PathBuf::from(&workspace.task.proposed_worktree_path)
                .ends_with(".git/shepherd/worktrees/fix-the-parser"),
            "unexpected proposed worktree path: {}",
            workspace.task.proposed_worktree_path
        );

        // Naming the branch and the path must not create either of them.
        assert!(!PathBuf::from(&workspace.task.proposed_worktree_path).exists());
        assert_eq!(git_in(&repository, &["branch", "--list", "shepherd/*"]), "");
        assert_eq!(worktree_count(&repository), 1);
        assert_eq!(git_in(&repository, &["status", "--porcelain"]), "");

        let tasks = core.task_list(&project_id).expect("task list");
        assert_eq!(tasks.tasks.len(), 1);
        assert_eq!(workspace.project.unfinished_task_count, 1);
    }

    #[tokio::test]
    async fn discarding_a_draft_removes_it_without_git_state() {
        let repository = TestRepository::new().expect("temp repository");
        let mut core = Core::new();
        let project_id = add_project(&mut core, repository.path().to_path_buf())
            .await
            .expect("project should be added");
        let result = core
            .execute(Command::CreateDraft {
                project_id: project_id.clone(),
                title: "Temporary".to_string(),
            })
            .await;
        let CommandResult::TaskWorkspace(workspace) = result.expect("draft should be created")
        else {
            panic!("expected a task workspace");
        };
        let proposed_worktree_path = PathBuf::from(&workspace.task.proposed_worktree_path);
        let task_id = TaskId::new(workspace.task.id);

        assert!(
            core.execute(Command::DiscardDraft { task_id })
                .await
                .is_ok()
        );
        assert!(
            core.task_list(&project_id)
                .expect("task list")
                .tasks
                .is_empty()
        );

        // Discarding removes the record and leaves the repository untouched.
        assert!(!proposed_worktree_path.exists());
        assert_eq!(git_in(&repository, &["branch", "--list", "shepherd/*"]), "");
        assert_eq!(worktree_count(&repository), 1);
        assert_eq!(git_in(&repository, &["status", "--porcelain"]), "");
    }

    #[test]
    fn the_task_state_machine_rejects_skipping_states() {
        let directory = TestDirectory::new().expect("temp directory");
        let mut task = Task::draft(ProjectId::new("project-1"), "Title", directory.path());

        assert!(task.set_state(TaskState::Working).is_err());
        assert!(task.set_state(TaskState::Starting).is_ok());
        assert!(task.set_state(TaskState::Ready).is_ok());
        assert!(task.set_state(TaskState::Working).is_ok());
        assert!(task.set_state(TaskState::Waiting).is_ok());
        assert!(task.set_state(TaskState::Ready).is_ok());
        assert!(task.set_state(TaskState::Working).is_ok());
        assert!(task.set_state(TaskState::Waiting).is_ok());
        assert!(task.set_state(TaskState::Review).is_ok());
        assert!(task.set_state(TaskState::Done).is_ok());
        assert!(task.set_state(TaskState::Review).is_ok());
    }

    #[tokio::test]
    async fn quitting_with_live_work_requires_confirmation_then_interrupts() {
        let directory = TestDirectory::new().expect("temp directory");
        let mut core = Core::new();
        let project_id = add_project(&mut core, directory.path().to_path_buf())
            .await
            .expect("project should be added");
        let result = core
            .execute(Command::CreateDraft {
                project_id,
                title: "Live work".to_string(),
            })
            .await;
        let CommandResult::TaskWorkspace(workspace) = result.expect("draft should be created")
        else {
            panic!("expected a task workspace");
        };
        let task_id = TaskId::new(workspace.task.id);
        if let Some(task) = core.tasks.get_mut(&task_id) {
            task.state = TaskState::Working;
        }

        let refused = core.execute(Command::Quit { force: false }).await;
        assert!(matches!(
            refused,
            Err(CoreError::QuitConfirmationRequired { live_tasks: 1 })
        ));
        assert_eq!(core.shutdown_state(), ShutdownState::Running);
        assert_eq!(
            core.tasks.get(&task_id).map(|task| task.state),
            Some(TaskState::Working)
        );

        let result = core.execute(Command::Quit { force: true }).await;
        let CommandResult::Quit(summary) = result.expect("force quit should succeed") else {
            panic!("expected a quit result");
        };
        assert_eq!(summary.interrupted_tasks, 1);
        assert_eq!(core.shutdown_state(), ShutdownState::Closed);
        assert_eq!(
            core.tasks.get(&task_id).map(|task| task.state),
            Some(TaskState::Interrupted)
        );

        let after_shutdown = core
            .execute(Command::AddProject {
                path: directory.path().to_path_buf(),
            })
            .await;
        assert!(matches!(
            after_shutdown,
            Err(CoreError::NotRunning {
                state: ShutdownState::Closed
            })
        ));
    }

    #[tokio::test]
    async fn quitting_idle_work_is_immediate() {
        let mut core = Core::new();
        let result = core.execute(Command::Quit { force: false }).await;
        let CommandResult::Quit(summary) = result.expect("idle quit should succeed") else {
            panic!("expected a quit result");
        };
        assert_eq!(summary.interrupted_tasks, 0);
        assert_eq!(core.shutdown_state(), ShutdownState::Closed);
    }

    #[tokio::test]
    async fn a_task_left_waiting_is_interrupted_by_the_next_load()
    -> Result<(), Box<dyn std::error::Error>> {
        let repository = TestRepository::new()?;
        let database = TestDatabase::new()?;
        let uri = database.uri();

        let first_run = Core::with_services(
            Box::new(TursoStore::open(&uri).await?),
            Box::new(FakeHarnessFactory::new(vec![vec![FakeTurn::new(vec![
                FakeStep::approval("approval-1", "Run tests"),
            ])]])),
        );
        let mut scenario = TestCore::with_core(first_run);
        let project_id = add_project(scenario.core_mut(), repository.path().to_path_buf()).await?;
        let draft = workspace_of(
            scenario
                .execute(Command::CreateDraft {
                    project_id,
                    title: "Survive a crash".to_string(),
                })
                .await?,
        );
        let task_id = TaskId::new(draft.task.id.clone());
        let workspace = workspace_of(
            scenario
                .execute(Command::SendPrompt {
                    task_id: task_id.clone(),
                    text: "go".to_string(),
                })
                .await?,
        );
        assert_eq!(workspace.task.state, TaskState::Waiting);

        // No quit: the process disappears while the task waits for approval.
        drop(scenario);

        let reopened = Core::load(
            Box::new(TursoStore::open(&uri).await?),
            Box::new(UnavailableHarnessFactory),
        )
        .await?;
        let workspace = reopened.task_workspace(&task_id)?;
        // The record survives and is no longer live; the transcript belongs to
        // the session that vanished, so it comes back empty.
        assert_eq!(workspace.task.state, TaskState::Interrupted);
        assert!(workspace.transcript.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn a_fake_task_runs_through_approval_quit_and_resume()
    -> Result<(), Box<dyn std::error::Error>> {
        let repository = TestRepository::new()?;
        let database = TestDatabase::new()?;
        let uri = database.uri();

        let first_run = Core::with_services(
            Box::new(TursoStore::open(&uri).await?),
            // One session script with two turns: the second prompt reuses the
            // same worktree and agent session as the first.
            Box::new(FakeHarnessFactory::new(vec![vec![
                FakeTurn::new(vec![
                    FakeStep::Message("working on it".to_string()),
                    FakeStep::approval("approval-1", "Run tests"),
                    FakeStep::Message("all done".to_string()),
                ]),
                FakeTurn::new(vec![FakeStep::approval("approval-2", "Write the file")]),
            ]])),
        );
        let mut scenario = TestCore::with_core(first_run);

        let project_id = add_project(scenario.core_mut(), repository.path().to_path_buf()).await?;
        let draft = workspace_of(
            scenario
                .execute(Command::CreateDraft {
                    project_id: project_id.clone(),
                    title: "Fix the parser".to_string(),
                })
                .await?,
        );
        assert_eq!(draft.task.state, TaskState::Draft);
        assert!(draft.task.worktree_path.is_none());
        let worktree_path = PathBuf::from(&draft.task.proposed_worktree_path);
        assert!(!worktree_path.exists());
        let task_id = TaskId::new(draft.task.id.clone());

        let workspace = workspace_of(
            scenario
                .execute(Command::SendPrompt {
                    task_id: task_id.clone(),
                    text: "fix it".to_string(),
                })
                .await?,
        );
        assert_eq!(workspace.task.state, TaskState::Waiting);
        assert_eq!(
            workspace.task.branch.as_deref(),
            Some("shepherd/fix-the-parser")
        );
        assert_eq!(workspace.task.base_branch.as_deref(), Some("main"));
        assert!(worktree_path.join("README.md").is_file());
        assert_eq!(
            summaries(&workspace),
            vec![
                "you: fix it".to_string(),
                "agent: working on it".to_string(),
                "approval: Run tests (pending)".to_string(),
            ]
        );
        assert_eq!(
            workspace
                .transcript
                .iter()
                .map(|entry| entry.sequence)
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_eq!(workspace.transcript[0].id, "entry-0");

        let workspace = workspace_of(
            scenario
                .execute(Command::ResolveApproval {
                    task_id: task_id.clone(),
                    approval_id: ApprovalId::new("approval-1"),
                    decision: Decision::Allow,
                })
                .await?,
        );
        assert_eq!(workspace.task.state, TaskState::Ready);
        assert_eq!(
            summaries(&workspace),
            vec![
                "you: fix it".to_string(),
                "agent: working on it".to_string(),
                "approval: Run tests (Allow)".to_string(),
                "agent: all done".to_string(),
            ]
        );

        let workspace = workspace_of(
            scenario
                .execute(Command::SendPrompt {
                    task_id: task_id.clone(),
                    text: "go again".to_string(),
                })
                .await?,
        );
        assert_eq!(workspace.task.state, TaskState::Waiting);

        let refused = scenario.execute(Command::Quit { force: false }).await;
        assert!(matches!(
            refused,
            Err(CoreError::QuitConfirmationRequired { live_tasks: 1 })
        ));
        let result = scenario.execute(Command::Quit { force: true }).await?;
        let CommandResult::Quit(summary) = result else {
            panic!("expected a quit result");
        };
        assert_eq!(summary.interrupted_tasks, 1);
        drop(scenario);

        let reopened = Core::load(
            Box::new(TursoStore::open(&uri).await?),
            Box::new(FakeHarnessFactory::new(vec![vec![FakeTurn::new(vec![
                FakeStep::Message("resumed answer".to_string()),
            ])]])),
        )
        .await?;
        let mut scenario = TestCore::with_core(reopened);

        let workspace = scenario.core().task_workspace(&task_id)?;
        assert_eq!(workspace.task.state, TaskState::Interrupted);
        assert!(workspace.transcript.is_empty());
        assert!(worktree_path.join("README.md").is_file());
        assert_eq!(workspace.project.unfinished_task_count, 1);

        let workspace = workspace_of(
            scenario
                .execute(Command::ResumeTask {
                    task_id: task_id.clone(),
                })
                .await?,
        );
        assert_eq!(workspace.task.state, TaskState::Ready);
        assert_eq!(workspace.transcript.len(), 1);
        assert!(
            workspace.transcript[0]
                .summary()
                .contains("resumed the previous agent session")
        );

        let workspace = workspace_of(
            scenario
                .execute(Command::SendPrompt {
                    task_id: task_id.clone(),
                    text: "continue".to_string(),
                })
                .await?,
        );
        assert_eq!(workspace.task.state, TaskState::Ready);
        assert_eq!(
            workspace.transcript.last().map(Entry::summary),
            Some("agent: resumed answer".to_string())
        );
        Ok(())
    }

    #[tokio::test]
    async fn denying_an_approval_ends_the_turn() -> Result<(), Box<dyn std::error::Error>> {
        let repository = TestRepository::new()?;
        let mut core = Core::with_services(
            Box::new(MemoryStore::new()),
            Box::new(FakeHarnessFactory::new(vec![vec![FakeTurn::new(vec![
                FakeStep::approval("approval-1", "Run tests"),
                FakeStep::Message("must not appear".to_string()),
            ])]])),
        );
        let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
        let draft = workspace_of(
            core.execute(Command::CreateDraft {
                project_id,
                title: "Deny me".to_string(),
            })
            .await?,
        );
        let task_id = TaskId::new(draft.task.id.clone());
        let workspace = workspace_of(
            core.execute(Command::SendPrompt {
                task_id: task_id.clone(),
                text: "go".to_string(),
            })
            .await?,
        );
        assert_eq!(workspace.task.state, TaskState::Waiting);

        let workspace = workspace_of(
            core.execute(Command::ResolveApproval {
                task_id,
                approval_id: ApprovalId::new("approval-1"),
                decision: Decision::Deny,
            })
            .await?,
        );
        assert_eq!(workspace.task.state, TaskState::Ready);
        assert_eq!(
            summaries(&workspace),
            vec![
                "you: go".to_string(),
                "approval: Run tests (Deny)".to_string(),
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn cancelling_keeps_the_worktree_and_the_session()
    -> Result<(), Box<dyn std::error::Error>> {
        let repository = TestRepository::new()?;
        let mut core = Core::with_services(
            Box::new(MemoryStore::new()),
            Box::new(FakeHarnessFactory::new(vec![vec![
                FakeTurn::new(vec![FakeStep::approval("approval-1", "Run tests")]),
                FakeTurn::new(vec![FakeStep::Message("after cancel".to_string())]),
            ]])),
        );

        let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
        let draft = workspace_of(
            core.execute(Command::CreateDraft {
                project_id,
                title: "Cancel me".to_string(),
            })
            .await?,
        );
        let task_id = TaskId::new(draft.task.id.clone());

        let workspace = workspace_of(
            core.execute(Command::SendPrompt {
                task_id: task_id.clone(),
                text: "go".to_string(),
            })
            .await?,
        );
        assert_eq!(workspace.task.state, TaskState::Waiting);
        let worktree_path = PathBuf::from(
            workspace
                .task
                .worktree_path
                .clone()
                .expect("the first send should create a worktree"),
        );

        let workspace = workspace_of(
            core.execute(Command::CancelTask {
                task_id: task_id.clone(),
            })
            .await?,
        );
        assert_eq!(workspace.task.state, TaskState::Ready);
        assert!(workspace.task.worktree_path.is_some());
        assert!(worktree_path.is_dir());
        assert!(summaries(&workspace).contains(&"approval: Run tests (Deny)".to_string()));

        let workspace = workspace_of(
            core.execute(Command::SendPrompt {
                task_id: task_id.clone(),
                text: "try again".to_string(),
            })
            .await?,
        );
        assert_eq!(workspace.task.state, TaskState::Ready);
        assert_eq!(
            workspace.transcript.last().map(Entry::summary),
            Some("agent: after cancel".to_string())
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_completed_turn_with_changes_enters_review() -> Result<(), Box<dyn std::error::Error>>
    {
        let repository = TestRepository::new()?;
        let mut core = Core::with_services(
            Box::new(MemoryStore::new()),
            Box::new(FakeHarnessFactory::new(vec![vec![FakeTurn::new(vec![
                FakeStep::WriteFile {
                    path: PathBuf::from("notes.txt"),
                    contents: "hello\n".to_string(),
                },
            ])]])),
        );

        let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
        let draft = workspace_of(
            core.execute(Command::CreateDraft {
                project_id,
                title: "Change a file".to_string(),
            })
            .await?,
        );
        let task_id = TaskId::new(draft.task.id.clone());

        let workspace = workspace_of(
            core.execute(Command::SendPrompt {
                task_id,
                text: "write it".to_string(),
            })
            .await?,
        );
        assert_eq!(workspace.task.state, TaskState::Review);
        let worktree_path = PathBuf::from(
            workspace
                .task
                .worktree_path
                .expect("a changed turn needs a worktree"),
        );
        assert!(worktree_path.join("notes.txt").is_file());
        assert!(!repository.path().join("notes.txt").exists());
        Ok(())
    }

    #[tokio::test]
    async fn the_walking_skeleton_runs_from_project_to_restart()
    -> Result<(), Box<dyn std::error::Error>> {
        let repository = TestRepository::new()?;
        let database = TestDatabase::new()?;
        let uri = database.uri();

        let first_run = Core::with_services(
            Box::new(TursoStore::open(&uri).await?),
            Box::new(FakeHarnessFactory::new(vec![vec![FakeTurn::new(vec![
                FakeStep::Message("streaming text".to_string()),
                FakeStep::approval("approval-1", "Run tests"),
            ])]])),
        );
        let mut scenario = TestCore::with_core(first_run);

        let project_id = add_project(scenario.core_mut(), repository.path().to_path_buf()).await?;
        let draft = workspace_of(
            scenario
                .execute(Command::CreateDraft {
                    project_id: project_id.clone(),
                    title: "Ship the skeleton".to_string(),
                })
                .await?,
        );
        assert_eq!(draft.task.state, TaskState::Draft);
        assert_eq!(draft.task.proposed_branch, "shepherd/ship-the-skeleton");
        let task_id = TaskId::new(draft.task.id.clone());

        let workspace = workspace_of(
            scenario
                .execute(Command::SendPrompt {
                    task_id: task_id.clone(),
                    text: "start".to_string(),
                })
                .await?,
        );
        assert_eq!(workspace.task.state, TaskState::Waiting);
        assert_eq!(
            summaries(&workspace),
            vec![
                "you: start".to_string(),
                "agent: streaming text".to_string(),
                "approval: Run tests (pending)".to_string(),
            ]
        );
        let worktree_path = PathBuf::from(
            workspace
                .task
                .worktree_path
                .clone()
                .expect("the first prompt creates the worktree"),
        );
        assert!(worktree_path.is_dir());
        assert!(
            !git_in(
                &repository,
                &["branch", "--list", "shepherd/ship-the-skeleton"]
            )
            .is_empty()
        );

        let trace = scenario.trace();
        for expected in [
            "event: ProjectListChanged",
            "event: TaskListChanged",
            "event: TaskChanged",
            "event: TaskWorkspaceChanged",
        ] {
            assert!(
                trace.contains(expected),
                "{expected} missing from the stream:\n{trace}"
            );
        }

        let refused = scenario.execute(Command::Quit { force: false }).await;
        assert!(matches!(
            refused,
            Err(CoreError::QuitConfirmationRequired { live_tasks: 1 })
        ));
        let result = scenario.execute(Command::Quit { force: true }).await?;
        let CommandResult::Quit(summary) = result else {
            panic!("expected a quit result");
        };
        assert_eq!(summary.interrupted_tasks, 1);
        assert_eq!(summary.live_terminals, 0);
        assert_eq!(scenario.core().shutdown_state(), ShutdownState::Closed);
        drop(scenario);

        let reopened = Core::load(
            Box::new(TursoStore::open(&uri).await?),
            Box::new(FakeHarnessFactory::new(vec![vec![FakeTurn::new(vec![
                FakeStep::Message("after restart".to_string()),
            ])]])),
        )
        .await?;
        let mut scenario = TestCore::with_core(reopened);

        let projects = scenario.core().project_list();
        assert_eq!(projects.projects.len(), 1);
        assert_eq!(projects.projects[0].unfinished_task_count, 1);

        let workspace = scenario.core().task_workspace(&task_id)?;
        assert_eq!(workspace.task.state, TaskState::Interrupted);
        assert_eq!(
            workspace.task.branch.as_deref(),
            Some("shepherd/ship-the-skeleton")
        );

        let workspace = workspace_of(scenario.execute(Command::ResumeTask { task_id }).await?);
        assert_eq!(workspace.task.state, TaskState::Ready);
        assert!(
            workspace.transcript[0]
                .summary()
                .contains("resumed the previous agent session"),
            "unexpected resume note: {:?}",
            workspace.transcript[0].summary()
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_terminal_record_is_ended_after_a_restart() -> Result<(), Box<dyn std::error::Error>>
    {
        let directory = TestDirectory::new()?;
        let database = directory.join("shepherd.db");
        let store = crate::store::TursoStore::open_at(&database).await?;
        let mut core = Core::with_services(Box::new(store), Box::new(UnavailableHarnessFactory));
        let project_id = add_project(&mut core, directory.path().to_path_buf()).await?;

        let terminal = TerminalRecord::project_shared(
            TerminalId::new("terminal-1"),
            project_id.clone(),
            "Terminal 1".to_string(),
            directory.path().to_path_buf(),
            "/bin/zsh".to_string(),
            1_000,
        );
        core.record_terminal(&terminal).await?;
        drop(core);

        let store = crate::store::TursoStore::open_at(&database).await?;
        let reloaded = Core::load(Box::new(store), Box::new(UnavailableHarnessFactory)).await?;
        let terminals = reloaded.terminals_for(&project_id);
        assert_eq!(terminals.len(), 1);
        assert!(terminals[0].ended_at.is_some());
        assert_eq!(terminals[0].exit_code, None);
        Ok(())
    }

    #[tokio::test]
    async fn the_agent_session_survives_a_restart() -> Result<(), Box<dyn std::error::Error>> {
        let repository = TestRepository::new()?;
        let database = TestDatabase::new()?;
        let uri = database.uri();

        let first_run = Core::with_services(
            Box::new(TursoStore::open(&uri).await?),
            // A turn waiting on approval stays live, so quitting interrupts it
            // and the restart has something to resume.
            Box::new(FakeHarnessFactory::new(vec![vec![FakeTurn::new(vec![
                FakeStep::approval("approval-1", "Run tests"),
            ])]])),
        );
        let mut scenario = TestCore::with_core(first_run);
        let project_id = add_project(scenario.core_mut(), repository.path().to_path_buf()).await?;
        let draft = workspace_of(
            scenario
                .execute(Command::CreateDraft {
                    project_id,
                    title: "Resume me".to_string(),
                })
                .await?,
        );
        let task_id = TaskId::new(draft.task.id.clone());
        scenario
            .execute(Command::SendPrompt {
                task_id: task_id.clone(),
                text: "go".to_string(),
            })
            .await?;
        scenario.execute(Command::Quit { force: true }).await?;
        drop(scenario);

        let reopened = Core::load(
            Box::new(TursoStore::open(&uri).await?),
            Box::new(FakeHarnessFactory::new(vec![vec![FakeTurn::new(vec![
                FakeStep::Message("second answer".to_string()),
            ])]])),
        )
        .await?;
        let mut scenario = TestCore::with_core(reopened);

        let workspace = scenario.core().task_workspace(&task_id)?;
        assert_eq!(workspace.task.state, TaskState::Interrupted);

        let workspace = workspace_of(
            scenario
                .execute(Command::ResumeTask {
                    task_id: task_id.clone(),
                })
                .await?,
        );
        assert_eq!(workspace.task.state, TaskState::Ready);
        assert!(
            workspace.transcript[0]
                .summary()
                .contains("resumed the previous agent session fake-session-1"),
            "unexpected resume note: {:?}",
            workspace.transcript[0].summary()
        );

        let sessions = TursoStore::open(&uri).await?.load_sessions().await?;
        assert_eq!(sessions.len(), 2, "each session start writes its own row");
        assert!(sessions.iter().all(|session| {
            session.task_id == task_id
                && session.adapter_session_id.as_deref() == Some("fake-session-1")
        }));
        assert!(sessions[0].is_ended(), "quit should end the first record");
        assert!(!sessions[1].is_ended(), "the resumed session is still live");
        Ok(())
    }

    #[tokio::test]
    async fn a_gone_agent_session_falls_back_with_a_clear_note()
    -> Result<(), Box<dyn std::error::Error>> {
        let repository = TestRepository::new()?;
        let database = TestDatabase::new()?;
        let uri = database.uri();

        let first_run = Core::with_services(
            Box::new(TursoStore::open(&uri).await?),
            // A turn waiting on approval stays live, so quitting interrupts it
            // and the restart has something to resume.
            Box::new(FakeHarnessFactory::new(vec![vec![FakeTurn::new(vec![
                FakeStep::approval("approval-1", "Run tests"),
            ])]])),
        );
        let mut scenario = TestCore::with_core(first_run);
        let project_id = add_project(scenario.core_mut(), repository.path().to_path_buf()).await?;
        let draft = workspace_of(
            scenario
                .execute(Command::CreateDraft {
                    project_id,
                    title: "Resume me".to_string(),
                })
                .await?,
        );
        let task_id = TaskId::new(draft.task.id.clone());
        scenario
            .execute(Command::SendPrompt {
                task_id: task_id.clone(),
                text: "go".to_string(),
            })
            .await?;
        scenario.execute(Command::Quit { force: true }).await?;
        drop(scenario);

        let reopened = Core::load(
            Box::new(TursoStore::open(&uri).await?),
            Box::new(FakeHarnessFactory::refusing_resume(vec![vec![
                FakeTurn::new(vec![FakeStep::Message("second answer".to_string())]),
            ]])),
        )
        .await?;
        let mut scenario = TestCore::with_core(reopened);

        let workspace = workspace_of(scenario.execute(Command::ResumeTask { task_id }).await?);
        assert_eq!(workspace.task.state, TaskState::Ready);
        let note = workspace.transcript[0].summary();
        assert!(note.contains("cannot resume"), "unexpected note: {note}");
        assert!(
            note.contains("started a new session"),
            "unexpected note: {note}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_missing_binary_leaves_a_failure_row_and_a_ready_task()
    -> Result<(), Box<dyn std::error::Error>> {
        let repository = TestRepository::new()?;
        let mut scenario = TestCore::with_core(Core::with_services(
            Box::new(MemoryStore::new()),
            Box::new(FakeHarnessFactory::failing_start(
                agent_harness::fake_harness::FakeFailure::MissingProgram,
            )),
        ));
        let project_id = add_project(scenario.core_mut(), repository.path().to_path_buf()).await?;
        let draft = workspace_of(
            scenario
                .execute(Command::CreateDraft {
                    project_id,
                    title: "Install me".to_string(),
                })
                .await?,
        );
        let task_id = TaskId::new(draft.task.id.clone());

        let result = scenario
            .execute(Command::SendPrompt {
                task_id: task_id.clone(),
                text: "go".to_string(),
            })
            .await;
        assert!(matches!(
            result,
            Err(CoreError::Harness(
                agent_harness::HarnessError::MissingProgram { .. }
            ))
        ));

        let workspace = scenario.core().task_workspace(&task_id)?;
        assert_eq!(workspace.task.state, TaskState::Ready);
        assert!(
            workspace.transcript[0]
                .summary()
                .contains("failure: agent not installed"),
            "unexpected row: {:?}",
            workspace.transcript[0].summary()
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_rejected_prompt_shows_a_sign_in_failure() -> Result<(), Box<dyn std::error::Error>> {
        let repository = TestRepository::new()?;
        let mut scenario = TestCore::with_core(Core::with_services(
            Box::new(MemoryStore::new()),
            Box::new(FakeHarnessFactory::failing_prompt(
                agent_harness::fake_harness::FakeFailure::AuthRequired,
            )),
        ));
        let project_id = add_project(scenario.core_mut(), repository.path().to_path_buf()).await?;
        let draft = workspace_of(
            scenario
                .execute(Command::CreateDraft {
                    project_id,
                    title: "Sign in".to_string(),
                })
                .await?,
        );
        let task_id = TaskId::new(draft.task.id.clone());

        let result = scenario
            .execute(Command::SendPrompt {
                task_id: task_id.clone(),
                text: "go".to_string(),
            })
            .await;
        assert!(matches!(result, Err(CoreError::Harness(_))));

        let workspace = scenario.core().task_workspace(&task_id)?;
        assert_eq!(workspace.task.state, TaskState::Ready);
        assert_eq!(
            workspace.transcript.first().map(Entry::summary),
            Some("you: go".to_string())
        );
        assert!(
            workspace
                .transcript
                .last()
                .is_some_and(|entry| entry.summary().contains("failure: sign-in required")),
            "unexpected transcript: {:?}",
            workspace
                .transcript
                .iter()
                .map(Entry::summary)
                .collect::<Vec<_>>()
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_mid_turn_exit_is_a_failure_row_and_a_failed_task()
    -> Result<(), Box<dyn std::error::Error>> {
        let repository = TestRepository::new()?;
        let mut scenario = TestCore::with_core(Core::with_services(
            Box::new(MemoryStore::new()),
            Box::new(FakeHarnessFactory::new(vec![vec![FakeTurn::new(vec![
                FakeStep::Exit {
                    code: 3,
                    stderr: "the agent crashed".to_string(),
                },
            ])]])),
        ));
        let project_id = add_project(scenario.core_mut(), repository.path().to_path_buf()).await?;
        let draft = workspace_of(
            scenario
                .execute(Command::CreateDraft {
                    project_id,
                    title: "Crash me".to_string(),
                })
                .await?,
        );
        let task_id = TaskId::new(draft.task.id.clone());

        let workspace = workspace_of(
            scenario
                .execute(Command::SendPrompt {
                    task_id,
                    text: "go".to_string(),
                })
                .await?,
        );
        assert_eq!(workspace.task.state, TaskState::Failed);
        assert!(
            workspace
                .transcript
                .last()
                .is_some_and(|entry| entry.summary().contains("failure: agent exited mid-turn")),
            "unexpected transcript: {:?}",
            workspace
                .transcript
                .iter()
                .map(Entry::summary)
                .collect::<Vec<_>>()
        );
        Ok(())
    }

    #[tokio::test]
    async fn diagnostics_name_the_command_and_protocol() -> Result<(), Box<dyn std::error::Error>> {
        let repository = TestRepository::new()?;
        let mut core = Core::with_services(
            Box::new(MemoryStore::new()),
            Box::new(FakeHarnessFactory::new(vec![vec![FakeTurn::default()]])),
        );
        let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
        let draft = workspace_of(
            core.execute(Command::CreateDraft {
                project_id,
                title: "Diagnose".to_string(),
            })
            .await?,
        );
        assert!(
            draft.diagnostics.is_none(),
            "a draft has no session to report"
        );

        let workspace = workspace_of(
            core.execute(Command::SendPrompt {
                task_id: TaskId::new(draft.task.id.clone()),
                text: "go".to_string(),
            })
            .await?,
        );
        let diagnostics = workspace
            .diagnostics
            .expect("a running session reports diagnostics");
        assert_eq!(diagnostics.command, "fake");
        assert_eq!(diagnostics.protocol, "fake-harness");
        assert!(diagnostics.stderr_tail.is_empty());
        Ok(())
    }
}
