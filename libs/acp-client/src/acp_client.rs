//! ACP (Agent Client Protocol) client.
//!
//! Speaks the stable v1 protocol of `agent-client-protocol` to a coding agent
//! running as a child process: it launches the process in a project directory,
//! owns the session lifecycle, reduces `session/update` notifications into a
//! transcript model, and serves the client-side capabilities the agent may ask
//! for (filesystem reads and writes, terminals, permission prompts).
//!
//! The crate is deliberately independent of the app that embeds it: the agent
//! is chosen through [`agent::AgentServer`], transport concerns live in
//! [`connection`], and UI-facing state is plain data in [`thread`].

pub mod agent;
pub mod connection;
pub mod fs;
pub mod permissions;
pub mod session_store;
pub mod terminal;
pub mod thread;

#[cfg(test)]
mod fake_agent;


pub use acp::schema::v1 as schema;

use agent_client_protocol as acp;

pub use agent::{AgentServer, OpenCodeAgent, SpawnSpec};
pub use connection::{AcpConnection, AgentInfo, ConnectOptions, Requester, connect};
pub use fs::SessionRoots;
pub use permissions::{
    PatternRule, PermissionDecision, PermissionReply, PermissionRule, ToolPermissions, ToolRule,
    Verdict, haystack, tool_key,
};
pub use session_store::{SessionStore, StoredSession};
pub use thread::{
    Activity, AuthMethodRow, Entry, EntryKind, NoticeLevel, PermissionChoice, PermissionRecord,
    PlanRow, ToolStatus, Transcript, TranscriptDelta,
};

use acp::schema::v1::{PermissionOption, ToolCallUpdate};

/// Errors surfaced to the UI.
#[derive(Debug, thiserror::Error)]
pub enum AcpError {
    #[error("the agent executable `{program}` was not found on PATH")]
    AgentNotFound { program: String },
    #[error("no directory in this project exists on disk")]
    NoProjectDirectory,
    #[error("failed to launch the agent: {0}")]
    Launch(String),
    #[error("the agent speaks protocol version {agent}, which this client does not support")]
    UnsupportedVersion { agent: String },
    #[error("agent protocol error: {0}")]
    Protocol(String),
    #[error("the agent process exited{code}: {stderr}")]
    Exited { code: String, stderr: String },
}

impl From<acp::Error> for AcpError {
    fn from(error: acp::Error) -> Self {
        AcpError::Protocol(error.to_string())
    }
}

/// Everything the UI learns from a running agent, in one stream so ordering is
/// preserved: notifications, interactive requests, and process-level events.
pub enum AcpEvent {
    /// A `session/update` notification, to be reduced by [`Transcript::apply`].
    SessionUpdate(Box<acp::schema::v1::SessionNotification>),
    /// The agent wants the user to approve a tool call.
    PermissionRequested {
        tool_call: ToolCallUpdate,
        options: Vec<PermissionOption>,
        decision: PermissionReply,
    },
    /// The client answered a permission request from the project's policy,
    /// without asking.
    PermissionAutoDecided {
        tool_call: ToolCallUpdate,
        choice: thread::PermissionChoice,
    },
    /// A terminal started by the agent produced output.
    TerminalOutput {
        terminal_id: String,
        chunk: String,
        truncated: bool,
    },
    /// A terminal started by the agent exited.
    TerminalExited {
        terminal_id: String,
        exit_code: Option<u32>,
        signal: Option<String>,
    },
    /// A line captured from the agent's stderr.
    Stderr(String),
    /// The agent process exited.
    Exited { code: Option<i32>, stderr: String },
}

impl std::fmt::Debug for AcpEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SessionUpdate(notification) => {
                f.debug_tuple("SessionUpdate").field(&notification.update).finish()
            }
            Self::PermissionRequested { tool_call, options, .. } => f
                .debug_struct("PermissionRequested")
                .field("tool_call", tool_call)
                .field("options", options)
                .finish(),
            Self::TerminalOutput { terminal_id, chunk, .. } => f
                .debug_struct("TerminalOutput")
                .field("terminal_id", terminal_id)
                .field("bytes", &chunk.len())
                .finish(),
            Self::TerminalExited { terminal_id, exit_code, .. } => f
                .debug_struct("TerminalExited")
                .field("terminal_id", terminal_id)
                .field("exit_code", exit_code)
                .finish(),
            Self::PermissionAutoDecided { tool_call, choice } => f
                .debug_struct("PermissionAutoDecided")
                .field("tool_call", tool_call)
                .field("choice", choice)
                .finish(),
            Self::Stderr(line) => f.debug_tuple("Stderr").field(line).finish(),
            Self::Exited { code, stderr } => f
                .debug_struct("Exited")
                .field("code", code)
                .field("stderr", stderr)
                .finish(),
        }
    }
}

/// The directories a session is scoped to, in the order the project lists them.
///
/// The first directory that exists on disk becomes the session `cwd`; every
/// other existing directory is passed as an additional workspace root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionSpec {
    pub candidates: Vec<std::path::PathBuf>,
}

impl SessionSpec {
    pub fn new(candidates: impl IntoIterator<Item = std::path::PathBuf>) -> Self {
        Self {
            candidates: candidates.into_iter().collect(),
        }
    }

    /// Split the candidates into `(cwd, additional roots)`, dropping entries
    /// that do not exist. `None` when no candidate exists.
    pub fn resolve(&self) -> Option<(std::path::PathBuf, Vec<std::path::PathBuf>)> {
        let mut existing = self
            .candidates
            .iter()
            .filter(|path| path.is_dir())
            .map(|path| path.canonicalize().unwrap_or_else(|_| path.clone()));
        let cwd = existing.next()?;
        let additional = existing.collect();
        Some((cwd, additional))
    }
}
