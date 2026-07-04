use clap::Parser;
use clap_complete::generate;
use peko::commands::{
    auth, config, credential, cron, daemon, ext, init_logging, principal, provider, registry,
    runtime, search, send, system, tunnel, update, vault, Cli, Commands, GlobalPaths,
};

/// Peko - Lightweight Multi-Agent Runtime
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
    let cli_registry = cli.registry.as_deref();
    let result = run_command(cli.command, &paths, cli.json, cli_registry).await;

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
    use peko::extensions::framework::core::{init_global_core, ExtensionCore, ExtensionServices};
    use peko::extensions::framework::services::AsyncExecutionRouter;
    use std::sync::Arc;

    let is_daemon_cmd = matches!(command, Commands::Daemon(_));

    let router = if is_daemon_cmd {
        tracing::info!("Initializing ExtensionCore with LocalAsyncTransport (daemon mode)");
        AsyncExecutionRouter::with_transport(
            peko::extensions::framework::services::async_transport::create_local_transport(),
        )
    } else {
        tracing::info!("Auto-detecting async transport for CLI mode");
        match peko::extensions::framework::services::async_transport::create_transport().await {
            Ok(transport) => AsyncExecutionRouter::with_transport(transport),
            Err(_) => {
                // Daemon does not auto-start; user must start it manually.
                AsyncExecutionRouter::with_transport(std::sync::Arc::new(
                    peko::extensions::framework::services::async_transport::UnavailableAsyncTransport::new(
                        "peko daemon is not running. Async tool execution requires the daemon.\n\
                         Start it with: peko daemon start\n\
                         Or wait for the task to complete via AsyncOutput.",
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

async fn run_command(
    command: Commands,
    paths: &GlobalPaths,
    json: bool,
    cli_registry: Option<&str>,
) -> anyhow::Result<()> {
    match command {
        Commands::Principal(cmd) => principal::handle_principal(cmd, paths, json).await,
        Commands::Send(args) => send::handle_send(args, paths, json).await,
        Commands::Auth(cmd) => auth::handle_auth(cmd, paths, json),
        Commands::Credential(cmd) => credential::execute(cmd, paths).await,
        Commands::Vault(cmd) => vault::execute(cmd, paths).await,
        Commands::Ext(cmd) => ext::handle_ext_command(cmd, paths, json, cli_registry).await,
        Commands::Config(cmd) => config::handle_config(cmd, paths, json).await,
        Commands::System(cmd) => system::handle_system(cmd, paths, json).await,
        Commands::Daemon(cmd) => daemon::handle_daemon(cmd, paths, json).await,
        Commands::Cron(cmd) => cron::handle_cron(cmd, paths, json).await,
        Commands::Provider(cmd) => provider::execute(cmd, paths).await,
        Commands::Search(cmd) => search::handle_search(cmd, paths, json).await,
        Commands::Registry(cmd) => registry::handle_registry(cmd, paths, json),
        Commands::Runtime(cmd) => runtime::handle_runtime(cmd, paths, json).await,
        Commands::Tunnel(cmd) => tunnel::handle_tunnel(cmd, paths, json).await,
        Commands::Login { registry, api_key } => {
            let host = registry.unwrap_or_else(|| paths.registry_config().default);
            auth::handle_login(paths, &host, api_key)
        }
        Commands::Logout { registry } => {
            let host = registry.unwrap_or_else(|| paths.registry_config().default);
            auth::handle_logout(paths, &host)
        }
        Commands::Update { check, force } => update::handle_update(check, force).await,
        Commands::Completions { shell } => {
            let mut cmd = <Cli as clap::CommandFactory>::command();
            let name = cmd.get_name().to_string();
            generate(shell, &mut cmd, name, &mut std::io::stdout());
            Ok(())
        }
    }
}
