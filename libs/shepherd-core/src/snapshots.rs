//! Snapshots returned by commands and included in change events.

use agent_harness::{AgentDiagnostics, AgentTerminal, Capabilities, Entry, SessionApproval};
use serde::{Deserialize, Serialize};

use crate::projects::{ProjectId, ProjectSnapshot};
use crate::store::ProjectApproval;
use crate::tasks::{Agent, AgentTier, TaskDetail, TaskSummary};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectApprovalsSnapshot {
    pub saved: Vec<ProjectApproval>,
    pub sessions: Vec<TaskApproval>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskApproval {
    pub task_id: String,
    pub task_title: String,
    pub agent: Agent,
    #[serde(flatten)]
    pub approval: SessionApproval,
}

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
    pub agent_terminals: Vec<AgentTerminal>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queued_review_note: Option<String>,
    /// What the adapter launched and speaks, once a session exists.
    pub diagnostics: Option<AgentDiagnostics>,
    pub adapter_tier: Option<AgentTier>,
    pub capabilities: Option<Capabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modes: Option<agent_harness::AgentModes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub models: Option<agent_harness::AgentModels>,
}
