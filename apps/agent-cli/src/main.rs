use crate::cli_commands::Cli;
use crate::config::AppConfig;
use crate::providers::AgentRuntime;
use clap::Parser;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tokio_util::sync::CancellationToken;

pub mod acp;
pub mod cli_commands;
pub mod config;
pub mod providers;
pub mod signals;
pub mod tui;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // fastrace::set_reporter(ConsoleReporter, Config::default());
    let cli = Cli::parse();

    // // Initialize tracing
    // if cli.verbose {
    //     tracing_subscriber::fmt()
    //         .with_env_filter("abstract=debug,cersei=debug")
    //         .init();
    // } else {
    //     tracing_subscriber::fmt()
    //         .with_env_filter("abstract=warn,cersei=warn")
    //         .init();
    // }

    // Make saved credentials visible as env vars so downstream registry lookups
    // find them. Explicit env vars still win.
    // login::export_saved_keys_to_env();

    // Load config with CLI overrides
    let mut config = config::load();
    config::apply_cli_overrides(&cli, &mut config);

    // ACP server mode: speak Agent Client Protocol over stdio.
    if cli.acp {
        return acp::run_server(cli, config).await;
    }

    let runtime = Arc::new(AgentRuntime::new(&config)?);

    let prompt = cli.prompt.as_deref().filter(|p| *p != ".");
    if let Some(prompt_text) = prompt {
        let stream = runtime.agent().run_stream(prompt_text);
        let text = stream.collect_text().await?;
        dbg!(&text);
    } else {
        run_tui_app(cli, config, runtime).await?;
    }

    // fastrace::flush();
    Ok(())
}

pub async fn run_tui_app(_cli: Cli, config: AppConfig, runtime: Arc<AgentRuntime>) -> anyhow::Result<()> {
    // let theme = Theme::from_name(&config.theme);

    // Resolve or create session ID
    // let session_id = if let Some(ref resume) = cli.resume {
    //     if resume == "last" {
    //         sessions::last_session_id(&config)
    //             .ok_or_else(|| anyhow::anyhow!("No previous session found"))?
    //     } else {
    //         resume.clone()
    //     }
    // } else {
    //     uuid::Uuid::new_v4().to_string()
    // };

    // Build memory manager with graph memory
    // let memory_manager = build_memory_manager(&config)?;

    let cancel_token = CancellationToken::new();
    let running = Arc::new(AtomicBool::new(false));

    // Install signal handlers
    crate::signals::install(cancel_token.clone(), running.clone())?;

    // Build the initial agent with shared permission mode and TUI permission channel
    // let shared_mode = crate::permissions::new_shared_mode();
    // let (perm_tx, perm_rx) = crate::permissions::permission_channel();
    // let (agent, resolved_model) = build_agent(
    //     &config.model,
    //     &config,
    //     &memory_manager,
    //     &session_id,
    //     cancel_token.clone(),
    //     None,
    //     Some(shared_mode.clone()),
    //     Some(perm_tx),
    // )?;
    // config.model = resolved_model;

    // Show startup banner
    // let effort = EffortLevel::from_str(&config.effort);
    // JSON mode: --json flag OR --output-format stream-json
    // let json_mode = cli.json || config.output_format == "stream-json";
    // if !json_mode {
    //     print_banner(&config, &session_id, &effort);
    // }

    // Dispatch to REPL or single-shot
    // "." means "start interactive in current directory"
    // let prompt = cli.prompt.as_deref().filter(|p| *p != ".");
    // if let Some(prompt_text) = prompt {
    //     let prompt_text = prompt_text.to_string();
    //     repl::run_single_shot(
    //         agent,
    //         &prompt_text,
    //         &theme,
    //         &session_id,
    //         &config,
    //         &memory_manager,
    //         json_mode,
    //         running,
    //         cancel_token,
    //     )
    //     .await
    // } else if json_mode {
    //     // JSON mode uses the old REPL (no TUI)
    //     repl::run_repl(
    //         agent,
    //         &theme,
    //         &session_id,
    //         &config,
    //         &memory_manager,
    //         json_mode,
    //         running,
    //         cancel_token.clone(),
    //     )
    //     .await
    // } else {
    // TUI mode (default interactive)
    tui::run_repl(
        runtime,
        &config,
        // &memory_manager,
        // &session_id,
        cancel_token,
        // shared_mode,
        // perm_rx,
    )
    .await
    // }
}

// async fn alt_main_oneshot() -> anyhow::Result<()> {
//     struct State {
//         start_time: Instant,
//         text_bytes: usize,
//         tool_count: u32,
//     }

//     let agent = std::sync::Arc::new(build_agent_wip()?);
//     let mut stream = agent.run_stream("What files are in the current directory? List them.");
//     let mut state = State {
//         start_time: Instant::now(),
//         text_bytes: 0usize,
//         tool_count: 0u32,
//     };
//     while let Some(event) = stream.next().await {
//         let root = Span::root("worker-loop", SpanContext::random());
//         let _guard = root.set_local_parent();

//         handle_agent_event(&event, &mut state)?;
//         if matches!(&event, AgentEvent::Error(_) | AgentEvent::Complete(_)) {
//             break;
//         }
//     }
//     Ok(())
// }
// #[fastrace::trace]
// fn handle_agent_event(event: &AgentEvent, state: &mut State) -> anyhow::Result<()> {
//     match event {
//         AgentEvent::TextDelta(text) => {
//             state.text_bytes += text.len();
//             print!("{}", text);
//         }
//         AgentEvent::ThinkingDelta(text) => {
//             eprint!("\x1b[2m{}\x1b[0m", text); // dim
//         }
//         AgentEvent::TurnStart { turn } => {
//             eprintln!("\n\x1b[36m── Turn {} ──\x1b[0m", turn);
//         }
//         AgentEvent::ToolStart { name, .. } => {
//             state.tool_count += 1;
//             eprint!("\x1b[33m⚙ {}...\x1b[0m ", name);
//         }
//         AgentEvent::ToolEnd {
//             name: _,
//             duration,
//             is_error,
//             ..
//         } => {
//             let status = if *is_error {
//                 "\x1b[31m✗\x1b[0m"
//             } else {
//                 "\x1b[32m✓\x1b[0m"
//             };
//             eprintln!("{} ({}ms)", status, duration.as_millis());
//         }
//         AgentEvent::CostUpdate {
//             cumulative_cost,
//             input_tokens,
//             output_tokens,
//             ..
//         } if *cumulative_cost > 0.0 => {
//             eprintln!(
//                 "\x1b[2m  cost: ${:.4} | {}in/{}out tokens\x1b[0m",
//                 cumulative_cost, input_tokens, output_tokens
//             );
//         }
//         AgentEvent::CostUpdate {
//             cumulative_cost: _,
//             input_tokens: _,
//             output_tokens: _,
//             ..
//         } => {}
//         AgentEvent::TokenWarning { pct_used, .. } => {
//             eprintln!("\x1b[31m⚠ Context {:.0}% full\x1b[0m", pct_used * 100.0);
//         }
//         AgentEvent::Complete(output) => {
//             let elapsed = state.start_time.elapsed();
//             println!("\n");
//             println!("─── Stream Complete ───");
//             println!("Time:        {:.2}s", elapsed.as_secs_f64());
//             println!("Turns:       {}", output.turns);
//             println!("Tool calls:  {}", state.tool_count);
//             println!("Text bytes:  {}", state.text_bytes);
//             println!("Input tok:   {}", output.usage.input_tokens);
//             println!("Output tok:  {}", output.usage.output_tokens);
//             // break;
//             // Return Ok(None)
//             return Ok(());
//         }
//         AgentEvent::Error(e) => {
//             eprintln!("\n\x1b[31mError: {}\x1b[0m", e);
//             bail!("{}", e);
//         }
//         _ => {} // Ignore other events
//     }
//     Ok(())
// }
