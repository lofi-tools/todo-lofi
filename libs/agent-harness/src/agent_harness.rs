//! The contract between Shepherd's core and a coding-agent adapter.
//!
//! This crate owns the wire-independent event and transcript types. It does not
//! know about Tauri, GPUI, a database, or a particular CLI. Adapters translate
//! their protocol into [`HarnessEvent`] values; the core assembles those events
//! into [`Transcript`] rows and persists settled rows.

pub mod acp;
pub mod codex;
pub mod fake_harness;
#[cfg(any(test, feature = "live-test-support"))]
pub mod live_codex;
pub mod opencode;
pub mod pi;
pub mod settings;

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;

const MAX_COMMAND_OUTPUT_BYTES: usize = 64 * 1024;
const COMMAND_OUTPUT_OMITTED: &str = "[… earlier command output omitted …]\n";

fn limit_command_output(output: &mut String) {
    if output.len() > MAX_COMMAND_OUTPUT_BYTES {
        let start = command_output_tail_start(output);
        output.replace_range(..start, COMMAND_OUTPUT_OMITTED);
    }
}

fn bounded_command_output(output: &str) -> String {
    if output.len() <= MAX_COMMAND_OUTPUT_BYTES {
        return output.to_owned();
    }
    format!(
        "{COMMAND_OUTPUT_OMITTED}{}",
        &output[command_output_tail_start(output)..]
    )
}

fn command_output_tail_start(output: &str) -> usize {
    let start = output.len() - MAX_COMMAND_OUTPUT_BYTES + COMMAND_OUTPUT_OMITTED.len();
    output.ceil_char_boundary(start)
}

/// Errors an adapter may report to the core.
#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    #[error("the harness has not been started")]
    NotStarted,
    #[error("no agent session is open")]
    SessionMissing,
    #[error("the agent is waiting for an approval")]
    ApprovalPending,
    #[error("the current adapter cannot cancel a turn")]
    CancelUnavailable,
    #[error("there is no pending approval")]
    NoPendingApproval,
    #[error("the agent did not offer this approval option")]
    ApprovalOptionUnavailable,
    #[error("This session did not offer the selected mode.")]
    ModeUnavailable,
    #[error("the harness has no scripted turn left")]
    ScriptExhausted,
    #[error("the harness event channel closed")]
    ChannelClosed,
    #[error("the `{program}` program was not found")]
    MissingProgram { program: String },
    #[error("failed to start `{program}`: {source}")]
    Spawn {
        program: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to write {path}: {source}")]
    WriteFile {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("adapter protocol error: {0}")]
    Protocol(String),
    #[error("{message}")]
    Reported { kind: FailureKind, message: String },
    #[error("{0}")]
    TransportClosed(String),
}

pub type HarnessResult<T> = Result<T, HarnessError>;

pub(crate) fn protect_discovery_error(error: HarnessError, hide: bool) -> HarnessError {
    if hide {
        HarnessError::Reported {
            kind: FailureKind::classify(&error).unwrap_or(FailureKind::Protocol),
            message: "Agent discovery details are hidden because launch environment values are configured.".into(),
        }
    } else {
        error
    }
}

/// Why a session failed, in terms the UI can label without parsing an adapter's
/// prose. An older server reports an unsupported build or a missing sign-in as
/// a JSON-RPC error, so classification reads the message it sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FailureKind {
    MissingBinary,
    UnsupportedVersion,
    AuthRequired,
    AgentRejected,
    Protocol,
    MidTurnExit,
}

impl FailureKind {
    /// The failure a reported error belongs to, or `None` for an error that is
    /// about the caller's own state (an unopened session, a pending approval).
    pub fn classify(error: &HarnessError) -> Option<Self> {
        match error {
            HarnessError::MissingProgram { .. } => Some(Self::MissingBinary),
            HarnessError::Spawn { source, .. } if source.kind() == std::io::ErrorKind::NotFound => {
                Some(Self::MissingBinary)
            }
            HarnessError::Reported { kind, .. } => Some(*kind),
            HarnessError::Protocol(message) => Some(Self::from_message(message, Self::Protocol)),
            HarnessError::Spawn { .. }
            | HarnessError::WriteFile { .. }
            | HarnessError::TransportClosed(_)
            | HarnessError::ChannelClosed => Some(Self::Protocol),
            _ => None,
        }
    }

    pub(crate) fn from_message(message: &str, fallback: Self) -> Self {
        let message = message.to_lowercase();
        let says_one = |markers: &[&str]| markers.iter().any(|marker| message.contains(marker));
        if says_one(&[
            "unauthorized",
            "unauthenticated",
            "401 unauthorized",
            "login",
            "log in",
            "sign in",
            "sign-in",
            "authentication",
            "api key",
            "credential",
        ]) {
            Self::AuthRequired
        } else if says_one(&[
            "unsupported",
            "does not support",
            "unknown method",
            "method not found",
            "unknown command",
            "unexpected argument",
        ]) {
            Self::UnsupportedVersion
        } else {
            fallback
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::MissingBinary => "agent not installed",
            Self::UnsupportedVersion => "version not supported",
            Self::AuthRequired => "sign-in required",
            Self::AgentRejected => "agent rejected the turn",
            Self::Protocol => "protocol error",
            Self::MidTurnExit => "agent exited mid-turn",
        }
    }

    /// What the user can do about it, shown under the cause.
    pub fn hint(self) -> &'static str {
        match self {
            Self::MissingBinary => {
                "Install the agent or correct its configured executable path, then retry."
            }
            Self::UnsupportedVersion => {
                "Update the agent; this build does not speak the protocol Shepherd needs."
            }
            Self::AuthRequired => "Sign in to the agent, then retry.",
            Self::AgentRejected => "Check the agent's message before retrying.",
            Self::Protocol => "The agent's response could not be used; retry the turn.",
            Self::MidTurnExit => {
                "The turn stopped before it finished; resume the task to continue."
            }
        }
    }
}

/// A task's agent session identifier. The adapter chooses the value; the core
/// persists it and passes it back to `open_session` for resume.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SessionId(String);

impl SessionId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A canonical transcript item identifier. Adapters use their own IDs inside
/// this value; the core maps it to one row.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ItemId(String);

impl ItemId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ItemId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A tool-call identifier supplied by the adapter.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ToolCallId(String);

impl ToolCallId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ToolCallId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// An approval request identifier supplied by the adapter.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ApprovalId(String);

impl ApprovalId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ApprovalId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A turn identifier supplied by the adapter.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TurnId(String);

impl TurnId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What an adapter needs to launch its child process.
#[derive(Clone, Default)]
pub struct SpawnSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub environment: BTreeMap<String, String>,
}

impl std::fmt::Debug for SpawnSpec {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SpawnSpec")
            .field("program", &self.program)
            .field("args", &self.args)
            .field("cwd", &self.cwd)
            .field(
                "environment_keys",
                &self.environment.keys().collect::<Vec<_>>(),
            )
            .finish()
    }
}

/// The exact launch and protocol an adapter used, for Task → Diagnostics. The
/// core shows these verbatim, so nothing secret belongs in them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentDiagnostics {
    /// The program and arguments, as launched.
    pub command: String,
    /// The protocol surface and version the adapter verified.
    pub protocol: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// The child's stderr, newest last.
    pub stderr_tail: String,
}

/// The command line as diagnostics show it. It is display text, not a shell
/// command, so arguments are joined without quoting.
pub fn command_line(spec: &SpawnSpec) -> String {
    std::iter::once(spec.program.as_str())
        .chain(spec.args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The directories an agent session may touch. The first root is the primary
/// working directory.
#[derive(Clone, Debug, Default)]
pub struct SessionRoots {
    pub roots: Vec<PathBuf>,
}

impl SessionRoots {
    pub fn new(roots: impl IntoIterator<Item = PathBuf>) -> Self {
        Self {
            roots: roots.into_iter().collect(),
        }
    }

    pub fn primary(&self) -> Option<&PathBuf> {
        self.roots.first()
    }
}

/// What an adapter can do. False means the capability is absent, not merely
/// untested; the core must hide the corresponding control.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Capabilities {
    pub streaming: bool,
    pub thoughts: bool,
    pub tool_calls: bool,
    pub approvals: bool,
    /// Tool calls run without Shepherd deciding them, so the UI must say so
    /// instead of offering an approval that the adapter cannot produce.
    pub unreviewed_tools: bool,
    pub resume: bool,
    pub cancel: bool,
    pub model_picker: bool,
    pub mode_picker: bool,
    pub images: bool,
    pub resource_links: bool,
    pub usage: bool,
    pub agent_terminals: bool,
    pub patches_in_approvals: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentModes {
    pub current: Option<String>,
    pub options: Vec<AgentMode>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentMode {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AdapterTier {
    #[default]
    Native,
    Acp,
}

impl Capabilities {
    /// The capability set of the deterministic test harness.
    pub fn fake() -> Self {
        Self {
            streaming: true,
            thoughts: true,
            tool_calls: true,
            approvals: true,
            resume: true,
            cancel: true,
            usage: true,
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PromptInput {
    pub text: String,
    pub attachments: Vec<PromptAttachment>,
    pub codex: CodexTurnOptions,
    pub mode: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CodexTurnOptions {
    pub model: Option<String>,
    pub effort: Option<String>,
}

impl PromptInput {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PromptAttachment {
    pub path: PathBuf,
    pub image_mime_type: Option<String>,
    pub metadata: Option<AttachmentMetadata>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentMetadata {
    pub id: String,
    pub relative_path: PathBuf,
    pub mime: String,
    pub bytes: u64,
}

impl PromptAttachment {
    fn file_uri(&self) -> HarnessResult<reqwest::Url> {
        reqwest::Url::from_file_path(&self.path).map_err(|()| {
            HarnessError::Protocol(format!(
                "could not make an attachment URI for {}",
                self.path.display()
            ))
        })
    }
}

/// A user or policy decision on an approval request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Decision {
    Allow,
    AllowSession,
    AllowProject,
    Deny,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ApprovalSource {
    User,
    Agent,
    SessionGrant,
    ProjectGrant,
}

/// The result of one turn, as reported by the adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TurnFinishReason {
    Completed,
    Cancelled,
    Failed,
    Interrupted,
}

/// A state an adapter can report. The core owns task states; this is the
/// session-level view the adapter knows about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionState {
    Starting,
    Ready,
    Working,
    Waiting,
    Exited,
}

/// A streamed message or thought.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeltaKind {
    Message,
    Thought,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolStatus {
    #[default]
    Pending,
    Running,
    Completed,
    Failed,
    Stopped,
}

/// A file change reported by a tool call or an approval request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileDiff {
    pub path: String,
    pub old_text: Option<String>,
    pub new_text: String,
}

/// One row of an agent plan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanRow {
    pub text: String,
    pub status: String,
}

/// An approval request. `paths` and `patch` may be absent when the adapter
/// cannot provide them.
#[derive(Clone, Debug)]
pub enum ApprovalCapacity {
    Acp(Arc<acp_client::permissions::PermissionCapacity>),
    Stream(Arc<tokio::sync::OwnedSemaphorePermit>),
}

#[derive(Clone, Debug)]
pub struct ApprovalRequest {
    pub approval_id: ApprovalId,
    pub tool_call_id: ToolCallId,
    pub title: String,
    pub arguments: Value,
    pub paths: Vec<PathBuf>,
    pub patch: Option<FileDiff>,
    pub choices: Option<Vec<Decision>>,
    /// Retains request capacity until the queued approval has been consumed.
    pub stream_permit: Option<ApprovalCapacity>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionApproval {
    pub id: String,
    pub title: String,
    pub arguments: Value,
    pub paths: Vec<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTerminal {
    pub id: String,
    pub title: String,
    pub live: bool,
    pub exit_code: Option<u32>,
}

#[allow(
    clippy::double_must_use,
    reason = "async_trait generates must-use futures"
)]
#[async_trait]
pub trait ProjectApprovalPolicy: Send + Sync {
    async fn allows(&self, key: &str) -> Result<bool, String>;
    async fn remember(&self, key: &str, request: &ApprovalRequest) -> Result<(), String>;
}

/// Environment variable carrying the launch token. Every descendant of a
/// launched agent inherits it, so a surviving member can be attributed to its
/// launch after the leader's PID is gone.
pub const PROCESS_TOKEN_ENV: &str = "SHEPHERD_PROCESS_TOKEN";

#[allow(
    clippy::double_must_use,
    reason = "async_trait generates must-use futures"
)]
#[async_trait]

pub trait AgentProcessTracker: Send + Sync {
    async fn started(&self, pid: u32) -> Result<String, String>;
    async fn stopped(&self, record_id: &str) -> Result<(), String>;
}

/// Everything an adapter reports to the core, in one ordered stream.
#[derive(Clone, Debug)]
pub enum HarnessEvent {
    SessionState {
        state: SessionState,
    },
    TurnStarted {
        turn_id: TurnId,
    },
    Delta {
        item_id: ItemId,
        kind: DeltaKind,
        text: String,
        /// Retains the stream buffer permit while this event is in use.
        stream_permit: Option<Arc<tokio::sync::OwnedSemaphorePermit>>,
    },
    ToolCall {
        tool_call_id: ToolCallId,
        title: String,
        kind: String,
        status: ToolStatus,
        output: Option<String>,
        diffs: Vec<FileDiff>,
        terminal_id: Option<String>,
        /// Retains backpressure while the event is queued or in use.
        stream_permit: Option<Arc<tokio::sync::OwnedSemaphorePermit>>,
    },
    CommandOutput {
        tool_call_id: ToolCallId,
        text: String,
        /// Retains backpressure until all copies of this output event are consumed.
        output_permit: Option<Arc<tokio::sync::OwnedSemaphorePermit>>,
    },
    AgentTerminalStarted {
        id: String,
        title: String,
        stream_permit: Option<Arc<tokio::sync::OwnedSemaphorePermit>>,
    },
    AgentTerminalExited {
        id: String,
        exit_code: Option<u32>,
        stream_permit: Option<Arc<tokio::sync::OwnedSemaphorePermit>>,
    },
    AgentTerminalError {
        id: String,
        text: String,
        stream_permit: Option<Arc<tokio::sync::OwnedSemaphorePermit>>,
    },
    AgentTerminalRemoved {
        id: String,
        stream_permit: Option<Arc<tokio::sync::OwnedSemaphorePermit>>,
    },
    Plan {
        rows: Vec<PlanRow>,
        stream_permit: Option<Arc<tokio::sync::OwnedSemaphorePermit>>,
    },
    Usage {
        used: u64,
        size: u64,
        cost: Option<f64>,
        stream_permit: Option<Arc<tokio::sync::OwnedSemaphorePermit>>,
    },
    ApprovalRequested {
        request: ApprovalRequest,
    },
    ApprovalResolved {
        approval_id: ApprovalId,
        decision: Decision,
        source: ApprovalSource,
    },
    ApprovalAutomaticallyResolved {
        request: ApprovalRequest,
        decision: Decision,
        source: ApprovalSource,
    },
    ApprovalWithdrawn {
        approval_id: ApprovalId,
    },
    ApprovalPolicyError {
        text: String,
    },
    ModeChanged {
        stream_permit: Option<Arc<tokio::sync::OwnedSemaphorePermit>>,
    },
    TurnError {
        kind: FailureKind,
        text: String,
        stream_permit: Option<Arc<tokio::sync::OwnedSemaphorePermit>>,
    },
    ConnectionFailed {
        kind: FailureKind,
        text: String,
    },
    TurnFinished {
        reason: TurnFinishReason,
    },
    Exited {
        code: Option<i32>,
        stderr: String,
    },
}

/// One settled or streaming transcript row.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    /// Canonical row ID assigned by the transcript, never by the adapter.
    pub id: String,
    /// Stable per-session order.
    pub sequence: u64,
    pub kind: EntryKind,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<AttachmentMetadata>,
    /// The adapter item this row came from, when it has one. Persisted with the
    /// row so a restored transcript re-attaches later updates to it instead of
    /// appending a duplicate.
    #[serde(default)]
    pub source_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    content = "data",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum EntryKind {
    UserMessage {
        text: String,
    },
    AgentText {
        text: String,
    },
    Thought {
        text: String,
    },
    ToolCall {
        tool_call_id: String,
        title: String,
        kind: String,
        status: ToolStatus,
        output: Option<String>,
        diffs: Vec<FileDiff>,
        terminal_id: Option<String>,
    },
    Plan {
        rows: Vec<PlanRow>,
    },
    Permission {
        approval_id: String,
        title: String,
        arguments: Value,
        paths: Vec<String>,
        patch: Option<FileDiff>,
        decision: Option<Decision>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        choices: Option<Vec<Decision>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<ApprovalSource>,
        #[serde(default)]
        withdrawn: bool,
    },
    Notice {
        text: String,
        is_error: bool,
    },
    Failure {
        kind: FailureKind,
        text: String,
        detail: Option<String>,
    },
    Usage {
        used: u64,
        size: u64,
        cost: Option<f64>,
    },
}

impl Entry {
    /// A short label for tests and the accessibility tree.
    pub fn summary(&self) -> String {
        match &self.kind {
            EntryKind::UserMessage { text } => format!("you: {text}"),
            EntryKind::AgentText { text } => format!("agent: {text}"),
            EntryKind::Thought { text } => format!("thought: {text}"),
            EntryKind::ToolCall { title, status, .. } => format!("{title} ({status:?})"),
            EntryKind::Plan { rows } => format!("plan ({} rows)", rows.len()),
            EntryKind::Permission {
                title,
                decision,
                withdrawn,
                ..
            } => match decision {
                Some(decision) => format!("approval: {title} ({decision:?})"),
                None if *withdrawn => format!("approval: {title} (withdrawn)"),
                None => format!("approval: {title} (pending)"),
            },
            EntryKind::Notice { text, is_error } => {
                if *is_error {
                    format!("error: {text}")
                } else {
                    format!("notice: {text}")
                }
            }
            EntryKind::Failure { kind, text, .. } => {
                format!("failure: {}: {text}", kind.label())
            }
            EntryKind::Usage { used, size, .. } => format!("usage: {used}/{size}"),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TranscriptChange {
    pub appended: bool,
    pub changed: Option<usize>,
}

impl TranscriptChange {
    pub fn is_empty(&self) -> bool {
        !self.appended && self.changed.is_none()
    }
}

/// The core-owned transcript. Adapter item IDs are mapped to row indexes in
/// memory, so an out-of-order update still lands on the original row.
#[derive(Debug, Default)]
pub struct Transcript {
    entries: Vec<Entry>,
    item_rows: BTreeMap<String, usize>,
    tool_rows: BTreeMap<String, usize>,
    approval_rows: BTreeMap<String, usize>,
    next_sequence: u64,
}

impl Transcript {
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entry(&self, index: usize) -> Option<&Entry> {
        self.entries.get(index)
    }

    pub fn reset(&mut self) {
        self.entries.clear();
        self.clear_adapter_row_keys();
        self.next_sequence = 0;
    }

    pub fn clear_adapter_row_keys(&mut self) {
        self.item_rows.clear();
        self.tool_rows.clear();
        self.approval_rows.clear();
    }

    pub fn clear_streaming_row_keys(&mut self) {
        self.item_rows
            .retain(|key, _| key == "plan" || key == "usage");
    }

    /// Rebuild from persisted rows. Canonical IDs, sequence numbers and adapter
    /// keys come back, so order holds after a reconnect and an update for an
    /// already-seen adapter item lands on its original row.
    pub fn restore(&mut self, entries: impl IntoIterator<Item = Entry>) {
        self.reset();
        let mut entries = entries.into_iter().collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.sequence);

        for entry in entries {
            let index = self.entries.len();
            match &entry.kind {
                EntryKind::AgentText { .. } => {
                    if let Some(source) = entry.source_id.as_ref() {
                        let key = format!("{:?}:{source}", DeltaKind::Message);
                        self.item_rows.insert(key, index);
                    }
                }
                EntryKind::Thought { .. } => {
                    if let Some(source) = entry.source_id.as_ref() {
                        let key = format!("{:?}:{source}", DeltaKind::Thought);
                        self.item_rows.insert(key, index);
                    }
                }
                EntryKind::ToolCall { tool_call_id, .. } => {
                    self.tool_rows.insert(tool_call_id.clone(), index);
                }
                EntryKind::Permission { approval_id, .. } => {
                    self.approval_rows.insert(approval_id.clone(), index);
                }
                EntryKind::Plan { .. } => {
                    self.item_rows.insert("plan".to_string(), index);
                }
                EntryKind::Usage { .. } => {
                    self.item_rows.insert("usage".to_string(), index);
                }
                EntryKind::UserMessage { .. }
                | EntryKind::Notice { .. }
                | EntryKind::Failure { .. } => {}
            }
            self.next_sequence = self.next_sequence.max(entry.sequence + 1);
            self.entries.push(entry);
        }
    }

    /// Record the user's prompt. Agents normally echo it back; the transcript
    /// deliberately ignores those echoes.
    pub fn push_user_message(&mut self, text: impl Into<String>) -> TranscriptChange {
        self.push_user_message_with_attachments(text, Vec::new())
    }

    pub fn push_user_message_with_attachments(
        &mut self,
        text: impl Into<String>,
        attachments: Vec<AttachmentMetadata>,
    ) -> TranscriptChange {
        self.append_entry(
            EntryKind::UserMessage { text: text.into() },
            None,
            attachments,
        )
    }

    pub fn push_notice(&mut self, text: impl Into<String>, is_error: bool) -> TranscriptChange {
        self.append(EntryKind::Notice {
            text: text.into(),
            is_error,
        })
    }

    /// Record a failure the core diagnosed. Unlike a notice, a failure carries
    /// a kind, so the row can be labelled and acted on without parsing text.
    pub fn push_failure(
        &mut self,
        kind: FailureKind,
        text: impl Into<String>,
        detail: Option<String>,
    ) -> TranscriptChange {
        self.append(EntryKind::Failure {
            kind,
            text: text.into(),
            detail,
        })
    }

    /// Fold one adapter event into the transcript. Events that are not
    /// transcript rows leave it untouched.
    pub fn apply(&mut self, event: &HarnessEvent) -> TranscriptChange {
        match event {
            HarnessEvent::Delta {
                item_id,
                kind,
                text,
                ..
            } => self.push_delta(item_id.as_str(), *kind, text),
            HarnessEvent::ToolCall {
                tool_call_id,
                title,
                kind,
                status,
                output,
                diffs,
                terminal_id,
                ..
            } => self.upsert_tool_call(
                tool_call_id.as_str(),
                title,
                kind,
                *status,
                output.clone(),
                diffs.clone(),
                terminal_id.clone(),
            ),
            HarnessEvent::CommandOutput {
                tool_call_id, text, ..
            } => self.append_command_output(tool_call_id.as_str(), text),
            HarnessEvent::TurnFinished { .. } | HarnessEvent::Exited { .. } => {
                self.stop_unfinished_commands()
            }
            HarnessEvent::Plan { rows, .. } => self.upsert_plan(rows.clone()),
            HarnessEvent::Usage {
                used, size, cost, ..
            } => self.upsert_usage(*used, *size, *cost),
            HarnessEvent::ApprovalRequested { request } => {
                self.push_approval(request.clone(), None)
            }
            HarnessEvent::ApprovalResolved {
                approval_id,
                decision,
                source,
            } => self.settle_approval(approval_id.as_str(), Some((*decision, *source))),
            HarnessEvent::ApprovalAutomaticallyResolved {
                request,
                decision,
                source,
            } => self.push_approval(request.clone(), Some((*decision, *source))),
            HarnessEvent::ApprovalWithdrawn { approval_id } => {
                self.settle_approval(approval_id.as_str(), None)
            }
            HarnessEvent::ApprovalPolicyError { text }
            | HarnessEvent::AgentTerminalError { text, .. } => self.push_notice(text, true),
            HarnessEvent::TurnError { kind, text, .. }
            | HarnessEvent::ConnectionFailed { kind, text } => {
                if matches!(event, HarnessEvent::ConnectionFailed { .. }) {
                    self.stop_unfinished_commands();
                }
                self.push_failure(*kind, text.clone(), Some(kind.hint().to_string()))
            }
            _ => TranscriptChange::default(),
        }
    }

    fn push_delta(&mut self, item_id: &str, kind: DeltaKind, text: &str) -> TranscriptChange {
        let key = format!("{kind:?}:{item_id}");
        if let Some(index) = self.item_rows.get(&key).copied() {
            if let Some(entry) = self.entries.get_mut(index) {
                match (&mut entry.kind, kind) {
                    (EntryKind::AgentText { text: existing }, DeltaKind::Message)
                    | (EntryKind::Thought { text: existing }, DeltaKind::Thought) => {
                        existing.push_str(text);
                    }
                    _ => {
                        entry.kind = match kind {
                            DeltaKind::Message => EntryKind::AgentText {
                                text: text.to_string(),
                            },
                            DeltaKind::Thought => EntryKind::Thought {
                                text: text.to_string(),
                            },
                        };
                    }
                }
            }
            return TranscriptChange {
                appended: false,
                changed: Some(index),
            };
        }

        let entry_kind = match kind {
            DeltaKind::Message => EntryKind::AgentText {
                text: text.to_string(),
            },
            DeltaKind::Thought => EntryKind::Thought {
                text: text.to_string(),
            },
        };
        let change = self.append_from(entry_kind, Some(item_id.to_string()));
        if let Some(index) = change.changed {
            self.item_rows.insert(key, index);
        }
        change
    }

    fn append_command_output(&mut self, tool_call_id: &str, text: &str) -> TranscriptChange {
        let Some(index) = self.tool_rows.get(tool_call_id).copied() else {
            return TranscriptChange::default();
        };
        let Some(EntryKind::ToolCall {
            kind,
            status: ToolStatus::Pending | ToolStatus::Running,
            output,
            ..
        }) = self.entries.get_mut(index).map(|entry| &mut entry.kind)
        else {
            return TranscriptChange::default();
        };
        if kind != "command" || text.is_empty() {
            return TranscriptChange::default();
        }
        let output = output.get_or_insert_with(String::new);
        if text.len() > MAX_COMMAND_OUTPUT_BYTES {
            *output = bounded_command_output(text);
        } else {
            output.push_str(text);
            limit_command_output(output);
        }
        TranscriptChange {
            appended: false,
            changed: Some(index),
        }
    }

    fn stop_unfinished_commands(&mut self) -> TranscriptChange {
        let mut change = TranscriptChange::default();
        for (index, entry) in self.entries.iter_mut().enumerate() {
            if let EntryKind::ToolCall { kind, status, .. } = &mut entry.kind
                && kind == "command"
                && matches!(status, ToolStatus::Pending | ToolStatus::Running)
            {
                *status = ToolStatus::Stopped;
                change.changed.get_or_insert(index);
            }
        }
        change
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the fields belong to one tool-call event"
    )]
    fn upsert_tool_call(
        &mut self,
        tool_call_id: &str,
        title: &str,
        kind: &str,
        status: ToolStatus,
        mut output: Option<String>,
        diffs: Vec<FileDiff>,
        terminal_id: Option<String>,
    ) -> TranscriptChange {
        if kind == "command"
            && let Some(output) = &mut output
        {
            limit_command_output(output);
        }
        if let Some(index) = self.tool_rows.get(tool_call_id).copied() {
            if let Some(entry) = self.entries.get_mut(index) {
                if kind == "command"
                    && output.is_none()
                    && let EntryKind::ToolCall {
                        kind: previous_kind,
                        output: previous_output,
                        ..
                    } = &mut entry.kind
                    && previous_kind == "command"
                {
                    output = previous_output.take();
                }
                entry.kind = EntryKind::ToolCall {
                    tool_call_id: tool_call_id.to_string(),
                    title: title.to_string(),
                    kind: kind.to_string(),
                    status,
                    output,
                    diffs,
                    terminal_id,
                };
            }
            return TranscriptChange {
                appended: false,
                changed: Some(index),
            };
        }

        let change = self.append_from(
            EntryKind::ToolCall {
                tool_call_id: tool_call_id.to_string(),
                title: title.to_string(),
                kind: kind.to_string(),
                status,
                output,
                diffs,
                terminal_id,
            },
            Some(tool_call_id.to_string()),
        );
        if let Some(index) = change.changed {
            self.tool_rows.insert(tool_call_id.to_string(), index);
        }
        change
    }

    fn upsert_plan(&mut self, rows: Vec<PlanRow>) -> TranscriptChange {
        let key = "plan";
        if let Some(index) = self.item_rows.get(key).copied() {
            if let Some(entry) = self.entries.get_mut(index) {
                entry.kind = EntryKind::Plan { rows };
            }
            return TranscriptChange {
                appended: false,
                changed: Some(index),
            };
        }
        let change = self.append(EntryKind::Plan { rows });
        if let Some(index) = change.changed {
            self.item_rows.insert(key.to_string(), index);
        }
        change
    }

    fn upsert_usage(&mut self, used: u64, size: u64, cost: Option<f64>) -> TranscriptChange {
        let key = "usage";
        if let Some(index) = self.item_rows.get(key).copied() {
            if let Some(entry) = self.entries.get_mut(index) {
                entry.kind = EntryKind::Usage { used, size, cost };
            }
            return TranscriptChange {
                appended: false,
                changed: Some(index),
            };
        }
        let change = self.append(EntryKind::Usage { used, size, cost });
        if let Some(index) = change.changed {
            self.item_rows.insert(key.to_string(), index);
        }
        change
    }

    fn push_approval(
        &mut self,
        request: ApprovalRequest,
        resolution: Option<(Decision, ApprovalSource)>,
    ) -> TranscriptChange {
        let key = request.approval_id.as_str().to_string();
        let kind = EntryKind::Permission {
            approval_id: key.clone(),
            title: request.title,
            arguments: request.arguments,
            paths: request
                .paths
                .iter()
                .map(|path| path.display().to_string())
                .collect(),
            patch: request.patch,
            choices: request.choices,
            decision: resolution.map(|(decision, _)| decision),
            source: resolution.map(|(_, source)| source),
            withdrawn: false,
        };
        if let Some(index) = self.approval_rows.get(&key).copied() {
            if let Some(entry) = self.entries.get_mut(index) {
                entry.kind = kind;
            }
            return TranscriptChange {
                appended: false,
                changed: Some(index),
            };
        }

        let change = self.append_from(kind, Some(key.clone()));
        if let Some(index) = change.changed {
            self.approval_rows.insert(key, index);
        }
        change
    }

    fn settle_approval(
        &mut self,
        approval_id: &str,
        resolution: Option<(Decision, ApprovalSource)>,
    ) -> TranscriptChange {
        let Some(index) = self.approval_rows.get(approval_id).copied() else {
            return TranscriptChange::default();
        };
        if let Some(entry) = self.entries.get_mut(index)
            && let EntryKind::Permission {
                decision: slot,
                source,
                withdrawn,
                ..
            } = &mut entry.kind
            && (resolution.is_some() || slot.is_none())
        {
            *slot = resolution.map(|(decision, _)| decision);
            *source = resolution.map(|(_, source)| source);
            *withdrawn = resolution.is_none();
        }
        TranscriptChange {
            appended: false,
            changed: Some(index),
        }
    }

    fn append(&mut self, kind: EntryKind) -> TranscriptChange {
        self.append_from(kind, None)
    }

    fn append_from(&mut self, kind: EntryKind, source_id: Option<String>) -> TranscriptChange {
        self.append_entry(kind, source_id, Vec::new())
    }

    fn append_entry(
        &mut self,
        kind: EntryKind,
        source_id: Option<String>,
        attachments: Vec<AttachmentMetadata>,
    ) -> TranscriptChange {
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        let index = self.entries.len();
        self.entries.push(Entry {
            id: format!("entry-{sequence}"),
            sequence,
            kind,
            attachments,
            source_id,
        });
        TranscriptChange {
            appended: true,
            changed: Some(index),
        }
    }
}

/// The adapter contract. The core drives it; adapters never write to the store.
#[allow(
    clippy::double_must_use,
    reason = "async_trait generates must-use futures"
)]
#[async_trait]
pub trait AgentHarness: Send {
    /// Receive adapter events. The receiver is owned by the harness and must
    /// remain available for the whole session.
    fn events(&mut self) -> &mut mpsc::UnboundedReceiver<HarnessEvent>;

    /// What this session launched and which protocol it speaks. The default is
    /// empty, so a test adapter need not invent values.
    fn diagnostics(&self) -> AgentDiagnostics {
        AgentDiagnostics::default()
    }

    fn tier(&self) -> AdapterTier {
        AdapterTier::Native
    }

    fn modes(&self) -> Option<AgentModes> {
        None
    }

    async fn set_mode(&mut self, _mode: &str) -> HarnessResult<()> {
        Err(HarnessError::ModeUnavailable)
    }

    fn set_project_approval_policy(&mut self, _policy: Arc<dyn ProjectApprovalPolicy>) {}

    fn set_process_tracker(&mut self, _tracker: Arc<dyn AgentProcessTracker>) {}

    fn session_approvals(&self) -> Vec<SessionApproval> {
        Vec::new()
    }

    async fn reset_session_approval(&mut self, _id: &str) -> HarnessResult<()> {
        Ok(())
    }

    /// History returned while opening a persisted agent session.
    fn take_replayed_transcript(&mut self) -> Vec<Entry> {
        Vec::new()
    }

    async fn start(&mut self, spec: SpawnSpec) -> HarnessResult<Capabilities>;

    async fn open_session(&mut self, resume: Option<SessionId>) -> HarnessResult<SessionId>;

    async fn prompt(&mut self, input: PromptInput) -> HarnessResult<()>;

    async fn cancel(&mut self) -> HarnessResult<()>;

    async fn respond(&mut self, approval_id: ApprovalId, decision: Decision) -> HarnessResult<()>;

    async fn shutdown(&mut self) -> HarnessResult<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_spec_debug_does_not_expose_environment_values() {
        let spec = SpawnSpec {
            program: "agent".into(),
            environment: BTreeMap::from([("TOKEN".into(), "fixture-private-value".into())]),
            ..SpawnSpec::default()
        };
        let formatted = format!("{spec:?}");
        assert!(formatted.contains("agent"));
        assert!(formatted.contains("TOKEN"));
        assert!(!formatted.contains("fixture-private-value"));
    }

    fn tool_event(title: &str, status: ToolStatus) -> HarnessEvent {
        HarnessEvent::ToolCall {
            tool_call_id: ToolCallId::new("tool-1"),
            title: title.to_string(),
            kind: "edit".to_string(),
            status,
            output: None,
            diffs: Vec::new(),
            terminal_id: None,
            stream_permit: None,
        }
    }

    #[test]
    fn delta_chunks_grow_one_message_row() {
        let mut transcript = Transcript::default();
        transcript.apply(&HarnessEvent::Delta {
            item_id: ItemId::new("message-1"),
            kind: DeltaKind::Message,
            text: "Hello ".to_string(),
            stream_permit: None,
        });
        let second = transcript.apply(&HarnessEvent::Delta {
            item_id: ItemId::new("message-1"),
            kind: DeltaKind::Message,
            text: "world".to_string(),
            stream_permit: None,
        });

        assert!(!second.appended);
        assert_eq!(transcript.len(), 1);
        assert_eq!(
            transcript.entry(0).map(Entry::summary),
            Some("agent: Hello world".to_string())
        );
    }

    #[test]
    fn an_out_of_order_tool_update_keeps_one_row() {
        let mut transcript = Transcript::default();
        transcript.apply(&tool_event("Edit file", ToolStatus::Running));
        let update = transcript.apply(&tool_event("Edit file", ToolStatus::Completed));

        assert!(!update.appended);
        assert_eq!(transcript.len(), 1);
        assert_eq!(
            transcript.entry(0).map(Entry::summary),
            Some("Edit file (Completed)".to_string())
        );
    }

    #[test]
    fn command_output_keeps_a_utf8_tail_when_the_turn_stops() {
        let mut transcript = Transcript::default();
        let call = HarnessEvent::ToolCall {
            tool_call_id: ToolCallId::new("command-1"),
            title: "run".to_string(),
            kind: "command".to_string(),
            status: ToolStatus::Running,
            output: None,
            diffs: Vec::new(),
            terminal_id: None,
            stream_permit: None,
        };
        transcript.apply(&call);
        transcript.apply(&HarnessEvent::CommandOutput {
            tool_call_id: ToolCallId::new("command-1"),
            text: "界".repeat(MAX_COMMAND_OUTPUT_BYTES),
            output_permit: None,
        });
        transcript.apply(&HarnessEvent::CommandOutput {
            tool_call_id: ToolCallId::new("command-1"),
            text: "\nlast line\n".to_string(),
            output_permit: None,
        });
        transcript.apply(&HarnessEvent::TurnFinished {
            reason: TurnFinishReason::Cancelled,
        });
        assert!(
            transcript
                .apply(&HarnessEvent::CommandOutput {
                    tool_call_id: ToolCallId::new("command-1"),
                    text: "late output".to_string(),
                    output_permit: None,
                })
                .is_empty()
        );

        assert!(matches!(
            transcript.entry(0).map(|entry| &entry.kind),
            Some(EntryKind::ToolCall { status: ToolStatus::Stopped, output: Some(output), .. })
                if output.len() <= MAX_COMMAND_OUTPUT_BYTES
                    && output.starts_with(COMMAND_OUTPUT_OMITTED)
                    && output.ends_with("\nlast line\n")
        ));
    }

    #[test]
    fn command_output_oversized_chunk_retains_tail_without_a_large_buffer()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut transcript = Transcript::default();
        transcript.apply(&HarnessEvent::ToolCall {
            tool_call_id: ToolCallId::new("command-1"),
            title: "run".to_string(),
            kind: "command".to_string(),
            status: ToolStatus::Running,
            output: Some("earlier output".to_string()),
            diffs: Vec::new(),
            terminal_id: None,
            stream_permit: None,
        });
        let text = format!("{}tail", "界".repeat(MAX_COMMAND_OUTPUT_BYTES));
        let mut expected = format!("earlier output{text}");
        limit_command_output(&mut expected);
        transcript.apply(&HarnessEvent::CommandOutput {
            tool_call_id: ToolCallId::new("command-1"),
            text,
            output_permit: None,
        });
        let Some(EntryKind::ToolCall {
            output: Some(output),
            ..
        }) = transcript.entry(0).map(|entry| &entry.kind)
        else {
            return Err("missing command output".into());
        };
        assert_eq!(output, &expected);
        assert!(output.capacity() <= MAX_COMMAND_OUTPUT_BYTES * 2);
        Ok(())
    }

    #[test]
    fn a_restored_transcript_keeps_order_and_continues_numbering() {
        let mut transcript = Transcript::default();
        transcript.push_user_message("go");
        transcript.apply(&HarnessEvent::Delta {
            item_id: ItemId::new("message-1"),
            kind: DeltaKind::Message,
            text: "hello".to_string(),
            stream_permit: None,
        });
        transcript.apply(&tool_event("Edit file", ToolStatus::Completed));

        let mut restored = Transcript::default();
        restored.restore(transcript.entries().to_vec());

        assert_eq!(restored.len(), 3);
        let sequences = restored
            .entries()
            .iter()
            .map(|entry| entry.sequence)
            .collect::<Vec<_>>();
        assert_eq!(sequences, vec![0, 1, 2]);
        assert_eq!(
            restored.entry(0).map(|entry| entry.id.clone()),
            Some("entry-0".to_string())
        );

        let change = restored.apply(&HarnessEvent::Delta {
            item_id: ItemId::new("message-1"),
            kind: DeltaKind::Message,
            text: " again".to_string(),
            stream_permit: None,
        });
        assert!(!change.appended, "a restored message keeps its row");
        assert_eq!(restored.len(), 3);
        assert_eq!(
            restored.entry(1).map(Entry::summary),
            Some("agent: hello again".to_string())
        );

        restored.push_user_message("next");
        assert_eq!(
            restored.entry(3).map(|entry| entry.id.clone()),
            Some("entry-3".to_string())
        );
    }

    #[test]
    fn a_restored_tool_row_takes_later_updates() {
        let mut transcript = Transcript::default();
        transcript.apply(&tool_event("Edit file", ToolStatus::Running));

        let mut restored = Transcript::default();
        restored.restore(transcript.entries().to_vec());
        let change = restored.apply(&tool_event("Edit file", ToolStatus::Completed));

        assert!(!change.appended);
        assert_eq!(restored.len(), 1);
        assert_eq!(
            restored.entry(0).map(Entry::summary),
            Some("Edit file (Completed)".to_string())
        );
    }

    #[test]
    fn approval_resolution_updates_the_original_card() {
        let mut transcript = Transcript::default();
        transcript.apply(&HarnessEvent::ApprovalRequested {
            request: ApprovalRequest {
                approval_id: ApprovalId::new("approval-1"),
                tool_call_id: ToolCallId::new("tool-1"),
                title: "Run tests".to_string(),
                arguments: serde_json::json!({"command": "cargo test"}),
                paths: Vec::new(),
                patch: None,
                choices: None,
                stream_permit: None,
            },
        });
        transcript.apply(&HarnessEvent::ApprovalResolved {
            approval_id: ApprovalId::new("approval-1"),
            decision: Decision::Allow,
            source: ApprovalSource::User,
        });

        assert_eq!(transcript.len(), 1);
        assert_eq!(
            transcript.entry(0).map(Entry::summary),
            Some("approval: Run tests (Allow)".to_string())
        );
    }
}
