use clap::Parser;
use clap_complete::generate;
use pekobot::types::config::PekobotConfig;
use pekobot::commands::{
    agent, auth, config, cron, daemon, gateway, init_logging, mcp, orchestration, provider, session,
    system, tool, update, Cli, Commands, GlobalPaths,
};

/// Pekobot - Lightweight Multi-Agent Runtime
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Initialize logging
    init_logging(cli.verbose, cli.quiet);

    // Set up global paths
    let paths = GlobalPaths::from_cli(&cli);

    match cli.command {
        Commands::Agent(cmd) => agent::handle_agent(cmd, &paths, cli.json).await,
        Commands::Auth(cmd) => auth::handle_auth(cmd, &paths, cli.json).await,
        Commands::Tool(cmd) => tool::handle_tool(cmd, &paths, cli.json).await,
        Commands::Session(cmd) => session::handle_session(cmd, &paths, cli.json).await,
        Commands::Config(cmd) => config::handle_config(cmd, &paths, cli.json).await,
        Commands::System(cmd) => system::handle_system(cmd, &paths, cli.json).await,
        Commands::Daemon(cmd) => daemon::handle_daemon(cmd, &paths, cli.json).await,
        Commands::Cron(cmd) => cron::handle_cron(cmd, &paths, cli.json).await,
        Commands::Gateway(cmd) => gateway::handle_gateway(cmd, &paths, cli.json).await,
        Commands::Mcp(cmd) => mcp::handle(cmd, paths.mcp_config()).await,
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
