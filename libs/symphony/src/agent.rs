use crate::domain::*;
use crate::error::Result;
use crate::error::SymphonyError::*;
use log::{error, info};
use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// Agent runner that manages the coding agent process.
pub struct AgentRunner {
    /// Configuration for the codex agent.
    codex_config: crate::config::CodexConfig,
    /// Callback to send events to the orchestrator.
    event_callback: Arc<dyn Fn(AgentEvent) + Send + Sync>,
}

#[derive(Debug, Clone)]
pub enum AgentEvent {
    SessionStarted {
        session_id: String,
        pid: u32,
    },
    StartupFailed {
        error: String,
    },
    TurnCompleted {
        turn_id: String,
        session_id: String,
    },
    TurnFailed {
        turn_id: String,
        session_id: String,
        error: String,
    },
    TurnCancelled {
        turn_id: String,
        session_id: String,
    },
    TurnEndedWithError {
        turn_id: String,
        session_id: String,
        error: String,
    },
    TurnInputRequired {
        turn_id: String,
        session_id: String,
        prompt: String,
    },
    ApprovalAutoApproved {
        turn_id: String,
        session_id: String,
        request_type: String,
    },
    UnsupportedToolCall {
        turn_id: String,
        session_id: String,
        tool_name: String,
    },
    Notification {
        turn_id: String,
        session_id: String,
        message: String,
    },
    OtherMessage {
        turn_id: String,
        session_id: String,
        message: String,
    },
    Malformed {
        turn_id: String,
        session_id: String,
        message: String,
    },
    TokenUsage {
        turn_id: String,
        session_id: String,
        input_tokens: u64,
        output_tokens: u64,
        total_tokens: u64,
    },
}

impl AgentRunner {
    /// Create a new agent runner.
    pub fn new<F: Fn(AgentEvent) + Send + Sync + 'static>(
        codex_config: crate::config::CodexConfig,
        event_callback: F,
    ) -> Self {
        Self {
            codex_config,
            event_callback: Arc::new(event_callback),
        }
    }

    /// Run an agent attempt for an issue.
    pub fn run_attempt(
        &self,
        issue: &Issue,
        workspace_path: &Path,
        prompt: String,
        attempt: Option<u32>,
    ) -> Result<LiveSession> {
        info!(
            "Starting agent attempt for issue {} (attempt: {:?})",
            issue.identifier, attempt
        );

        // Launch the agent process
        let (child_pid, tx_rx) = self.launch_agent_process(workspace_path, &prompt)?;

        // Process output from the agent
        let mut session = LiveSession {
            session_id: String::new(),
            thread_id: String::new(),
            turn_id: String::new(),
            codex_app_server_pid: Some(child_pid),
            last_codex_event: None,
            last_codex_timestamp: None,
            last_codex_message: None,
            codex_input_tokens: 0,
            codex_output_tokens: 0,
            codex_total_tokens: 0,
            last_reported_input_tokens: 0,
            last_reported_output_tokens: 0,
            last_reported_total_tokens: 0,
            turn_count: 0,
        };

        // Process events from the agent
        let result = self.process_agent_events(tx_rx, &mut session, attempt)?;

        // Update session with final state
        session.turn_count = result.turn_count;
        session.last_codex_event = result.last_event;
        session.last_codex_timestamp = result.last_timestamp;

        Ok(session)
    }

    /// Launch the agent process and set up communication channels.
    fn launch_agent_process(
        &self,
        workspace_path: &Path,
        initial_prompt: &str,
    ) -> Result<(u32, mpsc::Receiver<AgentEvent>)> {
        // Verify we're in the correct workspace
        if std::env::current_dir().map_err(|e| AgentLaunchError {
            source: Box::new(e),
        })? != workspace_path
        {
            return Err(AgentLaunchError {
                source: Box::new(io::Error::other(format!(
                    "Current directory ({:?}) does not match workspace path ({:?})",
                    std::env::current_dir().map_err(|e| AgentLaunchError {
                        source: Box::new(e)
                    })?,
                    workspace_path
                ))),
            });
        }

        // Set up pipes for communication
        let (tx, rx) = mpsc::channel();

        // Clone the callback for the thread
        let callback = self.event_callback.clone();

        // Spawn the agent process
        let mut child = Command::new("sh")
            .arg("-lc")
            .arg(&self.codex_config.command)
            .current_dir(workspace_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| AgentLaunchError {
                source: Box::new(e),
            })?;

        // Take stdin before moving child into the thread
        let stdin = child.stdin.take();
        let child_pid = child.id();

        // Thread to handle process output
        let tx_clone = tx.clone();
        let callback_clone = callback.clone();
        thread::spawn(move || {
            Self::handle_process_output(child, tx_clone, callback_clone);
        });

        // Send initial prompt to the agent
        if let Some(stdin) = stdin {
            let prompt = initial_prompt.to_string();
            thread::spawn(move || {
                let mut stdin = stdin;
                if let Err(e) = writeln!(stdin, "{}", prompt) {
                    error!("Failed to send initial prompt to agent: {}", e);
                    let _ = tx.send(AgentEvent::StartupFailed {
                        error: format!("Failed to send initial prompt: {}", e),
                    });
                }
            });
        }

        Ok((child_pid, rx))
    }

    /// Handle output from the agent process.
    fn handle_process_output(
        mut child: std::process::Child,
        tx: mpsc::Sender<AgentEvent>,
        callback: Arc<dyn Fn(AgentEvent) + Send + Sync>,
    ) {
        // In a real implementation, we would parse the actual Codex app-server protocol
        // For now, we'll simulate some basic events

        // Simulate session started
        let session_id = format!("thread-{}", std::process::id());
        let turn_id = format!("turn-{}", std::process::id());

        tx.send(AgentEvent::SessionStarted {
            session_id: session_id.clone(),
            pid: child.id(),
        })
        .ok();

        callback(AgentEvent::SessionStarted {
            session_id: session_id.clone(),
            pid: child.id(),
        });

        // Simulate some token usage
        tx.send(AgentEvent::TokenUsage {
            turn_id: turn_id.clone(),
            session_id: session_id.clone(),
            input_tokens: 100,
            output_tokens: 50,
            total_tokens: 150,
        })
        .ok();

        callback(AgentEvent::TokenUsage {
            turn_id: turn_id.clone(),
            session_id: session_id.clone(),
            input_tokens: 100,
            output_tokens: 50,
            total_tokens: 150,
        });

        // Simulate turn completed
        tx.send(AgentEvent::TurnCompleted {
            turn_id: turn_id.clone(),
            session_id: session_id.clone(),
        })
        .ok();

        callback(AgentEvent::TurnCompleted {
            turn_id: turn_id.clone(),
            session_id: session_id.clone(),
        });

        // Wait for process to exit
        let _ = child.wait();
    }

    /// Process events from the agent and update session state.
    fn process_agent_events(
        &self,
        rx: mpsc::Receiver<AgentEvent>,
        session: &mut LiveSession,
        attempt: Option<u32>,
    ) -> Result<AgentProcessingResult> {
        let mut turn_count = 0;
        let mut last_event = None;
        let mut last_timestamp = None;

        // Set a timeout for the entire agent process
        let start = std::time::Instant::now();
        let timeout = Duration::from_millis(self.codex_config.turn_timeout_ms);

        loop {
            // Check for timeout
            if start.elapsed() > timeout {
                return Err(AgentTimeout);
            }

            // Try to receive an event (with timeout to check for process exit)
            let event = rx.recv_timeout(Duration::from_millis(100));

            match event {
                Ok(AgentEvent::SessionStarted { session_id, pid }) => {
                    session.session_id = session_id;
                    // thread_id and turn_id would be parsed from actual events
                    session.thread_id = "thread-1".to_string(); // placeholder
                    session.turn_id = "turn-1".to_string(); // placeholder
                    session.codex_app_server_pid = Some(pid);
                    last_event = Some("session_started".to_string());
                    last_timestamp = Some(std::time::SystemTime::now());
                }
                Ok(AgentEvent::TokenUsage {
                    turn_id: _,
                    session_id: _,
                    input_tokens,
                    output_tokens,
                    total_tokens,
                }) => {
                    // Update session token counts
                    session.codex_input_tokens = input_tokens;
                    session.codex_output_tokens = output_tokens;
                    session.codex_total_tokens = total_tokens;
                    last_event = Some("token_usage".to_string());
                    last_timestamp = Some(std::time::SystemTime::now());
                }
                Ok(AgentEvent::TurnCompleted {
                    turn_id,
                    session_id,
                }) => {
                    session.turn_id = turn_id;
                    session.session_id = session_id;
                    turn_count += 1;
                    last_event = Some("turn_completed".to_string());
                    last_timestamp = Some(std::time::SystemTime::now());

                    // Check if we should continue based on max_turns
                    if attempt.is_some() && turn_count >= self.codex_config.max_turns {
                        // We've reached max turns, exit normally
                        break;
                    }
                }
                Ok(AgentEvent::TurnFailed {
                    turn_id: _,
                    session_id: _,
                    error,
                }) => {
                    return Err(AgentCommunicationError {
                        source: Box::new(io::Error::other(error)),
                    });
                }
                Ok(AgentEvent::TurnCancelled {
                    turn_id: _,
                    session_id: _,
                }) => {
                    return Err(AgentCommunicationError {
                        source: Box::new(io::Error::other("Turn cancelled")),
                    });
                }
                Ok(AgentEvent::StartupFailed { error }) => {
                    return Err(AgentCommunicationError {
                        source: Box::new(io::Error::other(error)),
                    });
                }
                // Handle other events as needed...
                Ok(_) => {
                    // Other events we don't specifically handle but still update timestamp
                    last_timestamp = Some(std::time::SystemTime::now());
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    // Process may have exited; continue looping
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    // Channel disconnected, process likely died
                    break;
                }
            }
        }

        Ok(AgentProcessingResult {
            turn_count,
            last_event,
            last_timestamp,
        })
    }
}

struct AgentProcessingResult {
    turn_count: u32,
    last_event: Option<String>,
    last_timestamp: Option<std::time::SystemTime>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn test_agent_runner_creation() {
        let codex_config = crate::config::CodexConfig {
            command: "echo 'test'".to_string(),
            approval_policy: "test".to_string(),
            thread_sandbox: "test".to_string(),
            turn_sandbox_policy: "test".to_string(),
            turn_timeout_ms: 1000,
            read_timeout_ms: 1000,
            stall_timeout_ms: 1000,
            max_turns: 10,
        };

        let events = Mutex::new(Vec::new());
        let callback = move |event: AgentEvent| {
            events.lock().unwrap().push(event);
        };

        let runner = AgentRunner::new(codex_config, callback);
        assert_eq!(runner.codex_config.command, "echo 'test'");
    }
}
