//! Snapshots returned by commands and included in change events.

use agent_harness::{AgentDiagnostics, Entry};
use serde::Serialize;

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

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskWorkspace {
    pub project: ProjectSnapshot,
    pub task: TaskDetail,
    pub transcript: Vec<Entry>,
    /// What the adapter launched and speaks, once a session exists.
    pub diagnostics: Option<AgentDiagnostics>,
}
