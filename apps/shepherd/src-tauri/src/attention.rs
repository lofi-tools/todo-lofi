use crate::bridge::{BridgeError, ShellState};
use crate::settings::SettingKey;
use shepherd_core::TaskId;
use tauri::State;

#[tauri::command]
pub async fn notify_task_needs_input(
    state: State<'_, ShellState>,
    task_id: String,
) -> Result<(), BridgeError> {
    if state
        .preferences
        .value(None, SettingKey::Notifications)
        .await
        .as_bool()
        != Some(true)
    {
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
pub(crate) async fn authorize_notifications() -> Result<(), BridgeError> {
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
