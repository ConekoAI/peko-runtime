use clap::Parser;
use clap_complete::generate;
use pekobot::cap;
use pekobot::commands::{
    agent, auth, config, cron, daemon, ext, gateway, init_logging, orchestration, provider, send,
    session, system, team, update, Cli, Commands, GlobalPaths,
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

/// Run auto-migration for legacy extensions (Phase 8)
/// 
/// This function checks if legacy extensions need to be migrated to the new
/// Extension 2.0 system and performs the migration if needed.
async fn run_extension_migration(paths: &GlobalPaths) -> anyhow::Result<()> {
    use pekobot::extensions::manager::{ExtensionManager, ExtensionStorage};
    use pekobot::extensions::migration::migrate_legacy_extensions;
    
    // Create extension manager with storage
    let storage = ExtensionStorage::with_dir(paths.data_dir.join("extensions"));
    let mut manager = ExtensionManager::with_storage(storage);
    
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
        Commands::Cap(cmd) => cap::commands::handle_cap_command(cmd, paths, json).await,
        Commands::Ext(cmd) => ext::handle_ext_command(cmd, paths).await,
        Commands::Session(cmd) => session::handle_session(cmd, paths, json).await,
        Commands::Config(cmd) => config::handle_config(cmd, paths, json).await,
        Commands::System(cmd) => system::handle_system(cmd, paths, json).await,
        Commands::Daemon(cmd) => daemon::handle_daemon(cmd, paths, json).await,
        Commands::Cron(cmd) => cron::handle_cron(cmd, paths, json).await,
        Commands::Gateway(cmd) => gateway::handle_gateway(cmd, paths, json).await,
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
