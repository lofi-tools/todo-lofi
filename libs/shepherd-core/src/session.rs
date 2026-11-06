//! Durable session records.
//!
//! A record keeps the adapter's own session handle, which is the only thing that
//! makes resume after a restart possible. The core keeps the newest record per
//! task; the rest is history for diagnostics.

use uuid::Uuid;

use crate::projects::now_millis;
use crate::tasks::TaskId;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRecord {
    pub id: String,
    pub task_id: TaskId,
    /// The adapter family this session belongs to.
    pub adapter: String,
    /// The handle the adapter returned from `open_session`, when it had one.
    pub adapter_session_id: Option<String>,
    pub started_at: u64,
    pub ended_at: Option<u64>,
    pub exit_code: Option<i32>,
    pub stderr: Option<String>,
}

impl SessionRecord {
    pub fn started(
        task_id: TaskId,
        adapter: impl Into<String>,
        adapter_session_id: Option<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            task_id,
            adapter: adapter.into(),
            adapter_session_id,
            started_at: now_millis(),
            ended_at: None,
            exit_code: None,
            stderr: None,
        }
    }

    /// Record how the process ended. A record without an end time is one the
    /// previous run never closed, which is what makes it resumable.
    pub fn end(&mut self, exit_code: Option<i32>, stderr: Option<String>) {
        self.ended_at = Some(now_millis());
        self.exit_code = exit_code;
        self.stderr = stderr;
    }

    pub fn is_ended(&self) -> bool {
        self.ended_at.is_some()
    }
}
