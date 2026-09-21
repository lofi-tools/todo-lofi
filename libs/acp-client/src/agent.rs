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

    /// Resolve the launch for `cwd`, failing when the executable is missing.
    fn spawn_spec(&self, cwd: &Path) -> Result<SpawnSpec, AcpError> {
        let program =
            resolve_program(self.program()).ok_or_else(|| AcpError::AgentNotFound {
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
