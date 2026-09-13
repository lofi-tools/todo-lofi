use crate::domain::*;
use crate::error::Result;
use crate::error::SymphonyError::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Default poll interval in milliseconds (Section 5.3.2).
pub const DEFAULT_POLL_INTERVAL_MS: u64 = 30_000;
/// Default hook timeout in milliseconds (Section 5.3.4).
pub const DEFAULT_HOOK_TIMEOUT_MS: u64 = 60_000;
/// Default global concurrency limit (Section 5.3.5).
pub const DEFAULT_MAX_CONCURRENT_AGENTS: u32 = 10;
/// Default turn limit per worker session (Section 5.3.5).
pub const DEFAULT_MAX_TURNS: u32 = 20;
/// Default retry backoff cap (Section 5.3.5).
pub const DEFAULT_MAX_RETRY_BACKOFF_MS: u64 = 300_000;
/// Default `codex.command` (Section 5.3.6).
pub const DEFAULT_CODEX_COMMAND: &str = "codex app-server";
/// Default per-turn stream timeout (Section 5.3.6).
pub const DEFAULT_TURN_TIMEOUT_MS: u64 = 3_600_000;
/// Default request/response timeout (Section 5.3.6).
pub const DEFAULT_READ_TIMEOUT_MS: u64 = 5_000;
/// Default stall timeout (Section 5.3.6).
pub const DEFAULT_STALL_TIMEOUT_MS: u64 = 300_000;
/// Default Linear GraphQL endpoint (Section 5.3.1).
pub const DEFAULT_LINEAR_ENDPOINT: &str = "https://api.linear.app/graphql";
/// Canonical Linear API-key environment variable (Section 5.3.1).
pub const LINEAR_API_KEY_ENV: &str = "LINEAR_API_KEY";

/// Implementation-defined default approval policy for the coding agent.
///
/// This service targets trusted environments and auto-approves agent approvals
/// (see `demos/symphony/README.md` for the documented trust posture).
pub const DEFAULT_APPROVAL_POLICY: &str = "never";
/// Implementation-defined default thread sandbox mode.
pub const DEFAULT_THREAD_SANDBOX: &str = "workspace-write";
/// Implementation-defined default turn sandbox policy.
pub const DEFAULT_TURN_SANDBOX_POLICY: &str = "workspace-write";

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
    /// OPTIONAL HTTP observability server extension.
    pub server: ServerConfig,
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

impl TrackerConfig {
    pub fn is_active_state(&self, state: &str) -> bool {
        let state = normalize_state(state);
        self.active_states
            .iter()
            .any(|candidate| normalize_state(candidate) == state)
    }

    pub fn is_terminal_state(&self, state: &str) -> bool {
        let state = normalize_state(state);
        self.terminal_states
            .iter()
            .any(|candidate| normalize_state(candidate) == state)
    }
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

impl AgentConfig {
    /// Per-state concurrency limit for a tracker state, when one is configured.
    pub fn limit_for_state(&self, state: &str) -> Option<u32> {
        self.max_concurrent_agents_by_state
            .get(&normalize_state(state))
            .copied()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CodexConfig {
    /// Shell command launched via `bash -lc` in the per-issue workspace.
    pub command: String,
    /// Pass-through Codex `AskForApproval` value.
    pub approval_policy: String,
    /// Pass-through Codex thread `SandboxMode` value.
    pub thread_sandbox: String,
    /// Pass-through Codex turn `SandboxPolicy` value.
    pub turn_sandbox_policy: String,
    pub turn_timeout_ms: u64,
    pub read_timeout_ms: u64,
    /// `0` disables stall detection.
    pub stall_timeout_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerConfig {
    /// OPTIONAL HTTP server port. `Some(0)` requests an ephemeral port.
    pub port: Option<u16>,
}

/// Load and validate service configuration from a workflow definition.
pub fn load_config(workflow: &WorkflowDefinition, workflow_dir: &Path) -> Result<ServiceConfig> {
    let config = ServiceConfig {
        tracker: load_tracker_config(&workflow.config)?,
        polling: load_polling_config(&workflow.config)?,
        workspace: load_workspace_config(&workflow.config, workflow_dir)?,
        hooks: load_hooks_config(&workflow.config)?,
        agent: load_agent_config(&workflow.config)?,
        codex: load_codex_config(&workflow.config),
        server: load_server_config(&workflow.config)?,
    };

    config.validate()?;
    Ok(config)
}

/// The effective, reloadable view of a workflow: config plus prompt template.
#[derive(Debug, Clone)]
pub struct EffectiveWorkflow {
    /// Path of the workflow file this configuration was loaded from.
    pub path: PathBuf,
    /// Typed runtime configuration.
    pub config: ServiceConfig,
    /// Parsed prompt template from the workflow markdown body.
    pub prompt_template: crate::prompt::Template,
}

impl EffectiveWorkflow {
    /// Load and validate a workflow file, returning the effective view.
    pub fn load(path: &Path) -> Result<Self> {
        let definition = crate::workflow::load_workflow(path)?;
        Self::from_definition(path, &definition)
    }

    /// Build the effective view from an already parsed workflow definition.
    pub fn from_definition(path: &Path, definition: &WorkflowDefinition) -> Result<Self> {
        let workflow_dir = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let config = load_config(definition, &workflow_dir)?;
        let prompt_template = crate::workflow::parse_prompt_template(&definition.prompt_template)?;
        Ok(Self {
            path: path.to_path_buf(),
            config,
            prompt_template,
        })
    }
}

fn load_tracker_config(config: &HashMap<String, serde_yaml::Value>) -> Result<TrackerConfig> {
    let Some(tracker) = config.get("tracker") else {
        return Err(ConfigValidation {
            message: "Missing 'tracker' section in workflow config".to_string(),
        });
    };
    let Some(tracker_map) = tracker.as_mapping() else {
        return Err(ConfigValidation {
            message: "'tracker' must be a mapping/object".to_string(),
        });
    };

    let kind = tracker_map
        .get("kind")
        .and_then(|value| value.as_str())
        .ok_or_else(|| ConfigValidation {
            message: "Tracker 'kind' is required and must be a string".to_string(),
        })?;

    if kind != "linear" {
        return Err(UnsupportedTrackerKind {
            kind: kind.to_string(),
        });
    }

    let endpoint = tracker_map
        .get("endpoint")
        .and_then(|value| value.as_str())
        .unwrap_or(DEFAULT_LINEAR_ENDPOINT)
        .to_string();

    let raw_api_key = tracker_map
        .get("api_key")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let api_key = resolve_api_key(raw_api_key)?;

    let project_slug = tracker_map
        .get("project_slug")
        .and_then(|value| value.as_str())
        .ok_or_else(|| ConfigValidation {
            message: "Tracker 'project_slug' is required for linear tracker".to_string(),
        })?
        .to_string();

    let active_states = string_list(tracker_map.get("active_states")).unwrap_or_else(|| {
        vec!["Todo".to_string(), "In Progress".to_string()]
    });

    let terminal_states = string_list(tracker_map.get("terminal_states")).unwrap_or_else(|| {
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
        project_slug,
        active_states,
        terminal_states,
    })
}

/// Resolve `tracker.api_key`, which MAY be a literal token or `$VAR_NAME`.
fn resolve_api_key(raw: &str) -> Result<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(MissingTrackerApiKey);
    }

    let resolved = if let Some(variable) = environment_variable_name(raw) {
        std::env::var(variable).unwrap_or_default()
    } else {
        raw.to_string()
    };

    if resolved.trim().is_empty() {
        return Err(MissingTrackerApiKey);
    }
    Ok(resolved)
}

/// Extract the variable name when the whole value is `$VAR` or `${VAR}`.
fn environment_variable_name(raw: &str) -> Option<&str> {
    let trimmed = raw.trim();
    let remainder = trimmed.strip_prefix('$')?;
    if remainder.is_empty() {
        return None;
    }
    let name = remainder
        .strip_prefix('{')
        .and_then(|inner| inner.strip_suffix('}'))
        .unwrap_or(remainder);
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

fn load_polling_config(config: &HashMap<String, serde_yaml::Value>) -> Result<PollingConfig> {
    let polling = config.get("polling").and_then(|value| value.as_mapping());
    let interval_ms = match polling.and_then(|mapping| mapping.get("interval_ms")) {
        Some(value) => value.as_u64().ok_or_else(|| ConfigValidation {
            message: "'polling.interval_ms' must be a positive integer".to_string(),
        })?,
        None => DEFAULT_POLL_INTERVAL_MS,
    };
    Ok(PollingConfig { interval_ms })
}

fn load_workspace_config(
    config: &HashMap<String, serde_yaml::Value>,
    workflow_dir: &Path,
) -> Result<WorkspaceConfig> {
    let workspace = config.get("workspace").and_then(|value| value.as_mapping());
    let configured = match workspace.and_then(|mapping| mapping.get("root")) {
        Some(value) => Some(value.as_str().ok_or_else(|| ConfigValidation {
            message: "'workspace.root' must be a string path".to_string(),
        })?),
        None => None,
    };

    let raw = match configured {
        Some(raw) => expand_environment(raw)?,
        None => std::env::temp_dir()
            .join("symphony_workspaces")
            .to_string_lossy()
            .into_owned(),
    };

    let expanded = expand_home(&raw);
    let mut root = PathBuf::from(&expanded);
    if !root.is_absolute() {
        root = workflow_dir.join(&root);
    }
    root = normalize_path(&root);

    Ok(WorkspaceConfig { root })
}

fn load_hooks_config(config: &HashMap<String, serde_yaml::Value>) -> Result<HooksConfig> {
    let hooks = config.get("hooks").and_then(|value| value.as_mapping());

    let script = |name: &str| -> Option<String> {
        hooks
            .and_then(|mapping| mapping.get(name))
            .and_then(|value| value.as_str())
            .map(|value| value.to_string())
    };

    let timeout_ms = match hooks.and_then(|mapping| mapping.get("timeout_ms")) {
        Some(value) => {
            let timeout_ms = value.as_u64().ok_or_else(|| ConfigValidation {
                message: "'hooks.timeout_ms' must be a positive integer".to_string(),
            })?;
            if timeout_ms == 0 {
                return Err(ConfigValidation {
                    message: "'hooks.timeout_ms' must be greater than 0".to_string(),
                });
            }
            timeout_ms
        }
        None => DEFAULT_HOOK_TIMEOUT_MS,
    };

    Ok(HooksConfig {
        after_create: script("after_create"),
        before_run: script("before_run"),
        after_run: script("after_run"),
        before_remove: script("before_remove"),
        timeout_ms,
    })
}

fn load_agent_config(config: &HashMap<String, serde_yaml::Value>) -> Result<AgentConfig> {
    let agent = config.get("agent").and_then(|value| value.as_mapping());

    let positive_integer = |name: &str| -> Result<Option<u32>> {
        match agent.and_then(|mapping| mapping.get(name)) {
            Some(value) => {
                let integer = value.as_u64().ok_or_else(|| ConfigValidation {
                    message: format!("'agent.{name}' must be a positive integer"),
                })?;
                if integer == 0 {
                    return Err(ConfigValidation {
                        message: format!("'agent.{name}' must be greater than 0"),
                    });
                }
                Ok(Some(integer as u32))
            }
            None => Ok(None),
        }
    };

    let max_concurrent_agents =
        positive_integer("max_concurrent_agents")?.unwrap_or(DEFAULT_MAX_CONCURRENT_AGENTS);
    let max_turns = positive_integer("max_turns")?.unwrap_or(DEFAULT_MAX_TURNS);

    let max_retry_backoff_ms = match agent.and_then(|mapping| mapping.get("max_retry_backoff_ms")) {
        Some(value) => value.as_u64().ok_or_else(|| ConfigValidation {
            message: "'agent.max_retry_backoff_ms' must be a non-negative integer".to_string(),
        })?,
        None => DEFAULT_MAX_RETRY_BACKOFF_MS,
    };

    // State keys are normalized and invalid (non-positive or non-numeric) entries ignored.
    let max_concurrent_agents_by_state = agent
        .and_then(|mapping| mapping.get("max_concurrent_agents_by_state"))
        .and_then(|value| value.as_mapping())
        .map(|mapping| {
            mapping
                .iter()
                .filter_map(|(key, value)| {
                    let key = key.as_str()?;
                    let limit = value.as_u64()?;
                    if limit == 0 {
                        return None;
                    }
                    Some((normalize_state(key), limit as u32))
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(AgentConfig {
        max_concurrent_agents,
        max_turns,
        max_retry_backoff_ms,
        max_concurrent_agents_by_state,
    })
}

fn load_codex_config(config: &HashMap<String, serde_yaml::Value>) -> CodexConfig {
    let codex = config.get("codex").and_then(|value| value.as_mapping());

    let string_value = |name: &str, default: &str| -> String {
        codex
            .and_then(|mapping| mapping.get(name))
            .and_then(|value| value.as_str())
            .unwrap_or(default)
            .to_string()
    };

    let millis = |name: &str, default: u64| -> u64 {
        match codex.and_then(|mapping| mapping.get(name)) {
            Some(value) => match value.as_i64() {
                Some(value) if value > 0 => value as u64,
                // `stall_timeout_ms <= 0` disables stall detection; other
                // non-positive values fall back to the documented default.
                Some(_) if name == "stall_timeout_ms" => 0,
                _ => default,
            },
            None => default,
        }
    };

    CodexConfig {
        command: {
            let command = string_value("command", DEFAULT_CODEX_COMMAND);
            if command.trim().is_empty() {
                DEFAULT_CODEX_COMMAND.to_string()
            } else {
                command
            }
        },
        approval_policy: string_value("approval_policy", DEFAULT_APPROVAL_POLICY),
        thread_sandbox: string_value("thread_sandbox", DEFAULT_THREAD_SANDBOX),
        turn_sandbox_policy: string_value("turn_sandbox_policy", DEFAULT_TURN_SANDBOX_POLICY),
        turn_timeout_ms: millis("turn_timeout_ms", DEFAULT_TURN_TIMEOUT_MS),
        read_timeout_ms: millis("read_timeout_ms", DEFAULT_READ_TIMEOUT_MS),
        stall_timeout_ms: millis("stall_timeout_ms", DEFAULT_STALL_TIMEOUT_MS),
    }
}

fn load_server_config(config: &HashMap<String, serde_yaml::Value>) -> Result<ServerConfig> {
    let server = config.get("server").and_then(|value| value.as_mapping());
    let port = match server.and_then(|mapping| mapping.get("port")) {
        Some(value) => {
            let port = value.as_u64().ok_or_else(|| ConfigValidation {
                message: "'server.port' must be an integer".to_string(),
            })?;
            let port = u16::try_from(port).map_err(|_| ConfigValidation {
                message: "'server.port' must be between 0 and 65535".to_string(),
            })?;
            Some(port)
        }
        None => None,
    };
    Ok(ServerConfig { port })
}

fn string_list(value: Option<&serde_yaml::Value>) -> Option<Vec<String>> {
    let sequence = value?.as_sequence()?;
    Some(
        sequence
            .iter()
            .filter_map(|item| item.as_str().map(|item| item.to_string()))
            .collect(),
    )
}

/// Expand `$VAR` / `${VAR}` tokens embedded in a path value.
///
/// Expansion is only applied to values that are local filesystem paths.
fn expand_environment(input: &str) -> Result<String> {
    let mut output = String::with_capacity(input.len());
    let characters: Vec<char> = input.chars().collect();
    let mut index = 0;

    while index < characters.len() {
        let character = characters[index];
        if character != '$' {
            output.push(character);
            index += 1;
            continue;
        }

        index += 1;
        let mut name = String::new();
        if characters.get(index) == Some(&'{') {
            index += 1;
            while index < characters.len() && characters[index] != '}' {
                name.push(characters[index]);
                index += 1;
            }
            if characters.get(index) != Some(&'}') {
                return Err(ConfigValidation {
                    message: format!("Unterminated environment token in `{input}`"),
                });
            }
            index += 1;
        } else {
            while index < characters.len()
                && (characters[index].is_alphanumeric() || characters[index] == '_')
            {
                name.push(characters[index]);
                index += 1;
            }
        }

        if name.is_empty() {
            output.push('$');
            continue;
        }

        let value = std::env::var(&name).map_err(|_| ConfigValidation {
            message: format!("Environment variable `{name}` referenced by a path is not set"),
        })?;
        output.push_str(&value);
    }

    Ok(output)
}

/// Expand a leading `~` to the user's home directory.
fn expand_home(input: &str) -> String {
    if input == "~" {
        return home_directory().unwrap_or_else(|| input.to_string());
    }
    if let Some(remainder) = input.strip_prefix("~/") {
        if let Some(home) = home_directory() {
            return format!("{home}/{remainder}");
        }
    }
    input.to_string()
}

fn home_directory() -> Option<String> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| home.to_string_lossy().into_owned())
}

/// Lexically normalize an absolute path: resolve `.` and `..` without touching the filesystem.
pub fn normalize_path(path: &Path) -> PathBuf {
    use std::path::Component;

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    normalized.push("..");
                }
            }
            Component::RootDir | Component::Prefix(_) => normalized.push(component.as_os_str()),
            Component::Normal(part) => normalized.push(part),
        }
    }
    if normalized.as_os_str().is_empty() && path.is_absolute() {
        normalized.push("/");
    }
    normalized
}

impl ServiceConfig {
    /// Validate the loaded configuration (Section 6.3).
    pub fn validate(&self) -> Result<()> {
        if self.tracker.kind != "linear" {
            return Err(UnsupportedTrackerKind {
                kind: self.tracker.kind.clone(),
            });
        }

        if self.tracker.api_key.trim().is_empty() {
            return Err(MissingTrackerApiKey);
        }

        if self.tracker.project_slug.trim().is_empty() {
            return Err(MissingTrackerProjectSlug);
        }

        if !self
            .tracker
            .endpoint
            .starts_with("http://")
            && !self.tracker.endpoint.starts_with("https://")
        {
            return Err(ConfigValidation {
                message: format!("Tracker endpoint must be an HTTP(S) URL: {}", self.tracker.endpoint),
            });
        }

        if self.tracker.active_states.is_empty() {
            return Err(ConfigValidation {
                message: "Tracker 'active_states' must not be empty".to_string(),
            });
        }

        if self.polling.interval_ms == 0 {
            return Err(ConfigValidation {
                message: "Polling interval must be greater than 0".to_string(),
            });
        }

        if !self.workspace.root.is_absolute() {
            return Err(ConfigValidation {
                message: "Workspace root must be an absolute path".to_string(),
            });
        }

        if self.hooks.timeout_ms == 0 {
            return Err(ConfigValidation {
                message: "Hooks timeout must be greater than 0".to_string(),
            });
        }

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

        if self.codex.command.trim().is_empty() {
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

    fn write_workflow(directory: &TempDir, contents: &str) -> PathBuf {
        let path = directory.path().join("WORKFLOW.md");
        std::fs::write(&path, contents).expect("workflow file is writable");
        path
    }

    #[test]
    fn test_load_config_minimal() {
        let workflow_dir = TempDir::new().expect("temp dir");
        let workflow_path = write_workflow(&workflow_dir, "# Test prompt\n");

        let workflow = crate::workflow::load_workflow(&workflow_path).expect("workflow loads");
        let result = load_config(&workflow, workflow_dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn test_load_config_full() {
        let workflow_dir = TempDir::new().expect("temp dir");
        let workflow_path = write_workflow(
            &workflow_dir,
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
    "IN PROGRESS": 3
codex:
  command: "codex app-server --flag"
  turn_timeout_ms: 1800000
  read_timeout_ms: 2000
  stall_timeout_ms: 60000
server:
  port: 0
---

# Test prompt for {{ issue.identifier }}
"#,
        );

        // SAFETY: single-threaded test setup of the process environment.
        unsafe {
            std::env::set_var("TEST_FULL_CONFIG_KEY_42", "test-key-123");
        }

        let workflow = crate::workflow::load_workflow(&workflow_path).expect("workflow loads");
        let config = load_config(&workflow, workflow_dir.path()).expect("config is valid");

        assert_eq!(config.tracker.kind, "linear");
        assert_eq!(config.tracker.endpoint, "https://api.linear.app/graphql");
        assert_eq!(config.tracker.api_key, "test-key-123");
        assert_eq!(config.tracker.project_slug, "test-project");
        assert_eq!(config.tracker.active_states, vec!["Todo", "In Progress"]);
        assert_eq!(config.tracker.terminal_states, vec!["Done", "Cancelled"]);

        assert_eq!(config.polling.interval_ms, 15000);

        assert!(config.workspace.root.is_absolute());
        assert!(config.workspace.root.ends_with("workspaces"));

        assert_eq!(config.hooks.after_create, Some("echo 'created'".to_string()));
        assert_eq!(config.hooks.before_run, Some("echo 'running'".to_string()));
        assert_eq!(config.hooks.timeout_ms, 30000);

        assert_eq!(config.agent.max_concurrent_agents, 5);
        assert_eq!(config.agent.max_turns, 10);
        assert_eq!(config.agent.max_retry_backoff_ms, 60000);
        // State keys are normalized for lookup.
        assert_eq!(config.agent.limit_for_state("todo"), Some(2));
        assert_eq!(config.agent.limit_for_state("In Progress"), Some(3));

        assert_eq!(config.codex.command, "codex app-server --flag");
        assert_eq!(config.codex.turn_timeout_ms, 1800000);
        assert_eq!(config.codex.read_timeout_ms, 2000);
        assert_eq!(config.codex.stall_timeout_ms, 60000);

        assert_eq!(config.server.port, Some(0));
    }

    #[test]
    fn test_defaults_applied() {
        let workflow_dir = TempDir::new().expect("temp dir");
        let workflow_path = write_workflow(
            &workflow_dir,
            r#"---
tracker:
  kind: linear
  api_key: test-key
  project_slug: test
---
prompt
"#,
        );

        let workflow = crate::workflow::load_workflow(&workflow_path).expect("workflow loads");
        let config = load_config(&workflow, workflow_dir.path()).expect("config is valid");

        assert_eq!(config.tracker.endpoint, DEFAULT_LINEAR_ENDPOINT);
        assert_eq!(config.tracker.active_states, vec!["Todo", "In Progress"]);
        assert_eq!(
            config.tracker.terminal_states,
            vec!["Closed", "Cancelled", "Canceled", "Duplicate", "Done"]
        );
        assert_eq!(config.polling.interval_ms, DEFAULT_POLL_INTERVAL_MS);
        assert_eq!(config.hooks.timeout_ms, DEFAULT_HOOK_TIMEOUT_MS);
        assert_eq!(config.agent.max_concurrent_agents, DEFAULT_MAX_CONCURRENT_AGENTS);
        assert_eq!(config.agent.max_turns, DEFAULT_MAX_TURNS);
        assert_eq!(
            config.agent.max_retry_backoff_ms,
            DEFAULT_MAX_RETRY_BACKOFF_MS
        );
        assert_eq!(config.codex.command, DEFAULT_CODEX_COMMAND);
        assert_eq!(config.codex.turn_timeout_ms, DEFAULT_TURN_TIMEOUT_MS);
        assert_eq!(config.codex.read_timeout_ms, DEFAULT_READ_TIMEOUT_MS);
        assert_eq!(config.codex.stall_timeout_ms, DEFAULT_STALL_TIMEOUT_MS);
        assert_eq!(config.codex.approval_policy, DEFAULT_APPROVAL_POLICY);
        assert_eq!(config.codex.thread_sandbox, DEFAULT_THREAD_SANDBOX);
        assert_eq!(config.server.port, None);
        assert!(config.workspace.root.is_absolute());
    }

    #[test]
    fn test_config_validation_missing_api_key() {
        let workflow_dir = TempDir::new().expect("temp dir");
        let workflow_path = write_workflow(
            &workflow_dir,
            r#"---
tracker:
  kind: linear
  api_key: $NONEXISTENT_API_KEY_FOR_TEST_42
  project_slug: test
---

# Test
"#,
        );

        let workflow = crate::workflow::load_workflow(&workflow_path).expect("workflow loads");
        let result = load_config(&workflow, workflow_dir.path());
        assert!(matches!(result, Err(MissingTrackerApiKey)));
    }

    #[test]
    fn test_empty_api_key_variable_is_missing() {
        let workflow_dir = TempDir::new().expect("temp dir");
        let workflow_path = write_workflow(
            &workflow_dir,
            r#"---
tracker:
  kind: linear
  api_key: $EMPTY_API_KEY_FOR_TEST_42
  project_slug: test
---
prompt
"#,
        );

        // SAFETY: single-threaded test setup of the process environment.
        unsafe {
            std::env::set_var("EMPTY_API_KEY_FOR_TEST_42", "");
        }

        let workflow = crate::workflow::load_workflow(&workflow_path).expect("workflow loads");
        assert!(matches!(
            load_config(&workflow, workflow_dir.path()),
            Err(MissingTrackerApiKey)
        ));
    }

    #[test]
    fn test_config_validation_missing_project_slug() {
        let workflow_dir = TempDir::new().expect("temp dir");
        let workflow_path = write_workflow(
            &workflow_dir,
            r#"---
tracker:
  kind: linear
  api_key: test-key
---

# Test
"#,
        );

        let workflow = crate::workflow::load_workflow(&workflow_path).expect("workflow loads");
        assert!(matches!(
            load_config(&workflow, workflow_dir.path()),
            Err(ConfigValidation { .. })
        ));
    }

    #[test]
    fn test_unsupported_tracker_kind() {
        let workflow_dir = TempDir::new().expect("temp dir");
        let workflow_path = write_workflow(
            &workflow_dir,
            r#"---
tracker:
  kind: jira
  api_key: test-key
  project_slug: test
---
prompt
"#,
        );

        let workflow = crate::workflow::load_workflow(&workflow_path).expect("workflow loads");
        assert!(matches!(
            load_config(&workflow, workflow_dir.path()),
            Err(UnsupportedTrackerKind { .. })
        ));
    }

    #[test]
    fn test_invalid_hook_timeout_is_rejected() {
        let workflow_dir = TempDir::new().expect("temp dir");
        let workflow_path = write_workflow(
            &workflow_dir,
            r#"---
tracker:
  kind: linear
  api_key: test-key
  project_slug: test
hooks:
  timeout_ms: 0
---
prompt
"#,
        );

        let workflow = crate::workflow::load_workflow(&workflow_path).expect("workflow loads");
        assert!(matches!(
            load_config(&workflow, workflow_dir.path()),
            Err(ConfigValidation { .. })
        ));
    }

    #[test]
    fn test_invalid_max_turns_is_rejected() {
        let workflow_dir = TempDir::new().expect("temp dir");
        let workflow_path = write_workflow(
            &workflow_dir,
            r#"---
tracker:
  kind: linear
  api_key: test-key
  project_slug: test
agent:
  max_turns: -3
---
prompt
"#,
        );

        let workflow = crate::workflow::load_workflow(&workflow_path).expect("workflow loads");
        assert!(matches!(
            load_config(&workflow, workflow_dir.path()),
            Err(ConfigValidation { .. })
        ));
    }

    #[test]
    fn test_invalid_per_state_limits_are_ignored() {
        let workflow_dir = TempDir::new().expect("temp dir");
        let workflow_path = write_workflow(
            &workflow_dir,
            r#"---
tracker:
  kind: linear
  api_key: test-key
  project_slug: test
agent:
  max_concurrent_agents_by_state:
    Todo: 2
    Broken: nope
    Zero: 0
    Negative: -1
---
prompt
"#,
        );

        let workflow = crate::workflow::load_workflow(&workflow_path).expect("workflow loads");
        let config = load_config(&workflow, workflow_dir.path()).expect("config is valid");
        assert_eq!(config.agent.max_concurrent_agents_by_state.len(), 1);
        assert_eq!(config.agent.limit_for_state("TODO"), Some(2));
    }

    #[test]
    fn test_workspace_root_environment_expansion() {
        let workflow_dir = TempDir::new().expect("temp dir");
        let workflow_path = write_workflow(
            &workflow_dir,
            r#"---
tracker:
  kind: linear
  api_key: test-key
  project_slug: test
workspace:
  root: $SYMPHONY_TEST_WORKSPACE_ROOT_42
---
prompt
"#,
        );

        // SAFETY: single-threaded test setup of the process environment.
        unsafe {
            std::env::set_var("SYMPHONY_TEST_WORKSPACE_ROOT_42", "/tmp/symphony-test-root");
        }

        let workflow = crate::workflow::load_workflow(&workflow_path).expect("workflow loads");
        let config = load_config(&workflow, workflow_dir.path()).expect("config is valid");
        assert_eq!(config.workspace.root, PathBuf::from("/tmp/symphony-test-root"));
    }

    #[test]
    fn test_workspace_root_tilde_expansion() {
        let workflow_dir = TempDir::new().expect("temp dir");
        let workflow_path = write_workflow(
            &workflow_dir,
            r#"---
tracker:
  kind: linear
  api_key: test-key
  project_slug: test
workspace:
  root: ~/symphony-workspaces
---
prompt
"#,
        );

        let workflow = crate::workflow::load_workflow(&workflow_path).expect("workflow loads");
        let config = load_config(&workflow, workflow_dir.path()).expect("config is valid");
        let home = std::env::var("HOME").expect("HOME is set");
        assert_eq!(
            config.workspace.root,
            PathBuf::from(home).join("symphony-workspaces")
        );
    }

    #[test]
    fn test_relative_workspace_root_resolves_against_workflow_dir() {
        let workflow_dir = TempDir::new().expect("temp dir");
        let workflow_path = write_workflow(
            &workflow_dir,
            r#"---
tracker:
  kind: linear
  api_key: test-key
  project_slug: test
workspace:
  root: nested/workspaces
---
prompt
"#,
        );

        let workflow = crate::workflow::load_workflow(&workflow_path).expect("workflow loads");
        let config = load_config(&workflow, workflow_dir.path()).expect("config is valid");
        let expected = normalize_path(&workflow_dir.path().join("nested/workspaces"));
        assert_eq!(config.workspace.root, expected);
    }

    #[test]
    fn test_stall_timeout_zero_disables_stalls() {
        let workflow_dir = TempDir::new().expect("temp dir");
        let workflow_path = write_workflow(
            &workflow_dir,
            r#"---
tracker:
  kind: linear
  api_key: test-key
  project_slug: test
codex:
  stall_timeout_ms: -1
---
prompt
"#,
        );

        let workflow = crate::workflow::load_workflow(&workflow_path).expect("workflow loads");
        let config = load_config(&workflow, workflow_dir.path()).expect("config is valid");
        assert_eq!(config.codex.stall_timeout_ms, 0);
    }

    #[test]
    fn test_normalize_path_removes_parent_segments() {
        assert_eq!(
            normalize_path(Path::new("/tmp/root/../other/./nested")),
            PathBuf::from("/tmp/other/nested")
        );
    }
}
