use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectEnvironment {
    pub source_hash: String,
    pub setup: String,
    pub actions: Vec<ProjectAction>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ProjectAction {
    pub name: String,
    pub icon: String,
    pub command: String,
}

fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn read(project: &Path) -> Result<Option<ProjectEnvironment>, String> {
    let path = project.join(".codex/environments/environment.toml");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("Could not read {}: {error}", path.display())),
    };
    parse(&bytes)
        .map(Some)
        .map_err(|error| format!("{}: {error}", path.display()))
}

fn parse(bytes: &[u8]) -> Result<ProjectEnvironment, String> {
    let source = std::str::from_utf8(bytes).map_err(|error| error.to_string())?;
    let document: toml::Value =
        toml::from_str(source).map_err(|error| format!("Invalid TOML: {error}"))?;
    if document.get("version").and_then(toml::Value::as_integer) != Some(1) {
        return Err("Unsupported environment version; expected version 1.".into());
    }
    let table = document
        .as_table()
        .ok_or("Environment must be a TOML table.")?;
    if let Some(key) = table
        .keys()
        .find(|key| !matches!(key.as_str(), "version" | "name" | "setup" | "actions"))
    {
        return Err(format!("Unsupported environment field: {key}."));
    }
    let platform = if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "windows") {
        "win32"
    } else {
        "linux"
    };
    let setup = document.get("setup");
    if let Some(setup) = setup {
        let setup = setup.as_table().ok_or("Setup must be a table.")?;
        if let Some(key) = setup
            .keys()
            .find(|key| !matches!(key.as_str(), "script" | "darwin" | "linux" | "win32"))
        {
            return Err(format!("Unsupported setup field: {key}."));
        }
        for (key, value) in setup.iter().filter(|(key, _)| key.as_str() != "script") {
            let table = value
                .as_table()
                .ok_or_else(|| format!("Platform setup {key} must be a table."))?;
            if table.keys().any(|key| key != "script") {
                return Err("Unsupported platform setup field.".into());
            }
            if table.get("script").is_some_and(|script| !script.is_str()) {
                return Err(format!("Platform setup {key} script must be a string."));
            }
        }
    }
    let script = setup
        .and_then(|setup| setup.get(platform))
        .and_then(|setup| setup.get("script"))
        .or_else(|| setup.and_then(|setup| setup.get("script")))
        .map(|value| value.as_str().ok_or("Setup script must be a string."))
        .transpose()?
        .unwrap_or_default()
        .to_owned();
    let mut actions: Vec<(ProjectAction, bool)> = Vec::new();
    if let Some(value) = document.get("actions") {
        for action in value.as_array().ok_or("Actions must be an array.")? {
            let table = action.as_table().ok_or("Action must be a table.")?;
            if let Some(key) = table
                .keys()
                .find(|key| !matches!(key.as_str(), "name" | "icon" | "command" | "platform"))
            {
                return Err(format!("Unsupported action field: {key}."));
            }
            let action_platform = action
                .get("platform")
                .map(|value| value.as_str().ok_or("Action platform must be a string."))
                .transpose()?;
            if action_platform.is_some_and(|value| value != platform) {
                continue;
            }
            let name = action
                .get("name")
                .and_then(toml::Value::as_str)
                .ok_or("Action name must be a string.")?;
            let command = action
                .get("command")
                .and_then(toml::Value::as_str)
                .ok_or("Action command must be a string.")?;
            let icon = action
                .get("icon")
                .and_then(toml::Value::as_str)
                .unwrap_or("run");
            let candidate = ProjectAction {
                name: name.to_owned(),
                icon: icon.to_owned(),
                command: command.to_owned(),
            };
            let specific = action_platform.is_some();
            if let Some((existing, existing_specific)) = actions
                .iter_mut()
                .find(|(existing, _)| existing.name == name)
            {
                if specific || !*existing_specific {
                    *existing = candidate;
                    *existing_specific = specific;
                }
            } else {
                actions.push((candidate, specific));
            }
        }
    }
    Ok(ProjectEnvironment {
        source_hash: hash(bytes),
        setup: script,
        actions: actions.into_iter().map(|(action, _)| action).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_setup_and_actions_and_hashes_the_source() -> Result<(), String> {
        let source = b"version = 1\n[setup]\nscript = 'install'\n[[actions]]\nname = 'Run'\nicon = 'run'\ncommand = 'serve'\n";
        let environment = parse(source)?;
        assert_eq!(environment.setup, "install");
        assert_eq!(
            environment.actions.first().ok_or("missing action")?.command,
            "serve"
        );
        assert_eq!(environment.source_hash, hash(source));
        assert!(parse(b"version = 2").is_err());
        assert!(parse(b"version = 1\n[cleanup]\nscript = 'unsupported'").is_err());
        Ok(())
    }

    #[test]
    fn platform_specific_action_overrides_the_default() -> Result<(), String> {
        let platform = if cfg!(target_os = "macos") {
            "darwin"
        } else if cfg!(target_os = "windows") {
            "win32"
        } else {
            "linux"
        };
        let source = format!(
            "version = 1\n[[actions]]\nname = 'Run'\ncommand = 'default'\n[[actions]]\nname = 'Run'\nplatform = '{platform}'\ncommand = 'platform'\n"
        );
        let environment = parse(source.as_bytes())?;
        assert_eq!(environment.actions.len(), 1);
        assert_eq!(
            environment.actions.first().ok_or("missing action")?.command,
            "platform"
        );
        Ok(())
    }
}
