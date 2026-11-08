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
pub mod bounded_lines;
pub mod connection;
pub mod fs;
pub mod permissions;
pub mod session_store;
pub mod terminal;
pub mod thread;

#[cfg(any(test, feature = "test-support"))]
pub mod fake_agent;

pub use acp::schema::v1 as schema;

use agent_client_protocol as acp;

pub use agent::{AgentServer, OpenCodeAgent, OpencodeVersion, SpawnSpec, detect_opencode_version};
pub use connection::{
    AcpConnection, AgentInfo, ConnectOptions, Requester, connect, connect_spawn,
    connect_spawn_with_terminals,
};
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
    #[error("{0}")]
    AuthRequired(String),
    #[error("{0}")]
    TransportClosed(String),
    #[error("agent protocol error: {0}")]
    Protocol(String),
    #[error("the agent process exited{code}: {stderr}")]
    Exited { code: String, stderr: String },
}

impl From<acp::Error> for AcpError {
    fn from(error: acp::Error) -> Self {
        if error.code == acp::ErrorCode::AuthRequired {
            AcpError::AuthRequired(error.message)
        } else if acp::is_incoming_transport_closed(&error) {
            AcpError::TransportClosed(error.message)
        } else {
            AcpError::Protocol(error.message)
        }
    }
}

/// Everything the UI learns from a running agent, in one stream so ordering is
/// preserved: notifications, interactive requests, and process-level events.
pub enum AcpEvent {
    /// A `session/update` notification, to be reduced by [`Transcript::apply`].
    SessionUpdate(
        Box<acp::schema::v1::SessionNotification>,
        Option<std::sync::Arc<tokio::sync::OwnedSemaphorePermit>>,
    ),
    /// The agent wants the user to approve a tool call.
    PermissionRequested {
        request_id: u64,
        session_id: schema::SessionId,
        tool_call: ToolCallUpdate,
        options: Vec<PermissionOption>,
        decision: PermissionReply,
    },
    PermissionCancelled {
        request_id: u64,
    },
    /// The client answered a permission request from the project's policy,
    /// without asking.
    PermissionAutoDecided {
        tool_call: ToolCallUpdate,
        choice: thread::PermissionChoice,
        permit: std::sync::Arc<tokio::sync::OwnedSemaphorePermit>,
    },
    TerminalStarted {
        session_id: schema::SessionId,
        terminal_id: String,
        title: String,
    },
    TerminalReleased {
        terminal_id: String,
    },
    TerminalFailed {
        terminal_id: String,
        message: String,
    },
    /// A terminal started by the agent produced output.
    TerminalOutput {
        terminal_id: String,
        chunk: String,
        truncated: bool,
        permit: tokio::sync::OwnedSemaphorePermit,
    },
    /// A terminal started by the agent exited.
    TerminalExited {
        terminal_id: String,
        exit_code: Option<u32>,
        signal: Option<String>,
    },
    /// A line captured from the agent's stderr.
    Stderr(StderrMessage),
    ProtocolFailed {
        message: String,
    },
    /// The agent process exited.
    Exited {
        code: Option<i32>,
        stderr: String,
    },
}

pub struct StderrMessage {
    text: String,
    _permit: Option<tokio::sync::OwnedSemaphorePermit>,
}

impl StderrMessage {
    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub(crate) fn limited(text: String, permit: tokio::sync::OwnedSemaphorePermit) -> Self {
        Self {
            text,
            _permit: Some(permit),
        }
    }
}

impl From<String> for StderrMessage {
    fn from(text: String) -> Self {
        Self {
            text,
            _permit: None,
        }
    }
}

impl std::fmt::Display for StderrMessage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.text, formatter)
    }
}

impl std::fmt::Debug for StderrMessage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.text, formatter)
    }
}

impl std::fmt::Debug for AcpEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SessionUpdate(notification, _) => f
                .debug_tuple("SessionUpdate")
                .field(&notification.update)
                .finish(),
            Self::PermissionRequested {
                tool_call, options, ..
            } => f
                .debug_struct("PermissionRequested")
                .field("tool_call", tool_call)
                .field("options", options)
                .finish(),
            Self::PermissionCancelled { request_id } => f
                .debug_struct("PermissionCancelled")
                .field("request_id", request_id)
                .finish(),
            Self::TerminalOutput {
                terminal_id, chunk, ..
            } => f
                .debug_struct("TerminalOutput")
                .field("terminal_id", terminal_id)
                .field("bytes", &chunk.len())
                .finish(),
            Self::TerminalStarted {
                session_id,
                terminal_id,
                ..
            } => f
                .debug_struct("TerminalStarted")
                .field("session_id", session_id)
                .field("terminal_id", terminal_id)
                .finish(),
            Self::TerminalReleased { terminal_id } => f
                .debug_tuple("TerminalReleased")
                .field(terminal_id)
                .finish(),
            Self::TerminalFailed { terminal_id, .. } => f
                .debug_struct("TerminalFailed")
                .field("terminal_id", terminal_id)
                .finish(),
            Self::TerminalExited {
                terminal_id,
                exit_code,
                ..
            } => f
                .debug_struct("TerminalExited")
                .field("terminal_id", terminal_id)
                .field("exit_code", exit_code)
                .finish(),
            Self::PermissionAutoDecided {
                tool_call, choice, ..
            } => f
                .debug_struct("PermissionAutoDecided")
                .field("tool_call", tool_call)
                .field("choice", choice)
                .finish(),
            Self::Stderr(line) => f.debug_tuple("Stderr").field(line).finish(),
            Self::ProtocolFailed { message } => {
                f.debug_tuple("ProtocolFailed").field(message).finish()
            }
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
