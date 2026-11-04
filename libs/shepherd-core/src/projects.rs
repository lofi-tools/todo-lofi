//! Local project records and path validation.

use std::fmt;
use std::path::PathBuf;

use serde::Serialize;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub struct ProjectId(String);

impl ProjectId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProjectId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Debug)]
pub struct Project {
    pub id: ProjectId,
    pub path: PathBuf,
    pub name: String,
    pub missing: bool,
    pub last_task_id: Option<crate::tasks::TaskId>,
    pub created_at: u64,
}

impl Project {
    pub fn new(path: PathBuf) -> Self {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_string)
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| path.display().to_string());
        Self {
            id: ProjectId::new(Uuid::new_v4().to_string()),
            missing: !path.exists(),
            path,
            name,
            last_task_id: None,
            created_at: now_millis(),
        }
    }
}

/// The project row a shell paints. Task counts exclude `done` and `cancelled`,
/// matching the sidebar's unfinished count.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSnapshot {
    pub id: String,
    pub path: String,
    pub name: String,
    pub missing: bool,
    pub last_task_id: Option<String>,
    pub unfinished_task_count: usize,
}

pub(crate) fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}
