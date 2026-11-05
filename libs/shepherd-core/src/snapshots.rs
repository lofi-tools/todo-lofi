//! Snapshots returned by commands and included in change events.

use agent_harness::{AgentDiagnostics, Entry};
use serde::{Deserialize, Serialize};

use crate::projects::{ProjectId, ProjectSnapshot};
use crate::tasks::{TaskDetail, TaskSummary};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectList {
    pub projects: Vec<ProjectSnapshot>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskList {
    pub project_id: ProjectId,
    pub tasks: Vec<TaskSummary>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceLayout {
    pub inspector_width: Option<u16>,
}

impl WorkspaceLayout {
    pub(crate) fn is_valid(self) -> bool {
        self.inspector_width
            .is_none_or(|width| (240..=1600).contains(&width))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskWorkspace {
    pub project: ProjectSnapshot,
    pub task: TaskDetail,
    pub layout: WorkspaceLayout,
    pub cancelling: bool,
    pub transcript: Vec<Entry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queued_review_note: Option<String>,
    /// What the adapter launched and speaks, once a session exists.
    pub diagnostics: Option<AgentDiagnostics>,
}
