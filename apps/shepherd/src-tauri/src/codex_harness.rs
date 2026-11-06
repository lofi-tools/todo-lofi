//! The live agent adapters the desktop app runs.
//!
//! Detection happens once at startup. `shepherd-core` stays CLI-agnostic, so the
//! core asks this factory both for the adapter and for the command it should
//! launch inside each task's directory.

use std::path::{Path, PathBuf};

use agent_harness::codex::{CodexHarness, CodexInstall};
use agent_harness::opencode::OpenCodeHarness;
use agent_harness::{AgentHarness, HarnessError, SpawnSpec};
use shepherd_core::harness::HarnessFactory;
use shepherd_core::{Agent, CoreError};

const DEFAULT_CODEX_MODEL: &str = "gpt-6.1-terra";

pub struct DesktopHarnessFactory {
    pub codex: Box<dyn HarnessFactory>,
    pub opencode: Result<Option<PathBuf>, HarnessError>,
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
        if agent == Agent::Codex {
            return self.codex.launch(agent, cwd);
        }
        let program = match &self.opencode {
            Ok(Some(path)) => path.to_str().ok_or_else(|| CoreError::HarnessMissing {
                report: "OpenCode executable path must be UTF-8".into(),
            })?,
            Ok(None) => {
                return Err(CoreError::HarnessMissing {
                    report: "Set agents.opencode.executable in Shepherd's settings.json".into(),
                });
            }
            Err(error) => {
                return Err(CoreError::HarnessMissing {
                    report: error.to_string(),
                });
            }
        };
        let spec = SpawnSpec {
            program: program.to_owned(),
            cwd,
            ..SpawnSpec::default()
        };
        Ok((Box::new(OpenCodeHarness::default()), spec))
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
        };
        let cwd = PathBuf::from("/tmp/worktree");
        let (_harness, spec) = factory.launch(Agent::OpenCode, cwd.clone())?;
        assert_eq!(Path::new(&spec.program), executable);
        assert_eq!(spec.cwd, cwd);
        assert!(factory.launch(Agent::Codex, cwd).is_err());
        Ok(())
    }
}
