use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Map, Value};

use crate::{HarnessError, HarnessResult};

#[derive(Default, Deserialize)]
pub struct AgentSettings {
    #[serde(default)]
    app: Map<String, Value>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ConfiguredCommand {
    pub executable: PathBuf,
    pub args: Vec<String>,
}

impl AgentSettings {
    pub fn read(path: &Path) -> HarnessResult<Self> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => {
                return Err(HarnessError::Protocol(format!(
                    "could not read agent settings at {}: {error}",
                    path.display()
                )));
            }
        };
        serde_json::from_slice(&bytes).map_err(|error| {
            HarnessError::Protocol(format!(
                "could not parse agent settings at {}: {error}",
                path.display()
            ))
        })
    }

    pub fn executable(&self, key: &str) -> HarnessResult<Option<PathBuf>> {
        let executable = self.value::<PathBuf>(key)?;
        if executable.as_ref().is_some_and(|path| !path.is_absolute()) {
            return Err(HarnessError::Protocol(format!(
                "{key} must be an absolute path"
            )));
        }
        Ok(executable)
    }

    pub fn command(&self, prefix: &str) -> HarnessResult<Option<ConfiguredCommand>> {
        let Some(executable) = self.executable(&format!("{prefix}.executable"))? else {
            return Ok(None);
        };
        let args = self.value(&format!("{prefix}.args"))?.unwrap_or_default();
        Ok(Some(ConfiguredCommand { executable, args }))
    }

    fn value<T: DeserializeOwned>(&self, key: &str) -> HarnessResult<Option<T>> {
        self.app
            .get(key)
            .filter(|value| !value.is_null())
            .map(|value| {
                serde_json::from_value(value.clone())
                    .map_err(|error| HarnessError::Protocol(format!("invalid {key}: {error}")))
            })
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_uses_the_configured_path_and_argument_boundaries()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let path = root.path().join("settings.json");
        assert_eq!(AgentSettings::read(&path)?.command("agents.claude")?, None);
        let executable = root.path().join("Claude Code ACP");
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({"app": {
                "agents.claude.executable": executable,
                "agents.claude.args": ["--option", "a value with spaces"],
                "agents.opencode.executable": "relative-invalid"
            }}))?,
        )?;
        let settings = AgentSettings::read(&path)?;
        assert_eq!(
            settings.command("agents.claude")?,
            Some(ConfiguredCommand {
                executable,
                args: vec!["--option".into(), "a value with spaces".into()],
            })
        );
        assert!(settings.executable("agents.opencode.executable").is_err());
        Ok(())
    }
}
