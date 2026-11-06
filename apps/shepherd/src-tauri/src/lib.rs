//! Shepherd's desktop shell.
//!
//! This crate owns the Tauri window, the invoke commands and the event channel.
//! Task behaviour lives in `shepherd-core`; the shell never holds a second copy
//! of the state model.

mod bridge;
mod codex_harness;
#[cfg(any(test, debug_assertions))]
mod dev_harness;
mod file_actions;
mod file_watch;
mod pty_terminal;
mod terminal_screen;

use std::path::PathBuf;

use agent_harness::codex::CodexTier;
use agent_harness::settings::AgentSettings;
use shepherd_core::harness::HarnessFactory;
use tauri::Manager;

fn codex_factory(data_directory: &std::path::Path) -> (Box<dyn HarnessFactory>, Option<PathBuf>) {
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
                Box::new(codex_harness::MissingCodexFactory::new(
                    detection.summary(),
                    detection.failure_kind(),
                )),
                None,
            )
        }
    }
}

/// Build and run the window. A startup failure is returned so `main` can report
/// it and exit with a non-zero status.
pub fn run() -> Result<(), tauri::Error> {
    let builder = tauri::Builder::default();
    #[cfg(target_os = "macos")]
    let builder = {
        use tauri::menu::{Menu, MenuItem, MenuItemKind};

        const QUIT_MENU_ITEM: &str = "shepherd-quit";

        builder
            .menu(|app| {
                let menu = Menu::default(app)?;
                let Some(MenuItemKind::Submenu(application)) = menu.items()?.into_iter().next()
                else {
                    return Err(std::io::Error::other("missing macOS application menu").into());
                };
                let Some(MenuItemKind::Predefined(quit)) = application.items()?.pop() else {
                    return Err(std::io::Error::other("missing macOS quit menu item").into());
                };

                // AppKit's Quit role terminates before the window can confirm and shut down.
                let guarded_quit =
                    MenuItem::with_id(app, QUIT_MENU_ITEM, quit.text()?, true, Some("Cmd+Q"))?;
                application.remove(&quit)?;
                application.append(&guarded_quit)?;
                Ok(menu)
            })
            .on_menu_event(|app, event| {
                if event.id() == QUIT_MENU_ITEM
                    && let Some(window) = app.get_webview_window("main")
                    && let Err(error) = window.close()
                {
                    tracing::error!(%error, "could not request window close from the Quit menu");
                }
            })
    };

    let builder = builder
        .setup(|app| {
            let terminals = bridge::TerminalHost::default();
            let override_directory = cfg!(debug_assertions)
                .then(|| std::env::var_os("SHEPHERD_DEV_DATA_DIR"))
                .flatten()
                .filter(|path| !path.is_empty());
            #[cfg(debug_assertions)]
            let dev_factory =
                dev_harness::factory(app, override_directory.as_deref().map(std::path::Path::new))?;
            #[cfg(not(debug_assertions))]
            let dev_factory: Option<Box<dyn HarnessFactory>> = None;
            let data_directory = match override_directory {
                Some(path) => path.into(),
                None => app.path().app_data_dir()?,
            };
            let (harness_factory, codex_program, opencode_configured, claude_configured): (
                Box<dyn HarnessFactory>,
                _,
                _,
                _,
            ) = match dev_factory {
                Some(factory) => (factory, None, false, false),
                None => {
                    let (codex, codex_program) = codex_factory(&data_directory);
                    let settings = app.path().app_config_dir()?.join("settings.json");
                    let (opencode, claude) = match AgentSettings::read(&settings) {
                        Ok(settings) => (
                            settings.executable("agents.opencode.executable"),
                            settings.command("agents.claude"),
                        ),
                        Err(error) => {
                            let report = error.to_string();
                            (
                                Err(error),
                                Err(agent_harness::HarnessError::Protocol(report)),
                            )
                        }
                    };
                    if let Err(error) = &opencode {
                        tracing::warn!(%error, "could not load OpenCode configuration");
                    }
                    if let Err(error) = &claude {
                        tracing::warn!(%error, "could not load Claude Code configuration");
                    }
                    let opencode_configured = matches!(&opencode, Ok(Some(_)));
                    let claude_configured = matches!(&claude, Ok(Some(_)));
                    (
                        Box::new(codex_harness::DesktopHarnessFactory {
                            codex,
                            opencode,
                            claude,
                            terminals: std::sync::Arc::new(terminals.clone()),
                        }),
                        codex_program,
                        opencode_configured,
                        claude_configured,
                    )
                }
            };
            let mut state = tauri::async_runtime::block_on(bridge::ShellState::load(
                &data_directory,
                harness_factory,
                terminals,
            ))?;
            state.codex_program = codex_program;
            state.opencode_configured = opencode_configured;
            state.claude_configured = claude_configured;
            state.start_event_pump();
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            bridge::commands::add_project,
            bridge::commands::create_draft,
            bridge::commands::set_draft_agent,
            bridge::commands::opencode_configured,
            bridge::commands::claude_configured,
            bridge::commands::discard_draft,
            bridge::commands::send_prompt,
            bridge::commands::resolve_approval,
            bridge::commands::project_approvals,
            bridge::commands::reset_approval_grant,
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
            bridge::commands::save_workspace_layout,
            bridge::commands::review_file_diff,
            bridge::commands::review_snapshot,
            bridge::commands::review_task,
            bridge::commands::git_file_diff,
            bridge::commands::git_snapshot,
            bridge::commands::list_files,
            bridge::commands::read_workspace_file,
            bridge::commands::act_on_workspace_file,
            bridge::commands::watch_workspace_files,
            bridge::commands::unwatch_workspace_files,
            bridge::commands::reserve_file_search,
            bridge::commands::search_workspace_files,
            bridge::commands::cancel_file_search,
            bridge::commands::mutate_git,
            bridge::commands::terminal_layout,
            bridge::commands::save_terminal_layout,
            bridge::commands::save_inspector_tab,
            bridge::commands::terminal_list,
            bridge::commands::start_terminal,
            bridge::commands::start_freebuff_terminal,
            bridge::commands::restart_terminal,
            bridge::commands::search_terminal,
            bridge::commands::terminal_scroll,
            bridge::commands::terminal_follow_output,
            bridge::commands::terminal_write,
            bridge::commands::open_terminal_link,
            bridge::commands::terminal_resize,
            bridge::commands::close_terminal,
            bridge::commands::acknowledge_terminal_frame,
            bridge::commands::unsubscribe_terminal,
            bridge::commands::subscribe_terminal,
            bridge::commands::subscribe,
            #[cfg(all(debug_assertions, feature = "desktop-codex-smoke"))]
            dev_harness::finish_codex_smoke,
            #[cfg(all(debug_assertions, feature = "desktop-codex-smoke"))]
            dev_harness::trigger_codex_smoke_exit,
        ]);

    let context = tauri::generate_context!();
    #[cfg(all(target_os = "macos", debug_assertions))]
    if std::env::var("SHEPHERD_DEV_BACKGROUND_SMOKE").as_deref() == Ok("1") {
        if !std::env::var_os("SHEPHERD_DEV_DATA_DIR").is_some_and(|path| !path.is_empty()) {
            return Err(std::io::Error::other("Background smoke requires isolated data").into());
        }
        if context
            .config()
            .app
            .windows
            .iter()
            .any(|window| window.visible || window.focus || window.focusable)
        {
            return Err(std::io::Error::other("Background smoke requires hidden windows").into());
        }
        let mut app = builder.build(context)?;
        // Tao activates regular apps before Tauri runs setup.
        app.set_activation_policy(tauri::ActivationPolicy::Prohibited);
        app.run(|_, _| {});
        return Ok(());
    }
    builder.run(context)
}
