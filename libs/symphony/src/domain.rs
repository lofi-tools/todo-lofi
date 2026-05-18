use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BlockerRef {
    /// Blocker issue ID.
    pub id: Option<String>,
    /// Blocker issue identifier.
    pub identifier: Option<String>,
    /// Blocker issue state.
    pub state: Option<String>,
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
    /// Attempt number (`null` for first run, `>=1` for retries/continuation).
    pub attempt: Option<u32>,
    /// Workspace path used for this attempt.
    pub workspace_path: String,
    /// When the attempt started.
    pub started_at: SystemTime,
    /// Current status of the attempt.
    pub status: RunAttemptStatus,
    /// Optional error if the attempt failed.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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

/// State tracked while a coding-agent subprocess is running.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LiveSession {
    /// Session ID: `<thread_id>-<turn_id>`.
    pub session_id: String,
    /// Coding agent thread ID.
    pub thread_id: String,
    /// Coding agent turn ID.
    pub turn_id: String,
    /// Codex app-server process ID.
    pub codex_app_server_pid: Option<u32>,
    /// Last received event type from Codex.
    pub last_codex_event: Option<String>,
    /// Timestamp of last Codex event.
    pub last_codex_timestamp: Option<SystemTime>,
    /// Summarized last message from Codex.
    pub last_codex_message: Option<String>,
    /// Token usage counters.
    pub codex_input_tokens: u64,
    pub codex_output_tokens: u64,
    pub codex_total_tokens: u64,
    /// Last reported token counts (for delta calculation).
    pub last_reported_input_tokens: u64,
    pub last_reported_output_tokens: u64,
    pub last_reported_total_tokens: u64,
    /// Number of turns started in this worker lifetime.
    pub turn_count: u32,
}

/// Scheduled retry state for an issue.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RetryEntry {
    /// Issue ID.
    pub issue_id: String,
    /// Human-readable identifier for logs.
    pub identifier: String,
    /// 1-based attempt number for retry queue.
    pub attempt: u32,
    /// When the retry is due (monotonic milliseconds).
    pub due_at_ms: u64,
    /// Runtime-specific timer reference (implementation-dependent).
    pub timer_handle: Option<u64>,
    /// Error that caused this retry.
    pub error: Option<String>,
}

/// Single authoritative in-memory state owned by the orchestrator.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OrchestratorState {
    /// Current effective poll interval (milliseconds).
    pub poll_interval_ms: u64,
    /// Current effective global concurrency limit.
    pub max_concurrent_agents: u32,
    /// Currently running issue attempts: `issue_id -> run attempt`.
    pub running: HashMap<String, RunAttempt>,
    /// Set of issue IDs that are claimed (running or retry queued).
    pub claimed: std::collections::HashSet<String>,
    /// Retry queue: `issue_id -> retry entry`.
    pub retry_attempts: HashMap<String, RetryEntry>,
    /// Completed issue IDs (bookkeeping only).
    pub completed: std::collections::HashSet<String>,
    /// Aggregate token usage and runtime.
    pub codex_totals: CodexTotals,
    /// Latest rate limit snapshot from agent events.
    pub codex_rate_limits: Option<RateLimitInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CodexTotals {
    /// Total input tokens consumed.
    pub input_tokens: u64,
    /// Total output tokens consumed.
    pub output_tokens: u64,
    /// Total tokens consumed.
    pub total_tokens: u64,
    /// Aggregate runtime seconds (including active sessions).
    pub seconds_running: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RateLimitInfo {
    /// Limit value.
    pub limit: Option<u32>,
    /// Remaining requests.
    pub remaining: Option<u32>,
    /// Reset timestamp.
    pub reset: Option<u64>,
    /// Raw rate limit payload.
    pub raw: Option<serde_json::Value>,
}