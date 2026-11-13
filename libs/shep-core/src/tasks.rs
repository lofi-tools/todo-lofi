//! Durable task records and the task state machine.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::projects::{ProjectId, now_millis};

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TaskId(String);

impl TaskId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TaskId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Agent {
    #[default]
    Codex,
    OpenCode,
    Pi,
    Claude,
    Freebuff,
}

impl Agent {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "codex" => Some(Self::Codex),
            "opencode" => Some(Self::OpenCode),
            "pi" => Some(Self::Pi),
            "claude" => Some(Self::Claude),
            "freebuff" => Some(Self::Freebuff),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::OpenCode => "opencode",
            Self::Pi => "pi",
            Self::Claude => "claude",
            Self::Freebuff => "freebuff",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::OpenCode => "OpenCode",
            Self::Pi => "Pi",
            Self::Claude => "Claude Code",
            Self::Freebuff => "Freebuff",
        }
    }

    pub fn default_tier(self) -> AgentTier {
        match self {
            Self::Claude | Self::OpenCode => AgentTier::Acp,
            Self::Freebuff => AgentTier::TerminalWrapper,
            Self::Codex | Self::Pi => AgentTier::Native,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentTier {
    Native,
    Acp,
    TerminalWrapper,
    PlainPane,
}

impl AgentTier {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "native" => Some(Self::Native),
            "acp" => Some(Self::Acp),
            "terminal-wrapper" => Some(Self::TerminalWrapper),
            "plain-pane" => Some(Self::PlainPane),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Acp => "acp",
            Self::TerminalWrapper => "terminal-wrapper",
            Self::PlainPane => "plain-pane",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskState {
    Draft,
    Starting,
    Ready,
    Working,
    Waiting,
    Queued,
    Review,
    Done,
    Failed,
    Cancelled,
    Interrupted,
}

impl TaskState {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "draft" => Some(Self::Draft),
            "starting" => Some(Self::Starting),
            "ready" => Some(Self::Ready),
            "working" => Some(Self::Working),
            "waiting" => Some(Self::Waiting),
            "queued" => Some(Self::Queued),
            "review" => Some(Self::Review),
            "done" => Some(Self::Done),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            "interrupted" => Some(Self::Interrupted),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Starting => "starting",
            Self::Ready => "ready",
            Self::Working => "working",
            Self::Waiting => "waiting",
            Self::Queued => "queued",
            Self::Review => "review",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }

    pub fn can_transition_to(self, next: Self) -> bool {
        use TaskState::{
            Cancelled, Done, Draft, Failed, Interrupted, Queued, Ready, Review, Starting, Waiting,
            Working,
        };
        match (self, next) {
            (Draft, Starting | Cancelled) => true,
            (Starting, Ready | Failed | Cancelled | Interrupted) => true,
            (Ready, Starting | Working | Cancelled | Interrupted) => true,
            (Working, Waiting | Review | Ready | Failed | Cancelled | Interrupted) => true,
            (Waiting, Ready | Working | Review | Failed | Cancelled | Interrupted) => true,
            (Queued, Working | Cancelled | Interrupted) => true,
            (Review, Working | Done | Failed | Interrupted) => true,
            (Done, Review) => true,
            (Failed, Starting | Ready | Working | Cancelled) => true,
            (Interrupted, Starting | Ready | Working | Cancelled) => true,
            (Cancelled, _) => false,
            _ => false,
        }
    }

    /// The transitions this state allows, for diagnostics.
    pub fn allowed_next(self) -> &'static str {
        match self {
            Self::Draft => "starting, cancelled",
            Self::Starting => "ready, failed, cancelled, interrupted",
            Self::Ready => "starting, working, cancelled, interrupted",
            Self::Working => "waiting, review, ready, failed, cancelled, interrupted",
            Self::Waiting => "ready, working, review, failed, cancelled, interrupted",
            Self::Queued => "working, cancelled, interrupted",
            Self::Review => "working, done, failed, interrupted",
            Self::Done => "review",
            Self::Failed => "starting, ready, working, cancelled",
            Self::Interrupted => "starting, ready, working, cancelled",
            Self::Cancelled => "none",
        }
    }

    pub fn is_live(self) -> bool {
        matches!(
            self,
            Self::Starting | Self::Working | Self::Waiting | Self::Queued
        )
    }
}

#[derive(Clone, Debug)]
pub struct Task {
    pub id: TaskId,
    pub project_id: ProjectId,
    pub title: String,
    pub state: TaskState,
    pub agent: Agent,
    pub tier: AgentTier,
    pub proposed_worktree_path: PathBuf,
    pub proposed_branch: String,
    pub worktree_path: Option<PathBuf>,
    pub branch: Option<String>,
    pub base_branch: Option<String>,
    pub branch_tip_sha: Option<String>,
    pub model: Option<String>,
    pub mode: Option<String>,
    pub branch_error: Option<String>,
    pub error: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
    pub worktree_last_used_at: u64,
    pub collection_pending: bool,
}

impl Task {
    /// The proposal names the repository's real Git directory, so a checkout
    /// whose `.git` is a file still gets the right path.
    pub fn draft(project_id: ProjectId, title: &str, git_dir: &Path) -> Self {
        let slug = slugify(title);
        let now = now_millis();
        Self {
            id: TaskId::new(Uuid::new_v4().to_string()),
            project_id,
            title: title.to_string(),
            state: TaskState::Draft,
            agent: Agent::default(),
            tier: AgentTier::Native,
            proposed_worktree_path: git_dir.join("shepherd").join("worktrees").join(&slug),
            proposed_branch: format!("shepherd/{slug}"),
            worktree_path: None,
            branch: None,
            base_branch: None,
            branch_tip_sha: None,
            model: None,
            mode: None,
            branch_error: None,
            error: None,
            created_at: now,
            updated_at: now,
            worktree_last_used_at: now,
            collection_pending: false,
        }
    }

    pub fn set_state(&mut self, next: TaskState) -> Result<(), crate::CoreError> {
        if !self.state.can_transition_to(next) {
            return Err(crate::CoreError::InvalidTransition {
                id: self.id.as_str().to_string(),
                from: self.state.as_str(),
                to: next.as_str(),
                allowed: self.state.allowed_next(),
            });
        }
        self.state = next;
        self.updated_at = now_millis();
        Ok(())
    }
}

/// A compact row for the flat sidebar task list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskSummary {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub state: TaskState,
    pub agent: Agent,
    pub tier: AgentTier,
    pub branch: Option<String>,
    pub worktree_path: Option<String>,
    pub error: Option<String>,
    pub created_at: u64,
}

impl From<&Task> for TaskSummary {
    fn from(task: &Task) -> Self {
        Self {
            id: task.id.as_str().to_string(),
            project_id: task.project_id.as_str().to_string(),
            title: task.title.clone(),
            state: task.state,
            agent: task.agent,
            tier: task.tier,
            branch: task.branch.clone(),
            worktree_path: task
                .worktree_path
                .as_ref()
                .map(|path| path.display().to_string()),
            error: task.error.clone(),
            created_at: task.created_at,
        }
    }
}

/// The fuller task record for the fixed task workspace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskDetail {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub state: TaskState,
    pub agent: Agent,
    pub tier: AgentTier,
    pub proposed_branch: String,
    pub proposed_worktree_path: String,
    pub branch: Option<String>,
    pub worktree_path: Option<String>,
    pub base_branch: Option<String>,
    pub model: Option<String>,
    pub mode: Option<String>,
    pub branch_error: Option<String>,
    pub error: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
}

impl From<&Task> for TaskDetail {
    fn from(task: &Task) -> Self {
        Self {
            id: task.id.as_str().to_string(),
            project_id: task.project_id.as_str().to_string(),
            title: task.title.clone(),
            state: task.state,
            agent: task.agent,
            tier: task.tier,
            proposed_branch: task.proposed_branch.clone(),
            proposed_worktree_path: task.proposed_worktree_path.display().to_string(),
            branch: task.branch.clone(),
            worktree_path: task
                .worktree_path
                .as_ref()
                .map(|path| path.display().to_string()),
            base_branch: task.base_branch.clone(),
            model: task.model.clone(),
            mode: task.mode.clone(),
            branch_error: task.branch_error.clone(),
            error: task.error.clone(),
            created_at: task.created_at,
            updated_at: task.updated_at,
        }
    }
}

fn slugify(title: &str) -> String {
    let mut slug = String::new();
    let mut previous_dash = false;
    for character in title.chars() {
        if character.is_alphanumeric() {
            for lower in character.to_lowercase() {
                slug.push(lower);
            }
            previous_dash = false;
        } else if !slug.is_empty() && !previous_dash {
            slug.push('-');
            previous_dash = true;
        }
        if slug.chars().count() >= 48 {
            break;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        "task".to_string()
    } else {
        slug
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freebuff_is_a_terminal_wrapper_agent() {
        assert_eq!(Agent::parse("freebuff"), Some(Agent::Freebuff));
        assert_eq!(Agent::Freebuff.as_str(), "freebuff");
        assert_eq!(Agent::Freebuff.default_tier(), AgentTier::TerminalWrapper);
        assert_eq!(
            AgentTier::parse(Agent::Freebuff.default_tier().as_str()),
            Some(AgentTier::TerminalWrapper)
        );
    }
}
