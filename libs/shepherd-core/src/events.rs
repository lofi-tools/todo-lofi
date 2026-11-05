//! Events emitted by the core after a command or a state change.

use serde::Serialize;

use crate::snapshots::{ProjectList, TaskList, TaskWorkspace};
use crate::tasks::TaskSummary;
use crate::terminal::TerminalSnapshot;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ShutdownState {
    Running,
    Draining,
    Terminating,
    Closed,
}

/// Every change a shell can observe, in one tag/content shape so the frontend
/// can switch on `type` without a second model.
#[derive(Clone, Debug, Serialize)]
#[serde(
    tag = "type",
    content = "data",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
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
    TerminalBell {
        project_id: crate::projects::ProjectId,
        task_id: Option<String>,
        terminal_id: String,
    },
    TerminalListChanged {
        project_id: crate::projects::ProjectId,
        terminals: Vec<TerminalSnapshot>,
    },
}
