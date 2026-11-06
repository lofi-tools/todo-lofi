//! The live agent adapters the desktop app runs.
//!
//! Detection happens once at startup. `shepherd-core` stays CLI-agnostic, so the
//! core asks this factory both for the adapter and for the command it should
//! launch inside each task's directory.

use std::path::{Path, PathBuf};

use agent_harness::acp::AcpHarness;
use agent_harness::codex::{CodexHarness, CodexInstall};
use agent_harness::opencode::OpenCodeHarness;
use agent_harness::settings::ConfiguredCommand;
use agent_harness::{AgentHarness, HarnessError, SpawnSpec};
use shepherd_core::harness::HarnessFactory;
use shepherd_core::{Agent, CoreError};

const DEFAULT_CODEX_MODEL: &str = "gpt-6.1-terra";

pub struct DesktopHarnessFactory {
    pub codex: Box<dyn HarnessFactory>,
    pub opencode: Result<Option<PathBuf>, HarnessError>,
    pub claude: Result<Option<ConfiguredCommand>, HarnessError>,
}

impl HarnessFactory for DesktopHarnessFactory {
    fn create(&self) -> Result<Box<dyn AgentHarness>, CoreError> {
        self.codex.create()
    }

    fn spawn_spec(&self, cwd: PathBuf) -> SpawnSpec {
        self.codex.spawn_spec(cwd)
    }

    fn launch(
        &self,
        agent: Agent,
        cwd: PathBuf,
    ) -> Result<(Box<dyn AgentHarness>, SpawnSpec), CoreError> {
        let (harness, executable, args): (Box<dyn AgentHarness>, &Path, Vec<String>) = match agent {
            Agent::Codex => return self.codex.launch(agent, cwd),
            Agent::OpenCode => (
                Box::new(OpenCodeHarness::default()),
                require_configured(&self.opencode, "agents.opencode.executable")?,
                Vec::new(),
            ),
            Agent::Claude => {
                let command = require_configured(&self.claude, "agents.claude.executable")?;
                (
                    Box::new(AcpHarness::default()),
                    &command.executable,
                    command.args.clone(),
                )
            }
        };
        let program = executable
            .to_str()
            .ok_or_else(|| CoreError::HarnessMissing {
                report: format!("{} executable path must be UTF-8", agent.as_str()),
            })?;
        let spec = SpawnSpec {
            program: program.to_owned(),
            args,
            cwd,
            ..SpawnSpec::default()
        };
        Ok((harness, spec))
    }
}

fn require_configured<'a, T>(
    setting: &'a Result<Option<T>, HarnessError>,
    key: &str,
) -> Result<&'a T, CoreError> {
    match setting {
        Ok(Some(value)) => Ok(value),
        Ok(None) => Err(CoreError::HarnessMissing {
            report: format!("Set {key} in Shepherd's settings.json"),
        }),
        Err(error) => Err(CoreError::HarnessMissing {
            report: error.to_string(),
        }),
    }
}

pub(crate) fn codex_spawn_spec(program: &Path, cwd: PathBuf) -> SpawnSpec {
    SpawnSpec {
        program: program.to_string_lossy().into_owned(),
        args: vec![
            "-c".into(),
            format!("model=\"{DEFAULT_CODEX_MODEL}\""),
            "app-server".into(),
        ],
        cwd,
        ..SpawnSpec::default()
    }
}

pub struct CodexHarnessFactory {
    program: PathBuf,
    version: Option<String>,
}

impl CodexHarnessFactory {
    pub fn new(install: &CodexInstall) -> Self {
        Self {
            program: install.program.clone(),
            version: install.probe.version.clone(),
        }
    }
}

impl HarnessFactory for CodexHarnessFactory {
    fn create(&self) -> Result<Box<dyn AgentHarness>, CoreError> {
        let harness = CodexHarness::new();
        let harness = match &self.version {
            Some(version) => harness.with_version(version.clone()),
            None => harness,
        };
        Ok(Box::new(harness))
    }

    fn spawn_spec(&self, cwd: PathBuf) -> SpawnSpec {
        codex_spawn_spec(&self.program, cwd)
    }
}

/// Used when detection found no usable Codex. Prompting fails with the detection
/// report; the app never falls back to the scripted fake silently.
pub struct MissingCodexFactory {
    report: String,
}

impl MissingCodexFactory {
    pub fn new(report: String) -> Self {
        Self { report }
    }
}

impl HarnessFactory for MissingCodexFactory {
    fn create(&self) -> Result<Box<dyn AgentHarness>, CoreError> {
        Err(CoreError::HarnessMissing {
            report: self.report.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launches_the_configured_opencode_independently_of_codex() -> Result<(), CoreError> {
        let executable = PathBuf::from("/etc/profiles/per-user/me/bin/opencode");
        let factory = DesktopHarnessFactory {
            codex: Box::new(MissingCodexFactory::new("fixture".into())),
            opencode: Ok(Some(executable.clone())),
            claude: Ok(None),
        };
        let cwd = PathBuf::from("/tmp/worktree");
        let (_harness, spec) = factory.launch(Agent::OpenCode, cwd.clone())?;
        assert_eq!(Path::new(&spec.program), executable);
        assert_eq!(spec.cwd, cwd);
        assert!(factory.launch(Agent::Codex, cwd).is_err());
        Ok(())
    }

    #[test]
    fn launches_claude_through_the_configured_acp_command() -> Result<(), CoreError> {
        let executable = PathBuf::from("/tmp/claude-code-acp");
        let factory = DesktopHarnessFactory {
            codex: Box::new(MissingCodexFactory::new("fixture".into())),
            opencode: Ok(None),
            claude: Ok(Some(ConfiguredCommand {
                executable: executable.clone(),
                args: vec!["--flag".into(), "value with spaces".into()],
            })),
        };
        let cwd = PathBuf::from("/tmp/worktree");
        let (harness, spec) = factory.launch(Agent::Claude, cwd.clone())?;
        assert_eq!(harness.tier(), agent_harness::AdapterTier::Acp);
        assert_eq!(Path::new(&spec.program), executable);
        assert_eq!(spec.args, ["--flag", "value with spaces"]);
        assert_eq!(spec.cwd, cwd);
        Ok(())
    }
}
