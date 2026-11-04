//! Commands a shell may send and the typed result of each one.

use std::path::PathBuf;

use crate::projects::ProjectId;
use crate::snapshots::{ProjectList, TaskList, TaskWorkspace};
use crate::tasks::TaskId;

#[derive(Clone, Debug)]
pub enum Command {
    AddProject {
        path: PathBuf,
    },
    CreateDraft {
        project_id: ProjectId,
        title: String,
    },
    DiscardDraft {
        task_id: TaskId,
    },
    Quit {
        force: bool,
    },
}

#[derive(Clone, Debug)]
pub enum CommandResult {
    ProjectList(ProjectList),
    TaskList(TaskList),
    TaskWorkspace(TaskWorkspace),
    Quit(QuitSummary),
    Ack,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuitSummary {
    pub interrupted_tasks: usize,
    pub live_terminals: usize,
}
