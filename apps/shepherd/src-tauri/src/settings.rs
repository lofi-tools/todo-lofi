use std::io;
use std::path::{Path, PathBuf};

use agent_harness::settings::AgentSettings;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tauri::State;
use tokio::sync::Mutex;

use crate::bridge::{BridgeError, ShellState};
use crate::config_files::{backup_path, encode_json, read_optional, write_atomic};

pub const DEFAULT_SCROLLBACK: usize = 10_000;
pub const DEFAULT_ATTACHMENT_BYTES: usize = 50_000_000;
const MAX_SCROLLBACK: u64 = 100_000;

#[derive(Clone, Copy, Deserialize)]
pub enum SettingKey {
    #[serde(rename = "tasks.show_archived")]
    ShowArchived,
    #[serde(rename = "appearance.send_accent")]
    SendAccent,
    #[serde(rename = "notifications.enabled")]
    Notifications,
    #[serde(rename = "attachments.max_file_bytes")]
    AttachmentBytes,
    #[serde(rename = "terminal.scrollback_lines")]
    Scrollback,
}

impl SettingKey {
    const ALL: [Self; 5] = [
        Self::ShowArchived,
        Self::SendAccent,
        Self::Notifications,
        Self::AttachmentBytes,
        Self::Scrollback,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::ShowArchived => "tasks.show_archived",
            Self::SendAccent => "appearance.send_accent",
            Self::Notifications => "notifications.enabled",
            Self::AttachmentBytes => "attachments.max_file_bytes",
            Self::Scrollback => "terminal.scrollback_lines",
        }
    }

    fn default_value(self) -> Value {
        match self {
            Self::ShowArchived | Self::Notifications => json!(false),
            Self::SendAccent => json!("Blue"),
            Self::AttachmentBytes => json!(DEFAULT_ATTACHMENT_BYTES),
            Self::Scrollback => json!(DEFAULT_SCROLLBACK),
        }
    }

    fn project_scoped(self) -> bool {
        matches!(
            self,
            Self::ShowArchived | Self::AttachmentBytes | Self::Scrollback
        )
    }

    fn validate(self, value: &Value) -> Result<(), String> {
        let reason = match self {
            Self::ShowArchived | Self::Notifications if !value.is_boolean() => {
                "Enter true or false."
            }
            Self::SendAccent
                if !matches!(
                    value.as_str(),
                    Some("Blue" | "Violet" | "Teal" | "Amber" | "Neutral")
                ) =>
            {
                "Choose a supported accent."
            }
            Self::AttachmentBytes
                if !value.as_u64().is_some_and(|bytes| {
                    (1..=9_007_199_254_740_991).contains(&bytes) && usize::try_from(bytes).is_ok()
                }) =>
            {
                "Enter a positive whole number of bytes within the supported integer range."
            }
            Self::Scrollback if !value.as_u64().is_some_and(|lines| lines <= MAX_SCROLLBACK) => {
                "Enter a whole number from 0 to 100,000."
            }
            _ => return Ok(()),
        };
        Err(format!("Invalid {}. {reason}", self.name()))
    }
}

#[derive(Default)]
pub struct Settings {
    path: Option<PathBuf>,
    cached: Mutex<CachedSettings>,
}

#[derive(Default)]
struct CachedSettings {
    document: Value,
    warning: Option<String>,
}

#[derive(Serialize)]
pub struct SettingsSnapshot {
    app: Map<String, Value>,
    projects: Map<String, Value>,
    defaults: Map<String, Value>,
    warning: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsEdit {
    project_id: Option<String>,
    key: SettingKey,
    previous: Option<Value>,
    value: Option<Value>,
}

impl CachedSettings {
    fn snapshot(&self) -> SettingsSnapshot {
        fn preferences(scope: &Value, project: bool) -> Map<String, Value> {
            SettingKey::ALL
                .into_iter()
                .filter(|key| !project || key.project_scoped())
                .filter_map(|key| {
                    let value = scope.get(key.name()).filter(|value| !value.is_null())?;
                    Some((key.name().to_owned(), value.clone()))
                })
                .collect()
        }
        let projects = self.document["projects"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(path, scope)| {
                let values = preferences(scope, true);
                (!values.is_empty()).then(|| (path.clone(), Value::Object(values)))
            })
            .collect();
        SettingsSnapshot {
            app: preferences(&self.document["app"], false),
            projects,
            defaults: SettingKey::ALL
                .into_iter()
                .map(|key| (key.name().to_owned(), key.default_value()))
                .collect(),
            warning: self.warning.clone(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum DocumentError {
    #[error("This settings version is newer than Shepherd.")]
    FutureVersion,
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

fn parse_document(bytes: Option<&[u8]>) -> Result<(Value, bool), DocumentError> {
    let Some(bytes) = bytes else {
        return Ok((json!({"version": 1, "app": {}, "projects": {}}), false));
    };
    let mut document: Value = serde_json::from_slice(bytes)?;
    let shape = || {
        DocumentError::Invalid("Expected a settings object with app and projects objects.".into())
    };
    let root = document.as_object_mut().ok_or_else(shape)?;
    let version = match root.get("version") {
        None => 0,
        Some(version) => version.as_u64().ok_or_else(|| {
            DocumentError::Invalid("Expected an integer settings version.".into())
        })?,
    };
    if version > 1 {
        return Err(DocumentError::FutureVersion);
    }
    let app = root
        .entry("app")
        .or_insert_with(|| json!({}))
        .as_object()
        .ok_or_else(shape)?;
    validate_scope(app, false)?;
    let projects = root
        .entry("projects")
        .or_insert_with(|| json!({}))
        .as_object()
        .ok_or_else(shape)?;
    for (path, scope) in projects {
        if !Path::new(path).is_absolute() {
            return Err(DocumentError::Invalid(format!(
                "Expected an absolute project path for {path}."
            )));
        }
        validate_scope(scope.as_object().ok_or_else(shape)?, true)?;
    }
    root.insert("version".into(), json!(1));
    Ok((document, version == 0))
}

fn validate_scope(scope: &Map<String, Value>, project: bool) -> Result<(), DocumentError> {
    for key in SettingKey::ALL {
        if (!project || key.project_scoped())
            && let Some(value) = scope.get(key.name()).filter(|value| !value.is_null())
        {
            key.validate(value).map_err(DocumentError::Invalid)?;
        }
    }
    Ok(())
}

impl Settings {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path: Some(path),
            ..Self::default()
        }
    }

    fn message(path: &Path, cause: impl std::fmt::Display) -> String {
        format!("Could not use {}. {cause}", path.display())
    }

    async fn replace(
        path: &Path,
        original: Option<&[u8]>,
        document: &Value,
    ) -> Result<(), BridgeError> {
        let error = |error| BridgeError::new("settings", Self::message(path, error));
        let bytes = encode_json(document)
            .map_err(io::Error::from)
            .map_err(error)?;
        if let Some(original) = original {
            write_atomic(&backup_path(path), original).map_err(error)?;
        }
        if read_optional(path).await.map_err(error)?.as_deref() != original {
            return Err(BridgeError::new(
                "settings_conflict",
                Self::message(
                    path,
                    "The file changed while saving. Reload configuration before retrying.",
                ),
            ));
        }
        write_atomic(path, &bytes).map_err(error)
    }

    pub async fn reload(&self) -> SettingsSnapshot {
        let mut cached = self.cached.lock().await;
        if let Some(path) = &self.path {
            let loaded = async {
                let original = read_optional(path)
                    .await
                    .map_err(|error| Self::message(path, error))?;
                match parse_document(original.as_deref()) {
                    Ok((document, migrate)) => {
                        if migrate {
                            Self::replace(path, original.as_deref(), &document)
                                .await
                                .map_err(|error| error.to_string())?;
                        }
                        Ok(document)
                    }
                    Err(error) => {
                        let mut message = Self::message(path, &error);
                        if !matches!(error, DocumentError::FutureVersion)
                            && let Some(bytes) = original
                        {
                            let backup = backup_path(path);
                            match write_atomic(&backup, &bytes) {
                                Ok(()) => message.push_str(&format!(
                                    " A copy was saved to {}.",
                                    backup.display()
                                )),
                                Err(error) => {
                                    message.push_str(&format!(" Could not save a backup. {error}"))
                                }
                            }
                        }
                        Err(message)
                    }
                }
            }
            .await;
            *cached = match loaded {
                Ok(document) => CachedSettings {
                    document,
                    warning: None,
                },
                Err(warning) => CachedSettings {
                    document: Value::Null,
                    warning: Some(warning),
                },
            };
        }
        cached.snapshot()
    }

    pub async fn agent_settings(&self) -> AgentSettings {
        let cached = self.cached.lock().await;
        if cached.document.is_null() {
            return AgentSettings::default();
        }
        match serde_json::from_value(cached.document.clone()) {
            Ok(settings) => settings,
            Err(error) => {
                tracing::warn!(%error, "Could not load agent settings");
                AgentSettings::default()
            }
        }
    }

    pub async fn value(&self, project: Option<&Path>, key: SettingKey) -> Value {
        let cached = self.cached.lock().await;
        project
            .filter(|_| key.project_scoped())
            .and_then(Path::to_str)
            .and_then(|path| cached.document["projects"][path].get(key.name()))
            .filter(|value| !value.is_null())
            .or_else(|| {
                cached.document["app"]
                    .get(key.name())
                    .filter(|value| !value.is_null())
            })
            .cloned()
            .unwrap_or_else(|| key.default_value())
    }

    async fn edit(
        &self,
        project: Option<&Path>,
        edit: SettingsEdit,
    ) -> Result<SettingsSnapshot, BridgeError> {
        let path = self
            .path
            .as_deref()
            .ok_or_else(|| BridgeError::new("settings", "No settings file is configured."))?;
        let mut cached = self.cached.lock().await;
        let original = read_optional(path)
            .await
            .map_err(|error| BridgeError::new("settings", Self::message(path, error)))?;
        let (mut document, migrate) = parse_document(original.as_deref())
            .map_err(|error| BridgeError::new("settings", Self::message(path, error)))?;
        let project = project
            .map(|project| {
                if !edit.key.project_scoped() {
                    return Err(BridgeError::new(
                        "settings",
                        "This setting only applies to the app.",
                    ));
                }
                project.to_str().ok_or_else(|| {
                    BridgeError::new("settings", "Project settings require a UTF-8 path.")
                })
            })
            .transpose()?;
        if let Some(value) = &edit.value {
            edit.key
                .validate(value)
                .map_err(|error| BridgeError::new("settings", error))?;
        }
        let name = edit.key.name();
        let scope = match project {
            Some(project) => &document["projects"][project],
            None => &document["app"],
        };
        let current = scope.get(name).filter(|value| !value.is_null());
        if current != edit.value.as_ref() {
            if current != edit.previous.as_ref() {
                return Err(BridgeError::new(
                    "settings_conflict",
                    Self::message(
                        path,
                        format!("{name} changed on disk. Reload configuration before retrying."),
                    ),
                ));
            }
            let scope = match project {
                Some(project) => &mut document["projects"][project],
                None => &mut document["app"],
            };
            if let Some(value) = edit.value {
                scope[name] = value;
            } else if let Some(scope) = scope.as_object_mut() {
                scope.remove(name);
            }
            Self::replace(path, original.as_deref(), &document).await?;
        } else if migrate {
            Self::replace(path, original.as_deref(), &document).await?;
        }
        *cached = CachedSettings {
            document,
            warning: None,
        };
        Ok(cached.snapshot())
    }
}

#[tauri::command]
pub async fn reload_settings(
    state: State<'_, ShellState>,
) -> Result<SettingsSnapshot, BridgeError> {
    Ok(state.preferences.reload().await)
}

#[tauri::command]
pub async fn set_setting(
    state: State<'_, ShellState>,
    edit: SettingsEdit,
) -> Result<SettingsSnapshot, BridgeError> {
    let project = match &edit.project_id {
        Some(id) => Some(state.settings_project_path(id).await?),
        None => None,
    };
    #[cfg(target_os = "macos")]
    if matches!(edit.key, SettingKey::Notifications)
        && edit.value.as_ref().and_then(Value::as_bool) == Some(true)
        && project.is_none()
    {
        crate::attention::authorize_notifications().await?;
    }
    state.preferences.edit(project.as_deref(), edit).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(key: SettingKey, previous: Option<Value>, value: Option<Value>) -> SettingsEdit {
        SettingsEdit {
            project_id: None,
            key,
            previous,
            value,
        }
    }

    #[tokio::test]
    async fn migrates_legacy_settings_and_resolves_project_overrides_per_key()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("settings.json");
        let old = json!({
            "app": {"tasks.show_archived": true, "attachments.max_file_bytes": 30, "agents.opencode.executable": "/existing/bin/opencode"},
            "projects": {"/one": {"attachments.max_file_bytes": 10, "future": ["preserve"]}, "/two": {"terminal.scrollback_lines": 20}},
            "future": {"preserve": true}
        });
        let original = encode_json(&old)?;
        write_atomic(&path, &original)?;
        let settings = Settings::new(path.clone());
        let snapshot = settings.reload().await;
        assert!(snapshot.warning.is_none());
        assert!(snapshot.app.get("agents.opencode.executable").is_none());
        assert_eq!(
            settings
                .agent_settings()
                .await
                .executable("agents.opencode.executable")?,
            Some("/existing/bin/opencode".into())
        );
        assert_eq!(
            settings
                .value(Some(Path::new("/one")), SettingKey::AttachmentBytes)
                .await,
            json!(10)
        );
        assert_eq!(
            settings
                .value(Some(Path::new("/one")), SettingKey::ShowArchived)
                .await,
            json!(true)
        );
        assert_eq!(
            settings
                .value(Some(Path::new("/one")), SettingKey::Scrollback)
                .await,
            json!(DEFAULT_SCROLLBACK)
        );
        assert_eq!(
            settings
                .value(Some(Path::new("/two")), SettingKey::AttachmentBytes)
                .await,
            json!(30)
        );
        assert_eq!(
            settings
                .value(Some(Path::new("/two")), SettingKey::Scrollback)
                .await,
            json!(20)
        );
        assert_eq!(std::fs::read(backup_path(&path))?, original);
        let migrated: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
        assert_eq!(migrated["version"], 1);
        assert_eq!(migrated["future"], old["future"]);
        assert_eq!(
            migrated["projects"]["/one"]["future"],
            old["projects"]["/one"]["future"]
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path)?.permissions().mode() & 0o077, 0);
        }
        Ok(())
    }

    #[tokio::test]
    async fn settings_edits_replay_unrelated_changes_and_keep_same_path_conflicts()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("settings.json");
        let settings = Settings::new(path.clone());
        let project = Some(Path::new("/project"));
        assert!(settings.reload().await.warning.is_none());
        assert_eq!(settings.value(None, SettingKey::Notifications).await, false);
        let external = json!({
            "version": 1, "app": {"attachments.max_file_bytes": 100, "agents.opencode.executable": "/existing/bin/opencode"}
        });
        write_atomic(&path, &encode_json(&external)?)?;
        settings
            .edit(
                project,
                edit(SettingKey::AttachmentBytes, None, Some(json!(20))),
            )
            .await?;
        assert_eq!(
            settings.value(project, SettingKey::AttachmentBytes).await,
            20
        );
        assert_eq!(settings.value(None, SettingKey::AttachmentBytes).await, 100);
        let mut external: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
        assert_eq!(
            external["app"]["agents.opencode.executable"],
            "/existing/bin/opencode"
        );
        external["projects"]["/project"]["attachments.max_file_bytes"] = json!(40);
        let updated = encode_json(&external)?;
        write_atomic(&path, &updated)?;
        let result = settings
            .edit(
                project,
                edit(
                    SettingKey::AttachmentBytes,
                    Some(json!(20)),
                    Some(json!(10)),
                ),
            )
            .await;
        assert!(
            result
                .err()
                .is_some_and(|error| error.to_string().contains("changed on disk"))
        );
        assert_eq!(std::fs::read(&path)?, updated);
        assert_eq!(
            settings.value(project, SettingKey::AttachmentBytes).await,
            20
        );
        settings.reload().await;
        settings
            .edit(
                project,
                edit(SettingKey::AttachmentBytes, Some(json!(40)), None),
            )
            .await?;
        assert_eq!(
            settings.value(project, SettingKey::AttachmentBytes).await,
            100
        );
        assert!(
            settings
                .edit(
                    None,
                    edit(SettingKey::Scrollback, None, Some(json!(100_001)))
                )
                .await
                .is_err()
        );
        assert!(
            settings
                .edit(
                    project,
                    edit(SettingKey::Notifications, None, Some(json!(true)))
                )
                .await
                .is_err()
        );
        settings
            .edit(
                None,
                edit(SettingKey::Notifications, None, Some(json!(true))),
            )
            .await?;
        assert_eq!(
            settings.value(project, SettingKey::Notifications).await,
            true
        );
        Ok(())
    }

    #[tokio::test]
    async fn malformed_settings_keep_defaults_and_backup_the_exact_bytes()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("settings.json");
        let settings = Settings::new(path.clone());
        let invalid = b"{broken";
        write_atomic(&path, invalid)?;
        let snapshot = settings.reload().await;
        assert!(
            snapshot
                .warning
                .is_some_and(|warning| warning.contains("settings.json.bak"))
        );
        assert_eq!(snapshot.defaults["appearance.send_accent"], "Blue");
        assert_eq!(
            settings.value(None, SettingKey::AttachmentBytes).await,
            DEFAULT_ATTACHMENT_BYTES as u64
        );
        assert_eq!(std::fs::read(backup_path(&path))?, invalid);
        assert_eq!(std::fs::read(&path)?, invalid);

        let newer = encode_json(&json!({"version": 2, "app": {"appearance.send_accent": "Teal"}}))?;
        write_atomic(&path, &newer)?;
        assert!(
            settings
                .reload()
                .await
                .warning
                .is_some_and(|warning| warning.contains("newer"))
        );
        assert!(
            settings
                .edit(
                    None,
                    edit(SettingKey::SendAccent, None, Some(json!("Amber")))
                )
                .await
                .is_err()
        );
        assert_eq!(std::fs::read(&path)?, newer);
        assert_eq!(std::fs::read(backup_path(&path))?, invalid);
        Ok(())
    }
}
