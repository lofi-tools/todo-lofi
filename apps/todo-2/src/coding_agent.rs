//! The agent profiles a coding run launches.
//!
//! The interview phase must not be able to change files, so it runs in its own
//! opencode process with a read-only profile injected through
//! `OPENCODE_CONFIG_CONTENT` (see
//! `docs/spec/coding-interview-readonly-session-spec.md` §5). The coding
//! phases keep the default `opencode acp` launch and its full tool set.

use std::collections::BTreeMap;
use std::path::Path;

use acp_client::schema::{HttpHeader, McpServer, McpServerHttp};
use acp_client::{AcpError, AgentServer, SpawnSpec};

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

/// The app's loopback MCP endpoint plus the bearer token of one agent profile.
/// Each profile gets its own token, so a tool call can be attributed to the
/// process that made it (spec decision #11).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpEndpoint {
    pub url: String,
    pub token: String,
    pub profile: String,
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
    fn the_mcp_endpoint_carries_the_token_as_a_header() {
        let endpoint = McpEndpoint {
            url: "http://127.0.0.1:9/mcp".to_string(),
            token: "secret".to_string(),
            profile: "interview".to_string(),
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
