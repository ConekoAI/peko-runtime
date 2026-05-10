use clap::Parser;
use clap_complete::generate;
use pekobot::commands::{
    agent, auth, config, cron, daemon, ext, init_logging, orchestration, provider, send, session,
    system, team, update, Cli, Commands, GlobalPaths,
};
use pekobot::types::config::PekobotConfig;

/// Pekobot - Lightweight Multi-Agent Runtime
#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // Initialize logging
    init_logging(cli.verbose, cli.quiet);

    // Set up global paths
    let paths = GlobalPaths::from_cli(&cli);

    // Initialize global ExtensionCore with the appropriate async transport
    // BEFORE running any command that might create agents.
    // - Daemon commands use LocalAsyncTransport (daemon owns task execution)
    // - CLI commands use DaemonHttpTransport if daemon is reachable;
    //   otherwise UnavailableAsyncTransport so async tools fail fast with a clear error.
    //   ADR-020: No in-process fallback. The old tokio::spawn path is removed from CLI.
    init_extension_core(&cli.command).await;

    // Run the command and handle results/exit codes
    let result = run_command(cli.command, &paths, cli.json).await;

    match result {
        Ok(()) => std::process::exit(0),
        Err(e) => {
            // Print error message
            if cli.debug {
                // With --debug, show full error chain and backtrace if available
                eprintln!("❌ Error: {:?}", e);
            } else {
                // Default: just show the error message
                eprintln!("❌ Error: {}", e);
            }

            std::process::exit(1);
        }
    }
}

/// Initialize the global ExtensionCore with the appropriate transport
///
/// - Daemon commands: LocalAsyncTransport (daemon executes tasks locally)
/// - CLI commands: DaemonHttpTransport if daemon is reachable, else UnavailableAsyncTransport
///   so that async tools fail fast with a clear error instead of falling back to
///   in-process execution that would be dropped on CLI exit (ADR-020).
async fn init_extension_core(command: &Commands) {
    use pekobot::extension::core::{init_global_core, ExtensionCore, ExtensionServices};
    use pekobot::extension::services::AsyncExecutionRouter;
    use std::sync::Arc;

    let is_daemon_cmd = matches!(command, Commands::Daemon(_));

    let router = if is_daemon_cmd {
        tracing::info!("Initializing ExtensionCore with LocalAsyncTransport (daemon mode)");
        AsyncExecutionRouter::with_transport(
            pekobot::extension::services::async_transport::create_local_transport(),
        )
    } else {
        tracing::info!("Auto-detecting async transport for CLI mode");
        match pekobot::extension::services::async_transport::create_transport().await {
            Ok(transport) => AsyncExecutionRouter::with_transport(transport),
            Err(_) => {
                // Daemon does not auto-start; user must start it manually.
                AsyncExecutionRouter::with_transport(std::sync::Arc::new(
                    pekobot::extension::services::async_transport::UnavailableAsyncTransport::new(
                        "Pekobot daemon is not running. Async tool execution requires the daemon.\n\
                         Start it with: pekobot daemon start\n\
                         Or use sync mode (remove _async: true from the tool call).",
                    ),
                ))
            }
        }
    };

    let services = ExtensionServices::with_async_router(router);
    let core = Arc::new(ExtensionCore::with_services(Arc::new(services)));
    init_global_core(core);
    tracing::debug!("Initialized global ExtensionCore with async transport");
}

async fn run_command(command: Commands, paths: &GlobalPaths, json: bool) -> anyhow::Result<()> {
    match command {
        Commands::Agent(cmd) => agent::handle_agent(cmd, paths, json).await,
        Commands::Team(cmd) => team::handle_team(cmd, paths, json).await,
        Commands::Send(args) => send::handle_send(args, paths, json).await,
        Commands::Auth(cmd) => auth::handle_auth(cmd, paths, json),
        Commands::Ext(cmd) => ext::handle_ext_command(cmd, paths, json).await,
        Commands::Session(cmd) => session::handle_session(cmd, paths, json).await,
        Commands::Config(cmd) => config::handle_config(cmd, paths, json).await,
        Commands::System(cmd) => system::handle_system(cmd, paths, json).await,
        Commands::Daemon(cmd) => daemon::handle_daemon(cmd, paths, json).await,
        Commands::Cron(cmd) => cron::handle_cron(cmd, paths, json).await,
        Commands::Orchestration(cmd) => {
            // Load configuration for orchestration commands
            let config_path = paths.config_dir.join("config.toml");
            let config = if config_path.exists() {
                PekobotConfig::from_file(&config_path)?
            } else {
                PekobotConfig::default()
            };
            orchestration::run(cmd, &config, &config_path).await
        }
        Commands::Provider(cmd) => provider::execute(cmd).await,
        Commands::Update { check, force } => update::handle_update(check, force).await,
        Commands::Completions { shell } => {
            let mut cmd = <Cli as clap::CommandFactory>::command();
            let name = cmd.get_name().to_string();
            generate(shell, &mut cmd, name, &mut std::io::stdout());
            Ok(())
        }
    }
}
