use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use agent_harness::settings::AgentSettings;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tauri::State;
use tokio::sync::Mutex;

use crate::bridge::{BridgeError, ShellState};
use crate::config_files::{backup_path, encode_json, read_optional, write_atomic};

pub use shep_terminal_renderer::DEFAULT_SCROLLBACK;
pub const DEFAULT_ATTACHMENT_BYTES: usize = 50_000_000;
const MAX_SCROLLBACK: u64 = 100_000;

#[derive(Clone, Copy, Deserialize)]
pub enum SettingKey {
    #[serde(rename = "agents.priority")]
    AgentPriority,
    #[serde(rename = "theme")]
    Theme,
    #[serde(rename = "appearance.density")]
    Density,
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
    #[serde(rename = "files.show_dotfiles")]
    ShowDotfiles,
}

impl SettingKey {
    const ALL: [Self; 9] = [
        Self::AgentPriority,
        Self::Theme,
        Self::Density,
        Self::ShowArchived,
        Self::SendAccent,
        Self::Notifications,
        Self::AttachmentBytes,
        Self::Scrollback,
        Self::ShowDotfiles,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::AgentPriority => "agents.priority",
            Self::Theme => "theme",
            Self::Density => "appearance.density",
            Self::ShowArchived => "tasks.show_archived",
            Self::SendAccent => "appearance.send_accent",
            Self::Notifications => "notifications.enabled",
            Self::AttachmentBytes => "attachments.max_file_bytes",
            Self::Scrollback => "terminal.scrollback_lines",
            Self::ShowDotfiles => "files.show_dotfiles",
        }
    }

    fn default_value(self) -> Value {
        match self {
            Self::AgentPriority => json!(["codex", "opencode", "pi", "claude", "freebuff"]),
            Self::Theme => json!("system"),
            Self::Density => json!("compact"),
            Self::ShowArchived | Self::Notifications => json!(false),
            Self::ShowDotfiles => json!(true),
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
            Self::AgentPriority
                if !value.as_array().is_some_and(|agents| {
                    let mut seen = std::collections::BTreeSet::new();
                    agents.iter().all(|agent| {
                        agent.as_str().is_some_and(|name| {
                            matches!(name, "codex" | "opencode" | "pi" | "claude" | "freebuff")
                                && seen.insert(name)
                        })
                    })
                }) =>
            {
                "Enter a list of unique supported agents."
            }
            Self::Theme if !matches!(value.as_str(), Some("system" | "light" | "dark")) => {
                "Choose system, light, or dark."
            }
            Self::Density if !matches!(value.as_str(), Some("compact" | "comfortable")) => {
                "Choose compact or comfortable."
            }
            Self::ShowArchived | Self::Notifications | Self::ShowDotfiles
                if !value.is_boolean() =>
            {
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

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PoolConfiguration {
    pub slots: Vec<ConfiguredPoolSlot>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConfiguredPoolSlot {
    pub id: shep_core::store::PoolSlotId,
    pub agent: shep_core::Agent,
    pub provider_id: String,
    pub provider_name: String,
    pub model_id: String,
    pub model_name: String,
    pub credential_ref: Option<String>,
    pub priority: i64,
    pub enabled: bool,
    #[serde(default)]
    pub manual_limits: Vec<ConfiguredPoolLimit>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConfiguredPoolLimit {
    pub window_key: Option<String>,
    pub limit: Option<u64>,
    pub limit_unit: Option<String>,
    pub reset_at: Option<u64>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl ConfiguredPoolSlot {
    pub fn from_slot(slot: &shep_core::store::PoolSlot) -> Self {
        Self {
            id: slot.id.clone(),
            agent: slot.agent,
            provider_id: slot.provider_id.clone(),
            provider_name: slot.provider_name.clone(),
            model_id: slot.model_id.clone(),
            model_name: slot.model_name.clone(),
            credential_ref: slot.credential_ref.clone(),
            priority: slot.priority,
            enabled: slot.enabled,
            manual_limits: Vec::new(),
            extra: Map::new(),
        }
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
    project_local: BTreeMap<String, Map<String, Value>>,
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
        let mut projects: Map<String, Value> = self.document["projects"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(path, scope)| {
                let values = preferences(scope, true);
                (!values.is_empty()).then(|| (path.clone(), Value::Object(values)))
            })
            .collect();
        for (path, local) in &self.project_local {
            let scope = projects.entry(path.clone()).or_insert_with(|| json!({}));
            if let Some(scope) = scope.as_object_mut() {
                scope.extend(local.clone());
            }
        }
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
                    project_local: BTreeMap::new(),
                },
                Err(warning) => CachedSettings {
                    document: Value::Null,
                    warning: Some(warning),
                    project_local: BTreeMap::new(),
                },
            };
        }
        cached.snapshot()
    }

    pub async fn load_project_preferences(&self, projects: &[PathBuf]) -> SettingsSnapshot {
        let mut local = BTreeMap::new();
        let mut warnings = Vec::new();
        for project in projects {
            let path = project.join(".config/shepherd/settings.json");
            let result = async {
                let Some(bytes) = read_optional(&path)
                    .await
                    .map_err(|error| error.to_string())?
                else {
                    return Ok(None);
                };
                let document: Value =
                    serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
                if document.get("version").and_then(Value::as_u64) != Some(1) {
                    return Err("Expected version 1.".to_string());
                }
                let scope = document
                    .get("settings")
                    .and_then(Value::as_object)
                    .ok_or_else(|| "Expected a settings object.".to_string())?;
                validate_scope(scope, true).map_err(|error| error.to_string())?;
                if let Some(value) = scope.get("preview.url").filter(|value| !value.is_null()) {
                    let raw = value
                        .as_str()
                        .ok_or_else(|| "preview.url must be a loopback HTTP(S) URL.".to_string())?;
                    if shep_core::projects::parse_preview_url(raw).is_none() {
                        return Err("preview.url must be a loopback HTTP(S) URL.".to_string());
                    }
                }
                let mut values: Map<String, Value> = SettingKey::ALL
                    .into_iter()
                    .filter(|key| key.project_scoped())
                    .filter_map(|key| {
                        scope
                            .get(key.name())
                            .filter(|value| !value.is_null())
                            .map(|value| (key.name().to_string(), value.clone()))
                    })
                    .collect();
                if let Some(value) = scope.get("preview.url").filter(|value| !value.is_null()) {
                    values.insert("preview.url".to_string(), value.clone());
                }
                Ok::<_, String>(Some(values))
            }
            .await;
            match result {
                Ok(Some(scope)) => {
                    local.insert(project.to_string_lossy().into_owned(), scope);
                }
                Ok(None) => {}
                Err(error) => warnings.push(Self::message(&path, error)),
            }
        }
        let mut cached = self.cached.lock().await;
        cached.project_local = local;
        if !warnings.is_empty() {
            let warning = warnings.join(" ");
            cached.warning = Some(match cached.warning.take() {
                Some(existing) => format!("{existing} {warning}"),
                None => warning,
            });
        }
        cached.snapshot()
    }

    pub async fn project_preview_overrides(&self) -> BTreeMap<String, String> {
        self.cached
            .lock()
            .await
            .project_local
            .iter()
            .filter_map(|(path, values)| {
                values
                    .get("preview.url")
                    .and_then(Value::as_str)
                    .map(|value| (path.clone(), value.to_string()))
            })
            .collect()
    }

    pub(crate) fn has_path(&self) -> bool {
        self.path.is_some()
    }

    pub(crate) async fn pool_configuration(
        &self,
    ) -> Result<Option<PoolConfiguration>, BridgeError> {
        if self.path.is_none() {
            return Ok(None);
        }
        let cached = self.cached.lock().await;
        if cached.document.is_null() {
            return Err(BridgeError::new(
                "settings",
                cached
                    .warning
                    .clone()
                    .unwrap_or_else(|| "Settings are unavailable.".into()),
            ));
        }
        cached.document["app"]
            .get("agent_pool")
            .filter(|value| !value.is_null())
            .map(|value| {
                serde_json::from_value(value.clone()).map_err(|error| {
                    BridgeError::new(
                        "settings",
                        format!("Invalid agent_pool configuration: {error}"),
                    )
                })
            })
            .transpose()
    }

    pub(crate) async fn write_pool_configuration(
        &self,
        config: &PoolConfiguration,
    ) -> Result<(), BridgeError> {
        let path = self
            .path
            .as_deref()
            .ok_or_else(|| BridgeError::new("settings", "No settings file is configured."))?;
        let mut cached = self.cached.lock().await;
        let original = read_optional(path)
            .await
            .map_err(|error| BridgeError::new("settings", Self::message(path, error)))?;
        let (mut document, _) = parse_document(original.as_deref())
            .map_err(|error| BridgeError::new("settings", Self::message(path, error)))?;
        if document["app"].get("agent_pool") != cached.document["app"].get("agent_pool") {
            return Err(BridgeError::new(
                "settings_conflict",
                "Agent pool configuration changed on disk. Reload settings before editing.",
            ));
        }
        document["app"]["agent_pool"] = serde_json::to_value(config)
            .map_err(|error| BridgeError::new("settings", error.to_string()))?;
        Self::replace(path, original.as_deref(), &document).await?;
        cached.document = document;
        Ok(())
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
            .and_then(|path| {
                cached
                    .project_local
                    .get(path)
                    .and_then(|scope| scope.get(key.name()))
                    .or_else(|| cached.document["projects"][path].get(key.name()))
            })
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
        if project.is_some_and(|project| {
            cached
                .project_local
                .get(project)
                .is_some_and(|scope| scope.contains_key(edit.key.name()))
        }) {
            return Err(BridgeError::new(
                "settings",
                "This value is set in the project's .config/shepherd/settings.json. Edit that file to change it.",
            ));
        }
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
        cached.document = document;
        cached.warning = None;
        Ok(cached.snapshot())
    }
}

#[tauri::command]
pub async fn reload_settings(
    state: State<'_, ShellState>,
) -> Result<SettingsSnapshot, BridgeError> {
    state.preferences.reload().await;
    let projects = state
        .project_list()
        .await
        .projects
        .into_iter()
        .filter(|project| !project.missing)
        .map(|project| PathBuf::from(project.path))
        .collect::<Vec<_>>();
    let snapshot = state.preferences.load_project_preferences(&projects).await;
    state.refresh_project_preview_overrides().await?;
    Ok(snapshot)
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
    async fn dotfiles_default_on_and_persist_globally() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("settings.json");
        let settings = Settings::new(path.clone());
        settings.reload().await;
        assert_eq!(
            settings.value(None, SettingKey::ShowDotfiles).await,
            json!(true)
        );
        settings
            .edit(
                None,
                edit(SettingKey::ShowDotfiles, None, Some(json!(false))),
            )
            .await?;
        let reopened = Settings::new(path);
        reopened.reload().await;
        assert_eq!(
            reopened.value(None, SettingKey::ShowDotfiles).await,
            json!(false)
        );
        assert_eq!(
            reopened
                .value(Some(Path::new("/other")), SettingKey::ShowDotfiles)
                .await,
            json!(false)
        );
        assert!(
            reopened
                .edit(
                    Some(Path::new("/other")),
                    edit(SettingKey::ShowDotfiles, None, Some(json!(true)))
                )
                .await
                .is_err()
        );
        Ok(())
    }

    #[tokio::test]
    async fn local_project_preferences_override_saved_values()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let project = directory.path().join("project");
        std::fs::create_dir_all(&project)?;
        let settings_path = directory.path().join("settings.json");
        let project_path = project.to_str().ok_or("project path")?;
        let global = json!({"version": 1, "app": {"terminal.scrollback_lines": 30}, "projects": {(project_path): {"terminal.scrollback_lines": 40}}});
        write_atomic(&settings_path, &encode_json(&global)?)?;
        let local_path = project.join(".config/shepherd/settings.json");
        let local = json!({"version": 1, "settings": {"terminal.scrollback_lines": 50}});
        write_atomic(&local_path, &encode_json(&local)?)?;
        let settings = Settings::new(settings_path);
        settings.reload().await;
        let snapshot = settings
            .load_project_preferences(std::slice::from_ref(&project))
            .await;
        assert!(snapshot.warning.is_none());
        assert_eq!(
            settings.value(Some(&project), SettingKey::Scrollback).await,
            json!(50)
        );
        assert_eq!(
            snapshot.projects[project.to_str().ok_or("project path")?]["terminal.scrollback_lines"],
            json!(50)
        );
        assert!(
            settings
                .edit(
                    Some(&project),
                    edit(SettingKey::Scrollback, Some(json!(50)), Some(json!(60)))
                )
                .await
                .is_err()
        );
        assert_eq!(std::fs::read(&local_path)?, encode_json(&local)?);
        Ok(())
    }

    #[tokio::test]
    async fn migrates_legacy_settings_and_resolves_project_overrides_per_key()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("settings.json");
        let old = json!({
            "app": {"tasks.show_archived": true, "attachments.max_file_bytes": 30, "agents.opencode.executable": "/existing/bin/opencode", "agents.codex.executable": "/existing/bin/codex"},
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
                .agent_settings()
                .await
                .executable("agents.codex.executable")?,
            Some("/existing/bin/codex".into())
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
