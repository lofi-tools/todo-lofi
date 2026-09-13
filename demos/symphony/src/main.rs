//! Symphony CLI host (Section 17.7).

use argh::FromArgs;
use log::info;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;
use symphony::config::EffectiveWorkflow;
use symphony::http;
use symphony::orchestrator::Orchestrator;
use symphony::tracker::{LinearGraphqlTool, LinearTracker};
use symphony::workflow::select_workflow_path;
use symphony::workspace::WorkspaceManager;
use tokio::sync::watch;

#[derive(FromArgs, Debug)]
/// Run the Symphony coding-agent orchestrator.
struct Args {
    /// path to WORKFLOW.md (defaults to ./WORKFLOW.md)
    #[argh(positional)]
    workflow: Option<PathBuf>,

    /// port for the optional HTTP observability server (overrides `server.port`)
    #[argh(option)]
    port: Option<u16>,
}

fn main() -> ExitCode {
    let args: Args = argh::from_env();
    init_logging();

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("symphony: startup failed: could not start async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };

    match runtime.block_on(run(args)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("symphony: startup failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn init_logging() {
    // Structured `key=value` logs to stderr; `RUST_LOG` controls the level.
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    if let Err(error) = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_writer(std::io::stderr)
        .try_init()
    {
        // Logging is best-effort: the service keeps running on the default sink.
        eprintln!("symphony: warning: could not initialise logging: {error}");
    }
}

async fn run(args: Args) -> symphony::error::Result<()> {
    let workflow_path = select_workflow_path(args.workflow.as_deref());
    if !workflow_path.exists() {
        return Err(symphony::error::SymphonyError::MissingWorkflowFile {
            path: workflow_path.to_string_lossy().into_owned(),
        });
    }

    let mut effective = EffectiveWorkflow::load(&workflow_path)?;
    if let Some(port) = args.port {
        // A CLI `--port` overrides `server.port` when both are present.
        effective.config.server.port = Some(port);
    }

    info!(
        target: "symphony",
        "outcome=started workflow={} workspace_root={} poll_interval_ms={} max_concurrent_agents={}",
        workflow_path.display(),
        effective.config.workspace.root.display(),
        effective.config.polling.interval_ms,
        effective.config.agent.max_concurrent_agents,
    );

    let workspace_root = effective.config.workspace.root.clone();
    tokio::fs::create_dir_all(&workspace_root)
        .await
        .map_err(|error| symphony::error::SymphonyError::WorkspaceError {
            source: Box::new(error),
        })?;

    let workspace_manager = WorkspaceManager::new(workspace_root);
    let tracker = LinearTracker::from_config(&effective.config.tracker)?;

    // The optional `linear_graphql` client tool is advertised to the session.
    let tools: Vec<Arc<dyn symphony::agent::ClientTool>> = vec![Arc::new(
        LinearGraphqlTool::from_config(&effective.config.tracker)?,
    )];

    let server_port = effective.config.server.port;
    let mut orchestrator =
        Orchestrator::new(tracker, workspace_manager, effective, tools)?;
    let observability = orchestrator.observability();

    if let Some(port) = server_port {
        let listener = http::bind(port).await?;
        tokio::spawn(async move {
            if let Err(error) = http::serve(listener, observability).await {
                log::error!(target: "symphony", "outcome=failed action=http_server error={error}");
            }
        });
    }

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    tokio::spawn(async move {
        wait_for_shutdown_signal().await;
        info!(target: "symphony", "outcome=stopping signal=received");
        if shutdown_tx.send(true).is_err() {
            log::debug!(
                target: "symphony",
                "outcome=ignored message=orchestrator already stopped"
            );
        }
    });

    orchestrator.run(shutdown_rx).await?;

    // Give workers a moment to notice cancellation before the process exits.
    tokio::time::sleep(Duration::from_millis(50)).await;
    Ok(())
}

async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        match signal(SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = terminate.recv() => {}
                }
                return;
            }
            Err(error) => {
                log::warn!(
                    target: "symphony",
                    "outcome=failed action=install_sigterm error={error}"
                );
            }
        }
    }

    if let Err(error) = tokio::signal::ctrl_c().await {
        log::warn!(target: "symphony", "outcome=failed action=wait_for_shutdown error={error}");
    }
}
