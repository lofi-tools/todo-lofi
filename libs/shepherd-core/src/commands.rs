//! Commands a shell may send and the typed result of each one.

use std::path::PathBuf;

use agent_harness::{ApprovalId, Decision, PromptInput};
use serde::{Deserialize, Serialize};

use crate::projects::ProjectId;
use crate::snapshots::{ProjectList, TaskList, TaskWorkspace};
use crate::tasks::{Agent, TaskId};

#[derive(Clone, Debug)]
pub enum Command {
    AddProject {
        path: PathBuf,
    },
    SetProjectPreviewUrl {
        project_id: ProjectId,
        url: String,
    },
    CreateDraft {
        project_id: ProjectId,
        title: String,
        agent: Agent,
    },
    SetDraftAgent {
        task_id: TaskId,
        agent: Agent,
    },
    DiscardDraft {
        task_id: TaskId,
    },
    SendPrompt {
        task_id: TaskId,
        input: PromptInput,
    },
    ResolveApproval {
        task_id: TaskId,
        approval_id: ApprovalId,
        decision: Decision,
    },
    CancelTask {
        task_id: TaskId,
    },
    ResumeTask {
        task_id: TaskId,
    },
    ReviewTask {
        task_id: TaskId,
        action: ReviewAction,
    },
    Quit {
        force: bool,
    },
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ReviewAction {
    Archive,
    Reopen,
    SendNote { text: String },
    SendQueuedNote,
    RemoveQueuedNote,
}

#[derive(Clone, Debug)]
pub enum CommandResult {
    ProjectList(ProjectList),
    TaskList(TaskList),
    TaskWorkspace(TaskWorkspace),
    Quit(QuitSummary),
    Ack,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuitSummary {
    pub interrupted_tasks: usize,
    pub live_terminals: usize,
}
