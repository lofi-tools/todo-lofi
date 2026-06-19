use crate::domain::*;
use crate::error::Result;
use crate::error::SymphonyError::*;
use crate::tracker::IssueTracker;
use crate::workspace::WorkspaceManager;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};
use tokio::runtime::Runtime;
use log::{info, error};

/// Orchestrator that manages the scheduling and execution of agent runs.
pub struct Orchestrator<T: IssueTracker> {
    /// Tracker for fetching issues.
    tracker: Arc<T>,
    /// Workspace manager.
    workspace_manager: WorkspaceManager,
    /// Configuration.
    config: Arc<crate::config::ServiceConfig>,
    /// Current orchestrator state.
    state: Arc<Mutex<OrchestratorState>>,
    /// Runtime for async tasks.
    tokio_runtime: Runtime,
    /// Whether the orchestrator is running.
    running: Arc<Mutex<bool>>,
}

#[derive(Debug, Clone)]
pub struct OrchestratorConfig {
    /// Initial poll interval in milliseconds.
    pub poll_interval_ms: u64,
    /// Maximum concurrent agents.
    pub max_concurrent_agents: u32,
}

impl<T: IssueTracker + 'static> Orchestrator<T> {
    /// Create a new orchestrator.
    pub fn new(
        tracker: T,
        workspace_manager: WorkspaceManager,
        config: crate::config::ServiceConfig,
    ) -> Result<Self> {
        let tokio_runtime = Runtime::new()
            .map_err(|e| OrchestratorError {
                message: format!("Failed to create Tokio runtime: {}", e),
            })?;
        
        Ok(Self {
            tracker: Arc::new(tracker),
            workspace_manager,
            config: Arc::new(config),
            state: Arc::new(Mutex::new(OrchestratorState {
                poll_interval_ms: 30000, // Will be updated from config
                max_concurrent_agents: 10, // Will be updated from config
                running: HashMap::new(),
                claimed: HashSet::new(),
                retry_attempts: HashMap::new(),
                completed: HashSet::new(),
                codex_totals: CodexTotals {
                    input_tokens: 0,
                    output_tokens: 0,
                    total_tokens: 0,
                    seconds_running: 0,
                },
                codex_rate_limits: None,
            })),
            tokio_runtime,
            running: Arc::new(Mutex::new(false)),
        })
    }
    
    /// Start the orchestrator main loop.
    pub fn start(&self) -> Result<()> {
        let running = self.running.clone();
        let mut running_guard = running.lock().unwrap();
        *running_guard = true;
        drop(running_guard);
        
        info!("Starting symphony orchestrator");
        
        // Perform startup cleanup
        self.startup_cleanup()?;
        
        // Schedule the first tick immediately
        self.tick()?;
        
        // Main loop
        loop {
            {
                let running = self.running.lock().unwrap();
                if !*running {
                    break;
                }
            }
            
            // Sleep for the poll interval
            {
                let state = self.state.lock().unwrap();
                thread::sleep(Duration::from_millis(state.poll_interval_ms));
            }
            
            // Perform a tick
            if let Err(e) = self.tick() {
                error!("Error in orchestrator tick: {}", e);
                // Continue running despite errors
            }
        }
        
        info!("Symphony orchestrator stopped");
        Ok(())
    }
    
    /// Stop the orchestrator.
    pub fn stop(&self) {
        let mut running = self.running.lock().unwrap();
        *running = false;
    }
    
    /// Perform startup cleanup (remove workspaces for terminal issues).
    fn startup_cleanup(&self) -> Result<()> {
        info!("Performing startup cleanup for terminal states");
        
        // Fetch issues in terminal states
        let terminal_issues = self
            .tokio_runtime
            .block_on(self.tracker.fetch_issues_by_states(
                self.config.tracker.terminal_states.clone(),
            ))?;
        
        for issue in terminal_issues {
            info!("Cleaning up workspace for terminal issue: {}", issue.identifier);
            let _ = self.workspace_manager.remove_workspace(&issue.identifier);
        }
        
        Ok(())
    }
    
    /// Perform one orchestration tick.
    fn tick(&self) -> Result<()> {
        info!("Performing orchestrator tick");
        
        // 1. Reconcile running issues
        self.reconcile_running_issues()?;
        
        // 2. Run dispatch preflight validation
        self.validate_dispatch_preflight()?;
        
        // 3. Fetch candidate issues
        let candidate_issues = self
            .tokio_runtime
            .block_on(self.tracker.fetch_candidate_issues())?;
        
        // 4. Sort issues by dispatch priority
        let mut sorted_issues = self.sort_issues_by_priority(candidate_issues);
        
        // 5. Dispatch eligible issues while slots remain
        self.dispatch_issues(&mut sorted_issues)?;
        
        Ok(())
    }
    
    /// Reconcile running issues (check for stalls, state changes).
    fn reconcile_running_issues(&self) -> Result<()> {
        let state = self.state.lock().unwrap();
        let issue_ids: Vec<String> = state.running.keys().cloned().collect();
        drop(state);
        
        if issue_ids.is_empty() {
            return Ok(());
        }
        
        info!("Reconciling {} running issues", issue_ids.len());
        
        // Fetch current states for running issues
        let current_states = self
            .tokio_runtime
            .block_on(self.tracker.fetch_issue_states_by_ids(issue_ids.clone()))?;
        
        let mut state = self.state.lock().unwrap();
        
        for issue_id in issue_ids {
            if let Some(mut run_attempt) = state.running.remove(&issue_id) {
                // Check if we have current state info
                if let Some(current_state) = current_states.get(&issue_id) {
                    let current_state_lower = current_state.to_lowercase();
                    
                    // Check if state is now terminal
                    if self.config.tracker.terminal_states.iter().any(|s| {
                        s.to_lowercase() == current_state_lower
                    }) {
                        info!("Issue {} is now terminal ({}), terminating run", 
                              issue_id, current_state);
                        
                        // Mark as finished (in reality, we'd signal the agent to stop)
                        run_attempt.status = RunAttemptStatus::Finishing;
                        // We would normally wait for the agent to finish here
                        
                        // For now, we'll just mark it as succeeded and clean up
                        run_attempt.status = RunAttemptStatus::Succeeded;
                        state.completed.insert(issue_id);
                        
                        // Clean up workspace
                        let _ = self.workspace_manager.remove_workspace(&run_attempt.issue_identifier);
                        continue;
                    }
                    
                    // Check if state is no longer active
                    let is_active = self.config.tracker.active_states.iter().any(|s| {
                        s.to_lowercase() == current_state_lower
                    }) && !self.config.tracker.terminal_states.iter().any(|s| {
                        s.to_lowercase() == current_state_lower
                    });
                    
                    if !is_active {
                        info!("Issue {} is no longer active ({}), terminating run", 
                              issue_id, current_state);
                        
                        run_attempt.status = RunAttemptStatus::CanceledByReconciliation;
                        // We would normally signal the agent to stop
                        
                        // Clean up workspace
                        let _ = self.workspace_manager.remove_workspace(&run_attempt.issue_identifier);
                        continue;
                    }
                    
                    // Issue is still active, put it back
                    state.running.insert(issue_id, run_attempt);
                } else {
                    // No state info available, assume still active and keep running
                    state.running.insert(issue_id, run_attempt);
                }
            }
        }
        
        // TODO: Implement actual stall detection based on last event timestamps
        // This would require tracking timestamps in the LiveSession
        
        Ok(())
    }
    
    /// Validate that we can dispatch new work.
    fn validate_dispatch_preflight(&self) -> Result<()> {
        // In a real implementation, we would validate the workflow/config
        // For now, we'll just check that we have a valid tracker config
        
        if self.config.tracker.api_key.is_empty() {
            return Err(MissingTrackerApiKey);
        }
        
        if self.config.tracker.kind == "linear" && self.config.tracker.project_slug.is_empty() {
            return Err(MissingTrackerProjectSlug);
        }
        
        if self.config.codex.command.is_empty() {
            return Err(ConfigValidation {
                message: "Codex command must not be empty".to_string(),
            });
        }
        
        Ok(())
    }
    
    /// Sort issues by dispatch priority.
    fn sort_issues_by_priority(&self, mut issues: Vec<Issue>) -> Vec<Issue> {
        issues.sort_by(|a, b| {
            // 1. Priority ascending (lower numbers = higher priority)
            let a_prio = a.priority.unwrap_or(i32::MAX);
            let b_prio = b.priority.unwrap_or(i32::MAX);
            let priority_cmp = a_prio.cmp(&b_prio);
            if priority_cmp != std::cmp::Ordering::Equal {
                return priority_cmp;
            }
            
            // 2. Created_at oldest first
            let a_created = a.created_at.unwrap_or(SystemTime::UNIX_EPOCH);
            let b_created = b.created_at.unwrap_or(SystemTime::UNIX_EPOCH);
            let created_cmp = a_created.cmp(&b_created);
            if created_cmp != std::cmp::Ordering::Equal {
                return created_cmp;
            }
            
            // 3. Identifier lexicographic tie-breaker
            a.identifier.cmp(&b.identifier)
        });
        
        issues
    }
    
    /// Dispatch issues to available slots.
    fn dispatch_issues(&self, issues: &mut Vec<Issue>) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        
        // Calculate available slots
        let running_count = state.running.len() as u32;
        let global_max = state.max_concurrent_agents;
        let mut available_slots = global_max.saturating_sub(running_count);
        
        info!("Dispatching issues: {} running, {} available slots", 
              running_count, available_slots);
        
        // Process issues in priority order
        issues.retain(|issue| {
            // Skip if already claimed or running
            if state.claimed.contains(&issue.id) {
                return false;
            }
            
            // Check per-state limits
            let state_limit = self.config.agent.max_concurrent_agents_by_state.get(&issue.state.to_lowercase());
            if let Some(limit) = state_limit {
                // Count how many we're currently running in this state
                let state_running_count = state.running.values()
                    .filter(|run| run.issue_id == issue.id) // Simplified - should check issue state
                    .count() as u32;
                
                if state_running_count >= *limit {
                    return false;
                }
            }
            
            // If we have available slots, dispatch this issue
            if available_slots > 0 {
                info!("Dispatching issue: {}", issue.identifier);
                
                // Mark as claimed
                state.claimed.insert(issue.id.clone());
                available_slots -= 1;
                
                // Actually dispatch in a separate thread (in real impl, this would be async)
                let tracker = Arc::clone(&self.tracker);
                let workspace_manager = self.workspace_manager.clone();
                let config = Arc::clone(&self.config);
                let state = Arc::clone(&self.state);
                let issue_id = issue.id.clone();
                let issue_identifier = issue.identifier.clone();
                
                thread::spawn(move || {
                    if let Err(e) = Self::dispatch_issue_inner(
                        tracker,
                        workspace_manager,
                        config,
                        Arc::clone(&state),
                        &issue_id,
                        &issue_identifier,
                    ) {
                        error!("Failed to dispatch issue {}: {}", issue_identifier, e);
                        
                        // Clean up claim on failure
                        {
                            let mut s = state.lock().unwrap();
                            s.claimed.remove(&issue_id);
                        }
                        
                        // Schedule a retry
                        Self::schedule_retry(
                            &state,
                            &issue_id,
                            &issue_identifier,
                            1,
                            format!("Dispatch failed: {}", e),
                        );
                    }
                });
                
                // Continue to next issue (don't retain)
                false
            } else {
                // No slots available, keep this issue for next time
                true
            }
        });
        
        Ok(())
    }
    
    /// Internal function to dispatch a single issue.
    fn dispatch_issue_inner(
        _tracker: Arc<T>,
        workspace_manager: WorkspaceManager,
        _config: Arc<crate::config::ServiceConfig>,
        state: Arc<Mutex<OrchestratorState>>,
        issue_id: &str,
        issue_identifier: &str,
    ) -> Result<()> {
        info!("Starting dispatch for issue {}", issue_identifier);
        
        // Fetch the full issue details (in a real impl, we'd have this from fetch_candidate_issues)
        // For simplicity, we'll just use the identifier - in reality we'd need the full issue
        // This is a simplification for the example
        
        // Create a minimal issue for demonstration
        let issue = Issue {
            id: issue_id.to_string(),
            identifier: issue_identifier.to_string(),
            title: format!("Issue {}", issue_identifier),
            description: None,
            priority: None,
            state: "Todo".to_string(), // Would be fetched from tracker
            branch_name: None,
            url: None,
            labels: vec![],
            blocked_by: vec![],
            created_at: None,
            updated_at: None,
        };
        
        // Get or create workspace
        let workspace_path = workspace_manager.get_or_create_workspace(&issue.identifier)?;
        
        // Validate the workspace path is within root
        workspace_manager.validate_path(&workspace_path)?;
        
        // Load workflow and build prompt
        // In a real implementation, we would:
        // 1. Load WORKFLOW.md from the workspace
        // 2. Parse it
        // 3. Render the template with the issue data
        // For now, we'll use a simple prompt
        let _prompt = format!("You are working on issue {}: {}", issue.identifier, issue.title);
        
        // Update running state
        {
            let mut state = state.lock().unwrap();
            state.running.insert(
                issue_id.to_string(),
                RunAttempt {
                    issue_id: issue_id.to_string(),
                    issue_identifier: issue_identifier.to_string(),
                    attempt: None, // First attempt
                    workspace_path: workspace_path.to_string_lossy().into_owned(),
                    started_at: SystemTime::now(),
                    status: RunAttemptStatus::PreparingWorkspace,
                    error: None,
                }
            );
        }
        
        // In a real implementation, we would:
        // 1. Create an agent runner
        // 2. Run the attempt
        // 3. Handle the result
        // For now, we'll simulate doing work
        
        info!("Running agent for issue {} in workspace {}", 
              issue_identifier, workspace_path.display());
        
        // Simulate some work
        thread::sleep(Duration::from_secs(2));
        
        // Mark as completed
        {
            let mut state = state.lock().unwrap();
            if let Some(mut run_attempt) = state.running.remove(issue_id) {
                run_attempt.status = RunAttemptStatus::Succeeded;
                state.completed.insert(issue_id.to_string());
                
                // Update token totals (simulated)
                state.codex_totals.input_tokens += 100;
                state.codex_totals.output_tokens += 50;
                state.codex_totals.total_tokens += 150;
                state.codex_totals.seconds_running += 2;
            }
        }
        
        // Clean up claim
        {
            let mut state = state.lock().unwrap();
            state.claimed.remove(issue_id);
        }
        
        info!("Completed dispatch for issue {}", issue_identifier);
        
        Ok(())
    }
    
    /// Schedule a retry for an issue.
    fn schedule_retry(
        state: &Arc<Mutex<OrchestratorState>>,
        issue_id: &str,
        issue_identifier: &str,
        attempt: u32,
        error: String,
    ) {
        // Calculate delay based on attempt number
        let delay_ms = if attempt == 1 {
            // First retry after quick failure - short delay
            1000
        } else {
            // Exponential backoff: 10000 * 2^(attempt-1), capped by max_retry_backoff_ms
            let base = 10000;
            let exp = attempt - 1;
            let delay = base * 2u64.pow(exp);
            let max_delay = 300000; // 5 minutes from spec
            if delay > max_delay {
                max_delay
            } else {
                delay
            }
        };
        
        let due_at_ms = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            + delay_ms;
        
        let mut state = state.lock().unwrap();
        state.retry_attempts.insert(
            issue_id.to_string(),
            RetryEntry {
                issue_id: issue_id.to_string(),
                identifier: issue_identifier.to_string(),
                attempt,
                due_at_ms,
                timer_handle: None, // In a real impl, we'd store the timer handle here
                error: Some(error),
            }
        );
        
        info!("Scheduled retry {} for issue {} in {} ms", 
              attempt, issue_identifier, delay_ms);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use crate::config::{HooksConfig, ServiceConfig, TrackerConfig, PollingConfig, WorkspaceConfig, AgentConfig, CodexConfig};
    
    // Mock tracker for testing
    struct MockTracker {
        issues: Vec<Issue>,
    }
    
    #[async_trait::async_trait]
    impl IssueTracker for MockTracker {
        async fn fetch_candidate_issues(&self) -> Result<Vec<Issue>> {
            Ok(self.issues.clone())
        }
        
        async fn fetch_issues_by_states(&self, _state_names: Vec<String>) -> Result<Vec<Issue>> {
            Ok(Vec::new())
        }
        
        async fn fetch_issue_states_by_ids(&self, _issue_ids: Vec<String>) -> Result<HashMap<String, String>> {
            Ok(HashMap::new())
        }
    }
    
    #[test]
    fn test_orchestrator_creation() {
        let temp_dir = TempDir::new().unwrap();
        let workspace_manager = WorkspaceManager::new(
            temp_dir.path().join("workspaces"),
            HooksConfig::default()
        );
        
        let config = ServiceConfig {
            tracker: TrackerConfig {
                kind: "linear".to_string(),
                endpoint: "https://api.linear.app/graphql".to_string(),
                api_key: "test-key".to_string(),
                project_slug: "test-project".to_string(),
                active_states: vec!["Todo".to_string(), "In Progress".to_string()],
                terminal_states: vec!["Done".to_string(), "Cancelled".to_string()],
            },
            polling: PollingConfig { interval_ms: 30000 },
            workspace: WorkspaceConfig { root: temp_dir.path().join("workspaces") },
            hooks: HooksConfig::default(),
            agent: AgentConfig {
                max_concurrent_agents: 10,
                max_turns: 20,
                max_retry_backoff_ms: 300000,
                max_concurrent_agents_by_state: HashMap::new(),
            },
            codex: CodexConfig {
                command: "echo test".to_string(),
                approval_policy: "test".to_string(),
                thread_sandbox: "test".to_string(),
                turn_sandbox_policy: "test".to_string(),
                turn_timeout_ms: 3600000,
                read_timeout_ms: 5000,
                stall_timeout_ms: 300000,
                max_turns: 20,
            },
        };
        
        let tracker = MockTracker { issues: Vec::new() };
        let orchestrator = Orchestrator::new(tracker, workspace_manager, config).unwrap();
        let _ = orchestrator.tokio_runtime.handle().clone();
    }
    
    #[test]
    fn test_sort_issues_by_priority() {
        let temp_dir = TempDir::new().unwrap();
        let workspace_manager = WorkspaceManager::new(
            temp_dir.path().join("workspaces"),
            HooksConfig::default()
        );
        
        let config = ServiceConfig {
            tracker: TrackerConfig {
                kind: "linear".to_string(),
                endpoint: "https://api.linear.app/graphql".to_string(),
                api_key: "test-key".to_string(),
                project_slug: "test-project".to_string(),
                active_states: vec!["Todo".to_string(), "In Progress".to_string()],
                terminal_states: vec!["Done".to_string(), "Cancelled".to_string()],
            },
            polling: PollingConfig { interval_ms: 30000 },
            workspace: WorkspaceConfig { root: temp_dir.path().join("workspaces") },
            hooks: HooksConfig::default(),
            agent: AgentConfig {
                max_concurrent_agents: 10,
                max_turns: 20,
                max_retry_backoff_ms: 300000,
                max_concurrent_agents_by_state: HashMap::new(),
            },
            codex: CodexConfig {
                command: "echo test".to_string(),
                approval_policy: "test".to_string(),
                thread_sandbox: "test".to_string(),
                turn_sandbox_policy: "test".to_string(),
                turn_timeout_ms: 3600000,
                read_timeout_ms: 5000,
                stall_timeout_ms: 300000,
                max_turns: 20,
            },
        };
        
        let tracker = MockTracker { issues: Vec::new() };
        let orchestrator = Orchestrator::new(tracker, workspace_manager, config).unwrap();
        
        let issues = vec![
            Issue {
                id: "3".to_string(),
                identifier: "C-3".to_string(),
                title: "Issue C".to_string(),
                description: None,
                priority: Some(2),
                state: "Todo".to_string(),
                branch_name: None,
                url: None,
                labels: vec![],
                blocked_by: vec![],
                created_at: None,
                updated_at: None,
            },
            Issue {
                id: "1".to_string(),
                identifier: "A-1".to_string(),
                title: "Issue A".to_string(),
                description: None,
                priority: Some(1),
                state: "Todo".to_string(),
                branch_name: None,
                url: None,
                labels: vec![],
                blocked_by: vec![],
                created_at: None,
                updated_at: None,
            },
            Issue {
                id: "2".to_string(),
                identifier: "B-2".to_string(),
                title: "Issue B".to_string(),
                description: None,
                priority: Some(1),
                state: "Todo".to_string(),
                branch_name: None,
                url: None,
                labels: vec![],
                blocked_by: vec![],
                created_at: None,
                updated_at: None,
            },
        ];
        
        let sorted = orchestrator.sort_issues_by_priority(issues);
        
        // Should be sorted by priority first (1,1,2), then by identifier for same priority
        assert_eq!(sorted[0].identifier, "A-1"); // priority 1
        assert_eq!(sorted[1].identifier, "B-2"); // priority 1, identifier B-2 < C-3
        assert_eq!(sorted[2].identifier, "C-3"); // priority 2
    }
}