//! Events emitted by the core after a command or a state change.

use crate::snapshots::{ProjectList, TaskList, TaskWorkspace};
use crate::tasks::TaskSummary;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShutdownState {
    Running,
    Draining,
    Terminating,
    Closed,
}

#[derive(Clone, Debug)]
pub enum Event {
    ProjectListChanged(ProjectList),
    TaskListChanged(TaskList),
    TaskChanged(TaskSummary),
    TaskWorkspaceChanged(TaskWorkspace),
    ShutdownStateChanged(ShutdownState),
    Error {
        task_id: Option<String>,
        message: String,
    },
}
