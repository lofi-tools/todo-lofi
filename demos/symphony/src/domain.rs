use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::SystemTime;

/// Normalized issue record used by orchestration, prompt rendering, and observability output.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Issue {
    /// Stable tracker-internal ID.
    pub id: String,
    /// Human-readable ticket key (example: `ABC-123`).
    pub identifier: String,
    /// Issue title.
    pub title: String,
    /// Issue description (can be null).
    pub description: Option<String>,
    /// Priority (lower numbers are higher priority in dispatch sorting).
    pub priority: Option<i32>,
    /// Current tracker state name.
    pub state: String,
    /// Tracker-provided branch metadata if available.
    pub branch_name: Option<String>,
    /// Issue URL.
    pub url: Option<String>,
    /// Normalized to lowercase labels.
    pub labels: Vec<String>,
    /// Blocker relationships.
    pub blocked_by: Vec<BlockerRef>,
    /// Creation timestamp.
    pub created_at: Option<SystemTime>,
    /// Last update timestamp.
    pub updated_at: Option<SystemTime>,
}

impl Issue {
    /// Whether this issue has everything dispatch requires.
    pub fn is_dispatchable(&self) -> bool {
        !self.id.is_empty()
            && !self.identifier.is_empty()
            && !self.title.is_empty()
            && !self.state.is_empty()
    }

    /// Normalized (lowercase) tracker state.
    pub fn normalized_state(&self) -> String {
        normalize_state(&self.state)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BlockerRef {
    /// Blocker issue ID.
    pub id: Option<String>,
    /// Blocker issue identifier.
    pub identifier: Option<String>,
    /// Blocker issue state.
    pub state: Option<String>,
}

/// Compare tracker states case-insensitively.
pub fn normalize_state(state: &str) -> String {
    state.to_lowercase()
}

/// Parsed `WORKFLOW.md` payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowDefinition {
    /// YAML front matter root object.
    pub config: HashMap<String, serde_yaml::Value>,
    /// Markdown body after front matter, trimmed.
    pub prompt_template: String,
}

/// One execution attempt for one issue.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunAttempt {
    /// Issue ID.
    pub issue_id: String,
    /// Issue identifier (human-readable).
    pub issue_identifier: String,
    /// Attempt number (`None` for the first run, `>=1` for retries/continuations).
    pub attempt: Option<u32>,
    /// Workspace path used for this attempt.
    pub workspace_path: PathBuf,
    /// When the attempt started.
    pub started_at: SystemTime,
    /// Current status of the attempt.
    pub status: RunAttemptStatus,
    /// Failure reason, when the attempt is not succeeding.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum RunAttemptStatus {
    PreparingWorkspace,
    BuildingPrompt,
    LaunchingAgentProcess,
    InitializingSession,
    StreamingTurn,
    Finishing,
    Succeeded,
    Failed,
    TimedOut,
    Stalled,
    CanceledByReconciliation,
}

impl RunAttemptStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            RunAttemptStatus::PreparingWorkspace => "preparing_workspace",
            RunAttemptStatus::BuildingPrompt => "building_prompt",
            RunAttemptStatus::LaunchingAgentProcess => "launching_agent_process",
            RunAttemptStatus::InitializingSession => "initializing_session",
            RunAttemptStatus::StreamingTurn => "streaming_turn",
            RunAttemptStatus::Finishing => "finishing",
            RunAttemptStatus::Succeeded => "succeeded",
            RunAttemptStatus::Failed => "failed",
            RunAttemptStatus::TimedOut => "timed_out",
            RunAttemptStatus::Stalled => "stalled",
            RunAttemptStatus::CanceledByReconciliation => "canceled_by_reconciliation",
        }
    }
}

/// State tracked while a coding-agent subprocess is running.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LiveSession {
    /// Session ID: `<thread_id>-<turn_id>`.
    pub session_id: Option<String>,
    /// Coding agent thread ID.
    pub thread_id: Option<String>,
    /// Coding agent turn ID.
    pub turn_id: Option<String>,
    /// Codex app-server process ID.
    pub codex_app_server_pid: Option<u32>,
    /// Last received event type from Codex.
    pub last_codex_event: Option<String>,
    /// Timestamp of last Codex event.
    pub last_codex_timestamp: Option<SystemTime>,
    /// Summarized last message from Codex.
    pub last_codex_message: Option<String>,
    /// Absolute token usage totals reported by the agent for the current thread.
    pub codex_input_tokens: u64,
    pub codex_output_tokens: u64,
    pub codex_total_tokens: u64,
    /// Last absolute totals that were folded into the orchestrator counters.
    pub last_reported_input_tokens: u64,
    pub last_reported_output_tokens: u64,
    pub last_reported_total_tokens: u64,
    /// Number of turns started in this worker lifetime.
    pub turn_count: u32,
}

impl LiveSession {
    /// Compose the stable session identifier from thread and turn identity.
    pub fn compose_session_id(thread_id: &str, turn_id: &str) -> String {
        format!("{thread_id}-{turn_id}")
    }

    /// Refresh `session_id` from the current thread/turn identity.
    pub fn refresh_session_id(&mut self) {
        if let (Some(thread_id), Some(turn_id)) = (&self.thread_id, &self.turn_id) {
            self.session_id = Some(Self::compose_session_id(thread_id, turn_id));
        }
    }
}

/// Token counters reported by an agent event.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

/// Scheduled retry state for an issue.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RetryEntry {
    /// Issue ID.
    pub issue_id: String,
    /// Human-readable identifier for logs.
    pub identifier: String,
    /// 1-based attempt number for the retry queue.
    pub attempt: u32,
    /// When the retry is due, as milliseconds from the process start of the monotonic clock.
    pub due_at_ms: u64,
    /// Error (or reason) that caused this retry.
    pub error: Option<String>,
}

/// A running issue: the attempt plus the live agent session snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunningEntry {
    /// The current attempt for this issue.
    pub attempt: RunAttempt,
    /// Latest known issue snapshot (refreshed by reconciliation).
    pub issue: Issue,
    /// Live coding-agent session metadata.
    pub session: LiveSession,
    /// Attempt number that preceded this run (`None` for the first run).
    pub retry_attempt: Option<u32>,
}

/// Aggregate token usage and runtime accounting.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct CodexTotals {
    /// Total input tokens consumed.
    pub input_tokens: u64,
    /// Total output tokens consumed.
    pub output_tokens: u64,
    /// Total tokens consumed.
    pub total_tokens: u64,
    /// Cumulative runtime of ended sessions, in seconds.
    pub seconds_running: f64,
}

/// Single authoritative in-memory state owned by the orchestrator.
#[derive(Debug, Clone, Default)]
pub struct OrchestratorState {
    /// Current effective poll interval (milliseconds).
    pub poll_interval_ms: u64,
    /// Current effective global concurrency limit.
    pub max_concurrent_agents: u32,
    /// Currently running issues: `issue_id -> running entry`.
    pub running: HashMap<String, RunningEntry>,
    /// Set of issue IDs that are claimed (running or retry queued).
    pub claimed: HashSet<String>,
    /// Retry queue: `issue_id -> retry entry`.
    pub retry_attempts: HashMap<String, RetryEntry>,
    /// Completed issue IDs (bookkeeping only, not dispatch gating).
    pub completed: HashSet<String>,
    /// Aggregate token usage and runtime.
    pub codex_totals: CodexTotals,
    /// Latest rate limit snapshot from agent events.
    pub codex_rate_limits: Option<serde_json::Value>,
}

impl OrchestratorState {
    pub fn running_count(&self) -> usize {
        self.running.len()
    }

    /// Number of running issues whose latest known tracker state is `state`.
    pub fn running_count_in_state(&self, state: &str) -> usize {
        let state = normalize_state(state);
        self.running
            .values()
            .filter(|entry| entry.issue.normalized_state() == state)
            .count()
    }

    /// Remaining global dispatch slots.
    pub fn available_slots(&self) -> u32 {
        self.max_concurrent_agents
            .saturating_sub(self.running_count() as u32)
    }

    /// Remaining dispatch slots for a tracker state, honouring per-state overrides.
    pub fn available_slots_for_state(&self, state: &str, by_state: &HashMap<String, u32>) -> u32 {
        match by_state.get(&normalize_state(state)) {
            Some(limit) => limit.saturating_sub(self.running_count_in_state(state) as u32),
            None => self.available_slots(),
        }
    }

    /// Runtime of active sessions as of `now`, in seconds.
    pub fn active_runtime_seconds(&self, now: SystemTime) -> f64 {
        self.running
            .values()
            .map(|entry| {
                now.duration_since(entry.attempt.started_at)
                    .unwrap_or_default()
                    .as_secs_f64()
            })
            .sum()
    }

    /// Aggregate token totals including the live per-session counters.
    pub fn aggregated_totals(&self, now: SystemTime) -> CodexTotals {
        let mut totals = self.codex_totals;
        totals.seconds_running += self.active_runtime_seconds(now);
        totals
    }
}

/// A running session as reported by the runtime snapshot interface.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SnapshotRunning {
    pub issue_id: String,
    pub issue_identifier: String,
    pub state: String,
    pub session_id: Option<String>,
    pub turn_count: u32,
    pub last_event: Option<String>,
    pub last_message: Option<String>,
    pub started_at: SystemTime,
    pub last_event_at: Option<SystemTime>,
    pub tokens: TokenUsage,
    pub workspace_path: String,
    pub attempt: Option<u32>,
}

/// A retry queue row as reported by the runtime snapshot interface.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SnapshotRetry {
    pub issue_id: String,
    pub issue_identifier: String,
    pub attempt: u32,
    pub due_at: SystemTime,
    pub error: Option<String>,
}

/// Synchronous runtime snapshot used by status surfaces and monitoring.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimeSnapshot {
    pub generated_at: SystemTime,
    pub running: Vec<SnapshotRunning>,
    pub retrying: Vec<SnapshotRetry>,
    pub codex_totals: CodexTotals,
    pub rate_limits: Option<serde_json::Value>,
}

impl RuntimeSnapshot {
    /// Render the snapshot using the JSON shape recommended by the specification.
    pub fn to_json(&self) -> serde_json::Value {
        use crate::clock::format_timestamp;

        serde_json::json!({
            "generated_at": format_timestamp(Some(self.generated_at)),
            "counts": {
                "running": self.running.len(),
                "retrying": self.retrying.len(),
            },
            "running": self
                .running
                .iter()
                .map(|row| serde_json::json!({
                    "issue_id": row.issue_id,
                    "issue_identifier": row.issue_identifier,
                    "state": row.state,
                    "session_id": row.session_id,
                    "turn_count": row.turn_count,
                    "last_event": row.last_event,
                    "last_message": row.last_message.clone().unwrap_or_default(),
                    "started_at": format_timestamp(Some(row.started_at)),
                    "last_event_at": format_timestamp(row.last_event_at),
                    "attempt": row.attempt,
                    "workspace": { "path": row.workspace_path },
                    "tokens": {
                        "input_tokens": row.tokens.input_tokens,
                        "output_tokens": row.tokens.output_tokens,
                        "total_tokens": row.tokens.total_tokens,
                    },
                }))
                .collect::<Vec<_>>(),
            "retrying": self
                .retrying
                .iter()
                .map(|row| serde_json::json!({
                    "issue_id": row.issue_id,
                    "issue_identifier": row.issue_identifier,
                    "attempt": row.attempt,
                    "due_at": format_timestamp(Some(row.due_at)),
                    "error": row.error,
                }))
                .collect::<Vec<_>>(),
            "codex_totals": {
                "input_tokens": self.codex_totals.input_tokens,
                "output_tokens": self.codex_totals.output_tokens,
                "total_tokens": self.codex_totals.total_tokens,
                "seconds_running": self.codex_totals.seconds_running,
            },
            "rate_limits": self.rate_limits,
        })
    }
}
