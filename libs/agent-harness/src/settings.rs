use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Map, Value};

use crate::{HarnessError, HarnessResult};

#[derive(Default, Deserialize)]
pub struct AgentSettings {
    #[serde(default)]
    app: Map<String, Value>,
    #[serde(default)]
    projects: BTreeMap<PathBuf, Map<String, Value>>,
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
        self.absolute_path(key)
    }

    pub(crate) fn absolute_path(&self, key: &str) -> HarnessResult<Option<PathBuf>> {
        let path = self.value::<PathBuf>(key)?;
        if path.as_ref().is_some_and(|path| !path.is_absolute()) {
            return Err(HarnessError::Protocol(format!(
                "{key} must be an absolute path"
            )));
        }
        Ok(path)
    }

    pub fn command(&self, prefix: &str) -> HarnessResult<Option<ConfiguredCommand>> {
        let Some(executable) = self.executable(&format!("{prefix}.executable"))? else {
            return Ok(None);
        };
        let args = self.value(&format!("{prefix}.args"))?.unwrap_or_default();
        Ok(Some(ConfiguredCommand { executable, args }))
    }

    fn value<T: DeserializeOwned>(&self, key: &str) -> HarnessResult<Option<T>> {
        Self::decode(key, self.app.get(key))
    }

    pub fn project_value<T: DeserializeOwned>(
        &self,
        project: &Path,
        key: &str,
    ) -> HarnessResult<Option<T>> {
        let value = self
            .projects
            .get(project)
            .and_then(|settings| settings.get(key))
            .filter(|value| !value.is_null())
            .or_else(|| self.app.get(key));
        Self::decode(key, value)
    }

    fn decode<T: DeserializeOwned>(key: &str, value: Option<&Value>) -> HarnessResult<Option<T>> {
        value
            .filter(|value| !value.is_null())
            .map(|value| {
                T::deserialize(value)
                    .map_err(|error| HarnessError::Protocol(format!("invalid {key}: {error}")))
            })
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_settings_override_app_values_per_key() -> Result<(), Box<dyn std::error::Error>> {
        let settings: AgentSettings = serde_json::from_value(serde_json::json!({
            "app": {"limit": 50, "other": true},
            "projects": {
                "/project": {"limit": 15},
                "/inherits": {"limit": null},
                "/invalid": {"limit": "fifty"}
            }
        }))?;
        assert_eq!(
            settings.project_value::<u64>(Path::new("/project"), "limit")?,
            Some(15)
        );
        assert_eq!(
            settings.project_value::<bool>(Path::new("/project"), "other")?,
            Some(true)
        );
        assert_eq!(
            settings.project_value::<u64>(Path::new("/inherits"), "limit")?,
            Some(50)
        );
        assert_eq!(
            settings.project_value::<u64>(Path::new("/elsewhere"), "missing")?,
            None
        );
        assert!(
            settings
                .project_value::<u64>(Path::new("/invalid"), "limit")
                .is_err()
        );
        Ok(())
    }

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
