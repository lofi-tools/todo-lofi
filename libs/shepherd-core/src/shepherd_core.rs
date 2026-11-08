//! Shepherd's headless core.
//!
//! Shells send [`Command`] values and receive [`Event`] values. Snapshots are
//! returned by commands so a fresh shell can paint without replaying history.
//! This crate does not depend on Tauri or GPUI.

pub mod commands;
pub mod events;
pub mod files;
pub mod git;
pub mod harness;
pub mod input;
pub mod project_scan;
pub mod projects;
pub mod search;
pub mod session;
pub mod snapshots;
pub mod store;
pub mod tasks;
pub mod terminal;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use agent_harness::{
    Decision, FailureKind, HarnessEvent, PromptAttachment, PromptInput, SessionId, Transcript,
    TurnFinishReason,
};
use tokio::sync::mpsc;

pub use commands::{Command, CommandResult, QuitSummary, ReviewAction};
pub use events::{Event, ShutdownState};
pub use input::{Focus, InputMode};
pub use project_scan::{ProjectNode, ProjectScan};
pub use projects::{Project, ProjectId, ProjectSnapshot};
pub use snapshots::{
    ProjectApprovalsSnapshot, ProjectList, TaskList, TaskWorkspace, WorkspaceLayout,
};
pub use tasks::{Agent, AgentTier, Task, TaskDetail, TaskId, TaskState, TaskSummary};

use crate::git::{GitError, GitService, TurnBaseline};
use crate::harness::{HarnessFactory, HarnessSession, ProjectApprovals, UnavailableHarnessFactory};
use crate::session::SessionRecord;
use crate::snapshots::{SessionRecovery, TaskApproval};
use crate::store::{MemoryStore, Store, StoreError, WorktreeDeletion};
use crate::terminal::{
    InspectorTab, ProcessRecord, TerminalId, TerminalLayout, TerminalList, TerminalRecord,
    TerminalSnapshot,
};

/// A failure reported by the core. Errors are returned to the caller; the core
/// does not turn a failed command into a successful event.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("project path is empty")]
    EmptyProjectPath,
    #[error("project path is already registered: {path}")]
    DuplicateProject { path: String },
    #[error(
        "Enter a loopback HTTP or HTTPS URL without credentials, such as http://localhost:3000."
    )]
    InvalidPreviewUrl,
    #[error("project not found: {id}")]
    ProjectNotFound { id: String },
    #[error("project path is missing: {path}")]
    ProjectPathMissing { path: String },
    #[error("task not found: {id}")]
    TaskNotFound { id: String },
    #[error("the terminal layout contains invalid or duplicate panes")]
    InvalidTerminalLayout,
    #[error("the terminal layout changed; reload it before editing")]
    TerminalLayoutChanged,
    #[error("the inspector width must be between 240 and 1600 pixels")]
    InvalidWorkspaceLayout,
    #[error("terminal not found: {id}")]
    TerminalNotFound { id: String },
    #[error("task title cannot be empty")]
    EmptyTitle,
    #[error("the prompt is empty")]
    EmptyPrompt,
    #[error("Codex model and effort options require a Codex task.")]
    CodexOptionsForOtherAgent,
    #[error("Could not resolve mentioned file {path}. {source}")]
    FileMention {
        path: String,
        source: files::FileError,
    },
    #[error("task {id} is not a draft")]
    NotADraft { id: String },
    #[error("Close the task's running terminals before deleting it.")]
    TaskTerminalsRunning,
    #[error("Task {id} is {state}; stop its active turn before deleting it.")]
    TaskDeletionBusy { id: String, state: &'static str },
    #[error("task {id} is {state}; it cannot accept a prompt")]
    TaskNotReady { id: String, state: &'static str },
    #[error("task {id} is {state}; it has nothing to resume")]
    TaskNotResumable { id: String, state: &'static str },
    #[error("task {id} is {state}; only archived tasks can be reopened")]
    TaskNotArchived { id: String, state: &'static str },
    #[error("task {id} has no recorded branch for worktree collection")]
    CollectionBranchMissing { id: String },
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
    #[error("task {id} already has a queued review note")]
    ReviewNoteAlreadyQueued { id: String },
    #[error("task {id} has no queued review note")]
    ReviewNoteNotQueued { id: String },
    #[error("cannot transition task {id} from {from} to {to}")]
    InvalidTransition {
        id: String,
        from: &'static str,
        to: &'static str,
    },
    #[error("quit requires confirmation: {live_tasks} live task(s); retry with force to stop them")]
    QuitConfirmationRequired {
        live_tasks: usize,
        live_terminals: usize,
    },
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
    #[error("could not recover worktree deletion for task {task_id}: {reason}")]
    WorktreeDeletionRecovery { task_id: String, reason: String },
    #[error("task {task_id} was deleted, but staged worktree cleanup remains pending: {reason}")]
    TaskDeletedCleanupPending { task_id: String, reason: String },
    #[error("could not save the agent session: {store}; could not stop the agent: {cleanup}")]
    SessionRecordCleanup { store: String, cleanup: String },
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

struct QueuedReviewNote {
    text: String,
    ready: bool,
}

/// The core owns all mutable state. It is a library, not a daemon.
pub struct Core {
    projects: BTreeMap<ProjectId, Project>,
    project_order: Vec<ProjectId>,
    tasks: BTreeMap<TaskId, Task>,
    workspace_layouts: BTreeMap<TaskId, WorkspaceLayout>,
    sessions: BTreeMap<TaskId, HarnessSession>,
    detached_transcripts: BTreeMap<TaskId, Transcript>,
    turn_baselines: BTreeMap<TaskId, TurnBaseline>,
    review_notes: BTreeMap<TaskId, QueuedReviewNote>,
    /// The newest durable session per task, kept across restarts so resume can
    /// ask the adapter for the thread the previous process opened.
    session_records: BTreeMap<TaskId, SessionRecord>,
    terminal_layouts: BTreeMap<ProjectId, TerminalLayout>,
    terminals: BTreeMap<TerminalId, TerminalRecord>,
    terminal_list_revision: u64,
    archived_worktree_issues: Vec<ArchivedWorktreeIssue>,
    store: Arc<dyn Store>,
    harness_factory: Box<dyn HarnessFactory>,
    shutdown: ShutdownState,
    shutdown_events: BTreeMap<TaskId, Vec<HarnessEvent>>,
    shutdown_interrupted: BTreeSet<TaskId>,
    events: mpsc::UnboundedSender<Event>,
    event_receiver: mpsc::UnboundedReceiver<Event>,
}

pub struct TaskGitRead {
    project_path: PathBuf,
    worktree_path: PathBuf,
    task_id: String,
    state: &'static str,
    base_branch: Option<String>,
    branch: Option<String>,
}

impl TaskGitRead {
    fn repository(&self) -> Result<git::GitRepository, CoreError> {
        let repository = GitService::discover(&self.project_path)?;
        GitService.validate_worktree(&repository, &self.worktree_path)?;
        Ok(repository)
    }

    fn base_branch(&self) -> Result<&str, CoreError> {
        self.base_branch
            .as_deref()
            .ok_or_else(|| CoreError::TaskNotReady {
                id: self.task_id.clone(),
                state: self.state,
            })
    }

    pub fn review_snapshot(&self) -> Result<git::ReviewSnapshot, CoreError> {
        self.repository()?;
        Ok(GitService.review(&self.worktree_path, self.base_branch()?)?)
    }

    pub fn review_file_diff(&self, file: &str) -> Result<String, CoreError> {
        self.repository()?;
        Ok(GitService.review_file_diff(&self.worktree_path, self.base_branch()?, file)?)
    }

    pub fn git_file_diff(&self, file: &str) -> Result<git::GitFileDiff, CoreError> {
        self.repository()?;
        Ok(GitService.file_diff(&self.worktree_path, file)?)
    }

    pub fn git_snapshot(&self) -> Result<git::GitSnapshot, CoreError> {
        let repository = self.repository()?;
        let mut snapshot = GitService.snapshot(&self.worktree_path)?;
        snapshot.project_root = Some(GitService.checkout_status(&repository.root)?);
        snapshot.merge_blocked = match (&self.base_branch, &self.branch) {
            (Some(base), Some(branch)) => {
                GitService.merge_blocked(&repository.root, &self.worktree_path, base, branch)?
            }
            _ => Some("The task has no recorded branch.".into()),
        };
        Ok(snapshot)
    }
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchivedWorktreeIssue {
    pub task_id: String,
    pub title: String,
    pub path: String,
    pub reason: String,
}

impl Default for Core {
    fn default() -> Self {
        Self::new()
    }
}

fn archived_worktree_cutoff() -> u64 {
    // Terminal input timestamps are coalesced for up to a minute.
    const ARCHIVE_AGE_MILLIS: u64 = 30 * 24 * 60 * 60 * 1000 + 60_000;
    projects::now_millis().saturating_sub(ARCHIVE_AGE_MILLIS)
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
            workspace_layouts: BTreeMap::new(),
            sessions: BTreeMap::new(),
            detached_transcripts: BTreeMap::new(),
            turn_baselines: BTreeMap::new(),
            review_notes: BTreeMap::new(),
            session_records: BTreeMap::new(),
            terminal_layouts: BTreeMap::new(),
            terminals: BTreeMap::new(),
            terminal_list_revision: 0,
            archived_worktree_issues: Vec::new(),
            store: Arc::from(store),
            harness_factory,
            shutdown: ShutdownState::Running,
            shutdown_events: BTreeMap::new(),
            shutdown_interrupted: BTreeSet::new(),
            events,
            event_receiver,
        }
    }

    /// Open durable records. A previously live task is loaded as `interrupted`
    /// because the new process cannot assume its agent is still attached.
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

        for (task, layout) in core.store.load_workspace_layouts().await? {
            if core.tasks.contains_key(&task) && layout.is_valid() {
                core.workspace_layouts.insert(task, layout);
            } else {
                tracing::warn!(%task, "ignored invalid workspace layout");
            }
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
            // An old PTY is not attached to this instance, even if its process
            // survived the previous instance.
            if terminal.ended_at.is_none() {
                terminal.ended_at = Some(crate::projects::now_millis());
                core.store.save_terminal(&terminal).await?;
            }
            core.terminals.insert(terminal.id.clone(), terminal);
        }

        for (project, layout) in core.store.load_terminal_layouts().await? {
            if core.valid_terminal_layout(&project, &layout, |_| true) {
                core.terminal_layouts.insert(project, layout);
            } else {
                tracing::warn!(%project, "ignored invalid terminal layout");
            }
        }
        core.recover_worktree_deletions().await?;
        core.collect_archived_worktrees().await;
        Ok(core)
    }

    async fn recover_worktree_deletions(&mut self) -> Result<(), CoreError> {
        for deletion in self.store.load_worktree_deletions().await? {
            self.recover_worktree_deletion(&deletion)
                .await
                .map_err(|error| CoreError::WorktreeDeletionRecovery {
                    task_id: deletion.task_id.to_string(),
                    reason: error.to_string(),
                })?;
        }
        Ok(())
    }

    async fn recover_worktree_deletion(
        &mut self,
        deletion: &WorktreeDeletion,
    ) -> Result<(), CoreError> {
        let repository = GitService::discover(&deletion.repository_root)?;
        let _lock = GitService.lock_worktree_deletions(&repository)?;
        self.recover_worktree_deletion_locked(deletion).await
    }

    async fn recover_worktree_deletion_locked(
        &mut self,
        deletion: &WorktreeDeletion,
    ) -> Result<(), CoreError> {
        let invalid = || CoreError::WorktreeDeletionRecovery {
            task_id: deletion.task_id.to_string(),
            reason: "the recorded paths or worktree identity are inconsistent".into(),
        };
        let identity = uuid::Uuid::parse_str(&deletion.identity).map_err(|_| invalid())?;
        if identity.to_string() != deletion.identity {
            return Err(invalid());
        }
        let repository = GitService::discover(&deletion.repository_root)?;
        let worktrees = repository.git_dir.join("shepherd/worktrees");
        if repository.root != deletion.repository_root
            || deletion.original_path.parent() != Some(worktrees.as_path())
            || deletion.staged_path != worktrees.join(format!(".shepherd-deletion-{identity}"))
            || deletion.original_path == deletion.staged_path
            || deletion.worktree_git_dir.parent()
                != Some(repository.git_dir.join("worktrees").as_path())
        {
            return Err(invalid());
        }
        if let Some(task) = self.tasks.get(&deletion.task_id) {
            if task.worktree_path.as_ref() != Some(&deletion.original_path)
                || GitService::discover(&self.project_path(&task.project_id)?)?.root
                    != repository.root
            {
                return Err(invalid());
            }
        } else if !self.projects.values().any(|project| {
            GitService::discover(&project.path)
                .is_ok_and(|candidate| candidate.root == repository.root)
        }) {
            return Err(invalid());
        }
        let exists = |path: &Path| -> Result<bool, CoreError> {
            match path.symlink_metadata() {
                Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(true),
                Ok(_) => Err(invalid()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(source) => Err(CoreError::Io {
                    path: path.display().to_string(),
                    source,
                }),
            }
        };
        let original = exists(&deletion.original_path)?;
        let staged = exists(&deletion.staged_path)?;
        match (self.tasks.contains_key(&deletion.task_id), original, staged) {
            (true, original, staged) if original != staged => {
                let current = if original {
                    &deletion.original_path
                } else {
                    &deletion.staged_path
                };
                GitService.verify_worktree_deletion(
                    &repository,
                    current,
                    &deletion.worktree_git_dir,
                    &deletion.identity,
                )?;
                if staged {
                    GitService.move_worktree(&repository, current, &deletion.original_path)?;
                }
                self.store
                    .remove_worktree_deletion(&deletion.task_id)
                    .await?;
                GitService.clear_worktree_deletion_marker(
                    &repository,
                    &deletion.original_path,
                    &deletion.worktree_git_dir,
                    &deletion.identity,
                )?;
            }
            (false, false, true) => {
                GitService.verify_worktree_deletion(
                    &repository,
                    &deletion.staged_path,
                    &deletion.worktree_git_dir,
                    &deletion.identity,
                )?;
                GitService.remove_clean_worktree(&repository, &deletion.staged_path)?;
                self.store
                    .remove_worktree_deletion(&deletion.task_id)
                    .await?;
            }
            (false, false, false) if !exists(&deletion.worktree_git_dir)? => {
                self.store
                    .remove_worktree_deletion(&deletion.task_id)
                    .await?;
            }
            _ => return Err(invalid()),
        }
        Ok(())
    }

    async fn collect_archived_worktrees(&mut self) {
        let cutoff = archived_worktree_cutoff();
        let candidates: Vec<TaskId> = self
            .tasks
            .values()
            .filter(|task| {
                task.state == TaskState::Done
                    && task.worktree_path.is_some()
                    && (task.collection_pending
                        || (task.updated_at <= cutoff && task.worktree_last_used_at <= cutoff))
            })
            .map(|task| task.id.clone())
            .collect();
        for task_id in candidates {
            if let Err(error) = self.collect_archived_worktree(&task_id).await {
                tracing::info!(%task_id, %error, "archived worktree collection needs attention");
                if let Some(task) = self.tasks.get(&task_id) {
                    self.archived_worktree_issues.push(ArchivedWorktreeIssue {
                        task_id: task_id.to_string(),
                        title: task.title.clone(),
                        path: task
                            .worktree_path
                            .as_ref()
                            .unwrap_or(&task.proposed_worktree_path)
                            .display()
                            .to_string(),
                        reason: error.to_string(),
                    });
                }
            }
        }
    }

    pub fn archived_worktree_issues(&self) -> &[ArchivedWorktreeIssue] {
        &self.archived_worktree_issues
    }

    async fn collect_archived_worktree(&mut self, task_id: &TaskId) -> Result<(), CoreError> {
        let task = self.task(task_id)?.clone();
        if task.collection_pending
            && let Some(path) = &task.worktree_path
        {
            match path.symlink_metadata() {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    let repository = GitService::discover(&self.project_path(&task.project_id)?)?;
                    if path != &task.proposed_worktree_path
                        || path.parent()
                            != Some(repository.git_dir.join("shepherd/worktrees").as_path())
                    {
                        return Err(GitError::InvalidWorktree { path: path.clone() }.into());
                    }
                    let tip = task.branch_tip_sha.as_deref().ok_or_else(|| {
                        CoreError::CollectionBranchMissing {
                            id: task_id.to_string(),
                        }
                    })?;
                    let reference = format!("refs/shepherd/tasks/{}", task_id.as_str());
                    if git::run_git(&repository.root, &["rev-parse", "--verify", &reference])?
                        != tip
                    {
                        return Err(CoreError::Io {
                            path: path.display().to_string(),
                            source: std::io::Error::other(
                                "retained task tip does not match the interrupted collection",
                            ),
                        });
                    }
                    GitService.prune_worktrees(&repository)?;
                    let mut updated = task;
                    updated.worktree_path = None;
                    updated.collection_pending = false;
                    self.store.save_task(&updated).await?;
                    self.tasks.insert(task_id.clone(), updated);
                    return Ok(());
                }
                Err(error) => {
                    return Err(CoreError::Io {
                        path: path.display().to_string(),
                        source: error,
                    });
                }
                Ok(_) => {
                    self.validate_pending_collection_worktree(task_id)?;
                    let mut updated = task.clone();
                    updated.collection_pending = false;
                    self.store.save_task(&updated).await?;
                    self.tasks.insert(task_id.clone(), updated);
                    let cutoff = archived_worktree_cutoff();
                    if task.updated_at > cutoff || task.worktree_last_used_at > cutoff {
                        return Ok(());
                    }
                }
            }
        }
        let (task, repository, path) = self.task_git_worktree(task_id)?;
        let mut updated = task.clone();
        let Some(branch) = task.branch.as_deref() else {
            return Err(CoreError::CollectionBranchMissing {
                id: task_id.to_string(),
            });
        };
        if GitService.has_changes(path)? {
            return Err(GitError::DirtyWorktree {
                path: path.to_path_buf(),
            }
            .into());
        }
        let tip = GitService.checked_out_tip(path, branch)?;
        GitService.retain_task_tip(&repository, task_id.as_str(), &tip)?;
        updated.branch_tip_sha = Some(tip);
        updated.collection_pending = true;
        self.store.save_task(&updated).await?;
        self.tasks.insert(task_id.clone(), updated.clone());
        let Some(path) = updated.worktree_path.as_deref() else {
            return Ok(());
        };
        if let Err(error) = GitService.remove_archived_worktree(&repository, path) {
            match path.symlink_metadata() {
                Ok(_) => {
                    updated.collection_pending = false;
                    if let Err(save_error) = self.store.save_task(&updated).await {
                        return Err(CoreError::Io {
                            path: path.display().to_string(),
                            source: std::io::Error::other(format!(
                                "collection failed: {error}; could not clear collection state: {save_error}"
                            )),
                        });
                    }
                    self.tasks.insert(task_id.clone(), updated);
                }
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(CoreError::Io {
                        path: path.display().to_string(),
                        source: std::io::Error::other(format!(
                            "collection failed: {error}; could not inspect the checkout: {source}"
                        )),
                    });
                }
            }
            return Err(error.into());
        }
        GitService.prune_worktrees(&repository)?;
        updated.worktree_path = None;
        updated.collection_pending = false;
        self.store.save_task(&updated).await?;
        self.tasks.insert(task_id.clone(), updated);
        Ok(())
    }

    fn validate_pending_collection_worktree(&self, task_id: &TaskId) -> Result<(), CoreError> {
        let (task, _, path) = self.task_git_worktree(task_id)?;
        if path != task.proposed_worktree_path {
            return Err(GitError::InvalidWorktree {
                path: path.to_path_buf(),
            }
            .into());
        }
        let branch = task
            .branch
            .as_deref()
            .ok_or_else(|| CoreError::CollectionBranchMissing {
                id: task_id.to_string(),
            })?;
        GitService.checked_out_tip(path, branch)?;
        Ok(())
    }

    pub async fn touch_archived_worktree(&mut self, task_id: &TaskId) -> Result<(), CoreError> {
        let task = self.task(task_id)?;
        if task.state == TaskState::Done
            && task.collection_pending
            && let Some(path) = &task.worktree_path
        {
            match path.symlink_metadata() {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return self.collect_archived_worktree(task_id).await;
                }
                Err(source) => {
                    return Err(CoreError::Io {
                        path: path.display().to_string(),
                        source,
                    });
                }
                Ok(_) => {
                    self.validate_pending_collection_worktree(task_id)?;
                }
            }
        }
        if task.state == TaskState::Done && task.worktree_path.is_some() {
            let mut updated = task.clone();
            updated.worktree_last_used_at = projects::now_millis();
            updated.collection_pending = false;
            self.store.save_task(&updated).await?;
            self.tasks.insert(task_id.clone(), updated);
        }
        Ok(())
    }

    pub async fn touch_archived_worktree_if_due(
        &mut self,
        task_id: &TaskId,
    ) -> Result<(), CoreError> {
        let task = self.task(task_id)?;
        if task.state == TaskState::Done
            && task.worktree_path.is_some()
            && (task.collection_pending
                || projects::now_millis().saturating_sub(task.worktree_last_used_at) >= 60_000)
        {
            self.touch_archived_worktree(task_id).await?;
        }
        Ok(())
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

    pub fn terminal_list(&self, project_id: &ProjectId) -> TerminalList {
        TerminalList {
            project_id: project_id.clone(),
            revision: self.terminal_list_revision,
            terminals: self.terminals_for(project_id),
        }
    }

    pub fn workspace_directory(
        &self,
        project_id: &ProjectId,
        task_id: Option<&TaskId>,
    ) -> Result<PathBuf, CoreError> {
        let root = self.project_path(project_id)?;
        let Some(task_id) = task_id else {
            return Ok(root);
        };
        let task = self.task(task_id)?;
        if &task.project_id != project_id {
            return Err(CoreError::TaskNotFound {
                id: task_id.to_string(),
            });
        }
        let (_, _, path) = self.task_git_worktree(task_id)?;
        Ok(path.to_path_buf())
    }

    fn valid_terminal_layout(
        &self,
        project_id: &ProjectId,
        layout: &TerminalLayout,
        allow_missing: impl Fn(&str) -> bool,
    ) -> bool {
        self.projects.contains_key(project_id)
            && layout.root.as_ref().is_none_or(|root| {
                root.validate(|id| match self.terminals.get(&TerminalId::new(id)) {
                    Some(terminal) => &terminal.project_id == project_id,
                    None => allow_missing(id),
                })
            })
    }

    pub fn terminal_layout(&self, project_id: &ProjectId) -> Result<TerminalLayout, CoreError> {
        if !self.projects.contains_key(project_id) {
            return Err(CoreError::ProjectNotFound {
                id: project_id.to_string(),
            });
        }
        Ok(self
            .terminal_layouts
            .get(project_id)
            .cloned()
            .unwrap_or_default())
    }

    pub async fn save_terminal_layout(
        &mut self,
        project_id: &ProjectId,
        mut layout: TerminalLayout,
    ) -> Result<TerminalLayout, CoreError> {
        if self.shutdown != ShutdownState::Running {
            return Err(CoreError::NotRunning {
                state: self.shutdown,
            });
        }
        let current = self
            .terminal_layouts
            .get(project_id)
            .cloned()
            .unwrap_or_default();
        if !self.valid_terminal_layout(project_id, &layout, |id| {
            current.root.as_ref().is_some_and(|root| root.contains(id))
        }) {
            return Err(CoreError::InvalidTerminalLayout);
        }
        if current.revision != layout.revision {
            return Err(CoreError::TerminalLayoutChanged);
        }
        layout.inspector_tab = current.inspector_tab;
        layout.revision = layout
            .revision
            .checked_add(1)
            .ok_or(CoreError::TerminalLayoutChanged)?;
        self.persist_terminal_layout(project_id, &layout).await?;
        Ok(layout)
    }

    pub async fn save_inspector_tab(
        &mut self,
        project_id: &ProjectId,
        tab: InspectorTab,
    ) -> Result<(), CoreError> {
        if self.shutdown != ShutdownState::Running {
            return Err(CoreError::NotRunning {
                state: self.shutdown,
            });
        }
        let mut layout = self.terminal_layout(project_id)?;
        if layout.inspector_tab == tab {
            return Ok(());
        }
        // Tab changes leave the split revision available for in-flight split edits.
        layout.inspector_tab = tab;
        self.persist_terminal_layout(project_id, &layout).await
    }

    async fn persist_terminal_layout(
        &mut self,
        project_id: &ProjectId,
        layout: &TerminalLayout,
    ) -> Result<(), CoreError> {
        self.store.save_terminal_layout(project_id, layout).await?;
        self.terminal_layouts
            .insert(project_id.clone(), layout.clone());
        Ok(())
    }

    pub fn terminal_record(&self, id: &TerminalId) -> Result<TerminalRecord, CoreError> {
        self.terminals
            .get(id)
            .cloned()
            .ok_or_else(|| CoreError::TerminalNotFound {
                id: id.as_str().into(),
            })
    }

    pub async fn record_process(&mut self, process: &ProcessRecord) -> Result<(), CoreError> {
        self.store.save_process(process).await?;
        Ok(())
    }

    pub fn process_store(&self) -> Arc<dyn Store> {
        self.store.clone()
    }

    pub async fn remove_process(&mut self, id: &str) -> Result<(), CoreError> {
        self.store.remove_process(id).await?;
        Ok(())
    }

    pub async fn recorded_processes(&mut self) -> Result<Vec<ProcessRecord>, CoreError> {
        Ok(self.store.load_processes().await?)
    }

    pub async fn record_terminal(&mut self, terminal: &TerminalRecord) -> Result<(), CoreError> {
        self.store.save_terminal(terminal).await?;
        self.terminals.insert(terminal.id.clone(), terminal.clone());
        self.publish_terminal_list(&terminal.project_id);
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
        self.publish_terminal_list(&project_id);
        Ok(())
    }

    fn publish_terminal_list(&mut self, project_id: &ProjectId) {
        self.terminal_list_revision += 1;
        self.emit(Event::TerminalListChanged(self.terminal_list(project_id)));
    }

    pub fn shutdown_state(&self) -> ShutdownState {
        self.shutdown
    }

    pub async fn project_approvals(
        &mut self,
        project: &ProjectId,
    ) -> Result<ProjectApprovalsSnapshot, CoreError> {
        self.project_path(project)?;
        let saved = self.store.project_approvals(project).await?;
        let mut sessions = Vec::new();
        for (task_id, session) in &self.sessions {
            let Some(task) = self
                .tasks
                .get(task_id)
                .filter(|task| task.project_id == *project)
            else {
                continue;
            };
            sessions.extend(
                session
                    .session_approvals()
                    .into_iter()
                    .map(|approval| TaskApproval {
                        task_id: task_id.to_string(),
                        task_title: task.title.clone(),
                        agent: task.agent,
                        approval,
                    }),
            );
        }
        Ok(ProjectApprovalsSnapshot { saved, sessions })
    }

    pub async fn reset_approval_grant(
        &mut self,
        project: &ProjectId,
        id: &str,
        task_id: Option<&TaskId>,
    ) -> Result<(), CoreError> {
        self.project_path(project)?;
        if let Some(task_id) = task_id {
            if self.task(task_id)?.project_id != *project {
                return Err(CoreError::TaskNotFound {
                    id: task_id.to_string(),
                });
            }
            if let Some(session) = self.sessions.get_mut(task_id) {
                session.reset_session_approval(id).await?;
            }
        } else {
            self.store.reset_project_approval(project, id).await?;
        }
        self.emit(Event::ProjectApprovalsChanged {
            project_id: project.clone(),
        });
        Ok(())
    }

    /// Execute one command. A shell should send exactly this enum over its
    /// bridge; it does not get a second copy of the state model. Git mutations
    /// run here under exclusive access to the core.
    pub async fn execute(&mut self, command: Command) -> Result<CommandResult, CoreError> {
        if self.shutdown != ShutdownState::Running
            && !(self.shutdown == ShutdownState::Draining
                && matches!(&command, Command::Quit { .. }))
        {
            return Err(CoreError::NotRunning {
                state: self.shutdown,
            });
        }

        match command {
            Command::AddProject { path } => self.add_project(path).await,
            Command::SetProjectPreviewUrl { project_id, url } => {
                self.set_project_preview_url(project_id, &url).await
            }
            Command::CreateDraft {
                project_id,
                title,
                agent,
            } => self.create_draft(&project_id, title, agent).await,
            Command::SetDraftAgent { task_id, agent } => {
                self.set_draft_agent(&task_id, agent).await
            }
            Command::DiscardDraft { task_id } => self.discard_draft(&task_id).await,
            Command::DeleteTask {
                task_id,
                remove_worktree,
            } => self.delete_task(&task_id, remove_worktree).await,
            Command::SendPrompt { task_id, input } => self.send_prompt(&task_id, input).await,
            Command::SetSessionMode { task_id, mode } => {
                self.set_session_mode(&task_id, &mode).await
            }
            Command::ResolveApproval {
                task_id,
                approval_id,
                decision,
            } => self.resolve_approval(&task_id, approval_id, decision).await,
            Command::CancelTask { task_id } => self.cancel_task(&task_id).await,
            Command::ResumeTask { task_id } => self.resume_task(&task_id).await,
            Command::ReviewTask { task_id, action } => self.review_task(&task_id, action).await,
            Command::ArchiveReviewed { task_ids } => self.archive_reviewed(task_ids).await,
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
    /// shell calls it when an agent event wakes it. Returns whether anything was folded.
    pub async fn pump(&mut self) -> Result<bool, CoreError> {
        let task_ids: Vec<TaskId> = self.sessions.keys().cloned().collect();
        let mut changed = false;
        for task_id in task_ids {
            let events = match self.sessions.get_mut(&task_id) {
                Some(session) if !session.is_closed() => session.pump(),
                Some(_) => Vec::new(),
                None => continue,
            };
            if events.is_empty() {
                if self.persist_task_transcript(&task_id).await? {
                    self.publish_task(&task_id)?;
                    changed = true;
                }
                continue;
            }
            let task = self.task(&task_id)?;
            let before = (task.state, task.error.clone());
            self.apply_events(&task_id, &events)?;
            let task = self.task(&task_id)?;
            if (task.state, &task.error) != (before.0, &before.1) {
                self.save_task(&task_id).await?;
            } else {
                self.persist_task_transcript(&task_id).await?;
            }
            self.publish_task(&task_id)?;
            changed = true;
        }
        if self.shutdown == ShutdownState::Running {
            let ready_notes: Vec<TaskId> = self
                .review_notes
                .iter()
                .filter(|(id, note)| {
                    note.ready
                        && self.tasks.get(*id).is_some_and(|task| {
                            matches!(task.state, TaskState::Ready | TaskState::Review)
                        })
                })
                .map(|(id, _)| id.clone())
                .collect();
            for task_id in ready_notes {
                if let Err(error) = self.send_queued_review_note(&task_id).await {
                    self.emit(Event::Error {
                        task_id: Some(task_id.to_string()),
                        message: format!("Could not send queued review note: {error}"),
                    });
                }
                changed = true;
            }
        }
        Ok(changed)
    }

    pub fn arm_agent_events(&mut self, notify: &Arc<tokio::sync::Notify>) {
        struct EventWake(Arc<tokio::sync::Notify>);
        impl std::task::Wake for EventWake {
            fn wake(self: Arc<Self>) {
                self.0.notify_one();
            }
            fn wake_by_ref(self: &Arc<Self>) {
                self.0.notify_one();
            }
        }
        let waker = std::task::Waker::from(Arc::new(EventWake(notify.clone())));
        let mut cx = std::task::Context::from_waker(&waker);
        let mut ready = false;
        for session in self.sessions.values_mut() {
            ready |= session.arm_events(&mut cx);
        }
        if ready {
            notify.notify_one();
        }
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

    pub fn task_search(&self) -> search::TaskSearch {
        search::TaskSearch(self.store.clone())
    }

    pub fn task_list(&self, project_id: &ProjectId) -> Result<TaskList, CoreError> {
        if !self.projects.contains_key(project_id) {
            return Err(CoreError::ProjectNotFound {
                id: project_id.as_str().to_string(),
            });
        }

        Ok(self.task_list_matching(project_id, |_| true))
    }

    fn task_list_matching(
        &self,
        project_id: &ProjectId,
        matches: impl Fn(TaskState) -> bool,
    ) -> TaskList {
        let mut tasks: Vec<TaskSummary> = self
            .tasks
            .values()
            .filter(|task| &task.project_id == project_id && matches(task.state))
            .map(TaskSummary::from)
            .collect();
        tasks.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.id.cmp(&right.id))
        });

        TaskList {
            project_id: project_id.clone(),
            tasks,
        }
    }

    pub fn review_task_lists(&self) -> Vec<TaskList> {
        self.project_order
            .iter()
            .map(|project_id| {
                self.task_list_matching(project_id, |state| state == TaskState::Review)
            })
            .filter(|list| !list.tasks.is_empty())
            .collect()
    }

    pub fn waiting_task_title(&self, task_id: &TaskId) -> Result<Option<&str>, CoreError> {
        let task = self.task(task_id)?;
        Ok((task.state == TaskState::Waiting).then_some(task.title.as_str()))
    }

    fn task_git_worktree(
        &self,
        task_id: &TaskId,
    ) -> Result<(&Task, git::GitRepository, &Path), CoreError> {
        let task = self.task(task_id)?;
        let path = task
            .worktree_path
            .as_deref()
            .ok_or_else(|| CoreError::TaskNotReady {
                id: task.id.to_string(),
                state: task.state.as_str(),
            })?;
        let repository = GitService::discover(&self.project_path(&task.project_id)?)?;
        GitService.validate_worktree(&repository, path)?;
        Ok((task, repository, path))
    }

    pub fn task_git_read(&self, task_id: &TaskId) -> Result<TaskGitRead, CoreError> {
        let task = self.task(task_id)?;
        let worktree_path = task
            .worktree_path
            .clone()
            .ok_or_else(|| CoreError::TaskNotReady {
                id: task.id.to_string(),
                state: task.state.as_str(),
            })?;
        Ok(TaskGitRead {
            project_path: self.project_path(&task.project_id)?,
            worktree_path,
            task_id: task.id.to_string(),
            state: task.state.as_str(),
            base_branch: task.base_branch.clone(),
            branch: task.branch.clone(),
        })
    }

    pub fn review_snapshot(&self, task_id: &TaskId) -> Result<git::ReviewSnapshot, CoreError> {
        self.task_git_read(task_id)?.review_snapshot()
    }

    pub fn review_file_diff(&self, task_id: &TaskId, file: &str) -> Result<String, CoreError> {
        self.task_git_read(task_id)?.review_file_diff(file)
    }

    pub fn git_file_diff(
        &self,
        task_id: &TaskId,
        file: &str,
    ) -> Result<git::GitFileDiff, CoreError> {
        self.task_git_read(task_id)?.git_file_diff(file)
    }

    pub async fn mutate_git(
        &mut self,
        task_id: &TaskId,
        mutation: git::GitMutation,
    ) -> Result<git::GitSnapshot, CoreError> {
        let (task, repository, path) = self.task_git_worktree(task_id)?;
        if self.shutdown != ShutdownState::Running {
            return Err(CoreError::NotRunning {
                state: self.shutdown,
            });
        }
        if task.state.is_live() || task.state == TaskState::Done {
            return Err(CoreError::TaskNotReady {
                id: task_id.to_string(),
                state: task.state.as_str(),
            });
        }
        match mutation {
            git::GitMutation::Stage { path: file } => GitService.stage(path, file.as_deref())?,
            git::GitMutation::Unstage { path: file } => GitService.unstage(path, &file)?,
            git::GitMutation::Commit { message, expected } => {
                GitService.commit(path, &message, &expected)?
            }
            git::GitMutation::Amend { message, expected } => {
                GitService.amend(path, &message, &expected)?
            }
            git::GitMutation::Discard {
                path: file,
                expected,
            } => GitService.discard_unstaged(path, &file, &expected)?,
            git::GitMutation::Checkout { branch } => GitService.checkout(path, &branch)?,
            git::GitMutation::CreateBranch { branch } => GitService.create_branch(path, &branch)?,
            git::GitMutation::SquashMerge => {
                let (Some(base), Some(branch)) = (&task.base_branch, &task.branch) else {
                    return Err(GitError::MergeUnavailable {
                        reason: "the task has no recorded branch".into(),
                    }
                    .into());
                };
                let mut updated = task.clone();
                updated.branch_tip_sha = Some(GitService.checked_out_tip(path, branch)?);
                self.store.save_task(&updated).await?;
                let outcome =
                    GitService.squash_merge(&repository.root, path, base, branch, &task.title);
                self.tasks.insert(task_id.clone(), updated);
                outcome?;
            }
        }
        self.git_snapshot(task_id)
    }

    pub fn git_snapshot(&self, task_id: &TaskId) -> Result<git::GitSnapshot, CoreError> {
        self.task_git_read(task_id)?.git_snapshot()
    }

    pub fn task_workspace(&self, task_id: &TaskId) -> Result<TaskWorkspace, CoreError> {
        let task = self.task(task_id)?;
        let project =
            self.projects
                .get(&task.project_id)
                .ok_or_else(|| CoreError::ProjectNotFound {
                    id: task.project_id.as_str().to_string(),
                })?;
        let session = self.sessions.get(task_id);
        let record = self.session_records.get(task_id);
        let recovery = if matches!(task.state, TaskState::Interrupted | TaskState::Failed)
            && task.worktree_path.is_some()
        {
            Some(
                match record.filter(|record| record.resume_handle().is_some()) {
                    Some(record) if record.capabilities.is_some() => SessionRecovery::Resume,
                    Some(_) => SessionRecovery::Reconnect,
                    None => SessionRecovery::StartNew,
                },
            )
        } else {
            None
        };
        Ok(TaskWorkspace {
            project: self.project_snapshot(project),
            task: TaskDetail::from(task),
            layout: self
                .workspace_layouts
                .get(task_id)
                .copied()
                .unwrap_or_default(),
            cancelling: session.is_some_and(HarnessSession::is_cancelling),
            queued_review_note: self.review_notes.get(task_id).map(|note| note.text.clone()),
            transcript: session
                .map(HarnessSession::transcript)
                .or_else(|| self.detached_transcripts.get(task_id))
                .map(|transcript| transcript.entries().to_vec())
                .unwrap_or_default(),
            agent_terminals: session
                .map(|session| session.agent_terminals().to_vec())
                .unwrap_or_default(),
            diagnostics: session.map(HarnessSession::diagnostics),
            adapter_tier: record.map(|_| task.tier),
            capabilities: session
                .filter(|session| session.session_id().is_some())
                .map(|session| session.capabilities().clone())
                .or_else(|| record.and_then(|record| record.capabilities.clone())),
            modes: session.and_then(HarnessSession::modes),
            recovery,
        })
    }

    pub async fn open_task_workspace(
        &mut self,
        task_id: &TaskId,
    ) -> Result<TaskWorkspace, CoreError> {
        self.touch_archived_worktree(task_id).await?;
        self.restore_task_transcript(task_id).await?;
        self.task_workspace(task_id)
    }

    async fn restore_task_transcript(&mut self, task_id: &TaskId) -> Result<(), CoreError> {
        if !self.sessions.contains_key(task_id)
            && !self.detached_transcripts.contains_key(task_id)
            && self.session_records.contains_key(task_id)
        {
            let entries = self.store.load_transcript(task_id).await?;
            let mut transcript = Transcript::default();
            transcript.restore(entries);
            self.detached_transcripts
                .insert(task_id.clone(), transcript);
        }
        Ok(())
    }

    pub async fn save_workspace_layout(
        &mut self,
        task_id: &TaskId,
        layout: WorkspaceLayout,
    ) -> Result<WorkspaceLayout, CoreError> {
        if self.shutdown != ShutdownState::Running {
            return Err(CoreError::NotRunning {
                state: self.shutdown,
            });
        }
        self.task(task_id)?;
        if !layout.is_valid() {
            return Err(CoreError::InvalidWorkspaceLayout);
        }
        self.store.save_workspace_layout(task_id, layout).await?;
        self.workspace_layouts.insert(task_id.clone(), layout);
        Ok(layout)
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

    async fn set_project_preview_url(
        &mut self,
        project_id: ProjectId,
        input: &str,
    ) -> Result<CommandResult, CoreError> {
        let input = input.trim();
        let url = if input.is_empty() {
            None
        } else {
            Some(projects::parse_preview_url(input).ok_or(CoreError::InvalidPreviewUrl)?)
        };
        let mut project = self.project(&project_id)?.clone();
        if project.preview_url == url {
            return Ok(CommandResult::ProjectList(self.project_list()));
        }
        project.preview_url = url;
        self.store.save_project(&project).await?;
        self.projects.insert(project_id, project);
        let projects = self.project_list();
        self.emit(Event::ProjectListChanged(projects.clone()));
        Ok(CommandResult::ProjectList(projects))
    }

    async fn create_draft(
        &mut self,
        project_id: &ProjectId,
        title: String,
        agent: Agent,
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
        let mut task = Task::draft(project_id.clone(), &title, &git_dir);
        task.agent = agent;
        task.tier = agent.default_tier();
        let task_id = task.id.clone();
        let mut updated_project = project.clone();
        updated_project.last_task_id = Some(task_id.clone());

        self.store.save_draft(&updated_project, &task).await?;

        self.projects.insert(project_id.clone(), updated_project);
        self.tasks.insert(task_id.clone(), task);

        let workspace = self.task_workspace(&task_id)?;
        self.emit(Event::TaskWorkspaceChanged(workspace.clone()));
        self.emit(Event::TaskListChanged(self.task_list(project_id)?));
        self.emit(Event::ProjectListChanged(self.project_list()));
        Ok(CommandResult::TaskWorkspace(workspace))
    }

    async fn set_draft_agent(
        &mut self,
        task_id: &TaskId,
        agent: Agent,
    ) -> Result<CommandResult, CoreError> {
        let mut task = self.task(task_id)?.clone();
        if task.state != TaskState::Draft {
            return Err(CoreError::NotADraft {
                id: task_id.as_str().to_string(),
            });
        }
        task.agent = agent;
        task.tier = agent.default_tier();
        self.store.save_task(&task).await?;
        self.tasks.insert(task_id.clone(), task);
        Ok(CommandResult::TaskWorkspace(self.publish_task(task_id)?))
    }

    async fn discard_draft(&mut self, task_id: &TaskId) -> Result<CommandResult, CoreError> {
        let task = self.task(task_id)?;
        if task.state != TaskState::Draft {
            return Err(CoreError::NotADraft {
                id: task_id.as_str().to_string(),
            });
        }
        self.delete_task_records(task_id).await
    }

    async fn delete_task(
        &mut self,
        task_id: &TaskId,
        remove_worktree: bool,
    ) -> Result<CommandResult, CoreError> {
        if let Some(pending) = self
            .store
            .load_worktree_deletions()
            .await?
            .into_iter()
            .find(|deletion| &deletion.task_id == task_id)
        {
            self.recover_worktree_deletion(&pending).await?;
        }
        let task = self.task(task_id)?;
        if matches!(
            task.state,
            TaskState::Starting | TaskState::Working | TaskState::Waiting | TaskState::Queued
        ) {
            return Err(CoreError::TaskDeletionBusy {
                id: task_id.to_string(),
                state: task.state.as_str(),
            });
        }
        if self.terminals.values().any(|terminal| {
            terminal.task_id.as_ref() == Some(task_id) && terminal.ended_at.is_none()
        }) {
            return Err(CoreError::TaskTerminalsRunning);
        }
        if remove_worktree {
            self.check_task_worktree_removal(task_id)?;
        }
        if let Some(session) = self.sessions.get_mut(task_id)
            && session.needs_shutdown()
        {
            session.shutdown().await?;
        }
        if !remove_worktree {
            return self.delete_task_records(task_id).await;
        }
        let (_, repository, path) = self.task_git_worktree(task_id)?;
        let original_path = path.to_path_buf();
        let _deletion_lock = GitService.lock_worktree_deletions(&repository)?;
        let identity = uuid::Uuid::new_v4().to_string();
        let staged_path = repository
            .git_dir
            .join("shepherd/worktrees")
            .join(format!(".shepherd-deletion-{identity}"));
        let worktree_git_dir =
            GitService.mark_worktree_deletion(&repository, &original_path, &identity)?;
        let deletion = WorktreeDeletion {
            task_id: task_id.clone(),
            repository_root: repository.root.clone(),
            original_path,
            staged_path,
            worktree_git_dir,
            identity,
        };
        if let Err(error) = self.store.save_worktree_deletion(&deletion).await {
            if let Err(cleanup) = GitService.clear_worktree_deletion_marker(
                &repository,
                &deletion.original_path,
                &deletion.worktree_git_dir,
                &deletion.identity,
            ) {
                return Err(CoreError::WorktreeDeletionRecovery {
                    task_id: task_id.to_string(),
                    reason: format!(
                        "could not save deletion intent: {error}; could not clear its marker: {cleanup}"
                    ),
                });
            }
            return Err(error.into());
        }
        if let Err(error) =
            GitService.move_worktree(&repository, &deletion.original_path, &deletion.staged_path)
        {
            return Err(self
                .rollback_worktree_deletion(&deletion, error.into())
                .await);
        }
        match GitService.has_changes(&deletion.staged_path) {
            Ok(false) => {}
            Ok(true) => {
                let error = GitError::DirtyWorktree {
                    path: deletion.original_path.clone(),
                };
                return Err(self
                    .rollback_worktree_deletion(&deletion, error.into())
                    .await);
            }
            Err(error) => {
                return Err(self
                    .rollback_worktree_deletion(&deletion, error.into())
                    .await);
            }
        }
        #[cfg(test)]
        pause_worktree_deletion_fixture("staged", task_id).await?;
        let result = match self.delete_task_records(task_id).await {
            Ok(result) => result,
            Err(error) => return Err(self.rollback_worktree_deletion(&deletion, error).await),
        };
        #[cfg(test)]
        pause_worktree_deletion_fixture("committed", task_id).await?;
        self.recover_worktree_deletion_locked(&deletion)
            .await
            .map_err(|error| CoreError::TaskDeletedCleanupPending {
                task_id: task_id.to_string(),
                reason: error.to_string(),
            })?;
        Ok(result)
    }

    async fn rollback_worktree_deletion(
        &mut self,
        deletion: &WorktreeDeletion,
        error: CoreError,
    ) -> CoreError {
        match self.recover_worktree_deletion_locked(deletion).await {
            Ok(()) => error,
            Err(recovery) => CoreError::WorktreeDeletionRecovery {
                task_id: deletion.task_id.to_string(),
                reason: format!(
                    "deletion failed: {error}; checkout recovery remains pending: {recovery}"
                ),
            },
        }
    }

    pub fn check_task_worktree_removal(&self, task_id: &TaskId) -> Result<(), CoreError> {
        let (_, _, path) = self.task_git_worktree(task_id)?;
        if GitService.has_changes(path)? {
            return Err(GitError::DirtyWorktree {
                path: path.to_path_buf(),
            }
            .into());
        }
        Ok(())
    }

    async fn delete_task_records(&mut self, task_id: &TaskId) -> Result<CommandResult, CoreError> {
        let task = self.task(task_id)?;
        let project_id = task.project_id.clone();
        let terminals: Vec<_> = self
            .terminals
            .values()
            .filter(|terminal| terminal.task_id.as_ref() == Some(task_id))
            .collect();
        if terminals.iter().any(|terminal| terminal.ended_at.is_none()) {
            return Err(CoreError::TaskTerminalsRunning);
        }
        let terminal_ids: std::collections::BTreeSet<String> = terminals
            .iter()
            .map(|terminal| terminal.id.as_str().to_owned())
            .collect();
        let mut updated_layout = None;
        if !terminal_ids.is_empty() {
            let mut layout = self.terminal_layout(&project_id)?;
            let previous = layout.root.clone();
            layout.root = layout.root.and_then(|root| root.without(&terminal_ids));
            if layout.root != previous {
                layout.revision = layout
                    .revision
                    .checked_add(1)
                    .ok_or(CoreError::TerminalLayoutChanged)?;
                updated_layout = Some(layout);
            }
        }
        self.store
            .delete_task(
                task_id,
                updated_layout.as_ref().map(|layout| (&project_id, layout)),
            )
            .await?;
        if let Some(layout) = updated_layout {
            self.terminal_layouts.insert(project_id.clone(), layout);
        }
        self.tasks.remove(task_id);
        self.workspace_layouts.remove(task_id);
        if self.sessions.remove(task_id).is_some() {
            self.emit(Event::ProjectApprovalsChanged {
                project_id: project_id.clone(),
            });
        }
        self.detached_transcripts.remove(task_id);
        self.session_records.remove(task_id);
        self.turn_baselines.remove(task_id);
        self.review_notes.remove(task_id);
        self.terminals
            .retain(|id, _| !terminal_ids.contains(id.as_str()));
        if !terminal_ids.is_empty() {
            self.publish_terminal_list(&project_id);
        }

        if let Some(project) = self.projects.get_mut(&project_id)
            && project.last_task_id.as_ref() == Some(task_id)
        {
            project.last_task_id = None;
        }

        self.emit(Event::TaskListChanged(self.task_list(&project_id)?));
        self.emit(Event::ProjectListChanged(self.project_list()));
        Ok(CommandResult::Ack)
    }

    async fn review_task(
        &mut self,
        task_id: &TaskId,
        action: ReviewAction,
    ) -> Result<CommandResult, CoreError> {
        if matches!(action, ReviewAction::Reopen) && self.task(task_id)?.collection_pending {
            self.touch_archived_worktree(task_id).await?;
        }
        let mut task = self.task(task_id)?.clone();
        match action {
            ReviewAction::Archive => {
                self.archive_reviewed(vec![task_id.clone()]).await?;
                return Ok(CommandResult::TaskWorkspace(self.task_workspace(task_id)?));
            }
            ReviewAction::Reopen => {
                if task.state != TaskState::Done {
                    return Err(CoreError::TaskNotArchived {
                        id: task_id.to_string(),
                        state: task.state.as_str(),
                    });
                }
                task.set_state(TaskState::Review)?;
                if task.worktree_path.is_none() && task.branch.is_some() {
                    task.worktree_path = Some(task.proposed_worktree_path.clone());
                }
                self.restore_task_worktree(&task)?;
                task.collection_pending = false;
            }
            ReviewAction::SendNote { text } => {
                if text.trim().is_empty() {
                    return Err(CoreError::EmptyPrompt);
                }
                if self.review_notes.contains_key(task_id) {
                    return Err(CoreError::ReviewNoteAlreadyQueued {
                        id: task_id.to_string(),
                    });
                }
                if task.state.is_live() {
                    self.review_notes
                        .insert(task_id.clone(), QueuedReviewNote { text, ready: false });
                    return Ok(CommandResult::TaskWorkspace(self.publish_task(task_id)?));
                }
                if task.state != TaskState::Review {
                    return Err(CoreError::TaskNotReady {
                        id: task_id.to_string(),
                        state: task.state.as_str(),
                    });
                }
                return self.send_review_note(task_id, text).await;
            }
            ReviewAction::SendQueuedNote => return self.send_queued_review_note(task_id).await,
            ReviewAction::RemoveQueuedNote => {
                self.review_notes.remove(task_id);
                return Ok(CommandResult::TaskWorkspace(self.publish_task(task_id)?));
            }
        }
        self.store.save_task(&task).await?;
        self.tasks.insert(task_id.clone(), task);
        Ok(CommandResult::TaskWorkspace(self.publish_task(task_id)?))
    }

    async fn archive_reviewed(
        &mut self,
        task_ids: Vec<TaskId>,
    ) -> Result<CommandResult, CoreError> {
        let archived = task_ids
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|task_id| {
                let mut task = self.task(&task_id)?.clone();
                task.set_state(TaskState::Done)?;
                Ok(task)
            })
            .collect::<Result<Vec<_>, CoreError>>()?;
        if archived.is_empty() {
            return Ok(CommandResult::Ack);
        }
        for task in &archived {
            let Some(session) = self.sessions.get_mut(&task.id) else {
                continue;
            };
            let events = if session.needs_shutdown() {
                session.shutdown().await?
            } else {
                Vec::new()
            };
            self.end_session_record(&task.id, &events).await?;
            self.persist_task_transcript(&task.id).await?;
        }
        self.store.archive_tasks(&archived).await?;
        let mut projects = BTreeSet::new();
        let mut task_ids = Vec::new();
        for task in archived {
            self.review_notes.remove(&task.id);
            projects.insert(task.project_id.clone());
            task_ids.push(task.id.clone());
            self.tasks.insert(task.id.clone(), task);
        }
        for task_id in task_ids {
            self.emit(Event::TaskChanged(TaskSummary::from(self.task(&task_id)?)));
            self.emit(Event::TaskWorkspaceChanged(self.task_workspace(&task_id)?));
        }
        for project_id in projects {
            self.emit(Event::TaskListChanged(self.task_list(&project_id)?));
        }
        self.emit(Event::ProjectListChanged(self.project_list()));
        Ok(CommandResult::Ack)
    }

    async fn send_queued_review_note(
        &mut self,
        task_id: &TaskId,
    ) -> Result<CommandResult, CoreError> {
        let state = self.task(task_id)?.state;
        if !matches!(state, TaskState::Ready | TaskState::Review) {
            return Err(CoreError::TaskNotReady {
                id: task_id.to_string(),
                state: state.as_str(),
            });
        }
        let mut note =
            self.review_notes
                .remove(task_id)
                .ok_or_else(|| CoreError::ReviewNoteNotQueued {
                    id: task_id.to_string(),
                })?;
        let outcome = self.send_review_note(task_id, note.text.clone()).await;
        if outcome.is_err() {
            note.ready = false;
            self.review_notes.insert(task_id.clone(), note);
            self.publish_task(task_id)?;
        }
        outcome
    }

    async fn send_review_note(
        &mut self,
        task_id: &TaskId,
        text: String,
    ) -> Result<CommandResult, CoreError> {
        self.repair_worktree(task_id).await?;
        let paths = self
            .review_snapshot(task_id)?
            .files
            .into_iter()
            .map(|file| file.path)
            .collect::<Vec<_>>();
        let prompt = if paths.is_empty() {
            text
        } else {
            format!(
                "{text}\n\nChanged files under review:\n{}",
                paths.join("\n")
            )
        };
        self.send_prompt(task_id, PromptInput::text(prompt)).await
    }

    async fn send_prompt(
        &mut self,
        task_id: &TaskId,
        mut input: PromptInput,
    ) -> Result<CommandResult, CoreError> {
        input.text = input.text.trim().to_owned();
        if input.text.is_empty() {
            return Err(CoreError::EmptyPrompt);
        }
        if input.codex != Default::default() && self.task(task_id)?.agent != Agent::Codex {
            return Err(CoreError::CodexOptionsForOtherAgent);
        }

        match self.task(task_id)?.state {
            TaskState::Draft => self.create_worktree(task_id).await?,
            TaskState::Ready | TaskState::Review => self.repair_worktree(task_id).await?,
            state => {
                return Err(CoreError::TaskNotReady {
                    id: task_id.as_str().to_string(),
                    state: state.as_str(),
                });
            }
        }

        if let Some(path) = self.task(task_id)?.worktree_path.as_ref() {
            for mention in files::prompt_mentions(&input.text) {
                let resolved = match files::external_file_path(path, &mention) {
                    Ok(resolved) => resolved,
                    Err(source) => {
                        self.publish_task(task_id)?;
                        return Err(CoreError::FileMention {
                            path: mention.display().to_string(),
                            source,
                        });
                    }
                };
                input.attachments.push(PromptAttachment {
                    path: resolved,
                    image_mime_type: None,
                    metadata: None,
                });
            }
            let baseline = GitService.turn_baseline(path)?;
            self.turn_baselines.insert(task_id.clone(), baseline);
        }
        self.set_task_state(task_id, TaskState::Working)?;
        if let Some(task) = self.tasks.get_mut(task_id) {
            task.error = None;
        }
        let stored = self
            .sessions
            .get(task_id)
            .filter(|session| session.is_closed())
            .and_then(|_| self.session_records.get(task_id))
            .and_then(SessionRecord::resume_handle)
            .map(SessionId::new);
        let opened = self.ensure_session(task_id, stored).await;
        if let Ok(SessionOpened::ResumeFailed(reason)) = &opened
            && let Some(session) = self.sessions.get_mut(task_id)
        {
            session.note(
                format!("Could not resume the previous session: {reason}. Started a new session."),
                true,
            );
        }
        if let Err(error) = opened {
            // A session that never started leaves the task ready to retry; the
            // failure row is already in its transcript.
            self.set_task_state(task_id, TaskState::Ready)?;
            self.turn_baselines.remove(task_id);
            if let Some(task) = self.tasks.get_mut(task_id) {
                task.error = Some(error.to_string());
            }
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
            session.prompt(input).await
        };
        let events = match outcome {
            Ok(events) => events,
            Err(error) => {
                self.turn_baselines.remove(task_id);
                let events = self
                    .sessions
                    .get_mut(task_id)
                    .map(HarnessSession::pump)
                    .unwrap_or_default();
                self.apply_events(task_id, &events)?;
                if self.task(task_id)?.state.is_live() {
                    self.record_failure(task_id, &error);
                    self.set_task_state(task_id, TaskState::Ready)?;
                }
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

    async fn set_session_mode(
        &mut self,
        task_id: &TaskId,
        mode: &str,
    ) -> Result<CommandResult, CoreError> {
        if !matches!(
            self.task(task_id)?.state,
            TaskState::Ready | TaskState::Review
        ) {
            return Err(agent_harness::HarnessError::ModeUnavailable.into());
        }
        let session = self
            .sessions
            .get_mut(task_id)
            .ok_or(agent_harness::HarnessError::ModeUnavailable)?;
        let events = session.set_mode(mode).await?;
        self.apply_events(task_id, &events)?;
        self.save_task(task_id).await?;
        Ok(CommandResult::TaskWorkspace(self.publish_task(task_id)?))
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
                let pending = self
                    .sessions
                    .get_mut(task_id)
                    .map(HarnessSession::pump_existing)
                    .unwrap_or_default();
                let disconnected = pending.iter().any(|event| {
                    matches!(
                        event,
                        HarnessEvent::ConnectionFailed { .. } | HarnessEvent::Exited { .. }
                    )
                });
                if !pending.is_empty() {
                    self.apply_events(task_id, &pending)?;
                }
                let expected = matches!(
                    error,
                    CoreError::Harness(
                        agent_harness::HarnessError::NoPendingApproval
                            | agent_harness::HarnessError::ApprovalOptionUnavailable
                    )
                );
                if !expected && !disconnected {
                    self.record_failure(task_id, &error);
                    self.set_task_state(task_id, TaskState::Ready)?;
                }
                if !expected || !pending.is_empty() {
                    self.save_task(task_id).await?;
                    self.publish_task(task_id)?;
                }
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
        if state == TaskState::Review {
            return self.publish_task(task_id).map(CommandResult::TaskWorkspace);
        }
        if !state.is_live() {
            return Err(CoreError::TaskNotCancellable {
                id: task_id.as_str().to_string(),
                state: state.as_str(),
            });
        }

        if let Some(session) = self.sessions.get_mut(task_id) {
            let events = session.cancel().await?;
            self.apply_events(task_id, &events)?;
        } else {
            self.set_task_state(task_id, TaskState::Ready)?;
        }
        self.save_task(task_id).await?;
        let workspace = self.publish_task(task_id)?;
        Ok(CommandResult::TaskWorkspace(workspace))
    }

    /// Resume an interrupted task with the agent session the previous process
    /// left behind, and say plainly when that session is gone.
    async fn resume_task(&mut self, task_id: &TaskId) -> Result<CommandResult, CoreError> {
        let task = self.task(task_id)?;
        let state = task.state;
        if !matches!(state, TaskState::Interrupted | TaskState::Failed)
            || task.worktree_path.is_none()
        {
            return Err(CoreError::TaskNotResumable {
                id: task_id.as_str().to_string(),
                state: state.as_str(),
            });
        }

        self.repair_worktree(task_id).await?;
        self.set_task_state(task_id, TaskState::Starting)?;
        if let Some(session) = self.sessions.remove(task_id) {
            let mut transcript = Transcript::default();
            transcript.restore(session.transcript().entries().iter().cloned());
            self.detached_transcripts
                .insert(task_id.clone(), transcript);
            self.emit(Event::ProjectApprovalsChanged {
                project_id: self.task(task_id)?.project_id.clone(),
            });
        }
        let stored_session_id = self
            .session_records
            .get(task_id)
            .and_then(SessionRecord::resume_handle)
            .map(SessionId::new);
        let opened = match self.ensure_session(task_id, stored_session_id).await {
            Ok(opened) => opened,
            Err(error) => {
                // Stay resumable: the user can install or update the agent and
                // try again.
                self.set_task_state(task_id, state)?;
                if let Some(task) = self.tasks.get_mut(task_id) {
                    task.error = Some(error.to_string());
                }
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
                        "the adapter cannot resume the previous session ({error}). Shepherd started a new session"
                    ),
                    true,
                ),
                SessionOpened::Fresh => session.note("started a new agent session", false),
            }
        }
        self.set_task_state(task_id, TaskState::Ready)?;
        if let Some(task) = self.tasks.get_mut(task_id) {
            task.error = None;
        }
        self.save_task(task_id).await?;
        let workspace = self.publish_task(task_id)?;
        Ok(CommandResult::TaskWorkspace(workspace))
    }

    async fn repair_worktree(&mut self, task_id: &TaskId) -> Result<(), CoreError> {
        let task = self.task(task_id)?.clone();
        if let (Some(path), Some(branch)) = (&task.worktree_path, &task.branch) {
            let repository = GitService::discover(&self.project_path(&task.project_id)?)?;
            let missing = !path.exists();
            if missing && let Some(session) = self.sessions.get_mut(task_id) {
                session.shutdown().await?;
                self.sessions.remove(task_id);
                self.emit(Event::ProjectApprovalsChanged {
                    project_id: task.project_id.clone(),
                });
            }
            GitService.restore_worktree(
                &repository,
                path,
                branch,
                task.branch_tip_sha.as_deref(),
            )?;
        }
        Ok(())
    }

    fn restore_task_worktree(&self, task: &Task) -> Result<(), CoreError> {
        if let (Some(path), Some(branch)) = (&task.worktree_path, &task.branch) {
            let repository = GitService::discover(&self.project_path(&task.project_id)?)?;
            GitService.restore_worktree(
                &repository,
                path,
                branch,
                task.branch_tip_sha.as_deref(),
            )?;
        }
        Ok(())
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
        self.restore_task_transcript(task_id).await?;
        if let Some(session) = self.sessions.get_mut(task_id)
            && (session.has_failed() || session.is_closed())
            && session.needs_shutdown()
        {
            session.shutdown().await?;
        }
        let carried = match self.sessions.get(task_id) {
            Some(session) if !session.has_failed() && !session.is_closed() => {
                return Ok(SessionOpened::Fresh);
            }
            Some(_) => {
                self.emit(Event::ProjectApprovalsChanged {
                    project_id: self.task(task_id)?.project_id.clone(),
                });
                self.sessions
                    .remove(task_id)
                    .map(|session| session.transcript().entries().to_vec())
                    .unwrap_or_default()
            }
            None => self
                .detached_transcripts
                .remove(task_id)
                .map(|transcript| transcript.entries().to_vec())
                .unwrap_or_default(),
        };

        let task = self.task(task_id)?.clone();
        let cwd = match task.worktree_path.clone() {
            Some(worktree_path) => worktree_path,
            None => self.project_path(&task.project_id)?,
        };
        let (harness, mut spec) = match self
            .harness_factory
            .launch_for_task(task_id, task.agent, cwd)
        {
            Ok(launch) => launch,
            Err(error) => {
                let mut transcript = Transcript::default();
                transcript.restore(carried);
                if let Some(kind) = adapter_failure_kind(&error) {
                    transcript.push_failure(kind, error.to_string(), Some(kind.hint().to_string()));
                }
                self.detached_transcripts
                    .insert(task_id.clone(), transcript);
                return Err(error);
            }
        };
        let project_path = self.project_path(&task.project_id)?;
        let environment = self
            .harness_factory
            .environment_for_project(task.agent, &project_path);
        let mut session = HarnessSession::new(harness);
        session.restore_transcript(carried);
        match environment {
            Ok(environment) => spec.environment.extend(environment),
            Err(error) => return Err(self.fail_session_start(task_id, session, error)),
        }
        let policy = ProjectApprovals::new(
            self.store.clone(),
            task.project_id.clone(),
            task.agent,
            &spec,
        )?;
        session.set_project_approval_policy(Arc::new(policy));
        if let Err(error) =
            agent_harness::settings::resolve_environment(&mut spec.environment, &project_path).await
        {
            return Err(self.fail_session_start(task_id, session, error.into()));
        }
        let capabilities = match session.start(spec).await {
            Ok(capabilities) => capabilities,
            Err(error) => return Err(self.fail_session_start(task_id, session, error)),
        };
        let opened_session = match resume {
            Some(_)
                if task.tier != session.tier() && self.session_records.contains_key(task_id) =>
            {
                let reason = format!(
                    "the adapter changed from {} to {}",
                    task.tier.as_str(),
                    session.tier().as_str()
                );
                session
                    .open_session(None)
                    .await
                    .map(|session_id| (SessionOpened::ResumeFailed(reason), session_id))
            }
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

        let mut record = SessionRecord::started(
            task_id.clone(),
            task.agent.as_str(),
            Some(session_id.as_str().to_string()),
        );
        record.capabilities = Some(session.capabilities().clone());
        if let Err(error) = self.store.save_session(&record).await {
            let result = match session.shutdown().await {
                Ok(_) => CoreError::Store(error),
                Err(cleanup) => {
                    session.push_failure(
                        FailureKind::Protocol,
                        "Could not stop the agent after a session save failure.",
                        None,
                    );
                    CoreError::SessionRecordCleanup {
                        store: error.to_string(),
                        cleanup: cleanup.to_string(),
                    }
                }
            };
            self.sessions.insert(task_id.clone(), session);
            return Err(result);
        }
        if let Some(task) = self.tasks.get_mut(task_id) {
            task.tier = session.tier();
        }
        self.session_records.insert(task_id.clone(), record);
        self.sessions.insert(task_id.clone(), session);
        Ok(opened)
    }

    /// Fold adapter activity into the task state. The transcript itself is
    /// updated by [`HarnessSession`] as it pumps the same events.
    fn apply_events(&mut self, task_id: &TaskId, events: &[HarnessEvent]) -> Result<(), CoreError> {
        let task = self.task(task_id)?;
        if events.iter().any(|event| {
            matches!(
                event,
                HarnessEvent::ApprovalResolved {
                    decision: Decision::AllowSession | Decision::AllowProject,
                    ..
                } | HarnessEvent::Exited { .. }
                    | HarnessEvent::ConnectionFailed { .. }
            )
        }) {
            self.emit(Event::ProjectApprovalsChanged {
                project_id: task.project_id.clone(),
            });
        }
        let initial_state = task.state;
        let started_live = initial_state.is_live();
        let mut next_state = None;
        let mut turn_completed = false;
        let mut task_error = task.error.clone();
        let mut failure = None;

        for event in events {
            match event {
                HarnessEvent::ApprovalRequested { .. } => {
                    next_state = Some(TaskState::Waiting);
                }
                HarnessEvent::ApprovalResolved { .. } | HarnessEvent::ApprovalWithdrawn { .. } => {
                    if next_state.unwrap_or(initial_state) == TaskState::Waiting
                        && self
                            .sessions
                            .get(task_id)
                            .and_then(HarnessSession::pending_approval)
                            .is_none()
                    {
                        next_state = Some(TaskState::Working);
                    }
                }
                HarnessEvent::TurnFinished { reason } => match reason {
                    TurnFinishReason::Completed => match self.state_after_completed_turn(task_id) {
                        Ok(state) => {
                            turn_completed = true;
                            next_state = Some(state);
                        }
                        Err(error) => {
                            next_state = Some(TaskState::Failed);
                            task_error = Some(format!("could not compare turn changes: {error}"));
                        }
                    },
                    TurnFinishReason::Cancelled => next_state = Some(TaskState::Ready),
                    TurnFinishReason::Failed => {
                        next_state = Some(TaskState::Failed);
                        if task_error.is_none() {
                            let message = "The agent reported a failed turn.".to_string();
                            task_error = Some(message.clone());
                            failure = Some((FailureKind::AgentRejected, message, None));
                        }
                    }
                    TurnFinishReason::Interrupted => {
                        next_state = Some(TaskState::Interrupted);
                    }
                },
                HarnessEvent::TurnError { text, .. } => {
                    task_error = Some(text.clone());
                }
                HarnessEvent::ConnectionFailed { text, .. } => {
                    task_error = Some(text.clone());
                    if next_state.unwrap_or(initial_state).is_live() {
                        next_state = Some(TaskState::Failed);
                    }
                }
                HarnessEvent::Exited { code, stderr } => {
                    let turn_in_progress = next_state.map_or(started_live, TaskState::is_live);
                    if turn_in_progress {
                        let text = code.map_or_else(
                            || "The agent exited before the turn finished.".into(),
                            |code| {
                                format!(
                                    "The agent exited with code {code} before the turn finished."
                                )
                            },
                        );
                        let tail = stderr.trim();
                        task_error = Some(if tail.is_empty() {
                            text.clone()
                        } else {
                            format!("{text}\n{tail}")
                        });
                        failure = Some((
                            FailureKind::MidTurnExit,
                            text,
                            (!tail.is_empty()).then(|| tail.to_owned()),
                        ));
                        next_state = Some(TaskState::Failed);
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
            if let Some(note) = self.review_notes.get_mut(task_id) {
                note.ready =
                    turn_completed && matches!(state, TaskState::Ready | TaskState::Review);
            }
            if !state.is_live() {
                self.turn_baselines.remove(task_id);
            }
            let task = self
                .tasks
                .get_mut(task_id)
                .ok_or_else(|| CoreError::TaskNotFound {
                    id: task_id.as_str().to_string(),
                })?;
            if task.state != state {
                task.set_state(state)?;
                if state == TaskState::Waiting {
                    self.emit(Event::TaskNeedsInput {
                        task_id: task_id.to_string(),
                    });
                }
            }
        }
        if let Some(task) = self.tasks.get_mut(task_id) {
            task.error = task_error;
        }

        Ok(())
    }

    fn state_after_completed_turn(&mut self, task_id: &TaskId) -> Result<TaskState, CoreError> {
        let Some(baseline) = self.turn_baselines.remove(task_id) else {
            return Ok(TaskState::Ready);
        };
        let task = self.task(task_id)?;
        let Some(worktree_path) = task.worktree_path.as_ref() else {
            return Ok(TaskState::Ready);
        };
        if GitService.turn_baseline(worktree_path)? != baseline {
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
        if let Some(kind) = adapter_failure_kind(&error) {
            session.push_failure(kind, error.to_string(), Some(kind.hint().to_string()));
        }
        self.sessions.insert(task_id.clone(), session);
        error
    }

    /// Turn an adapter error into a visible failure row. Errors about Shepherd's
    /// own state get no row, because the caller already reports them.
    fn record_failure(&mut self, task_id: &TaskId, error: &CoreError) {
        let Some(kind) = adapter_failure_kind(error) else {
            return;
        };
        if let Some(task) = self.tasks.get_mut(task_id) {
            task.error = Some(error.to_string());
        }
        if let Some(session) = self.sessions.get_mut(task_id) {
            session.push_failure(kind, error.to_string(), Some(kind.hint().to_string()));
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
        self.emit(Event::ProjectApprovalsChanged {
            project_id: self.task(task_id)?.project_id.clone(),
        });
        let Some(record) = self.session_records.get_mut(task_id) else {
            return Ok(());
        };
        if !record.is_ended() {
            let (exit_code, stderr) = events
                .iter()
                .rev()
                .find_map(|event| match event {
                    HarnessEvent::Exited { code, stderr } => Some((*code, Some(stderr.clone()))),
                    _ => None,
                })
                .unwrap_or_default();
            record.end(exit_code, stderr);
        }
        self.store.save_session(record).await?;
        Ok(())
    }

    async fn quit(&mut self, force: bool) -> Result<CommandResult, CoreError> {
        let live_tasks: Vec<TaskId> = self
            .tasks
            .values()
            .filter(|task| task.state.is_live())
            .map(|task| task.id.clone())
            .collect();
        let live_terminals = self
            .terminals
            .values()
            .filter(|terminal| terminal.ended_at.is_none())
            .count();
        if !live_tasks.is_empty() && !force {
            return Err(CoreError::QuitConfirmationRequired {
                live_tasks: live_tasks.len(),
                live_terminals,
            });
        }

        self.set_shutdown(ShutdownState::Draining);

        let session_tasks: Vec<TaskId> = self.sessions.keys().cloned().collect();
        for task_id in session_tasks {
            if !self.shutdown_events.contains_key(&task_id) {
                let session =
                    self.sessions
                        .get_mut(&task_id)
                        .ok_or_else(|| CoreError::SessionMissing {
                            id: task_id.to_string(),
                        })?;
                let events = if session.needs_shutdown() {
                    match session.shutdown().await {
                        Ok(events) => events,
                        Err(error) => {
                            tracing::error!(task_id = task_id.as_str(), %error, "failed to stop the agent session during shutdown");
                            return Err(error);
                        }
                    }
                } else {
                    Vec::new()
                };
                self.shutdown_events.insert(task_id.clone(), events);
            }
            let events = self
                .shutdown_events
                .get(&task_id)
                .cloned()
                .unwrap_or_default();
            let mut task = self.task(&task_id)?.clone();
            let was_live = task.state.is_live();
            if was_live {
                task.set_state(TaskState::Interrupted)?;
            }
            if let Some(record) = self.session_records.get(&task_id) {
                let settled = !(was_live || self.shutdown_interrupted.contains(&task_id))
                    || events
                        .iter()
                        .any(|event| matches!(event, HarnessEvent::TurnFinished { .. }));
                let session =
                    self.sessions
                        .get_mut(&task_id)
                        .ok_or_else(|| CoreError::SessionMissing {
                            id: task_id.to_string(),
                        })?;
                session
                    .persist_transcript(&*self.store, record, settled, was_live.then_some(&task))
                    .await?;
            } else if was_live {
                self.store.save_task(&task).await?;
            }
            if was_live {
                self.tasks.insert(task_id.clone(), task.clone());
                self.shutdown_interrupted.insert(task_id.clone());
                self.emit(Event::TaskChanged(TaskSummary::from(&task)));
            }
            self.end_session_record(&task_id, &events).await?;
            self.sessions.remove(&task_id);
            self.shutdown_events.remove(&task_id);
        }

        for task_id in live_tasks {
            if self.shutdown_interrupted.contains(&task_id) {
                continue;
            }
            let mut task = self.task(&task_id)?.clone();
            task.set_state(TaskState::Interrupted)?;
            self.store.save_task(&task).await?;
            self.tasks.insert(task_id.clone(), task.clone());
            self.shutdown_interrupted.insert(task_id);
            self.emit(Event::TaskChanged(TaskSummary::from(&task)));
        }

        let interrupted_tasks = self.shutdown_interrupted.len();
        let affected_projects: BTreeSet<_> = self
            .shutdown_interrupted
            .iter()
            .filter_map(|task_id| self.tasks.get(task_id).map(|task| task.project_id.clone()))
            .collect();

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
            live_terminals,
        }))
    }

    fn task(&self, task_id: &TaskId) -> Result<&Task, CoreError> {
        self.tasks
            .get(task_id)
            .ok_or_else(|| CoreError::TaskNotFound {
                id: task_id.as_str().to_string(),
            })
    }

    pub fn project_path(&self, project_id: &ProjectId) -> Result<PathBuf, CoreError> {
        Ok(self.project(project_id)?.path.clone())
    }

    pub fn project_launch_environment(
        &self,
        project_id: &ProjectId,
        agent: Agent,
    ) -> Result<(PathBuf, BTreeMap<String, String>), CoreError> {
        let path = self.project_path(project_id)?;
        let environment = self.harness_factory.environment_for_project(agent, &path)?;
        Ok((path, environment))
    }

    pub fn task_project_path(&self, task_id: &TaskId) -> Result<PathBuf, CoreError> {
        self.project_path(&self.task(task_id)?.project_id)
    }

    pub fn project_preview_url(
        &self,
        project_id: &ProjectId,
    ) -> Result<Option<url::Url>, CoreError> {
        Ok(self.project(project_id)?.preview_url.clone())
    }

    fn project(&self, project_id: &ProjectId) -> Result<&Project, CoreError> {
        self.projects
            .get(project_id)
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
        if let (Some(session), Some(record)) = (
            self.sessions.get_mut(task_id),
            self.session_records.get(task_id),
        ) {
            session
                .persist_transcript(&*self.store, record, !task.state.is_live(), Some(&task))
                .await?;
        } else {
            self.store.save_task(&task).await?;
        }
        Ok(())
    }

    async fn persist_task_transcript(&mut self, task_id: &TaskId) -> Result<bool, CoreError> {
        let settled = !self.task(task_id)?.state.is_live();
        let (Some(session), Some(record)) = (
            self.sessions.get_mut(task_id),
            self.session_records.get(task_id),
        ) else {
            return Ok(false);
        };
        session
            .persist_transcript(&*self.store, record, settled, None)
            .await
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
        self.emit(Event::ProjectListChanged(self.project_list()));
        Ok(workspace)
    }

    fn project_snapshot(&self, project: &Project) -> ProjectSnapshot {
        let mut unfinished_task_count = 0;
        let mut review_task_count = 0;
        let mut running_task_count = 0;
        for task in self
            .tasks
            .values()
            .filter(|task| task.project_id == project.id)
        {
            unfinished_task_count += usize::from(!matches!(
                task.state,
                TaskState::Done | TaskState::Cancelled
            ));
            review_task_count += usize::from(task.state == TaskState::Review);
            running_task_count += usize::from(task.state.is_live());
        }
        ProjectSnapshot {
            id: project.id.as_str().to_string(),
            path: project.path.display().to_string(),
            name: project.name.clone(),
            missing: project.missing,
            is_git_repository: project
                .path
                .join(".git")
                .metadata()
                .is_ok_and(|metadata| metadata.is_dir() || metadata.is_file()),
            last_task_id: project
                .last_task_id
                .as_ref()
                .map(|task_id| task_id.as_str().to_string()),
            preview_url: project.preview_url.as_ref().map(ToString::to_string),
            unfinished_task_count,
            review_task_count,
            running_task_count,
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

fn adapter_failure_kind(error: &CoreError) -> Option<FailureKind> {
    match error {
        CoreError::HarnessMissing { .. } => Some(FailureKind::MissingBinary),
        CoreError::HarnessUnavailable => Some(FailureKind::Protocol),
        CoreError::Harness(error) => FailureKind::classify(error),
        _ => None,
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

    use agent_harness::live_codex::LiveCodexRun;
    use agent_harness::{AgentHarness, PromptInput, SpawnSpec};

    use crate::harness::{HarnessFactory, UnavailableHarnessFactory};
    use crate::store::TursoStore;
    use crate::test_support::{TestDatabase, TestRepository};
    use crate::{Agent, Command, CommandResult, Core, CoreError, ProjectId, TaskId, TaskState};

    /// Starts the real Codex, then kills it with SIGKILL once the test creates
    /// the marker file, so a mid-turn death is reproducible on demand.
    struct KilledCodexFactory {
        run: LiveCodexRun,
        marker: PathBuf,
    }

    impl HarnessFactory for KilledCodexFactory {
        fn create(&self) -> Result<Box<dyn AgentHarness>, CoreError> {
            Ok(Box::new(self.run.harness("killed", true)))
        }

        fn spawn_spec(&self, cwd: PathBuf) -> SpawnSpec {
            self.run.spec_with_exit_gate(cwd, "killed", &self.marker)
        }
    }

    #[tokio::test]
    #[ignore]
    async fn a_killed_codex_process_fails_the_task_with_its_stderr_tail()
    -> Result<(), Box<dyn std::error::Error>> {
        let Some(run) = LiveCodexRun::load("core-mid-turn-exit").await? else {
            return Ok(());
        };

        let repository = TestRepository::new()?;
        let database = TestDatabase::new()?;
        let directory = tempfile::TempDir::new()?;
        let marker = directory.path().join("kill");
        let mut core = Core::with_services(
            Box::new(TursoStore::open(&database.uri()).await?),
            Box::new(KilledCodexFactory {
                run: run.clone(),
                marker: marker.clone(),
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
                agent: Agent::Codex,
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
            input: PromptInput::text("Run the shell command `sleep 30` in the repository and wait for it to finish, then say done."),
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
        assert!(
            error.contains("before the turn finished"),
            "task error: {error}"
        );
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
        run.finish(None)?;
        drop(core);
        let mut reopened = Core::load(
            Box::new(TursoStore::open(&database.uri()).await?),
            Box::new(UnavailableHarnessFactory),
        )
        .await?;
        let workspace = reopened.open_task_workspace(&task_id).await?;
        assert_eq!(workspace.task.state, TaskState::Failed);
        assert!(
            workspace
                .task
                .error
                .as_deref()
                .is_some_and(|error| error.contains("before the turn finished"))
        );
        assert!(
            workspace
                .transcript
                .iter()
                .any(|entry| entry.summary().starts_with("failure:"))
        );
        Ok(())
    }
}

#[cfg(test)]
mod live_restart {
    use std::path::PathBuf;
    use std::time::Duration;

    use agent_harness::live_codex::LiveCodexRun;
    use agent_harness::{AgentHarness, PromptInput, SpawnSpec};

    use crate::harness::HarnessFactory;
    use crate::store::{Store as _, TursoStore};
    use crate::test_support::{TestDatabase, TestRepository};
    use crate::{Agent, Command, CommandResult, Core, CoreError, ProjectId, TaskId, TaskState};

    struct LiveCodexFactory {
        run: LiveCodexRun,
        session: &'static str,
    }

    impl HarnessFactory for LiveCodexFactory {
        fn create(&self) -> Result<Box<dyn AgentHarness>, CoreError> {
            Ok(Box::new(self.run.harness(self.session, false)))
        }

        fn spawn_spec(&self, cwd: PathBuf) -> SpawnSpec {
            self.run.spec(cwd, self.session, None)
        }
    }

    #[tokio::test]
    #[ignore]
    async fn a_restarted_core_resumes_the_recorded_thread() -> Result<(), Box<dyn std::error::Error>>
    {
        let Some(run) = LiveCodexRun::load("core-restart-resume").await? else {
            return Ok(());
        };

        let repository = TestRepository::new()?;
        let database = TestDatabase::new()?;
        let uri = database.uri();
        let factory = |session| LiveCodexFactory {
            run: run.clone(),
            session,
        };

        let mut core = Core::with_services(
            Box::new(TursoStore::open(&uri).await?),
            Box::new(factory("initial")),
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
                agent: Agent::Codex,
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
            input: PromptInput::text("Run the shell command `sleep 30` in the repository, wait for it to finish, then say done."),
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

        let mut restarted = Core::load(Box::new(store), Box::new(factory("resumed"))).await?;
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
                input: PromptInput::text("Reply with the single word: resumed."),
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
        run.finish(None)?;
        Ok(())
    }
}

#[cfg(test)]
mod streaming {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use agent_harness::{
        AgentHarness, ApprovalId, Capabilities, Decision, DeltaKind, Entry, HarnessEvent,
        HarnessResult, ItemId, PromptInput, SessionApproval, SessionId, SpawnSpec,
        TurnFinishReason,
    };
    use async_trait::async_trait;
    use tokio::sync::mpsc;

    use crate::harness::{HarnessFactory, HarnessSession};
    use crate::store::{Store, TursoStore};
    use crate::test_support::{TestDatabase, TestRepository};
    use crate::{
        Agent, Command, CommandResult, Core, CoreError, Event, Project, ProjectId, Task, TaskId,
        TaskState,
    };

    struct LateHarnessFactory {
        receiver: Mutex<Option<mpsc::UnboundedReceiver<HarnessEvent>>>,
        prompts: mpsc::UnboundedSender<(Option<String>, PromptInput)>,
    }

    impl HarnessFactory for LateHarnessFactory {
        fn create(&self) -> Result<Box<dyn AgentHarness>, CoreError> {
            let receiver = self
                .receiver
                .lock()
                .expect("the test holds the receiver lock")
                .take()
                .expect("the factory creates one harness per test");
            Ok(Box::new(LateHarness {
                events: receiver,
                approvals: Vec::new(),
                prompts: Some(self.prompts.clone()),
                mode: None,
            }))
        }
    }

    /// Emits nothing on its own; the test sends events after the prompt returns,
    /// the way a real adapter streams them between commands.
    struct LateHarness {
        events: mpsc::UnboundedReceiver<HarnessEvent>,
        approvals: Vec<SessionApproval>,
        prompts: Option<mpsc::UnboundedSender<(Option<String>, PromptInput)>>,
        mode: Option<String>,
    }

    #[tokio::test]
    async fn agent_event_wakeups_cover_multiple_sessions_and_buffered_events()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut core = Core::new();
        let mut senders = Vec::new();
        let mut ids = Vec::new();
        for index in 0..3 {
            let (sender, events) = mpsc::unbounded_channel();
            let id = TaskId::new(format!("task-{index}"));
            core.sessions.insert(
                id.clone(),
                HarnessSession::new(Box::new(LateHarness {
                    events,
                    approvals: Vec::new(),
                    prompts: None,
                    mode: None,
                })),
            );
            senders.push(sender);
            ids.push(id);
        }
        let [first, middle, last] = senders.as_slice() else {
            return Err("expected three senders".into());
        };
        let [first_id, middle_id, last_id] = ids.as_slice() else {
            return Err("expected three sessions".into());
        };
        let notify = Arc::new(tokio::sync::Notify::new());
        core.arm_agent_events(&notify);
        middle.send(HarnessEvent::Delta {
            item_id: ItemId::new("middle"),
            kind: DeltaKind::Message,
            text: "first".into(),
            stream_permit: None,
        })?;
        tokio::time::timeout(Duration::from_secs(1), notify.notified()).await?;
        let events = core
            .sessions
            .get_mut(middle_id)
            .ok_or("missing session")?
            .pump();
        assert_eq!(events.len(), 1);
        first.send(HarnessEvent::Delta {
            item_id: ItemId::new("first"),
            kind: DeltaKind::Message,
            text: "buffered".into(),
            stream_permit: None,
        })?;
        core.arm_agent_events(&notify);
        tokio::time::timeout(Duration::from_secs(1), notify.notified()).await?;
        assert_eq!(
            core.sessions
                .get_mut(first_id)
                .ok_or("missing session")?
                .pump()
                .len(),
            1
        );
        core.arm_agent_events(&notify);
        last.send(HarnessEvent::Delta {
            item_id: ItemId::new("last"),
            kind: DeltaKind::Message,
            text: "last".into(),
            stream_permit: None,
        })?;
        tokio::time::timeout(Duration::from_secs(1), notify.notified()).await?;
        assert_eq!(
            core.sessions
                .get_mut(last_id)
                .ok_or("missing session")?
                .pump()
                .len(),
            1
        );
        Ok(())
    }

    #[tokio::test]
    async fn background_batches_are_fair_and_preserve_all_agent_events()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut core = Core::new();
        let (busy_sender, busy_events) = mpsc::unbounded_channel();
        let (other_sender, other_events) = mpsc::unbounded_channel();
        let busy = TaskId::new("busy");
        let other = TaskId::new("other");
        for (id, events) in [(busy.clone(), busy_events), (other.clone(), other_events)] {
            core.sessions.insert(
                id,
                HarnessSession::new(Box::new(LateHarness {
                    events,
                    approvals: Vec::new(),
                    prompts: None,
                    mode: None,
                })),
            );
        }
        let count = crate::harness::MAX_EVENTS_PER_PUMP * 2 + 1;
        for index in 0..count {
            busy_sender.send(HarnessEvent::Delta {
                item_id: ItemId::new("busy"),
                kind: DeltaKind::Message,
                text: format!("{index},"),
                stream_permit: None,
            })?;
        }
        other_sender.send(HarnessEvent::Delta {
            item_id: ItemId::new("other"),
            kind: DeltaKind::Message,
            text: "other".into(),
            stream_permit: None,
        })?;
        let notify = Arc::new(tokio::sync::Notify::new());
        core.arm_agent_events(&notify);
        tokio::time::timeout(Duration::from_secs(1), notify.notified()).await?;
        let first = core
            .sessions
            .get_mut(&busy)
            .ok_or("missing busy session")?
            .pump();
        assert_eq!(first.len(), crate::harness::MAX_EVENTS_PER_PUMP);
        assert_eq!(
            core.sessions
                .get_mut(&other)
                .ok_or("missing other session")?
                .pump()
                .len(),
            1
        );
        let mut received = first;
        loop {
            core.arm_agent_events(&notify);
            if received.len() == count {
                break;
            }
            tokio::time::timeout(Duration::from_secs(1), notify.notified()).await?;
            received.extend(
                core.sessions
                    .get_mut(&busy)
                    .ok_or("missing busy session")?
                    .pump(),
            );
        }
        let chunks: Vec<_> = received
            .into_iter()
            .filter_map(|event| match event {
                HarnessEvent::Delta { text, .. } => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(
            chunks,
            (0..count)
                .map(|index| format!("{index},"))
                .collect::<Vec<_>>()
        );
        Ok(())
    }

    #[async_trait]
    impl AgentHarness for LateHarness {
        fn events(&mut self) -> &mut mpsc::UnboundedReceiver<HarnessEvent> {
            &mut self.events
        }

        fn session_approvals(&self) -> Vec<SessionApproval> {
            self.approvals.clone()
        }

        async fn reset_session_approval(&mut self, id: &str) -> HarnessResult<()> {
            self.approvals.retain(|approval| approval.id != id);
            Ok(())
        }

        async fn start(&mut self, _spec: SpawnSpec) -> HarnessResult<Capabilities> {
            Ok(Capabilities {
                cancel: true,
                ..Capabilities::default()
            })
        }

        async fn open_session(&mut self, resume: Option<SessionId>) -> HarnessResult<SessionId> {
            Ok(resume.unwrap_or_else(|| SessionId::new("late-thread")))
        }

        async fn prompt(&mut self, input: PromptInput) -> HarnessResult<()> {
            if let Some(prompts) = &self.prompts {
                prompts
                    .send((self.mode.clone(), input))
                    .map_err(|_| agent_harness::HarnessError::ChannelClosed)?;
            }
            Ok(())
        }

        async fn set_mode(&mut self, mode: &str) -> HarnessResult<()> {
            if mode != "plan" {
                return Err(agent_harness::HarnessError::ModeUnavailable);
            }
            self.mode = Some(mode.to_owned());
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
    async fn live_grants_are_listed_and_reset_by_project_and_task()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestRepository::new()?;
        let project = Project::new(directory.path().join("project"));
        let other_project = Project::new(directory.path().join("other"));
        let mut core = Core::new();
        core.projects.insert(project.id.clone(), project.clone());
        core.projects
            .insert(other_project.id.clone(), other_project.clone());
        let approval = SessionApproval {
            id: "grant".into(),
            title: "Run tests".into(),
            arguments: serde_json::json!({"command": "cargo test"}),
            paths: Vec::new(),
        };
        let mut launch = |project: &Project, title: &str| {
            let mut task = Task::draft(project.id.clone(), title, &project.path);
            task.agent = Agent::Claude;
            let task_id = task.id.clone();
            let (sender, events) = mpsc::unbounded_channel();
            core.tasks.insert(task_id.clone(), task);
            core.sessions.insert(
                task_id.clone(),
                HarnessSession::new(Box::new(LateHarness {
                    events,
                    approvals: vec![approval.clone()],
                    prompts: None,
                    mode: None,
                })),
            );
            (task_id, sender)
        };
        let (first, _first_events) = launch(&project, "First chat");
        let (second, second_events) = launch(&project, "Second chat");
        let (other, _other_events) = launch(&other_project, "Other project chat");

        let grants = core.project_approvals(&project.id).await?;
        assert_eq!(grants.sessions.len(), 2);
        assert!(
            grants
                .sessions
                .iter()
                .any(|grant| grant.task_id == first.as_str() && grant.task_title == "First chat")
        );
        assert!(matches!(
            core.reset_approval_grant(&project.id, "grant", Some(&other))
                .await,
            Err(CoreError::TaskNotFound { .. })
        ));
        core.reset_approval_grant(&project.id, "grant", Some(&first))
            .await?;
        let grants = core.project_approvals(&project.id).await?;
        assert_eq!(
            grants.sessions.first().map(|grant| grant.task_id.as_str()),
            Some(second.as_str())
        );
        assert_eq!(grants.sessions.len(), 1);
        assert_eq!(
            core.project_approvals(&other_project.id)
                .await?
                .sessions
                .len(),
            1
        );
        core.drain_events();

        second_events.send(HarnessEvent::Exited {
            code: Some(0),
            stderr: String::new(),
        })?;
        core.pump().await?;
        assert!(
            core.project_approvals(&project.id)
                .await?
                .sessions
                .is_empty()
        );
        assert!(core.drain_events().iter().any(|event| matches!(
            event, Event::ProjectApprovalsChanged { project_id } if *project_id == project.id
        )));
        Ok(())
    }

    #[tokio::test]
    async fn pumping_folds_a_streaming_turn_into_the_workspace()
    -> Result<(), Box<dyn std::error::Error>> {
        let repository = TestRepository::new()?;
        let database = TestDatabase::new()?;
        let (sender, receiver) = mpsc::unbounded_channel();
        let (prompts, mut sent) = mpsc::unbounded_channel();
        let mut core = Core::with_services(
            Box::new(TursoStore::open(&database.uri()).await?),
            Box::new(LateHarnessFactory {
                receiver: Mutex::new(Some(receiver)),
                prompts,
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
                agent: Agent::Codex,
                project_id: ProjectId::new(project.id.clone()),
                title: "Stream a turn".to_string(),
            })
            .await?
        else {
            return Err("expected a draft".into());
        };
        let task_id = TaskId::new(draft.task.id.clone());
        let prompt = r#"Open @file("README.md")"#;
        let CommandResult::TaskWorkspace(workspace) = core
            .execute(Command::SendPrompt {
                task_id: task_id.clone(),
                input: PromptInput {
                    mode: Some("plan".into()),
                    ..PromptInput::text(prompt)
                },
            })
            .await?
        else {
            return Err("expected a task workspace".into());
        };
        assert_eq!(workspace.task.state, TaskState::Working);
        assert_eq!(workspace.transcript.len(), 1);
        let (mode, input) = sent.try_recv()?;
        assert_eq!(mode.as_deref(), Some("plan"));
        assert_eq!(input.text, prompt);
        let [file] = input.attachments.as_slice() else {
            return Err("expected one referenced file".into());
        };
        let worktree = workspace
            .task
            .worktree_path
            .as_deref()
            .ok_or("missing worktree")?;
        assert_eq!(
            file.path,
            std::path::Path::new(worktree)
                .canonicalize()?
                .join("README.md")
        );
        assert!(file.metadata.is_none());
        core.drain_events();

        sender.send(HarnessEvent::Delta {
            item_id: ItemId::new("item-1"),
            kind: DeltaKind::Message,
            text: "streamed".to_string(),
            stream_permit: None,
        })?;
        assert!(core.pump().await?);
        let store = TursoStore::open(&database.uri()).await?;
        let persisted = store.load_transcript(&task_id).await?;
        assert_eq!(
            persisted.iter().map(Entry::summary).collect::<Vec<_>>(),
            [format!("you: {prompt}")]
        );
        sender.send(HarnessEvent::TurnFinished {
            reason: TurnFinishReason::Completed,
        })?;

        assert!(core.pump().await?);

        let workspace = core.task_workspace(&task_id)?;
        assert_eq!(workspace.task.state, TaskState::Ready);
        assert_eq!(workspace.transcript.len(), 2);
        assert_eq!(workspace.transcript[1].summary(), "agent: streamed");
        assert_eq!(store.load_transcript(&task_id).await?, workspace.transcript);
        let rejected = core
            .execute(Command::SendPrompt {
                task_id: task_id.clone(),
                input: PromptInput {
                    mode: Some("unavailable".into()),
                    ..PromptInput::text("Do not send this prompt")
                },
            })
            .await;
        assert!(matches!(
            rejected,
            Err(CoreError::Harness(
                agent_harness::HarnessError::ModeUnavailable
            ))
        ));
        assert!(sent.try_recv().is_err());
        assert_eq!(core.task_workspace(&task_id)?.task.state, TaskState::Ready);
        assert_eq!(store.load_transcript(&task_id).await?, workspace.transcript);
        let invalid = core
            .execute(Command::SendPrompt {
                task_id: task_id.clone(),
                input: PromptInput::text(r#"Read @file("../README.md")"#),
            })
            .await;
        assert!(matches!(invalid, Err(CoreError::FileMention { .. })));
        assert!(sent.try_recv().is_err());
        assert!(
            core.drain_events()
                .iter()
                .any(|event| matches!(event, Event::TaskWorkspaceChanged(_))),
            "the pump must publish the changed workspace"
        );

        core.execute(Command::SendPrompt {
            task_id: task_id.clone(),
            input: PromptInput::text("Cancel this turn"),
        })
        .await?;
        let CommandResult::TaskWorkspace(cancelling) = core
            .execute(Command::CancelTask {
                task_id: task_id.clone(),
            })
            .await?
        else {
            return Err("expected a task workspace".into());
        };
        assert!(cancelling.cancelling);
        assert_eq!(cancelling.task.state, TaskState::Working);
        assert!(
            core.execute(Command::SendPrompt {
                task_id: task_id.clone(),
                input: PromptInput::text("Too soon"),
            })
            .await
            .is_err()
        );

        sender.send(HarnessEvent::TurnFinished {
            reason: TurnFinishReason::Cancelled,
        })?;
        assert!(core.pump().await?);
        let finished = core.task_workspace(&task_id)?;
        assert!(!finished.cancelling);
        assert_eq!(finished.task.state, TaskState::Ready);
        assert_eq!(finished.task.worktree_path, cancelling.task.worktree_path);

        core.execute(Command::SendPrompt {
            task_id: task_id.clone(),
            input: PromptInput::text("Cancel before the process exits"),
        })
        .await?;
        core.execute(Command::CancelTask {
            task_id: task_id.clone(),
        })
        .await?;
        sender.send(HarnessEvent::Exited {
            code: Some(0),
            stderr: String::new(),
        })?;
        assert!(core.pump().await?);
        let exited = core.task_workspace(&task_id)?;
        assert!(!exited.cancelling);
        assert_eq!(exited.task.state, TaskState::Failed);
        Ok(())
    }
}

#[cfg(test)]
async fn pause_worktree_deletion_fixture(phase: &str, task_id: &TaskId) -> Result<(), CoreError> {
    if std::env::var("SHEPHERD_DELETION_CRASH_PHASE")
        .ok()
        .as_deref()
        == Some(phase)
    {
        let path = std::env::var_os("SHEPHERD_DELETION_CRASH_READY")
            .map(PathBuf::from)
            .ok_or_else(|| CoreError::Io {
                path: "deletion fixture".into(),
                source: std::io::Error::other("missing ready path"),
            })?;
        std::fs::write(&path, task_id.as_str()).map_err(|source| CoreError::Io {
            path: path.display().to_string(),
            source,
        })?;
        std::future::pending::<()>().await;
    }
    Ok(())
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
            .last()
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

    #[cfg(unix)]
    #[tokio::test]
    async fn launch_environment_reaches_process_without_persisting_secret()
    -> Result<(), Box<dyn std::error::Error>> {
        use agent_harness::fake_harness::FakeHarness;
        use agent_harness::{AgentHarness, Capabilities, HarnessResult, SpawnSpec};

        struct Factory;
        impl HarnessFactory for Factory {
            fn create(&self) -> Result<Box<dyn AgentHarness>, CoreError> {
                Ok(Box::new(InspectingHarness(FakeHarness::one_turn(
                    FakeTurn::new(vec![FakeStep::Message("done".into())]),
                ))))
            }
            fn environment_for_project(
                &self,
                _: Agent,
                _: &Path,
            ) -> Result<BTreeMap<String, String>, CoreError> {
                Ok(BTreeMap::from([(
                    "SHEPHERD_TEST_TOKEN".into(),
                    "!cat token".into(),
                )]))
            }
        }
        struct InspectingHarness(FakeHarness);
        #[async_trait::async_trait]
        impl AgentHarness for InspectingHarness {
            fn events(&mut self) -> &mut mpsc::UnboundedReceiver<HarnessEvent> {
                self.0.events()
            }
            fn diagnostics(&self) -> agent_harness::AgentDiagnostics {
                self.0.diagnostics()
            }
            async fn start(&mut self, spec: SpawnSpec) -> HarnessResult<Capabilities> {
                let output = tokio::process::Command::new("/bin/sh")
                    .args(["-c", "printf %s \"$SHEPHERD_TEST_TOKEN\""])
                    .envs(&spec.environment)
                    .output()
                    .await
                    .map_err(|error| agent_harness::HarnessError::Protocol(error.to_string()))?;
                if !output.status.success() || output.stdout != b"fixture-private-value" {
                    return Err(agent_harness::HarnessError::Protocol(
                        "environment not delivered".into(),
                    ));
                }
                self.0.start(spec).await
            }
            async fn open_session(
                &mut self,
                resume: Option<SessionId>,
            ) -> HarnessResult<SessionId> {
                self.0.open_session(resume).await
            }
            async fn prompt(&mut self, input: PromptInput) -> HarnessResult<()> {
                self.0.prompt(input).await
            }
            async fn cancel(&mut self) -> HarnessResult<()> {
                self.0.cancel().await
            }
            async fn respond(&mut self, id: ApprovalId, decision: Decision) -> HarnessResult<()> {
                self.0.respond(id, decision).await
            }
            async fn shutdown(&mut self) -> HarnessResult<()> {
                self.0.shutdown().await
            }
        }
        let repository = TestRepository::new()?;
        std::fs::write(repository.path().join("token"), "fixture-private-value\n")?;
        let database = TestDatabase::new()?;
        let mut core = Core::with_services(
            Box::new(TursoStore::open(&database.uri()).await?),
            Box::new(Factory),
        );
        let project = add_project(&mut core, repository.path().to_path_buf()).await?;
        let draft = workspace_of(
            core.execute(Command::CreateDraft {
                project_id: project,
                title: "environment".into(),
                agent: Agent::Codex,
            })
            .await?,
        );
        let task_id = TaskId::new(draft.task.id);
        let workspace = workspace_of(
            core.execute(Command::SendPrompt {
                task_id: task_id.clone(),
                input: PromptInput::text("hello"),
            })
            .await?,
        );
        assert!(!serde_json::to_string(&workspace)?.contains("fixture-private-value"));
        drop(core);
        let mut reopened = Core::load(
            Box::new(TursoStore::open(&database.uri()).await?),
            Box::new(UnavailableHarnessFactory),
        )
        .await?;
        assert!(
            !serde_json::to_string(&reopened.open_task_workspace(&task_id).await?)?
                .contains("fixture-private-value")
        );
        drop(reopened);
        for entry in std::fs::read_dir(database.path())? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                let data = std::fs::read(entry.path())?;
                assert!(
                    !data
                        .windows(b"fixture-private-value".len())
                        .any(|window| window == b"fixture-private-value")
                );
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn cancelled_admission_failure_is_persisted_and_cleanup_runs()
    -> Result<(), Box<dyn std::error::Error>> {
        use agent_harness::fake_harness::FakeHarness;
        use agent_harness::{AgentHarness, Capabilities, HarnessResult, SpawnSpec};
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct Factory {
            started: Arc<tokio::sync::Notify>,
            shutdowns: Arc<AtomicUsize>,
        }
        impl HarnessFactory for Factory {
            fn create(&self) -> Result<Box<dyn AgentHarness>, CoreError> {
                let (sender, receiver) = mpsc::unbounded_channel();
                Ok(Box::new(AdmissionHarness {
                    inner: FakeHarness::one_turn(FakeTurn::default()),
                    started: self.started.clone(),
                    shutdowns: self.shutdowns.clone(),
                    sender,
                    receiver,
                }))
            }
        }
        struct AdmissionHarness {
            inner: FakeHarness,
            started: Arc<tokio::sync::Notify>,
            shutdowns: Arc<AtomicUsize>,
            sender: mpsc::UnboundedSender<HarnessEvent>,
            receiver: mpsc::UnboundedReceiver<HarnessEvent>,
        }
        struct CancelledAdmission(mpsc::UnboundedSender<HarnessEvent>);
        impl Drop for CancelledAdmission {
            fn drop(&mut self) {
                if self
                    .0
                    .send(HarnessEvent::ConnectionFailed {
                        kind: FailureKind::Protocol,
                        text: "fixture admission cancelled".into(),
                    })
                    .is_err()
                {
                    tracing::debug!("cancelled admission fixture receiver closed");
                }
            }
        }
        #[async_trait::async_trait]
        impl AgentHarness for AdmissionHarness {
            fn events(&mut self) -> &mut mpsc::UnboundedReceiver<HarnessEvent> {
                &mut self.receiver
            }
            async fn start(&mut self, spec: SpawnSpec) -> HarnessResult<Capabilities> {
                self.inner.start(spec).await
            }
            async fn open_session(
                &mut self,
                resume: Option<SessionId>,
            ) -> HarnessResult<SessionId> {
                self.inner.open_session(resume).await
            }
            async fn prompt(&mut self, _: PromptInput) -> HarnessResult<()> {
                let _guard = CancelledAdmission(self.sender.clone());
                self.started.notify_one();
                std::future::pending().await
            }
            async fn cancel(&mut self) -> HarnessResult<()> {
                self.inner.cancel().await
            }
            async fn respond(&mut self, id: ApprovalId, decision: Decision) -> HarnessResult<()> {
                self.inner.respond(id, decision).await
            }
            async fn shutdown(&mut self) -> HarnessResult<()> {
                self.shutdowns.fetch_add(1, Ordering::SeqCst);
                self.inner.shutdown().await
            }
        }

        let repository = TestRepository::new()?;
        let database = TestDatabase::new()?;
        let started = Arc::new(tokio::sync::Notify::new());
        let shutdowns = Arc::new(AtomicUsize::new(0));
        let mut core = Core::with_services(
            Box::new(TursoStore::open(&database.uri()).await?),
            Box::new(Factory {
                started: started.clone(),
                shutdowns: shutdowns.clone(),
            }),
        );
        let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
        let draft = workspace_of(
            core.execute(Command::CreateDraft {
                project_id,
                title: "Cancelled admission".into(),
                agent: Agent::Codex,
            })
            .await?,
        );
        let task_id = TaskId::new(draft.task.id);
        let mut sending = Box::pin(core.execute(Command::SendPrompt {
            task_id: task_id.clone(),
            input: PromptInput::text("Keep this prompt"),
        }));
        tokio::select! {
            result = &mut sending => return Err(format!("admission finished unexpectedly: {result:?}").into()),
            _ = started.notified() => {}
        }
        drop(sending);
        assert!(core.pump().await?);
        let workspace = core.task_workspace(&task_id)?;
        assert_eq!(workspace.task.state, TaskState::Failed);
        let text = summaries(&workspace).join("\n");
        assert!(text.contains("Keep this prompt"));
        assert!(text.contains("fixture admission cancelled"));
        core.execute(Command::Quit { force: true }).await?;
        assert_eq!(shutdowns.load(Ordering::SeqCst), 1);
        drop(core);

        let mut reopened = Core::load(
            Box::new(TursoStore::open(&database.uri()).await?),
            Box::new(UnavailableHarnessFactory),
        )
        .await?;
        let restored = reopened.open_task_workspace(&task_id).await?;
        assert_eq!(restored.task.state, TaskState::Failed);
        let text = summaries(&restored).join("\n");
        assert!(text.contains("Keep this prompt"));
        assert!(text.contains("fixture admission cancelled"));
        Ok(())
    }

    #[tokio::test]
    async fn three_agent_sessions_keep_projects_approvals_and_worktrees_isolated()
    -> Result<(), Box<dyn std::error::Error>> {
        struct ThreeAgentFactory;
        impl HarnessFactory for ThreeAgentFactory {
            fn create(&self) -> Result<Box<dyn agent_harness::AgentHarness>, CoreError> {
                Err(CoreError::HarnessUnavailable)
            }

            fn launch(
                &self,
                agent: Agent,
                cwd: PathBuf,
            ) -> Result<
                (
                    Box<dyn agent_harness::AgentHarness>,
                    agent_harness::SpawnSpec,
                ),
                CoreError,
            > {
                let name = agent.as_str();
                let turn = FakeTurn::new(vec![
                    FakeStep::Message(format!("{name} started")),
                    FakeStep::approval("same-approval-id", &format!("Approve {name}")),
                    FakeStep::WriteFile {
                        path: "agent-result.txt".into(),
                        contents: name.into(),
                    },
                    FakeStep::Message(format!("{name} finished")),
                ]);
                Ok((
                    Box::new(agent_harness::fake_harness::FakeHarness::one_turn(turn)),
                    self.spawn_spec(cwd),
                ))
            }
        }

        let first_repository = TestRepository::new()?;
        let second_repository = TestRepository::new()?;
        let database = TestDatabase::new()?;
        let uri = database.uri();
        let mut core = Core::with_services(
            Box::new(TursoStore::open(&uri).await?),
            Box::new(ThreeAgentFactory),
        );
        let first_project = add_project(&mut core, first_repository.path().to_path_buf()).await?;
        let second_project = add_project(&mut core, second_repository.path().to_path_buf()).await?;
        let mut tasks = Vec::new();
        for (project, agent) in [
            (&first_project, Agent::Codex),
            (&first_project, Agent::OpenCode),
            (&second_project, Agent::Claude),
        ] {
            let draft = workspace_of(
                core.execute(Command::CreateDraft {
                    project_id: project.clone(),
                    title: format!("{} task", agent.as_str()),
                    agent,
                })
                .await?,
            );
            let id = TaskId::new(draft.task.id);
            let waiting = workspace_of(
                core.execute(Command::SendPrompt {
                    task_id: id.clone(),
                    input: PromptInput::text(format!("prompt for {}", agent.as_str())),
                })
                .await?,
            );
            assert_eq!(waiting.task.state, TaskState::Waiting);
            assert_eq!(waiting.task.project_id, project.as_str());
            tasks.push((
                id,
                agent,
                PathBuf::from(waiting.task.worktree_path.ok_or("missing worktree")?),
            ));
        }
        assert!(tasks.iter().all(|(id, _, _)| {
            core.task_workspace(id)
                .is_ok_and(|workspace| workspace.task.state == TaskState::Waiting)
        }));
        for (index, (id, agent, path)) in tasks.iter().enumerate().rev() {
            let denied = *agent == Agent::Claude;
            let completed = workspace_of(
                core.execute(Command::ResolveApproval {
                    task_id: id.clone(),
                    approval_id: ApprovalId::new("same-approval-id"),
                    decision: if denied {
                        Decision::Deny
                    } else {
                        Decision::Allow
                    },
                })
                .await?,
            );
            if denied {
                assert_ne!(completed.task.state, TaskState::Waiting);
                assert!(!path.join("agent-result.txt").exists());
            } else {
                assert_eq!(completed.task.state, TaskState::Review);
                assert_eq!(
                    std::fs::read_to_string(path.join("agent-result.txt"))?,
                    agent.as_str()
                );
            }
            let text = summaries(&completed).join("\n");
            assert_eq!(
                text.contains(&format!("{} finished", agent.as_str())),
                !denied
            );
            assert!(completed.transcript.iter().any(|entry| matches!(
                &entry.kind,
                agent_harness::EntryKind::Permission { approval_id, decision: Some(decision), .. }
                    if approval_id == "same-approval-id" && *decision == if denied { Decision::Deny } else { Decision::Allow }
            )));
            for (other_id, other_agent, other_path) in &tasks[..index] {
                assert_eq!(
                    core.task_workspace(other_id)?.task.state,
                    TaskState::Waiting
                );
                assert!(!other_path.join("agent-result.txt").exists());
                assert!(!text.contains(&format!("{} started", other_agent.as_str())));
            }
        }
        drop(core);
        let mut reopened = Core::load(
            Box::new(TursoStore::open(&uri).await?),
            Box::new(UnavailableHarnessFactory),
        )
        .await?;
        for (id, agent, _) in tasks {
            let restored = reopened.open_task_workspace(&id).await?;
            assert!(restored.transcript.iter().any(|entry| matches!(
                &entry.kind,
                agent_harness::EntryKind::Permission { approval_id, decision: Some(decision), .. }
                    if approval_id == "same-approval-id" && *decision == if agent == Agent::Claude { Decision::Deny } else { Decision::Allow }
            )));
            let text = summaries(&restored).join("\n");
            assert_eq!(
                text.contains(&format!("{} finished", agent.as_str())),
                agent != Agent::Claude
            );
            for other in [Agent::Codex, Agent::OpenCode, Agent::Claude] {
                if other != agent {
                    assert!(!text.contains(&format!("{} started", other.as_str())));
                }
            }
        }
        Ok(())
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
                agent: Agent::Codex,
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
                agent: Agent::Codex,
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

    #[tokio::test]
    async fn discarding_draft_removes_ended_terminals_from_layout()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::terminal::{SplitAxis, TerminalSplit};

        let repository = TestRepository::new()?;
        let mut core = Core::new();
        let project = add_project(&mut core, repository.path().to_path_buf()).await?;
        let CommandResult::TaskWorkspace(draft) = core
            .execute(Command::CreateDraft {
                project_id: project.clone(),
                title: "Draft".into(),
                agent: Agent::Codex,
            })
            .await?
        else {
            return Err("expected draft workspace".into());
        };
        let task = TaskId::new(draft.task.id);
        let shared = TerminalRecord::project_shared(
            TerminalId::new("shared"),
            project.clone(),
            "Shared",
            repository.path().to_path_buf(),
            "shell",
            1,
        );
        let scoped = TerminalRecord::project_shared(
            TerminalId::new("task"),
            project.clone(),
            "Task",
            repository.path().to_path_buf(),
            "shell",
            2,
        )
        .for_task(task.clone());
        core.record_terminal(&shared).await?;
        core.record_terminal(&scoped).await?;
        let layout = TerminalLayout {
            root: Some(TerminalSplit::Split {
                axis: SplitAxis::Horizontal,
                ratio: 500,
                first: Box::new(TerminalSplit::Pane {
                    terminal_id: "shared".into(),
                }),
                second: Box::new(TerminalSplit::Pane {
                    terminal_id: "task".into(),
                }),
            }),
            ..TerminalLayout::default()
        };
        core.save_terminal_layout(&project, layout).await?;
        assert!(matches!(
            core.execute(Command::DiscardDraft {
                task_id: task.clone()
            })
            .await,
            Err(CoreError::TaskTerminalsRunning)
        ));
        core.end_terminal(&scoped.id, None).await?;
        core.execute(Command::DiscardDraft { task_id: task })
            .await?;
        assert_eq!(
            core.terminal_layout(&project)?.root,
            Some(TerminalSplit::Pane {
                terminal_id: "shared".into()
            })
        );
        assert_eq!(core.terminals_for(&project).len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn deleting_a_saved_task_preserves_its_worktree_and_branch()
    -> Result<(), Box<dyn std::error::Error>> {
        let repository = TestRepository::new()?;
        let database = TestDatabase::new()?;
        let uri = database.uri();
        let mut core = Core::with_services(
            Box::new(TursoStore::open(&uri).await?),
            Box::new(FakeHarnessFactory::new(vec![vec![FakeTurn::new(vec![
                FakeStep::WriteFile {
                    path: "notes.txt".into(),
                    contents: "Keep this work".into(),
                },
            ])]])),
        );
        let project = add_project(&mut core, repository.path().to_path_buf()).await?;
        let draft = workspace_of(
            core.execute(Command::CreateDraft {
                project_id: project.clone(),
                title: "Keep work".into(),
                agent: Agent::Codex,
            })
            .await?,
        );
        let task_id = TaskId::new(draft.task.id);
        let task = workspace_of(
            core.execute(Command::SendPrompt {
                task_id: task_id.clone(),
                input: PromptInput::text("Edit notes"),
            })
            .await?,
        );
        assert_eq!(task.task.state, TaskState::Review);
        let worktree = PathBuf::from(task.task.worktree_path.ok_or("missing worktree")?);
        let branch = task.task.branch.ok_or("missing branch")?;
        assert!(!core.store.load_transcript(&task_id).await?.is_empty());
        assert!(matches!(
            core.execute(Command::DeleteTask {
                task_id: task_id.clone(),
                remove_worktree: true
            })
            .await,
            Err(CoreError::Git(GitError::DirtyWorktree { .. }))
        ));
        assert!(core.task_workspace(&task_id).is_ok());
        core.execute(Command::DeleteTask {
            task_id: task_id.clone(),
            remove_worktree: false,
        })
        .await?;
        assert!(core.task_list(&project)?.tasks.is_empty());
        assert!(core.store.load_transcript(&task_id).await?.is_empty());
        assert_eq!(
            std::fs::read_to_string(worktree.join("notes.txt"))?,
            "Keep this work"
        );
        assert!(git_in(&repository, &["branch", "--list", &branch]).contains(&branch));
        drop(core);
        let store = TursoStore::open(&uri).await?;
        assert!(store.load_tasks().await?.is_empty());
        assert!(store.load_sessions().await?.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn deleting_a_task_with_an_active_turn_is_rejected()
    -> Result<(), Box<dyn std::error::Error>> {
        let repository = TestRepository::new()?;
        let mut core = Core::with_services(
            Box::new(MemoryStore::new()),
            Box::new(FakeHarnessFactory::new(vec![vec![FakeTurn::new(vec![
                FakeStep::approval("approval", "Run"),
            ])]])),
        );
        let project = add_project(&mut core, repository.path().to_path_buf()).await?;
        let draft = workspace_of(
            core.execute(Command::CreateDraft {
                project_id: project,
                title: "Active".into(),
                agent: Agent::Codex,
            })
            .await?,
        );
        let task_id = TaskId::new(draft.task.id);
        let task = workspace_of(
            core.execute(Command::SendPrompt {
                task_id: task_id.clone(),
                input: PromptInput::text("go"),
            })
            .await?,
        );
        assert_eq!(task.task.state, TaskState::Waiting);
        assert!(matches!(
            core.execute(Command::DeleteTask {
                task_id: task_id.clone(),
                remove_worktree: false,
            })
            .await,
            Err(CoreError::TaskDeletionBusy { .. })
        ));
        assert_eq!(
            core.task_workspace(&task_id)?.task.state,
            TaskState::Waiting
        );
        Ok(())
    }

    #[tokio::test]
    async fn deleting_a_task_can_remove_a_clean_worktree_without_removing_its_branch()
    -> Result<(), Box<dyn std::error::Error>> {
        let repository = TestRepository::new()?;
        let mut core = Core::with_services(
            Box::new(MemoryStore::new()),
            Box::new(FakeHarnessFactory::new(vec![vec![FakeTurn::new(vec![])]])),
        );
        let project = add_project(&mut core, repository.path().to_path_buf()).await?;
        let draft = workspace_of(
            core.execute(Command::CreateDraft {
                project_id: project,
                title: "Clean".into(),
                agent: Agent::Codex,
            })
            .await?,
        );
        let task_id = TaskId::new(draft.task.id);
        let task = workspace_of(
            core.execute(Command::SendPrompt {
                task_id: task_id.clone(),
                input: PromptInput::text("hello"),
            })
            .await?,
        );
        let worktree = PathBuf::from(task.task.worktree_path.ok_or("missing worktree")?);
        let branch = task.task.branch.ok_or("missing branch")?;
        core.execute(Command::DeleteTask {
            task_id,
            remove_worktree: true,
        })
        .await?;
        assert!(!worktree.exists());
        assert!(git_in(&repository, &["branch", "--list", &branch]).contains(&branch));
        Ok(())
    }

    #[tokio::test]
    async fn failed_task_deletion_restores_checkout_and_ignored_files()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = TestRepository::new()?;
        let database = TestDatabase::new()?;
        let uri = database.uri();
        let mut core = Core::with_services(
            Box::new(TursoStore::open(&uri).await?),
            Box::new(FakeHarnessFactory::new(vec![vec![FakeTurn::new(vec![])]])),
        );
        let project = add_project(&mut core, fixture.path().to_path_buf()).await?;
        let draft = workspace_of(
            core.execute(Command::CreateDraft {
                project_id: project,
                title: "Recover deletion".into(),
                agent: Agent::Codex,
            })
            .await?,
        );
        let task_id = TaskId::new(draft.task.id);
        let workspace = workspace_of(
            core.execute(Command::SendPrompt {
                task_id: task_id.clone(),
                input: PromptInput::text("hello"),
            })
            .await?,
        );
        let worktree = PathBuf::from(workspace.task.worktree_path.ok_or("missing worktree")?);
        let repository = GitService::discover(fixture.path())?;
        std::fs::write(repository.git_dir.join("info/exclude"), "ignored.txt\n")?;
        std::fs::write(worktree.join("ignored.txt"), "important ignored data")?;
        let mut database_probe = toasty::Db::builder()
            .build(toasty_driver_turso::Turso::new(&uri)?.experimental_index_method(true))
            .await?;
        toasty::sql::statement("CREATE TRIGGER block_task_deletion BEFORE DELETE ON tasks BEGIN SELECT RAISE(ABORT, 'fixture deletion failure'); END").exec(&mut database_probe).await?;
        assert!(
            core.execute(Command::DeleteTask {
                task_id: task_id.clone(),
                remove_worktree: true
            })
            .await
            .is_err()
        );
        assert_eq!(
            std::fs::read_to_string(worktree.join("ignored.txt"))?,
            "important ignored data"
        );
        assert!(core.task(&task_id).is_ok());
        assert!(core.store.load_worktree_deletions().await?.is_empty());
        toasty::sql::statement("DROP TRIGGER block_task_deletion")
            .exec(&mut database_probe)
            .await?;
        core.execute(Command::DeleteTask {
            task_id: task_id.clone(),
            remove_worktree: true,
        })
        .await?;
        assert!(!worktree.exists());
        assert!(core.task(&task_id).is_err());
        assert!(core.store.load_worktree_deletions().await?.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn committed_task_deletion_recovers_if_journal_cleanup_fails()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = TestRepository::new()?;
        let database = TestDatabase::new()?;
        let uri = database.uri();
        let mut core = Core::with_services(
            Box::new(TursoStore::open(&uri).await?),
            Box::new(FakeHarnessFactory::new(vec![vec![FakeTurn::new(vec![])]])),
        );
        let project = add_project(&mut core, fixture.path().to_path_buf()).await?;
        let draft = workspace_of(
            core.execute(Command::CreateDraft {
                project_id: project,
                title: "Finish deletion".into(),
                agent: Agent::Codex,
            })
            .await?,
        );
        let task_id = TaskId::new(draft.task.id);
        let workspace = workspace_of(
            core.execute(Command::SendPrompt {
                task_id: task_id.clone(),
                input: PromptInput::text("hello"),
            })
            .await?,
        );
        let worktree = PathBuf::from(workspace.task.worktree_path.ok_or("missing worktree")?);
        let mut database_probe = toasty::Db::builder()
            .build(toasty_driver_turso::Turso::new(&uri)?.experimental_index_method(true))
            .await?;
        toasty::sql::statement("CREATE TRIGGER block_deletion_cleanup BEFORE DELETE ON worktree_deletions BEGIN SELECT RAISE(ABORT, 'fixture cleanup failure'); END").exec(&mut database_probe).await?;
        let error = core
            .execute(Command::DeleteTask {
                task_id: task_id.clone(),
                remove_worktree: true,
            })
            .await
            .err()
            .ok_or("expected cleanup failure")?;
        assert!(matches!(error, CoreError::TaskDeletedCleanupPending { .. }));
        assert!(core.task(&task_id).is_err());
        assert!(!worktree.exists());
        assert_eq!(core.store.load_worktree_deletions().await?.len(), 1);
        toasty::sql::statement("DROP TRIGGER block_deletion_cleanup")
            .exec(&mut database_probe)
            .await?;
        drop(core);
        let reopened = Core::load(
            Box::new(TursoStore::open(&uri).await?),
            Box::new(UnavailableHarnessFactory),
        )
        .await?;
        assert!(!reopened.tasks.contains_key(&task_id));
        assert!(reopened.store.load_worktree_deletions().await?.is_empty());
        Ok(())
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
                agent: Agent::Codex,
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
            Err(CoreError::QuitConfirmationRequired {
                live_tasks: 1,
                live_terminals: 0
            })
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
    async fn failed_session_record_save_stops_agent_and_allows_retry()
    -> Result<(), Box<dyn std::error::Error>> {
        let repository = TestRepository::new()?;
        let database = TestDatabase::new()?;
        let uri = database.uri();
        let turns = || vec![FakeTurn::new(vec![FakeStep::Message("done".into())])];
        let mut core = Core::with_services(
            Box::new(TursoStore::open(&uri).await?),
            Box::new(FakeHarnessFactory::new(vec![turns(), turns()])),
        );
        let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
        let draft = workspace_of(
            core.execute(Command::CreateDraft {
                project_id,
                agent: Agent::Codex,
                title: "Retry session".into(),
            })
            .await?,
        );
        let task_id = TaskId::new(draft.task.id);
        let mut database_probe = toasty::Db::builder()
            .build(toasty_driver_turso::Turso::new(&uri)?.experimental_index_method(true))
            .await?;
        toasty::sql::statement("CREATE TRIGGER block_session_start BEFORE INSERT ON sessions BEGIN SELECT RAISE(ABORT, 'session save blocked'); END")
            .exec(&mut database_probe).await?;
        let failure = core
            .execute(Command::SendPrompt {
                task_id: task_id.clone(),
                input: PromptInput::text("go"),
            })
            .await;
        assert!(failure.is_err());
        assert!(
            core.sessions
                .get(&task_id)
                .is_some_and(HarnessSession::is_closed)
        );
        assert!(core.store.load_sessions().await?.is_empty());
        toasty::sql::statement("DROP TRIGGER block_session_start")
            .exec(&mut database_probe)
            .await?;
        let workspace = workspace_of(
            core.execute(Command::SendPrompt {
                task_id,
                input: PromptInput::text("go"),
            })
            .await?,
        );
        assert!(
            summaries(&workspace)
                .iter()
                .any(|summary| summary.contains("done"))
        );
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn killed_deletion_recovers_worktree_before_and_after_commit()
    -> Result<(), Box<dyn std::error::Error>> {
        if let Some(uri) = std::env::var_os("SHEPHERD_DELETION_CRASH_DATABASE") {
            let uri = uri.into_string().map_err(|_| "database URI is not UTF-8")?;
            let repository_path = PathBuf::from(
                std::env::var_os("SHEPHERD_DELETION_CRASH_REPOSITORY")
                    .ok_or("missing repository")?,
            );
            let mut core = Core::with_services(
                Box::new(TursoStore::open(&uri).await?),
                Box::new(FakeHarnessFactory::new(vec![vec![FakeTurn::new(vec![])]])),
            );
            let project_id = add_project(&mut core, repository_path.clone()).await?;
            let draft = workspace_of(
                core.execute(Command::CreateDraft {
                    project_id,
                    title: "Crash deletion".into(),
                    agent: Agent::Codex,
                })
                .await?,
            );
            let task_id = TaskId::new(draft.task.id);
            let workspace = workspace_of(
                core.execute(Command::SendPrompt {
                    task_id: task_id.clone(),
                    input: PromptInput::text("fixture"),
                })
                .await?,
            );
            let worktree = PathBuf::from(workspace.task.worktree_path.ok_or("missing worktree")?);
            let repository = GitService::discover(&repository_path)?;
            std::fs::write(repository.git_dir.join("info/exclude"), "ignored.txt\n")?;
            std::fs::write(worktree.join("ignored.txt"), "important ignored data")?;
            core.execute(Command::DeleteTask {
                task_id,
                remove_worktree: true,
            })
            .await?;
            return Err("deletion fixture unexpectedly finished".into());
        }

        use std::os::unix::process::ExitStatusExt as _;
        use std::time::Duration;
        for phase in ["staged", "committed"] {
            let repository = TestRepository::new()?;
            let database = TestDatabase::new()?;
            let uri = database.uri();
            let ready = database.path().join("deletion-ready");
            let mut child = tokio::process::Command::new(std::env::current_exe()?)
                .args([
                    "--exact",
                    "tests::killed_deletion_recovers_worktree_before_and_after_commit",
                    "--nocapture",
                ])
                .env("SHEPHERD_DELETION_CRASH_DATABASE", &uri)
                .env("SHEPHERD_DELETION_CRASH_REPOSITORY", repository.path())
                .env("SHEPHERD_DELETION_CRASH_READY", &ready)
                .env("SHEPHERD_DELETION_CRASH_PHASE", phase)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .kill_on_drop(true)
                .spawn()?;
            let task_id = tokio::time::timeout(Duration::from_secs(20), async {
                loop {
                    match std::fs::read_to_string(&ready) {
                        Ok(id) if !id.is_empty() => {
                            break Ok::<_, Box<dyn std::error::Error>>(TaskId::new(id));
                        }
                        Ok(_) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => break Err(error.into()),
                    }
                    if let Some(status) = child.try_wait()? {
                        break Err(format!(
                            "deletion fixture exited before becoming ready: {status}"
                        )
                        .into());
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await??;
            child.kill().await?;
            assert_eq!(child.wait().await?.signal(), Some(9));
            let store = TursoStore::open(&uri).await?;
            let deletion = store
                .load_worktree_deletions()
                .await?
                .into_iter()
                .next()
                .ok_or("missing deletion journal")?;
            assert_eq!(deletion.task_id, task_id);
            assert_eq!(
                std::fs::read_to_string(deletion.staged_path.join("ignored.txt"))?,
                "important ignored data"
            );
            drop(store);
            let core = Core::load(
                Box::new(TursoStore::open(&uri).await?),
                Box::new(UnavailableHarnessFactory),
            )
            .await?;
            assert!(core.store.load_worktree_deletions().await?.is_empty());
            assert!(!deletion.staged_path.exists());
            if phase == "staged" {
                assert!(core.tasks.contains_key(&task_id));
                assert_eq!(
                    std::fs::read_to_string(deletion.original_path.join("ignored.txt"))?,
                    "important ignored data"
                );
            } else {
                assert!(!core.tasks.contains_key(&task_id));
                assert!(!deletion.original_path.exists());
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn startup_recovers_staged_worktree_deletions_conservatively()
    -> Result<(), Box<dyn std::error::Error>> {
        for scenario in [
            "before-move",
            "restore",
            "finish",
            "finished",
            "wrong-identity",
            "both-paths",
            "dirty",
            "locked",
        ] {
            let fixture = TestRepository::new()?;
            let repository = GitService::discover(fixture.path())?;
            let database = TestDatabase::new()?;
            let store = TursoStore::open(&database.uri()).await?;
            let project = projects::Project::new(repository.root.clone());
            let mut task = Task::draft(project.id.clone(), "Recovery", &repository.git_dir);
            let original = task.proposed_worktree_path.clone();
            GitService.create_worktree(
                &repository,
                &original,
                &task.proposed_branch,
                &repository.default_branch,
            )?;
            task.worktree_path = Some(original.clone());
            task.branch = Some(task.proposed_branch.clone());
            std::fs::write(repository.git_dir.join("info/exclude"), "ignored.txt\n")?;
            std::fs::write(original.join("ignored.txt"), "preserve me")?;
            store.save_project(&project).await?;
            store.save_task(&task).await?;
            let identity = uuid::Uuid::new_v4().to_string();
            let git_dir = GitService.mark_worktree_deletion(&repository, &original, &identity)?;
            let staged = repository
                .git_dir
                .join("shepherd/worktrees")
                .join(format!(".shepherd-deletion-{identity}"));
            let deletion = WorktreeDeletion {
                task_id: task.id.clone(),
                repository_root: repository.root.clone(),
                original_path: original.clone(),
                staged_path: staged.clone(),
                worktree_git_dir: git_dir.clone(),
                identity: identity.clone(),
            };
            store.save_worktree_deletion(&deletion).await?;
            if scenario != "before-move" {
                GitService.move_worktree(&repository, &original, &staged)?;
            }
            if matches!(scenario, "finish" | "finished" | "dirty") {
                store.delete_task(&task.id, None).await?;
            }
            if scenario == "finished" {
                GitService.remove_clean_worktree(&repository, &staged)?;
            }
            if scenario == "both-paths" {
                std::fs::create_dir(&original)?;
            }
            if scenario == "dirty" {
                std::fs::write(staged.join("new-file"), "do not remove")?;
            }
            if scenario == "wrong-identity" {
                GitService.clear_worktree_deletion_marker(
                    &repository,
                    &staged,
                    &git_dir,
                    &identity,
                )?;
            }
            drop(store);
            let lock = if scenario == "locked" {
                Some(GitService.lock_worktree_deletions(&repository)?)
            } else {
                None
            };
            let result = Core::load(
                Box::new(TursoStore::open(&database.uri()).await?),
                Box::new(UnavailableHarnessFactory),
            )
            .await;
            if scenario == "locked" {
                assert!(result.is_err());
                assert_eq!(
                    std::fs::read_to_string(staged.join("ignored.txt"))?,
                    "preserve me"
                );
                drop(lock);
                let core = Core::load(
                    Box::new(TursoStore::open(&database.uri()).await?),
                    Box::new(UnavailableHarnessFactory),
                )
                .await?;
                assert!(core.store.load_worktree_deletions().await?.is_empty());
                assert_eq!(
                    std::fs::read_to_string(original.join("ignored.txt"))?,
                    "preserve me"
                );
                continue;
            }
            if matches!(scenario, "wrong-identity" | "both-paths" | "dirty") {
                assert!(result.is_err());
                assert_eq!(
                    std::fs::read_to_string(staged.join("ignored.txt"))?,
                    "preserve me"
                );
                continue;
            }
            let core = result?;
            assert!(core.store.load_worktree_deletions().await?.is_empty());
            if matches!(scenario, "finish" | "finished") {
                assert!(!original.exists() && !staged.exists());
                assert!(!core.tasks.contains_key(&task.id));
            } else {
                assert_eq!(
                    std::fs::read_to_string(original.join("ignored.txt"))?,
                    "preserve me"
                );
                assert!(!staged.exists());
                assert!(core.tasks.contains_key(&task.id));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn failed_unrecorded_session_cleanup_is_retried_before_new_agent()
    -> Result<(), Box<dyn std::error::Error>> {
        use agent_harness::fake_harness::FakeHarness;
        use agent_harness::{AgentHarness, Capabilities, HarnessError, HarnessResult, SpawnSpec};
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct Factory {
            starts: Arc<AtomicUsize>,
            stops: Arc<AtomicUsize>,
        }
        impl HarnessFactory for Factory {
            fn create(&self) -> Result<Box<dyn AgentHarness>, CoreError> {
                self.starts.fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(FailingShutdown {
                    inner: FakeHarness::one_turn(FakeTurn::new(vec![FakeStep::Message(
                        "done".into(),
                    )])),
                    stops: self.stops.clone(),
                }))
            }
        }
        struct FailingShutdown {
            inner: FakeHarness,
            stops: Arc<AtomicUsize>,
        }
        #[async_trait::async_trait]
        impl AgentHarness for FailingShutdown {
            fn events(&mut self) -> &mut mpsc::UnboundedReceiver<HarnessEvent> {
                self.inner.events()
            }
            async fn start(&mut self, spec: SpawnSpec) -> HarnessResult<Capabilities> {
                self.inner.start(spec).await
            }
            async fn open_session(
                &mut self,
                resume: Option<SessionId>,
            ) -> HarnessResult<SessionId> {
                self.inner.open_session(resume).await
            }
            async fn prompt(&mut self, input: PromptInput) -> HarnessResult<()> {
                self.inner.prompt(input).await
            }
            async fn cancel(&mut self) -> HarnessResult<()> {
                self.inner.cancel().await
            }
            async fn respond(&mut self, id: ApprovalId, decision: Decision) -> HarnessResult<()> {
                self.inner.respond(id, decision).await
            }
            async fn shutdown(&mut self) -> HarnessResult<()> {
                if self.stops.fetch_add(1, Ordering::SeqCst) < 2 {
                    return Err(HarnessError::Protocol("fixture shutdown failed".into()));
                }
                self.inner.shutdown().await
            }
        }
        let repository = TestRepository::new()?;
        let database = TestDatabase::new()?;
        let uri = database.uri();
        let starts = Arc::new(AtomicUsize::new(0));
        let stops = Arc::new(AtomicUsize::new(0));
        let mut core = Core::with_services(
            Box::new(TursoStore::open(&uri).await?),
            Box::new(Factory {
                starts: starts.clone(),
                stops: stops.clone(),
            }),
        );
        let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
        let draft = workspace_of(
            core.execute(Command::CreateDraft {
                project_id,
                agent: Agent::Codex,
                title: "Retry cleanup".into(),
            })
            .await?,
        );
        let task_id = TaskId::new(draft.task.id);
        let mut database_probe = toasty::Db::builder()
            .build(toasty_driver_turso::Turso::new(&uri)?.experimental_index_method(true))
            .await?;
        toasty::sql::statement("CREATE TRIGGER block_session_start BEFORE INSERT ON sessions BEGIN SELECT RAISE(ABORT, 'session save blocked'); END").exec(&mut database_probe).await?;
        let prompt = || Command::SendPrompt {
            task_id: task_id.clone(),
            input: PromptInput::text("go"),
        };
        let error = core
            .execute(prompt())
            .await
            .err()
            .ok_or("expected save and cleanup failure")?;
        assert!(matches!(error, CoreError::SessionRecordCleanup { .. }));
        assert!(
            core.sessions
                .get(&task_id)
                .is_some_and(|session| session.has_failed() && !session.is_closed())
        );
        toasty::sql::statement("DROP TRIGGER block_session_start")
            .exec(&mut database_probe)
            .await?;
        assert!(core.execute(prompt()).await.is_err());
        assert_eq!(starts.load(Ordering::SeqCst), 1);
        let workspace = workspace_of(core.execute(prompt()).await?);
        assert_eq!(starts.load(Ordering::SeqCst), 2);
        assert_eq!(stops.load(Ordering::SeqCst), 3);
        assert!(
            summaries(&workspace)
                .iter()
                .any(|summary| summary.contains("done"))
        );
        Ok(())
    }

    #[tokio::test]
    async fn quit_retries_agent_shutdown_after_failure() -> Result<(), Box<dyn std::error::Error>> {
        use agent_harness::{AgentHarness, Capabilities, HarnessError, HarnessResult, SpawnSpec};
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct RetryShutdown {
            receiver: mpsc::UnboundedReceiver<HarnessEvent>,
            attempts: Arc<AtomicUsize>,
        }
        #[async_trait::async_trait]
        impl AgentHarness for RetryShutdown {
            fn events(&mut self) -> &mut mpsc::UnboundedReceiver<HarnessEvent> {
                &mut self.receiver
            }
            async fn start(&mut self, _: SpawnSpec) -> HarnessResult<Capabilities> {
                Err(HarnessError::NotStarted)
            }
            async fn open_session(&mut self, _: Option<SessionId>) -> HarnessResult<SessionId> {
                Err(HarnessError::NotStarted)
            }
            async fn prompt(&mut self, _: PromptInput) -> HarnessResult<()> {
                Err(HarnessError::NotStarted)
            }
            async fn cancel(&mut self) -> HarnessResult<()> {
                Err(HarnessError::NotStarted)
            }
            async fn respond(&mut self, _: ApprovalId, _: Decision) -> HarnessResult<()> {
                Err(HarnessError::NotStarted)
            }
            async fn shutdown(&mut self) -> HarnessResult<()> {
                if self.attempts.fetch_add(1, Ordering::SeqCst) < 4 {
                    Err(HarnessError::Protocol("fixture shutdown failed".into()))
                } else {
                    Ok(())
                }
            }
        }
        let directory = TestDirectory::new()?;
        let mut core = Core::with_services(
            Box::new(MemoryStore::new()),
            Box::new(UnavailableHarnessFactory),
        );
        let project_id = add_project(&mut core, directory.path().to_path_buf()).await?;
        let draft = workspace_of(
            core.execute(Command::CreateDraft {
                project_id,
                agent: Agent::Codex,
                title: "Retry quit".into(),
            })
            .await?,
        );
        let task_id = TaskId::new(draft.task.id);
        let (sender, receiver) = mpsc::unbounded_channel();
        let attempts = Arc::new(AtomicUsize::new(0));
        let mut session = HarnessSession::new(Box::new(RetryShutdown {
            receiver,
            attempts: attempts.clone(),
        }));
        sender.send(HarnessEvent::ConnectionFailed {
            kind: FailureKind::Protocol,
            text: "fixture disconnect".into(),
        })?;
        session.pump();
        assert!(session.is_closed());
        assert!(session.needs_shutdown());
        core.sessions.insert(task_id.clone(), session);
        assert!(core.ensure_session(&task_id, None).await.is_err());
        assert!(core.sessions.contains_key(&task_id));
        assert!(
            core.execute(Command::DeleteTask {
                task_id: task_id.clone(),
                remove_worktree: false
            })
            .await
            .is_err()
        );
        assert!(core.task(&task_id).is_ok());
        core.tasks.get_mut(&task_id).ok_or("missing task")?.state = TaskState::Review;
        assert!(core.archive_reviewed(vec![task_id.clone()]).await.is_err());
        assert_eq!(core.task(&task_id)?.state, TaskState::Review);
        assert!(core.execute(Command::Quit { force: true }).await.is_err());
        assert_eq!(core.shutdown_state(), ShutdownState::Draining);
        assert!(core.sessions.contains_key(&task_id));
        assert!(!core.shutdown_events.contains_key(&task_id));
        assert!(matches!(
            core.execute(Command::Quit { force: true }).await?,
            CommandResult::Quit(_)
        ));
        assert_eq!(attempts.load(Ordering::SeqCst), 5);
        assert_eq!(core.shutdown_state(), ShutdownState::Closed);
        Ok(())
    }

    #[tokio::test]
    async fn failed_shutdown_persistence_keeps_the_session_for_retry()
    -> Result<(), Box<dyn std::error::Error>> {
        let repository = TestRepository::new()?;
        let mut core = Core::with_services(
            Box::new(MemoryStore::new()),
            Box::new(FakeHarnessFactory::new(vec![vec![FakeTurn::new(vec![
                FakeStep::Message("working".into()),
                FakeStep::approval("approval", "Run tests"),
            ])]])),
        );
        let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
        let draft = workspace_of(
            core.execute(Command::CreateDraft {
                project_id,
                agent: Agent::Codex,
                title: "Retry shutdown".into(),
            })
            .await?,
        );
        let task_id = TaskId::new(draft.task.id);
        let workspace = workspace_of(
            core.execute(Command::SendPrompt {
                task_id: task_id.clone(),
                input: PromptInput::text("go"),
            })
            .await?,
        );
        assert_eq!(workspace.task.state, TaskState::Waiting);
        core.sessions
            .get_mut(&task_id)
            .ok_or("missing session")?
            .restore_transcript(workspace.transcript);
        let record = core
            .session_records
            .get_mut(&task_id)
            .ok_or("missing session record")?;
        let original_id = std::mem::replace(&mut record.id, "missing-session".into());
        assert!(core.execute(Command::Quit { force: true }).await.is_err());
        assert_eq!(core.shutdown_state(), ShutdownState::Draining);
        assert_eq!(core.task(&task_id)?.state, TaskState::Waiting);
        assert!(core.sessions.contains_key(&task_id));
        assert!(matches!(
            core.execute(Command::AddProject {
                path: repository.path().to_path_buf()
            })
            .await,
            Err(CoreError::NotRunning {
                state: ShutdownState::Draining
            })
        ));
        core.session_records
            .get_mut(&task_id)
            .ok_or("missing session record")?
            .id = original_id;
        let CommandResult::Quit(summary) = core.execute(Command::Quit { force: true }).await?
        else {
            return Err("expected quit summary".into());
        };
        assert_eq!(summary.interrupted_tasks, 1);
        assert_eq!(core.task(&task_id)?.state, TaskState::Interrupted);
        assert!(core.sessions.is_empty());
        let saved = core.store.load_tasks().await?;
        assert_eq!(
            saved.first().ok_or("missing task")?.state,
            TaskState::Interrupted
        );
        assert!(!core.store.load_transcript(&task_id).await?.is_empty());
        Ok(())
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
                    agent: Agent::Codex,
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
                    input: PromptInput::text("go"),
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

    #[cfg(unix)]
    #[tokio::test]
    async fn abrupt_process_death_recovers_the_waiting_task()
    -> Result<(), Box<dyn std::error::Error>> {
        if let Some(uri) = std::env::var_os("SHEPHERD_CRASH_FIXTURE_DATABASE") {
            let uri = uri.into_string().map_err(|_| "database URI is not UTF-8")?;
            let repository = PathBuf::from(
                std::env::var_os("SHEPHERD_CRASH_FIXTURE_REPOSITORY")
                    .ok_or("missing repository")?,
            );
            let ready = PathBuf::from(
                std::env::var_os("SHEPHERD_CRASH_FIXTURE_READY").ok_or("missing ready path")?,
            );
            let mut core = Core::with_services(
                Box::new(TursoStore::open(&uri).await?),
                Box::new(FakeHarnessFactory::new(vec![vec![FakeTurn::new(vec![
                    FakeStep::Message("unfinished answer".into()),
                    FakeStep::TerminalError {
                        id: "crash-terminal".into(),
                        text: "Agent terminal error: missing exit status".into(),
                    },
                    FakeStep::approval("crash-approval", "Run tests"),
                ])]])),
            );
            let project = add_project(&mut core, repository).await?;
            let draft = workspace_of(
                core.execute(Command::CreateDraft {
                    agent: Agent::Codex,
                    project_id: project,
                    title: "Crash fixture".into(),
                })
                .await?,
            );
            let task_id = TaskId::new(draft.task.id);
            let waiting = workspace_of(
                core.execute(Command::SendPrompt {
                    task_id: task_id.clone(),
                    input: PromptInput::text("persist this prompt"),
                })
                .await?,
            );
            assert_eq!(waiting.task.state, TaskState::Waiting);
            let temporary = ready.with_extension("tmp");
            std::fs::write(&temporary, task_id.as_str())?;
            std::fs::rename(temporary, ready)?;
            std::future::pending::<()>().await;
            return Ok(());
        }

        use std::os::unix::process::ExitStatusExt as _;
        let repository = TestRepository::new()?;
        let database = TestDatabase::new()?;
        let uri = database.uri();
        let ready = database.path().join("crash-fixture-ready");
        let mut child = tokio::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "tests::abrupt_process_death_recovers_the_waiting_task",
                "--nocapture",
            ])
            .env("SHEPHERD_CRASH_FIXTURE_DATABASE", &uri)
            .env("SHEPHERD_CRASH_FIXTURE_REPOSITORY", repository.path())
            .env("SHEPHERD_CRASH_FIXTURE_READY", &ready)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let task_id = tokio::time::timeout(std::time::Duration::from_secs(20), async {
            loop {
                match std::fs::read_to_string(&ready) {
                    Ok(id) => break Ok::<_, Box<dyn std::error::Error>>(TaskId::new(id)),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => break Err(error.into()),
                }
                if let Some(status) = child.try_wait()? {
                    break Err(
                        format!("crash fixture exited before becoming ready: {status}").into(),
                    );
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await??;
        child.kill().await?;
        let status = child.wait().await?;
        assert_eq!(status.signal(), Some(9));
        let mut recovered = Core::load(
            Box::new(TursoStore::open(&uri).await?),
            Box::new(FakeHarnessFactory::new(vec![vec![FakeTurn::new(vec![
                FakeStep::Message("answer after crash".into()),
            ])]])),
        )
        .await?;
        let workspace = recovered.open_task_workspace(&task_id).await?;
        assert_eq!(workspace.task.state, TaskState::Interrupted);
        let text = summaries(&workspace).join("\n");
        assert!(text.contains("persist this prompt"));
        assert!(workspace.transcript.iter().any(|entry| matches!(&entry.kind, agent_harness::EntryKind::Notice { text, is_error: true } if text.contains("missing exit status"))));
        assert!(!text.contains("unfinished answer"));
        assert!(!recovered.sessions.contains_key(&task_id));
        recovered
            .execute(Command::ResumeTask {
                task_id: task_id.clone(),
            })
            .await?;
        let workspace = workspace_of(
            recovered
                .execute(Command::SendPrompt {
                    task_id: task_id.clone(),
                    input: PromptInput::text("continue after crash"),
                })
                .await?,
        );
        assert!(workspace.transcript.iter().any(|entry| matches!(&entry.kind, agent_harness::EntryKind::Notice { text, is_error: true } if text.contains("missing exit status"))));
        assert!(
            summaries(&workspace)
                .iter()
                .any(|entry| entry.contains("answer after crash"))
        );
        let sequences: Vec<_> = workspace
            .transcript
            .iter()
            .map(|entry| entry.sequence)
            .collect();
        assert!(
            sequences
                .windows(2)
                .all(|pair| matches!(pair, [first, second] if first < second))
        );
        drop(recovered);
        let mut reopened = Core::load(
            Box::new(TursoStore::open(&uri).await?),
            Box::new(UnavailableHarnessFactory),
        )
        .await?;
        let workspace = reopened.open_task_workspace(&task_id).await?;
        assert!(workspace.transcript.iter().any(|entry| matches!(&entry.kind, agent_harness::EntryKind::Notice { text, is_error: true } if text.contains("missing exit status"))));
        assert!(
            summaries(&workspace)
                .iter()
                .any(|entry| entry.contains("answer after crash"))
        );
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
                    agent: Agent::Codex,
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
                    input: PromptInput::text("fix it"),
                })
                .await?,
        );
        assert_eq!(workspace.task.state, TaskState::Waiting);
        assert_eq!(scenario.trace().matches("event: TaskNeedsInput").count(), 1);
        scenario.core_mut().pump().await?;
        assert!(
            !scenario
                .core_mut()
                .drain_events()
                .iter()
                .any(|event| matches!(event, Event::TaskNeedsInput { .. }))
        );
        assert_eq!(
            scenario.core().waiting_task_title(&task_id)?,
            Some("Fix the parser")
        );
        assert_eq!(
            scenario
                .core()
                .project_snapshot(scenario.core().project(&project_id)?)
                .running_task_count,
            1
        );
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
        assert_eq!(scenario.core().waiting_task_title(&task_id)?, None);
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
                    input: PromptInput::text("go again"),
                })
                .await?,
        );
        assert_eq!(workspace.task.state, TaskState::Waiting);
        assert_eq!(scenario.trace().matches("event: TaskNeedsInput").count(), 2);

        let refused = scenario.execute(Command::Quit { force: false }).await;
        assert!(matches!(
            refused,
            Err(CoreError::QuitConfirmationRequired {
                live_tasks: 1,
                live_terminals: 0
            })
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

        let workspace = scenario.core_mut().open_task_workspace(&task_id).await?;
        assert_eq!(workspace.task.state, TaskState::Interrupted);
        assert_eq!(
            workspace.recovery,
            Some(crate::snapshots::SessionRecovery::Resume)
        );
        assert!(
            workspace
                .capabilities
                .as_ref()
                .is_some_and(|capabilities| capabilities.resume)
        );
        assert_eq!(
            summaries(&workspace),
            [
                "you: fix it",
                "agent: working on it",
                "approval: Run tests (Allow)",
                "agent: all done",
                "you: go again",
            ]
        );
        let saved = workspace.transcript;
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
        assert!(workspace.transcript.starts_with(&saved));
        assert!(workspace.transcript.last().is_some_and(|entry| {
            entry
                .summary()
                .contains("resumed the previous agent session")
        }));

        let workspace = workspace_of(
            scenario
                .execute(Command::SendPrompt {
                    task_id: task_id.clone(),
                    input: PromptInput::text("continue"),
                })
                .await?,
        );
        assert_eq!(workspace.task.state, TaskState::Ready);
        assert!(workspace.transcript.starts_with(&saved));
        assert_eq!(
            workspace.transcript.last().map(Entry::summary),
            Some("agent: resumed answer".to_string())
        );
        let store = TursoStore::open(&uri).await?;
        assert_eq!(store.load_transcript(&task_id).await?, workspace.transcript);
        Ok(())
    }

    #[tokio::test]
    async fn approval_errors_fold_queued_disconnect_and_superseding_request()
    -> Result<(), Box<dyn std::error::Error>> {
        use agent_harness::{
            AgentHarness, ApprovalRequest, Capabilities, DeltaKind, FailureKind, HarnessError,
            HarnessResult, ItemId, SpawnSpec, ToolCallId, TurnId,
        };
        use tokio::sync::mpsc;
        struct Factory {
            stale: bool,
            withdrawn: bool,
        }
        impl HarnessFactory for Factory {
            fn create(&self) -> Result<Box<dyn AgentHarness>, CoreError> {
                let (sender, receiver) = mpsc::unbounded_channel();
                Ok(Box::new(FailingApproval {
                    sender,
                    receiver,
                    stale: self.stale,
                    withdrawn: self.withdrawn,
                }))
            }
            fn launch(
                &self,
                _: Agent,
                cwd: PathBuf,
            ) -> Result<(Box<dyn AgentHarness>, SpawnSpec), CoreError> {
                Ok((self.create()?, self.spawn_spec(cwd)))
            }
        }
        struct FailingApproval {
            sender: mpsc::UnboundedSender<HarnessEvent>,
            receiver: mpsc::UnboundedReceiver<HarnessEvent>,
            stale: bool,
            withdrawn: bool,
        }
        #[async_trait::async_trait]
        impl AgentHarness for FailingApproval {
            fn events(&mut self) -> &mut mpsc::UnboundedReceiver<HarnessEvent> {
                &mut self.receiver
            }
            async fn start(&mut self, _: SpawnSpec) -> HarnessResult<Capabilities> {
                Ok(Capabilities::default())
            }
            async fn open_session(&mut self, _: Option<SessionId>) -> HarnessResult<SessionId> {
                Ok(SessionId::new("fixture"))
            }
            async fn prompt(&mut self, _: PromptInput) -> HarnessResult<()> {
                self.sender
                    .send(HarnessEvent::TurnStarted {
                        turn_id: TurnId::new("turn"),
                    })
                    .map_err(|_| HarnessError::ChannelClosed)?;
                self.sender
                    .send(HarnessEvent::ApprovalRequested {
                        request: ApprovalRequest {
                            approval_id: ApprovalId::new("approval"),
                            tool_call_id: ToolCallId::new("tool"),
                            title: "Run tests".into(),
                            arguments: serde_json::Value::Null,
                            paths: Vec::new(),
                            patch: None,
                            choices: None,
                        },
                    })
                    .map_err(|_| HarnessError::ChannelClosed)?;
                Ok(())
            }
            async fn respond(&mut self, _: ApprovalId, _: Decision) -> HarnessResult<()> {
                if self.stale {
                    let settled = if self.withdrawn {
                        HarnessEvent::ApprovalWithdrawn {
                            approval_id: ApprovalId::new("approval"),
                        }
                    } else {
                        HarnessEvent::ApprovalResolved {
                            approval_id: ApprovalId::new("approval"),
                            decision: Decision::Deny,
                            source: agent_harness::ApprovalSource::Agent,
                        }
                    };
                    self.sender
                        .send(settled)
                        .map_err(|_| HarnessError::ChannelClosed)?;
                    self.sender
                        .send(HarnessEvent::ApprovalRequested {
                            request: ApprovalRequest {
                                approval_id: ApprovalId::new("next"),
                                tool_call_id: ToolCallId::new("next-tool"),
                                title: "Next request".into(),
                                arguments: serde_json::Value::Null,
                                paths: Vec::new(),
                                patch: None,
                                choices: None,
                            },
                        })
                        .map_err(|_| HarnessError::ChannelClosed)?;
                    return Err(HarnessError::NoPendingApproval);
                }
                for _ in 0..300 {
                    self.sender
                        .send(HarnessEvent::Delta {
                            item_id: ItemId::new("stream"),
                            kind: DeltaKind::Message,
                            text: "x".into(),
                            stream_permit: None,
                        })
                        .map_err(|_| HarnessError::ChannelClosed)?;
                }
                self.sender
                    .send(HarnessEvent::ApprovalWithdrawn {
                        approval_id: ApprovalId::new("approval"),
                    })
                    .map_err(|_| HarnessError::ChannelClosed)?;
                self.sender
                    .send(HarnessEvent::ConnectionFailed {
                        kind: FailureKind::Protocol,
                        text: "approval delivery failed".into(),
                    })
                    .map_err(|_| HarnessError::ChannelClosed)?;
                Err(HarnessError::Reported {
                    kind: FailureKind::Protocol,
                    message: "approval delivery failed".into(),
                })
            }
            async fn cancel(&mut self) -> HarnessResult<()> {
                Ok(())
            }
            async fn shutdown(&mut self) -> HarnessResult<()> {
                Ok(())
            }
        }
        for (stale, was_withdrawn) in [(false, false), (true, false), (true, true)] {
            let repository = TestRepository::new()?;
            let database = TestDatabase::new()?;
            let uri = database.uri();
            let mut core = Core::with_services(
                Box::new(TursoStore::open(&uri).await?),
                Box::new(Factory {
                    stale,
                    withdrawn: was_withdrawn,
                }),
            );
            let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
            let draft = workspace_of(
                core.execute(Command::CreateDraft {
                    project_id,
                    agent: Agent::OpenCode,
                    title: "Approval failure".into(),
                })
                .await?,
            );
            let task_id = TaskId::new(draft.task.id);
            let workspace = workspace_of(
                core.execute(Command::SendPrompt {
                    task_id: task_id.clone(),
                    input: PromptInput::text("go"),
                })
                .await?,
            );
            assert_eq!(workspace.task.state, TaskState::Waiting);
            let error = core
                .execute(Command::ResolveApproval {
                    task_id: task_id.clone(),
                    approval_id: ApprovalId::new("approval"),
                    decision: Decision::Allow,
                })
                .await;
            assert!(error.is_err());
            let workspace = core.task_workspace(&task_id)?;
            if stale {
                assert!(matches!(
                    error,
                    Err(CoreError::Harness(HarnessError::NoPendingApproval))
                ));
                assert_eq!(workspace.task.state, TaskState::Waiting);
                assert_eq!(
                    core.sessions
                        .get(&task_id)
                        .and_then(HarnessSession::pending_approval)
                        .map(ApprovalId::as_str),
                    Some("next")
                );
                assert!(workspace.transcript.iter().any(|entry| matches!(&entry.kind, agent_harness::EntryKind::Permission { approval_id, decision, withdrawn, .. } if approval_id == "approval" && *withdrawn == was_withdrawn && (was_withdrawn || *decision == Some(Decision::Deny)))));
                assert!(workspace.transcript.iter().any(|entry| matches!(&entry.kind, agent_harness::EntryKind::Permission { approval_id, decision: None, .. } if approval_id == "next")));
                let store = TursoStore::open(&uri).await?;
                let saved = store.load_transcript(&task_id).await?;
                assert!(saved.iter().any(|entry| matches!(&entry.kind, agent_harness::EntryKind::Permission { approval_id, decision, withdrawn, .. } if approval_id == "approval" && *withdrawn == was_withdrawn && (was_withdrawn || *decision == Some(Decision::Deny)))));
                assert!(!saved.iter().any(|entry| matches!(&entry.kind, agent_harness::EntryKind::Permission { approval_id, .. } if approval_id == "next")));
                drop(store);
                drop(core);
                let mut reopened = Core::load(
                    Box::new(TursoStore::open(&uri).await?),
                    Box::new(Factory {
                        stale,
                        withdrawn: was_withdrawn,
                    }),
                )
                .await?;
                let restored = reopened.open_task_workspace(&task_id).await?;
                assert_eq!(restored.task.state, TaskState::Interrupted);
                assert!(restored.transcript.iter().any(|entry| matches!(&entry.kind, agent_harness::EntryKind::Permission { approval_id, decision, withdrawn, .. } if approval_id == "approval" && *withdrawn == was_withdrawn && (was_withdrawn || *decision == Some(Decision::Deny)))));
                assert!(!restored.transcript.iter().any(|entry| matches!(&entry.kind, agent_harness::EntryKind::Permission { approval_id, .. } if approval_id == "next")));
                continue;
            }
            assert_eq!(workspace.task.state, TaskState::Failed);
            assert!(workspace.transcript.iter().any(|entry| matches!(&entry.kind, agent_harness::EntryKind::Permission { approval_id, withdrawn: true, .. } if approval_id == "approval")));
            assert_eq!(workspace.transcript.iter().filter(|entry| matches!(&entry.kind, agent_harness::EntryKind::Failure { text, .. } if text == "approval delivery failed")).count(), 1);
            assert!(
                core.sessions
                    .get(&task_id)
                    .is_some_and(HarnessSession::is_closed)
            );
            drop(core);
            let mut reopened = Core::load(
                Box::new(TursoStore::open(&uri).await?),
                Box::new(Factory {
                    stale,
                    withdrawn: was_withdrawn,
                }),
            )
            .await?;
            let restored = reopened.open_task_workspace(&task_id).await?;
            assert_eq!(restored.task.state, TaskState::Failed);
            assert!(restored.transcript.iter().any(|entry| matches!(&entry.kind, agent_harness::EntryKind::Permission { approval_id, withdrawn: true, .. } if approval_id == "approval")));
        }
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
                agent: Agent::Codex,
                project_id,
                title: "Deny me".to_string(),
            })
            .await?,
        );
        let task_id = TaskId::new(draft.task.id.clone());
        let workspace = workspace_of(
            core.execute(Command::SendPrompt {
                task_id: task_id.clone(),
                input: PromptInput::text("go"),
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
                agent: Agent::Codex,
                project_id,
                title: "Cancel me".to_string(),
            })
            .await?,
        );
        let task_id = TaskId::new(draft.task.id.clone());

        let workspace = workspace_of(
            core.execute(Command::SendPrompt {
                task_id: task_id.clone(),
                input: PromptInput::text("go"),
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
                input: PromptInput::text("try again"),
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
    async fn editing_then_restoring_a_file_does_not_enter_review()
    -> Result<(), Box<dyn std::error::Error>> {
        let repository = TestRepository::new()?;
        let mut core = Core::with_services(
            Box::new(MemoryStore::new()),
            Box::new(FakeHarnessFactory::new(vec![vec![FakeTurn::new(vec![
                FakeStep::WriteFile {
                    path: "README.md".into(),
                    contents: "temporary\n".into(),
                },
                FakeStep::WriteFile {
                    path: "README.md".into(),
                    contents: "initial\n".into(),
                },
            ])]])),
        );
        let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
        let draft = workspace_of(
            core.execute(Command::CreateDraft {
                agent: Agent::Codex,
                project_id,
                title: "Try an edit".into(),
            })
            .await?,
        );
        let workspace = workspace_of(
            core.execute(Command::SendPrompt {
                task_id: TaskId::new(draft.task.id),
                input: PromptInput::text("Try an edit and restore it"),
            })
            .await?,
        );
        assert_eq!(workspace.task.state, TaskState::Ready);
        Ok(())
    }

    #[tokio::test]
    async fn later_turns_compare_content_in_an_already_dirty_worktree()
    -> Result<(), Box<dyn std::error::Error>> {
        let repository = TestRepository::new()?;
        let edit = |contents: &str| {
            FakeTurn::new(vec![FakeStep::WriteFile {
                path: "README.md".into(),
                contents: contents.into(),
            }])
        };
        let mut core = Core::with_services(
            Box::new(MemoryStore::new()),
            Box::new(FakeHarnessFactory::new(vec![vec![
                edit("first edit\n"),
                FakeTurn::new(vec![]),
                edit("second edit\n"),
            ]])),
        );
        let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
        let draft = workspace_of(
            core.execute(Command::CreateDraft {
                agent: Agent::Codex,
                project_id,
                title: "Iterate on an edit".into(),
            })
            .await?,
        );
        let task_id = TaskId::new(draft.task.id);
        for (text, expected) in [
            ("Edit", TaskState::Review),
            ("Explain", TaskState::Ready),
            ("Edit again", TaskState::Review),
        ] {
            let workspace = workspace_of(
                core.execute(Command::SendPrompt {
                    task_id: task_id.clone(),
                    input: PromptInput::text(text),
                })
                .await?,
            );
            assert_eq!(workspace.task.state, expected, "{text}");
        }
        Ok(())
    }

    #[tokio::test]
    async fn a_queued_review_note_waits_for_the_current_turn()
    -> Result<(), Box<dyn std::error::Error>> {
        let repository = TestRepository::new()?;
        let mut core = Core::with_services(
            Box::new(MemoryStore::new()),
            Box::new(FakeHarnessFactory::new(vec![vec![
                FakeTurn::new(vec![
                    FakeStep::approval("permission", "Edit the readme"),
                    FakeStep::WriteFile {
                        path: "README.md".into(),
                        contents: "first pass\n".into(),
                    },
                ]),
                FakeTurn::new(vec![]),
            ]])),
        );
        let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
        let draft = workspace_of(
            core.execute(Command::CreateDraft {
                agent: Agent::Codex,
                project_id,
                title: "Review notes".into(),
            })
            .await?,
        );
        let task_id = TaskId::new(draft.task.id);
        let waiting = workspace_of(
            core.execute(Command::SendPrompt {
                task_id: task_id.clone(),
                input: PromptInput::text("Edit the readme"),
            })
            .await?,
        );
        assert_eq!(waiting.task.state, TaskState::Waiting);

        assert!(matches!(
            core.execute(Command::ReviewTask {
                task_id: task_id.clone(),
                action: ReviewAction::Reopen,
            })
            .await,
            Err(CoreError::TaskNotArchived {
                state: "waiting",
                ..
            })
        ));
        assert_eq!(core.task(&task_id)?.state, TaskState::Waiting);

        let queued = workspace_of(
            core.execute(Command::ReviewTask {
                task_id: task_id.clone(),
                action: ReviewAction::SendNote {
                    text: "Explain the changes".into(),
                },
            })
            .await?,
        );
        assert_eq!(
            queued.queued_review_note.as_deref(),
            Some("Explain the changes")
        );
        assert_eq!(queued.task.state, TaskState::Waiting);
        core.pump().await?;
        assert!(core.task_workspace(&task_id)?.queued_review_note.is_some());

        core.execute(Command::ResolveApproval {
            task_id: task_id.clone(),
            approval_id: ApprovalId::new("permission"),
            decision: Decision::Allow,
        })
        .await?;
        core.pump().await?;
        let finished = core.task_workspace(&task_id)?;
        assert!(finished.queued_review_note.is_none());
        assert_eq!(finished.task.state, TaskState::Ready);
        assert!(
            summaries(&finished).iter().any(|entry| {
                entry.contains("Explain the changes") && entry.contains("README.md")
            })
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
                agent: Agent::Codex,
                project_id,
                title: "Change a file".to_string(),
            })
            .await?,
        );
        let task_id = TaskId::new(draft.task.id.clone());

        let workspace = workspace_of(
            core.execute(Command::SendPrompt {
                task_id,
                input: PromptInput::text("write it"),
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
    async fn batch_archives_review_tasks_across_projects_and_rejects_a_stale_selection()
    -> Result<(), Box<dyn std::error::Error>> {
        let first_repository = TestRepository::new()?;
        let second_repository = TestRepository::new()?;
        let database = TestDatabase::new()?;
        let uri = database.uri();
        let changing_turn = || {
            vec![FakeTurn::new(vec![FakeStep::WriteFile {
                path: "notes.txt".into(),
                contents: "A change under review\n".into(),
            }])]
        };
        let mut core = Core::with_services(
            Box::new(TursoStore::open(&uri).await?),
            Box::new(FakeHarnessFactory::new(vec![
                changing_turn(),
                changing_turn(),
            ])),
        );
        let first_project = add_project(&mut core, first_repository.path().into()).await?;
        let second_project = add_project(&mut core, second_repository.path().into()).await?;
        let mut review_ids = Vec::new();
        for project_id in [first_project.clone(), second_project] {
            let draft = workspace_of(
                core.execute(Command::CreateDraft {
                    project_id,
                    title: "Edit notes".into(),
                    agent: Agent::Codex,
                })
                .await?,
            );
            let task_id = TaskId::new(draft.task.id);
            let completed = workspace_of(
                core.execute(Command::SendPrompt {
                    task_id: task_id.clone(),
                    input: PromptInput::text("Edit notes"),
                })
                .await?,
            );
            assert_eq!(completed.task.state, TaskState::Review);
            review_ids.push(task_id);
        }
        assert_eq!(core.review_task_lists().len(), 2);

        let draft = workspace_of(
            core.execute(Command::CreateDraft {
                project_id: first_project,
                title: "Unsaved work".into(),
                agent: Agent::Codex,
            })
            .await?,
        );
        let mut stale_selection = review_ids.clone();
        stale_selection.push(TaskId::new(draft.task.id));
        assert!(
            core.execute(Command::ArchiveReviewed {
                task_ids: stale_selection
            })
            .await
            .is_err()
        );
        for task_id in &review_ids {
            assert_eq!(core.task(task_id)?.state, TaskState::Review);
            assert!(
                core.sessions
                    .get(task_id)
                    .is_some_and(|session| !session.is_closed())
            );
        }
        assert_eq!(
            core.store
                .load_tasks()
                .await?
                .iter()
                .filter(|task| task.state == TaskState::Review)
                .count(),
            2
        );

        let mut database_probe = toasty::Db::builder()
            .build(toasty_driver_turso::Turso::new(&uri)?.experimental_index_method(true))
            .await?;
        toasty::sql::statement(
            "CREATE TRIGGER block_session_close BEFORE UPDATE ON sessions \
             WHEN NEW.state = 'ended' BEGIN SELECT RAISE(ABORT, 'session write blocked'); END",
        )
        .exec(&mut database_probe)
        .await?;
        let error = core
            .execute(Command::ArchiveReviewed {
                task_ids: review_ids.clone(),
            })
            .await
            .err()
            .ok_or("the session write should fail")?;
        assert!(error.to_string().contains("session write blocked"));
        let first_task = review_ids.iter().min().ok_or("missing first review task")?;
        assert!(
            core.sessions
                .get(first_task)
                .is_some_and(HarnessSession::is_closed)
        );
        let closed_record = core
            .session_records
            .get(first_task)
            .ok_or("missing session record")?
            .clone();
        assert_eq!(closed_record.exit_code, Some(0));
        assert!(closed_record.is_ended());
        toasty::sql::statement("DROP TRIGGER block_session_close")
            .exec(&mut database_probe)
            .await?;

        let stale_task = review_ids
            .iter()
            .max()
            .ok_or("missing second review task")?;
        toasty::sql::statement("UPDATE tasks SET state = 'ready' WHERE id = ?1")
            .bind(stale_task.as_str())
            .exec(&mut database_probe)
            .await?;
        assert!(matches!(
            core.execute(Command::ArchiveReviewed { task_ids: review_ids.clone() }).await,
            Err(CoreError::Store(StoreError::ReviewStateChanged { task_id })) if task_id == *stale_task
        ));
        for task in core.store.load_tasks().await? {
            if task.id == *first_task {
                assert_eq!(task.state, TaskState::Review);
            } else if task.id == *stale_task {
                assert_eq!(task.state, TaskState::Ready);
            }
        }
        let saved_record = core
            .store
            .load_sessions()
            .await?
            .into_iter()
            .find(|record| record.id == closed_record.id)
            .ok_or("missing durable session record")?;
        assert_eq!(saved_record.ended_at, closed_record.ended_at);
        assert_eq!(saved_record.exit_code, closed_record.exit_code);
        toasty::sql::statement("UPDATE tasks SET state = 'review' WHERE id = ?1")
            .bind(stale_task.as_str())
            .exec(&mut database_probe)
            .await?;

        let failing_id = stale_task.as_str().replace('\'', "''");
        let trigger = format!(
            "CREATE TRIGGER block_archive BEFORE UPDATE ON tasks WHEN NEW.state = 'done' AND NEW.id = '{failing_id}' BEGIN SELECT RAISE(ABORT, 'archive write blocked'); END"
        );
        toasty::sql::statement(&trigger)
            .exec(&mut database_probe)
            .await?;
        let error = core
            .execute(Command::ArchiveReviewed {
                task_ids: review_ids.clone(),
            })
            .await
            .err()
            .ok_or("the archive write should fail")?;
        assert!(error.to_string().contains("archive write blocked"));
        let saved = core.store.load_tasks().await?;
        for task_id in &review_ids {
            assert_eq!(core.task(task_id)?.state, TaskState::Review);
            assert_eq!(
                saved
                    .iter()
                    .find(|task| &task.id == task_id)
                    .ok_or("missing task")?
                    .state,
                TaskState::Review
            );
        }
        toasty::sql::statement("DROP TRIGGER block_archive")
            .exec(&mut database_probe)
            .await?;

        core.drain_events();
        assert!(matches!(
            core.execute(Command::ArchiveReviewed {
                task_ids: review_ids.clone()
            })
            .await?,
            CommandResult::Ack
        ));
        assert!(core.review_task_lists().is_empty());
        assert!(
            core.project_list()
                .projects
                .iter()
                .all(|project| project.review_task_count == 0)
        );
        assert_eq!(
            core.drain_events()
                .iter()
                .filter(|event| matches!(event, Event::TaskListChanged(_)))
                .count(),
            2
        );
        for task_id in &review_ids {
            let task = core.task(task_id)?;
            assert_eq!(task.state, TaskState::Done);
            assert!(
                task.worktree_path
                    .as_ref()
                    .is_some_and(|path| path.join("notes.txt").is_file())
            );
            assert!(
                core.sessions
                    .get(task_id)
                    .is_some_and(HarnessSession::is_closed)
            );
        }
        drop(core);
        let reopened = Core::load(
            Box::new(TursoStore::open(&uri).await?),
            Box::new(UnavailableHarnessFactory),
        )
        .await?;
        for task_id in review_ids {
            assert_eq!(reopened.task(&task_id)?.state, TaskState::Done);
        }
        Ok(())
    }

    #[tokio::test]
    async fn terminal_error_notice_is_saved_while_turn_is_running()
    -> Result<(), Box<dyn std::error::Error>> {
        let repository = TestRepository::new()?;
        let database = TestDatabase::new()?;
        let scripts = vec![vec![FakeTurn::new(vec![
            FakeStep::Message("unfinished answer".into()),
            FakeStep::TerminalError {
                id: "terminal".into(),
                text: "Agent terminal error: missing exit status".into(),
            },
            FakeStep::approval("pending", "Await approval"),
        ])]];
        let mut core = Core::with_services(
            Box::new(TursoStore::open(&database.uri()).await?),
            Box::new(FakeHarnessFactory::new(scripts)),
        );
        let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
        let draft = workspace_of(
            core.execute(Command::CreateDraft {
                project_id,
                title: "terminal error".into(),
                agent: Agent::Codex,
            })
            .await?,
        );
        let task_id = TaskId::new(draft.task.id);
        core.execute(Command::SendPrompt {
            task_id: task_id.clone(),
            input: PromptInput::text("run"),
        })
        .await?;
        assert_eq!(core.task(&task_id)?.state, TaskState::Waiting);
        let store = TursoStore::open(&database.uri()).await?;
        let saved = store.load_transcript(&task_id).await?;
        assert!(saved.iter().any(|entry| matches!(&entry.kind, agent_harness::EntryKind::Notice { text, is_error: true } if text.contains("missing exit status"))));
        assert!(!saved.iter().any(|entry| matches!(&entry.kind, agent_harness::EntryKind::AgentText { text } if text.contains("unfinished answer"))));
        drop(store);
        drop(core);
        let mut reopened = Core::load(
            Box::new(TursoStore::open(&database.uri()).await?),
            Box::new(UnavailableHarnessFactory),
        )
        .await?;
        let workspace = reopened.open_task_workspace(&task_id).await?;
        assert!(workspace.transcript.iter().any(|entry| matches!(&entry.kind, agent_harness::EntryKind::Notice { text, is_error: true } if text.contains("missing exit status"))));
        assert!(!workspace.transcript.iter().any(|entry| matches!(&entry.kind, agent_harness::EntryKind::AgentText { text } if text.contains("unfinished answer"))));
        Ok(())
    }

    #[tokio::test]
    async fn terminal_error_notice_survives_task_reopen() -> Result<(), Box<dyn std::error::Error>>
    {
        let repository = TestRepository::new()?;
        let database = TestDatabase::new()?;
        let scripts = vec![vec![FakeTurn::new(vec![FakeStep::TerminalError {
            id: "fixture-terminal".into(),
            text: "Agent terminal error: missing exit status".into(),
        }])]];
        let mut core = Core::with_services(
            Box::new(TursoStore::open(&database.uri()).await?),
            Box::new(FakeHarnessFactory::new(scripts)),
        );
        let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
        let draft = workspace_of(
            core.execute(Command::CreateDraft {
                project_id,
                title: "terminal error".into(),
                agent: Agent::Codex,
            })
            .await?,
        );
        let task_id = TaskId::new(draft.task.id);
        let workspace = workspace_of(
            core.execute(Command::SendPrompt {
                task_id: task_id.clone(),
                input: PromptInput::text("run"),
            })
            .await?,
        );
        assert!(workspace.transcript.iter().any(|entry| matches!(&entry.kind, agent_harness::EntryKind::Notice { text, is_error: true } if text.contains("missing exit status"))));
        drop(core);
        let mut reopened = Core::load(
            Box::new(TursoStore::open(&database.uri()).await?),
            Box::new(UnavailableHarnessFactory),
        )
        .await?;
        let workspace = reopened.open_task_workspace(&task_id).await?;
        assert!(workspace.transcript.iter().any(|entry| matches!(&entry.kind, agent_harness::EntryKind::Notice { text, is_error: true } if text.contains("missing exit status"))));
        Ok(())
    }

    #[tokio::test]
    async fn large_transcript_and_git_diff_survive_restart()
    -> Result<(), Box<dyn std::error::Error>> {
        let repository = TestRepository::new()?;
        let database = TestDatabase::new()?;
        let content: String = (0..12_000)
            .map(|index| format!("large diff line {index:05}\n"))
            .collect();
        let mut steps = vec![FakeStep::WriteFile {
            path: "large.txt".into(),
            contents: content,
        }];
        steps.extend((0..1_000).map(|index| FakeStep::Message(format!("message {index:04}"))));
        let mut core = Core::with_services(
            Box::new(TursoStore::open(&database.uri()).await?),
            Box::new(FakeHarnessFactory::new(vec![vec![FakeTurn::new(steps)]])),
        );
        let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
        let draft = workspace_of(
            core.execute(Command::CreateDraft {
                project_id,
                title: "Large review".into(),
                agent: Agent::Codex,
            })
            .await?,
        );
        let task_id = TaskId::new(draft.task.id);
        core.execute(Command::SendPrompt {
            task_id: task_id.clone(),
            input: PromptInput::text("Create a large review"),
        })
        .await?;
        while core.pump().await? {}
        let workspace = core.open_task_workspace(&task_id).await?;
        assert_eq!(workspace.task.state, TaskState::Review);
        let transcript = summaries(&workspace);
        for index in 0..1_000 {
            assert!(
                transcript
                    .iter()
                    .any(|entry| entry == &format!("agent: message {index:04}"))
            );
        }
        let diff = core.review_file_diff(&task_id, "large.txt")?;
        let expected_lines: Vec<String> = (0..12_000)
            .map(|index| format!("+large diff line {index:05}"))
            .collect();
        let actual_lines: Vec<&str> = diff
            .lines()
            .filter(|line| line.starts_with("+large diff line "))
            .collect();
        assert_eq!(
            actual_lines,
            expected_lines
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
        );
        drop(core);

        let mut reopened = Core::load(
            Box::new(TursoStore::open(&database.uri()).await?),
            Box::new(UnavailableHarnessFactory),
        )
        .await?;
        let restored = reopened.open_task_workspace(&task_id).await?;
        assert_eq!(restored.task.state, TaskState::Review);
        assert_eq!(summaries(&restored), transcript);
        assert_eq!(reopened.review_file_diff(&task_id, "large.txt")?, diff);
        for index in [0, 32, 999] {
            let text = format!("message {index:04}");
            let entry = restored
                .transcript
                .iter()
                .find(|entry| {
                    matches!(
                        &entry.kind,
                        agent_harness::EntryKind::AgentText { text: message } if message == &text
                    )
                })
                .ok_or("missing indexed message")?;
            let hits = reopened.task_search().search(&format!("{text} ")).await?;
            assert!(
                hits.iter()
                    .any(|hit| hit.task_id == task_id.as_str() && hit.entry_id == entry.id)
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn missing_worktree_is_repaired_before_the_next_turn()
    -> Result<(), Box<dyn std::error::Error>> {
        let repository = TestRepository::new()?;
        let database = TestDatabase::new()?;
        let scripts = vec![
            vec![FakeTurn::new(vec![FakeStep::Message("first reply".into())])],
            vec![FakeTurn::new(vec![FakeStep::Message(
                "second reply".into(),
            )])],
        ];
        let mut core = Core::with_services(
            Box::new(TursoStore::open(&database.uri()).await?),
            Box::new(FakeHarnessFactory::new(scripts)),
        );
        let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
        let draft = workspace_of(
            core.execute(Command::CreateDraft {
                project_id,
                title: "repair".into(),
                agent: Agent::Codex,
            })
            .await?,
        );
        let task_id = TaskId::new(draft.task.id);
        let first = workspace_of(
            core.execute(Command::SendPrompt {
                task_id: task_id.clone(),
                input: PromptInput::text("first prompt"),
            })
            .await?,
        );
        let path = PathBuf::from(first.task.worktree_path.ok_or("missing worktree")?);
        let branch = first.task.branch.ok_or("missing branch")?;
        let tip = crate::git::run_git(&path, &["rev-parse", "HEAD"])?;
        std::fs::remove_dir_all(&path)?;
        let second = workspace_of(
            core.execute(Command::SendPrompt {
                task_id: task_id.clone(),
                input: PromptInput::text("second prompt"),
            })
            .await?,
        );
        assert_eq!(second.task.state, TaskState::Ready);
        assert_eq!(crate::git::run_git(&path, &["rev-parse", "HEAD"])?, tip);
        assert_eq!(
            crate::git::run_git(&path, &["branch", "--show-current"])?,
            branch
        );
        let transcript = summaries(&second).join("\n");
        assert!(transcript.contains("first reply"));
        assert!(transcript.contains("second reply"));
        drop(core);
        let mut reopened = Core::load(
            Box::new(TursoStore::open(&database.uri()).await?),
            Box::new(UnavailableHarnessFactory),
        )
        .await?;
        let transcript = summaries(&reopened.open_task_workspace(&task_id).await?).join("\n");
        assert!(transcript.contains("first reply"));
        assert!(transcript.contains("second reply"));
        Ok(())
    }

    #[tokio::test]
    async fn archived_collection_keeps_ignored_files_and_clears_its_intent()
    -> Result<(), Box<dyn std::error::Error>> {
        let repository = TestRepository::new()?;
        let database = TestDatabase::new()?;
        let mut core = Core::with_services(
            Box::new(TursoStore::open(&database.uri()).await?),
            Box::new(UnavailableHarnessFactory),
        );
        let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
        let mut task = Task::draft(
            project_id,
            "Archived task",
            &repository.repository().git_dir,
        );
        GitService.create_worktree(
            repository.repository(),
            &task.proposed_worktree_path,
            &task.proposed_branch,
            "main",
        )?;
        std::fs::write(
            repository.repository().git_dir.join("info/exclude"),
            "ignored.txt\n",
        )?;
        let ignored = task.proposed_worktree_path.join("ignored.txt");
        std::fs::write(&ignored, "keep this data")?;
        task.worktree_path = Some(task.proposed_worktree_path.clone());
        task.branch = Some(task.proposed_branch.clone());
        task.state = TaskState::Done;
        core.store.save_task(&task).await?;
        core.tasks.insert(task.id.clone(), task.clone());

        assert!(matches!(
            core.collect_archived_worktree(&task.id).await,
            Err(CoreError::Git(GitError::IgnoredWorktreeFiles { .. }))
        ));
        assert_eq!(std::fs::read_to_string(ignored)?, "keep this data");
        assert!(!core.task(&task.id)?.collection_pending);
        assert!(
            !core
                .store
                .load_tasks()
                .await?
                .into_iter()
                .find(|saved| saved.id == task.id)
                .ok_or("missing task")?
                .collection_pending
        );
        Ok(())
    }

    #[tokio::test]
    async fn touching_an_interrupted_collection_preserves_and_recovers_its_intent()
    -> Result<(), Box<dyn std::error::Error>> {
        let repository = TestRepository::new()?;
        let database = TestDatabase::new()?;
        let mut core = Core::with_services(
            Box::new(TursoStore::open(&database.uri()).await?),
            Box::new(UnavailableHarnessFactory),
        );
        let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
        let mut task = Task::draft(
            project_id,
            "Archived task",
            &repository.repository().git_dir,
        );
        GitService.create_worktree(
            repository.repository(),
            &task.proposed_worktree_path,
            &task.proposed_branch,
            "main",
        )?;
        let tip =
            GitService.checked_out_tip(&task.proposed_worktree_path, &task.proposed_branch)?;
        GitService.retain_task_tip(repository.repository(), task.id.as_str(), &tip)?;
        GitService.remove_clean_worktree(repository.repository(), &task.proposed_worktree_path)?;
        task.worktree_path = Some(task.proposed_worktree_path.clone());
        task.branch = Some(task.proposed_branch.clone());
        task.state = TaskState::Done;
        task.collection_pending = true;
        task.branch_tip_sha = Some("invalid-tip".into());
        core.store.save_task(&task).await?;
        core.tasks.insert(task.id.clone(), task.clone());

        assert!(core.touch_archived_worktree(&task.id).await.is_err());
        assert!(core.task(&task.id)?.collection_pending);
        assert!(
            core.review_task(&task.id, ReviewAction::Reopen)
                .await
                .is_err()
        );
        assert!(core.task(&task.id)?.collection_pending);
        assert_eq!(core.task(&task.id)?.state, TaskState::Done);
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(repository.path(), &task.proposed_worktree_path)?;
            assert!(core.touch_archived_worktree(&task.id).await.is_err());
            assert!(core.collect_archived_worktree(&task.id).await.is_err());
            assert!(core.task(&task.id)?.collection_pending);
            std::fs::remove_file(&task.proposed_worktree_path)?;
        }
        GitService.create_worktree(
            repository.repository(),
            &task.proposed_worktree_path,
            "different-task-branch",
            "main",
        )?;
        assert!(core.touch_archived_worktree(&task.id).await.is_err());
        assert!(core.collect_archived_worktree(&task.id).await.is_err());
        assert!(core.task(&task.id)?.collection_pending);
        GitService.remove_clean_worktree(repository.repository(), &task.proposed_worktree_path)?;
        task.branch_tip_sha = Some(tip);
        core.store.save_task(&task).await?;
        core.tasks.insert(task.id.clone(), task.clone());
        core.touch_archived_worktree(&task.id).await?;
        let recovered = core.task(&task.id)?;
        assert!(!recovered.collection_pending && recovered.worktree_path.is_none());
        core.review_task(&task.id, ReviewAction::Reopen).await?;
        assert!(task.proposed_worktree_path.is_dir());
        Ok(())
    }

    #[tokio::test]
    async fn startup_reconciles_recent_pending_collections_without_removing_live_worktrees()
    -> Result<(), Box<dyn std::error::Error>> {
        let repository = TestRepository::new()?;
        let database = TestDatabase::new()?;
        let uri = database.uri();
        let mut core = Core::with_services(
            Box::new(TursoStore::open(&uri).await?),
            Box::new(UnavailableHarnessFactory),
        );
        let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
        let mut missing = Task::draft(
            project_id.clone(),
            "Missing",
            &repository.repository().git_dir,
        );
        let mut present = Task::draft(project_id, "Present", &repository.repository().git_dir);
        for task in [&mut missing, &mut present] {
            GitService.create_worktree(
                repository.repository(),
                &task.proposed_worktree_path,
                &task.proposed_branch,
                "main",
            )?;
            let tip =
                GitService.checked_out_tip(&task.proposed_worktree_path, &task.proposed_branch)?;
            GitService.retain_task_tip(repository.repository(), task.id.as_str(), &tip)?;
            task.branch_tip_sha = Some(tip);
            task.branch = Some(task.proposed_branch.clone());
            task.worktree_path = Some(task.proposed_worktree_path.clone());
            task.state = TaskState::Done;
            task.collection_pending = true;
        }
        GitService
            .remove_clean_worktree(repository.repository(), &missing.proposed_worktree_path)?;
        core.store.save_task(&missing).await?;
        core.store.save_task(&present).await?;
        drop(core);
        let reopened = Core::load(
            Box::new(TursoStore::open(&uri).await?),
            Box::new(UnavailableHarnessFactory),
        )
        .await?;
        let recovered = reopened.task(&missing.id)?;
        assert!(!recovered.collection_pending && recovered.worktree_path.is_none());
        let kept = reopened.task(&present.id)?;
        assert!(!kept.collection_pending && kept.proposed_worktree_path.is_dir());
        Ok(())
    }

    #[tokio::test]
    async fn startup_keeps_dirty_and_recent_archived_worktrees()
    -> Result<(), Box<dyn std::error::Error>> {
        let repository = TestRepository::new()?;
        let database = TestDatabase::new()?;
        let uri = database.uri();
        let mut core = Core::with_services(
            Box::new(TursoStore::open(&uri).await?),
            Box::new(UnavailableHarnessFactory),
        );
        let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
        let mut dirty = Task::draft(
            project_id.clone(),
            "Dirty task",
            &repository.repository().git_dir,
        );
        let mut recent = Task::draft(project_id, "Recent task", &repository.repository().git_dir);
        for task in [&mut dirty, &mut recent] {
            GitService.create_worktree(
                repository.repository(),
                &task.proposed_worktree_path,
                &task.proposed_branch,
                "main",
            )?;
            task.worktree_path = Some(task.proposed_worktree_path.clone());
            task.branch = Some(task.proposed_branch.clone());
            task.state = TaskState::Done;
        }
        dirty.updated_at = projects::now_millis().saturating_sub(31 * 24 * 60 * 60 * 1000);
        dirty.worktree_last_used_at = dirty.updated_at;
        recent.updated_at =
            projects::now_millis().saturating_sub(30 * 24 * 60 * 60 * 1000 + 30_000);
        recent.worktree_last_used_at = recent.updated_at;
        std::fs::write(dirty.proposed_worktree_path.join("draft.txt"), "keep me")?;
        core.store.save_task(&dirty).await?;
        core.store.save_task(&recent).await?;
        drop(core);

        let reopened = Core::load(
            Box::new(TursoStore::open(&uri).await?),
            Box::new(UnavailableHarnessFactory),
        )
        .await?;
        assert_eq!(
            std::fs::read_to_string(dirty.proposed_worktree_path.join("draft.txt"))?,
            "keep me"
        );
        assert!(recent.proposed_worktree_path.is_dir());
        let [issue] = reopened.archived_worktree_issues() else {
            return Err("expected one collection issue".into());
        };
        assert_eq!(issue.task_id, dirty.id.to_string());
        assert!(issue.reason.contains("untracked changes"));
        Ok(())
    }

    #[tokio::test]
    async fn review_commit_merge_archive_and_reopen_a_task()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::git::GitMutation;

        let repository = TestRepository::new()?;
        let database = TestDatabase::new()?;
        let uri = database.uri();
        let mut core = Core::with_services(
            Box::new(TursoStore::open(&uri).await?),
            Box::new(FakeHarnessFactory::new(vec![vec![
                FakeTurn::new(vec![
                    FakeStep::WriteFile {
                        path: "README.md".into(),
                        contents: "first pass\n".into(),
                    },
                    FakeStep::WriteFile {
                        path: "notes.txt".into(),
                        contents: "review context\n".into(),
                    },
                ]),
                FakeTurn::new(vec![FakeStep::WriteFile {
                    path: "README.md".into(),
                    contents: "reviewed result\n".into(),
                }]),
            ]])),
        );
        let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
        let draft = workspace_of(
            core.execute(Command::CreateDraft {
                agent: Agent::Codex,
                project_id,
                title: "Improve the readme".into(),
            })
            .await?,
        );
        let task_id = TaskId::new(draft.task.id);
        let review = workspace_of(
            core.execute(Command::SendPrompt {
                task_id: task_id.clone(),
                input: PromptInput::text("Edit the readme and leave notes"),
            })
            .await?,
        );
        assert_eq!(review.task.state, TaskState::Review);
        let path = PathBuf::from(review.task.worktree_path.ok_or("missing worktree")?);
        let branch = review.task.branch.ok_or("missing task branch")?;
        let files = core.review_snapshot(&task_id)?.files;
        assert!(files.iter().any(|file| file.path == "README.md"));
        assert!(files.iter().any(|file| file.path == "notes.txt"));
        let diff = core.review_file_diff(&task_id, "README.md")?;
        assert!(diff.contains("+first pass"));
        assert!(!diff.contains("review context"));

        let reviewed = workspace_of(
            core.execute(Command::ReviewTask {
                task_id: task_id.clone(),
                action: ReviewAction::SendNote {
                    text: "Revise the first line".into(),
                },
            })
            .await?,
        );
        assert_eq!(reviewed.task.state, TaskState::Review);
        assert!(summaries(&reviewed).iter().any(|entry| {
            entry.contains("Revise the first line") && entry.contains("notes.txt")
        }));

        let staged = core
            .mutate_git(&task_id, GitMutation::Stage { path: None })
            .await?;
        let committed = core
            .mutate_git(
                &task_id,
                GitMutation::Commit {
                    message: "Apply the review feedback".into(),
                    expected: staged.staged_revision,
                },
            )
            .await?;
        assert!(committed.files.is_empty());
        let tip = crate::git::run_git(&path, &["rev-parse", "HEAD"])?;
        core.mutate_git(&task_id, GitMutation::SquashMerge).await?;
        assert_eq!(
            std::fs::read_to_string(repository.path().join("README.md"))?,
            "reviewed result\n"
        );

        let archived = workspace_of(
            core.execute(Command::ReviewTask {
                task_id: task_id.clone(),
                action: ReviewAction::Archive,
            })
            .await?,
        );
        assert_eq!(archived.task.state, TaskState::Done);
        let mut archived_record = core.task(&task_id)?.clone();
        archived_record.updated_at =
            projects::now_millis().saturating_sub(31 * 24 * 60 * 60 * 1000);
        archived_record.worktree_last_used_at = archived_record.updated_at;
        core.store.save_task(&archived_record).await?;
        drop(core);

        let mut reopened = Core::load(
            Box::new(TursoStore::open(&uri).await?),
            Box::new(UnavailableHarnessFactory),
        )
        .await?;
        assert!(!path.exists());
        assert!(
            reopened
                .task_workspace(&task_id)?
                .task
                .worktree_path
                .is_none()
        );
        assert_eq!(
            git_in(&repository, &["rev-parse", &format!("refs/heads/{branch}")]),
            tip
        );
        crate::git::run_git(repository.path(), &["branch", "-D", &branch])?;
        let restored = workspace_of(
            reopened
                .execute(Command::ReviewTask {
                    task_id,
                    action: ReviewAction::Reopen,
                })
                .await?,
        );
        assert_eq!(restored.task.state, TaskState::Review);
        assert_eq!(crate::git::run_git(&path, &["rev-parse", "HEAD"])?, tip);
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
                    agent: Agent::Codex,
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
                    input: PromptInput::text("start"),
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
            Err(CoreError::QuitConfirmationRequired {
                live_tasks: 1,
                live_terminals: 0
            })
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
        assert_eq!(
            workspace.transcript.first().map(Entry::summary).as_deref(),
            Some("you: start")
        );
        assert!(workspace.transcript.last().is_some_and(|entry| {
            entry
                .summary()
                .contains("resumed the previous agent session")
        }));
        Ok(())
    }

    #[tokio::test]
    async fn workspace_width_is_saved_per_task_and_restored()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;
        let database = directory.join("shepherd.db");
        let store = TursoStore::open_at(&database).await?;
        let mut core = Core::with_services(Box::new(store), Box::new(UnavailableHarnessFactory));
        let project_id = add_project(&mut core, directory.path().to_path_buf()).await?;
        let first = workspace_of(
            core.execute(Command::CreateDraft {
                agent: Agent::Codex,
                project_id: project_id.clone(),
                title: "Wide inspector".into(),
            })
            .await?,
        );
        let second = workspace_of(
            core.execute(Command::CreateDraft {
                agent: Agent::Codex,
                project_id,
                title: "Default inspector".into(),
            })
            .await?,
        );
        let first = TaskId::new(first.task.id);
        let second = TaskId::new(second.task.id);
        let layout = WorkspaceLayout {
            inspector_width: Some(760),
        };
        core.save_workspace_layout(&first, layout).await?;
        assert!(matches!(
            core.save_workspace_layout(
                &first,
                WorkspaceLayout {
                    inspector_width: Some(0)
                }
            )
            .await,
            Err(CoreError::InvalidWorkspaceLayout)
        ));
        drop(core);

        let store = TursoStore::open_at(&database).await?;
        let core = Core::load(Box::new(store), Box::new(UnavailableHarnessFactory)).await?;
        assert_eq!(core.task_workspace(&first)?.layout, layout);
        assert_eq!(
            core.task_workspace(&second)?.layout,
            WorkspaceLayout::default()
        );
        Ok(())
    }

    #[tokio::test]
    async fn terminal_layout_round_trips_and_rejects_stale_or_foreign_panes()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::terminal::{SplitAxis, TerminalSplit};

        let directory = TestDirectory::new()?;
        let other = TestDirectory::new()?;
        let database = directory.join("shepherd.db");
        let store = TursoStore::open_at(&database).await?;
        let mut core = Core::with_services(Box::new(store), Box::new(UnavailableHarnessFactory));
        let project = add_project(&mut core, directory.path().to_path_buf()).await?;
        core.execute(Command::AddProject {
            path: other.path().to_path_buf(),
        })
        .await?;
        let foreign = core
            .project_order
            .last()
            .cloned()
            .ok_or("second project was not added")?;
        for (id, owner, path) in [
            ("first", &project, directory.path()),
            ("second", &project, directory.path()),
            ("foreign", &foreign, other.path()),
        ] {
            let terminal = TerminalRecord::project_shared(
                TerminalId::new(id),
                owner.clone(),
                id,
                path.to_path_buf(),
                "/bin/sh",
                1,
            );
            core.record_terminal(&terminal).await?;
        }
        let pane = |id: &str| {
            Box::new(TerminalSplit::Pane {
                terminal_id: id.into(),
            })
        };
        let legacy_layout: TerminalLayout = serde_json::from_str(r#"{"revision":0,"root":null}"#)?;
        assert_eq!(legacy_layout, TerminalLayout::default());
        let edit = TerminalLayout {
            root: Some(TerminalSplit::Split {
                axis: SplitAxis::Horizontal,
                ratio: 650,
                first: pane("first"),
                second: pane("second"),
            }),
            ..legacy_layout
        };
        core.save_inspector_tab(&project, InspectorTab::Review)
            .await?;
        let saved = core.save_terminal_layout(&project, edit.clone()).await?;
        assert_eq!(saved.revision, 1);
        assert_eq!(saved.inspector_tab, InspectorTab::Review);
        assert!(matches!(
            core.save_terminal_layout(&project, edit).await,
            Err(CoreError::TerminalLayoutChanged)
        ));
        assert!(matches!(
            core.save_terminal_layout(
                &project,
                TerminalLayout {
                    revision: saved.revision,
                    root: Some(*pane("foreign")),
                    ..TerminalLayout::default()
                }
            )
            .await,
            Err(CoreError::InvalidTerminalLayout)
        ));
        drop(core);

        let store = TursoStore::open_at(&database).await?;
        let reopened = Core::load(Box::new(store), Box::new(UnavailableHarnessFactory)).await?;
        assert_eq!(reopened.terminal_layout(&project)?, saved);
        assert_eq!(
            reopened.terminal_layout(&foreign)?,
            TerminalLayout::default()
        );
        Ok(())
    }

    #[tokio::test]
    async fn restoring_a_missing_terminal_keeps_the_other_panes()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::terminal::{SplitAxis, TerminalSplit};

        let directory = TestDirectory::new()?;
        let database = directory.join("shepherd.db");
        let store = TursoStore::open_at(&database).await?;
        let mut core = Core::with_services(Box::new(store), Box::new(UnavailableHarnessFactory));
        let project = add_project(&mut core, directory.path().to_path_buf()).await?;
        core.record_terminal(&TerminalRecord::project_shared(
            TerminalId::new("kept"),
            project.clone(),
            "Kept",
            directory.path().to_path_buf(),
            "/bin/sh",
            1,
        ))
        .await?;
        let pane = |id: &str| TerminalSplit::Pane {
            terminal_id: id.into(),
        };
        let split = |ratio| TerminalSplit::Split {
            axis: SplitAxis::Horizontal,
            ratio,
            first: Box::new(pane("kept")),
            second: Box::new(pane("missing")),
        };
        let saved = TerminalLayout {
            root: Some(split(650)),
            inspector_tab: InspectorTab::Review,
            ..TerminalLayout::default()
        };
        assert!(matches!(
            core.save_terminal_layout(&project, saved.clone()).await,
            Err(CoreError::InvalidTerminalLayout)
        ));
        core.store.save_terminal_layout(&project, &saved).await?;
        drop(core);

        let store = TursoStore::open_at(&database).await?;
        let mut core = Core::load(Box::new(store), Box::new(UnavailableHarnessFactory)).await?;
        assert_eq!(core.terminal_layout(&project)?, saved);
        let resized = core
            .save_terminal_layout(
                &project,
                TerminalLayout {
                    root: Some(split(700)),
                    ..saved
                },
            )
            .await?;
        let cleaned = core
            .save_terminal_layout(
                &project,
                TerminalLayout {
                    root: Some(pane("kept")),
                    ..resized
                },
            )
            .await?;
        assert_eq!(cleaned.root, Some(pane("kept")));
        assert_eq!(cleaned.inspector_tab, InspectorTab::Review);
        assert!(matches!(
            core.save_terminal_layout(
                &project,
                TerminalLayout {
                    root: Some(split(700)),
                    ..cleaned
                },
            )
            .await,
            Err(CoreError::InvalidTerminalLayout)
        ));
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
                    agent: Agent::Codex,
                    project_id,
                    title: "Resume me".to_string(),
                })
                .await?,
        );
        let task_id = TaskId::new(draft.task.id.clone());
        scenario
            .execute(Command::SendPrompt {
                task_id: task_id.clone(),
                input: PromptInput::text("go"),
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
        assert!(workspace.transcript.last().is_some_and(|entry| {
            entry
                .summary()
                .contains("resumed the previous agent session fake-session-1")
        }));

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
                    agent: Agent::Codex,
                    project_id,
                    title: "Resume me".to_string(),
                })
                .await?,
        );
        let task_id = TaskId::new(draft.task.id.clone());
        scenario
            .execute(Command::SendPrompt {
                task_id: task_id.clone(),
                input: PromptInput::text("go"),
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
        let note = workspace
            .transcript
            .last()
            .ok_or("missing resume note")?
            .summary();
        assert!(note.contains("cannot resume"), "unexpected note: {note}");
        assert!(
            note.contains("started a new session"),
            "unexpected note: {note}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn an_unconfigured_adapter_reports_why_it_could_not_launch()
    -> Result<(), Box<dyn std::error::Error>> {
        let repository = TestRepository::new()?;
        let mut core = Core::new();
        let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
        let draft = workspace_of(
            core.execute(Command::CreateDraft {
                agent: Agent::Claude,
                project_id,
                title: "Unconfigured adapter".into(),
            })
            .await?,
        );
        let task_id = TaskId::new(draft.task.id);
        let result = core
            .execute(Command::SendPrompt {
                task_id: task_id.clone(),
                input: PromptInput::text("go"),
            })
            .await;
        assert!(matches!(result, Err(CoreError::HarnessMissing { .. })));
        let workspace = core.task_workspace(&task_id)?;
        assert_eq!(workspace.task.state, TaskState::Ready);
        assert!(
            workspace
                .task
                .error
                .is_some_and(|text| text.contains("Claude Code ACP executable"))
        );
        assert!(
            workspace
                .transcript
                .iter()
                .any(|entry| entry.summary().contains("agent not installed"))
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
                    agent: Agent::Codex,
                    project_id,
                    title: "Install me".to_string(),
                })
                .await?,
        );
        let task_id = TaskId::new(draft.task.id.clone());

        let result = scenario
            .execute(Command::SendPrompt {
                task_id: task_id.clone(),
                input: PromptInput::text("go"),
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
                    agent: Agent::Codex,
                    project_id,
                    title: "Sign in".to_string(),
                })
                .await?,
        );
        let task_id = TaskId::new(draft.task.id.clone());

        let result = scenario
            .execute(Command::SendPrompt {
                task_id: task_id.clone(),
                input: PromptInput::text("go"),
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
                    agent: Agent::Codex,
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
                    input: PromptInput::text("go"),
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
                agent: Agent::Codex,
                project_id,
                title: "Diagnose".to_string(),
            })
            .await?,
        );
        assert!(
            draft.diagnostics.is_none(),
            "a draft has no session to report"
        );
        assert!(draft.adapter_tier.is_none() && draft.capabilities.is_none());

        let workspace = workspace_of(
            core.execute(Command::SendPrompt {
                task_id: TaskId::new(draft.task.id.clone()),
                input: PromptInput::text("go"),
            })
            .await?,
        );
        let diagnostics = workspace
            .diagnostics
            .expect("a running session reports diagnostics");
        assert_eq!(diagnostics.command, "fake");
        assert_eq!(diagnostics.protocol, "fake-harness");
        assert!(diagnostics.stderr_tail.is_empty());
        assert_eq!(workspace.adapter_tier, Some(crate::AgentTier::Native));
        assert!(
            workspace
                .capabilities
                .is_some_and(|capabilities| capabilities.cancel)
        );
        Ok(())
    }

    #[tokio::test]
    async fn recovery_distinguishes_known_non_resumable_and_legacy_sessions()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::Task;
        use crate::session::SessionRecord;
        use crate::snapshots::SessionRecovery;
        use agent_harness::Capabilities;

        let repository = TestRepository::new()?;
        let mut core = Core::new();
        let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
        let mut task = Task::draft(project_id, "Interrupted", repository.path());
        task.worktree_path = Some(repository.path().to_path_buf());
        task.state = TaskState::Interrupted;
        let task_id = task.id.clone();
        core.tasks.insert(task_id.clone(), task);
        assert_eq!(
            core.task_workspace(&task_id)?.recovery,
            Some(SessionRecovery::StartNew)
        );

        let mut record =
            SessionRecord::started(task_id.clone(), "codex", Some("previous-handle".into()));
        core.session_records.insert(task_id.clone(), record.clone());
        assert_eq!(
            core.task_workspace(&task_id)?.recovery,
            Some(SessionRecovery::Reconnect)
        );

        record.capabilities = Some(Capabilities {
            cancel: true,
            ..Capabilities::default()
        });
        assert!(record.resume_handle().is_none());
        core.session_records.insert(task_id.clone(), record);
        let workspace = core.task_workspace(&task_id)?;
        assert_eq!(workspace.recovery, Some(SessionRecovery::StartNew));
        assert!(
            workspace
                .capabilities
                .is_some_and(|capabilities| capabilities.cancel && !capabilities.resume)
        );
        Ok(())
    }
}
