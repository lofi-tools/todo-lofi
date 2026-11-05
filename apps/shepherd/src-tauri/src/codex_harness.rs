//! The live agent the desktop app runs.
//!
//! Detection happens once at startup. `shepherd-core` stays CLI-agnostic, so the
//! core asks this factory both for the adapter and for the command it should
//! launch inside each task's directory.

use std::path::{Path, PathBuf};

use agent_harness::codex::{CodexHarness, CodexInstall};
use agent_harness::{AgentHarness, SpawnSpec};
use shepherd_core::CoreError;
use shepherd_core::harness::HarnessFactory;

const DEFAULT_CODEX_MODEL: &str = "gpt-6.1-terra";

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
