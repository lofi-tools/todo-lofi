use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use shep_core::input::{
    CommandRegistry, KeyPlatform, command_registry as defaults, command_registry_with_overrides,
    normalize_binding,
};
use tauri::State;
use tokio::sync::Mutex;

use crate::bridge::BridgeError;
use crate::config_files::{backup_path, encode_json, read_optional, write_atomic};

pub struct KeybindingSettings {
    path: PathBuf,
    edits: Mutex<()>,
    legacy_path: Option<PathBuf>,
}

#[derive(Serialize)]
pub struct KeybindingSnapshot {
    #[serde(flatten)]
    registry: CommandRegistry,
    warning: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeybindingEdit {
    command: String,
    previous: Option<String>,
    chord: Option<String>,
}

#[derive(Debug, thiserror::Error)]
enum DocumentError {
    #[error("Expected a version 1 object with a bindings object.")]
    Shape,
    #[error("This settings version is newer than Shepherd.")]
    FutureVersion,
    #[error("The binding for {0} must be a shortcut string.")]
    Value(String),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Binding(#[from] shep_core::input::KeybindingError),
}

fn parse_document(
    platform: KeyPlatform,
    bytes: Option<&[u8]>,
) -> Result<(Value, CommandRegistry), DocumentError> {
    let document: Value = match bytes {
        Some(bytes) => serde_json::from_slice(bytes)?,
        None => json!({"version": 1, "bindings": {}}),
    };
    let version = document.get("version").and_then(Value::as_u64);
    if version.is_some_and(|version| version > 1) {
        return Err(DocumentError::FutureVersion);
    }
    let bindings = document
        .get("bindings")
        .and_then(Value::as_object)
        .filter(|_| version == Some(1))
        .ok_or(DocumentError::Shape)?;
    let overrides = defaults(platform)?
        .commands
        .into_iter()
        .filter_map(|command| {
            bindings.get(&command.id).map(|binding| {
                let chord = binding
                    .as_str()
                    .ok_or(DocumentError::Value(command.label))?;
                Ok((command.id, chord.to_owned()))
            })
        })
        .collect::<Result<BTreeMap<_, _>, DocumentError>>()?;
    let registry = command_registry_with_overrides(platform, &overrides)?;
    Ok((document, registry))
}

impl KeybindingSettings {
    pub fn new(path: PathBuf) -> Self {
        let legacy_path = (path.file_name().is_some_and(|name| name == "keymap.json"))
            .then(|| path.with_file_name("keybindings.json"));
        Self {
            path,
            edits: Mutex::new(()),
            legacy_path,
        }
    }

    fn message(&self, cause: impl std::fmt::Display) -> String {
        format!("Could not use {}. {cause}", self.path.display())
    }

    fn error(&self, cause: impl std::fmt::Display) -> BridgeError {
        BridgeError::new("keybindings", self.message(cause))
    }

    async fn read_document(&self) -> Result<(Option<Vec<u8>>, bool), BridgeError> {
        let current = read_optional(&self.path)
            .await
            .map_err(|error| self.error(error))?;
        if current.is_some() {
            return Ok((current, false));
        }
        if let Some(path) = &self.legacy_path {
            let legacy = read_optional(path)
                .await
                .map_err(|error| self.error(error))?;
            return Ok((legacy, true));
        }
        Ok((None, false))
    }

    async fn load(&self, platform: KeyPlatform) -> Result<KeybindingSnapshot, BridgeError> {
        let _edit = self.edits.lock().await;
        let warning = match self.read_document().await {
            Ok((bytes, _)) => match parse_document(platform, bytes.as_deref()) {
                Ok((_, registry)) => {
                    return Ok(KeybindingSnapshot {
                        registry,
                        warning: None,
                    });
                }
                Err(error) => {
                    let message = self.message(&error);
                    if let Some(bytes) = bytes
                        && !matches!(error, DocumentError::FutureVersion)
                    {
                        let backup = backup_path(&self.path);
                        match write_atomic(&backup, &bytes) {
                            Ok(()) => {
                                format!("{message} A copy was saved to {}.", backup.display())
                            }
                            Err(error) => format!("{message} Could not save a backup. {error}"),
                        }
                    } else {
                        message
                    }
                }
            },
            Err(error) => error.to_string(),
        };
        Ok(KeybindingSnapshot {
            registry: defaults(platform).map_err(|error| self.error(error))?,
            warning: Some(warning),
        })
    }

    async fn edit(
        &self,
        platform: KeyPlatform,
        edit: KeybindingEdit,
    ) -> Result<KeybindingSnapshot, BridgeError> {
        let _edit = self.edits.lock().await;
        let (original, from_legacy) = self.read_document().await?;
        let (mut document, registry) =
            parse_document(platform, original.as_deref()).map_err(|error| self.error(error))?;
        let binding = registry
            .commands
            .iter()
            .find(|binding| binding.id == edit.command)
            .ok_or_else(|| self.error("Unknown command."))?;
        let current = binding.chord.as_deref().filter(|_| binding.overridden);
        let next = edit
            .chord
            .as_deref()
            .map(|chord| normalize_binding(platform, chord))
            .transpose()
            .map_err(|error| self.error(error))?;
        if current == next.as_deref() {
            return Ok(KeybindingSnapshot {
                registry,
                warning: None,
            });
        }
        if current != edit.previous.as_deref() {
            return Err(self.error(format!(
                "The shortcut for {} changed on disk. Reload configuration before saving this edit.",
                binding.label,
            )));
        }
        let bindings = document
            .get_mut("bindings")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| self.error(DocumentError::Shape))?;
        if let Some(chord) = next {
            bindings.insert(edit.command, Value::String(chord));
        } else {
            bindings.remove(&edit.command);
        }
        let bytes = encode_json(&document).map_err(|error| self.error(error))?;
        let (_, registry) =
            parse_document(platform, Some(&bytes)).map_err(|error| self.error(error))?;

        if !from_legacy {
            if let Some(original) = &original {
                write_atomic(&backup_path(&self.path), original)
                    .map_err(|error| self.error(error))?;
            }
        }
        let (current, current_from_legacy) = self.read_document().await?;
        if current != original || current_from_legacy != from_legacy {
            return Err(self.error("The file changed while saving. Retry the edit."));
        }
        write_atomic(&self.path, &bytes).map_err(|error| self.error(error))?;
        Ok(KeybindingSnapshot {
            registry,
            warning: None,
        })
    }
}

#[tauri::command]
pub async fn command_registry(
    settings: State<'_, KeybindingSettings>,
    platform: KeyPlatform,
) -> Result<KeybindingSnapshot, BridgeError> {
    settings.load(platform).await
}

#[tauri::command]
pub async fn set_keybinding(
    settings: State<'_, KeybindingSettings>,
    platform: KeyPlatform,
    edit: KeybindingEdit,
) -> Result<KeybindingSnapshot, BridgeError> {
    settings.edit(platform, edit).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(command: &str, previous: Option<&str>, chord: Option<&str>) -> KeybindingEdit {
        KeybindingEdit {
            command: command.into(),
            previous: previous.map(str::to_owned),
            chord: chord.map(str::to_owned),
        }
    }

    #[tokio::test]
    async fn legacy_keybindings_are_read_and_first_edit_creates_keymap()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let old_path = directory.path().join("keybindings.json");
        let new_path = directory.path().join("keymap.json");
        let legacy = json!({"version": 1, "bindings": {"commandPalette": "F9"}});
        write_atomic(&old_path, &encode_json(&legacy)?)?;
        let settings = KeybindingSettings::new(new_path.clone());
        let loaded = settings.load(KeyPlatform::Linux).await?;
        assert!(loaded.registry.commands.iter().any(
            |command| command.id == "commandPalette" && command.chord.as_deref() == Some("F9")
        ));
        settings
            .edit(
                KeyPlatform::Linux,
                edit("commandPalette", Some("F9"), Some("F10")),
            )
            .await?;
        let saved: Value = serde_json::from_slice(&std::fs::read(new_path)?)?;
        assert_eq!(saved["bindings"]["commandPalette"], "F10");
        assert_eq!(std::fs::read(old_path)?, encode_json(&legacy)?);
        Ok(())
    }

    #[tokio::test]
    async fn edits_preserve_other_changes_reject_conflicts_and_restore_defaults()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("keybindings.json");
        let settings = KeybindingSettings::new(path.clone());
        let platform = KeyPlatform::Linux;
        assert!(settings.load(platform).await?.warning.is_none());
        assert!(!path.exists());

        let external = json!({
            "version": 1,
            "extra": {"preserve": true},
            "bindings": {"switchProject": "F8", "futureCommand": {"preserve": true}},
        });
        write_atomic(&path, &encode_json(&external)?)?;
        settings
            .edit(platform, edit("commandPalette", None, Some("F9")))
            .await?;
        let saved = std::fs::read(&path)?;
        let mut document: Value = serde_json::from_slice(&saved)?;
        assert_eq!(document["extra"], external["extra"]);
        assert_eq!(
            document["bindings"]["futureCommand"],
            external["bindings"]["futureCommand"]
        );
        assert_eq!(document["bindings"]["switchProject"], "F8");
        assert_eq!(document["bindings"]["commandPalette"], "F9");
        assert_eq!(std::fs::read(backup_path(&path))?, encode_json(&external)?);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path)?.permissions().mode() & 0o077, 0);
        }

        let conflicting = settings
            .edit(platform, edit("commandPalette", Some("F9"), Some("F8")))
            .await;
        assert!(
            conflicting
                .err()
                .is_some_and(|error| error.to_string().contains("Switch project"))
        );
        assert_eq!(std::fs::read(&path)?, saved);

        document["bindings"]["commandPalette"] = json!("F10");
        write_atomic(&path, &encode_json(&document)?)?;
        let stale = settings
            .edit(platform, edit("commandPalette", Some("F9"), Some("F11")))
            .await;
        assert!(
            stale
                .err()
                .is_some_and(|error| error.to_string().contains("changed on disk"))
        );
        assert_eq!(std::fs::read(&path)?, encode_json(&document)?);

        let reopened = KeybindingSettings::new(path.clone());
        let current = reopened.load(platform).await?;
        assert!(current.registry.commands.iter().any(
            |command| command.id == "commandPalette" && command.chord.as_deref() == Some("F10")
        ));
        let restored = reopened
            .edit(platform, edit("commandPalette", Some("F10"), None))
            .await?;
        assert!(
            restored
                .registry
                .commands
                .iter()
                .any(|command| command.id == "commandPalette" && !command.overridden)
        );
        assert_eq!(std::fs::read(backup_path(&path))?, encode_json(&document)?);
        document["bindings"]
            .as_object_mut()
            .ok_or("bindings must be an object")?
            .remove("commandPalette");
        assert_eq!(std::fs::read(&path)?, encode_json(&document)?);
        Ok(())
    }

    #[tokio::test]
    async fn malformed_and_newer_files_leave_defaults_available()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("keybindings.json");
        let settings = KeybindingSettings::new(path.clone());
        let platform = KeyPlatform::Mac;
        let corrupt = b"broken json";
        write_atomic(&path, corrupt)?;
        let snapshot = settings.load(platform).await?;
        assert!(
            snapshot
                .warning
                .is_some_and(|warning| warning.contains("keybindings.json.bak"))
        );
        assert!(
            snapshot
                .registry
                .commands
                .iter()
                .any(|command| command.id == "commandPalette"
                    && command.chord.as_deref() == Some("Meta+K"))
        );
        assert_eq!(std::fs::read(backup_path(&path))?, corrupt);
        assert_eq!(std::fs::read(&path)?, corrupt);

        let newer = encode_json(&json!({"version": 2, "bindings": {"commandPalette": "F10"}}))?;
        write_atomic(&path, &newer)?;
        let snapshot = settings.load(platform).await?;
        assert!(
            snapshot
                .warning
                .is_some_and(|warning| warning.contains("newer"))
        );
        assert!(
            settings
                .edit(platform, edit("commandPalette", None, Some("F9")))
                .await
                .is_err()
        );
        assert_eq!(std::fs::read(&path)?, newer);
        assert_eq!(std::fs::read(backup_path(&path))?, corrupt);
        Ok(())
    }
}
