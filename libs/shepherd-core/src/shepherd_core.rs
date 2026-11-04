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
pub mod projects;
pub mod snapshots;
pub mod store;
pub mod tasks;
pub mod terminal;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use tokio::sync::mpsc;

pub use commands::{Command, CommandResult, QuitSummary};
pub use events::{Event, ShutdownState};
pub use input::{Focus, InputMode};
pub use projects::{Project, ProjectId, ProjectSnapshot};
pub use snapshots::{ProjectList, TaskList, TaskWorkspace};
pub use tasks::{AgentTier, Task, TaskDetail, TaskId, TaskState, TaskSummary};

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
    #[error("task title cannot be empty")]
    EmptyTitle,
    #[error("task {id} is not a draft")]
    NotADraft { id: String },
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
    #[error("filesystem error at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("agent harness error: {0}")]
    Harness(#[from] agent_harness::HarnessError),
}

/// The core owns all mutable state. It is a library, not a daemon.
pub struct Core {
    projects: BTreeMap<ProjectId, Project>,
    project_order: Vec<ProjectId>,
    tasks: BTreeMap<TaskId, Task>,
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
    /// Create an in-memory core. Persistence is layered on in the store slice.
    pub fn new() -> Self {
        let (events, event_receiver) = mpsc::unbounded_channel();
        Self {
            projects: BTreeMap::new(),
            project_order: Vec::new(),
            tasks: BTreeMap::new(),
            shutdown: ShutdownState::Running,
            events,
            event_receiver,
        }
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
            Command::AddProject { path } => self.add_project(path),
            Command::CreateDraft { project_id, title } => self.create_draft(&project_id, title),
            Command::DiscardDraft { task_id } => self.discard_draft(&task_id),
            Command::Quit { force } => self.quit(force),
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
        })
    }

    fn add_project(&mut self, path: PathBuf) -> Result<CommandResult, CoreError> {
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

    fn create_draft(
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

        let task = Task::draft(project_id.clone(), &title, &project.path);
        let task_id = task.id.clone();
        if let Some(project) = self.projects.get_mut(project_id) {
            project.last_task_id = Some(task_id.clone());
        }
        self.tasks.insert(task_id.clone(), task);

        let workspace = self.task_workspace(&task_id)?;
        self.emit(Event::TaskWorkspaceChanged(workspace.clone()));
        self.emit(Event::TaskListChanged(self.task_list(project_id)?));
        self.emit(Event::ProjectListChanged(self.project_list()));
        Ok(CommandResult::TaskWorkspace(workspace))
    }

    fn discard_draft(&mut self, task_id: &TaskId) -> Result<CommandResult, CoreError> {
        let task = self.task(task_id)?;
        if task.state != TaskState::Draft {
            return Err(CoreError::NotADraft {
                id: task_id.as_str().to_string(),
            });
        }
        let project_id = task.project_id.clone();
        self.tasks.remove(task_id);
        if let Some(project) = self.projects.get_mut(&project_id)
            && project.last_task_id.as_ref() == Some(task_id)
        {
            project.last_task_id = None;
        }

        self.emit(Event::TaskListChanged(self.task_list(&project_id)?));
        self.emit(Event::ProjectListChanged(self.project_list()));
        Ok(CommandResult::Ack)
    }

    fn quit(&mut self, force: bool) -> Result<CommandResult, CoreError> {
        let live_tasks = self
            .tasks
            .values()
            .filter(|task| task.state.is_live())
            .count();
        if live_tasks > 0 && !force {
            return Err(CoreError::QuitConfirmationRequired { live_tasks });
        }

        self.set_shutdown(ShutdownState::Draining);
        let mut interrupted_tasks = 0;
        let mut affected_projects = Vec::new();
        let mut changed_tasks = Vec::new();
        for task in self.tasks.values_mut() {
            if task.state.is_live() {
                if let Err(error) = task.set_state(TaskState::Interrupted) {
                    tracing::error!(
                        task_id = task.id.as_str(),
                        %error,
                        "failed to mark live task interrupted during shutdown"
                    );
                    continue;
                }
                interrupted_tasks += 1;
                if !affected_projects.contains(&task.project_id) {
                    affected_projects.push(task.project_id.clone());
                }
                changed_tasks.push(TaskSummary::from(&*task));
            }
        }
        for summary in changed_tasks {
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
mod tests {
    use super::*;
    use crate::test_support::TestDirectory;

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
        let directory = TestDirectory::new().expect("temp directory");
        let mut core = Core::new();
        let project_id = add_project(&mut core, directory.path().to_path_buf())
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
        assert_eq!(workspace.task.proposed_branch, "shepherd/fix-the-parser");

        let tasks = core.task_list(&project_id).expect("task list");
        assert_eq!(tasks.tasks.len(), 1);
        assert_eq!(workspace.project.unfinished_task_count, 1);
    }

    #[tokio::test]
    async fn discarding_a_draft_removes_it_without_git_state() {
        let directory = TestDirectory::new().expect("temp directory");
        let mut core = Core::new();
        let project_id = add_project(&mut core, directory.path().to_path_buf())
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
}
