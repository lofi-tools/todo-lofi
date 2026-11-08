//! The live agent adapters the desktop app runs.
//!
//! Detection happens once at startup. `shepherd-core` stays CLI-agnostic, so the
//! core asks this factory both for the adapter and for the command it should
//! launch inside each task's directory.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use agent_harness::acp::{AcpHarness, TerminalBackend};
use agent_harness::codex::{CodexHarness, CodexInstall, CodexTier, ExecHarness};
use agent_harness::opencode::OpenCodeHarness;
use agent_harness::pi::PiHarness;
use agent_harness::settings::{AgentSettings, ConfiguredCommand};
use agent_harness::{AgentHarness, FailureKind, HarnessError, SpawnSpec};
use shepherd_core::harness::HarnessFactory;
use shepherd_core::{Agent, CoreError};

const DEFAULT_CODEX_MODEL: &str = "gpt-6.1-terra";

pub struct DesktopHarnessFactory {
    pub codex: Box<dyn HarnessFactory>,
    pub opencode: Result<Option<SpawnSpec>, HarnessError>,
    pub pi: Result<Option<ConfiguredCommand>, HarnessError>,
    pub claude: Result<Option<ConfiguredCommand>, HarnessError>,
    pub settings: Result<AgentSettings, String>,
    pub terminals: Arc<crate::bridge::TerminalHost>,
}

impl HarnessFactory for DesktopHarnessFactory {
    fn create(&self) -> Result<Box<dyn AgentHarness>, CoreError> {
        self.codex.create()
    }

    fn environment_for_project(
        &self,
        agent: Agent,
        project: &Path,
    ) -> Result<BTreeMap<String, String>, CoreError> {
        let settings = self
            .settings
            .as_ref()
            .map_err(|error| HarnessError::Protocol(error.clone()))?;
        let key = format!("agents.{}.environment", agent.as_str());
        let values: BTreeMap<String, String> =
            settings.project_value(project, &key)?.unwrap_or_default();
        if values
            .keys()
            .any(|key| key.is_empty() || key.contains(['=', '\0']))
        {
            return Err(HarnessError::Protocol(format!(
                "{key} contains an invalid environment variable name"
            ))
            .into());
        }
        if values.values().any(|value| value.contains('\0')) {
            return Err(HarnessError::Protocol(format!(
                "{key} contains an invalid environment value"
            ))
            .into());
        }
        Ok(values)
    }

    fn spawn_spec(&self, cwd: PathBuf) -> SpawnSpec {
        self.codex.spawn_spec(cwd)
    }

    fn launch(
        &self,
        agent: Agent,
        cwd: PathBuf,
    ) -> Result<(Box<dyn AgentHarness>, SpawnSpec), CoreError> {
        self.launch_with_terminals(agent, cwd, self.terminals.clone())
    }

    fn launch_for_task(
        &self,
        task_id: &shepherd_core::TaskId,
        agent: Agent,
        cwd: PathBuf,
    ) -> Result<(Box<dyn AgentHarness>, SpawnSpec), CoreError> {
        let (mut harness, mut spec) =
            self.launch_with_terminals(agent, cwd, self.terminals.for_task(task_id.clone()))?;
        let token = uuid::Uuid::new_v4().to_string();
        spec.environment
            .insert(agent_harness::PROCESS_TOKEN_ENV.into(), token.clone());
        harness.set_process_tracker(self.terminals.agent_process_tracker(task_id, token));
        Ok((harness, spec))
    }
}

impl DesktopHarnessFactory {
    fn launch_with_terminals(
        &self,
        agent: Agent,
        cwd: PathBuf,
        terminals: Arc<dyn TerminalBackend>,
    ) -> Result<(Box<dyn AgentHarness>, SpawnSpec), CoreError> {
        let (harness, executable, args): (Box<dyn AgentHarness>, &Path, Vec<String>) = match agent {
            Agent::Codex => return self.codex.launch(agent, cwd),
            Agent::OpenCode => {
                let launch = require_configured(&self.opencode, "agents.opencode.executable")?;
                return Ok((
                    Box::new(OpenCodeHarness::default().with_terminals(terminals)),
                    SpawnSpec {
                        cwd,
                        ..launch.clone()
                    },
                ));
            }
            Agent::Pi => {
                let command = require_configured(&self.pi, "agents.pi.executable")?;
                (
                    Box::new(PiHarness::new()),
                    &command.executable,
                    command.args.clone(),
                )
            }
            Agent::Claude => {
                let command = require_configured(&self.claude, "agents.claude.executable")?;
                (
                    Box::new(AcpHarness::default().with_terminals(terminals)),
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
        Err(error) => Err(HarnessError::Protocol(error.to_string()).into()),
    }
}

pub(crate) fn codex_spawn_spec(program: &Path, cwd: PathBuf) -> SpawnSpec {
    let mut spec = codex_command_spec(program, cwd);
    spec.args.push("app-server".into());
    spec
}

fn codex_command_spec(program: &Path, cwd: PathBuf) -> SpawnSpec {
    SpawnSpec {
        program: program.to_string_lossy().into_owned(),
        args: vec!["-c".into(), format!("model=\"{DEFAULT_CODEX_MODEL}\"")],
        cwd,
        ..SpawnSpec::default()
    }
}

pub struct CodexHarnessFactory {
    program: PathBuf,
    version: Option<String>,
    tier: CodexTier,
    images: bool,
}

impl CodexHarnessFactory {
    pub fn new(install: &CodexInstall) -> Self {
        Self {
            program: install.program.clone(),
            version: install.probe.version.clone(),
            tier: install.probe.tier,
            images: install.probe.images,
        }
    }
}

impl HarnessFactory for CodexHarnessFactory {
    fn create(&self) -> Result<Box<dyn AgentHarness>, CoreError> {
        match self.tier {
            CodexTier::AppServer => {
                let mut harness = CodexHarness::new();
                if let Some(version) = &self.version {
                    harness = harness.with_version(version.clone());
                }
                Ok(Box::new(harness))
            }
            CodexTier::Exec => {
                let mut harness = ExecHarness::new().with_images(self.images);
                if let Some(version) = &self.version {
                    harness = harness.with_version(version.clone());
                }
                Ok(Box::new(harness))
            }
        }
    }

    fn spawn_spec(&self, cwd: PathBuf) -> SpawnSpec {
        match self.tier {
            CodexTier::AppServer => codex_spawn_spec(&self.program, cwd),
            CodexTier::Exec => codex_command_spec(&self.program, cwd),
        }
    }
}

/// Used when detection found no usable Codex. Prompting fails with the detection
/// report; the app never falls back to the scripted fake silently.
pub struct MissingCodexFactory {
    report: String,
    failure: FailureKind,
}

impl MissingCodexFactory {
    pub fn new(report: String, failure: FailureKind) -> Self {
        Self { report, failure }
    }
}

impl HarnessFactory for MissingCodexFactory {
    fn create(&self) -> Result<Box<dyn AgentHarness>, CoreError> {
        match self.failure {
            FailureKind::MissingBinary => Err(CoreError::HarnessMissing {
                report: self.report.clone(),
            }),
            kind => Err(HarnessError::Reported {
                kind,
                message: self.report.clone(),
            }
            .into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn launches_the_detected_codex_exec_fallback() -> Result<(), Box<dyn std::error::Error>> {
        use agent_harness::codex::{CodexInstallKind, probe};
        use agent_harness::{HarnessEvent, PromptInput, TurnFinishReason};
        use std::os::unix::fs::PermissionsExt;

        let directory = shepherd_core::test_support::TestDirectory::new()?;
        let program = directory.join("codex-fixture");
        std::fs::write(
            &program,
            "#!/bin/sh\ncase \"$*\" in\n  --version) echo 'codex-cli 0.9.0';;\n  app-server) IFS= read -r line; exit 0;;\n  'exec --help') echo '  --json';;\n  'exec --json '*) exit 0;;\n  *) exit 12;;\nesac\n",
        )?;
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700))?;
        let probe = probe(
            program.to_str().ok_or("fixture path is not UTF-8")?,
            directory.path(),
        )
        .await?;
        assert_eq!(probe.tier, CodexTier::Exec);
        let factory = CodexHarnessFactory::new(&CodexInstall {
            program,
            kind: CodexInstallKind::ProfilePath,
            probe,
        });
        let (mut harness, spec) = factory.launch(Agent::Codex, directory.path().to_path_buf())?;
        assert_eq!(spec.args, ["-c", "model=\"gpt-6.1-terra\""]);
        let capabilities = harness.start(spec).await?;
        assert!(!capabilities.resume && !capabilities.approvals && capabilities.cancel);
        let diagnostics = harness.diagnostics();
        assert!(diagnostics.command.contains("exec --json -c"));
        assert!(diagnostics.protocol.contains("codex-cli 0.9.0"));
        assert!(diagnostics.fallback_reason.is_some());
        harness.open_session(None).await?;
        harness.prompt(PromptInput::text("fixture")).await?;
        let reason = tokio::time::timeout(std::time::Duration::from_secs(4), async {
            while let Some(event) = harness.events().recv().await {
                if let HarnessEvent::TurnFinished { reason } = event {
                    return Some(reason);
                }
            }
            None
        })
        .await?;
        assert_eq!(reason, Some(TurnFinishReason::Completed));
        harness.shutdown().await?;
        Ok(())
    }

    #[test]
    fn agent_environment_uses_project_overrides() -> Result<(), Box<dyn std::error::Error>> {
        let settings = serde_json::from_value(serde_json::json!({
            "app": {"agents.codex.environment": {"TOKEN": "!cat token"}},
            "projects": {"/project": {"agents.codex.environment": {"TOKEN": "!cat project-token"}}}
        }))?;
        let factory = DesktopHarnessFactory {
            codex: Box::new(MissingCodexFactory::new(
                "fixture".into(),
                FailureKind::MissingBinary,
            )),
            opencode: Ok(None),
            pi: Ok(None),
            claude: Ok(None),
            settings: Ok(settings),
            terminals: Arc::new(crate::bridge::TerminalHost::default()),
        };
        assert_eq!(
            factory
                .environment_for_project(Agent::Codex, Path::new("/project"))?
                .get("TOKEN")
                .map(String::as_str),
            Some("!cat project-token")
        );
        assert_eq!(
            factory
                .environment_for_project(Agent::Codex, Path::new("/other"))?
                .get("TOKEN")
                .map(String::as_str),
            Some("!cat token")
        );
        assert!(
            factory
                .environment_for_project(Agent::Claude, Path::new("/project"))?
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn launches_the_configured_opencode_independently_of_codex()
    -> Result<(), Box<dyn std::error::Error>> {
        let executable = PathBuf::from("/etc/profiles/per-user/me/bin/opencode");
        let config_file = "/tmp/OpenCode config.json";
        let settings = serde_json::from_value(serde_json::json!({"app": {
            "agents.opencode.executable": executable,
            "agents.opencode.config_file": config_file
        }}))?;
        let factory = DesktopHarnessFactory {
            codex: Box::new(MissingCodexFactory::new(
                "fixture".into(),
                FailureKind::MissingBinary,
            )),
            opencode: agent_harness::opencode::configured_launch(&settings),
            pi: Ok(None),
            claude: Ok(None),
            settings: Ok(settings),
            terminals: Arc::new(crate::bridge::TerminalHost::default()),
        };
        let cwd = PathBuf::from("/tmp/worktree");
        let (_harness, spec) = factory.launch(Agent::OpenCode, cwd.clone())?;
        assert_eq!(Path::new(&spec.program), executable);
        assert_eq!(
            spec.environment.get("OPENCODE_CONFIG").map(String::as_str),
            Some(config_file)
        );
        assert_eq!(spec.cwd, cwd);
        assert!(factory.launch(Agent::Codex, cwd).is_err());
        Ok(())
    }

    #[test]
    fn launches_claude_through_the_configured_acp_command() -> Result<(), CoreError> {
        let executable = PathBuf::from("/tmp/claude-code-acp");
        let factory = DesktopHarnessFactory {
            codex: Box::new(MissingCodexFactory::new(
                "fixture".into(),
                FailureKind::MissingBinary,
            )),
            opencode: Ok(None),
            pi: Ok(None),
            claude: Ok(Some(ConfiguredCommand {
                executable: executable.clone(),
                args: vec!["--flag".into(), "value with spaces".into()],
            })),
            settings: Ok(AgentSettings::default()),
            terminals: Arc::new(crate::bridge::TerminalHost::default()),
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
