use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use shepherd_core::TaskId;
use tauri::State;
use tokio::sync::Mutex;

use crate::bridge::{BridgeError, ShellState};

const NOTIFICATIONS_KEY: &str = "notifications.enabled";

pub struct NotificationSettings {
    path: PathBuf,
    write: Mutex<()>,
}

impl NotificationSettings {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            write: Mutex::new(()),
        }
    }

    async fn read(&self) -> Result<Value, BridgeError> {
        match tokio::fs::read(&self.path).await {
            Ok(bytes) => {
                let settings: Value =
                    serde_json::from_slice(&bytes).map_err(|error| self.error(error))?;
                if !settings.is_object() || settings.get("app").is_some_and(|app| !app.is_object())
                {
                    return Err(
                        self.error("Expected a settings object with an optional app object.")
                    );
                }
                Ok(settings)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(json!({ "version": 1 })),
            Err(error) => Err(self.error(error)),
        }
    }

    async fn enabled(&self) -> Result<bool, BridgeError> {
        match self.read().await?["app"].get(NOTIFICATIONS_KEY) {
            None | Some(Value::Null) => Ok(false),
            Some(Value::Bool(enabled)) => Ok(*enabled),
            Some(_) => Err(self.error("notifications.enabled must be a boolean.")),
        }
    }

    async fn save_enabled(&self, enabled: bool) -> Result<(), BridgeError> {
        let _write = self.write.lock().await;
        let mut document = self.read().await?;
        if document
            .get("version")
            .is_some_and(|version| version.as_u64() != Some(1))
        {
            return Err(self.error("This settings version cannot be edited."));
        }
        document["app"][NOTIFICATIONS_KEY] = json!(enabled);
        write_settings(&self.path, &document).map_err(|error| self.error(error))
    }

    fn error(&self, cause: impl std::fmt::Display) -> BridgeError {
        BridgeError::new(
            "settings",
            format!("Could not use {}. {cause}", self.path.display()),
        )
    }
}

fn write_settings(path: &Path, settings: &Value) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("Missing settings directory."))?;
    std::fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(temporary.as_file_mut(), settings)?;
    writeln!(temporary)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path)?;
    Ok(())
}

#[tauri::command]
pub async fn notifications_enabled(
    settings: State<'_, NotificationSettings>,
) -> Result<bool, BridgeError> {
    settings.enabled().await
}

#[tauri::command]
pub async fn set_notifications_enabled(
    settings: State<'_, NotificationSettings>,
    enabled: bool,
) -> Result<(), BridgeError> {
    #[cfg(target_os = "macos")]
    if enabled {
        authorize_notifications().await?;
    }
    settings.save_enabled(enabled).await
}

#[tauri::command]
pub async fn notify_task_needs_input(
    state: State<'_, ShellState>,
    settings: State<'_, NotificationSettings>,
    task_id: String,
) -> Result<(), BridgeError> {
    if !settings.enabled().await? {
        return Ok(());
    }
    if let Some(title) = state.waiting_task_title(TaskId::new(task_id)).await? {
        show_notification(&title).await?;
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
async fn show_notification(title: &str) -> Result<(), BridgeError> {
    notify_rust::Notification::new()
        .appname("Shepherd")
        .summary("Task needs input")
        .body(title)
        .show_async()
        .await
        .map_err(|error| {
            BridgeError::new(
                "notification",
                format!("Could not show a system notification. {error}"),
            )
        })?;
    Ok(())
}

#[cfg(target_os = "macos")]
async fn authorize_notifications() -> Result<(), BridgeError> {
    notify_rust::check_bundle().map_err(|error| {
        BridgeError::new(
            "notification",
            format!("Run the Shepherd app bundle to enable macOS notifications. {error}"),
        )
    })?;
    let granted = notify_rust::request_auth().await.map_err(|error| {
        BridgeError::new(
            "notification",
            format!("Could not request macOS notification permission. {error}"),
        )
    })?;
    if !granted {
        return Err(BridgeError::new(
            "notification",
            "Allow notifications for Shepherd in macOS System Settings.",
        ));
    }
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
async fn show_notification(_title: &str) -> Result<(), BridgeError> {
    Err(BridgeError::new(
        "notification",
        "System notifications are unavailable on this platform.",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn notification_preference_is_opt_in_and_preserves_agent_settings()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("settings.json");
        let settings = NotificationSettings::new(path.clone());
        assert!(!settings.enabled().await?);
        settings.save_enabled(true).await?;
        assert!(NotificationSettings::new(path.clone()).enabled().await?);

        let mut document = json!({
            "version": 1,
            "app": {"agents.opencode.executable": "/existing/bin/opencode", "notifications.enabled": true},
            "projects": {"/workspace": {"attachments.max_file_bytes": 1000}},
        });
        write_settings(&path, &document)?;
        settings.save_enabled(false).await?;
        document["app"][NOTIFICATIONS_KEY] = json!(false);
        assert_eq!(settings.read().await?, document);
        assert!(!settings.enabled().await?);

        document["version"] = json!(2);
        write_settings(&path, &document)?;
        assert!(settings.save_enabled(true).await.is_err());
        assert_eq!(settings.read().await?, document);
        Ok(())
    }
}
