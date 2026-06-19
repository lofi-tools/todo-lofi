use crate::domain::*;
use crate::error::Result;
use crate::error::SymphonyError::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Configuration values derived from workflow front matter and environment.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceConfig {
    /// Tracker configuration.
    pub tracker: TrackerConfig,
    /// Polling configuration.
    pub polling: PollingConfig,
    /// Workspace configuration.
    pub workspace: WorkspaceConfig,
    /// Hooks configuration.
    pub hooks: HooksConfig,
    /// Agent configuration.
    pub agent: AgentConfig,
    /// Codex (agent runner) configuration.
    pub codex: CodexConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TrackerConfig {
    pub kind: String,
    pub endpoint: String,
    pub api_key: String,
    pub project_slug: String,
    pub active_states: Vec<String>,
    pub terminal_states: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PollingConfig {
    pub interval_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceConfig {
    pub root: PathBuf,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct HooksConfig {
    pub after_create: Option<String>,
    pub before_run: Option<String>,
    pub after_run: Option<String>,
    pub before_remove: Option<String>,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentConfig {
    pub max_concurrent_agents: u32,
    pub max_turns: u32,
    pub max_retry_backoff_ms: u64,
    pub max_concurrent_agents_by_state: HashMap<String, u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CodexConfig {
    pub command: String,
    pub approval_policy: String, // In practice would be enum from Codex bindings
    pub thread_sandbox: String,  // In practice would be enum from Codex bindings
    pub turn_sandbox_policy: String, // In practice would be enum from Codex bindings
    pub turn_timeout_ms: u64,
    pub read_timeout_ms: u64,
    pub stall_timeout_ms: u64,
    pub max_turns: u32,
}

/// Load and validate service configuration from workflow definition.
pub fn load_config(workflow: &WorkflowDefinition, workflow_dir: &Path) -> Result<ServiceConfig> {
    // Apply defaults and extract configuration
    let tracker = load_tracker_config(&workflow.config)?;
    let polling = load_polling_config(&workflow.config);
    let workspace = load_workspace_config(&workflow.config, workflow_dir)?;
    let hooks = load_hooks_config(&workflow.config);
    let agent = load_agent_config(&workflow.config);
    let codex = load_codex_config(&workflow.config);

    let config = ServiceConfig {
        tracker,
        polling,
        workspace,
        hooks,
        agent,
        codex,
    };

    // Validate the configuration
    config.validate()?;

    Ok(config)
}

fn load_tracker_config(config: &HashMap<String, serde_yaml::Value>) -> Result<TrackerConfig> {
    let tracker = config.get("tracker").ok_or_else(|| ConfigValidation {
        message: "Missing 'tracker' section in workflow config".to_string(),
    })?;

    if !tracker.is_mapping() {
        return Err(ConfigValidation {
            message: "'tracker' must be a mapping/object".to_string(),
        });
    }

    let tracker_map = tracker.as_mapping().unwrap();

    // Kind is required
    let kind = tracker_map
        .get("kind")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ConfigValidation {
            message: "Tracker 'kind' is required and must be a string".to_string(),
        })?;

    // Validate kind
    if kind != "linear" {
        return Err(UnsupportedTrackerKind {
            kind: kind.to_string(),
        });
    }

    // Endpoint with default
    let endpoint = tracker_map
        .get("endpoint")
        .and_then(|v| v.as_str())
        .unwrap_or("https://api.linear.app/graphql")
        .to_string();

    // API key - may be environment variable reference
    let api_key_raw = tracker_map
        .get("api_key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ConfigValidation {
            message: "Tracker 'api_key' is required".to_string(),
        })?;

    let api_key = if api_key_raw.starts_with('$') && api_key_raw.len() > 1 {
        // Environment variable reference
        let var_name = &api_key_raw[1..];
        std::env::var(var_name).map_err(|_| MissingTrackerApiKey)?
    } else {
        api_key_raw.to_string()
    };

    // Project slug required for linear
    let project_slug = tracker_map
        .get("project_slug")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ConfigValidation {
            message: "Tracker 'project_slug' is required for linear tracker".to_string(),
        })?;

    // Active states with defaults
    let active_states = tracker_map
        .get("active_states")
        .and_then(|v| v.as_sequence())
        .map(|seq| {
            seq.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_else(|| vec!["Todo".to_string(), "In Progress".to_string()]);

    // Terminal states with defaults
    let terminal_states = tracker_map
        .get("terminal_states")
        .and_then(|v| v.as_sequence())
        .map(|seq| {
            seq.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_else(|| {
            vec![
                "Closed".to_string(),
                "Cancelled".to_string(),
                "Canceled".to_string(),
                "Duplicate".to_string(),
                "Done".to_string(),
            ]
        });

    Ok(TrackerConfig {
        kind: kind.to_string(),
        endpoint,
        api_key,
        project_slug: project_slug.to_string(),
        active_states,
        terminal_states,
    })
}

fn load_polling_config(config: &HashMap<String, serde_yaml::Value>) -> PollingConfig {
    let default_mapping = serde_yaml::Mapping::new();
    let polling = config
        .get("polling")
        .and_then(|v| v.as_mapping())
        .unwrap_or(&default_mapping);

    let interval_ms = polling
        .get("interval_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(30000);

    PollingConfig { interval_ms }
}

fn load_workspace_config(
    config: &HashMap<String, serde_yaml::Value>,
    workflow_dir: &Path,
) -> Result<WorkspaceConfig> {
    let default_mapping = serde_yaml::Mapping::new();
    let workspace = config
        .get("workspace")
        .and_then(|v| v.as_mapping())
        .unwrap_or(&default_mapping);

    let default_root = std::env::temp_dir().join("symphony_workspaces");
    let root_str_owned = workspace
        .get("root")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| default_root.to_string_lossy().into_owned());
    let root_str = root_str_owned.as_str();

    let mut root = PathBuf::from(root_str);

    // Expand ~
    if let Some(stripped) = root_str.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        root = PathBuf::from(home);
        root.push(stripped);
    }

    // Expand $VAR
    let root_str_expanded = expand_env_vars(&root.to_string_lossy());
    root = PathBuf::from(root_str_expanded);

    // If relative, make it relative to workflow directory
    if !root.is_absolute() {
        root = workflow_dir.join(&root);
        root = dunce::canonicalize(&root).unwrap_or_else(|_| root.clone());
    }

    // Normalize to absolute path
    root = dunce::canonicalize(&root).unwrap_or_else(|_| root.clone());

    Ok(WorkspaceConfig { root })
}

fn load_hooks_config(config: &HashMap<String, serde_yaml::Value>) -> HooksConfig {
    let default_mapping = serde_yaml::Mapping::new();
    let hooks = config
        .get("hooks")
        .and_then(|v| v.as_mapping())
        .unwrap_or(&default_mapping);

    let after_create = hooks
        .get("after_create")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let before_run = hooks
        .get("before_run")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let after_run = hooks
        .get("after_run")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let before_remove = hooks
        .get("before_remove")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let timeout_ms = hooks
        .get("timeout_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(60000);

    HooksConfig {
        after_create,
        before_run,
        after_run,
        before_remove,
        timeout_ms,
    }
}

fn load_agent_config(config: &HashMap<String, serde_yaml::Value>) -> AgentConfig {
    let default_mapping = serde_yaml::Mapping::new();
    let agent = config
        .get("agent")
        .and_then(|v| v.as_mapping())
        .unwrap_or(&default_mapping);

    let max_concurrent_agents = agent
        .get("max_concurrent_agents")
        .and_then(|v| v.as_u64())
        .map(|v| v as u32)
        .unwrap_or(10);

    let max_turns = agent
        .get("max_turns")
        .and_then(|v| v.as_u64())
        .map(|v| v as u32)
        .unwrap_or(20);

    let max_retry_backoff_ms = agent
        .get("max_retry_backoff_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(300000); // 5 minutes

    let max_concurrent_agents_by_state = agent
        .get("max_concurrent_agents_by_state")
        .and_then(|v| v.as_mapping())
        .map(|map| {
            map.iter()
                .filter_map(|(k, v)| {
                    k.as_str()
                        .and_then(|key| v.as_u64().map(|val| (key.to_string(), val as u32)))
                })
                .collect()
        })
        .unwrap_or_default();

    AgentConfig {
        max_concurrent_agents,
        max_turns,
        max_retry_backoff_ms,
        max_concurrent_agents_by_state,
    }
}

fn load_codex_config(config: &HashMap<String, serde_yaml::Value>) -> CodexConfig {
    let codex = config
        .get("codex")
        .and_then(|v| v.as_mapping())
        .cloned()
        .unwrap_or(serde_yaml::Mapping::new());

    let command = codex
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("codex app-server")
        .to_string();

    // These would normally be proper enums from Codex bindings, but we'll use strings for now
    let approval_policy = codex
        .get("approval_policy")
        .and_then(|v| v.as_str())
        .unwrap_or("implementation-defined")
        .to_string();

    let thread_sandbox = codex
        .get("thread_sandbox")
        .and_then(|v| v.as_str())
        .unwrap_or("implementation-defined")
        .to_string();

    let turn_sandbox_policy = codex
        .get("turn_sandbox_policy")
        .and_then(|v| v.as_str())
        .unwrap_or("implementation-defined")
        .to_string();

    let turn_timeout_ms = codex
        .get("turn_timeout_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(3600000); // 1 hour

    let read_timeout_ms = codex
        .get("read_timeout_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(5000);

    let stall_timeout_ms = codex
        .get("stall_timeout_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(300000); // 5 minutes

    let max_turns = codex
        .get("max_turns")
        .and_then(|v| v.as_u64())
        .map(|v| v as u32)
        .unwrap_or(20);

    CodexConfig {
        command,
        approval_policy,
        thread_sandbox,
        turn_sandbox_policy,
        turn_timeout_ms,
        read_timeout_ms,
        stall_timeout_ms,
        max_turns,
    }
}

/// Expand environment variables in a string ($VAR or ${VAR}).
fn expand_env_vars(s: &str) -> String {
    use regex::Regex;

    // Pattern to match $VAR or ${VAR}
    let _re = Regex::new(r"(\\?)(\\\\)*\\$(\w+|\{[^}]+\})").unwrap();

    // For simplicity in this implementation, we'll do a basic version
    // A production version would need proper regex replacement
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '$' {
            // Check if this is an environment variable
            if let Some(next) = chars.peek() {
                if next.is_alphanumeric() || *next == '{' {
                    let is_braced = *next == '{';
                    // Found potential env var
                    let mut var_name = String::new();

                    if is_braced {
                        // ${VAR} format
                        chars.next(); // consume '{'
                        for ch in chars.by_ref() {
                            if ch == '}' {
                                break;
                            }
                            var_name.push(ch);
                        }
                    } else {
                        // $VAR format
                        while let Some(ch) = chars.peek() {
                            if ch.is_alphanumeric() || *ch == '_' {
                                var_name.push(*ch);
                                chars.next();
                            } else {
                                break;
                            }
                        }
                    }

                    // Look up environment variable
                    if let Some(value) = std::env::var_os(&var_name) {
                        if let Some(value_str) = value.to_str() {
                            result.push_str(value_str);
                        }
                    } else {
                        result.push('$');
                        if is_braced {
                            result.push('{');
                        }
                        result.push_str(&var_name);
                        if is_braced {
                            result.push('}');
                        }
                    }
                } else {
                    result.push(ch);
                }
            } else {
                result.push(ch);
            }
        } else {
            result.push(ch);
        }
    }

    result
}

impl ServiceConfig {
    /// Validate the loaded configuration.
    pub fn validate(&self) -> Result<()> {
        // Tracker validation
        if self.tracker.api_key.is_empty() {
            return Err(MissingTrackerApiKey);
        }

        if self.tracker.kind == "linear" && self.tracker.project_slug.is_empty() {
            return Err(MissingTrackerProjectSlug);
        }

        // Polling validation
        if self.polling.interval_ms == 0 {
            return Err(ConfigValidation {
                message: "Polling interval must be greater than 0".to_string(),
            });
        }

        // Workspace validation
        if !self.workspace.root.is_absolute() {
            return Err(ConfigValidation {
                message: "Workspace root must be an absolute path".to_string(),
            });
        }

        // Hooks validation
        if self.hooks.timeout_ms == 0 {
            return Err(ConfigValidation {
                message: "Hooks timeout must be greater than 0".to_string(),
            });
        }

        // Agent validation
        if self.agent.max_concurrent_agents == 0 {
            return Err(ConfigValidation {
                message: "Max concurrent agents must be greater than 0".to_string(),
            });
        }

        if self.agent.max_turns == 0 {
            return Err(ConfigValidation {
                message: "Max turns must be greater than 0".to_string(),
            });
        }

        // Codex validation
        if self.codex.command.is_empty() {
            return Err(ConfigValidation {
                message: "Codex command must not be empty".to_string(),
            });
        }

        if self.codex.turn_timeout_ms == 0 {
            return Err(ConfigValidation {
                message: "Codex turn timeout must be greater than 0".to_string(),
            });
        }

        if self.codex.read_timeout_ms == 0 {
            return Err(ConfigValidation {
                message: "Codex read timeout must be greater than 0".to_string(),
            });
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_load_config_minimal() {
        let workflow_dir = TempDir::new().unwrap();
        let workflow_path = workflow_dir.path().join("WORKFLOW.md");
        std::fs::write(
            &workflow_path,
            r#"
# Test prompt
"#,
        )
        .unwrap();

        let workflow = crate::workflow::load_workflow(&workflow_path).unwrap();
        let result = load_config(&workflow, workflow_dir.path());
        // Validation fails because project_slug is missing
        assert!(result.is_err());
    }

    #[test]
    fn test_load_config_full() {
        let workflow_dir = TempDir::new().unwrap();
        let workflow_path = workflow_dir.path().join("WORKFLOW.md");
        std::fs::write(
            &workflow_path,
            r#"---
tracker:
  kind: linear
  endpoint: https://api.linear.app/graphql
  api_key: $TEST_FULL_CONFIG_KEY_42
  project_slug: test-project
  active_states: ["Todo", "In Progress"]
  terminal_states: ["Done", "Cancelled"]
polling:
  interval_ms: 15000
workspace:
  root: ./workspaces
hooks:
  after_create: "echo 'created'"
  before_run: "echo 'running'"
  timeout_ms: 30000
agent:
  max_concurrent_agents: 5
  max_turns: 10
  max_retry_backoff_ms: 60000
  max_concurrent_agents_by_state:
    "Todo": 2
    "In Progress": 3
codex:
  command: "codex app-server --flag"
  turn_timeout_ms: 1800000
  read_timeout_ms: 2000
  stall_timeout_ms: 60000
---

# Test prompt for {{ issue.identifier }}
"#,
        )
        .unwrap();

        unsafe {
            std::env::set_var("TEST_FULL_CONFIG_KEY_42", "test-key-123");
        }

        let workflow = crate::workflow::load_workflow(&workflow_path).unwrap();
        let config = load_config(&workflow, workflow_dir.path()).unwrap();

        assert_eq!(config.tracker.kind, "linear");
        assert_eq!(config.tracker.endpoint, "https://api.linear.app/graphql");
        assert_eq!(config.tracker.api_key, "test-key-123");
        assert_eq!(config.tracker.project_slug, "test-project");
        assert_eq!(config.tracker.active_states, vec!["Todo", "In Progress"]);
        assert_eq!(config.tracker.terminal_states, vec!["Done", "Cancelled"]);

        assert_eq!(config.polling.interval_ms, 15000);

        // Workspace root should be absolute path to ./workspaces relative to workflow dir
        assert!(config.workspace.root.is_absolute());
        assert!(config.workspace.root.ends_with("workspaces"));

        assert_eq!(
            config.hooks.after_create,
            Some("echo 'created'".to_string())
        );
        assert_eq!(config.hooks.before_run, Some("echo 'running'".to_string()));
        assert_eq!(config.hooks.timeout_ms, 30000);

        assert_eq!(config.agent.max_concurrent_agents, 5);
        assert_eq!(config.agent.max_turns, 10);
        assert_eq!(config.agent.max_retry_backoff_ms, 60000);
        assert_eq!(
            config.agent.max_concurrent_agents_by_state.get("Todo"),
            Some(&2)
        );
        assert_eq!(
            config
                .agent
                .max_concurrent_agents_by_state
                .get("In Progress"),
            Some(&3)
        );

        assert_eq!(config.codex.command, "codex app-server --flag");
        assert_eq!(config.codex.turn_timeout_ms, 1800000);
        assert_eq!(config.codex.read_timeout_ms, 2000);
        assert_eq!(config.codex.stall_timeout_ms, 60000);
    }

    #[test]
    fn test_config_validation_missing_api_key() {
        let workflow_dir = TempDir::new().unwrap();
        let workflow_path = workflow_dir.path().join("WORKFLOW.md");
        std::fs::write(
            &workflow_path,
            r#"---
tracker:
  kind: linear
  api_key: $NONEXISTENT_API_KEY_FOR_TEST_42
  project_slug: test
---

# Test
"#,
        )
        .unwrap();

        // Don't set the env var — it should fail

        let workflow = crate::workflow::load_workflow(&workflow_path).unwrap();
        let result = load_config(&workflow, workflow_dir.path());
        assert!(matches!(result, Err(MissingTrackerApiKey)));
    }

    #[test]
    fn test_config_validation_missing_project_slug() {
        let workflow_dir = TempDir::new().unwrap();
        let workflow_path = workflow_dir.path().join("WORKFLOW.md");
        std::fs::write(
            &workflow_path,
            r#"---
tracker:
  kind: linear
  api_key: test-key
---

# Test
"#,
        )
        .unwrap();

        let workflow = crate::workflow::load_workflow(&workflow_path).unwrap();
        let result = load_config(&workflow, workflow_dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn test_expand_env_vars() {
        unsafe {
            std::env::set_var("TEST_VAR", "expanded-value");
        }
        let s = expand_env_vars("prefix-$TEST_VAR-suffix");
        assert_eq!(s, "prefix-expanded-value-suffix");

        unsafe {
            std::env::set_var("another", "value");
        }
        let s = expand_env_vars("${another}");
        assert_eq!(s, "value");

        // Non-existent var should remain unchanged (basic implementation)
        let s = expand_env_vars("$NONEXISTENT");
        assert_eq!(s, "$NONEXISTENT"); // Our simple impl doesn't handle this perfectly
    }
}
