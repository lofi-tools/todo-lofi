use crate::agent::{AgentClient, AgentEvent, ClientTool, TurnTerminal, CONTINUATION_GUIDANCE};
use crate::clock::{monotonic_ms, now_plus_monotonic};
use crate::config::{EffectiveWorkflow, ServiceConfig};
use crate::domain::*;
use crate::error::Result;
use crate::error::SymphonyError::*;
use crate::prompt;
use crate::tracker::IssueTracker;
use crate::workflow::WorkflowWatcher;
use crate::workspace::{Workspace, WorkspaceManager};
use log::{debug, error, info, warn};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};
use std::time::{Duration, SystemTime};
use tokio::sync::{Notify, mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

/// Fixed continuation-retry delay after a clean worker exit (Section 8.4).
pub const CONTINUATION_RETRY_DELAY_MS: u64 = 1_000;
/// Base delay for failure-driven exponential backoff (Section 8.4).
pub const RETRY_BACKOFF_BASE_MS: u64 = 10_000;
/// Upper bound on an idle sleep so timers stay responsive.
const MAX_IDLE_WAKE_MS: u64 = 1_000;

/// Why a running issue was terminated by the orchestrator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TerminationReason {
    /// Tracker state became terminal: stop the run and clean the workspace.
    Terminal,
    /// Tracker state is no longer active: stop the run without cleanup.
    NonActive,
    /// The session stopped reporting activity.
    Stalled,
}

/// How a worker attempt ended.
#[derive(Debug, Clone)]
pub enum WorkerOutcome {
    /// The worker finished its turn loop normally.
    Normal,
    /// The worker failed; the reason is used for retry scheduling and logs.
    Failed(String),
    /// The orchestrator cancelled the worker (reconciliation or shutdown).
    Canceled,
}

/// The scheduling action taken after a worker exit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExitAction {
    /// Schedule the short continuation retry.
    Continuation,
    /// Schedule an exponential-backoff retry.
    Retry { attempt: u32, reason: String },
    /// Release the claim without scheduling a retry.
    Released,
}

/// Events reported by worker tasks to the orchestrator.
#[derive(Debug, Clone)]
pub enum WorkerEvent {
    Started {
        issue_id: String,
        generation: u64,
        workspace_path: PathBuf,
    },
    Agent {
        issue_id: String,
        generation: u64,
        event: AgentEvent,
    },
    Exited {
        issue_id: String,
        generation: u64,
        outcome: WorkerOutcome,
    },
}

struct WorkerHandle {
    generation: u64,
    cancel: watch::Sender<bool>,
    task: JoinHandle<()>,
}

/// Read-only handles shared with observability surfaces.
#[derive(Clone)]
pub struct ObservabilityState {
    pub state: Arc<Mutex<OrchestratorState>>,
    pub refresh: Arc<Notify>,
    pub effective: Arc<RwLock<EffectiveWorkflow>>,
}

/// Orchestrator that owns scheduling state and dispatches worker tasks.
pub struct Orchestrator<T: IssueTracker> {
    tracker: Arc<T>,
    workspace_manager: Arc<WorkspaceManager>,
    effective: Arc<RwLock<EffectiveWorkflow>>,
    watcher: WorkflowWatcher,
    state: Arc<Mutex<OrchestratorState>>,
    workers: HashMap<String, WorkerHandle>,
    events_tx: mpsc::UnboundedSender<WorkerEvent>,
    events: mpsc::UnboundedReceiver<WorkerEvent>,
    refresh: Arc<Notify>,
    tools: Vec<Arc<dyn ClientTool>>,
    next_generation: u64,
}

impl<T: IssueTracker + 'static> Orchestrator<T> {
    /// Create a new orchestrator from a validated effective workflow.
    pub fn new(
        tracker: T,
        workspace_manager: WorkspaceManager,
        effective: EffectiveWorkflow,
        tools: Vec<Arc<dyn ClientTool>>,
    ) -> Result<Self> {
        let watcher = WorkflowWatcher::new(effective.path.clone());
        let state = OrchestratorState {
            poll_interval_ms: effective.config.polling.interval_ms,
            max_concurrent_agents: effective.config.agent.max_concurrent_agents,
            ..OrchestratorState::default()
        };
        let (events_tx, events) = mpsc::unbounded_channel();

        Ok(Self {
            tracker: Arc::new(tracker),
            workspace_manager: Arc::new(workspace_manager),
            effective: Arc::new(RwLock::new(effective)),
            watcher,
            state: Arc::new(Mutex::new(state)),
            workers: HashMap::new(),
            events_tx,
            events,
            refresh: Arc::new(Notify::new()),
            tools,
            next_generation: 0,
        })
    }

    /// Handles for status surfaces and monitoring endpoints.
    pub fn observability(&self) -> ObservabilityState {
        ObservabilityState {
            state: Arc::clone(&self.state),
            refresh: Arc::clone(&self.refresh),
            effective: Arc::clone(&self.effective),
        }
    }

    /// Current runtime snapshot (Section 13.3).
    pub fn snapshot(&self) -> RuntimeSnapshot {
        snapshot_of(&self.lock_state())
    }

    /// Run the scheduling loop until a shutdown signal is received.
    pub async fn run(&mut self, mut shutdown: watch::Receiver<bool>) -> Result<()> {
        self.startup_cleanup().await;
        self.log_running_summary("startup");

        let mut next_tick = Instant::now();

        enum Step {
            Refresh,
            Event(Option<WorkerEvent>),
            Shutdown { closed: bool },
            Wake,
        }

        loop {
            let now = Instant::now();
            let next_retry = self.next_retry_instant();
            let wake = next_tick
                .min(next_retry.unwrap_or(now + Duration::from_millis(MAX_IDLE_WAKE_MS)))
                .min(now + Duration::from_millis(MAX_IDLE_WAKE_MS));

            // Each branch only produces a value, so the borrows taken by the
            // awaited futures end before the handlers below run.
            let step = tokio::select! {
                biased;

                changed = shutdown.changed() => Step::Shutdown { closed: changed.is_err() },

                _ = tokio::time::sleep_until(wake) => Step::Wake,

                _ = self.refresh.notified() => Step::Refresh,

                event = self.events.recv() => Step::Event(event),
            };

            match step {
                Step::Shutdown { closed } => {
                    if closed || *shutdown.borrow() {
                        info!(target: "symphony", "outcome=stopping message=shutdown requested");
                        self.shutdown_workers().await;
                        return Ok(());
                    }
                }
                Step::Refresh => {
                    // Operator-triggered refresh: reload, then poll and reconcile now.
                    self.reload_workflow_if_changed();
                    self.tick().await;
                    next_tick = Instant::now() + self.poll_interval();
                }
                Step::Event(Some(event)) => {
                    self.handle_worker_event(event).await;
                }
                Step::Event(None) => {
                    // Unreachable while this orchestrator holds a sender; treat
                    // it as teardown rather than spinning.
                    self.shutdown_workers().await;
                    return Ok(());
                }
                Step::Wake => {}
            }

            self.reload_workflow_if_changed();

            if Instant::now() >= next_tick {
                self.tick().await;
                next_tick = Instant::now() + self.poll_interval();
            }

            self.fire_due_retries().await;
        }
    }

    fn poll_interval(&self) -> Duration {
        Duration::from_millis(self.lock_state().poll_interval_ms.max(1))
    }

    fn lock_state(&self) -> MutexGuard<'_, OrchestratorState> {
        // Scheduler state is structurally consistent; recover rather than panic
        // the whole service if a worker panicked while holding the lock.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn read_effective(&self) -> EffectiveWorkflow {
        self.effective
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Apply a reloaded workflow, keeping the last known good configuration on error.
    fn reload_workflow_if_changed(&mut self) {
        let Some(result) = self.watcher.poll() else {
            return;
        };

        let path = self.watcher.path().to_path_buf();
        let definition = match result {
            Ok(definition) => definition,
            Err(error) => {
                error!(
                    target: "symphony",
                    "outcome=failed action=reload_workflow workflow={} error={error} message=keeping last known good configuration",
                    path.display()
                );
                return;
            }
        };

        match EffectiveWorkflow::from_definition(&path, &definition) {
            Ok(effective) => {
                let poll_interval_ms = effective.config.polling.interval_ms;
                let max_concurrent_agents = effective.config.agent.max_concurrent_agents;
                {
                    let mut guard = self
                        .effective
                        .write()
                        .unwrap_or_else(PoisonError::into_inner);
                    *guard = effective;
                }
                // Re-apply runtime settings that affect scheduling.
                {
                    let mut state = self.lock_state();
                    state.poll_interval_ms = poll_interval_ms;
                    state.max_concurrent_agents = max_concurrent_agents;
                }
                info!(
                    target: "symphony",
                    "outcome=reloaded workflow={} poll_interval_ms={poll_interval_ms} max_concurrent_agents={max_concurrent_agents}",
                    path.display()
                );
            }
            Err(error) => {
                error!(
                    target: "symphony",
                    "outcome=failed action=reload_workflow workflow={} error={error} message=keeping last known good configuration",
                    path.display()
                );
            }
        }
    }

    /// Remove workspaces for issues that are already in terminal states (Section 8.6).
    async fn startup_cleanup(&self) {
        let effective = self.read_effective();
        let terminal_states = effective.config.tracker.terminal_states.clone();

        match self.tracker.fetch_issues_by_states(terminal_states).await {
            Ok(issues) => {
                for issue in issues {
                    match self
                        .workspace_manager
                        .remove_for_issue(&issue.identifier, &effective.config.hooks)
                        .await
                    {
                        Ok(()) => info!(
                            target: "symphony",
                            "issue_identifier={} outcome=completed action=startup_workspace_cleanup",
                            issue.identifier
                        ),
                        Err(error) => warn!(
                            target: "symphony",
                            "issue_identifier={} outcome=failed action=startup_workspace_cleanup error={error}",
                            issue.identifier
                        ),
                    }
                }
            }
            Err(error) => {
                warn!(
                    target: "symphony",
                    "outcome=failed action=startup_terminal_cleanup error={error} message=continuing startup"
                );
            }
        }
    }

    /// One poll tick: reconcile, validate, fetch candidates, dispatch (Section 8.1).
    async fn tick(&mut self) {
        // Defensive reload in case a filesystem watch event was missed.
        self.reload_workflow_if_changed();

        self.reconcile().await;

        if let Err(error) = validate_dispatch_config(&self.read_effective().config) {
            error!(
                target: "symphony",
                "outcome=failed action=dispatch_preflight error={error} message=skipping dispatch for this tick"
            );
            return;
        }

        let candidates = match self.tracker.fetch_candidate_issues().await {
            Ok(candidates) => candidates,
            Err(error) => {
                error!(
                    target: "symphony",
                    "outcome=failed action=fetch_candidates error={error} message=skipping dispatch for this tick"
                );
                return;
            }
        };

        let sorted = sort_for_dispatch(candidates);
        let dispatched = self.select_dispatchable(&sorted);
        for (issue, attempt) in dispatched {
            self.dispatch_issue(issue, attempt).await;
        }

        self.log_running_summary("tick");
    }

    /// Choose the issues to dispatch this tick, respecting slots and blockers.
    fn select_dispatchable(&self, candidates: &[Issue]) -> Vec<(Issue, Option<u32>)> {
        let config = self.read_effective().config;
        let state = self.lock_state();
        select_dispatchable(candidates, &config, &state)
            .into_iter()
            .map(|issue| (issue, None))
            .collect()
    }

    /// Spawn a worker for an issue (Section 16.4).
    async fn dispatch_issue(&mut self, issue: Issue, attempt: Option<u32>) {
        let generation = self.next_generation;
        self.next_generation += 1;

        let workspace_path = self.workspace_manager.workspace_path(&issue.identifier);
        {
            let mut state = self.lock_state();
            state.claimed.insert(issue.id.clone());
            state.retry_attempts.remove(&issue.id);
            state.running.insert(
                issue.id.clone(),
                RunningEntry {
                    attempt: RunAttempt {
                        issue_id: issue.id.clone(),
                        issue_identifier: issue.identifier.clone(),
                        attempt,
                        workspace_path,
                        started_at: SystemTime::now(),
                        status: RunAttemptStatus::PreparingWorkspace,
                        error: None,
                    },
                    issue: issue.clone(),
                    session: LiveSession::default(),
                    retry_attempt: attempt,
                },
            );
        }

        info!(
            target: "symphony",
            "issue_id={} issue_identifier={} outcome=dispatched attempt={:?}",
            issue.id,
            issue.identifier,
            attempt
        );

        let (cancel, cancel_rx) = watch::channel(false);
        let task = tokio::spawn(run_worker(
            Arc::clone(&self.tracker),
            Arc::clone(&self.workspace_manager),
            Arc::clone(&self.effective),
            issue.clone(),
            attempt,
            generation,
            cancel_rx,
            self.events_tx.clone(),
            self.tools.clone(),
        ));

        self.workers.insert(
            issue.id.clone(),
            WorkerHandle {
                generation,
                cancel,
                task,
            },
        );
    }

    /// Whether an event belongs to the currently tracked worker generation.
    fn is_current_worker(&self, issue_id: &str, generation: u64) -> bool {
        self.workers
            .get(issue_id)
            .map(|handle| handle.generation == generation)
            .unwrap_or(false)
    }

    async fn handle_worker_event(&mut self, event: WorkerEvent) {
        match event {
            WorkerEvent::Started {
                issue_id,
                generation,
                workspace_path,
            } => {
                if !self.is_current_worker(&issue_id, generation) {
                    return;
                }
                let mut state = self.lock_state();
                if let Some(entry) = state.running.get_mut(&issue_id) {
                    entry.attempt.workspace_path = workspace_path;
                    entry.attempt.status = RunAttemptStatus::InitializingSession;
                }
            }
            WorkerEvent::Agent {
                issue_id,
                generation,
                event,
            } => {
                if !self.is_current_worker(&issue_id, generation) {
                    return;
                }
                self.apply_agent_event(&issue_id, event);
            }
            WorkerEvent::Exited {
                issue_id,
                generation,
                outcome,
            } => {
                if !self.is_current_worker(&issue_id, generation) {
                    return;
                }
                self.workers.remove(&issue_id);
                self.on_worker_exit(&issue_id, outcome).await;
            }
        }
    }

    /// Fold an agent event into the authoritative running entry.
    fn apply_agent_event(&self, issue_id: &str, event: AgentEvent) {
        let mut state = self.lock_state();
        let mut rate_limits: Option<serde_json::Value> = None;
        let mut token_delta = TokenUsage::default();
        let event_name = event.name().to_string();

        {
            let Some(entry) = state.running.get_mut(issue_id) else {
                return;
            };

            entry.session.last_codex_timestamp = Some(SystemTime::now());
            entry.session.last_codex_event = Some(event_name);

            match event {
                AgentEvent::SessionStarted {
                    session_id,
                    thread_id,
                    turn_id,
                    pid,
                } => {
                    entry.session.session_id = Some(session_id);
                    entry.session.thread_id = Some(thread_id);
                    entry.session.turn_id = Some(turn_id);
                    entry.session.codex_app_server_pid = Some(pid);
                    entry.attempt.status = RunAttemptStatus::StreamingTurn;
                }
                AgentEvent::TurnStarted { turn_id } => {
                    entry.session.turn_id = Some(turn_id);
                    entry.session.refresh_session_id();
                    entry.session.turn_count += 1;
                    entry.attempt.status = RunAttemptStatus::StreamingTurn;
                }
                AgentEvent::TokenUsage { usage } => {
                    // Fold only the delta against the last reported absolute totals.
                    token_delta.input_tokens = usage
                        .input_tokens
                        .saturating_sub(entry.session.last_reported_input_tokens);
                    token_delta.output_tokens = usage
                        .output_tokens
                        .saturating_sub(entry.session.last_reported_output_tokens);
                    token_delta.total_tokens = usage
                        .total_tokens
                        .saturating_sub(entry.session.last_reported_total_tokens);

                    entry.session.last_reported_input_tokens = entry
                        .session
                        .last_reported_input_tokens
                        .max(usage.input_tokens);
                    entry.session.last_reported_output_tokens = entry
                        .session
                        .last_reported_output_tokens
                        .max(usage.output_tokens);
                    entry.session.last_reported_total_tokens = entry
                        .session
                        .last_reported_total_tokens
                        .max(usage.total_tokens);

                    entry.session.codex_input_tokens = usage.input_tokens;
                    entry.session.codex_output_tokens = usage.output_tokens;
                    entry.session.codex_total_tokens = usage.total_tokens;
                }
                AgentEvent::RateLimits { payload } => {
                    rate_limits = Some(payload);
                }
                AgentEvent::TurnFailed { error, .. }
                | AgentEvent::TurnEndedWithError { error, .. } => {
                    entry.session.last_codex_message = Some(error.clone());
                    entry.attempt.error = Some(error);
                }
                AgentEvent::TurnInputRequired { prompt, .. } => {
                    entry.session.last_codex_message = Some(prompt);
                }
                AgentEvent::ApprovalAutoApproved { request_type, .. } => {
                    entry.session.last_codex_message = Some(format!("auto-approved {request_type}"));
                }
                AgentEvent::UnsupportedToolCall { tool_name, .. } => {
                    entry.session.last_codex_message =
                        Some(format!("unsupported tool call `{tool_name}`"));
                }
                AgentEvent::Notification { message }
                | AgentEvent::OtherMessage { message }
                | AgentEvent::Malformed { message } => {
                    entry.session.last_codex_message = Some(message);
                }
                AgentEvent::StartupFailed { error } => {
                    entry.session.last_codex_message = Some(error);
                }
                AgentEvent::TurnCompleted { .. } | AgentEvent::TurnCancelled { .. } => {}
            }
        }

        state.codex_totals.input_tokens += token_delta.input_tokens;
        state.codex_totals.output_tokens += token_delta.output_tokens;
        state.codex_totals.total_tokens += token_delta.total_tokens;
        if let Some(payload) = rate_limits {
            state.codex_rate_limits = Some(payload);
        }
    }

    /// Convert a worker exit into the next orchestration state (Section 16.6).
    async fn on_worker_exit(&mut self, issue_id: &str, outcome: WorkerOutcome) {
        let entry = { self.lock_state().running.remove(issue_id) };
        let Some(entry) = entry else {
            return;
        };

        let max_retry_backoff_ms = self.read_effective().config.agent.max_retry_backoff_ms;
        let elapsed = SystemTime::now()
            .duration_since(entry.attempt.started_at)
            .unwrap_or_default()
            .as_secs_f64();
        let identifier = entry.attempt.issue_identifier.clone();

        let action = {
            let mut state = self.lock_state();
            state.codex_totals.seconds_running += elapsed;
            worker_exit_transition(&mut state, issue_id, &entry, &outcome, max_retry_backoff_ms)
        };

        match action {
            ExitAction::Continuation => info!(
                target: "symphony",
                "issue_id={issue_id} issue_identifier={identifier} outcome=completed session_seconds={elapsed:.1} retrying=true"
            ),
            ExitAction::Retry { attempt, reason } => warn!(
                target: "symphony",
                "issue_id={issue_id} issue_identifier={identifier} outcome=failed reason={reason} attempt={attempt} session_seconds={elapsed:.1}"
            ),
            ExitAction::Released => info!(
                target: "symphony",
                "issue_id={issue_id} issue_identifier={identifier} outcome=cancelled session_seconds={elapsed:.1}"
            ),
        }
    }

    /// Reconciliation: stall detection then tracker state refresh (Section 8.5).
    async fn reconcile(&mut self) {
        let effective = self.read_effective();

        // Part A: stall detection.
        if effective.config.codex.stall_timeout_ms > 0 {
            let stall_timeout = Duration::from_millis(effective.config.codex.stall_timeout_ms);
            let stalled = {
                let state = self.lock_state();
                stalled_issue_ids(&state, stall_timeout, SystemTime::now())
            };

            for (issue_id, identifier) in stalled {
                warn!(
                    target: "symphony",
                    "issue_id={issue_id} issue_identifier={identifier} outcome=stalled action=terminating"
                );
                self.terminate(&issue_id, TerminationReason::Stalled).await;
            }
        }

        // Part B: tracker state refresh.
        let running_ids: Vec<String> = {
            let state = self.lock_state();
            state.running.keys().cloned().collect()
        };
        if running_ids.is_empty() {
            return;
        }

        let refreshed = match self.tracker.fetch_issue_states_by_ids(running_ids).await {
            Ok(issues) => issues,
            Err(error) => {
                debug!(
                    target: "symphony",
                    "outcome=failed action=reconcile_state_refresh error={error} message=keeping workers running"
                );
                return;
            }
        };

        let mut terminal = Vec::new();
        let mut non_active = Vec::new();
        let mut updates = Vec::new();

        for issue in refreshed {
            if effective.config.tracker.is_terminal_state(&issue.state) {
                terminal.push((issue.id.clone(), issue.identifier.clone()));
            } else if effective.config.tracker.is_active_state(&issue.state) {
                updates.push(issue);
            } else {
                non_active.push((issue.id.clone(), issue.identifier.clone()));
            }
        }

        if !updates.is_empty() {
            let mut state = self.lock_state();
            for issue in updates {
                if let Some(entry) = state.running.get_mut(&issue.id) {
                    entry.issue = issue;
                }
            }
        }

        for (issue_id, identifier) in terminal {
            info!(
                target: "symphony",
                "issue_id={issue_id} issue_identifier={identifier} outcome=reconciled action=terminal"
            );
            self.terminate(&issue_id, TerminationReason::Terminal).await;
        }

        for (issue_id, identifier) in non_active {
            info!(
                target: "symphony",
                "issue_id={issue_id} issue_identifier={identifier} outcome=reconciled action=non_active"
            );
            self.terminate(&issue_id, TerminationReason::NonActive).await;
        }
    }

    /// Stop a running issue according to the termination reason.
    async fn terminate(&mut self, issue_id: &str, reason: TerminationReason) {
        if let Some(handle) = self.workers.remove(issue_id) {
            if handle.cancel.send(true).is_err() {
                debug!(
                    target: "symphony",
                    "issue_id={issue_id} outcome=ignored message=worker already finished"
                );
            }
        }

        let entry = { self.lock_state().running.remove(issue_id) };
        let Some(entry) = entry else {
            return;
        };

        let effective = self.read_effective();
        let elapsed = SystemTime::now()
            .duration_since(entry.attempt.started_at)
            .unwrap_or_default()
            .as_secs_f64();
        let identifier = entry.attempt.issue_identifier.clone();
        let next_attempt = entry.retry_attempt.map(|attempt| attempt + 1).unwrap_or(1);

        {
            let mut state = self.lock_state();
            state.codex_totals.seconds_running += elapsed;
        }

        match reason {
            TerminationReason::Stalled => {
                let delay =
                    backoff_delay(next_attempt, effective.config.agent.max_retry_backoff_ms);
                let mut state = self.lock_state();
                schedule_retry(
                    &mut state,
                    issue_id,
                    &identifier,
                    next_attempt,
                    Some("stalled: no agent activity".to_string()),
                    delay,
                );
            }
            TerminationReason::Terminal | TerminationReason::NonActive => {
                {
                    let mut state = self.lock_state();
                    state.claimed.remove(issue_id);
                    state.retry_attempts.remove(issue_id);
                }
                if reason == TerminationReason::Terminal
                    && let Err(error) = self
                        .workspace_manager
                        .remove_for_issue(&identifier, &effective.config.hooks)
                        .await
                {
                    warn!(
                        target: "symphony",
                        "issue_id={issue_id} issue_identifier={identifier} outcome=failed action=workspace_cleanup error={error}"
                    );
                }
            }
        }
    }

    fn next_retry_instant(&self) -> Option<Instant> {
        let now_ms = monotonic_ms();
        let state = self.lock_state();
        state
            .retry_attempts
            .values()
            .map(|entry| {
                let remaining = entry.due_at_ms.saturating_sub(now_ms);
                Instant::now() + Duration::from_millis(remaining)
            })
            .min()
    }

    /// Fire retries whose due time has passed (Section 16.6).
    async fn fire_due_retries(&mut self) {
        let now_ms = monotonic_ms();
        let due: Vec<RetryEntry> = {
            let state = self.lock_state();
            let mut due: Vec<RetryEntry> = state
                .retry_attempts
                .values()
                .filter(|entry| entry.due_at_ms <= now_ms)
                .cloned()
                .collect();
            due.sort_by(|left, right| left.due_at_ms.cmp(&right.due_at_ms));
            due
        };

        for entry in due {
            self.handle_retry_timer(entry).await;
        }
    }

    async fn handle_retry_timer(&mut self, entry: RetryEntry) {
        let removed = { self.lock_state().retry_attempts.remove(&entry.issue_id) };
        if removed.is_none() {
            return;
        }

        let effective = self.read_effective();
        let candidates = match self.tracker.fetch_candidate_issues().await {
            Ok(candidates) => candidates,
            Err(error) => {
                warn!(
                    target: "symphony",
                    "issue_id={} outcome=failed action=retry_poll error={error}",
                    entry.issue_id
                );
                let mut state = self.lock_state();
                let delay = backoff_delay(
                    entry.attempt + 1,
                    effective.config.agent.max_retry_backoff_ms,
                );
                schedule_retry(
                    &mut state,
                    &entry.issue_id,
                    &entry.identifier,
                    entry.attempt + 1,
                    Some("retry poll failed".to_string()),
                    delay,
                );
                return;
            }
        };

        let Some(issue) = candidates.into_iter().find(|issue| issue.id == entry.issue_id) else {
            self.release_claim(&entry.issue_id, &entry.identifier, "not_a_candidate");
            return;
        };

        if is_blocked_by_non_terminal(&issue, &effective.config) {
            self.release_claim(&entry.issue_id, &entry.identifier, "blocked");
            return;
        }

        let slots = {
            let state = self.lock_state();
            state.available_slots_for_state(
                &issue.state,
                &effective.config.agent.max_concurrent_agents_by_state,
            )
        };

        if slots == 0 {
            let mut state = self.lock_state();
            let delay = backoff_delay(
                entry.attempt + 1,
                effective.config.agent.max_retry_backoff_ms,
            );
            schedule_retry(
                &mut state,
                &entry.issue_id,
                &issue.identifier,
                entry.attempt + 1,
                Some("no available orchestrator slots".to_string()),
                delay,
            );
            return;
        }

        self.dispatch_issue(issue, Some(entry.attempt)).await;
    }

    fn release_claim(&self, issue_id: &str, identifier: &str, reason: &str) {
        let mut state = self.lock_state();
        state.claimed.remove(issue_id);
        state.retry_attempts.remove(issue_id);
        info!(
            target: "symphony",
            "issue_id={issue_id} issue_identifier={identifier} outcome=released reason={reason}"
        );
    }

    /// Cancel and join all running workers.
    async fn shutdown_workers(&mut self) {
        let handles: Vec<WorkerHandle> = self.workers.drain().map(|(_, handle)| handle).collect();
        for handle in &handles {
            if handle.cancel.send(true).is_err() {
                debug!(
                    target: "symphony",
                    "outcome=ignored message=worker already finished during shutdown"
                );
            }
        }
        for handle in handles {
            handle.task.abort();
        }
    }

    fn log_running_summary(&self, phase: &str) {
        let state = self.lock_state();
        info!(
            target: "symphony",
            "phase={phase} running={} retrying={} claimed={} completed={} tokens_in={} tokens_out={}",
            state.running.len(),
            state.retry_attempts.len(),
            state.claimed.len(),
            state.completed.len(),
            state.codex_totals.input_tokens,
            state.codex_totals.output_tokens,
        );
    }
}

/// Runtime snapshot builder, shared with observability surfaces.
pub fn snapshot_of(state: &OrchestratorState) -> RuntimeSnapshot {
    let now = SystemTime::now();
    let mut running: Vec<SnapshotRunning> = state
        .running
        .values()
        .map(|entry| SnapshotRunning {
            issue_id: entry.attempt.issue_id.clone(),
            issue_identifier: entry.attempt.issue_identifier.clone(),
            state: entry.issue.state.clone(),
            session_id: entry.session.session_id.clone(),
            turn_count: entry.session.turn_count,
            last_event: entry.session.last_codex_event.clone(),
            last_message: entry.session.last_codex_message.clone(),
            started_at: entry.attempt.started_at,
            last_event_at: entry.session.last_codex_timestamp,
            tokens: TokenUsage {
                input_tokens: entry.session.codex_input_tokens,
                output_tokens: entry.session.codex_output_tokens,
                total_tokens: entry.session.codex_total_tokens,
            },
            workspace_path: entry.attempt.workspace_path.to_string_lossy().into_owned(),
            attempt: entry.attempt.attempt,
        })
        .collect();
    running.sort_by(|left, right| left.issue_identifier.cmp(&right.issue_identifier));

    let mut retrying: Vec<SnapshotRetry> = state
        .retry_attempts
        .values()
        .map(|entry| SnapshotRetry {
            issue_id: entry.issue_id.clone(),
            issue_identifier: entry.identifier.clone(),
            attempt: entry.attempt,
            due_at: now_plus_monotonic(entry.due_at_ms),
            error: entry.error.clone(),
        })
        .collect();
    retrying.sort_by(|left, right| left.issue_identifier.cmp(&right.issue_identifier));

    RuntimeSnapshot {
        generated_at: now,
        running,
        retrying,
        codex_totals: state.aggregated_totals(now),
        rate_limits: state.codex_rate_limits.clone(),
    }
}

/// Dispatch preflight validation (Section 6.3).
pub fn validate_dispatch_config(config: &ServiceConfig) -> Result<()> {
    if config.tracker.kind != "linear" {
        return Err(UnsupportedTrackerKind {
            kind: config.tracker.kind.clone(),
        });
    }
    if config.tracker.api_key.trim().is_empty() {
        return Err(MissingTrackerApiKey);
    }
    if config.tracker.project_slug.trim().is_empty() {
        return Err(MissingTrackerProjectSlug);
    }
    if config.codex.command.trim().is_empty() {
        return Err(ConfigValidation {
            message: "Codex command must not be empty".to_string(),
        });
    }
    Ok(())
}

/// Dispatch sort order: priority, then oldest creation, then identifier (Section 8.2).
pub fn sort_for_dispatch(mut issues: Vec<Issue>) -> Vec<Issue> {
    issues.sort_by(|left, right| {
        let left_priority = left.priority.unwrap_or(i32::MAX);
        let right_priority = right.priority.unwrap_or(i32::MAX);

        left_priority
            .cmp(&right_priority)
            .then_with(|| match (left.created_at, right.created_at) {
                (Some(left), Some(right)) => left.cmp(&right),
                (None, None) => Ordering::Equal,
                // Unknown creation times sort last.
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
            })
            .then_with(|| left.identifier.cmp(&right.identifier))
    });
    issues
}

/// The `Todo` blocker rule: a blocked `Todo` issue is not dispatch-eligible (Section 8.2).
pub fn is_blocked_by_non_terminal(issue: &Issue, config: &ServiceConfig) -> bool {
    if issue.normalized_state() != "todo" {
        return false;
    }
    issue.blocked_by.iter().any(|blocker| match &blocker.state {
        Some(state) => !config.tracker.is_terminal_state(state),
        // An unknown blocker state cannot be proven terminal.
        None => true,
    })
}

/// Exponential retry backoff with the configured cap (Section 8.4).
pub fn backoff_delay(attempt: u32, max_retry_backoff_ms: u64) -> u64 {
    let exponent = attempt.saturating_sub(1).min(32);
    let delay = RETRY_BACKOFF_BASE_MS.saturating_mul(1u64 << exponent);
    delay.min(max_retry_backoff_ms)
}

/// Select dispatch-eligible issues in order, honouring global and per-state slots.
pub fn select_dispatchable(
    candidates: &[Issue],
    config: &ServiceConfig,
    state: &OrchestratorState,
) -> Vec<Issue> {
    let mut selected = Vec::new();
    let mut planned_total = 0u32;
    let mut planned_by_state: HashMap<String, u32> = HashMap::new();

    for issue in candidates {
        if !issue.is_dispatchable() {
            continue;
        }
        if !config.tracker.is_active_state(&issue.state)
            || config.tracker.is_terminal_state(&issue.state)
        {
            continue;
        }
        if state.running.contains_key(&issue.id) || state.claimed.contains(&issue.id) {
            continue;
        }
        if is_blocked_by_non_terminal(issue, config) {
            debug!(
                target: "symphony",
                "issue_identifier={} outcome=skipped reason=non-terminal blockers",
                issue.identifier
            );
            continue;
        }

        let global_slots = state.available_slots().saturating_sub(planned_total);
        if global_slots == 0 {
            break;
        }

        let state_key = issue.normalized_state();
        let running_in_state = state.running_count_in_state(&issue.state) as u32;
        let planned_in_state = planned_by_state.get(&state_key).copied().unwrap_or(0);
        let state_slots = match config.agent.limit_for_state(&issue.state) {
            Some(limit) => limit
                .saturating_sub(running_in_state)
                .saturating_sub(planned_in_state),
            None => global_slots,
        };
        if state_slots == 0 {
            continue;
        }

        planned_total += 1;
        *planned_by_state.entry(state_key).or_insert(0) += 1;
        selected.push(issue.clone());
    }

    selected
}

/// Issues whose last agent activity has exceeded the stall timeout (Section 8.5).
pub fn stalled_issue_ids(
    state: &OrchestratorState,
    stall_timeout: Duration,
    now: SystemTime,
) -> Vec<(String, String)> {
    state
        .running
        .values()
        .filter(|entry| {
            let reference = entry
                .session
                .last_codex_timestamp
                .unwrap_or(entry.attempt.started_at);
            now.duration_since(reference)
                .map(|elapsed| elapsed > stall_timeout)
                .unwrap_or(false)
        })
        .map(|entry| {
            (
                entry.attempt.issue_id.clone(),
                entry.attempt.issue_identifier.clone(),
            )
        })
        .collect()
}

/// Apply the state transitions for a worker exit (Section 16.6).
pub fn worker_exit_transition(
    state: &mut OrchestratorState,
    issue_id: &str,
    entry: &RunningEntry,
    outcome: &WorkerOutcome,
    max_retry_backoff_ms: u64,
) -> ExitAction {
    let next_attempt = entry.retry_attempt.map(|attempt| attempt + 1).unwrap_or(1);
    let identifier = entry.attempt.issue_identifier.clone();

    match outcome {
        WorkerOutcome::Normal => {
            state.completed.insert(issue_id.to_string());
            schedule_retry(
                state,
                issue_id,
                &identifier,
                1,
                None,
                CONTINUATION_RETRY_DELAY_MS,
            );
            ExitAction::Continuation
        }
        WorkerOutcome::Failed(reason) => {
            let delay = backoff_delay(next_attempt, max_retry_backoff_ms);
            let error = format!("worker exited: {reason}");
            schedule_retry(
                state,
                issue_id,
                &identifier,
                next_attempt,
                Some(error.clone()),
                delay,
            );
            ExitAction::Retry {
                attempt: next_attempt,
                reason: error,
            }
        }
        WorkerOutcome::Canceled => {
            state.claimed.remove(issue_id);
            ExitAction::Released
        }
    }
}

/// Record a retry entry, replacing any existing timer for the issue.
fn schedule_retry(
    state: &mut OrchestratorState,
    issue_id: &str,
    identifier: &str,
    attempt: u32,
    error: Option<String>,
    delay_ms: u64,
) {
    state.retry_attempts.insert(
        issue_id.to_string(),
        RetryEntry {
            issue_id: issue_id.to_string(),
            identifier: identifier.to_string(),
            attempt,
            due_at_ms: monotonic_ms() + delay_ms,
            error,
        },
    );
    info!(
        target: "symphony",
        "issue_id={issue_id} issue_identifier={identifier} outcome=retry_scheduled attempt={attempt} delay_ms={delay_ms}"
    );
}

/// Run one worker attempt: workspace, hooks, agent turns, teardown (Section 16.5).
async fn run_worker<T: IssueTracker>(
    tracker: Arc<T>,
    workspace_manager: Arc<WorkspaceManager>,
    effective: Arc<RwLock<EffectiveWorkflow>>,
    issue: Issue,
    attempt: Option<u32>,
    generation: u64,
    mut cancel: watch::Receiver<bool>,
    events: mpsc::UnboundedSender<WorkerEvent>,
    tools: Vec<Arc<dyn ClientTool>>,
) {
    let issue_id = issue.id.clone();
    let outcome = run_worker_inner(
        tracker,
        Arc::clone(&workspace_manager),
        Arc::clone(&effective),
        &issue,
        attempt,
        generation,
        &mut cancel,
        &events,
        tools,
    )
    .await;

    send_event(
        &events,
        WorkerEvent::Exited {
            issue_id,
            generation,
            outcome,
        },
    );
}

#[allow(clippy::too_many_arguments)]
async fn run_worker_inner<T: IssueTracker>(
    tracker: Arc<T>,
    workspace_manager: Arc<WorkspaceManager>,
    effective: Arc<RwLock<EffectiveWorkflow>>,
    issue: &Issue,
    attempt: Option<u32>,
    generation: u64,
    cancel: &mut watch::Receiver<bool>,
    events: &mpsc::UnboundedSender<WorkerEvent>,
    tools: Vec<Arc<dyn ClientTool>>,
) -> WorkerOutcome {
    let config = read_effective(&effective);
    let workspace: Workspace = match workspace_manager
        .create_for_issue(&issue.identifier, &config.config.hooks)
        .await
    {
        Ok(workspace) => workspace,
        Err(error) => {
            return WorkerOutcome::Failed(format!("workspace error: {error}"));
        }
    };

    send_event(
        events,
        WorkerEvent::Started {
            issue_id: issue.id.clone(),
            generation,
            workspace_path: workspace.path.clone(),
        },
    );

    // `after_run` runs once the workspace exists, whatever the attempt outcome.
    let outcome = run_attempt(
        &tracker,
        &workspace_manager,
        &effective,
        &config,
        &workspace,
        issue,
        attempt,
        generation,
        cancel,
        events,
        tools,
    )
    .await;

    workspace_manager
        .finish_run(&workspace, &read_effective(&effective).config.hooks)
        .await;

    outcome
}

#[allow(clippy::too_many_arguments)]
async fn run_attempt<T: IssueTracker>(
    tracker: &Arc<T>,
    workspace_manager: &Arc<WorkspaceManager>,
    effective: &Arc<RwLock<EffectiveWorkflow>>,
    config: &EffectiveWorkflow,
    workspace: &Workspace,
    issue: &Issue,
    attempt: Option<u32>,
    generation: u64,
    cancel: &mut watch::Receiver<bool>,
    events: &mpsc::UnboundedSender<WorkerEvent>,
    tools: Vec<Arc<dyn ClientTool>>,
) -> WorkerOutcome {
    // Invariants 1 and 2: the launch cwd is the per-issue workspace inside the root.
    if let Err(error) = workspace.validate_launch_path(workspace_manager.root(), &workspace.path) {
        return WorkerOutcome::Failed(format!("invalid workspace cwd: {error}"));
    }

    if let Err(error) = workspace_manager
        .prepare_run(workspace, &config.config.hooks)
        .await
    {
        return WorkerOutcome::Failed(format!("before_run hook error: {error}"));
    }

    let mut client = match AgentClient::start(
        &config.config.codex,
        &workspace.path,
        &workspace.path,
        issue,
        tools,
    )
    .await
    {
        Ok(client) => client,
        Err(error) => {
            return WorkerOutcome::Failed(format!("agent session startup error: {error}"));
        }
    };

    // The agent client announces `session_started` once it has a thread and turn
    // identity to build the session id from.
    let mut current_issue = issue.clone();
    let mut turn_number = 1u32;
    let mut outcome = WorkerOutcome::Normal;

    loop {
        if *cancel.borrow() {
            outcome = WorkerOutcome::Canceled;
            break;
        }

        let prompt = if turn_number == 1 {
            let template = read_effective(effective).prompt_template;
            match prompt::render(&template, &prompt::build_context(&current_issue, attempt)) {
                Ok(prompt) => prompt,
                Err(error) => {
                    outcome = WorkerOutcome::Failed(format!("prompt error: {error}"));
                    break;
                }
            }
        } else {
            CONTINUATION_GUIDANCE.to_string()
        };

        let mut emit = |event: AgentEvent| {
            send_event(
                events,
                WorkerEvent::Agent {
                    issue_id: issue.id.clone(),
                    generation,
                    event,
                },
            );
        };

        match client.run_turn(&prompt, cancel, &mut emit).await {
            Ok(TurnTerminal::Completed) => {}
            Ok(TurnTerminal::Cancelled) => {
                outcome = WorkerOutcome::Canceled;
                break;
            }
            Ok(TurnTerminal::Failed(message)) => {
                outcome = WorkerOutcome::Failed(format!("agent turn error: {message}"));
                break;
            }
            Err(error) => {
                outcome = if matches!(error, TurnCancelled) {
                    WorkerOutcome::Canceled
                } else {
                    WorkerOutcome::Failed(format!("agent turn error: {error}"))
                };
                break;
            }
        }

        // Re-check the tracker before continuing on the same live thread.
        match tracker
            .fetch_issue_states_by_ids(vec![current_issue.id.clone()])
            .await
        {
            Ok(issues) => {
                if let Some(refreshed) = issues.into_iter().find(|issue| issue.id == current_issue.id)
                {
                    current_issue = refreshed;
                }
            }
            Err(error) => {
                outcome = WorkerOutcome::Failed(format!("issue state refresh error: {error}"));
                break;
            }
        }

        let config = read_effective(effective).config;
        if !config.tracker.is_active_state(&current_issue.state) {
            break;
        }
        if turn_number >= config.agent.max_turns {
            info!(
                target: "symphony",
                "issue_id={} issue_identifier={} outcome=max_turns_reached turns={turn_number}",
                current_issue.id,
                current_issue.identifier
            );
            break;
        }

        turn_number += 1;
    }

    client.stop().await;
    outcome
}

fn read_effective(effective: &Arc<RwLock<EffectiveWorkflow>>) -> EffectiveWorkflow {
    effective
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

fn send_event(events: &mpsc::UnboundedSender<WorkerEvent>, event: WorkerEvent) {
    if let Err(error) = events.send(event) {
        debug!(
            target: "symphony",
            "outcome=ignored message=orchestrator channel closed error={error}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AgentConfig, CodexConfig, EffectiveWorkflow, HooksConfig, PollingConfig, ServerConfig,
        TrackerConfig, WorkspaceConfig,
    };
    use crate::tracker::StaticTracker;
    use tempfile::TempDir;

    fn test_config() -> ServiceConfig {
        ServiceConfig {
            tracker: TrackerConfig {
                kind: "linear".to_string(),
                endpoint: "https://api.linear.app/graphql".to_string(),
                api_key: "test-key".to_string(),
                project_slug: "test-project".to_string(),
                active_states: vec!["Todo".to_string(), "In Progress".to_string()],
                terminal_states: vec!["Done".to_string(), "Cancelled".to_string()],
            },
            polling: PollingConfig { interval_ms: 30_000 },
            workspace: WorkspaceConfig {
                root: PathBuf::from("/tmp/symphony-test"),
            },
            hooks: HooksConfig::default(),
            agent: AgentConfig {
                max_concurrent_agents: 10,
                max_turns: 20,
                max_retry_backoff_ms: 300_000,
                max_concurrent_agents_by_state: HashMap::new(),
            },
            codex: CodexConfig {
                command: "true".to_string(),
                approval_policy: "never".to_string(),
                thread_sandbox: "workspace-write".to_string(),
                turn_sandbox_policy: "workspace-write".to_string(),
                turn_timeout_ms: 1_000,
                read_timeout_ms: 1_000,
                stall_timeout_ms: 300_000,
            },
            server: ServerConfig { port: None },
        }
    }

    fn issue(id: &str, identifier: &str, priority: Option<i32>, state: &str) -> Issue {
        Issue {
            id: id.to_string(),
            identifier: identifier.to_string(),
            title: format!("Issue {identifier}"),
            description: None,
            priority,
            state: state.to_string(),
            branch_name: None,
            url: None,
            labels: Vec::new(),
            blocked_by: Vec::new(),
            created_at: None,
            updated_at: None,
        }
    }

    fn running_entry(id: &str, identifier: &str, retry_attempt: Option<u32>) -> RunningEntry {
        RunningEntry {
            attempt: RunAttempt {
                issue_id: id.to_string(),
                issue_identifier: identifier.to_string(),
                attempt: retry_attempt,
                workspace_path: PathBuf::from("/tmp/ws"),
                started_at: SystemTime::now(),
                status: RunAttemptStatus::StreamingTurn,
                error: None,
            },
            issue: issue(id, identifier, Some(1), "Todo"),
            session: LiveSession::default(),
            retry_attempt,
        }
    }

    fn state_with_max(max_concurrent_agents: u32) -> OrchestratorState {
        OrchestratorState {
            poll_interval_ms: 30_000,
            max_concurrent_agents,
            ..OrchestratorState::default()
        }
    }

    #[test]
    fn sorts_by_priority_then_creation_then_identifier() {
        let mut oldest = issue("1", "A-1", Some(1), "Todo");
        oldest.created_at = Some(SystemTime::UNIX_EPOCH + Duration::from_secs(10));
        let mut newer = issue("2", "B-2", Some(1), "Todo");
        newer.created_at = Some(SystemTime::UNIX_EPOCH + Duration::from_secs(20));
        let unknown_created = issue("3", "C-3", Some(1), "Todo");
        let lowest_priority = issue("4", "D-4", Some(4), "Todo");
        let no_priority = issue("5", "E-5", None, "Todo");

        let sorted = sort_for_dispatch(vec![
            no_priority,
            lowest_priority,
            unknown_created,
            newer,
            oldest,
        ]);

        let order: Vec<&str> = sorted
            .iter()
            .map(|issue| issue.identifier.as_str())
            .collect();
        assert_eq!(order, vec!["A-1", "B-2", "C-3", "D-4", "E-5"]);
    }

    #[test]
    fn todo_with_non_terminal_blocker_is_not_dispatchable() {
        let config = test_config();

        let mut blocked = issue("1", "A-1", Some(1), "Todo");
        blocked.blocked_by = vec![BlockerRef {
            id: Some("b".to_string()),
            identifier: Some("A-0".to_string()),
            state: Some("In Progress".to_string()),
        }];
        assert!(is_blocked_by_non_terminal(&blocked, &config));

        let mut unknown_state = issue("2", "A-2", Some(1), "Todo");
        unknown_state.blocked_by = vec![BlockerRef {
            id: None,
            identifier: None,
            state: None,
        }];
        assert!(is_blocked_by_non_terminal(&unknown_state, &config));

        let mut resolved = issue("3", "A-3", Some(1), "Todo");
        resolved.blocked_by = vec![BlockerRef {
            id: Some("c".to_string()),
            identifier: Some("A-0".to_string()),
            state: Some("Done".to_string()),
        }];
        assert!(!is_blocked_by_non_terminal(&resolved, &config));

        // The blocker rule only applies to `Todo`.
        let mut in_progress = issue("4", "A-4", Some(1), "In Progress");
        in_progress.blocked_by = vec![BlockerRef {
            id: None,
            identifier: None,
            state: Some("Todo".to_string()),
        }];
        assert!(!is_blocked_by_non_terminal(&in_progress, &config));
    }

    #[test]
    fn backoff_is_exponential_and_capped() {
        assert_eq!(backoff_delay(1, 300_000), 10_000);
        assert_eq!(backoff_delay(2, 300_000), 20_000);
        assert_eq!(backoff_delay(3, 300_000), 40_000);
        assert_eq!(backoff_delay(6, 300_000), 300_000);
        assert_eq!(backoff_delay(30, 300_000), 300_000);
        assert_eq!(backoff_delay(1, 5_000), 5_000);
    }

    #[test]
    fn selection_respects_slots_states_and_blockers() {
        let mut config = test_config();
        config.agent.max_concurrent_agents = 2;

        let mut blocked = issue("blocked", "B-1", Some(1), "Todo");
        blocked.blocked_by = vec![BlockerRef {
            id: None,
            identifier: None,
            state: Some("Todo".to_string()),
        }];

        let candidates = sort_for_dispatch(vec![
            issue("todo", "T-1", Some(1), "Todo"),
            issue("progress", "P-1", Some(2), "In Progress"),
            blocked,
            issue("done", "D-1", Some(1), "Done"),
            issue("other", "O-1", Some(1), "Backlog"),
        ]);

        let state = state_with_max(2);
        let selected = select_dispatchable(&candidates, &config, &state);
        let identifiers: Vec<&str> = selected
            .iter()
            .map(|issue| issue.identifier.as_str())
            .collect();
        assert_eq!(identifiers, vec!["T-1", "P-1"]);
    }

    #[test]
    fn selection_skips_claimed_and_running_issues() {
        let config = test_config();
        let candidates = vec![
            issue("running", "R-1", Some(1), "Todo"),
            issue("claimed", "C-1", Some(1), "Todo"),
            issue("fresh", "F-1", Some(1), "Todo"),
        ];

        let mut state = state_with_max(10);
        state.running.insert(
            "running".to_string(),
            running_entry("running", "R-1", None),
        );
        state.claimed.insert("claimed".to_string());

        let selected = select_dispatchable(&candidates, &config, &state);
        let identifiers: Vec<&str> = selected
            .iter()
            .map(|issue| issue.identifier.as_str())
            .collect();
        assert_eq!(identifiers, vec!["F-1"]);
    }

    #[test]
    fn per_state_limits_apply() {
        let mut config = test_config();
        config.agent.max_concurrent_agents = 10;
        config
            .agent
            .max_concurrent_agents_by_state
            .insert("todo".to_string(), 1);

        let candidates = vec![
            issue("1", "T-1", Some(1), "Todo"),
            issue("2", "T-2", Some(1), "Todo"),
            issue("3", "P-1", Some(1), "In Progress"),
        ];

        let selected = select_dispatchable(&candidates, &config, &state_with_max(10));
        let identifiers: Vec<&str> = selected
            .iter()
            .map(|issue| issue.identifier.as_str())
            .collect();
        assert_eq!(identifiers, vec!["T-1", "P-1"]);
    }

    #[test]
    fn selection_stops_when_global_slots_are_gone() {
        let mut config = test_config();
        config.agent.max_concurrent_agents = 1;

        let candidates = vec![
            issue("1", "T-1", Some(1), "Todo"),
            issue("2", "T-2", Some(2), "Todo"),
        ];

        let selected = select_dispatchable(&candidates, &config, &state_with_max(1));
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].identifier, "T-1");
    }

    #[test]
    fn normal_exit_schedules_a_continuation_retry() {
        let entry = running_entry("issue-1", "T-1", None);
        let mut state = OrchestratorState::default();

        let action = worker_exit_transition(
            &mut state,
            "issue-1",
            &entry,
            &WorkerOutcome::Normal,
            300_000,
        );

        assert_eq!(action, ExitAction::Continuation);
        assert!(state.completed.contains("issue-1"));
        let retry = state.retry_attempts.get("issue-1").expect("retry queued");
        assert_eq!(retry.attempt, 1);
        assert!(retry.error.is_none());
        let delay = retry.due_at_ms.saturating_sub(monotonic_ms());
        assert!(delay <= CONTINUATION_RETRY_DELAY_MS);
    }

    #[test]
    fn abnormal_exit_uses_exponential_backoff() {
        let entry = running_entry("issue-1", "T-1", Some(2));
        let mut state = OrchestratorState::default();

        let action = worker_exit_transition(
            &mut state,
            "issue-1",
            &entry,
            &WorkerOutcome::Failed("boom".to_string()),
            300_000,
        );

        assert!(matches!(action, ExitAction::Retry { attempt: 3, .. }));
        let retry = state.retry_attempts.get("issue-1").expect("retry queued");
        assert_eq!(retry.attempt, 3);
        assert!(
            retry
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("boom")
        );

        // attempt 3 -> 10000 * 2^2 = 40s
        let delay = retry.due_at_ms.saturating_sub(monotonic_ms());
        assert!(delay > 39_000 && delay <= 40_000, "delay was {delay}");
    }

    #[test]
    fn abnormal_exit_respects_the_backoff_cap() {
        let entry = running_entry("issue-1", "T-1", Some(9));
        let mut state = OrchestratorState::default();

        worker_exit_transition(
            &mut state,
            "issue-1",
            &entry,
            &WorkerOutcome::Failed("boom".to_string()),
            60_000,
        );

        let retry = state.retry_attempts.get("issue-1").expect("retry queued");
        let delay = retry.due_at_ms.saturating_sub(monotonic_ms());
        assert!(delay <= 60_000, "delay was {delay}");
    }

    #[test]
    fn canceled_exit_releases_the_claim() {
        let entry = running_entry("issue-1", "T-1", None);
        let mut state = OrchestratorState::default();
        state.claimed.insert("issue-1".to_string());

        let action =
            worker_exit_transition(&mut state, "issue-1", &entry, &WorkerOutcome::Canceled, 300_000);

        assert_eq!(action, ExitAction::Released);
        assert!(!state.claimed.contains("issue-1"));
        assert!(state.retry_attempts.is_empty());
        assert!(!state.completed.contains("issue-1"));
    }

    #[test]
    fn stall_detection_uses_last_event_then_started_at() {
        let mut state = OrchestratorState::default();
        let now = SystemTime::now();

        let mut stalled = running_entry("stalled", "S-1", None);
        stalled.attempt.started_at = now - Duration::from_secs(30);
        state.running.insert("stalled".to_string(), stalled);

        let mut active = running_entry("active", "A-1", None);
        active.attempt.started_at = now - Duration::from_secs(30);
        active.session.last_codex_timestamp = Some(now);
        state.running.insert("active".to_string(), active);

        let stalled = stalled_issue_ids(&state, Duration::from_secs(1), now);
        assert_eq!(stalled.len(), 1);
        assert_eq!(stalled[0].0, "stalled");
    }

    #[test]
    fn stall_detection_is_disabled_by_zero_timeout() {
        let mut state = OrchestratorState::default();
        let now = SystemTime::now();
        let mut entry = running_entry("stalled", "S-1", None);
        entry.attempt.started_at = now - Duration::from_secs(600);
        state.running.insert("stalled".to_string(), entry);

        // The caller skips detection entirely when the timeout is zero; prove the
        // helper only reports what it is asked to.
        assert_eq!(stalled_issue_ids(&state, Duration::from_secs(1), now).len(), 1);
    }

    #[test]
    fn token_totals_avoid_double_counting() {
        let tracker = StaticTracker::default();
        let directory = TempDir::new().expect("temp dir");
        let effective = EffectiveWorkflow {
            path: directory.path().join("WORKFLOW.md"),
            config: test_config(),
            prompt_template: prompt::parse("prompt").expect("template"),
        };
        let orchestrator = Orchestrator::new(
            tracker,
            WorkspaceManager::new(directory.path().to_path_buf()),
            effective,
            Vec::new(),
        )
        .expect("orchestrator builds");

        {
            let mut state = orchestrator.lock_state();
            state
                .running
                .insert("issue-1".to_string(), running_entry("issue-1", "T-1", None));
        }

        // Absolute totals are folded as deltas; repeated deltas are not re-added.
        orchestrator.apply_agent_event(
            "issue-1",
            AgentEvent::TokenUsage {
                usage: TokenUsage {
                    input_tokens: 100,
                    output_tokens: 50,
                    total_tokens: 150,
                },
            },
        );
        orchestrator.apply_agent_event(
            "issue-1",
            AgentEvent::TokenUsage {
                usage: TokenUsage {
                    input_tokens: 100,
                    output_tokens: 50,
                    total_tokens: 150,
                },
            },
        );
        orchestrator.apply_agent_event(
            "issue-1",
            AgentEvent::TokenUsage {
                usage: TokenUsage {
                    input_tokens: 130,
                    output_tokens: 60,
                    total_tokens: 190,
                },
            },
        );

        let state = orchestrator.lock_state();
        assert_eq!(state.codex_totals.input_tokens, 130);
        assert_eq!(state.codex_totals.output_tokens, 60);
        assert_eq!(state.codex_totals.total_tokens, 190);
    }

    #[test]
    fn snapshot_reports_running_retrying_and_totals() {
        let mut state = OrchestratorState {
            max_concurrent_agents: 10,
            ..OrchestratorState::default()
        };
        let mut entry = running_entry("issue-1", "T-1", None);
        entry.session.session_id = Some("thr-1-turn-1".to_string());
        entry.session.turn_count = 7;
        entry.session.last_codex_event = Some("turn_completed".to_string());
        entry.session.codex_input_tokens = 1_200;
        entry.session.codex_output_tokens = 800;
        entry.session.codex_total_tokens = 2_000;
        state.running.insert("issue-1".to_string(), entry);
        state.retry_attempts.insert(
            "issue-2".to_string(),
            RetryEntry {
                issue_id: "issue-2".to_string(),
                identifier: "T-2".to_string(),
                attempt: 3,
                due_at_ms: monotonic_ms() + 5_000,
                error: Some("no available orchestrator slots".to_string()),
            },
        );
        state.codex_totals.input_tokens = 5_000;
        state.codex_totals.output_tokens = 2_400;
        state.codex_totals.total_tokens = 7_400;
        state.codex_totals.seconds_running = 1_800.0;

        let snapshot = snapshot_of(&state);
        assert_eq!(snapshot.running.len(), 1);
        assert_eq!(snapshot.running[0].turn_count, 7);
        assert_eq!(snapshot.retrying.len(), 1);
        assert_eq!(snapshot.retrying[0].attempt, 3);
        // Aggregate runtime includes the active session.
        assert!(snapshot.codex_totals.seconds_running >= 1_800.0);

        let json = snapshot.to_json();
        assert_eq!(json["counts"]["running"], 1);
        assert_eq!(json["counts"]["retrying"], 1);
        assert_eq!(json["running"][0]["issue_identifier"], "T-1");
        assert_eq!(json["running"][0]["tokens"]["total_tokens"], 2_000);
        assert_eq!(json["retrying"][0]["issue_id"], "issue-2");
        assert_eq!(json["codex_totals"]["total_tokens"], 7_400);
    }

    #[test]
    fn startup_cleanup_removes_terminal_workspaces_and_keeps_going() {
        let mut tracker = StaticTracker::default();
        tracker.by_states = vec![issue("t-1", "T-1", Some(1), "Done")];

        let directory = TempDir::new().expect("temp dir");
        let root = directory.path().to_path_buf();
        let workspace_path = root.join("T-1");
        std::fs::create_dir_all(&workspace_path).expect("workspace");

        let mut config = test_config();
        config.workspace.root = root.clone();
        let effective = EffectiveWorkflow {
            path: directory.path().join("WORKFLOW.md"),
            config,
            prompt_template: prompt::parse("prompt").expect("template"),
        };

        let orchestrator = Orchestrator::new(
            tracker,
            WorkspaceManager::new(root),
            effective,
            Vec::new(),
        )
        .expect("orchestrator builds");

        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(orchestrator.startup_cleanup());

        assert!(!workspace_path.exists());
    }
}
