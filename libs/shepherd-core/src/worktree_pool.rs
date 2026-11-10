//! Durable Git worktree pool shared across the tasks of one project.
//!
//! These records are intentionally separate from the provider/model agent pool
//! in `store.rs`: this pool owns reusable Git checkouts, not agent capacity.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::projects::{ProjectId, now_millis};
use crate::tasks::TaskId;

/// The number of reusable checkouts a project grows to under concurrent demand.
pub const TARGET_CHECKOUTS: usize = 3;
/// Highest numbered `pool-N` slot the allocator considers before falling back
/// to an unpooled overflow checkout for a retired slot that cannot be replaced.
const MAX_SLOT_INDEX: usize = 64;
pub const RETIRED_MARKER: &str = ".shepherd-retired";
const OVERFLOW_PREFIX: &str = "pool-temp-";

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorktreePoolId(String);

impl WorktreePoolId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for WorktreePoolId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorktreePoolState {
    Idle,
    Leased,
    Retired,
}

impl WorktreePoolState {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "idle" => Some(Self::Idle),
            "leased" => Some(Self::Leased),
            "retired" => Some(Self::Retired),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Leased => "leased",
            Self::Retired => "retired",
        }
    }
}

/// A reusable Git checkout owned by the project worktree pool.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreePoolRecord {
    pub id: WorktreePoolId,
    pub project_id: ProjectId,
    pub path: PathBuf,
    pub state: WorktreePoolState,
    pub task_id: Option<TaskId>,
    pub leased_at: Option<u64>,
    pub created_at: u64,
    pub updated_at: u64,
}

impl WorktreePoolRecord {
    pub fn new(project_id: ProjectId, path: PathBuf) -> Self {
        let now = now_millis();
        Self {
            id: WorktreePoolId::new(uuid::Uuid::new_v4().to_string()),
            project_id,
            path,
            state: WorktreePoolState::Idle,
            task_id: None,
            leased_at: None,
            created_at: now,
            updated_at: now,
        }
    }
}

pub fn slot_path(root: &Path, index: usize) -> PathBuf {
    root.join(format!("pool-{index}"))
}

pub fn overflow_path(root: &Path) -> PathBuf {
    root.join(format!("{OVERFLOW_PREFIX}{}", uuid::Uuid::new_v4()))
}

pub fn is_overflow_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(OVERFLOW_PREFIX))
}

pub fn reusable_count(records: &[WorktreePoolRecord]) -> usize {
    records
        .iter()
        .filter(|record| record.state != WorktreePoolState::Retired)
        .count()
}

/// The first unused, unrecorded `pool-N` path, ignoring retired records so a
/// dirty or broken checkout can be replaced instead of reused.
pub fn next_slot_path(
    root: &Path,
    records: &[WorktreePoolRecord],
    used: &std::collections::BTreeSet<PathBuf>,
) -> Option<PathBuf> {
    for index in 1..=MAX_SLOT_INDEX {
        let path = slot_path(root, index);
        if path.exists()
            || used.contains(&path)
            || records.iter().any(|record| record.path == path)
        {
            continue;
        }
        return Some(path);
    }
    None
}
