//! Snapshots returned by commands and included in change events.

use crate::projects::{ProjectId, ProjectSnapshot};
use crate::tasks::{TaskDetail, TaskSummary};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectList {
    pub projects: Vec<ProjectSnapshot>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskList {
    pub project_id: ProjectId,
    pub tasks: Vec<TaskSummary>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskWorkspace {
    pub project: ProjectSnapshot,
    pub task: TaskDetail,
}
