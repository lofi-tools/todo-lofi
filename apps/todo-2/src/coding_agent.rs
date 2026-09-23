//! The agent profiles a coding run launches.
//!
//! opencode v1 and v2 are separate integrations. v1 accepts an inline config
//! through `OPENCODE_CONFIG_CONTENT`, so the interview runs as an app-defined
//! `todo-interview` agent with a read-only permission profile, switched with
//! `session/set_mode`.
//!
//! v2 ignores `OPENCODE_CONFIG_CONTENT` (and `OPENCODE_CONFIG`) on its ACP
//! path — verified against v2.0.14: the custom agent never appears in the
//! session's mode list and the switch fails with `Invalid params: mode not
//! found: todo-interview`. The v2 ACP server is backed by the long-running
//! background service, so per-launch environment never reaches config
//! loading. The v2 integration therefore uses only built-in agents: `plan`
//! (edits denied, switched with `session/set_config_option` on the `mode`
//! config id) for the interview, `build` for coding.
//!
//! Both integrations follow the same tool strategy per step: attach one MCP
//! server carrying exactly the tools the step may use — the narrow interview
//! set for the interview step, the wider set for the coding step — and run
//! one fresh agent session per step; run launches never resume a stored
//! session (see `start_run_session`).

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use acp_client::schema::{HttpHeader, McpServer, McpServerHttp};
use acp_client::{AcpError, AgentServer, OpencodeVersion, SpawnSpec};

/// Name of the agent the app defines in the injected config.
pub const INTERVIEW_AGENT_NAME: &str = "todo-interview";
/// Environment variable opencode reads an inline config from. The app never
/// writes to the user's own config.
pub const OPENCODE_CONFIG_CONTENT: &str = "OPENCODE_CONFIG_CONTENT";
/// Name the app attaches its loopback MCP endpoint under.
pub const MCP_SERVER_NAME: &str = "todo-lofi-coding";

/// What the read-only interview agent is told, as its system prompt. Kept free
/// of any file-writing instruction: the spec reaches the app through
/// `set_spec`, never through the filesystem.
pub const INTERVIEW_AGENT_PROMPT: &str = "\
You are running an interview for a coding task. Your job is to gather context \
and ask clarifying questions before saving a detailed spec.

## Rules

- You cannot change files. Read, search and inspect as much as you need, but \
never try to write, edit or patch a file.
- Save the finished spec with the `set_spec` tool. Do not write a spec file; \
the app stores the spec for you.
- On a later round, call `get_spec` first to read the spec you are revising.

## Process

1. Gather context about the request: read files, search the codebase, check \
   existing docs.
2. Ask non-obvious clarifying questions with the `ask_user` tool, in several \
   rounds when needed. Always use `ask_user` for questions, never plain text.
3. When you have enough context, call `set_spec` with the detailed spec \
   markdown, and with a spec or a coverage mark for every open subtask the \
   prompt enumerates.

## Spec contents

Capture everything you learned: requirements, constraints, decisions, open \
questions, and the planned approach.";

/// The description shown for the injected agent in opencode's UI.
pub const INTERVIEW_AGENT_DESCRIPTION: &str =
    "todo-lofi interview: gathers context and saves a spec; never changes files.";

/// Shell commands the read-only profile may run. The deny-all rule comes
/// first because opencode matches `bash` globs in order and the **last**
/// matching rule wins.
pub const READ_ONLY_BASH_RULES: &[(&str, &str)] = &[
    ("*", "deny"),
    ("cat *", "allow"),
    ("bat *", "allow"),
    ("head *", "allow"),
    ("tail *", "allow"),
    ("ls *", "allow"),
    ("tree *", "allow"),
    ("find *", "allow"),
    ("fd *", "allow"),
    ("rg *", "allow"),
    ("grep *", "allow"),
    ("wc *", "allow"),
    ("file *", "allow"),
    ("git status *", "allow"),
    ("git log *", "allow"),
    ("git diff *", "allow"),
    ("git show *", "allow"),
    ("git branch *", "allow"),
    ("git rev-parse *", "allow"),
];

/// The inline opencode config defining the read-only interview agent, and
/// making it the process's default agent. `opencode acp` has no flag selecting
/// an agent (`--agent` belongs to `opencode run`; the ACP server prints its
/// help and exits when given one), so the profile is chosen from the config
/// itself: `default_agent` names a primary agent, and a session created without
/// one would otherwise run as `build` with write tools.
///
/// Built by hand rather than through a JSON map so the `bash` rule order is
/// exactly [`READ_ONLY_BASH_RULES`]: a map would be re-sorted, and the
/// deny-all rule depends on coming first.
pub fn interview_config() -> String {
    let mut bash = String::new();
    for (index, (pattern, action)) in READ_ONLY_BASH_RULES.iter().enumerate() {
        if index > 0 {
            bash.push(',');
        }
        bash.push_str(&quote(pattern));
        bash.push(':');
        bash.push_str(&quote(action));
    }
    format!(
        concat!(
            r#"{{"$schema":"https://opencode.ai/config.json","default_agent":{},"agent":{{{}:{{"#,
            r#""mode":"primary","description":{},"prompt":{},"permission":{{"#,
            r#""edit":"deny","bash":{{{}}},"task":"allow","external_directory":"allow","#,
            r#""webfetch":"allow","websearch":"allow"}}}}}}}}"#,
        ),
        quote(INTERVIEW_AGENT_NAME),
        quote(INTERVIEW_AGENT_NAME),
        quote(INTERVIEW_AGENT_DESCRIPTION),
        quote(INTERVIEW_AGENT_PROMPT),
        bash,
    )
}

/// A JSON string literal for `value`. Infallible: `Value`'s `Display` never
/// fails, so no `unwrap` is needed on an escaping operation.
fn quote(value: &str) -> String {
    serde_json::Value::String(value.to_string()).to_string()
}

/// The `mode` config id the v2 ACP server expects in
/// `session/set_config_option` (its `session/set_mode` answers `mode not
/// found` for anything but the built-ins it serves).
pub const V2_MODE_CONFIG_ID: &str = "mode";
/// The built-in v2 agent the interview step runs as: edits denied, so the
/// model cannot change files. No config injection involved — v2 ignores
/// `OPENCODE_CONFIG_CONTENT` on its ACP path (verified against v2.0.14),
/// and only built-in agents appear in the session's mode list.
pub const V2_INTERVIEW_AGENT_NAME: &str = "plan";
/// The built-in v2 agent the coding step runs as: the full tool set.
pub const V2_CODING_AGENT_NAME: &str = "build";

/// Pick the run's agent integrations for the installed opencode generation:
/// v1 gets the injected `todo-interview` profile (switched with
/// `session/set_mode`), v2 gets the built-in `plan`/`build` pair (switched
/// with `session/set_config_option` on the `mode` config id). Unparseable
/// versions stay v1, preserving the behaviour the app shipped with.
pub fn select_run_agents() -> (Arc<dyn AgentServer>, Arc<dyn AgentServer>) {
    match acp_client::detect_opencode_version("opencode") {
        OpencodeVersion::V2 => (
            Arc::new(OpenCodeV2InterviewAgent),
            Arc::new(OpenCodeV2CodingAgent),
        ),
        OpencodeVersion::V1 => (
            Arc::new(OpenCodeInterviewAgent),
            Arc::new(acp_client::OpenCodeAgent),
        ),
    }
}

/// The read-only interview process: `opencode acp` with the profile injected
/// through the environment. Its own agent id means its persisted session cannot
/// be confused with the coding profile's.
pub struct OpenCodeInterviewAgent;

impl AgentServer for OpenCodeInterviewAgent {
    fn id(&self) -> &'static str {
        "opencode-interview"
    }

    fn display_name(&self) -> &'static str {
        "opencode (interview, read-only)"
    }

    fn program(&self) -> &'static str {
        "opencode"
    }

    fn args(&self) -> &'static [&'static str] {
        &["acp"]
    }

    fn session_mode(&self) -> Option<&'static str> {
        Some(INTERVIEW_AGENT_NAME)
    }

    fn spawn_spec(&self, cwd: &Path) -> Result<SpawnSpec, AcpError> {
        let program = acp_client::agent::resolve_program(self.program()).ok_or_else(|| {
            AcpError::AgentNotFound {
                program: self.program().to_string(),
            }
        })?;
        let mut env = BTreeMap::new();
        env.insert(OPENCODE_CONFIG_CONTENT.to_string(), interview_config());
        Ok(SpawnSpec {
            program,
            args: self.args().iter().map(|arg| (*arg).to_string()).collect(),
            cwd: cwd.to_path_buf(),
            env,
        })
    }
}

/// The v2 interview process: a plain `opencode acp` launch whose session is
/// switched to the built-in `plan` agent (edits denied) with
/// `session/set_config_option` (`mode`). No config is injected: v2 ignores
/// `OPENCODE_CONFIG_CONTENT` on its ACP path, so a custom agent would fail
/// the switch with `mode not found`. Its own agent id means its persisted
/// session cannot be confused with the coding profile's.
pub struct OpenCodeV2InterviewAgent;

/// The v2 coding process: a plain `opencode acp` launch switched to the
/// built-in `build` agent with the full tool set.
pub struct OpenCodeV2CodingAgent;

impl AgentServer for OpenCodeV2InterviewAgent {
    fn id(&self) -> &'static str {
        "opencode-interview-v2"
    }

    fn display_name(&self) -> &'static str {
        "opencode v2 (interview, plan)"
    }

    fn program(&self) -> &'static str {
        "opencode"
    }

    fn args(&self) -> &'static [&'static str] {
        &["acp"]
    }

    fn session_mode(&self) -> Option<&'static str> {
        Some(V2_INTERVIEW_AGENT_NAME)
    }

    fn opencode_version(&self) -> OpencodeVersion {
        OpencodeVersion::V2
    }
}

impl AgentServer for OpenCodeV2CodingAgent {
    fn id(&self) -> &'static str {
        "opencode-coding-v2"
    }

    fn display_name(&self) -> &'static str {
        "opencode v2 (coding, build)"
    }

    fn program(&self) -> &'static str {
        "opencode"
    }

    fn args(&self) -> &'static [&'static str] {
        &["acp"]
    }

    fn session_mode(&self) -> Option<&'static str> {
        Some(V2_CODING_AGENT_NAME)
    }

    fn opencode_version(&self) -> OpencodeVersion {
        OpencodeVersion::V2
    }
}
/// The app's loopback MCP endpoint plus the bearer token of one session.
/// A token is minted per launch and bound to the task that launch is for, so a
/// tool call can be attributed — and resolved — to the session that made it
/// (spec decision #11). `task_id` is that binding, kept here as well so the app
/// can tell whether the session it already has is the one this launch wants.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpEndpoint {
    pub url: String,
    pub token: String,
    pub profile: String,
    pub task_id: u64,
}

impl McpEndpoint {
    /// The ACP `McpServer` entry to attach to a session.
    pub fn server(&self) -> McpServer {
        McpServer::Http(
            McpServerHttp::new(MCP_SERVER_NAME, self.url.clone()).headers(vec![HttpHeader::new(
                "Authorization",
                format!("Bearer {}", self.token),
            )]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_interview_profile_is_read_only() {
        let config: serde_json::Value =
            serde_json::from_str(&interview_config()).expect("valid JSON");
        let agent = &config["agent"][INTERVIEW_AGENT_NAME];
        assert_eq!(agent["mode"], "primary");
        // The config is the only channel selecting the agent, so the process
        // must default to it.
        assert_eq!(config["default_agent"], INTERVIEW_AGENT_NAME);
        assert_eq!(agent["permission"]["edit"], "deny");
        // `task` stays allowed (decision #4); read tools are not denied here.
        assert_eq!(agent["permission"]["task"], "allow");
        assert_eq!(agent["permission"]["webfetch"], "allow");
        // The prompt must not tell the model to write a file.
        let prompt = agent["prompt"].as_str().expect("prompt");
        assert!(prompt.contains("set_spec"));
        assert!(!prompt.contains("docs/spec"));
    }

    #[test]
    fn the_bash_deny_all_rule_comes_first() {
        let config: serde_json::Value =
            serde_json::from_str(&interview_config()).expect("valid JSON");
        let rules = config["agent"][INTERVIEW_AGENT_NAME]["permission"]["bash"]
            .as_object()
            .expect("bash map");
        // opencode evaluates the last matching glob, so the deny-all rule must
        // be the first entry; serde_json's map is sorted, and `*` sorts first.
        let first = rules.keys().next().expect("a rule");
        assert_eq!(first, "*");
        assert_eq!(rules[first], "deny");
        // A destructive command is not exempted by any later rule.
        assert!(rules.get("rm *").is_none());
    }

    #[test]
    fn the_interview_agent_injects_its_config_and_selects_the_profile() {
        let agent = OpenCodeInterviewAgent;
        assert_eq!(agent.id(), "opencode-interview");
        // `opencode acp` takes no `--agent`: passed one it prints its help and
        // exits, so the handshake dies and no interview can start.
        assert_eq!(agent.args(), &["acp"]);
        assert_eq!(agent.session_mode(), Some(INTERVIEW_AGENT_NAME));
        let spec = agent
            .spawn_spec(Path::new("/tmp"))
            .expect("opencode resolves");
        assert_eq!(spec.args, vec!["acp"]);
        let injected = spec.env.get(OPENCODE_CONFIG_CONTENT).expect("injected");
        assert!(injected.contains(INTERVIEW_AGENT_NAME));
        // The process's own default agent is the read-only one: a session
        // created without a mode would otherwise run as `build`.
        let config: serde_json::Value = serde_json::from_str(injected).expect("valid JSON");
        assert_eq!(config["default_agent"], INTERVIEW_AGENT_NAME);
    }

    #[test]
    fn the_v2_agents_use_only_builtin_modes_and_inject_no_config() {
        // v2 ignores `OPENCODE_CONFIG_CONTENT` on its ACP path, so the only
        // switchable agents are the built-ins the session advertises
        // (`build`, `plan`): anything else fails with `mode not found`.
        let interview = OpenCodeV2InterviewAgent;
        assert_eq!(interview.opencode_version(), OpencodeVersion::V2);
        assert_eq!(interview.session_mode(), Some(V2_INTERVIEW_AGENT_NAME));
        assert_eq!(V2_INTERVIEW_AGENT_NAME, "plan");
        assert_eq!(interview.id(), "opencode-interview-v2");
        assert_eq!(interview.args(), &["acp"]);
        let spec = interview
            .spawn_spec(Path::new("/tmp"))
            .expect("opencode resolves");
        assert_eq!(spec.args, vec!["acp"]);
        assert!(
            !spec.env.contains_key(OPENCODE_CONFIG_CONTENT),
            "v2 must not inject a config the server ignores"
        );

        let coding = OpenCodeV2CodingAgent;
        assert_eq!(coding.opencode_version(), OpencodeVersion::V2);
        assert_eq!(coding.session_mode(), Some(V2_CODING_AGENT_NAME));
        assert_eq!(V2_CODING_AGENT_NAME, "build");
        assert_eq!(coding.id(), "opencode-coding-v2");
        assert!(!coding
            .spawn_spec(Path::new("/tmp"))
            .expect("opencode resolves")
            .env
            .contains_key(OPENCODE_CONFIG_CONTENT));
    }

    #[test]
    fn the_mcp_endpoint_carries_the_token_as_a_header() {
        let endpoint = McpEndpoint {
            url: "http://127.0.0.1:9/mcp".to_string(),
            token: "secret".to_string(),
            profile: "interview".to_string(),
            task_id: 7,
        };
        let McpServer::Http(http) = endpoint.server() else {
            panic!("an HTTP server");
        };
        assert_eq!(http.name, MCP_SERVER_NAME);
        assert_eq!(http.url, "http://127.0.0.1:9/mcp");
        assert_eq!(http.headers.len(), 1);
        assert_eq!(http.headers[0].value, "Bearer secret");
    }
}
