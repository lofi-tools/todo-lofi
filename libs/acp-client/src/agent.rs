//! The agent registry: which external ACP-capable binary to run, and how to
//! launch it in a project directory.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::AcpError;

/// A resolved process launch: program, arguments, working directory, env.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpawnSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
}

impl SpawnSpec {
    /// The command as a user could retype it, for error messages and the
    /// launch state in the UI.
    pub fn command_line(&self) -> String {
        let mut line = self.program.display().to_string();
        for arg in &self.args {
            line.push(' ');
            line.push_str(arg);
        }
        line
    }
}

/// An external coding agent that speaks ACP over stdio.
pub trait AgentServer: Send + Sync + 'static {
    /// Stable identifier, used as the persistence key for sessions.
    fn id(&self) -> &'static str;
    /// Human-facing name, shown in the pane header.
    fn display_name(&self) -> &'static str;
    /// Executable to launch.
    fn program(&self) -> &'static str;
    /// Arguments selecting ACP mode.
    fn args(&self) -> &'static [&'static str];

    /// The ACP session mode (agent name) this profile must run under. `None`
    /// keeps whatever agent the process starts in; a profile whose tools are
    /// part of its contract names its agent here, so the launch switches the
    /// session before it can be prompted.
    fn session_mode(&self) -> Option<&'static str> {
        None
    }

    /// Which opencode generation this integration targets. The pane uses it
    /// to pick the mode switch: v1 `session/set_mode`, v2
    /// `session/set_config_option` with the `mode` config id.
    fn opencode_version(&self) -> OpencodeVersion {
        OpencodeVersion::V1
    }

    /// Resolve the launch for `cwd`, failing when the executable is missing.
    fn spawn_spec(&self, cwd: &Path) -> Result<SpawnSpec, AcpError> {
        let program = resolve_program(self.program()).ok_or_else(|| AcpError::AgentNotFound {
            program: self.program().to_string(),
        })?;
        Ok(SpawnSpec {
            program,
            args: self.args().iter().map(|arg| arg.to_string()).collect(),
            cwd: cwd.to_path_buf(),
            env: BTreeMap::new(),
        })
    }

    /// The command line shown while launching, resolved against a directory.
    fn command_line(&self, cwd: &Path) -> String {
        match self.spawn_spec(cwd) {
            Ok(spec) => spec.command_line(),
            Err(_) => format!("{} {}", self.program(), self.args().join(" ")),
        }
    }
}

/// opencode's ACP mode: `opencode acp`.
pub struct OpenCodeAgent;

impl AgentServer for OpenCodeAgent {
    fn id(&self) -> &'static str {
        "opencode"
    }

    fn display_name(&self) -> &'static str {
        "opencode"
    }

    fn program(&self) -> &'static str {
        "opencode"
    }

    fn args(&self) -> &'static [&'static str] {
        &["acp"]
    }
}

/// Which opencode generation an [`AgentServer`] targets. v1 and v2 differ in
/// config shape (`agent`+`permission`+`prompt` vs `agents`+`permissions`+
/// `system`), in MCP placement (`mcp.<name>` vs `mcp.servers.<name>`), and in
/// the mode switch (`session/set_mode` vs `session/set_config_option` with
/// the `mode` config id). Sending a v1 switch to a v2 server fails with
/// `Invalid params: mode not found: <name>`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OpencodeVersion {
    /// opencode 1.x: `session/set_mode` selects the agent.
    #[default]
    V1,
    /// opencode 2.x: `session/set_config_option` (`mode`) selects the agent.
    V2,
}

impl OpencodeVersion {
    /// Parse the major version out of `opencode --version` output, which
    /// looks like `opencode v2.0.6` or `2.0.6`. Anything unparseable is v1,
    /// preserving the behaviour the app shipped with.
    pub fn parse_version_output(output: &str) -> Self {
        let digits = output
            .split(|char: char| !char.is_ascii_digit())
            .filter(|part| !part.is_empty());
        match digits.clone().next().map(str::parse::<u64>) {
            Some(Ok(2..)) => OpencodeVersion::V2,
            _ => OpencodeVersion::V1,
        }
    }
}

/// Run `<program> --version` and report its generation. Missing binaries and
/// unreadable output fall back to [`OpencodeVersion::V1`].
pub fn detect_opencode_version(program: &str) -> OpencodeVersion {
    let output = std::process::Command::new(program)
        .arg("--version")
        .output()
        .ok()
        .and_then(|result| String::from_utf8(result.stdout).ok())
        .unwrap_or_default();
    OpencodeVersion::parse_version_output(&output)
}
/// Locate `program` on `PATH`. A path containing a separator is used as given.
pub fn resolve_program(program: &str) -> Option<PathBuf> {
    if program.contains(std::path::MAIN_SEPARATOR) {
        let path = PathBuf::from(program);
        return is_executable(&path).then_some(path);
    }
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var)
        .map(|dir| dir.join(program))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_output_selects_the_integration() {
        assert_eq!(
            OpencodeVersion::parse_version_output("opencode v2.0.6"),
            OpencodeVersion::V2
        );
        assert_eq!(
            OpencodeVersion::parse_version_output("2.1.0"),
            OpencodeVersion::V2
        );
        assert_eq!(
            OpencodeVersion::parse_version_output("opencode v1.9.3"),
            OpencodeVersion::V1
        );
        // Unparseable output keeps the v1 behaviour the app shipped with.
        assert_eq!(
            OpencodeVersion::parse_version_output(""),
            OpencodeVersion::V1
        );
        assert_eq!(
            OpencodeVersion::parse_version_output("not an agent"),
            OpencodeVersion::V1
        );
    }
}
