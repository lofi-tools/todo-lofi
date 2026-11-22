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
pub mod pool_reset;
pub use shep_project_scan as project_scan;
pub mod projects;
pub mod search;
pub mod session;
pub mod snapshots;
pub mod store;
pub mod tasks;
pub mod terminal;
pub mod worktree_environment;
pub mod worktree_pool;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use shep_harness_core::{
    Decision, DeltaKind, FailureKind, HarnessEvent, PromptAttachment, PromptInput, SessionId,
    Transcript, TurnFinishReason,
};
use tokio::sync::mpsc;

pub use commands::{Command, CommandResult, QuitSummary, ReviewAction};
pub use events::{Event, ShutdownState};
pub use input::{Focus, InputMode};
pub use projects::{Project, ProjectId, ProjectSnapshot};
pub use shep_project_scan::{ProjectNode, ProjectScan};
pub use snapshots::{
    ProjectApprovalsSnapshot, ProjectList, TaskList, TaskWorkspace, WorkspaceLayout,
};
pub use tasks::{Agent, AgentTier, Task, TaskDetail, TaskId, TaskState, TaskSummary};

use crate::git::{GitError, GitService, TurnBaseline};
use crate::harness::{HarnessFactory, HarnessSession, ProjectApprovals, UnavailableHarnessFactory};
use crate::session::SessionRecord;
use crate::snapshots::TaskApproval;
use crate::store::{
    MemoryStore, PoolCatalogAdvisory, PoolCatalogOutcome, PoolCooldownClearRequest,
    PoolQuotaCorrectionOutcome, PoolQuotaCorrectionRequest, PoolReorderOutcome, PoolReorderRequest,
    PoolSelectionRequest, PoolSlot, PoolSlotEnabledOutcome, PoolSlotEnabledRequest, PoolSlotId,
    PoolStatus, Store, StoreError, WorktreeDeletion, WorktreePoolRecord, WorktreePoolState,
};
use crate::terminal::{
    InspectorTab, ProcessRecord, TerminalId, TerminalLayout, TerminalList, TerminalRecord,
    TerminalScope, TerminalSnapshot,
};

/// Events buffered between drains. A shell drains after every command and pump,
/// so overflow means it stopped reading and must reload its snapshots.
#[doc(hidden)]
pub const MAX_PENDING_EVENTS: usize = 256;

/// A failure reported by the core. Errors are returned to the caller; the core
/// does not turn a failed command into a successful event.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("A task remains on its assigned branch and worktree.")]
    TaskBranchFixed,
    #[error("project path is empty")]
    EmptyProjectPath,
    #[error("project path is already registered: {path}")]
    DuplicateProject { path: String },
    #[error(
        "task worktree path already exists: {path}; another task in this project may have the same title"
    )]
    WorktreePathTaken { path: String },
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
    #[error("terminal is still running: {id}")]
    TerminalStillRunning { id: String },
    #[error("task title cannot be empty")]
    EmptyTitle,
    #[error("the prompt is empty")]
    EmptyPrompt,
    #[error("Agent pool settings are unavailable: {0}. Reload settings after fixing the file.")]
    PoolConfigurationUnavailable(String),
    #[error("{0}")]
    ProjectSetup(String),
    #[error("{message}")]
    StartTaskSetup { task_id: String, message: String },
    #[error("Worktree setup is still running.")]
    WorktreeSetupPending { task_id: String },
    #[error("{message}")]
    DevelopmentEnvironment {
        task_id: String,
        message: String,
        setup_terminal: bool,
    },
    #[error("Codex model and effort options require a Codex task.")]
    CodexOptionsForOtherAgent,
    #[error("Could not resolve mentioned file {path}. {source}")]
    FileMention {
        path: String,
        source: files::FileError,
    },
    #[error("task {id} is not a draft")]
    NotADraft { id: String },
    #[error(
        "first prompt for task {task_id} failed: {prompt_error}; cleanup was incomplete: {reason}"
    )]
    StartTaskCleanup {
        task_id: String,
        prompt_error: String,
        reason: String,
    },
    #[error("Close the task's running terminals before deleting it.")]
    TaskTerminalsRunning,
    #[error("Task {id} is {state}; stop its active turn before deleting it.")]
    TaskDeletionBusy { id: String, state: &'static str },
    #[error("task {id} is {state}; it cannot accept a prompt")]
    TaskNotReady { id: String, state: &'static str },
    #[error("task {id} is {state}; only archived tasks can be reopened")]
    TaskNotArchived { id: String, state: &'static str },
    #[error("task {id} has no recorded branch for worktree collection")]
    CollectionBranchMissing { id: String },
    #[error("task {id} is {state}; it cannot be cancelled")]
    TaskNotCancellable { id: String, state: &'static str },
    #[error("task {id} is {state}; stop its turn before compacting the conversation")]
    TaskCompactBusy { id: String, state: &'static str },
    #[error("task {id} is {state}; wait for its active turn before switching agents")]
    TaskAgentSwitchBusy { id: String, state: &'static str },
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
    #[error(
        "task {id} cannot move from {from} to {to}; from {from} Shepherd allows {allowed}. Nothing was changed."
    )]
    InvalidTransition {
        id: String,
        from: &'static str,
        to: &'static str,
        allowed: &'static str,
    },
    #[error("quit requires confirmation: {live_tasks} live task(s); retry with force to stop them")]
    QuitConfirmationRequired {
        live_tasks: usize,
        live_terminals: usize,
    },
    #[error("{reason}")]
    PoolUnavailable { reason: String },
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
    Harness(#[from] shep_harness_core::HarnessError),
}

/// What opening an agent session actually did. Resume reports this to the user
/// instead of pretending a stored thread was picked up.
#[doc(hidden)]
pub enum SessionOpened {
    Fresh,
    Resumed(String),
    ResumeFailed(String),
}

struct QueuedReviewNote {
    text: String,
    ready: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolForceRequest {
    pub slot_id: PoolSlotId,
    pub now: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolForceOutcome {
    pub forced_slot: Option<PoolSlotId>,
    pub applied: bool,
    pub reason: String,
}

/// The core owns all mutable state. It is a library, not a daemon.
pub struct Core {
    #[doc(hidden)]
    pub projects: BTreeMap<ProjectId, Project>,
    #[doc(hidden)]
    pub project_order: Vec<ProjectId>,
    project_preview_overrides: BTreeMap<ProjectId, url::Url>,
    project_setups: BTreeMap<PathBuf, ProjectSetup>,
    #[doc(hidden)]
    pub tasks: BTreeMap<TaskId, Task>,
    workspace_layouts: BTreeMap<TaskId, WorkspaceLayout>,
    #[doc(hidden)]
    pub sessions: BTreeMap<TaskId, HarnessSession>,
    detached_transcripts: BTreeMap<TaskId, Transcript>,
    turn_baselines: BTreeMap<TaskId, TurnBaseline>,
    review_notes: BTreeMap<TaskId, QueuedReviewNote>,
    queued_prompts: BTreeMap<TaskId, VecDeque<PromptInput>>,
    /// The newest durable session per task, kept across restarts so resume can
    /// ask the adapter for the thread the previous process opened.
    #[doc(hidden)]
    pub session_records: BTreeMap<TaskId, SessionRecord>,
    terminal_layouts: BTreeMap<ProjectId, TerminalLayout>,
    terminals: BTreeMap<TerminalId, TerminalRecord>,
    terminal_list_revision: u64,
    archived_worktree_issues: Vec<ArchivedWorktreeIssue>,
    /// A slot the user forced for the next turn. This is session state, so it is
    /// deliberately absent from the store and does not survive a restart.
    forced_pool_slot: Option<PoolSlotId>,
    pool_configuration_error: Option<String>,
    #[doc(hidden)]
    pub store: Arc<dyn Store>,
    harness_factory: Box<dyn HarnessFactory>,
    shutdown: ShutdownState,
    #[doc(hidden)]
    pub shutdown_events: BTreeMap<TaskId, Vec<HarnessEvent>>,
    shutdown_interrupted: BTreeSet<TaskId>,
    events: mpsc::Sender<Event>,
    event_receiver: mpsc::Receiver<Event>,
    events_saturated: AtomicBool,
}

pub struct TaskGitRead {
    project_path: PathBuf,
    worktree_path: PathBuf,
    task_id: String,
    state: &'static str,
    base_branch: Option<String>,
    branch: Option<String>,
    approved_tip_sha: Option<String>,
}

#[derive(Clone)]
pub struct ProjectSetup {
    pub script: String,
    pub approved: bool,
    pub environment: Option<worktree_environment::WorktreeSetup>,
    pub environment_approved: bool,
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

    pub fn sync_git(&self, request: &git::GitSyncRequest) -> Result<(), CoreError> {
        self.repository()?;
        let branch = self.branch.as_deref().ok_or(CoreError::TaskBranchFixed)?;
        GitService.sync(
            &self.worktree_path,
            branch,
            self.base_branch.as_deref(),
            request,
        )?;
        Ok(())
    }

    pub fn git_snapshot(&self) -> Result<git::GitSnapshot, CoreError> {
        let repository = self.repository()?;
        let mut snapshot = GitService.snapshot(&self.worktree_path)?;
        snapshot.project_root = Some(GitService.checkout_status(&repository.root)?);
        if let Some(branch) = self
            .branch
            .as_deref()
            .filter(|branch| snapshot.branch.as_deref() == Some(*branch))
        {
            snapshot.sync =
                GitService.sync_status(&self.worktree_path, branch, self.base_branch.as_deref())?;
        }
        snapshot.merge_blocked = match (&self.base_branch, &self.branch) {
            (Some(base), Some(branch)) => {
                GitService.merge_blocked(&repository.root, &self.worktree_path, base, branch)?
            }
            _ => Some("The task has no recorded branch.".into()),
        };
        if snapshot.merge_blocked.is_none() {
            snapshot.merge_blocked = match &self.branch {
                Some(branch)
                    if self.approved_tip_sha.as_ref()
                        == Some(&GitService.checked_out_tip(&self.worktree_path, branch)?) =>
                {
                    None
                }
                _ => Some("Approve the current committed changes before merging.".into()),
            };
        }
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
    pub fn set_project_setup(&mut self, project: PathBuf, setup: Option<ProjectSetup>) {
        if let Some(setup) = setup {
            self.project_setups.insert(project, setup);
        } else {
            self.project_setups.remove(&project);
        }
    }

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
        let (events, event_receiver) = mpsc::channel(MAX_PENDING_EVENTS);
        Self {
            projects: BTreeMap::new(),
            project_order: Vec::new(),
            project_preview_overrides: BTreeMap::new(),
            project_setups: BTreeMap::new(),
            tasks: BTreeMap::new(),
            workspace_layouts: BTreeMap::new(),
            sessions: BTreeMap::new(),
            detached_transcripts: BTreeMap::new(),
            turn_baselines: BTreeMap::new(),
            review_notes: BTreeMap::new(),
            queued_prompts: BTreeMap::new(),
            session_records: BTreeMap::new(),
            terminal_layouts: BTreeMap::new(),
            terminals: BTreeMap::new(),
            terminal_list_revision: 0,
            archived_worktree_issues: Vec::new(),
            forced_pool_slot: None,
            pool_configuration_error: None,
            store: Arc::from(store),
            harness_factory,
            shutdown: ShutdownState::Running,
            shutdown_events: BTreeMap::new(),
            shutdown_interrupted: BTreeSet::new(),
            events,
            event_receiver,
            events_saturated: AtomicBool::new(false),
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

        let saved_order = core.store.load_project_order().await?;
        let mut ordered = Vec::new();
        for id in saved_order
            .into_iter()
            .chain(core.project_order.iter().cloned())
        {
            if core.projects.contains_key(&id) && !ordered.contains(&id) {
                ordered.push(id);
            }
        }
        core.project_order = ordered;

        for task in core.store.load_tasks().await? {
            core.tasks.insert(task.id.clone(), task);
        }

        for (task, prompts) in core.store.load_prompt_queues().await? {
            if core.tasks.contains_key(&task) {
                core.queued_prompts
                    .insert(task, prompts.into_iter().collect());
            } else {
                tracing::warn!(%task, "ignored a prompt queue for a missing task");
            }
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
        core.reconcile_worktree_pool().await?;
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

    #[doc(hidden)]
    pub async fn collect_archived_worktree(&mut self, task_id: &TaskId) -> Result<(), CoreError> {
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
            layout: self
                .terminal_layouts
                .get(project_id)
                .cloned()
                .unwrap_or_default(),
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
        if current.revision != layout.revision {
            return Err(CoreError::TerminalLayoutChanged);
        }
        if !self.valid_terminal_layout(project_id, &layout, |id| {
            current.root.as_ref().is_some_and(|root| root.contains(id))
        }) {
            return Err(CoreError::InvalidTerminalLayout);
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

    pub fn set_pool_configuration_error(&mut self, error: Option<String>) {
        self.pool_configuration_error = error;
    }

    pub async fn pool_status(&mut self, now: u64) -> Result<PoolStatus, CoreError> {
        let mut status = self.store.load_pool_status(now).await?;
        let forced_is_known = self
            .forced_pool_slot
            .as_ref()
            .is_some_and(|forced| status.slots.iter().any(|entry| &entry.slot.id == forced));
        if !forced_is_known {
            self.forced_pool_slot = None;
        }
        status.forced_slot = self.forced_pool_slot.clone();
        Ok(status)
    }

    pub async fn force_pool_slot(
        &mut self,
        request: &PoolForceRequest,
    ) -> Result<PoolForceOutcome, CoreError> {
        if request.slot_id.as_str().trim().is_empty() {
            return Err(StoreError::InvalidRow("pool slot id is empty".to_string()).into());
        }
        let status = self.pool_status(request.now).await?;
        let Some(entry) = status
            .slots
            .iter()
            .find(|entry| entry.slot.id == request.slot_id)
        else {
            return Err(StoreError::PoolSlotNotFound {
                slot_id: request.slot_id.clone(),
            }
            .into());
        };
        if entry.availability.state.as_str() != "available" {
            return Ok(PoolForceOutcome {
                forced_slot: self.forced_pool_slot.clone(),
                applied: false,
                reason: format!(
                    "cannot force a {} slot: {}",
                    entry.availability.state.as_str(),
                    entry.availability.reason
                ),
            });
        }
        if self.forced_pool_slot.as_ref() == Some(&request.slot_id) {
            return Ok(PoolForceOutcome {
                forced_slot: self.forced_pool_slot.clone(),
                applied: false,
                reason: "slot is already forced for the next turn".to_string(),
            });
        }
        self.forced_pool_slot = Some(request.slot_id.clone());
        Ok(PoolForceOutcome {
            forced_slot: self.forced_pool_slot.clone(),
            applied: true,
            reason: "slot forced for the next turn".to_string(),
        })
    }

    pub fn clear_pool_force(&mut self) -> PoolForceOutcome {
        match self.forced_pool_slot.take() {
            Some(slot_id) => PoolForceOutcome {
                forced_slot: None,
                applied: true,
                reason: format!("force for {} cleared", slot_id.as_str()),
            },
            None => PoolForceOutcome {
                forced_slot: None,
                applied: false,
                reason: "no slot is forced for the next turn".to_string(),
            },
        }
    }

    /// Builds the request a turn's selection uses, consuming a pending force so
    /// it applies to exactly one turn.
    pub fn pool_selection_request(
        &mut self,
        request_id: impl Into<String>,
        now: u64,
    ) -> PoolSelectionRequest {
        let mut request = PoolSelectionRequest::new(request_id, now);
        request.override_slot = self.forced_pool_slot.take();
        request
    }

    pub async fn reorder_pool_slots(
        &mut self,
        request: &PoolReorderRequest,
    ) -> Result<PoolReorderOutcome, CoreError> {
        Ok(self.store.reorder_pool_slots(request).await?)
    }

    pub async fn set_pool_slot_enabled(
        &mut self,
        request: &PoolSlotEnabledRequest,
    ) -> Result<PoolSlotEnabledOutcome, CoreError> {
        Ok(self.store.set_pool_slot_enabled(request).await?)
    }

    pub async fn correct_pool_quota(
        &mut self,
        request: &PoolQuotaCorrectionRequest,
    ) -> Result<PoolQuotaCorrectionOutcome, CoreError> {
        Ok(self.store.correct_pool_quota(request).await?)
    }

    pub async fn clear_pool_slot_cooldown(
        &mut self,
        request: &PoolCooldownClearRequest,
    ) -> Result<PoolQuotaCorrectionOutcome, CoreError> {
        Ok(self.store.clear_pool_slot_cooldown(request).await?)
    }

    pub async fn apply_pool_catalog(
        &mut self,
        advisory: &PoolCatalogAdvisory,
        now: u64,
    ) -> Result<PoolCatalogOutcome, CoreError> {
        Ok(self
            .store
            .apply_pool_catalog_advisory(advisory, now)
            .await?)
    }

    /// Adds a slot the user chose. The catalog can offer provider/model
    /// choices, but only this explicit call creates a slot.
    pub async fn add_pool_slot(
        &mut self,
        agent: Agent,
        provider_id: &str,
        provider_name: &str,
        model_id: &str,
        model_name: &str,
    ) -> Result<PoolSlot, CoreError> {
        let provider_id = provider_id.trim();
        let model_id = model_id.trim();
        if provider_id.is_empty() || model_id.is_empty() {
            return Err(CoreError::Store(StoreError::InvalidRow(
                "pool slot provider and model ids must not be empty".to_string(),
            )));
        }
        let slots = self.store.load_pool_slots().await?;
        if slots.iter().any(|slot| {
            slot.agent == agent && slot.provider_id == provider_id && slot.model_id == model_id
        }) {
            return Err(CoreError::Store(StoreError::InvalidRow(
                "pool slot already exists".to_string(),
            )));
        }
        let priority = slots
            .iter()
            .map(|slot| slot.priority)
            .max()
            .map_or(0, |priority| priority + 1);
        let mut slot = PoolSlot::new(
            agent,
            provider_id,
            provider_name.trim(),
            model_id,
            model_name.trim(),
        );
        slot.priority = priority;
        self.store.save_pool_slot(&slot).await?;
        Ok(slot)
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

    pub async fn promote_terminal(
        &mut self,
        terminal_id: &TerminalId,
    ) -> Result<TerminalList, CoreError> {
        let mut terminal = self.terminal_record(terminal_id)?;
        if terminal.scope != crate::terminal::TerminalScope::Task {
            return Err(CoreError::Store(StoreError::InvalidRow(
                "Only task terminals can be shared".into(),
            )));
        }
        terminal.scope = crate::terminal::TerminalScope::Project;
        terminal.task_id = None;
        self.record_terminal(&terminal).await?;
        Ok(self.terminal_list(&terminal.project_id))
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

        // A quit keeps its ended records so the next launch can show the saved
        // layout with a restart. Freebuff and preview sessions also retain
        // their records so their output remains available after exit.
        if self.shutdown != ShutdownState::Running
            || seen.scope == TerminalScope::Freebuff
            || seen.id.as_str().starts_with("shepherd-preview:")
        {
            let mut terminal = seen.clone();
            terminal.ended_at = Some(crate::projects::now_millis());
            terminal.exit_code = exit_code;
            self.store.save_terminal(&terminal).await?;
            let project_id = terminal.project_id.clone();
            self.terminals.insert(terminal_id.clone(), terminal);
            self.publish_terminal_list(&project_id);
            return Ok(());
        }

        let project_id = seen.project_id.clone();
        self.remove_terminal_record(terminal_id, project_id).await
    }

    pub async fn remove_ended_terminal(
        &mut self,
        terminal_id: &TerminalId,
    ) -> Result<(), CoreError> {
        let terminal = self.terminal_record(terminal_id)?;
        if terminal.ended_at.is_none() {
            return Err(CoreError::TerminalStillRunning {
                id: terminal_id.as_str().to_string(),
            });
        }
        self.remove_terminal_record(terminal_id, terminal.project_id)
            .await
    }

    async fn remove_terminal_record(
        &mut self,
        terminal_id: &TerminalId,
        project_id: ProjectId,
    ) -> Result<(), CoreError> {
        let mut layout = self.terminal_layout(&project_id)?;
        let previous = layout.root.clone();
        let removed = std::collections::BTreeSet::from([terminal_id.as_str().to_owned()]);
        layout.root = layout.root.and_then(|root| root.without(&removed));
        let changed = layout.root != previous;
        if changed {
            layout.revision = layout
                .revision
                .checked_add(1)
                .ok_or(CoreError::TerminalLayoutChanged)?;
        }
        self.store
            .delete_terminal(terminal_id, changed.then_some((&project_id, &layout)))
            .await?;
        self.terminals.remove(terminal_id);
        if changed {
            self.terminal_layouts.insert(project_id.clone(), layout);
        }
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
        self.execute_with_draft_observer(command, None).await
    }

    pub async fn execute_with_draft_observer(
        &mut self,
        command: Command,
        observer: Option<&(dyn Fn(&TaskId) + Send + Sync)>,
    ) -> Result<CommandResult, CoreError> {
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
            Command::StartTask {
                project_id,
                agent,
                input,
            } => self.start_task(&project_id, agent, input, observer).await,
            Command::SetDraftAgent { task_id, agent } => {
                self.set_draft_agent(&task_id, agent).await
            }
            Command::SwitchTaskAgent { task_id, agent } => {
                self.switch_task_agent(&task_id, agent).await
            }
            Command::DiscardDraft { task_id } => self.discard_draft(&task_id).await,
            Command::DeleteTask {
                task_id,
                remove_worktree,
            } => self.delete_task(&task_id, remove_worktree).await,
            Command::SendPrompt { task_id, input } => self.send_prompt(&task_id, input).await,
            Command::SetTaskOptions {
                task_id,
                model,
                mode,
            } => self.set_task_options(&task_id, model, mode).await,
            Command::RetryTaskBranch { task_id } => self.retry_task_branch(&task_id).await,
            Command::SetSessionMode { task_id, mode } => {
                self.set_session_mode(&task_id, &mode).await
            }
            Command::ResolveApproval {
                task_id,
                approval_id,
                decision,
            } => self.resolve_approval(&task_id, approval_id, decision).await,
            Command::AnswerQuestion {
                task_id,
                question_id,
                answer,
            } => self.answer_question(&task_id, &question_id, &answer).await,
            Command::CancelTask { task_id } => self.cancel_task(&task_id).await,
            Command::CompactTask { task_id } => self.compact_task(&task_id).await,
            Command::ReviewTask { task_id, action } => self.review_task(&task_id, action).await,
            Command::ArchiveReviewed { task_ids } => self.archive_reviewed(task_ids).await,
            Command::Quit { force } => self.quit(force).await,
        }
    }

    /// Events produced since the last drain. A full queue drops its newest
    /// event and reports [`Event::ResyncRequired`] with the next drain so the
    /// shell reloads snapshots instead of missing a change silently.
    pub fn drain_events(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        while let Ok(event) = self.event_receiver.try_recv() {
            events.push(event);
        }
        if self.events_saturated.swap(false, Ordering::Relaxed) {
            events.push(Event::ResyncRequired);
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
                if self.persist_task_transcript(&task_id, false).await? {
                    self.emit(Event::TaskWorkspaceChanged(self.task_workspace(&task_id)?));
                    changed = true;
                }
                continue;
            }
            let before = TaskSummary::from(self.task(&task_id)?);
            self.apply_events(&task_id, &events)?;
            self.maybe_create_task_branch(&task_id, &events).await?;
            let after = TaskSummary::from(self.task(&task_id)?);
            if (after.state, &after.error) != (before.state, &before.error) {
                self.save_task(&task_id).await?;
            } else {
                self.persist_task_transcript(&task_id, false).await?;
            }
            if after != before {
                self.publish_task(&task_id)?;
            } else {
                self.emit(Event::TaskWorkspaceChanged(self.task_workspace(&task_id)?));
            }
            changed = true;
        }
        if self.shutdown == ShutdownState::Running {
            changed |= self.drain_queued_prompts().await?;
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

    pub async fn reorder_projects(
        &mut self,
        order: Vec<ProjectId>,
    ) -> Result<ProjectList, CoreError> {
        let current: std::collections::BTreeSet<_> = self.project_order.iter().collect();
        let proposed: std::collections::BTreeSet<_> = order.iter().collect();
        if order.len() != self.project_order.len() || current != proposed {
            return Err(StoreError::Backend(
                "Project list changed; reload before reordering.".into(),
            )
            .into());
        }
        self.store.save_project_order(&order).await?;
        self.project_order = order;
        let projects = self.project_list();
        self.emit(Event::ProjectListChanged(projects.clone()));
        Ok(projects)
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

    pub fn task_git_sync(&self, task_id: &TaskId) -> Result<TaskGitRead, CoreError> {
        let task = self.task(task_id)?;
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
        self.task_git_read(task_id)
    }

    pub fn finish_git_pull(
        &self,
        task_id: &TaskId,
        request: &git::GitSyncRequest,
    ) -> Result<(), CoreError> {
        let context = self.task_git_sync(task_id)?;
        context.repository()?;
        let branch = context
            .branch
            .as_deref()
            .ok_or(CoreError::TaskBranchFixed)?;
        GitService.finish_pull(
            &context.worktree_path,
            branch,
            context.base_branch.as_deref(),
            request,
        )?;
        Ok(())
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
            approved_tip_sha: task.approved_tip_sha.clone(),
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
            git::GitMutation::Checkout { .. } | git::GitMutation::CreateBranch { .. } => {
                return Err(CoreError::TaskBranchFixed);
            }
            operation @ (git::GitMutation::SquashMerge | git::GitMutation::MergeEach) => {
                if self
                    .task_git_read(task_id)?
                    .git_snapshot()?
                    .merge_blocked
                    .is_some()
                {
                    return Err(GitError::MergeUnavailable {
                        reason: "Review approval or merge prerequisites are missing".into(),
                    }
                    .into());
                }
                let (Some(base), Some(branch)) = (&task.base_branch, &task.branch) else {
                    return Err(GitError::MergeUnavailable {
                        reason: "the task has no recorded branch".into(),
                    }
                    .into());
                };
                let mut updated = task.clone();
                updated.branch_tip_sha = Some(GitService.checked_out_tip(path, branch)?);
                self.store.save_task(&updated).await?;
                let outcome = if matches!(operation, git::GitMutation::SquashMerge) {
                    GitService.squash_merge(&repository.root, path, base, branch, &task.title)
                } else {
                    GitService.merge_each(&repository.root, path, base, branch)
                };
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
            default_mode: session.and_then(HarnessSession::default_mode),
            models: session.and_then(HarnessSession::models),
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
        if self.project_preview_overrides.contains_key(&project_id) {
            return Err(CoreError::Store(StoreError::InvalidRow(
                "Preview URL is set in the project's .config/shepherd/settings.json".into(),
            )));
        }
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

    async fn start_task(
        &mut self,
        project_id: &ProjectId,
        agent: Agent,
        input: PromptInput,
        observer: Option<&(dyn Fn(&TaskId) + Send + Sync)>,
    ) -> Result<CommandResult, CoreError> {
        if input.text.trim().is_empty() {
            return Err(CoreError::EmptyPrompt);
        }
        if input.codex != Default::default() && agent != Agent::Codex {
            return Err(CoreError::CodexOptionsForOtherAgent);
        }
        let title = input
            .text
            .lines()
            .find(|line| !line.trim().is_empty())
            .map(str::trim)
            .unwrap_or_default()
            .chars()
            .take(80)
            .collect::<String>();
        let created = self.create_draft(project_id, title, agent).await?;
        let CommandResult::TaskWorkspace(workspace) = created else {
            return Err(CoreError::TaskNotFound {
                id: "new task".to_owned(),
            });
        };
        let task_id = TaskId::new(workspace.task.id);
        if let Some(observer) = observer {
            observer(&task_id);
        }
        let started = async {
            self.set_task_options(&task_id, input.model.clone(), input.mode.clone())
                .await?;
            self.send_prompt(&task_id, input).await
        }
        .await;
        match started {
            Ok(result) => Ok(result),
            Err(CoreError::ProjectSetup(message)) => {
                self.publish_task(&task_id)?;
                Err(CoreError::StartTaskSetup {
                    task_id: task_id.to_string(),
                    message,
                })
            }
            Err(
                error @ (CoreError::DevelopmentEnvironment { .. }
                | CoreError::WorktreeSetupPending { .. }),
            ) => Err(error),
            Err(error) => {
                let remove_worktree = self
                    .task(&task_id)
                    .ok()
                    .is_some_and(|task| task.worktree_path.is_some());
                if remove_worktree
                    && let Some(setup) = self
                        .project_setups
                        .get(&self.project_path(project_id)?)
                        .and_then(|setup| setup.environment.as_ref())
                {
                    return Err(CoreError::DevelopmentEnvironment {
                        task_id: task_id.to_string(),
                        message: error.to_string(),
                        setup_terminal: setup.setup_terminal.is_some(),
                    });
                }
                match self.delete_task(&task_id, remove_worktree).await {
                    Ok(_) => Err(error),
                    Err(cleanup) => {
                        let reason = if self.task(&task_id).is_ok() {
                            match self.delete_task_records(&task_id).await {
                                Ok(_) => cleanup.to_string(),
                                Err(records) => {
                                    format!("{cleanup}; could not remove task records: {records}")
                                }
                            }
                        } else {
                            cleanup.to_string()
                        };
                        Err(CoreError::StartTaskCleanup {
                            task_id: task_id.to_string(),
                            prompt_error: error.to_string(),
                            reason,
                        })
                    }
                }
            }
        }
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

    async fn switch_task_agent(
        &mut self,
        task_id: &TaskId,
        agent: Agent,
    ) -> Result<CommandResult, CoreError> {
        let mut task = self.task(task_id)?.clone();
        if task.state.is_live() {
            return Err(CoreError::TaskAgentSwitchBusy {
                id: task_id.as_str().to_owned(),
                state: task.state.as_str(),
            });
        }
        if task.agent == agent {
            return Ok(CommandResult::TaskWorkspace(self.publish_task(task_id)?));
        }
        if let Some(session) = self.sessions.get_mut(task_id) {
            session.shutdown().await?;
        }
        self.persist_task_transcript(task_id, true).await?;
        self.end_session_record(task_id, &[]).await?;
        if let Some(mut record) = self.session_records.get(task_id).cloned() {
            record.adapter_session_id = None;
            record.capabilities = None;
            self.store.save_session(&record).await?;
            self.session_records.insert(task_id.clone(), record);
        }
        task.agent = agent;
        task.tier = agent.default_tier();
        task.model = None;
        task.mode = None;
        self.store.save_task(&task).await?;
        self.tasks.insert(task_id.clone(), task);
        if let Some(session) = self.sessions.remove(task_id) {
            let mut transcript = Transcript::default();
            transcript.restore(session.transcript().entries().to_vec());
            self.detached_transcripts
                .insert(task_id.clone(), transcript);
        }
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
        #[cfg(any(test, feature = "test-support"))]
        pause_worktree_deletion_fixture("staged", task_id).await?;
        let result = match self.delete_task_records(task_id).await {
            Ok(result) => result,
            Err(error) => return Err(self.rollback_worktree_deletion(&deletion, error).await),
        };
        #[cfg(any(test, feature = "test-support"))]
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
        let task = self.task(task_id)?.clone();
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
        if let Some(path) = task.worktree_path.as_ref()
            && let Some(record) = self
                .store
                .load_worktree_pool()
                .await?
                .into_iter()
                .find(|record| record.project_id == project_id && &record.path == path)
        {
            self.store.remove_worktree_pool(&record.id).await?;
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
        self.queued_prompts.remove(task_id);
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

    #[doc(hidden)]
    pub async fn review_task(
        &mut self,
        task_id: &TaskId,
        action: ReviewAction,
    ) -> Result<CommandResult, CoreError> {
        if matches!(action, ReviewAction::Reopen) && self.task(task_id)?.collection_pending {
            self.touch_archived_worktree(task_id).await?;
        }
        let mut task = self.task(task_id)?.clone();
        match action {
            ReviewAction::Approve => {
                if task.state != TaskState::Review {
                    return Err(CoreError::TaskNotReady {
                        id: task_id.to_string(),
                        state: task.state.as_str(),
                    });
                }
                let (_, _, path) = self.task_git_worktree(task_id)?;
                let branch = task
                    .branch
                    .as_deref()
                    .ok_or_else(|| GitError::MergeUnavailable {
                        reason: "the task has no recorded branch".into(),
                    })?;
                if !GitService.snapshot(path)?.files.is_empty() {
                    return Err(GitError::MergeUnavailable {
                        reason: "commit all changes before approving".into(),
                    }
                    .into());
                }
                task.approved_tip_sha = Some(GitService.checked_out_tip(path, branch)?);
                task.updated_at = crate::projects::now_millis();
            }
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

    #[doc(hidden)]
    pub async fn archive_reviewed(
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
            self.persist_task_transcript(&task.id, false).await?;
        }
        self.store.archive_tasks(&archived).await?;
        let mut projects = BTreeSet::new();
        let mut task_ids = Vec::new();
        for task in archived {
            self.review_notes.remove(&task.id);
            self.queued_prompts.remove(&task.id);
            projects.insert(task.project_id.clone());
            task_ids.push(task.id.clone());
            self.tasks.insert(task.id.clone(), task);
        }
        for task_id in task_ids {
            self.release_worktree_to_pool(&task_id).await?;
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

    /// A live turn owns the session. Keep later prompts in per-task order until
    /// the turn settles, then start exactly one new turn per pump.
    async fn drain_queued_prompts(&mut self) -> Result<bool, CoreError> {
        let mut changed = false;
        while let Some((task_id, input)) = self.queued_prompts.iter().find_map(|(id, prompts)| {
            let task = self.tasks.get(id)?;
            if !matches!(task.state, TaskState::Ready | TaskState::Review) {
                return None;
            }
            prompts.front().cloned().map(|input| (id.clone(), input))
        }) {
            match self.send_prompt(&task_id, input.clone()).await {
                Ok(_) => {
                    changed = true;
                    let remaining = self
                        .queued_prompts
                        .get_mut(&task_id)
                        .map(|prompts| {
                            prompts.pop_front();
                            prompts.iter().cloned().collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    if remaining.is_empty() {
                        self.queued_prompts.remove(&task_id);
                    }
                    self.store.save_prompt_queue(&task_id, &remaining).await?;
                }
                Err(error) => {
                    self.emit(Event::Error {
                        task_id: Some(task_id.to_string()),
                        message: format!("Could not send a queued prompt: {error}"),
                    });
                    break;
                }
            }
        }
        Ok(changed)
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
        let task_options = self.task(task_id)?;
        if input.mode.is_none() {
            input.mode = task_options.mode.clone();
        }
        if input.model.is_none() {
            input.model = task_options.model.clone();
        }

        let state = self.task(task_id)?.state;
        if state.is_live() {
            if self
                .sessions
                .get(task_id)
                .is_some_and(HarnessSession::is_cancelling)
            {
                return Err(CoreError::TaskNotReady {
                    id: task_id.as_str().to_string(),
                    state: state.as_str(),
                });
            }
            let mut prompts = self
                .queued_prompts
                .get(task_id)
                .cloned()
                .unwrap_or_default();
            prompts.push_back(input);
            let persisted = prompts.iter().cloned().collect::<Vec<_>>();
            self.store.save_prompt_queue(task_id, &persisted).await?;
            self.queued_prompts.insert(task_id.clone(), prompts);
            return Ok(CommandResult::TaskWorkspace(self.publish_task(task_id)?));
        }
        let project_path = self.task_project_path(task_id)?;
        let setup = self.project_setups.get(&project_path).cloned();
        let repository = GitService::discover(&project_path)?;
        let setup_marker = repository
            .git_dir
            .join("shepherd")
            .join("setup")
            .join(task_id.as_str());
        if state == TaskState::Draft
            && setup
                .as_ref()
                .is_some_and(|setup| !setup.script.trim().is_empty())
        {
            let directory = setup_marker
                .parent()
                .ok_or_else(|| CoreError::ProjectSetup("Invalid setup state path.".into()))?;
            std::fs::create_dir_all(directory).map_err(|error| {
                CoreError::ProjectSetup(format!("Could not save setup state: {error}"))
            })?;
            std::fs::write(&setup_marker, b"pending").map_err(|error| {
                CoreError::ProjectSetup(format!("Could not save setup state: {error}"))
            })?;
        }
        match state {
            TaskState::Draft => {
                self.create_worktree(task_id).await?;
                self.publish_task(task_id)?;
            }
            TaskState::Ready | TaskState::Review | TaskState::Interrupted | TaskState::Failed => {
                self.repair_worktree(task_id).await?
            }
            state => {
                return Err(CoreError::TaskNotReady {
                    id: task_id.as_str().to_string(),
                    state: state.as_str(),
                });
            }
        }

        if setup
            .as_ref()
            .is_some_and(|setup| setup.environment.is_some() && !setup.environment_approved)
        {
            return Err(CoreError::ProjectSetup(
                "Worktree ready. Review and approve the project’s worktree commands in Project settings.".into(),
            ));
        }

        if setup_marker.exists()
            && setup
                .as_ref()
                .and_then(|setup| setup.environment.as_ref())
                .and_then(|environment| environment.use_imported_setup)
                != Some(false)
        {
            let setup = setup.as_ref().filter(|setup| !setup.script.trim().is_empty()).ok_or_else(|| {
                CoreError::ProjectSetup("The pending worktree setup is unavailable. Restore it in Project settings before retrying.".into())
            })?;
            if !setup.approved {
                return Err(CoreError::ProjectSetup("Review and approve the worktree setup script in Project settings before starting this task.".into()));
            }
            let directory = self.task(task_id)?.worktree_path.as_ref().ok_or_else(|| {
                CoreError::ProjectSetup("The task has no worktree for setup.".into())
            })?;
            let mut command: Vec<String> = if cfg!(target_os = "windows") {
                vec![
                    "powershell.exe".into(),
                    "-NoProfile".into(),
                    "-Command".into(),
                ]
            } else {
                vec!["/bin/sh".into(), "-c".into()]
            };
            command.push(setup.script.clone());
            let step = worktree_environment::SetupCommand {
                name: "Imported worktree setup".into(),
                command,
                required: true,
            };
            if !worktree_environment::record_command(
                &worktree_environment::runs_path(&repository.git_dir, task_id),
                "imported-setup".into(),
                &step,
                directory,
                false,
                self.task(task_id)?.agent,
            )
            .await?
            {
                return Err(CoreError::DevelopmentEnvironment {
                    task_id: task_id.to_string(),
                    message: "Worktree setup failed. See Setup runs for output.".into(),
                    setup_terminal: false,
                });
            }
            std::fs::remove_file(&setup_marker).map_err(|error| {
                CoreError::ProjectSetup(format!(
                    "Setup completed but its state could not be saved: {error}"
                ))
            })?;
        }

        let mut prepared_environment = None;
        if let Some(setup) = &setup
            && let Some(environment) = &setup.environment
        {
            let directory = self
                .task(task_id)?
                .worktree_path
                .as_deref()
                .ok_or_else(|| {
                    CoreError::ProjectSetup("The task has no worktree for setup.".into())
                })?;
            let runs_path = worktree_environment::runs_path(&repository.git_dir, task_id);
            let agent = self.task(task_id)?.agent;
            prepared_environment = self
                .harness_factory
                .prepare_worktree_environment(task_id, agent, directory, &runs_path, environment)
                .await?;
            if prepared_environment.is_none() {
                worktree_environment::prepare(environment, &runs_path, directory, task_id, agent)
                    .await?;
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
        // The next prompt is what reattaches a task; opening its conversation
        // never starts a process. A recorded session is picked up here when the
        // adapter still supports it, and a fresh one starts when it does not.
        let stored = self
            .session_records
            .get(task_id)
            .and_then(SessionRecord::resume_handle)
            .map(SessionId::new);
        let opened = self
            .ensure_session_with_environment(task_id, stored, prepared_environment)
            .await;
        if let Ok(opened) = &opened
            && let Some(session) = self.sessions.get_mut(task_id)
        {
            match opened {
                SessionOpened::Resumed(session_id) => session.note(
                    format!("resumed the previous agent session {session_id}"),
                    false,
                ),
                SessionOpened::ResumeFailed(reason) => session.note(
                    format!(
                        "Could not resume the previous session: {reason}. Started a new session."
                    ),
                    true,
                ),
                SessionOpened::Fresh => {}
            }
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

        self.fallback_model(task_id, &mut input);

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
        self.maybe_create_task_branch(task_id, &events).await?;
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
            return Err(shep_harness_core::HarnessError::ModeUnavailable.into());
        }
        let session = self
            .sessions
            .get_mut(task_id)
            .ok_or(shep_harness_core::HarnessError::ModeUnavailable)?;
        let events = session.set_mode(mode).await?;
        self.apply_events(task_id, &events)?;
        self.save_task(task_id).await?;
        Ok(CommandResult::TaskWorkspace(self.publish_task(task_id)?))
    }

    async fn set_task_options(
        &mut self,
        task_id: &TaskId,
        model: Option<String>,
        mode: Option<String>,
    ) -> Result<CommandResult, CoreError> {
        let task = self
            .tasks
            .get_mut(task_id)
            .ok_or_else(|| CoreError::TaskNotFound {
                id: task_id.as_str().to_string(),
            })?;
        task.model = model;
        task.mode = mode;
        task.updated_at = projects::now_millis();
        self.save_task(task_id).await?;
        Ok(CommandResult::TaskWorkspace(self.publish_task(task_id)?))
    }

    async fn retry_task_branch(&mut self, task_id: &TaskId) -> Result<CommandResult, CoreError> {
        let task = self.task(task_id)?.clone();
        if task.branch.is_some() {
            return Ok(CommandResult::TaskWorkspace(self.publish_task(task_id)?));
        }
        self.create_task_branch(task_id).await?;
        Ok(CommandResult::TaskWorkspace(self.publish_task(task_id)?))
    }

    async fn resolve_approval(
        &mut self,
        task_id: &TaskId,
        approval_id: shep_harness_core::ApprovalId,
        decision: shep_harness_core::Decision,
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
                        shep_harness_core::HarnessError::NoPendingApproval
                            | shep_harness_core::HarnessError::ApprovalOptionUnavailable
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

    async fn answer_question(
        &mut self,
        task_id: &TaskId,
        question_id: &str,
        answer: &str,
    ) -> Result<CommandResult, CoreError> {
        let session = self
            .sessions
            .get_mut(task_id)
            .ok_or_else(|| CoreError::SessionMissing {
                id: task_id.as_str().to_string(),
            })?;
        let outcome = session.answer_question(question_id, answer).await;
        let events = match outcome {
            Ok(events) => events,
            Err(error) => {
                let pending = self
                    .sessions
                    .get_mut(task_id)
                    .map(HarnessSession::pump_existing)
                    .unwrap_or_default();
                if !pending.is_empty() {
                    self.apply_events(task_id, &pending)?;
                    self.save_task(task_id).await?;
                    self.publish_task(task_id)?;
                }
                return Err(error);
            }
        };
        self.apply_events(task_id, &events)?;
        self.save_task(task_id).await?;
        Ok(CommandResult::TaskWorkspace(self.publish_task(task_id)?))
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

    /// Compact the conversation the adapter replays, keeping the session and
    /// the worktree so the next prompt fits the context window again.
    async fn compact_task(&mut self, task_id: &TaskId) -> Result<CommandResult, CoreError> {
        let state = self.task(task_id)?.state;
        if state.is_live() {
            return Err(CoreError::TaskCompactBusy {
                id: task_id.as_str().to_string(),
                state: state.as_str(),
            });
        }
        let id = task_id.as_str().to_string();
        let Some(session) = self.sessions.get_mut(task_id) else {
            return Err(CoreError::SessionMissing { id });
        };
        let events = session.compact().await?;
        self.apply_events(task_id, &events)?;
        self.save_task(task_id).await?;
        let workspace = self.publish_task(task_id)?;
        Ok(CommandResult::TaskWorkspace(workspace))
    }

    /// Resume an interrupted task with the agent session the previous process
    /// left behind, and say plainly when that session is gone.
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
        let repository = GitService::discover(&project_path)?;
        self.set_task_state(task_id, TaskState::Starting)?;
        let leased = self.lease_pool_worktree(&task, &repository).await;
        let worktree_path = match leased {
            Ok(path) => path,
            Err(error) => {
                self.fail_task(task_id, error.to_string()).await?;
                return Err(error);
            }
        };
        if let Err(error) = GitService::discover(&worktree_path) {
            self.fail_task(task_id, error.to_string()).await?;
            return Err(CoreError::Git(error));
        }

        let task = self
            .tasks
            .get_mut(task_id)
            .ok_or_else(|| CoreError::TaskNotFound {
                id: task_id.as_str().to_string(),
            })?;
        task.worktree_path = Some(worktree_path);
        task.branch = None;
        task.base_branch = Some(repository.default_branch);
        task.branch_error = None;
        task.set_state(TaskState::Ready)?;
        self.save_task(task_id).await
    }

    async fn reconcile_worktree_pool(&mut self) -> Result<(), CoreError> {
        let now = projects::now_millis();
        let records = self.store.load_worktree_pool().await?;
        let mut known = BTreeSet::new();
        for mut record in records {
            let Some(project) = self.projects.get(&record.project_id).cloned() else {
                self.store.remove_worktree_pool(&record.id).await?;
                continue;
            };
            let repository = match GitService::discover(&project.path) {
                Ok(repository) => repository,
                Err(error) => {
                    tracing::warn!(project = %project.path.display(), %error, "could not reconcile project worktree pool");
                    continue;
                }
            };
            let root = repository.git_dir.join("shepherd/worktrees");
            if record.path.parent() != Some(root.as_path()) {
                tracing::warn!(path = %record.path.display(), "removed a worktree pool record outside its project");
                self.store.remove_worktree_pool(&record.id).await?;
                continue;
            }
            known.insert(record.path.clone());
            let task = self
                .tasks
                .values()
                .find(|task| task.worktree_path.as_ref() == Some(&record.path))
                .cloned();
            let retired = record.path.join(".shepherd-retired").exists();
            let (state, task_id, leased_at) = if let Some(task) = task {
                (
                    WorktreePoolState::Leased,
                    Some(task.id),
                    record.leased_at.or(Some(task.updated_at)),
                )
            } else if !record.path.exists() {
                self.store.remove_worktree_pool(&record.id).await?;
                continue;
            } else if retired
                || GitService
                    .validate_worktree(&repository, &record.path)
                    .is_err()
                || GitService.has_changes(&record.path)?
            {
                (WorktreePoolState::Retired, None, None)
            } else {
                (WorktreePoolState::Idle, None, None)
            };
            if record.state != state || record.task_id != task_id || record.leased_at != leased_at {
                record.state = state;
                record.task_id = task_id;
                record.leased_at = leased_at;
                record.updated_at = now;
                self.store.save_worktree_pool(&record).await?;
            }
        }

        for project in self.projects.values() {
            let Ok(repository) = GitService::discover(&project.path) else {
                continue;
            };
            let root = repository.git_dir.join("shepherd/worktrees");
            let entries = match std::fs::read_dir(&root) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(source) => {
                    return Err(CoreError::Io {
                        path: root.display().to_string(),
                        source,
                    });
                }
            };
            for entry in entries.filter_map(Result::ok) {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().to_string();
                if !name.starts_with("pool-")
                    || known.contains(&path)
                    || self
                        .tasks
                        .values()
                        .any(|task| task.worktree_path.as_ref() == Some(&path))
                {
                    continue;
                }
                let state = if path.join(".shepherd-retired").exists()
                    || GitService.validate_worktree(&repository, &path).is_err()
                    || GitService.has_changes(&path)?
                {
                    WorktreePoolState::Retired
                } else {
                    WorktreePoolState::Idle
                };
                let mut record = WorktreePoolRecord::new(project.id.clone(), path);
                record.state = state;
                record.created_at = now;
                record.updated_at = now;
                self.store.save_worktree_pool(&record).await?;
            }
        }
        Ok(())
    }

    async fn lease_pool_worktree(
        &mut self,
        task: &Task,
        repository: &git::GitRepository,
    ) -> Result<PathBuf, CoreError> {
        self.reconcile_worktree_pool().await?;
        let root = repository.git_dir.join("shepherd/worktrees");
        std::fs::create_dir_all(&root).map_err(|source| CoreError::Io {
            path: root.display().to_string(),
            source,
        })?;
        let used: BTreeSet<PathBuf> = self
            .tasks
            .values()
            .filter(|other| other.id != task.id)
            .filter_map(|other| other.worktree_path.clone())
            .collect();
        let records: Vec<_> = self
            .store
            .load_worktree_pool()
            .await?
            .into_iter()
            .filter(|record| record.project_id == task.project_id)
            .collect();
        for mut record in records
            .iter()
            .filter(|record| record.state == WorktreePoolState::Idle)
            .cloned()
        {
            if used.contains(&record.path) {
                continue;
            }
            if !record.path.exists()
                || GitService
                    .validate_worktree(repository, &record.path)
                    .is_err()
            {
                self.store.remove_worktree_pool(&record.id).await?;
                continue;
            }
            if record.path.join(".shepherd-retired").exists()
                || GitService.has_changes(&record.path)?
            {
                record.state = WorktreePoolState::Retired;
                record.updated_at = projects::now_millis();
                self.store.save_worktree_pool(&record).await?;
                continue;
            }
            GitService.detach_worktree(&record.path, &repository.default_branch)?;
            record.state = WorktreePoolState::Leased;
            record.task_id = Some(task.id.clone());
            record.leased_at = Some(projects::now_millis());
            record.updated_at = projects::now_millis();
            self.store.save_worktree_pool(&record).await?;
            return Ok(record.path);
        }

        let active = records
            .iter()
            .filter(|record| record.state != WorktreePoolState::Retired)
            .count();
        if active < 3 {
            for number in 1..=3 {
                let path = root.join(format!("pool-{number}"));
                if path.exists()
                    || used.contains(&path)
                    || records.iter().any(|record| record.path == path)
                {
                    continue;
                }
                GitService.create_detached_worktree(
                    repository,
                    &path,
                    &repository.default_branch,
                )?;
                let mut record = WorktreePoolRecord::new(task.project_id.clone(), path.clone());
                record.state = WorktreePoolState::Leased;
                record.task_id = Some(task.id.clone());
                record.leased_at = Some(projects::now_millis());
                self.store.save_worktree_pool(&record).await?;
                return Ok(path);
            }
        }
        let path = root.join(format!("pool-temp-{}", uuid::Uuid::new_v4()));
        GitService.create_detached_worktree(repository, &path, &repository.default_branch)?;
        let mut record = WorktreePoolRecord::new(task.project_id.clone(), path.clone());
        record.state = WorktreePoolState::Leased;
        record.task_id = Some(task.id.clone());
        record.leased_at = Some(projects::now_millis());
        self.store.save_worktree_pool(&record).await?;
        Ok(path)
    }

    async fn maybe_create_task_branch(
        &mut self,
        task_id: &TaskId,
        events: &[HarnessEvent],
    ) -> Result<(), CoreError> {
        if self.task(task_id)?.branch.is_some() {
            return Ok(());
        }
        let succeeded = events.iter().any(|event| match event {
            HarnessEvent::Delta {
                kind: DeltaKind::Message,
                text,
                ..
            } => !text.trim().is_empty(),
            HarnessEvent::TurnFinished {
                reason: TurnFinishReason::Completed,
            } => true,
            _ => false,
        });
        if succeeded {
            self.create_task_branch(task_id).await?;
        }
        Ok(())
    }

    async fn create_task_branch(&mut self, task_id: &TaskId) -> Result<bool, CoreError> {
        let mut task = self.task(task_id)?.clone();
        if task.branch.is_some() {
            return Ok(false);
        }
        let Some(path) = task.worktree_path.clone() else {
            return Ok(false);
        };
        match GitService.create_task_branch(&path, &task.proposed_branch) {
            Ok(branch) => {
                task.branch = Some(branch);
                task.branch_error = None;
            }
            Err(error) => {
                tracing::warn!(%task_id, %error, "could not create the task branch");
                task.branch_error = Some(error.to_string());
            }
        }
        task.updated_at = projects::now_millis();
        self.store.save_task(&task).await?;
        self.tasks.insert(task_id.clone(), task);
        Ok(true)
    }

    async fn release_worktree_to_pool(&mut self, task_id: &TaskId) -> Result<(), CoreError> {
        let mut task = self.task(task_id)?.clone();
        let Some(path) = task.worktree_path.clone() else {
            return Ok(());
        };
        let repository = GitService::discover(&self.project_path(&task.project_id)?)?;
        if let Some(branch) = task.branch.as_deref() {
            task.branch_tip_sha = Some(GitService.checked_out_tip(&path, branch)?);
        }
        let dirty = GitService.has_changes(&path)?;
        if dirty {
            let marker = path.join(".shepherd-retired");
            std::fs::write(&marker, b"retired").map_err(|source| CoreError::Io {
                path: marker.display().to_string(),
                source,
            })?;
        } else {
            GitService.detach_worktree(&path, &repository.default_branch)?;
        }
        if let Some(mut record) = self
            .store
            .load_worktree_pool()
            .await?
            .into_iter()
            .find(|record| record.project_id == task.project_id && record.path == path)
        {
            record.state = if dirty {
                WorktreePoolState::Retired
            } else {
                WorktreePoolState::Idle
            };
            record.task_id = None;
            record.leased_at = None;
            record.updated_at = projects::now_millis();
            self.store.save_worktree_pool(&record).await?;
        }
        task.worktree_path = None;
        task.updated_at = projects::now_millis();
        self.store.save_task(&task).await?;
        self.tasks.insert(task_id.clone(), task);
        Ok(())
    }

    /// Open an adapter session and record it durably. A resume that the adapter
    /// cannot honour falls back to a fresh session, and the caller reports it.
    #[doc(hidden)]
    pub async fn ensure_session(
        &mut self,
        task_id: &TaskId,
        resume: Option<SessionId>,
    ) -> Result<SessionOpened, CoreError> {
        self.ensure_session_with_environment(task_id, resume, None)
            .await
    }

    async fn ensure_session_with_environment(
        &mut self,
        task_id: &TaskId,
        resume: Option<SessionId>,
        prepared_environment: Option<Vec<String>>,
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
        let project_path = self.project_path(&task.project_id)?;
        let cwd = match task.worktree_path.clone() {
            Some(worktree_path) => worktree_path,
            None => project_path.clone(),
        };
        let mut candidates = vec![task.agent];
        for agent in self.harness_factory.fallback_agents(task.agent) {
            if !candidates.contains(&agent) {
                candidates.push(agent);
            }
        }
        let mut launched = None;
        let mut launch_error = None;
        for agent in candidates {
            match self
                .harness_factory
                .launch_for_task(task_id, agent, cwd.clone())
            {
                Ok((harness, spec)) => {
                    launched = Some((harness, spec, agent));
                    break;
                }
                Err(error) => {
                    tracing::warn!(%error, agent = agent.as_str(), "agent could not launch");
                    launch_error = Some(error);
                }
            }
        }
        let (mut harness, mut spec, launched_agent) = match launched {
            Some(launch) => launch,
            None => {
                let error = launch_error.unwrap_or(CoreError::HarnessUnavailable);
                let mut transcript = Transcript::default();
                transcript.restore(carried);
                if let Some(kind) = adapter_failure_kind(&error) {
                    transcript.push_failure(
                        kind,
                        error.to_string(),
                        Some(kind.hint().to_string()),
                        None,
                    );
                }
                self.detached_transcripts
                    .insert(task_id.clone(), transcript);
                return Err(error);
            }
        };
        let has_prepared_environment = prepared_environment.is_some();
        let wrapper = prepared_environment.unwrap_or_else(|| {
            task.worktree_path
                .as_ref()
                .and_then(|_| {
                    self.project_setups
                        .get(&project_path)
                        .filter(|setup| setup.environment_approved)
                        .and_then(|setup| setup.environment.as_ref())
                        .map(|setup| setup.launch_wrapper.clone())
                })
                .unwrap_or_default()
        });
        let adapter_wraps_launch =
            wrapper.is_empty() || harness.set_launch_wrapper(wrapper.clone());
        let environment = self
            .harness_factory
            .environment_for_project(launched_agent, &project_path);
        let mut session = HarnessSession::new(harness);
        session.restore_transcript(carried);
        if launched_agent != task.agent {
            session.note(
                format!(
                    "{} is unavailable; continuing with {}.",
                    task.agent.label(),
                    launched_agent.label()
                ),
                false,
            );
            if let Some(task) = self.tasks.get_mut(task_id) {
                task.agent = launched_agent;
            }
        }
        match self
            .harness_factory
            .default_model_for_project(launched_agent, &project_path)
        {
            Ok(model) => session.set_default_model(model),
            Err(error) => return Err(self.fail_session_start(task_id, session, error)),
        }
        match environment {
            Ok(environment) => spec.environment.extend(environment),
            Err(error) => return Err(self.fail_session_start(task_id, session, error)),
        }
        let policy = ProjectApprovals::new(
            self.store.clone(),
            task.project_id.clone(),
            launched_agent,
            &spec,
        )?;
        session.set_project_approval_policy(Arc::new(policy));
        if let Err(error) =
            shep_harness_core::settings::resolve_environment(&mut spec.environment, &project_path)
                .await
        {
            return Err(self.fail_session_start(task_id, session, error.into()));
        }
        if has_prepared_environment {
            let names = spec.environment.keys().collect::<Vec<_>>();
            let names = serde_json::to_string(&names)
                .map_err(|error| CoreError::ProjectSetup(error.to_string()))?;
            spec.environment
                .insert("SHEPHERD_ENVIRONMENT_OVERRIDES".into(), names);
        }
        spec.environment
            .insert("SHEPHERD_AGENT".into(), launched_agent.as_str().to_owned());
        if !adapter_wraps_launch {
            #[cfg(unix)]
            {
                let program = std::mem::replace(&mut spec.program, "/bin/sh".into());
                let mut command = vec!["-c".into(), r#"exec "$@""#.into(), "agent-wrapper".into()];
                if launched_agent != Agent::Freebuff {
                    command[1] = r#"exec 3<&0 4>&1; if [ -t 2 ]; then exec </dev/tty >/dev/tty; else exec >&2; fi; exec "$@""#.into();
                }
                command.extend(wrapper);
                if launched_agent != Agent::Freebuff {
                    command.extend([
                        "/bin/sh".into(),
                        "-c".into(),
                        r#"exec <&3 >&4 3<&- 4>&-; exec "$@""#.into(),
                        "agent-protocol".into(),
                    ]);
                }
                command.push(program);
                command.extend(spec.args);
                spec.args = command;
            }
            #[cfg(not(unix))]
            {
                let mut wrapper = wrapper.into_iter();
                if let Some(program) = wrapper.next() {
                    let original = std::mem::replace(&mut spec.program, program);
                    spec.args = wrapper
                        .chain(std::iter::once(original))
                        .chain(spec.args)
                        .collect();
                }
            }
        }
        let capabilities = match session.start(spec).await {
            Ok(capabilities) => capabilities,
            Err(error) => return Err(self.fail_session_start(task_id, session, error)),
        };
        let opened_session = match resume {
            Some(_)
                if (launched_agent != task.agent || task.tier != session.tier())
                    && self.session_records.contains_key(task_id) =>
            {
                let reason = if launched_agent != task.agent {
                    format!(
                        "the agent changed from {} to {}",
                        task.agent.as_str(),
                        launched_agent.as_str()
                    )
                } else {
                    format!(
                        "the adapter changed from {} to {}",
                        task.tier.as_str(),
                        session.tier().as_str()
                    )
                };
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
            launched_agent.as_str(),
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
                HarnessEvent::QuestionRequested { .. } => {
                    next_state = Some(TaskState::Waiting);
                }
                HarnessEvent::ApprovalResolved { .. } | HarnessEvent::ApprovalWithdrawn { .. } => {
                    if next_state.unwrap_or(initial_state) == TaskState::Waiting
                        && self
                            .sessions
                            .get(task_id)
                            .and_then(HarnessSession::pending_approval)
                            .is_none()
                        && !self
                            .sessions
                            .get(task_id)
                            .is_some_and(HarnessSession::has_pending_questions)
                    {
                        next_state = Some(TaskState::Working);
                    }
                }
                HarnessEvent::QuestionAnswered { .. } => {
                    if next_state.unwrap_or(initial_state) == TaskState::Waiting
                        && self.sessions.get(task_id).is_some_and(|session| {
                            session.pending_approval().is_none() && !session.has_pending_questions()
                        })
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

        if events
            .iter()
            .any(|event| matches!(event, HarnessEvent::ModelsChanged { .. }))
        {
            self.emit(Event::TaskWorkspaceChanged(self.task_workspace(task_id)?));
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
                    .persist_transcript(
                        &*self.store,
                        record,
                        settled,
                        true,
                        was_live.then_some(&task),
                    )
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

    #[doc(hidden)]
    pub fn task(&self, task_id: &TaskId) -> Result<&Task, CoreError> {
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

    pub fn project_default_model(
        &self,
        project_id: &ProjectId,
        agent: Agent,
    ) -> Result<Option<String>, CoreError> {
        let path = self.project_path(project_id)?;
        self.harness_factory.default_model_for_project(agent, &path)
    }

    pub fn task_project_path(&self, task_id: &TaskId) -> Result<PathBuf, CoreError> {
        self.project_path(&self.task(task_id)?.project_id)
    }

    pub fn project_preview_url(
        &self,
        project_id: &ProjectId,
    ) -> Result<Option<url::Url>, CoreError> {
        let project = self.project(project_id)?;
        Ok(self
            .project_preview_overrides
            .get(project_id)
            .cloned()
            .or_else(|| project.preview_url.clone()))
    }

    pub fn set_project_preview_overrides(
        &mut self,
        overrides: BTreeMap<ProjectId, url::Url>,
    ) -> Vec<ProjectId> {
        let changed = self
            .project_order
            .iter()
            .filter(|id| self.project_preview_overrides.get(*id) != overrides.get(*id))
            .cloned()
            .collect::<Vec<_>>();
        self.project_preview_overrides = overrides;
        if !changed.is_empty() {
            self.emit(Event::ProjectListChanged(self.project_list()));
        }
        changed
    }

    #[doc(hidden)]
    pub fn project(&self, project_id: &ProjectId) -> Result<&Project, CoreError> {
        self.projects
            .get(project_id)
            .ok_or_else(|| CoreError::ProjectNotFound {
                id: project_id.as_str().to_string(),
            })
    }

    /// A conversation keeps its recorded model. When the adapter no longer
    /// offers it, continue on the adapter's current model and say so once.
    fn fallback_model(&mut self, task_id: &TaskId, input: &mut PromptInput) {
        let Some(requested) = input.model.clone() else {
            return;
        };
        let Some(models) = self.sessions.get(task_id).and_then(HarnessSession::models) else {
            return;
        };
        let Some(replacement) = Self::model_fallback(&requested, &models) else {
            return;
        };
        input.model = replacement.clone();
        let used = replacement
            .clone()
            .unwrap_or_else(|| "the adapter's default model".to_string());
        if let Some(task) = self.tasks.get_mut(task_id) {
            task.model = replacement;
        }
        if let Some(session) = self.sessions.get_mut(task_id) {
            session.note(
                format!("Model {requested} is no longer available; continuing with {used}."),
                false,
            );
        }
    }

    /// The replacement for a requested model the adapter does not list, or
    /// `None` when the request can stand.
    #[doc(hidden)]
    pub fn model_fallback(
        requested: &str,
        models: &shep_harness_core::AgentModels,
    ) -> Option<Option<String>> {
        if models.options.is_empty()
            || models.options.iter().any(|option| option.id == requested)
            || models.current.as_deref() == Some(requested)
        {
            return None;
        }
        Some(models.current.clone())
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
                .persist_transcript(
                    &*self.store,
                    record,
                    !task.state.is_live(),
                    false,
                    Some(&task),
                )
                .await?;
        } else {
            self.store.save_task(&task).await?;
        }
        Ok(())
    }

    async fn persist_task_transcript(
        &mut self,
        task_id: &TaskId,
        flush: bool,
    ) -> Result<bool, CoreError> {
        let settled = !self.task(task_id)?.state.is_live();
        let (Some(session), Some(record)) = (
            self.sessions.get_mut(task_id),
            self.session_records.get(task_id),
        ) else {
            return Ok(false);
        };
        session
            .persist_transcript(&*self.store, record, settled, flush, None)
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

    #[doc(hidden)]
    pub fn project_snapshot(&self, project: &Project) -> ProjectSnapshot {
        let mut unfinished_task_count = 0;
        let mut review_task_count = 0;
        let mut running_task_count = 0;
        let mut waiting_task_count = 0;
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
            waiting_task_count += usize::from(task.state == TaskState::Waiting);
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
            preview_url: self
                .project_preview_overrides
                .get(&project.id)
                .or(project.preview_url.as_ref())
                .map(ToString::to_string),
            unfinished_task_count,
            review_task_count,
            running_task_count,
            waiting_task_count,
        }
    }

    fn set_shutdown(&mut self, state: ShutdownState) {
        self.shutdown = state;
        self.emit(Event::ShutdownStateChanged(state));
    }

    #[doc(hidden)]
    pub fn emit(&self, event: Event) {
        match self.events.try_send(event) {
            Ok(()) => {}
            // Dropping the event is safe only because the next drain asks the
            // shell to reload its snapshots.
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.events_saturated.store(true, Ordering::Relaxed);
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                tracing::debug!("core event receiver closed");
            }
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

#[doc(hidden)]
pub async fn pause_worktree_deletion_fixture(
    phase: &str,
    task_id: &TaskId,
) -> Result<(), CoreError> {
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
mod terminal_layout_tests {
    use super::*;
    use crate::terminal::{SplitAxis, TerminalSplit};

    #[cfg(feature = "git")]
    #[tokio::test]
    async fn worktree_setup_requires_approval_and_retries_before_the_agent()
    -> Result<(), Box<dyn std::error::Error>> {
        let repository = crate::test_support::TestRepository::new()?;
        let mut core = Core::new();
        core.execute(Command::AddProject {
            path: repository.path().to_path_buf(),
        })
        .await?;
        let project = core
            .project_order
            .last()
            .cloned()
            .ok_or("missing project")?;
        core.create_draft(&project, "setup test".into(), Agent::Codex)
            .await?;
        let task_id = core.tasks.keys().next().cloned().ok_or("missing task")?;
        let project_path = core.task_project_path(&task_id)?;
        let script = "if [ ! -f first-attempt ]; then touch first-attempt; exit 1; fi; printf x >> setup-count".to_string();
        core.set_project_setup(
            project_path.clone(),
            Some(ProjectSetup {
                script: script.clone(),
                approved: false,
                environment: None,
                environment_approved: false,
            }),
        );
        assert!(matches!(
            core.send_prompt(&task_id, PromptInput::text("hello")).await,
            Err(CoreError::ProjectSetup(_))
        ));
        assert!(core.task(&task_id)?.worktree_path.is_some());
        core.set_project_setup(
            project_path,
            Some(ProjectSetup {
                script,
                approved: true,
                environment: None,
                environment_approved: false,
            }),
        );
        assert!(matches!(
            core.send_prompt(&task_id, PromptInput::text("hello")).await,
            Err(CoreError::DevelopmentEnvironment { .. })
        ));
        let worktree = core
            .task(&task_id)?
            .worktree_path
            .clone()
            .ok_or("missing worktree")?;
        assert!(worktree.join("first-attempt").exists());
        assert!(
            core.send_prompt(&task_id, PromptInput::text("hello"))
                .await
                .is_err()
        );
        assert_eq!(std::fs::read_to_string(worktree.join("setup-count"))?, "x");
        assert!(
            core.send_prompt(&task_id, PromptInput::text("hello"))
                .await
                .is_err()
        );
        assert_eq!(std::fs::read_to_string(worktree.join("setup-count"))?, "x");
        Ok(())
    }

    #[tokio::test]
    async fn terminal_exit_publishes_list_and_layout_together()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut core = Core::new();
        let directory = crate::test_support::TestDirectory::new()?;
        core.execute(Command::AddProject {
            path: directory.path().to_path_buf(),
        })
        .await?;
        let project_id = core
            .project_order
            .last()
            .cloned()
            .ok_or("missing project")?;
        for name in ["first", "second"] {
            core.record_terminal(&TerminalRecord::project_shared(
                TerminalId::new(name),
                project_id.clone(),
                name,
                directory.path().to_path_buf(),
                "/bin/sh",
                1,
            ))
            .await?;
        }
        let pane = |name: &str| {
            Box::new(TerminalSplit::Pane {
                terminal_id: name.into(),
            })
        };
        let initial = core
            .save_terminal_layout(
                &project_id,
                TerminalLayout {
                    root: Some(TerminalSplit::Split {
                        axis: SplitAxis::Horizontal,
                        ratio: 500,
                        first: pane("first"),
                        second: pane("second"),
                    }),
                    ..TerminalLayout::default()
                },
            )
            .await?;
        drop(core.drain_events());
        core.end_terminal(&TerminalId::new("second"), None).await?;
        let snapshot = core
            .drain_events()
            .into_iter()
            .find_map(|event| match event {
                Event::TerminalListChanged(snapshot) => Some(snapshot),
                _ => None,
            })
            .ok_or("missing terminal update")?;
        assert_eq!(snapshot.terminals.len(), 1);
        assert_eq!(
            snapshot
                .terminals
                .first()
                .map(|terminal| terminal.id.as_str()),
            Some("first")
        );
        assert_eq!(snapshot.layout.revision, initial.revision + 1);
        assert_eq!(snapshot.layout.root, Some(*pane("first")));
        assert!(matches!(
            core.save_terminal_layout(&project_id, initial).await,
            Err(CoreError::TerminalLayoutChanged)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn closing_an_ended_terminal_removes_its_record_and_pane()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut core = Core::new();
        let directory = crate::test_support::TestDirectory::new()?;
        core.execute(Command::AddProject {
            path: directory.path().to_path_buf(),
        })
        .await?;
        let project = core
            .project_order
            .last()
            .cloned()
            .ok_or("missing project")?;
        let id = TerminalId::new("ended");
        let mut terminal = TerminalRecord::project_shared(
            id.clone(),
            project.clone(),
            "Terminal",
            directory.path().to_path_buf(),
            "/bin/sh",
            1,
        );
        core.record_terminal(&terminal).await?;
        assert!(matches!(
            core.remove_ended_terminal(&id).await,
            Err(CoreError::TerminalStillRunning { .. })
        ));
        terminal.ended_at = Some(2);
        core.record_terminal(&terminal).await?;
        core.save_terminal_layout(
            &project,
            TerminalLayout {
                root: Some(TerminalSplit::Pane {
                    terminal_id: id.as_str().to_string(),
                }),
                ..TerminalLayout::default()
            },
        )
        .await?;
        core.remove_ended_terminal(&id).await?;
        let snapshot = core.terminal_list(&project);
        assert!(snapshot.terminals.is_empty());
        assert!(snapshot.layout.root.is_none());
        Ok(())
    }
}
