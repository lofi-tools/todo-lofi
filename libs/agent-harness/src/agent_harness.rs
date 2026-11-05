//! The contract between Shepherd's core and a coding-agent adapter.
//!
//! This crate owns the wire-independent event and transcript types. It does not
//! know about Tauri, GPUI, a database, or a particular CLI. Adapters translate
//! their protocol into [`HarnessEvent`] values; the core assembles those events
//! into [`Transcript`] rows and persists settled rows.

pub mod codex;
pub mod fake_harness;

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;

/// Errors an adapter may report to the core.
#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    #[error("the harness has not been started")]
    NotStarted,
    #[error("no agent session is open")]
    SessionMissing,
    #[error("the agent is waiting for an approval")]
    ApprovalPending,
    #[error("there is no pending approval")]
    NoPendingApproval,
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
}

pub type HarnessResult<T> = Result<T, HarnessError>;

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
#[derive(Clone, Debug, Default)]
pub struct SpawnSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub environment: BTreeMap<String, String>,
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
#[derive(Clone, Debug, Default)]
pub struct Capabilities {
    pub streaming: bool,
    pub thoughts: bool,
    pub tool_calls: bool,
    pub approvals: bool,
    pub resume: bool,
    pub model_picker: bool,
    pub mode_picker: bool,
    pub images: bool,
    pub resource_links: bool,
    pub usage: bool,
    pub agent_terminals: bool,
    pub patches_in_approvals: bool,
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
            usage: true,
            ..Self::default()
        }
    }
}

/// One user prompt. Attachments and resource links are added by later slices.
#[derive(Clone, Debug, Default)]
pub struct PromptInput {
    pub text: String,
}

/// A user or policy decision on an approval request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Decision {
    Allow,
    Deny,
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
pub struct ApprovalRequest {
    pub approval_id: ApprovalId,
    pub tool_call_id: ToolCallId,
    pub title: String,
    pub arguments: Value,
    pub paths: Vec<PathBuf>,
    pub patch: Option<FileDiff>,
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
    },
    ToolCall {
        tool_call_id: ToolCallId,
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
    Usage {
        used: u64,
        size: u64,
        cost: Option<f64>,
    },
    ApprovalRequested {
        request: ApprovalRequest,
    },
    ApprovalResolved {
        approval_id: ApprovalId,
        decision: Decision,
    },
    ModeChanged {
        mode: String,
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
    },
    Notice {
        text: String,
        is_error: bool,
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
                title, decision, ..
            } => match decision {
                Some(decision) => format!("approval: {title} ({decision:?})"),
                None => format!("approval: {title} (pending)"),
            },
            EntryKind::Notice { text, is_error } => {
                if *is_error {
                    format!("error: {text}")
                } else {
                    format!("notice: {text}")
                }
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
        self.item_rows.clear();
        self.tool_rows.clear();
        self.approval_rows.clear();
        self.next_sequence = 0;
    }

    /// Record the user's prompt. Agents normally echo it back; the transcript
    /// deliberately ignores those echoes.
    pub fn push_user_message(&mut self, text: impl Into<String>) -> TranscriptChange {
        self.append(EntryKind::UserMessage { text: text.into() })
    }

    pub fn push_notice(&mut self, text: impl Into<String>, is_error: bool) -> TranscriptChange {
        self.append(EntryKind::Notice {
            text: text.into(),
            is_error,
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
            } => self.push_delta(item_id.as_str(), *kind, text),
            HarnessEvent::ToolCall {
                tool_call_id,
                title,
                kind,
                status,
                output,
                diffs,
                terminal_id,
            } => self.upsert_tool_call(
                tool_call_id.as_str(),
                title,
                kind,
                *status,
                output.clone(),
                diffs.clone(),
                terminal_id.clone(),
            ),
            HarnessEvent::Plan { rows } => self.upsert_plan(rows.clone()),
            HarnessEvent::Usage { used, size, cost } => self.upsert_usage(*used, *size, *cost),
            HarnessEvent::ApprovalRequested { request } => self.push_approval(request.clone()),
            HarnessEvent::ApprovalResolved {
                approval_id,
                decision,
            } => self.resolve_approval(approval_id.as_str(), *decision),
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
        let change = self.append(entry_kind);
        if let Some(index) = change.changed {
            self.item_rows.insert(key, index);
        }
        change
    }

    fn upsert_tool_call(
        &mut self,
        tool_call_id: &str,
        title: &str,
        kind: &str,
        status: ToolStatus,
        output: Option<String>,
        diffs: Vec<FileDiff>,
        terminal_id: Option<String>,
    ) -> TranscriptChange {
        if let Some(index) = self.tool_rows.get(tool_call_id).copied() {
            if let Some(entry) = self.entries.get_mut(index) {
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

        let change = self.append(EntryKind::ToolCall {
            tool_call_id: tool_call_id.to_string(),
            title: title.to_string(),
            kind: kind.to_string(),
            status,
            output,
            diffs,
            terminal_id,
        });
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

    fn push_approval(&mut self, request: ApprovalRequest) -> TranscriptChange {
        let key = request.approval_id.as_str().to_string();
        if let Some(index) = self.approval_rows.get(&key).copied() {
            if let Some(entry) = self.entries.get_mut(index) {
                entry.kind = EntryKind::Permission {
                    approval_id: key.clone(),
                    title: request.title,
                    arguments: request.arguments,
                    paths: request
                        .paths
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect(),
                    patch: request.patch,
                    decision: None,
                };
            }
            return TranscriptChange {
                appended: false,
                changed: Some(index),
            };
        }

        let change = self.append(EntryKind::Permission {
            approval_id: key.clone(),
            title: request.title,
            arguments: request.arguments,
            paths: request
                .paths
                .iter()
                .map(|path| path.display().to_string())
                .collect(),
            patch: request.patch,
            decision: None,
        });
        if let Some(index) = change.changed {
            self.approval_rows.insert(key, index);
        }
        change
    }

    fn resolve_approval(&mut self, approval_id: &str, decision: Decision) -> TranscriptChange {
        let Some(index) = self.approval_rows.get(approval_id).copied() else {
            return TranscriptChange::default();
        };
        if let Some(entry) = self.entries.get_mut(index)
            && let EntryKind::Permission { decision: slot, .. } = &mut entry.kind
        {
            *slot = Some(decision);
        }
        TranscriptChange {
            appended: false,
            changed: Some(index),
        }
    }

    fn append(&mut self, kind: EntryKind) -> TranscriptChange {
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        let index = self.entries.len();
        self.entries.push(Entry {
            id: format!("entry-{sequence}"),
            sequence,
            kind,
        });
        TranscriptChange {
            appended: true,
            changed: Some(index),
        }
    }
}

/// The adapter contract. The core drives it; adapters never write to the store.
#[async_trait]
pub trait AgentHarness: Send {
    /// Receive adapter events. The receiver is owned by the harness and must
    /// remain available for the whole session.
    fn events(&mut self) -> &mut mpsc::UnboundedReceiver<HarnessEvent>;

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

    fn tool_event(title: &str, status: ToolStatus) -> HarnessEvent {
        HarnessEvent::ToolCall {
            tool_call_id: ToolCallId::new("tool-1"),
            title: title.to_string(),
            kind: "edit".to_string(),
            status,
            output: None,
            diffs: Vec::new(),
            terminal_id: None,
        }
    }

    #[test]
    fn delta_chunks_grow_one_message_row() {
        let mut transcript = Transcript::default();
        transcript.apply(&HarnessEvent::Delta {
            item_id: ItemId::new("message-1"),
            kind: DeltaKind::Message,
            text: "Hello ".to_string(),
        });
        let second = transcript.apply(&HarnessEvent::Delta {
            item_id: ItemId::new("message-1"),
            kind: DeltaKind::Message,
            text: "world".to_string(),
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
            },
        });
        transcript.apply(&HarnessEvent::ApprovalResolved {
            approval_id: ApprovalId::new("approval-1"),
            decision: Decision::Allow,
        });

        assert_eq!(transcript.len(), 1);
        assert_eq!(
            transcript.entry(0).map(Entry::summary),
            Some("approval: Run tests (Allow)".to_string())
        );
    }
}
