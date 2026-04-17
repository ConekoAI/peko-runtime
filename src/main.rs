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
    // - CLI commands try DaemonHttpTransport first, fall back to LocalAsyncTransport
    if let Err(e) = init_extension_core(&cli.command).await {
        tracing::warn!("Failed to initialize extension core with preferred transport: {}", e);
        // Fallback: init with default local transport
        let _ = run_extension_migration_with_local_transport().await;
    }

    // Run auto-migration for legacy extensions (Phase 8)
    // This is idempotent - will only migrate once
    if let Err(e) = run_extension_migration(&paths).await {
        tracing::warn!("Legacy extension migration failed: {}", e);
        // Don't fail startup on migration error, just warn
    }

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

            // Determine exit code
            let exit_code =
                if let Some(client_err) = e.downcast_ref::<pekobot::api::client::ClientError>() {
                    client_err.exit_code()
                } else {
                    1
                };

            std::process::exit(exit_code);
        }
    }
}

/// Initialize the global ExtensionCore with the appropriate transport
///
/// - Daemon commands: LocalAsyncTransport (daemon executes tasks locally)
/// - CLI commands: DaemonHttpTransport if daemon is reachable, else LocalAsyncTransport
async fn init_extension_core(command: &Commands) -> anyhow::Result<()> {
    use pekobot::extensions::core::{init_global_core, ExtensionCore, ExtensionServices};
    use pekobot::extensions::services::AsyncExecutionRouter;
    use std::sync::Arc;

    let is_daemon_cmd = matches!(command, Commands::Daemon(_));

    let router = if is_daemon_cmd {
        tracing::info!("Initializing ExtensionCore with LocalAsyncTransport (daemon mode)");
        AsyncExecutionRouter::with_transport(
            pekobot::extensions::services::async_transport::create_local_transport(),
        )
    } else {
        tracing::info!("Auto-detecting async transport for CLI mode");
        let transport = pekobot::extensions::services::async_transport::create_transport().await;
        AsyncExecutionRouter::with_transport(transport)
    };

    let services = ExtensionServices::with_async_router(router);
    let core = Arc::new(ExtensionCore::with_services(Arc::new(services)));
    init_global_core(core);
    tracing::debug!("Initialized global ExtensionCore with async transport");

    Ok(())
}

/// Fallback: initialize global ExtensionCore with default local transport
async fn run_extension_migration_with_local_transport() -> anyhow::Result<()> {
    use pekobot::extensions::core::{init_global_core, ExtensionCore};
    use std::sync::Arc;

    if pekobot::extensions::core::global_core().is_none() {
        let core = Arc::new(ExtensionCore::new());
        init_global_core(core);
    }
    Ok(())
}

/// Run auto-migration for legacy extensions (Phase 8)
///
/// This function checks if legacy extensions need to be migrated to the new
/// Extension 2.0 system and performs the migration if needed.
async fn run_extension_migration(_paths: &GlobalPaths) -> anyhow::Result<()> {
    use pekobot::extensions::core::{global_core, init_global_core, ExtensionCore};
    use pekobot::extensions::manager::ExtensionManager;
    use pekobot::extensions::migration::migrate_legacy_extensions;
    use std::sync::Arc;

    // Ensure global core is initialized (fallback if init_extension_core failed)
    let core = global_core().unwrap_or_else(|| {
        let core = Arc::new(ExtensionCore::new());
        init_global_core(core.clone());
        core
    });
    tracing::debug!("Using global ExtensionCore for migration");

    // Create extension manager with the global core
    let mut manager = ExtensionManager::with_core(core.clone());

    // Run migration
    let report = migrate_legacy_extensions(&mut manager).await?;

    // Log results if anything was migrated
    let total = report.total_migrated();
    if total > 0 {
        tracing::info!(
            "Migrated {} legacy extensions: {} skills, {} MCP servers, {} universal tools",
            total,
            report.skills_migrated.len(),
            report.mcp_servers_migrated.len(),
            report.tools_migrated.len()
        );
    }

    // Log any errors
    for (item, error) in &report.errors {
        tracing::warn!("Failed to migrate {}: {}", item, error);
    }

    Ok(())
}

async fn run_command(command: Commands, paths: &GlobalPaths, json: bool) -> anyhow::Result<()> {
    match command {
        Commands::Agent(cmd) => agent::handle_agent(cmd, paths, json).await,
        Commands::Team(cmd) => team::handle_team(cmd, paths, json).await,
        Commands::Send(args) => send::handle_send(args, paths, json).await,
        Commands::Auth(cmd) => auth::handle_auth(cmd, paths, json).await,
        Commands::Ext(cmd) => ext::handle_ext_command(cmd, paths).await,
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
