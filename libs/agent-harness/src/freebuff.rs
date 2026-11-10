use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::{HarnessError, HarnessResult, settings::AgentSettings};

const MAX_METADATA_BYTES: u64 = 16 * 1024;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct FreebuffRuntimeMetadata {
    pub version: String,
    pub target: String,
}

pub fn read_runtime_metadata(directory: &Path) -> HarnessResult<Option<FreebuffRuntimeMetadata>> {
    let path = directory.join("freebuff-metadata.json");
    let file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(HarnessError::Protocol(format!(
                "could not open Freebuff runtime metadata at {}: {error}",
                path.display()
            )));
        }
    };
    let mut bytes = Vec::new();
    file.take(MAX_METADATA_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            HarnessError::Protocol(format!(
                "could not read Freebuff runtime metadata at {}: {error}",
                path.display()
            ))
        })?;
    if bytes.len() as u64 > MAX_METADATA_BYTES {
        return Err(HarnessError::Protocol(format!(
            "Freebuff runtime metadata at {} exceeds {MAX_METADATA_BYTES} bytes",
            path.display()
        )));
    }
    let metadata: FreebuffRuntimeMetadata = serde_json::from_slice(&bytes).map_err(|error| {
        HarnessError::Protocol(format!(
            "invalid Freebuff runtime metadata at {}: {error}",
            path.display()
        ))
    })?;
    if metadata.version.trim().is_empty() || metadata.target.trim().is_empty() {
        return Err(HarnessError::Protocol(format!(
            "Freebuff runtime metadata at {} has an empty version or target",
            path.display()
        )));
    }
    Ok(Some(metadata))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FreebuffInstallSource {
    Configured,
    Path,
}

#[derive(Debug, PartialEq, Eq)]
pub struct FreebuffInstall {
    pub executable: PathBuf,
    pub source: FreebuffInstallSource,
}

#[derive(Debug, PartialEq, Eq)]
pub struct FreebuffAttempt {
    pub executable: PathBuf,
    pub source: FreebuffInstallSource,
    pub reason: String,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct FreebuffDiscovery {
    pub install: Option<FreebuffInstall>,
    pub attempts: Vec<FreebuffAttempt>,
}

impl FreebuffDiscovery {
    pub fn summary(&self) -> String {
        if let Some(install) = &self.install {
            return format!(
                "Freebuff executable found at {}",
                install.executable.display()
            );
        }
        if self.attempts.is_empty() {
            return "Freebuff executable not found; configure its path or add it to PATH".into();
        }
        let attempts = self
            .attempts
            .iter()
            .map(|attempt| format!("{} ({})", attempt.executable.display(), attempt.reason))
            .collect::<Vec<_>>()
            .join(", ");
        format!("Freebuff executable not found; tried {attempts}")
    }
}

pub fn discover(configured: Option<&Path>, path: &OsStr) -> FreebuffDiscovery {
    let mut discovery = FreebuffDiscovery::default();
    if let Some(executable) = configured {
        if !executable.is_absolute() {
            discovery.attempts.push(FreebuffAttempt {
                executable: executable.to_path_buf(),
                source: FreebuffInstallSource::Configured,
                reason: "configured executable must be an absolute path".into(),
            });
        } else if inspect(
            executable,
            FreebuffInstallSource::Configured,
            &mut discovery,
        ) {
            return discovery;
        }
    }
    for directory in std::env::split_paths(path) {
        if directory.as_os_str().is_empty() {
            continue;
        }
        let executable = directory.join(if cfg!(windows) {
            "freebuff.exe"
        } else {
            "freebuff"
        });
        let executable = if executable.is_absolute() {
            executable
        } else {
            match std::env::current_dir() {
                Ok(current_directory) => current_directory.join(executable),
                Err(error) => {
                    discovery.attempts.push(FreebuffAttempt {
                        executable,
                        source: FreebuffInstallSource::Path,
                        reason: format!("could not resolve relative PATH entry: {error}"),
                    });
                    continue;
                }
            }
        };
        if inspect(&executable, FreebuffInstallSource::Path, &mut discovery) {
            return discovery;
        }
    }
    discovery
}

pub fn discover_from_settings(
    settings: &AgentSettings,
    path: &OsStr,
) -> HarnessResult<FreebuffDiscovery> {
    let executable = settings.executable("agents.freebuff.executable")?;
    Ok(discover(executable.as_deref(), path))
}

fn inspect(path: &Path, source: FreebuffInstallSource, discovery: &mut FreebuffDiscovery) -> bool {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                && source == FreebuffInstallSource::Path =>
        {
            return false;
        }
        Err(error) => {
            discovery.attempts.push(FreebuffAttempt {
                executable: path.to_path_buf(),
                source,
                reason: format!("could not inspect executable: {error}"),
            });
            return false;
        }
    };
    let reason = if !metadata.is_file() {
        Some("not a regular file")
    } else if !executable_permissions(&metadata) {
        Some("not executable")
    } else {
        None
    };
    if let Some(reason) = reason {
        discovery.attempts.push(FreebuffAttempt {
            executable: path.to_path_buf(),
            source,
            reason: reason.into(),
        });
        return false;
    }
    discovery.install = Some(FreebuffInstall {
        executable: path.to_path_buf(),
        source,
    });
    true
}

#[cfg(unix)]
fn executable_permissions(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn executable_permissions(_metadata: &std::fs::Metadata) -> bool {
    true
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn discovery_reads_the_configured_executable_from_agent_settings()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let executable = directory.path().join("custom-freebuff");
        std::fs::write(&executable, b"fixture")?;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))?;
        let settings_path = directory.path().join("settings.json");
        std::fs::write(
            &settings_path,
            serde_json::to_vec(&serde_json::json!({
                "app": { "agents.freebuff.executable": executable }
            }))?,
        )?;
        let settings = AgentSettings::read(&settings_path)?;
        let discovery = discover_from_settings(&settings, OsStr::new(""))?;
        assert_eq!(
            discovery.install.map(|install| install.executable),
            Some(executable)
        );
        Ok(())
    }

    #[test]
    fn configured_executable_precedes_path_without_running_it()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let configured = directory.path().join("configured");
        let on_path = directory.path().join("freebuff");
        for path in [&configured, &on_path] {
            std::fs::write(path, b"not a runnable program")?;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
        }
        let result = discover(Some(&configured), directory.path().as_os_str());
        assert_eq!(
            result.install,
            Some(FreebuffInstall {
                executable: configured,
                source: FreebuffInstallSource::Configured,
            })
        );
        assert!(result.attempts.is_empty());
        Ok(())
    }

    #[test]
    fn unusable_configured_and_path_candidates_are_reported_before_fallback()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let first = directory.path().join("first");
        let second = directory.path().join("second");
        std::fs::create_dir_all(&first)?;
        std::fs::create_dir_all(&second)?;
        std::fs::write(first.join("freebuff"), b"not executable")?;
        let executable = second.join("freebuff");
        std::fs::write(&executable, b"fixture")?;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))?;
        let path = std::env::join_paths([first, second])?;
        let result = discover(Some(&directory.path().join("missing")), &path);
        assert_eq!(
            result.install,
            Some(FreebuffInstall {
                executable,
                source: FreebuffInstallSource::Path,
            })
        );
        assert_eq!(result.attempts.len(), 2);
        assert!(
            result
                .attempts
                .iter()
                .any(|attempt| attempt.reason == "not executable")
        );
        Ok(())
    }

    #[test]
    fn missing_executable_has_actionable_diagnostics() {
        let result = discover(None, OsStr::new(""));
        assert!(result.install.is_none());
        assert!(result.summary().contains("configure its path"));
    }

    #[test]
    fn runtime_metadata_is_read_without_launching_freebuff()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        assert!(read_runtime_metadata(directory.path())?.is_none());
        std::fs::write(
            directory.path().join("freebuff-metadata.json"),
            br#"{"version":"0.2.36","target":"darwin-arm64"}"#,
        )?;
        assert_eq!(
            read_runtime_metadata(directory.path())?,
            Some(FreebuffRuntimeMetadata {
                version: "0.2.36".into(),
                target: "darwin-arm64".into(),
            })
        );
        Ok(())
    }

    #[test]
    fn malformed_or_oversized_runtime_metadata_is_rejected()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("freebuff-metadata.json");
        std::fs::write(&path, br#"{"version":"","target":"darwin-arm64"}"#)?;
        assert!(read_runtime_metadata(directory.path()).is_err());
        std::fs::write(&path, vec![b' '; MAX_METADATA_BYTES as usize + 1])?;
        assert!(read_runtime_metadata(directory.path()).is_err());
        Ok(())
    }
}
