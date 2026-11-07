use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Map, Value};

use crate::{HarnessError, HarnessResult};

const MAX_ENVIRONMENT_OUTPUT: u64 = 1024 * 1024;

#[cfg(unix)]
struct EnvironmentProcessGroup(Option<i32>);

#[cfg(unix)]
impl EnvironmentProcessGroup {
    fn stop(&mut self) -> std::io::Result<()> {
        let Some(group) = self.0.take() else {
            return Ok(());
        };
        if unsafe { libc::killpg(group, libc::SIGKILL) } == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(error)
        }
    }

    fn disarm(&mut self) {
        self.0 = None;
    }
}

#[cfg(unix)]
impl Drop for EnvironmentProcessGroup {
    fn drop(&mut self) {
        if let Err(error) = self.stop() {
            tracing::warn!(%error, "could not stop a cancelled environment command process group");
        }
    }
}

pub async fn resolve_environment(
    environment: &mut BTreeMap<String, String>,
    cwd: &Path,
) -> HarnessResult<()> {
    resolve_environment_with_timeout(environment, cwd, std::time::Duration::from_secs(5)).await
}

async fn resolve_environment_with_timeout(
    environment: &mut BTreeMap<String, String>,
    cwd: &Path,
    deadline: std::time::Duration,
) -> HarnessResult<()> {
    use tokio::io::AsyncReadExt;
    let mut resolved = BTreeMap::new();
    for (key, value) in environment.iter() {
        let Some(command) = value.strip_prefix('!') else {
            continue;
        };
        let mut process = tokio::process::Command::new("/bin/sh");
        process
            .args(["-c", command])
            .current_dir(cwd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        #[cfg(unix)]
        process.process_group(0);
        let mut child = process.spawn().map_err(|error| {
            HarnessError::Protocol(format!(
                "Could not start environment command for {key}: {error}"
            ))
        })?;
        #[cfg(unix)]
        let mut group = EnvironmentProcessGroup(child.id().and_then(|id| i32::try_from(id).ok()));
        let stdout = child.stdout.take().ok_or_else(|| {
            HarnessError::Protocol(format!("Could not capture environment command for {key}"))
        })?;
        let capture = async {
            let read = async {
                let mut output = Vec::new();
                stdout
                    .take(MAX_ENVIRONMENT_OUTPUT + 1)
                    .read_to_end(&mut output)
                    .await
                    .map_err(|error| {
                        HarnessError::Protocol(format!(
                            "Could not read environment command for {key}: {error}"
                        ))
                    })?;
                if output.len() as u64 > MAX_ENVIRONMENT_OUTPUT {
                    return Err(HarnessError::Protocol(format!(
                        "Environment command for {key} produced too much output"
                    )));
                }
                Ok(output)
            };
            let wait = async {
                let status = child.wait().await.map_err(|error| {
                    HarnessError::Protocol(format!(
                        "Could not wait for environment command for {key}: {error}"
                    ))
                })?;
                if !status.success() {
                    return Err(HarnessError::Protocol(format!(
                        "Environment command for {key} failed ({status})"
                    )));
                }
                Ok(())
            };
            let (output, ()) = tokio::try_join!(read, wait)?;
            let mut output = String::from_utf8(output).map_err(|_| {
                HarnessError::Protocol(format!(
                    "Environment command for {key} did not produce UTF-8"
                ))
            })?;
            if output.ends_with('\n') {
                output.pop();
            }
            if output.contains('\0') {
                return Err(HarnessError::Protocol(format!(
                    "Environment command for {key} produced a NUL byte"
                )));
            }
            Ok(output)
        };
        let outcome = tokio::time::timeout(deadline, capture).await;
        #[cfg(unix)]
        if !matches!(outcome, Ok(Ok(_))) {
            // Commands can leave descendants holding stdout after the shell exits.
            if let Err(error) = group.stop() {
                let reason = match &outcome {
                    Err(_) => format!("Environment command for {key} timed out"),
                    Ok(Err(reason)) => reason.to_string(),
                    Ok(Ok(_)) => format!("Environment command for {key} finished"),
                };
                return Err(HarnessError::Protocol(format!(
                    "{reason}; could not stop environment command for {key}: {error}"
                )));
            }
        } else {
            group.disarm();
        }
        let output = outcome.map_err(|_| {
            HarnessError::Protocol(format!("Environment command for {key} timed out"))
        })??;
        resolved.insert(key.clone(), output);
    }
    environment.extend(resolved);
    Ok(())
}

#[derive(Clone, Default, Deserialize)]
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

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelling_environment_resolution_stops_descendants()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let cwd = directory.path().to_owned();
        let task = tokio::spawn(async move {
            let mut values = BTreeMap::from([(
                "TOKEN".into(),
                "!(sleep 1; printf leaked > escaped) & printf ready > ready; wait".into(),
            )]);
            resolve_environment(&mut values, &cwd).await
        });
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while !directory.path().join("ready").exists() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await?;
        task.abort();
        assert!(task.await.is_err_and(|error| error.is_cancelled()));
        tokio::time::sleep(std::time::Duration::from_millis(1300)).await;
        assert!(!directory.path().join("escaped").exists());
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn failed_environment_resolution_preserves_all_values_for_retry()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        std::fs::write(directory.path().join("value"), "first")?;
        let mut values = BTreeMap::from([
            ("A_TOKEN".into(), "!cat value".into()),
            ("Z_TOKEN".into(), "!exit 1".into()),
        ]);
        assert!(
            resolve_environment(&mut values, directory.path())
                .await
                .is_err()
        );
        assert_eq!(
            values.get("A_TOKEN").map(String::as_str),
            Some("!cat value")
        );
        assert_eq!(values.get("Z_TOKEN").map(String::as_str), Some("!exit 1"));
        std::fs::write(directory.path().join("value"), "second")?;
        values.insert("Z_TOKEN".into(), "!printf ready".into());
        resolve_environment(&mut values, directory.path()).await?;
        assert_eq!(values.get("A_TOKEN").map(String::as_str), Some("second"));
        assert_eq!(values.get("Z_TOKEN").map(String::as_str), Some("ready"));
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn environment_commands_resolve_at_launch_without_exposing_output()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        std::fs::write(directory.path().join("token"), "secret\n\n")?;
        let mut values = BTreeMap::from([
            ("TOKEN".into(), "!cat token".into()),
            ("LITERAL".into(), "value".into()),
        ]);
        resolve_environment(&mut values, directory.path()).await?;
        assert_eq!(values.get("TOKEN").map(String::as_str), Some("secret\n"));
        assert_eq!(values.get("LITERAL").map(String::as_str), Some("value"));
        let mut failure = BTreeMap::from([(
            "TOKEN".into(),
            "!printf secret; printf secret >&2; exit 1".into(),
        )]);
        let error = resolve_environment(&mut failure, directory.path())
            .await
            .expect_err("must fail");
        assert!(!error.to_string().contains("secret"));
        assert_eq!(
            failure.get("TOKEN").map(String::as_str),
            Some("!printf secret; printf secret >&2; exit 1")
        );
        let mut slow = BTreeMap::from([("TOKEN".into(), "!sleep 30".into())]);
        let error = resolve_environment_with_timeout(
            &mut slow,
            directory.path(),
            std::time::Duration::from_millis(100),
        )
        .await
        .expect_err("must time out");
        assert!(error.to_string().contains("timed out"));
        for (command, expected) in [
            ("!yes x", "too much output"),
            ("!sleep 30 & exit 1", "failed"),
        ] {
            let mut values = BTreeMap::from([("TOKEN".into(), command.into())]);
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(3),
                resolve_environment(&mut values, directory.path()),
            )
            .await?;
            let error = result.expect_err("command must fail");
            assert!(error.to_string().contains(expected), "{error}");
        }
        Ok(())
    }

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
