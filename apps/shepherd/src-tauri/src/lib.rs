//! Shepherd's desktop shell.
//!
//! This crate owns the Tauri window, the invoke commands and the event channel.
//! Task behaviour lives in `shepherd-core`; the shell never holds a second copy
//! of the state model.

mod bridge;
mod codex_harness;
#[cfg(any(test, debug_assertions))]
mod dev_harness;
mod pty_terminal;
mod terminal_screen;

use std::path::PathBuf;

use agent_harness::codex::CodexTier;
use shepherd_core::harness::HarnessFactory;
use tauri::Manager;

fn harness_factory(
    data_directory: &std::path::Path,
    isolated: bool,
) -> (Box<dyn HarnessFactory>, Option<PathBuf>) {
    #[cfg(debug_assertions)]
    if isolated && std::env::var("SHEPHERD_DEV_SCRIPTED_HARNESS").as_deref() == Ok("1") {
        return (Box::new(dev_harness::ScriptedHarnessFactory), None);
    }
    #[cfg(not(debug_assertions))]
    let _isolated = isolated;
    let probe_directory = std::env::current_dir().unwrap_or_else(|_| data_directory.to_path_buf());
    let detection =
        tauri::async_runtime::block_on(agent_harness::codex::detect(None, &probe_directory));
    match &detection.install {
        Some(install) => {
            tracing::info!("{}", detection.summary());
            let program =
                (install.probe.tier == CodexTier::AppServer).then(|| install.program.clone());
            (
                Box::new(codex_harness::CodexHarnessFactory::new(install)),
                program,
            )
        }
        None => {
            tracing::warn!("{}", detection.summary());
            (
                Box::new(codex_harness::MissingCodexFactory::new(detection.summary())),
                None,
            )
        }
    }
}

/// Build and run the window. A startup failure is returned so `main` can report
/// it and exit with a non-zero status.
pub fn run() -> Result<(), tauri::Error> {
    tauri::Builder::default()
        .setup(|app| {
            let override_directory = cfg!(debug_assertions)
                .then(|| std::env::var_os("SHEPHERD_DEV_DATA_DIR"))
                .flatten()
                .filter(|path| !path.is_empty());
            let isolated = override_directory.is_some();
            let data_directory = match override_directory {
                Some(path) => path.into(),
                None => app.path().app_data_dir()?,
            };
            let (harness_factory, codex_program) = harness_factory(&data_directory, isolated);
            let mut state = tauri::async_runtime::block_on(bridge::ShellState::load(
                &data_directory,
                harness_factory,
            ))?;
            state.codex_program = codex_program;
            state.start_event_pump();
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
            bridge::commands::project_remote,
            bridge::commands::project_scan,
            bridge::commands::local_codex_available,
            bridge::commands::local_codex_projects,
            bridge::commands::codex_project_model,
            bridge::commands::task_list,
            bridge::commands::task_workspace,
            bridge::commands::review_file_diff,
            bridge::commands::review_snapshot,
            bridge::commands::review_task,
            bridge::commands::git_file_diff,
            bridge::commands::git_snapshot,
            bridge::commands::mutate_git,
            bridge::commands::terminal_layout,
            bridge::commands::save_terminal_layout,
            bridge::commands::terminal_list,
            bridge::commands::start_terminal,
            bridge::commands::restart_terminal,
            bridge::commands::search_terminal,
            bridge::commands::terminal_scroll,
            bridge::commands::terminal_write,
            bridge::commands::open_terminal_link,
            bridge::commands::terminal_resize,
            bridge::commands::close_terminal,
            bridge::commands::acknowledge_terminal_frame,
            bridge::commands::unsubscribe_terminal,
            bridge::commands::subscribe_terminal,
            bridge::commands::subscribe,
        ])
        .run(tauri::generate_context!())
}
