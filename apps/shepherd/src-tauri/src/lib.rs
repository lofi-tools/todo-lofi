//! Shepherd's desktop shell.
//!
//! This crate owns the Tauri window, the invoke commands and the event channel.
//! Task behaviour lives in `shepherd-core`; the shell never holds a second copy
//! of the state model.

mod bridge;
mod codex_harness;
mod dev_harness;
mod pty_terminal;
mod terminal_screen;

use shepherd_core::harness::HarnessFactory;
use tauri::Manager;

/// Build and run the window. A startup failure is returned so `main` can report
/// it and exit with a non-zero status.
pub fn run() -> Result<(), tauri::Error> {
    tauri::Builder::default()
        .setup(|app| {
            let data_directory = app.path().app_data_dir()?;
            let probe_directory =
                std::env::current_dir().unwrap_or_else(|_| data_directory.clone());
            let detection = tauri::async_runtime::block_on(agent_harness::codex::detect(
                None,
                &probe_directory,
            ));
            let harness_factory: Box<dyn HarnessFactory> = match &detection.install {
                Some(install) => {
                    tracing::info!("{}", detection.summary());
                    Box::new(codex_harness::CodexHarnessFactory::new(install))
                }
                None => {
                    tracing::warn!("{}", detection.summary());
                    Box::new(codex_harness::MissingCodexFactory::new(detection.summary()))
                }
            };
            let state = tauri::async_runtime::block_on(bridge::ShellState::load(
                &data_directory,
                harness_factory,
            ))?;
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            bridge::commands::add_project,
            bridge::commands::create_draft,
            bridge::commands::discard_draft,
            bridge::commands::send_prompt,
            bridge::commands::resolve_approval,
            bridge::commands::cancel_task,
            bridge::commands::resume_task,
            bridge::commands::quit,
            bridge::commands::project_list,
            bridge::commands::project_scan,
            bridge::commands::task_list,
            bridge::commands::task_workspace,
            bridge::commands::terminal_list,
            bridge::commands::start_terminal,
            bridge::commands::terminal_write,
            bridge::commands::terminal_resize,
            bridge::commands::close_terminal,
            bridge::commands::terminal_frame,
            bridge::commands::subscribe_terminal,
            bridge::commands::subscribe,
        ])
        .run(tauri::generate_context!())
}
