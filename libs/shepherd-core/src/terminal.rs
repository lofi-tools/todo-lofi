//! The terminal boundary and its durable records. Parsing and rendering live in
//! the shell behind [`TerminalService`]; the core only knows what a terminal is
//! and what happened to it.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc::UnboundedSender, oneshot};

use crate::projects::ProjectId;
use crate::tasks::TaskId;

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TerminalId(String);

impl TerminalId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TerminalScope {
    Project,
    Task,
}

impl TerminalScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Task => "task",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "project" => Some(Self::Project),
            "task" => Some(Self::Task),
            _ => None,
        }
    }
}

/// Terminal metadata worth keeping. Scrollback is deliberately absent: it does
/// not survive a restart, and pretending otherwise would be a lie in the UI.
#[derive(Clone, Debug)]
pub struct TerminalRecord {
    pub id: TerminalId,
    pub project_id: ProjectId,
    pub task_id: Option<TaskId>,
    pub scope: TerminalScope,
    pub title: String,
    pub cwd: PathBuf,
    pub command: String,
    pub created_at: u64,
    pub ended_at: Option<u64>,
    pub exit_code: Option<i32>,
}

impl TerminalRecord {
    pub fn project_shared(
        id: TerminalId,
        project_id: ProjectId,
        title: impl Into<String>,
        cwd: PathBuf,
        command: impl Into<String>,
        created_at: u64,
    ) -> Self {
        Self {
            id,
            project_id,
            task_id: None,
            scope: TerminalScope::Project,
            title: title.into(),
            cwd,
            command: command.into(),
            created_at,
            ended_at: None,
            exit_code: None,
        }
    }

    pub fn for_task(mut self, task_id: TaskId) -> Self {
        self.task_id = Some(task_id);
        self.scope = TerminalScope::Task;
        self
    }

    pub fn snapshot(&self) -> TerminalSnapshot {
        TerminalSnapshot {
            id: self.id.as_str().to_string(),
            project_id: self.project_id.as_str().to_string(),
            task_id: self.task_id.as_ref().map(|id| id.as_str().to_string()),
            scope: self.scope,
            title: self.title.clone(),
            cwd: self.cwd.to_string_lossy().into_owned(),
            command: self.command.clone(),
            created_at: self.created_at,
            ended_at: self.ended_at,
            exit_code: self.exit_code,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSnapshot {
    pub id: String,
    pub project_id: String,
    pub task_id: Option<String>,
    pub scope: TerminalScope,
    pub title: String,
    pub cwd: String,
    pub command: String,
    pub created_at: u64,
    pub ended_at: Option<u64>,
    pub exit_code: Option<i32>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalLayout {
    pub revision: u64,
    pub root: Option<TerminalSplit>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SplitAxis {
    Horizontal,
    Vertical,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum TerminalSplit {
    Pane {
        terminal_id: String,
    },
    Split {
        axis: SplitAxis,
        ratio: u16,
        first: Box<TerminalSplit>,
        second: Box<TerminalSplit>,
    },
}

impl TerminalSplit {
    pub fn validate(&self, mut belongs_to_project: impl FnMut(&str) -> bool) -> bool {
        let mut panes = std::collections::BTreeSet::new();
        let mut remaining = vec![(self, 0)];
        while let Some((node, depth)) = remaining.pop() {
            if depth > 20 {
                return false;
            }
            match node {
                Self::Pane { terminal_id } => {
                    if !belongs_to_project(terminal_id)
                        || !panes.insert(terminal_id)
                        || panes.len() > 64
                    {
                        return false;
                    }
                }
                Self::Split {
                    ratio,
                    first,
                    second,
                    ..
                } => {
                    if !(100..=900).contains(ratio) {
                        return false;
                    }
                    remaining.extend([(first.as_ref(), depth + 1), (second.as_ref(), depth + 1)]);
                }
            }
        }
        true
    }
}

#[derive(Clone, Debug)]
pub struct TerminalSpec {
    pub id: TerminalId,
    pub columns: u16,
    pub rows: u16,
    pub cwd: PathBuf,
    pub command: String,
    pub args: Vec<String>,
    pub environment: BTreeMap<String, String>,
}

#[derive(Debug, thiserror::Error)]
pub enum TerminalError {
    #[error("terminal service error: {0}")]
    Backend(String),
}

pub fn validate_size(columns: u16, rows: u16) -> Result<(), TerminalError> {
    if !(2..=512).contains(&columns) || !(1..=256).contains(&rows) {
        return Err(TerminalError::Backend(format!(
            "invalid terminal size {columns}×{rows}"
        )));
    }
    Ok(())
}

/// What a running terminal reports back. Output is raw PTY bytes; the shell
/// parses them, the core never does.
#[derive(Debug)]
pub enum TerminalEvent {
    Output(Vec<u8>),
    Exited { exit_code: Option<i32> },
}

pub type TerminalWrite = oneshot::Receiver<Result<(), TerminalError>>;

pub trait TerminalService: Send {
    fn spawn(
        &mut self,
        spec: TerminalSpec,
        events: UnboundedSender<TerminalEvent>,
    ) -> Result<TerminalId, TerminalError>;
    fn write(
        &mut self,
        terminal_id: &TerminalId,
        bytes: Vec<u8>,
    ) -> Result<TerminalWrite, TerminalError>;
    fn resize(
        &mut self,
        terminal_id: &TerminalId,
        columns: u16,
        rows: u16,
    ) -> Result<(), TerminalError>;
    fn close(&mut self, terminal_id: &TerminalId) -> Result<(), TerminalError>;
}
