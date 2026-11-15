//! Persistence boundary: the [`Store`] trait, its error type, the value types it
//! exchanges, and the in-memory implementation.
//!
//! The durable Toasty/Turso implementation lives in the `shep-store` crate.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use shep_harness_core::{Entry, EntryKind, PromptInput};
use tokio::sync::{Mutex, RwLock};

pub use crate::worktree_pool::{WorktreePoolId, WorktreePoolRecord, WorktreePoolState};
use crate::{
    pool_reset::PoolResetPolicy,
    projects::{Project, ProjectId, now_millis},
    search::{SearchHit, SearchQuery},
    session::SessionRecord,
    snapshots::WorkspaceLayout,
    tasks::{Agent, Task, TaskId, TaskState},
    terminal::{ProcessRecord, TerminalLayout, TerminalRecord, TerminalScope},
};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("store backend error: {0}")]
    Backend(String),
    #[error("database error: {0}")]
    Database(String),
    #[error("migration checksum mismatch for {name}: expected {expected}, got {actual}")]
    MigrationChecksum {
        name: String,
        expected: String,
        actual: String,
    },
    #[error(
        "unknown database migration {name}; this database may require a newer Shepherd version"
    )]
    UnknownMigration { name: String },
    #[error("database migration history is incomplete: {missing} is missing before {applied}")]
    MigrationHistoryGap { missing: String, applied: String },
    #[error("invalid migration {name}: {reason}")]
    InvalidMigration { name: String, reason: String },
    #[error("migration {name} failed at statement {statement}: {reason}")]
    MigrationFailed {
        name: String,
        statement: usize,
        reason: String,
    },
    #[error("could not back up the database to {path}: {reason}")]
    Backup { path: String, reason: String },
    #[error("invalid database row: {0}")]
    InvalidRow(String),
    #[error("task {task_id} is no longer awaiting review in the database")]
    ReviewStateChanged { task_id: TaskId },
    #[error("session {session_id} does not belong to task {task_id} in the store")]
    TranscriptOwnerMissing { task_id: TaskId, session_id: String },
    #[error("session {session_id} cannot be saved for task {task_id}")]
    SessionOwnerMissing { task_id: TaskId, session_id: String },
    #[error("terminal {terminal_id} cannot be saved for its project and task")]
    TerminalOwnerMissing { terminal_id: String },
    #[error("terminal layout cannot be saved for missing project {project_id}")]
    TerminalLayoutOwnerMissing { project_id: ProjectId },
    #[error("approval cannot be saved for missing project {project_id}")]
    ApprovalOwnerMissing { project_id: ProjectId },
    #[error("quota ledger entry cannot be saved for missing pool slot {slot_id}")]
    QuotaLedgerOwnerMissing { slot_id: PoolSlotId },
    #[error("pool slot {slot_id} was not found")]
    PoolSlotNotFound { slot_id: PoolSlotId },
    #[error("worktree pool record cannot be saved for missing project {project_id}")]
    WorktreePoolOwnerMissing { project_id: ProjectId },
    #[error("workspace layout cannot be saved for missing task {task_id}")]
    WorkspaceOwnerMissing { task_id: TaskId },
    #[error("prompt queue cannot be saved for missing task {task_id}")]
    PromptQueueOwnerMissing { task_id: TaskId },
    #[error("task {task_id} cannot be saved for project {project_id}")]
    TaskOwnerMissing {
        task_id: TaskId,
        project_id: ProjectId,
    },
}

impl StoreError {
    /// The durable store converts its driver errors into this variant.
    pub fn database(error: impl std::fmt::Display) -> Self {
        StoreError::Database(error.to_string())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectApproval {
    pub id: String,
    pub agent: Agent,
    pub title: String,
    pub arguments: serde_json::Value,
    pub paths: Vec<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PoolSlotId(String);

impl PoolSlotId {
    /// A slot id read back from storage or written by a test.
    pub fn from_stored(id: String) -> Self {
        Self(id)
    }

    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PoolSlotId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// One provider/model slot in the local agent pool.
///
/// `credential_ref` is an opaque pointer into the agent's own credential store;
/// the secret itself never belongs in this record, its serialization or logs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolSlot {
    pub id: PoolSlotId,
    pub agent: Agent,
    pub provider_id: String,
    pub provider_name: String,
    pub model_id: String,
    pub model_name: String,
    pub credential_ref: Option<String>,
    pub priority: i64,
    pub enabled: bool,
    /// The last locally observed usage for the current reset window; the
    /// observation history belongs to the quota ledger.
    pub used: Option<u64>,
    pub limit: Option<u64>,
    pub limit_unit: Option<String>,
    pub reset_at: Option<u64>,
    pub cooldown_until: Option<u64>,
    pub last_used_at: Option<u64>,
    pub last_error: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
}

impl PoolSlot {
    pub fn new(
        agent: Agent,
        provider_id: impl Into<String>,
        provider_name: impl Into<String>,
        model_id: impl Into<String>,
        model_name: impl Into<String>,
    ) -> Self {
        let now = now_millis();
        Self {
            id: PoolSlotId::new(uuid::Uuid::new_v4().to_string()),
            agent,
            provider_id: provider_id.into(),
            provider_name: provider_name.into(),
            model_id: model_id.into(),
            model_name: model_name.into(),
            credential_ref: None,
            priority: 0,
            enabled: true,
            used: None,
            limit: None,
            limit_unit: None,
            reset_at: None,
            cooldown_until: None,
            last_used_at: None,
            last_error: None,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn availability(&self, latest: Option<&PoolQuotaEntry>, now: u64) -> PoolAvailability {
        if !self.enabled {
            return PoolAvailability::new(PoolAvailabilityState::Disabled, "slot is disabled");
        }
        match latest {
            Some(entry) => pool_entry_availability(entry, now),
            None => PoolAvailability::new(PoolAvailabilityState::Unknown, "no quota observation"),
        }
    }
}

pub fn latest_pool_quota_entry_for_window<'a>(
    entries: &'a [PoolQuotaEntry],
    window_key: Option<&str>,
) -> Option<&'a PoolQuotaEntry> {
    entries
        .iter()
        .filter(|entry| entry.window_key.as_deref() == window_key)
        .max_by(|left, right| pool_quota_entry_order(left, right))
}

pub fn pool_entry_availability(entry: &PoolQuotaEntry, now: u64) -> PoolAvailability {
    if let Some(until) = entry.cooldown_until.filter(|until| *until > now) {
        return PoolAvailability::new(
            PoolAvailabilityState::CoolingDown,
            format!("cooling down until {until}"),
        );
    }
    if let Some(error) = entry
        .error
        .as_deref()
        .filter(|error| !error.trim().is_empty())
    {
        return PoolAvailability::new(
            PoolAvailabilityState::Unhealthy,
            format!("last observation failed: {error}"),
        );
    }
    if entry.reset_at.is_some_and(|reset_at| reset_at <= now) {
        return match entry.limit {
            Some(0) => {
                PoolAvailability::new(PoolAvailabilityState::Exhausted, "quota limit is zero")
            }
            Some(_) => {
                PoolAvailability::new(PoolAvailabilityState::Available, "quota window reset")
            }
            None => PoolAvailability::new(
                PoolAvailabilityState::Unknown,
                "quota window reset without a known limit",
            ),
        };
    }
    let unit = entry.limit_unit.as_deref().unwrap_or("units");
    match (entry.used, entry.limit) {
        (Some(used), Some(limit)) if used >= limit => PoolAvailability::new(
            PoolAvailabilityState::Exhausted,
            format!("quota exhausted: used {used} of {limit} {unit}"),
        ),
        (Some(used), Some(limit)) => PoolAvailability::new(
            PoolAvailabilityState::Available,
            format!("quota available: used {used} of {limit} {unit}"),
        ),
        _ => PoolAvailability::new(PoolAvailabilityState::Unknown, "usage or limit is unknown"),
    }
}

/// A slot is available only when every provider window has headroom; a slot
/// with no published windows keeps the single-limit rule.
pub fn pool_slot_availability(
    slot: &PoolSlot,
    entries: &[PoolQuotaEntry],
    now: u64,
) -> PoolAvailability {
    if !slot.enabled {
        return PoolAvailability::new(PoolAvailabilityState::Disabled, "slot is disabled");
    }
    let policy = PoolResetPolicy::for_agent(slot.agent);
    if policy.windows().is_empty() {
        let Some(entry) = latest_pool_quota_entry_for_window(entries, None) else {
            return PoolAvailability::new(PoolAvailabilityState::Unknown, "no quota observation");
        };
        return pool_entry_availability(entry, now);
    }
    let single_limit = latest_pool_quota_entry_for_window(entries, None);
    let mut available = Vec::new();
    for window in policy.windows() {
        let Some(entry) =
            latest_pool_quota_entry_for_window(entries, Some(window.key)).or(single_limit)
        else {
            return PoolAvailability::new(
                PoolAvailabilityState::Unknown,
                format!("{} window: no quota observation", window.label),
            );
        };
        let availability = pool_entry_availability(entry, now);
        if availability.state != PoolAvailabilityState::Available {
            return PoolAvailability::new(
                availability.state,
                format!("{} window: {}", window.label, availability.reason),
            );
        }
        available.push(format!("{} window: {}", window.label, availability.reason));
    }
    PoolAvailability::new(PoolAvailabilityState::Available, available.join("; "))
}

pub fn pool_reset_window_statuses(
    slot: &PoolSlot,
    entries: &[PoolQuotaEntry],
) -> Vec<PoolResetWindowStatus> {
    let single_limit = latest_pool_quota_entry_for_window(entries, None);
    PoolResetPolicy::for_agent(slot.agent)
        .windows()
        .iter()
        .map(|window| {
            let entry =
                latest_pool_quota_entry_for_window(entries, Some(window.key)).or(single_limit);
            PoolResetWindowStatus {
                key: window.key.to_string(),
                label: window.label.to_string(),
                reset_at: entry.and_then(|entry| entry.reset_at).or_else(|| {
                    slot.last_used_at
                        .and_then(|last_used_at| window.reset_at(last_used_at))
                }),
                used: entry.and_then(|entry| entry.used),
                limit: entry.and_then(|entry| entry.limit),
                limit_unit: entry.and_then(|entry| entry.limit_unit.clone()),
                remaining: entry.and_then(|entry| match (entry.used, entry.limit) {
                    (Some(used), Some(limit)) => Some(limit.saturating_sub(used)),
                    _ => None,
                }),
            }
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PoolQuotaSource {
    Catalog,
    Provider,
    User,
}

impl PoolQuotaSource {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "catalog" => Some(Self::Catalog),
            "provider" => Some(Self::Provider),
            "user" => Some(Self::User),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Catalog => "catalog",
            Self::Provider => "provider",
            Self::User => "user",
        }
    }
}

/// One append-only quota observation for a pool slot.
///
/// The ledger carries evidence, not credentials: `source` and `observed_at`
/// explain where the values came from and when they were seen.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolQuotaEntry {
    pub id: String,
    pub slot_id: PoolSlotId,
    pub observed_at: u64,
    pub source: PoolQuotaSource,
    pub used: Option<u64>,
    pub limit: Option<u64>,
    pub limit_unit: Option<String>,
    pub reset_at: Option<u64>,
    pub cooldown_until: Option<u64>,
    pub error: Option<String>,
    /// The provider rate-limit window this observation describes. `None` is
    /// the single-limit evidence used by providers without published windows.
    #[serde(default)]
    pub window_key: Option<String>,
}

impl PoolQuotaEntry {
    pub fn new(slot_id: PoolSlotId, source: PoolQuotaSource, observed_at: u64) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            slot_id,
            observed_at,
            source,
            used: None,
            limit: None,
            limit_unit: None,
            reset_at: None,
            cooldown_until: None,
            error: None,
            window_key: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PoolAvailabilityState {
    Available,
    Exhausted,
    CoolingDown,
    Disabled,
    Unhealthy,
    Unknown,
}

impl PoolAvailabilityState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Exhausted => "exhausted",
            Self::CoolingDown => "cooling-down",
            Self::Disabled => "disabled",
            Self::Unhealthy => "unhealthy",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolAvailability {
    pub state: PoolAvailabilityState,
    pub reason: String,
}

impl PoolAvailability {
    pub fn new(state: PoolAvailabilityState, reason: impl Into<String>) -> Self {
        Self {
            state,
            reason: reason.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolSelectionRequest {
    pub request_id: String,
    pub override_slot: Option<PoolSlotId>,
    /// A turn can only use a slot that launches the task's own agent.
    #[serde(default)]
    pub agent: Option<crate::tasks::Agent>,
    pub now: u64,
}

impl PoolSelectionRequest {
    pub fn new(request_id: impl Into<String>, now: u64) -> Self {
        Self {
            request_id: request_id.into(),
            override_slot: None,
            agent: None,
            now,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolSkippedSlot {
    pub slot_id: PoolSlotId,
    pub state: Option<PoolAvailabilityState>,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolSelectionOutcome {
    pub request_id: String,
    pub slot: Option<PoolSlot>,
    pub selected_at: u64,
    pub reason: String,
    pub skipped: Vec<PoolSkippedSlot>,
    pub reused: bool,
}

pub const DEFAULT_POOL_IDLE_TIMEOUT_MS: u64 = 15 * 60 * 1_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolSelectionRecord {
    pub request_id: String,
    pub slot_id: Option<PoolSlotId>,
    pub selected_at: u64,
    pub released_at: Option<u64>,
    pub idle_since: Option<u64>,
    pub reason: String,
    pub skipped: Vec<PoolSkippedSlot>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolIdleReleaseRequest {
    pub now: u64,
    pub idle_timeout_ms: u64,
    pub active_requests: BTreeSet<String>,
}

impl PoolIdleReleaseRequest {
    pub fn new(now: u64, idle_timeout_ms: u64) -> Self {
        Self {
            now,
            idle_timeout_ms,
            active_requests: BTreeSet::new(),
        }
    }

    pub fn default_at(now: u64) -> Self {
        Self::new(now, DEFAULT_POOL_IDLE_TIMEOUT_MS)
    }

    pub fn with_active_requests(
        mut self,
        active_requests: impl IntoIterator<Item = String>,
    ) -> Self {
        self.active_requests = active_requests.into_iter().collect();
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolIdleReleased {
    pub request_id: String,
    pub slot_id: PoolSlotId,
    pub released_at: u64,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolIdleKept {
    pub request_id: String,
    pub slot_id: PoolSlotId,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolIdleReleaseOutcome {
    pub released: Vec<PoolIdleReleased>,
    pub kept: Vec<PoolIdleKept>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolResetWindowStatus {
    pub key: String,
    pub label: String,
    pub reset_at: Option<u64>,
    #[serde(default)]
    pub used: Option<u64>,
    #[serde(default)]
    pub limit: Option<u64>,
    #[serde(default)]
    pub limit_unit: Option<String>,
    #[serde(default)]
    pub remaining: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolSlotStatus {
    pub slot: PoolSlot,
    pub availability: PoolAvailability,
    pub latest_quota: Option<PoolQuotaEntry>,
    pub current_request: Option<String>,
    /// The provider's reset windows, derived from the slot's last local use.
    #[serde(default)]
    pub reset_windows: Vec<PoolResetWindowStatus>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolStatus {
    pub generated_at: u64,
    pub slots: Vec<PoolSlotStatus>,
    /// The slot forced for the next turn, which is session state rather than a
    /// stored pool field, so the stores always report `None` here.
    #[serde(default)]
    pub forced_slot: Option<PoolSlotId>,
}

/// The catalog's published agent-pool advisory contract.
pub const POOL_CATALOG_CONTRACT_VERSION: u32 = 2;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolCatalogAdvisory {
    pub contract_version: u32,
    pub snapshot_id: String,
    pub schema_version: u32,
    pub generated_at: String,
    pub stale: bool,
    pub records: Vec<PoolCatalogRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolCatalogRecord {
    pub provider_id: String,
    pub model_id: Option<String>,
    pub plan_id: Option<String>,
    pub offer_id: String,
    pub allowance: Option<u64>,
    pub allowance_unit: Option<String>,
    pub reset_period: Option<String>,
    pub usage_model: String,
    pub capacity: String,
    pub snapshot_id: String,
    pub schema_version: u32,
    pub stale: bool,
    pub source_url: String,
    pub conditions: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolCatalogSeed {
    pub slot_id: PoolSlotId,
    pub applied: bool,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolCatalogOutcome {
    pub snapshot_id: String,
    pub seeds: Vec<PoolCatalogSeed>,
    pub applied: usize,
    pub skipped: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolReorderRequest {
    pub slot_ids: Vec<PoolSlotId>,
    pub now: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolReorderOutcome {
    pub slots: Vec<PoolSlot>,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolSlotEnabledRequest {
    pub slot_id: PoolSlotId,
    pub enabled: bool,
    pub now: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolSlotEnabledOutcome {
    pub slot: PoolSlot,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolQuotaCorrectionRequest {
    pub slot_id: PoolSlotId,
    pub limit: Option<u64>,
    pub limit_unit: Option<String>,
    pub reset_at: Option<u64>,
    pub now: u64,
    /// The provider window the correction applies to; `None` corrects the
    /// slot's single limit.
    #[serde(default)]
    pub window_key: Option<String>,
}

impl PoolQuotaCorrectionRequest {
    pub fn new(slot_id: PoolSlotId, now: u64) -> Self {
        Self {
            slot_id,
            limit: None,
            limit_unit: None,
            reset_at: None,
            now,
            window_key: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolCooldownClearRequest {
    pub slot_id: PoolSlotId,
    pub now: u64,
}

impl PoolCooldownClearRequest {
    pub fn new(slot_id: PoolSlotId, now: u64) -> Self {
        Self { slot_id, now }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolQuotaCorrectionOutcome {
    pub slot: PoolSlot,
    pub latest_quota: Option<PoolQuotaEntry>,
    pub availability: PoolAvailability,
    pub applied: bool,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeDeletion {
    pub task_id: TaskId,
    pub repository_root: PathBuf,
    pub original_path: PathBuf,
    pub staged_path: PathBuf,
    pub worktree_git_dir: PathBuf,
    pub identity: String,
}

/// A short stable fingerprint for stored values and migration text.
pub fn checksum(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let bytes = hasher.finalize();
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
}

#[allow(
    clippy::double_must_use,
    reason = "async_trait generates must-use futures"
)]
#[async_trait]
pub trait Store: Send + Sync {
    async fn save_worktree_deletion(&self, deletion: &WorktreeDeletion) -> Result<(), StoreError>;
    async fn load_worktree_deletions(&self) -> Result<Vec<WorktreeDeletion>, StoreError>;
    async fn remove_worktree_deletion(&self, task_id: &TaskId) -> Result<(), StoreError>;
    async fn save_process(&self, process: &ProcessRecord) -> Result<(), StoreError>;
    async fn remove_process(&self, id: &str) -> Result<(), StoreError>;
    async fn load_processes(&self) -> Result<Vec<ProcessRecord>, StoreError>;
    async fn load_projects(&self) -> Result<Vec<Project>, StoreError>;
    async fn save_project(&self, project: &Project) -> Result<(), StoreError>;
    async fn save_draft(&self, project: &Project, task: &Task) -> Result<(), StoreError>;
    async fn load_tasks(&self) -> Result<Vec<Task>, StoreError>;
    async fn save_task(&self, task: &Task) -> Result<(), StoreError>;
    async fn save_task_with_transcript(
        &self,
        task: &Task,
        session: &SessionRecord,
        entries: &[&Entry],
    ) -> Result<(), StoreError>;
    async fn archive_tasks(&self, tasks: &[Task]) -> Result<(), StoreError>;
    /// Remove the task and its durable state with the last-task reference and any changed terminal layout.
    async fn delete_task(
        &self,
        task_id: &TaskId,
        layout: Option<(&ProjectId, &TerminalLayout)>,
    ) -> Result<(), StoreError>;
    async fn load_workspace_layouts(&self) -> Result<Vec<(TaskId, WorkspaceLayout)>, StoreError>;
    async fn save_workspace_layout(
        &self,
        task_id: &TaskId,
        layout: WorkspaceLayout,
    ) -> Result<(), StoreError>;
    async fn load_prompt_queues(&self) -> Result<Vec<(TaskId, Vec<PromptInput>)>, StoreError>;
    async fn save_prompt_queue(
        &self,
        task_id: &TaskId,
        prompts: &[PromptInput],
    ) -> Result<(), StoreError>;
    async fn load_terminals(&self) -> Result<Vec<TerminalRecord>, StoreError>;
    async fn save_terminal(&self, terminal: &TerminalRecord) -> Result<(), StoreError>;
    async fn load_terminal_layouts(&self) -> Result<Vec<(ProjectId, TerminalLayout)>, StoreError>;
    async fn save_terminal_layout(
        &self,
        project_id: &ProjectId,
        layout: &TerminalLayout,
    ) -> Result<(), StoreError>;
    async fn load_worktree_pool(&self) -> Result<Vec<WorktreePoolRecord>, StoreError>;
    async fn save_worktree_pool(&self, record: &WorktreePoolRecord) -> Result<(), StoreError>;
    async fn remove_worktree_pool(&self, id: &WorktreePoolId) -> Result<(), StoreError>;
    async fn load_sessions(&self) -> Result<Vec<SessionRecord>, StoreError>;
    async fn save_session(&self, session: &SessionRecord) -> Result<(), StoreError>;
    async fn load_transcript(&self, task_id: &TaskId) -> Result<Vec<Entry>, StoreError>;
    async fn search_entries(&self, _query: &SearchQuery) -> Result<Vec<SearchHit>, StoreError> {
        Err(StoreError::Backend(
            "Task search needs a durable Turso database.".into(),
        ))
    }
    async fn save_transcript_entries(
        &self,
        session: &SessionRecord,
        entries: &[&Entry],
    ) -> Result<(), StoreError>;
    async fn project_approvals(
        &self,
        project: &ProjectId,
    ) -> Result<Vec<ProjectApproval>, StoreError>;
    async fn has_project_approval(&self, project: &ProjectId, id: &str)
    -> Result<bool, StoreError>;
    async fn save_project_approval(
        &self,
        project: &ProjectId,
        grant: &ProjectApproval,
    ) -> Result<(), StoreError>;
    async fn reset_project_approval(&self, project: &ProjectId, id: &str)
    -> Result<(), StoreError>;
    async fn load_pool_slots(&self) -> Result<Vec<PoolSlot>, StoreError>;
    async fn save_pool_slot(&self, slot: &PoolSlot) -> Result<(), StoreError>;
    async fn remove_pool_slot(&self, slot_id: &PoolSlotId) -> Result<(), StoreError>;
    async fn append_pool_quota_entry(&self, entry: &PoolQuotaEntry) -> Result<(), StoreError>;
    async fn load_pool_quota_entries(
        &self,
        slot_id: &PoolSlotId,
    ) -> Result<Vec<PoolQuotaEntry>, StoreError>;
    async fn latest_pool_quota_entry(
        &self,
        slot_id: &PoolSlotId,
    ) -> Result<Option<PoolQuotaEntry>, StoreError>;
    async fn select_pool_slot(
        &self,
        request: &PoolSelectionRequest,
    ) -> Result<PoolSelectionOutcome, StoreError>;
    async fn release_pool_selection(
        &self,
        request_id: &str,
        released_at: u64,
    ) -> Result<(), StoreError>;
    async fn load_pool_selections(&self) -> Result<Vec<PoolSelectionRecord>, StoreError>;
    async fn mark_pool_selection_idle(
        &self,
        request_id: &str,
        idle_since: u64,
    ) -> Result<Option<PoolSelectionRecord>, StoreError>;
    async fn release_idle_pool_selections(
        &self,
        request: &PoolIdleReleaseRequest,
    ) -> Result<PoolIdleReleaseOutcome, StoreError>;
    async fn load_pool_status(&self, now: u64) -> Result<PoolStatus, StoreError>;
    async fn reorder_pool_slots(
        &self,
        request: &PoolReorderRequest,
    ) -> Result<PoolReorderOutcome, StoreError>;
    async fn set_pool_slot_enabled(
        &self,
        request: &PoolSlotEnabledRequest,
    ) -> Result<PoolSlotEnabledOutcome, StoreError>;
    async fn correct_pool_quota(
        &self,
        request: &PoolQuotaCorrectionRequest,
    ) -> Result<PoolQuotaCorrectionOutcome, StoreError>;
    async fn clear_pool_slot_cooldown(
        &self,
        request: &PoolCooldownClearRequest,
    ) -> Result<PoolQuotaCorrectionOutcome, StoreError>;
    async fn apply_pool_catalog_advisory(
        &self,
        advisory: &PoolCatalogAdvisory,
        now: u64,
    ) -> Result<PoolCatalogOutcome, StoreError>;
}

pub fn validate_draft_owner(project: &Project, task: &Task) -> Result<(), StoreError> {
    if task.project_id != project.id
        || project.last_task_id.as_ref() != Some(&task.id)
        || task.state != TaskState::Draft
    {
        return Err(StoreError::InvalidRow(
            "draft and selected project do not match".into(),
        ));
    }
    Ok(())
}

pub fn validate_attachments(entries: &[&Entry]) -> Result<(), StoreError> {
    for entry in entries {
        if matches!(entry.kind, EntryKind::UserMessage { .. }) {
            for attachment in &entry.attachments {
                if attachment.relative_path.to_str().is_none_or(str::is_empty)
                    || !attachment
                        .relative_path
                        .components()
                        .all(|part| matches!(part, std::path::Component::Normal(_)))
                {
                    return Err(StoreError::InvalidRow(
                        "expected a relative attachment path".into(),
                    ));
                }
                i64::try_from(attachment.bytes).map_err(|_| {
                    StoreError::InvalidRow("attachment size exceeds database range".into())
                })?;
            }
        }
    }
    Ok(())
}

pub fn validate_memory_task_owner(
    tasks: &BTreeMap<String, Task>,
    task: &Task,
) -> Result<(), StoreError> {
    if tasks
        .get(task.id.as_str())
        .is_some_and(|stored| stored.project_id != task.project_id)
    {
        return Err(StoreError::TaskOwnerMissing {
            task_id: task.id.clone(),
            project_id: task.project_id.clone(),
        });
    }
    Ok(())
}

/// A non-durable store used to test the boundary before a database is opened.
#[derive(Default)]
pub struct MemoryStore {
    worktree_deletions: RwLock<BTreeMap<TaskId, WorktreeDeletion>>,
    processes: RwLock<BTreeMap<String, ProcessRecord>>,
    projects: RwLock<BTreeMap<String, Project>>,
    tasks: RwLock<BTreeMap<String, Task>>,
    workspace_layouts: RwLock<BTreeMap<TaskId, WorkspaceLayout>>,
    prompt_queues: RwLock<BTreeMap<TaskId, Vec<PromptInput>>>,
    terminals: RwLock<BTreeMap<String, TerminalRecord>>,
    terminal_layouts: RwLock<BTreeMap<ProjectId, TerminalLayout>>,
    sessions: RwLock<Vec<SessionRecord>>,
    transcripts: RwLock<BTreeMap<TaskId, BTreeMap<u64, Entry>>>,
    project_approvals: RwLock<BTreeMap<ProjectId, BTreeMap<String, ProjectApproval>>>,
    pool_slots: RwLock<BTreeMap<PoolSlotId, PoolSlot>>,
    pool_quota_ledger: RwLock<Vec<PoolQuotaEntry>>,
    pool_selections: RwLock<BTreeMap<String, PoolSelectionRecord>>,
    pool_selection_lock: Mutex<()>,
    worktree_pool: RwLock<BTreeMap<WorktreePoolId, WorktreePoolRecord>>,
}

impl MemoryStore {
    /// The stored tasks, for tests that inspect or corrupt them directly.
    pub fn tasks(&self) -> &RwLock<BTreeMap<String, Task>> {
        &self.tasks
    }

    pub fn projects(&self) -> &RwLock<BTreeMap<String, Project>> {
        &self.projects
    }

    pub fn sessions(&self) -> &RwLock<Vec<SessionRecord>> {
        &self.sessions
    }

    pub fn transcripts(&self) -> &RwLock<BTreeMap<TaskId, BTreeMap<u64, Entry>>> {
        &self.transcripts
    }

    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl Store for MemoryStore {
    async fn save_worktree_deletion(&self, deletion: &WorktreeDeletion) -> Result<(), StoreError> {
        let mut deletions = self.worktree_deletions.write().await;
        if deletions.contains_key(&deletion.task_id) {
            return Err(StoreError::Backend(
                "worktree deletion is already recorded".into(),
            ));
        }
        deletions.insert(deletion.task_id.clone(), deletion.clone());
        Ok(())
    }

    async fn load_worktree_deletions(&self) -> Result<Vec<WorktreeDeletion>, StoreError> {
        Ok(self
            .worktree_deletions
            .read()
            .await
            .values()
            .cloned()
            .collect())
    }

    async fn remove_worktree_deletion(&self, task_id: &TaskId) -> Result<(), StoreError> {
        self.worktree_deletions.write().await.remove(task_id);
        Ok(())
    }

    async fn save_process(&self, process: &ProcessRecord) -> Result<(), StoreError> {
        self.processes
            .write()
            .await
            .insert(process.id.clone(), process.clone());
        Ok(())
    }

    async fn remove_process(&self, id: &str) -> Result<(), StoreError> {
        self.processes.write().await.remove(id);
        Ok(())
    }

    async fn load_processes(&self) -> Result<Vec<ProcessRecord>, StoreError> {
        Ok(self.processes.read().await.values().cloned().collect())
    }
    async fn load_transcript(&self, task_id: &TaskId) -> Result<Vec<Entry>, StoreError> {
        Ok(self
            .transcripts
            .read()
            .await
            .get(task_id)
            .into_iter()
            .flat_map(|entries| entries.values().cloned())
            .collect())
    }

    async fn save_transcript_entries(
        &self,
        session: &SessionRecord,
        entries: &[&Entry],
    ) -> Result<(), StoreError> {
        if entries.is_empty() {
            return Ok(());
        }
        validate_attachments(entries)?;
        let tasks = self.tasks.read().await;
        let sessions = self.sessions.read().await;
        let projects = self.projects.read().await;
        if !tasks
            .get(session.task_id.as_str())
            .is_some_and(|task| projects.contains_key(task.project_id.as_str()))
            || !sessions
                .iter()
                .any(|stored| stored.id == session.id && stored.task_id == session.task_id)
        {
            return Err(StoreError::TranscriptOwnerMissing {
                task_id: session.task_id.clone(),
                session_id: session.id.clone(),
            });
        }
        let mut transcripts = self.transcripts.write().await;
        let transcript = transcripts.entry(session.task_id.clone()).or_default();
        for entry in entries {
            transcript.insert(entry.sequence, (*entry).clone());
        }
        Ok(())
    }

    async fn project_approvals(
        &self,
        project: &ProjectId,
    ) -> Result<Vec<ProjectApproval>, StoreError> {
        let approvals = self.project_approvals.read().await;
        Ok(approvals
            .get(project)
            .into_iter()
            .flat_map(|grants| grants.values().cloned())
            .collect())
    }

    async fn has_project_approval(
        &self,
        project: &ProjectId,
        id: &str,
    ) -> Result<bool, StoreError> {
        Ok(self
            .project_approvals
            .read()
            .await
            .get(project)
            .is_some_and(|grants| grants.contains_key(id)))
    }

    async fn save_project_approval(
        &self,
        project: &ProjectId,
        grant: &ProjectApproval,
    ) -> Result<(), StoreError> {
        let projects = self.projects.read().await;
        if !projects.contains_key(project.as_str()) {
            return Err(StoreError::ApprovalOwnerMissing {
                project_id: project.clone(),
            });
        }
        self.project_approvals
            .write()
            .await
            .entry(project.clone())
            .or_default()
            .entry(grant.id.clone())
            .or_insert_with(|| grant.clone());
        Ok(())
    }

    async fn reset_project_approval(
        &self,
        project: &ProjectId,
        id: &str,
    ) -> Result<(), StoreError> {
        if let Some(grants) = self.project_approvals.write().await.get_mut(project) {
            grants.remove(id);
        }
        Ok(())
    }

    async fn load_projects(&self) -> Result<Vec<Project>, StoreError> {
        let projects = self.projects.read().await;
        Ok(projects.values().cloned().collect())
    }

    async fn save_project(&self, project: &Project) -> Result<(), StoreError> {
        let mut projects = self.projects.write().await;
        projects.insert(project.id.as_str().to_string(), project.clone());
        Ok(())
    }

    async fn save_draft(&self, project: &Project, task: &Task) -> Result<(), StoreError> {
        validate_draft_owner(project, task)?;
        let mut tasks = self.tasks.write().await;
        let mut projects = self.projects.write().await;
        validate_memory_task_owner(&tasks, task)?;
        projects.insert(project.id.as_str().to_string(), project.clone());
        tasks.insert(task.id.as_str().to_string(), task.clone());
        Ok(())
    }

    async fn load_tasks(&self) -> Result<Vec<Task>, StoreError> {
        let tasks = self.tasks.read().await;
        Ok(tasks.values().cloned().collect())
    }

    async fn save_task(&self, task: &Task) -> Result<(), StoreError> {
        let mut tasks = self.tasks.write().await;
        let projects = self.projects.read().await;
        if !projects.contains_key(task.project_id.as_str()) {
            return Err(StoreError::TaskOwnerMissing {
                task_id: task.id.clone(),
                project_id: task.project_id.clone(),
            });
        }
        validate_memory_task_owner(&tasks, task)?;
        tasks.insert(task.id.as_str().to_string(), task.clone());
        Ok(())
    }

    async fn save_task_with_transcript(
        &self,
        task: &Task,
        session: &SessionRecord,
        entries: &[&Entry],
    ) -> Result<(), StoreError> {
        if task.id != session.task_id {
            return Err(StoreError::TranscriptOwnerMissing {
                task_id: task.id.clone(),
                session_id: session.id.clone(),
            });
        }
        validate_attachments(entries)?;
        let mut tasks = self.tasks.write().await;
        let sessions = self.sessions.read().await;
        let projects = self.projects.read().await;
        validate_memory_task_owner(&tasks, task)?;
        if !tasks.contains_key(task.id.as_str())
            || !projects.contains_key(task.project_id.as_str())
            || !sessions
                .iter()
                .any(|stored| stored.id == session.id && stored.task_id == task.id)
        {
            return Err(StoreError::TranscriptOwnerMissing {
                task_id: task.id.clone(),
                session_id: session.id.clone(),
            });
        }
        let mut transcripts = self.transcripts.write().await;
        tasks.insert(task.id.as_str().to_string(), task.clone());
        if !entries.is_empty() {
            let transcript = transcripts.entry(task.id.clone()).or_default();
            for entry in entries {
                transcript.insert(entry.sequence, (*entry).clone());
            }
        }
        Ok(())
    }

    async fn archive_tasks(&self, archived: &[Task]) -> Result<(), StoreError> {
        let mut tasks = self.tasks.write().await;
        let mut seen = std::collections::BTreeSet::new();
        if let Some(task) = archived.iter().find(|task| {
            !seen.insert(task.id.clone())
                || tasks
                    .get(task.id.as_str())
                    .is_none_or(|stored| stored.state != TaskState::Review)
        }) {
            return Err(StoreError::ReviewStateChanged {
                task_id: task.id.clone(),
            });
        }
        for task in archived {
            if let Some(stored) = tasks.get_mut(task.id.as_str()) {
                stored.state = TaskState::Done;
                stored.approved_tip_sha = None;
                stored.updated_at = task.updated_at;
            }
        }
        let mut prompt_queues = self.prompt_queues.write().await;
        for task in archived {
            prompt_queues.remove(&task.id);
        }
        Ok(())
    }

    async fn delete_task(
        &self,
        task_id: &TaskId,
        layout: Option<(&ProjectId, &TerminalLayout)>,
    ) -> Result<(), StoreError> {
        let mut tasks = self.tasks.write().await;
        let mut sessions = self.sessions.write().await;
        let mut projects = self.projects.write().await;
        if let Some((project_id, _)) = layout
            && !projects.contains_key(project_id.as_str())
        {
            return Err(StoreError::TerminalLayoutOwnerMissing {
                project_id: project_id.clone(),
            });
        }
        let mut transcripts = self.transcripts.write().await;
        let mut workspace_layouts = self.workspace_layouts.write().await;
        let mut terminal_layouts = self.terminal_layouts.write().await;
        let mut terminals = self.terminals.write().await;
        if let Some((project_id, layout)) = layout {
            terminal_layouts.insert(project_id.clone(), layout.clone());
        }
        for project in projects.values_mut() {
            if project.last_task_id.as_ref() == Some(task_id) {
                project.last_task_id = None;
            }
        }
        tasks.remove(task_id.as_str());
        workspace_layouts.remove(task_id);
        self.prompt_queues.write().await.remove(task_id);
        transcripts.remove(task_id);
        sessions.retain(|session| &session.task_id != task_id);
        terminals.retain(|_, terminal| terminal.task_id.as_ref() != Some(task_id));
        Ok(())
    }

    async fn load_workspace_layouts(&self) -> Result<Vec<(TaskId, WorkspaceLayout)>, StoreError> {
        Ok(self
            .workspace_layouts
            .read()
            .await
            .iter()
            .map(|(task, layout)| (task.clone(), *layout))
            .collect())
    }

    async fn save_workspace_layout(
        &self,
        task_id: &TaskId,
        layout: WorkspaceLayout,
    ) -> Result<(), StoreError> {
        let tasks = self.tasks.read().await;
        if !tasks.contains_key(task_id.as_str()) {
            return Err(StoreError::WorkspaceOwnerMissing {
                task_id: task_id.clone(),
            });
        }
        self.workspace_layouts
            .write()
            .await
            .insert(task_id.clone(), layout);
        Ok(())
    }

    async fn load_prompt_queues(&self) -> Result<Vec<(TaskId, Vec<PromptInput>)>, StoreError> {
        Ok(self
            .prompt_queues
            .read()
            .await
            .iter()
            .map(|(task, prompts)| (task.clone(), prompts.clone()))
            .collect())
    }

    async fn save_prompt_queue(
        &self,
        task_id: &TaskId,
        prompts: &[PromptInput],
    ) -> Result<(), StoreError> {
        if prompts.is_empty() {
            self.prompt_queues.write().await.remove(task_id);
            return Ok(());
        }
        let tasks = self.tasks.read().await;
        if !tasks.contains_key(task_id.as_str()) {
            return Err(StoreError::PromptQueueOwnerMissing {
                task_id: task_id.clone(),
            });
        }
        self.prompt_queues
            .write()
            .await
            .insert(task_id.clone(), prompts.to_vec());
        Ok(())
    }

    async fn load_terminals(&self) -> Result<Vec<TerminalRecord>, StoreError> {
        let terminals = self.terminals.read().await;
        Ok(terminals.values().cloned().collect())
    }

    async fn save_terminal(&self, terminal: &TerminalRecord) -> Result<(), StoreError> {
        let tasks = self.tasks.read().await;
        let projects = self.projects.read().await;
        let mut terminals = self.terminals.write().await;
        let valid_owner = projects.contains_key(terminal.project_id.as_str())
            && match (terminal.scope, &terminal.task_id) {
                (TerminalScope::Project, None) | (TerminalScope::Freebuff, None) => true,
                (TerminalScope::Task, Some(id)) => tasks
                    .get(id.as_str())
                    .is_some_and(|task| task.project_id == terminal.project_id),
                _ => false,
            };
        let same_owner = terminals.get(terminal.id.as_str()).is_none_or(|existing| {
            existing.project_id == terminal.project_id
                && existing.task_id == terminal.task_id
                && existing.scope == terminal.scope
        });
        if !valid_owner || !same_owner {
            return Err(StoreError::TerminalOwnerMissing {
                terminal_id: terminal.id.as_str().to_string(),
            });
        }
        terminals.insert(terminal.id.as_str().to_string(), terminal.clone());
        Ok(())
    }

    async fn load_terminal_layouts(&self) -> Result<Vec<(ProjectId, TerminalLayout)>, StoreError> {
        Ok(self
            .terminal_layouts
            .read()
            .await
            .iter()
            .map(|(project, layout)| (project.clone(), layout.clone()))
            .collect())
    }

    async fn save_terminal_layout(
        &self,
        project_id: &ProjectId,
        layout: &TerminalLayout,
    ) -> Result<(), StoreError> {
        let projects = self.projects.read().await;
        if !projects.contains_key(project_id.as_str()) {
            return Err(StoreError::TerminalLayoutOwnerMissing {
                project_id: project_id.clone(),
            });
        }
        self.terminal_layouts
            .write()
            .await
            .insert(project_id.clone(), layout.clone());
        Ok(())
    }

    async fn load_worktree_pool(&self) -> Result<Vec<WorktreePoolRecord>, StoreError> {
        let mut records: Vec<_> = self.worktree_pool.read().await.values().cloned().collect();
        records.sort_by(|left, right| {
            left.project_id
                .as_str()
                .cmp(right.project_id.as_str())
                .then_with(|| left.path.cmp(&right.path))
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(records)
    }

    async fn save_worktree_pool(&self, record: &WorktreePoolRecord) -> Result<(), StoreError> {
        if record.path.as_os_str().is_empty() {
            return Err(StoreError::InvalidRow(
                "worktree pool path is empty".to_string(),
            ));
        }
        if !self
            .projects
            .read()
            .await
            .contains_key(record.project_id.as_str())
        {
            return Err(StoreError::WorktreePoolOwnerMissing {
                project_id: record.project_id.clone(),
            });
        }
        self.worktree_pool
            .write()
            .await
            .insert(record.id.clone(), record.clone());
        Ok(())
    }

    async fn remove_worktree_pool(&self, id: &WorktreePoolId) -> Result<(), StoreError> {
        self.worktree_pool.write().await.remove(id);
        Ok(())
    }

    async fn load_sessions(&self) -> Result<Vec<SessionRecord>, StoreError> {
        let sessions = self.sessions.read().await;
        let mut sessions = sessions.clone();
        // Stable, so sessions started in the same millisecond keep insertion order
        // and the last record stays the newest.
        sessions.sort_by_key(|session| session.started_at);
        Ok(sessions)
    }

    async fn save_session(&self, session: &SessionRecord) -> Result<(), StoreError> {
        let tasks = self.tasks.read().await;
        let mut sessions = self.sessions.write().await;
        if !tasks.contains_key(session.task_id.as_str())
            || sessions
                .iter()
                .any(|seen| seen.id == session.id && seen.task_id != session.task_id)
        {
            return Err(StoreError::SessionOwnerMissing {
                task_id: session.task_id.clone(),
                session_id: session.id.clone(),
            });
        }
        sessions.retain(|seen| seen.id != session.id);
        sessions.push(session.clone());
        Ok(())
    }

    async fn load_pool_slots(&self) -> Result<Vec<PoolSlot>, StoreError> {
        let mut slots: Vec<_> = self.pool_slots.read().await.values().cloned().collect();
        slots.sort_by(pool_slot_order);
        Ok(slots)
    }

    async fn save_pool_slot(&self, slot: &PoolSlot) -> Result<(), StoreError> {
        validate_pool_slot(slot)?;
        self.pool_slots
            .write()
            .await
            .insert(slot.id.clone(), slot.clone());
        Ok(())
    }

    async fn remove_pool_slot(&self, slot_id: &PoolSlotId) -> Result<(), StoreError> {
        let _guard = self.pool_selection_lock.lock().await;
        for record in self.pool_selections.write().await.values_mut() {
            if record
                .slot_id
                .as_ref()
                .is_some_and(|slot| slot.as_str() == slot_id.as_str())
            {
                let selected_at = record.selected_at;
                record.slot_id = None;
                record.released_at.get_or_insert(selected_at);
            }
        }
        self.pool_quota_ledger
            .write()
            .await
            .retain(|entry| entry.slot_id.as_str() != slot_id.as_str());
        self.pool_slots.write().await.remove(slot_id);
        Ok(())
    }

    async fn append_pool_quota_entry(&self, entry: &PoolQuotaEntry) -> Result<(), StoreError> {
        validate_pool_quota_entry(entry)?;
        if !self.pool_slots.read().await.contains_key(&entry.slot_id) {
            return Err(StoreError::QuotaLedgerOwnerMissing {
                slot_id: entry.slot_id.clone(),
            });
        }
        let mut ledger = self.pool_quota_ledger.write().await;
        if ledger.iter().any(|stored| stored.id == entry.id) {
            return Err(StoreError::Backend(
                "quota ledger entry is already recorded".into(),
            ));
        }
        ledger.push(entry.clone());
        Ok(())
    }

    async fn load_pool_quota_entries(
        &self,
        slot_id: &PoolSlotId,
    ) -> Result<Vec<PoolQuotaEntry>, StoreError> {
        let mut entries: Vec<_> = self
            .pool_quota_ledger
            .read()
            .await
            .iter()
            .filter(|entry| entry.slot_id.as_str() == slot_id.as_str())
            .cloned()
            .collect();
        entries.sort_by(pool_quota_entry_order);
        Ok(entries)
    }

    async fn latest_pool_quota_entry(
        &self,
        slot_id: &PoolSlotId,
    ) -> Result<Option<PoolQuotaEntry>, StoreError> {
        Ok(self
            .pool_quota_ledger
            .read()
            .await
            .iter()
            .filter(|entry| entry.slot_id.as_str() == slot_id.as_str())
            .max_by(|left, right| pool_quota_entry_order(left, right))
            .cloned())
    }

    async fn select_pool_slot(
        &self,
        request: &PoolSelectionRequest,
    ) -> Result<PoolSelectionOutcome, StoreError> {
        validate_pool_selection_request(request)?;
        let _guard = self.pool_selection_lock.lock().await;
        let existing = self
            .pool_selections
            .read()
            .await
            .get(&request.request_id)
            .cloned();
        if let Some(record) = existing
            && record.released_at.is_none()
        {
            let override_matches = request
                .override_slot
                .as_ref()
                .is_none_or(|slot| Some(slot) == record.slot_id.as_ref());
            if override_matches
                && let Some(slot_id) = record.slot_id.clone()
                && let Some(slot) = self.pool_slots.read().await.get(&slot_id).cloned()
            {
                let latest = self.latest_pool_quota_entry(&slot.id).await?;
                if slot.availability(latest.as_ref(), request.now).state
                    == PoolAvailabilityState::Available
                {
                    if let Some(stored) = self
                        .pool_selections
                        .write()
                        .await
                        .get_mut(&request.request_id)
                        && stored.released_at.is_none()
                    {
                        stored.idle_since = None;
                    }
                    return Ok(PoolSelectionOutcome {
                        request_id: request.request_id.clone(),
                        slot: Some(slot),
                        selected_at: record.selected_at,
                        reason: "reused reservation".into(),
                        skipped: record.skipped,
                        reused: true,
                    });
                }
            }
            if let Some(stored) = self
                .pool_selections
                .write()
                .await
                .get_mut(&request.request_id)
                && stored.released_at.is_none()
            {
                stored.released_at = Some(request.now);
            }
        }

        let slots = self.load_pool_slots().await?;
        let mut entries: BTreeMap<PoolSlotId, Vec<PoolQuotaEntry>> = BTreeMap::new();
        {
            let ledger = self.pool_quota_ledger.read().await;
            for entry in ledger.iter() {
                entries
                    .entry(entry.slot_id.clone())
                    .or_default()
                    .push(entry.clone());
            }
        }
        for list in entries.values_mut() {
            list.sort_by(pool_quota_entry_order);
        }
        let selections: Vec<_> = self
            .pool_selections
            .read()
            .await
            .values()
            .cloned()
            .collect();
        let reserved = selections
            .iter()
            .filter(|record| {
                record.released_at.is_none() && record.request_id != request.request_id
            })
            .filter_map(|record| {
                record
                    .slot_id
                    .clone()
                    .map(|slot_id| (slot_id, record.request_id.clone()))
            })
            .collect();
        let outcome = choose_pool_slot(request, &slots, &entries, &reserved);
        let record = PoolSelectionRecord {
            request_id: outcome.request_id.clone(),
            slot_id: outcome.slot.as_ref().map(|slot| slot.id.clone()),
            selected_at: outcome.selected_at,
            released_at: None,
            idle_since: None,
            reason: outcome.reason.clone(),
            skipped: outcome.skipped.clone(),
        };
        self.pool_selections
            .write()
            .await
            .insert(request.request_id.clone(), record);
        Ok(outcome)
    }

    async fn release_pool_selection(
        &self,
        request_id: &str,
        released_at: u64,
    ) -> Result<(), StoreError> {
        if request_id.trim().is_empty() {
            return Err(StoreError::InvalidRow(
                "pool selection request id is empty".to_string(),
            ));
        }
        timestamp(released_at)?;
        let _guard = self.pool_selection_lock.lock().await;
        if let Some(record) = self.pool_selections.write().await.get_mut(request_id)
            && record.released_at.is_none()
        {
            record.released_at = Some(released_at);
        }
        Ok(())
    }

    async fn load_pool_selections(&self) -> Result<Vec<PoolSelectionRecord>, StoreError> {
        let mut selections: Vec<_> = self
            .pool_selections
            .read()
            .await
            .values()
            .cloned()
            .collect();
        selections.sort_by(|left, right| {
            left.selected_at
                .cmp(&right.selected_at)
                .then(left.request_id.cmp(&right.request_id))
        });
        Ok(selections)
    }

    async fn mark_pool_selection_idle(
        &self,
        request_id: &str,
        idle_since: u64,
    ) -> Result<Option<PoolSelectionRecord>, StoreError> {
        if request_id.trim().is_empty() {
            return Err(StoreError::InvalidRow(
                "pool selection request id is empty".to_string(),
            ));
        }
        timestamp(idle_since)?;
        let _guard = self.pool_selection_lock.lock().await;
        let mut selections = self.pool_selections.write().await;
        let Some(record) = selections.get_mut(request_id) else {
            return Ok(None);
        };
        if idle_since < record.selected_at {
            return Err(StoreError::InvalidRow(
                "pool selection idle timestamp predates selection".to_string(),
            ));
        }
        if record.released_at.is_none() && record.idle_since.is_none() {
            record.idle_since = Some(idle_since);
        }
        Ok(Some(record.clone()))
    }

    async fn release_idle_pool_selections(
        &self,
        request: &PoolIdleReleaseRequest,
    ) -> Result<PoolIdleReleaseOutcome, StoreError> {
        validate_pool_idle_release_request(request)?;
        let _guard = self.pool_selection_lock.lock().await;
        let mut selections = self.pool_selections.write().await;
        let mut released = Vec::new();
        let mut kept = Vec::new();
        for record in selections.values_mut() {
            if record.released_at.is_some() {
                continue;
            }
            let Some(slot_id) = record.slot_id.clone() else {
                continue;
            };
            if let Some(reason) = idle_release_block_reason(record, request) {
                kept.push(PoolIdleKept {
                    request_id: record.request_id.clone(),
                    slot_id,
                    reason: reason.into(),
                });
                continue;
            }
            record.released_at = Some(request.now);
            released.push(PoolIdleReleased {
                request_id: record.request_id.clone(),
                slot_id,
                released_at: request.now,
                reason: "idle timeout elapsed".into(),
            });
        }
        Ok(PoolIdleReleaseOutcome { released, kept })
    }

    async fn load_pool_status(&self, now: u64) -> Result<PoolStatus, StoreError> {
        timestamp(now)?;
        let slots = self.load_pool_slots().await?;
        let selections = self.load_pool_selections().await?;
        let mut statuses = Vec::with_capacity(slots.len());
        for slot in slots {
            let entries = self.load_pool_quota_entries(&slot.id).await?;
            let latest_quota = entries
                .iter()
                .max_by(|left, right| pool_quota_entry_order(left, right))
                .cloned();
            let availability = pool_slot_availability(&slot, &entries, now);
            let reset_windows = pool_reset_window_statuses(&slot, &entries);
            let current_request = selections
                .iter()
                .find(|selection| {
                    selection.released_at.is_none() && selection.slot_id.as_ref() == Some(&slot.id)
                })
                .map(|selection| selection.request_id.clone());
            statuses.push(PoolSlotStatus {
                slot,
                availability,
                latest_quota,
                current_request,
                reset_windows,
            });
        }
        Ok(PoolStatus {
            generated_at: now,
            slots: statuses,
            forced_slot: None,
        })
    }

    async fn reorder_pool_slots(
        &self,
        request: &PoolReorderRequest,
    ) -> Result<PoolReorderOutcome, StoreError> {
        validate_pool_reorder_request(request)?;
        let mut slots = self.pool_slots.write().await;
        if slots.len() != request.slot_ids.len() {
            return Err(StoreError::InvalidRow(
                "pool reorder must include every slot exactly once".to_string(),
            ));
        }
        for (index, slot_id) in request.slot_ids.iter().enumerate() {
            let priority = i64::try_from(index).map_err(|_| {
                StoreError::InvalidRow("pool reorder has too many slots".to_string())
            })?;
            let slot = slots
                .get_mut(slot_id)
                .ok_or_else(|| StoreError::PoolSlotNotFound {
                    slot_id: slot_id.clone(),
                })?;
            slot.priority = priority;
            slot.updated_at = request.now;
        }
        let mut ordered: Vec<_> = slots.values().cloned().collect();
        ordered.sort_by(pool_slot_order);
        Ok(PoolReorderOutcome {
            slots: ordered,
            reason: "priority order updated by user".to_string(),
        })
    }

    async fn set_pool_slot_enabled(
        &self,
        request: &PoolSlotEnabledRequest,
    ) -> Result<PoolSlotEnabledOutcome, StoreError> {
        timestamp(request.now)?;
        if request.slot_id.as_str().trim().is_empty() {
            return Err(StoreError::InvalidRow("pool slot id is empty".to_string()));
        }
        let mut slots = self.pool_slots.write().await;
        let slot = slots
            .get_mut(&request.slot_id)
            .ok_or_else(|| StoreError::PoolSlotNotFound {
                slot_id: request.slot_id.clone(),
            })?;
        slot.enabled = request.enabled;
        slot.updated_at = request.now;
        Ok(PoolSlotEnabledOutcome {
            slot: slot.clone(),
            reason: if request.enabled {
                "slot enabled by user".to_string()
            } else {
                "slot disabled by user".to_string()
            },
        })
    }

    async fn correct_pool_quota(
        &self,
        request: &PoolQuotaCorrectionRequest,
    ) -> Result<PoolQuotaCorrectionOutcome, StoreError> {
        validate_pool_quota_correction_request(request)?;
        let mut slots = self.pool_slots.write().await;
        let slot = slots
            .get_mut(&request.slot_id)
            .ok_or_else(|| StoreError::PoolSlotNotFound {
                slot_id: request.slot_id.clone(),
            })?;
        if let Some(window_key) = request.window_key.as_deref()
            && PoolResetPolicy::for_agent(slot.agent)
                .window_by_key(window_key)
                .is_none()
        {
            return Err(StoreError::InvalidRow(format!(
                "unknown provider window: {window_key}"
            )));
        }
        let mut ledger = self.pool_quota_ledger.write().await;
        let entries: Vec<_> = ledger
            .iter()
            .filter(|entry| entry.slot_id == request.slot_id)
            .cloned()
            .collect();
        let latest =
            latest_pool_quota_entry_for_window(&entries, request.window_key.as_deref()).cloned();
        if pool_correction_is_current(latest.as_ref(), request) {
            return Ok(pool_correction_outcome(
                slot.clone(),
                latest,
                request.now,
                false,
                "limit correction is already current",
            ));
        }
        if latest
            .as_ref()
            .is_some_and(|entry| request.now <= entry.observed_at)
        {
            return Err(StoreError::InvalidRow(
                "pool correction must advance the latest quota observation".to_string(),
            ));
        }
        let entry = pool_quota_correction_entry(slot, latest.as_ref(), request);
        ledger.push(entry.clone());
        apply_pool_quota_entry_to_slot(slot, &entry, request.now);
        Ok(pool_correction_outcome(
            slot.clone(),
            Some(entry),
            request.now,
            true,
            "limit correction applied by user",
        ))
    }

    async fn clear_pool_slot_cooldown(
        &self,
        request: &PoolCooldownClearRequest,
    ) -> Result<PoolQuotaCorrectionOutcome, StoreError> {
        validate_pool_cooldown_clear_request(request)?;
        let mut slots = self.pool_slots.write().await;
        let slot = slots
            .get_mut(&request.slot_id)
            .ok_or_else(|| StoreError::PoolSlotNotFound {
                slot_id: request.slot_id.clone(),
            })?;
        let mut ledger = self.pool_quota_ledger.write().await;
        let latest = ledger
            .iter()
            .filter(|entry| entry.slot_id == request.slot_id)
            .max_by(|left, right| pool_quota_entry_order(left, right))
            .cloned();
        let cooldown = latest
            .as_ref()
            .map_or(slot.cooldown_until, |entry| entry.cooldown_until);
        if cooldown.is_none() {
            return Ok(pool_correction_outcome(
                slot.clone(),
                latest,
                request.now,
                false,
                "cooldown is already clear",
            ));
        }
        if latest
            .as_ref()
            .is_some_and(|entry| request.now <= entry.observed_at)
        {
            return Err(StoreError::InvalidRow(
                "cooldown clear must advance the latest quota observation".to_string(),
            ));
        }
        let entry = pool_cooldown_clear_entry(slot, latest.as_ref(), request.now);
        ledger.push(entry.clone());
        apply_pool_quota_entry_to_slot(slot, &entry, request.now);
        Ok(pool_correction_outcome(
            slot.clone(),
            Some(entry),
            request.now,
            true,
            "cooldown cleared by user",
        ))
    }

    async fn apply_pool_catalog_advisory(
        &self,
        advisory: &PoolCatalogAdvisory,
        now: u64,
    ) -> Result<PoolCatalogOutcome, StoreError> {
        validate_pool_catalog_advisory(advisory)?;
        timestamp(now)?;
        let mut slots = self.pool_slots.write().await;
        let mut ledger = self.pool_quota_ledger.write().await;
        let mut seeds = Vec::with_capacity(slots.len());
        for slot in slots.values_mut() {
            let entries: Vec<_> = ledger
                .iter()
                .filter(|entry| entry.slot_id == slot.id)
                .cloned()
                .collect();
            match pool_catalog_seed_entry(slot, advisory, &entries, now) {
                Ok(entry) => {
                    seeds.push(PoolCatalogSeed {
                        slot_id: slot.id.clone(),
                        applied: true,
                        reason: catalog_seed_reason(&entry),
                    });
                    apply_pool_quota_entry_to_slot(slot, &entry, now);
                    ledger.push(entry);
                }
                Err(reason) => seeds.push(PoolCatalogSeed {
                    slot_id: slot.id.clone(),
                    applied: false,
                    reason,
                }),
            }
        }
        Ok(pool_catalog_outcome(advisory, seeds))
    }
}

pub fn validate_pool_slot(slot: &PoolSlot) -> Result<(), StoreError> {
    if slot.provider_id.trim().is_empty() {
        return Err(StoreError::InvalidRow(
            "pool slot provider id is empty".to_string(),
        ));
    }
    if slot.model_id.trim().is_empty() {
        return Err(StoreError::InvalidRow(
            "pool slot model id is empty".to_string(),
        ));
    }
    if slot.priority < 0 {
        return Err(StoreError::InvalidRow(
            "pool slot priority is negative".to_string(),
        ));
    }
    if slot
        .limit_unit
        .as_deref()
        .is_some_and(|unit| unit.trim().is_empty())
    {
        return Err(StoreError::InvalidRow(
            "pool slot limit unit is empty".to_string(),
        ));
    }
    Ok(())
}

pub fn timestamp(value: u64) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| {
        StoreError::InvalidRow("pool timestamp exceeds the database range".to_string())
    })
}

pub fn timestamp_or_missing(value: Option<u64>) -> Result<i64, StoreError> {
    value
        .map(timestamp)
        .transpose()
        .map(|timestamp| timestamp.unwrap_or(-1))
}

pub fn pool_slot_order(left: &PoolSlot, right: &PoolSlot) -> std::cmp::Ordering {
    left.priority
        .cmp(&right.priority)
        .then(left.created_at.cmp(&right.created_at))
        .then(left.id.cmp(&right.id))
}

pub fn pool_quota_correction_entry(
    slot: &PoolSlot,
    latest: Option<&PoolQuotaEntry>,
    request: &PoolQuotaCorrectionRequest,
) -> PoolQuotaEntry {
    let (used, cooldown_until, error) = latest.map_or(
        (slot.used, slot.cooldown_until, slot.last_error.clone()),
        |entry| (entry.used, entry.cooldown_until, entry.error.clone()),
    );
    let mut entry = PoolQuotaEntry::new(slot.id.clone(), PoolQuotaSource::User, request.now);
    entry.used = used;
    entry.limit = request.limit;
    entry.limit_unit = request.limit_unit.clone();
    entry.reset_at = request.reset_at;
    entry.cooldown_until = cooldown_until;
    entry.error = error;
    entry.window_key = request.window_key.clone();
    entry
}

pub fn pool_cooldown_clear_entry(
    slot: &PoolSlot,
    latest: Option<&PoolQuotaEntry>,
    now: u64,
) -> PoolQuotaEntry {
    let (used, limit, limit_unit, reset_at, error) = latest.map_or(
        (
            slot.used,
            slot.limit,
            slot.limit_unit.clone(),
            slot.reset_at,
            slot.last_error.clone(),
        ),
        |entry| {
            (
                entry.used,
                entry.limit,
                entry.limit_unit.clone(),
                entry.reset_at,
                entry.error.clone(),
            )
        },
    );
    let mut entry = PoolQuotaEntry::new(slot.id.clone(), PoolQuotaSource::User, now);
    entry.used = used;
    entry.limit = limit;
    entry.limit_unit = limit_unit;
    entry.reset_at = reset_at;
    entry.cooldown_until = None;
    entry.error = error;
    entry
}

pub fn pool_correction_is_current(
    latest: Option<&PoolQuotaEntry>,
    request: &PoolQuotaCorrectionRequest,
) -> bool {
    latest.is_some_and(|entry| {
        entry.source == PoolQuotaSource::User
            && entry.limit == request.limit
            && entry.limit_unit.as_deref() == request.limit_unit.as_deref()
            && entry.reset_at == request.reset_at
            && entry.window_key == request.window_key
    })
}

pub fn pool_correction_outcome(
    slot: PoolSlot,
    latest: Option<PoolQuotaEntry>,
    now: u64,
    applied: bool,
    reason: impl Into<String>,
) -> PoolQuotaCorrectionOutcome {
    let availability = slot.availability(latest.as_ref(), now);
    PoolQuotaCorrectionOutcome {
        slot,
        latest_quota: latest,
        availability,
        applied,
        reason: reason.into(),
    }
}

pub fn apply_pool_quota_entry_to_slot(slot: &mut PoolSlot, entry: &PoolQuotaEntry, now: u64) {
    slot.used = entry.used;
    slot.limit = entry.limit;
    slot.limit_unit = entry.limit_unit.clone();
    slot.reset_at = entry.reset_at;
    slot.cooldown_until = entry.cooldown_until;
    slot.last_error = entry.error.clone();
    slot.updated_at = now;
}

pub fn validate_pool_quota_correction_request(
    request: &PoolQuotaCorrectionRequest,
) -> Result<(), StoreError> {
    timestamp(request.now)?;
    if request.slot_id.as_str().trim().is_empty() {
        return Err(StoreError::InvalidRow("pool slot id is empty".to_string()));
    }
    if request
        .limit_unit
        .as_deref()
        .is_some_and(|unit| unit.trim().is_empty())
    {
        return Err(StoreError::InvalidRow(
            "pool correction limit unit is empty".to_string(),
        ));
    }
    if request.limit.is_none() && request.limit_unit.is_some() {
        return Err(StoreError::InvalidRow(
            "pool correction limit unit requires a limit".to_string(),
        ));
    }
    if request
        .window_key
        .as_deref()
        .is_some_and(|key| key.trim().is_empty())
    {
        return Err(StoreError::InvalidRow(
            "pool correction window key is empty".to_string(),
        ));
    }
    if request
        .limit
        .is_some_and(|limit| i64::try_from(limit).is_err())
    {
        return Err(StoreError::InvalidRow(
            "pool correction limit exceeds the database range".to_string(),
        ));
    }
    if let Some(reset_at) = request.reset_at {
        timestamp(reset_at)?;
    }
    Ok(())
}

pub fn validate_pool_cooldown_clear_request(
    request: &PoolCooldownClearRequest,
) -> Result<(), StoreError> {
    timestamp(request.now)?;
    if request.slot_id.as_str().trim().is_empty() {
        return Err(StoreError::InvalidRow("pool slot id is empty".to_string()));
    }
    Ok(())
}

pub fn validate_pool_quota_entry(entry: &PoolQuotaEntry) -> Result<(), StoreError> {
    if entry.id.trim().is_empty() {
        return Err(StoreError::InvalidRow(
            "quota ledger entry id is empty".to_string(),
        ));
    }
    if entry.slot_id.as_str().trim().is_empty() {
        return Err(StoreError::InvalidRow(
            "quota ledger slot id is empty".to_string(),
        ));
    }
    if entry
        .limit_unit
        .as_deref()
        .is_some_and(|unit| unit.trim().is_empty())
    {
        return Err(StoreError::InvalidRow(
            "quota ledger limit unit is empty".to_string(),
        ));
    }
    Ok(())
}

pub fn validate_pool_catalog_advisory(advisory: &PoolCatalogAdvisory) -> Result<(), StoreError> {
    if advisory.contract_version != POOL_CATALOG_CONTRACT_VERSION {
        return Err(StoreError::InvalidRow(format!(
            "unsupported catalog advisory contract version {} (expected {})",
            advisory.contract_version, POOL_CATALOG_CONTRACT_VERSION
        )));
    }
    if advisory.snapshot_id.trim().is_empty() {
        return Err(StoreError::InvalidRow(
            "catalog snapshot id is empty".to_string(),
        ));
    }
    Ok(())
}

pub fn pool_catalog_seed_entry(
    slot: &PoolSlot,
    advisory: &PoolCatalogAdvisory,
    entries: &[PoolQuotaEntry],
    now: u64,
) -> Result<PoolQuotaEntry, String> {
    if advisory.stale {
        return Err("catalog snapshot is stale".to_string());
    }
    let matches: Vec<&PoolCatalogRecord> = advisory
        .records
        .iter()
        .filter(|record| {
            record.provider_id == slot.provider_id
                && record
                    .model_id
                    .as_deref()
                    .is_none_or(|model| model == slot.model_id)
        })
        .collect();
    let record = match matches.as_slice() {
        [] => return Err("catalog has no record for this slot".to_string()),
        [record] => *record,
        many => {
            return Err(format!(
                "catalog has {} records for this slot; no single advisory",
                many.len()
            ));
        }
    };
    if record.stale {
        return Err("catalog record is stale".to_string());
    }
    if record.capacity != "known" {
        return Err("catalog capacity is unknown".to_string());
    }
    let Some(allowance) = record.allowance else {
        return Err("catalog allowance is unknown".to_string());
    };
    let policy = PoolResetPolicy::for_agent(slot.agent);
    let window = record
        .reset_period
        .as_deref()
        .and_then(|period| policy.window_for_period(period));
    let latest = latest_pool_quota_entry_for_window(entries, window.map(|window| window.key))
        .or_else(|| latest_pool_quota_entry_for_window(entries, None));
    if latest.is_some_and(|entry| entry.source != PoolQuotaSource::Catalog && entry.limit.is_some())
    {
        return Err("local limit wins over the catalog".to_string());
    }
    if latest.is_some_and(|entry| now <= entry.observed_at) {
        return Err("catalog snapshot is not newer than the latest observation".to_string());
    }
    let (used, cooldown_until, error) = latest.map_or(
        (slot.used, slot.cooldown_until, slot.last_error.clone()),
        |entry| (entry.used, entry.cooldown_until, entry.error.clone()),
    );
    let mut entry = PoolQuotaEntry::new(slot.id.clone(), PoolQuotaSource::Catalog, now);
    entry.used = used;
    entry.limit = Some(allowance);
    entry.limit_unit = record
        .allowance_unit
        .clone()
        .filter(|unit| !unit.trim().is_empty());
    // A reset time needs both a window length and a start. The provider's
    // policy supplies the length and the slot's last use supplies the start;
    // a provider or user reset still wins over a derived one.
    let derived_reset = slot
        .last_used_at
        .and_then(|last_used_at| window.and_then(|window| window.reset_at(last_used_at)));
    let (local_reset, catalog_reset) = match latest {
        Some(entry) if entry.source == PoolQuotaSource::Catalog => (None, entry.reset_at),
        Some(entry) => (entry.reset_at, None),
        None => (None, None),
    };
    entry.reset_at = local_reset.or(derived_reset).or(catalog_reset);
    entry.cooldown_until = cooldown_until;
    entry.error = error;
    entry.window_key = window.map(|window| window.key.to_string());
    Ok(entry)
}

pub fn catalog_seed_reason(entry: &PoolQuotaEntry) -> String {
    format!(
        "catalog seeded {} {}",
        entry.limit.unwrap_or_default(),
        entry.limit_unit.as_deref().unwrap_or("units")
    )
}

pub fn pool_catalog_outcome(
    advisory: &PoolCatalogAdvisory,
    seeds: Vec<PoolCatalogSeed>,
) -> PoolCatalogOutcome {
    let applied = seeds.iter().filter(|seed| seed.applied).count();
    PoolCatalogOutcome {
        snapshot_id: advisory.snapshot_id.clone(),
        skipped: seeds.len() - applied,
        seeds,
        applied,
    }
}

pub fn pool_quota_entry_order(left: &PoolQuotaEntry, right: &PoolQuotaEntry) -> std::cmp::Ordering {
    left.observed_at
        .cmp(&right.observed_at)
        .then(left.id.cmp(&right.id))
}

pub fn validate_pool_reorder_request(request: &PoolReorderRequest) -> Result<(), StoreError> {
    timestamp(request.now)?;
    if request.slot_ids.is_empty() {
        return Err(StoreError::InvalidRow(
            "pool reorder must include every slot exactly once".to_string(),
        ));
    }
    let mut seen = BTreeSet::new();
    for slot_id in &request.slot_ids {
        if slot_id.as_str().trim().is_empty() {
            return Err(StoreError::InvalidRow("pool slot id is empty".to_string()));
        }
        if !seen.insert(slot_id) {
            return Err(StoreError::InvalidRow(
                "pool reorder contains a duplicate slot".to_string(),
            ));
        }
    }
    Ok(())
}

pub fn validate_pool_idle_release_request(
    request: &PoolIdleReleaseRequest,
) -> Result<(), StoreError> {
    timestamp(request.now)?;
    if request.idle_timeout_ms == 0 {
        return Err(StoreError::InvalidRow(
            "pool idle timeout must be positive".to_string(),
        ));
    }
    if request
        .active_requests
        .iter()
        .any(|request_id| request_id.trim().is_empty())
    {
        return Err(StoreError::InvalidRow(
            "pool active request id is empty".to_string(),
        ));
    }
    Ok(())
}

pub fn idle_release_block_reason(
    record: &PoolSelectionRecord,
    request: &PoolIdleReleaseRequest,
) -> Option<&'static str> {
    if request.active_requests.contains(&record.request_id) {
        return Some("request is active");
    }
    let Some(idle_since) = record.idle_since else {
        return Some("turn has not settled");
    };
    if idle_since > request.now {
        return Some("idle timestamp is in the future");
    }
    if request.now - idle_since < request.idle_timeout_ms {
        return Some("idle timeout not reached");
    }
    None
}

pub fn validate_pool_selection_request(request: &PoolSelectionRequest) -> Result<(), StoreError> {
    if request.request_id.trim().is_empty() {
        return Err(StoreError::InvalidRow(
            "pool selection request id is empty".to_string(),
        ));
    }
    if request
        .override_slot
        .as_ref()
        .is_some_and(|slot| slot.as_str().trim().is_empty())
    {
        return Err(StoreError::InvalidRow(
            "pool selection override slot id is empty".to_string(),
        ));
    }
    timestamp(request.now)?;
    Ok(())
}

pub fn validate_pool_selection_record(record: &PoolSelectionRecord) -> Result<(), StoreError> {
    if record.request_id.trim().is_empty() {
        return Err(StoreError::InvalidRow(
            "pool selection request id is empty".to_string(),
        ));
    }
    if record.reason.trim().is_empty() {
        return Err(StoreError::InvalidRow(
            "pool selection reason is empty".to_string(),
        ));
    }
    if record
        .idle_since
        .is_some_and(|idle_since| idle_since < record.selected_at)
    {
        return Err(StoreError::InvalidRow(
            "pool selection idle timestamp predates selection".to_string(),
        ));
    }
    if let Some(slot_id) = &record.slot_id
        && slot_id.as_str().trim().is_empty()
    {
        return Err(StoreError::InvalidRow(
            "pool selection slot id is empty".to_string(),
        ));
    }
    for skipped in &record.skipped {
        if skipped.slot_id.as_str().trim().is_empty() {
            return Err(StoreError::InvalidRow(
                "pool selection skipped slot id is empty".to_string(),
            ));
        }
        if skipped.reason.trim().is_empty() {
            return Err(StoreError::InvalidRow(
                "pool selection skipped reason is empty".to_string(),
            ));
        }
    }
    Ok(())
}

pub fn choose_pool_slot(
    request: &PoolSelectionRequest,
    slots: &[PoolSlot],
    entries: &BTreeMap<PoolSlotId, Vec<PoolQuotaEntry>>,
    reserved: &BTreeMap<PoolSlotId, String>,
) -> PoolSelectionOutcome {
    let mut ordered: Vec<&PoolSlot> = slots.iter().collect();
    ordered.sort_by(|left, right| pool_slot_order(left, right));
    let mut skipped = Vec::new();
    if ordered.is_empty() {
        return PoolSelectionOutcome {
            request_id: request.request_id.clone(),
            slot: None,
            selected_at: request.now,
            reason: "no pool slots".into(),
            skipped,
            reused: false,
        };
    }

    if let Some(override_slot) = request.override_slot.as_ref() {
        match ordered.iter().find(|slot| &slot.id == override_slot) {
            Some(slot) => {
                if let Some(skip) = pool_slot_skip(
                    slot,
                    entries,
                    reserved,
                    request.agent,
                    &request.request_id,
                    request.now,
                ) {
                    skipped.push(skip);
                } else {
                    return pool_selection_chosen(request, slot, "task override", skipped);
                }
            }
            None => skipped.push(PoolSkippedSlot {
                slot_id: override_slot.clone(),
                state: None,
                reason: "override slot is not in the pool".into(),
            }),
        }
    }

    for slot in ordered {
        if request.override_slot.as_ref() == Some(&slot.id) {
            continue;
        }
        if let Some(skip) = pool_slot_skip(
            slot,
            entries,
            reserved,
            request.agent,
            &request.request_id,
            request.now,
        ) {
            skipped.push(skip);
            continue;
        }
        return pool_selection_chosen(request, slot, "first available by priority", skipped);
    }

    PoolSelectionOutcome {
        request_id: request.request_id.clone(),
        slot: None,
        selected_at: request.now,
        reason: "no eligible pool slot".into(),
        skipped,
        reused: false,
    }
}

pub fn pool_slot_skip(
    slot: &PoolSlot,
    entries: &BTreeMap<PoolSlotId, Vec<PoolQuotaEntry>>,
    reserved: &BTreeMap<PoolSlotId, String>,
    agent: Option<crate::tasks::Agent>,
    request_id: &str,
    now: u64,
) -> Option<PoolSkippedSlot> {
    if let Some(agent) = agent
        && slot.agent != agent
    {
        return Some(PoolSkippedSlot {
            slot_id: slot.id.clone(),
            state: None,
            reason: format!("slot runs the {} agent", slot.agent.as_str()),
        });
    }
    if let Some(owner) = reserved.get(&slot.id)
        && owner != request_id
    {
        return Some(PoolSkippedSlot {
            slot_id: slot.id.clone(),
            state: None,
            reason: format!("reserved by request {owner}"),
        });
    }
    let availability = pool_slot_availability(
        slot,
        entries.get(&slot.id).map(Vec::as_slice).unwrap_or(&[]),
        now,
    );
    (availability.state != PoolAvailabilityState::Available).then(|| PoolSkippedSlot {
        slot_id: slot.id.clone(),
        state: Some(availability.state),
        reason: availability.reason,
    })
}

pub fn pool_selection_chosen(
    request: &PoolSelectionRequest,
    slot: &PoolSlot,
    reason: &str,
    skipped: Vec<PoolSkippedSlot>,
) -> PoolSelectionOutcome {
    PoolSelectionOutcome {
        request_id: request.request_id.clone(),
        slot: Some(slot.clone()),
        selected_at: request.now,
        reason: reason.into(),
        skipped,
        reused: false,
    }
}
