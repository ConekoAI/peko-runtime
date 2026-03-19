//! CLI Command Module
//!
//! This module contains all CLI subcommands for Pekobot.
//! Each submodule handles a specific command category:
//!
//! - `agent`: Agent lifecycle management
//! - `tool`: Tool registry and Pekohub integration
//! - `session`: Session management and introspection
//! - `config`: Configuration management
//! - `system`: System diagnostics and maintenance
//! - `daemon`: Daemon mode for cron job execution
//! - `cron`: Cron job scheduling
//! - `gateway`: Gateway plugin management

pub mod agent;
pub mod agent_bootstrap;
pub mod auth;
pub mod config;
pub mod cron;
pub mod daemon;
pub mod gateway;
pub mod identifier;
pub mod mcp;
pub mod orchestration;
pub mod provider;
pub mod send;
pub mod session;
pub mod system;
pub mod team;
pub mod tool;
pub mod update;

use clap::{Parser, Subcommand};
use clap_complete::Shell;
use std::path::PathBuf;

/// Global CLI structure
#[derive(Parser)]
#[command(name = "pekobot")]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(about = "Lightweight multi-agent runtime")]
#[command(propagate_version = true)]
#[command(after_help = "Examples:
  pekobot daemon start                          # Start the daemon
  pekobot team create myteam                    # Create a new team
  pekobot agent create myteam/my-agent --provider kimi  # Create agent in team
  pekobot agent init ./my-agent --provider openai  # Create new agent
  pekobot build ./my-agent/ -t my-agent:v1.0    # Build agent image
  pekobot run my-agent:v1.0                     # Run agent instance
  pekobot ps                                    # List instances
  pekobot session list myteam/my-agent          # List sessions
  pekobot send myteam/my-agent \"Hello\"         # Send message
")]
pub struct Cli {
    /// Configuration directory override
    #[arg(long, global = true, env = "PEKOBOT_CONFIG_DIR")]
    pub config_dir: Option<PathBuf>,

    /// Data directory override
    #[arg(long, global = true, env = "PEKOBOT_DATA_DIR")]
    pub data_dir: Option<PathBuf>,

    /// Cache directory override  
    #[arg(long, global = true, env = "PEKOBOT_CACHE_DIR")]
    pub cache_dir: Option<PathBuf>,

    /// Output results as JSON
    #[arg(long, global = true)]
    pub json: bool,

    /// Suppress non-error output
    #[arg(short, long, global = true)]
    pub quiet: bool,

    /// Enable verbose logging (-v=info, -vv=debug, -vvv=trace)
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Show debug information including stack traces
    #[arg(long, global = true, env = "PEKOBOT_DEBUG")]
    pub debug: bool,

    #[command(subcommand)]
    pub command: Commands,
}

/// Top-level commands
#[derive(Subcommand)]
pub enum Commands {
    /// Agent management commands
    #[command(subcommand)]
    Agent(agent::AgentCommands),

    /// Team management commands
    #[command(subcommand)]
    Team(team::TeamCommands),

    /// Send a message to an agent (unified command)
    ///
    /// This is the primary way to interact with agents. Examples:
    ///   pekobot send myagent "Hello"
    ///   pekobot send myagent --file prompt.txt
    ///   echo "Hello" | pekobot send myagent --stdin
    ///   pekobot send myagent "Hello" --session sess_xxx
    Send(send::SendArgs),

    /// Authentication and credential management
    #[command(subcommand)]
    Auth(auth::AuthCommands),

    /// Tool management commands (Pekohub integration)
    #[command(subcommand)]
    Tool(tool::ToolCommands),

    /// Session management commands
    #[command(subcommand)]
    Session(session::SessionCommands),

    /// Configuration management
    #[command(subcommand)]
    Config(config::ConfigCommands),

    /// System diagnostics and maintenance
    #[command(subcommand)]
    System(system::SystemCommands),

    /// Daemon management (for cron job execution)
    #[command(subcommand)]
    Daemon(daemon::DaemonCommands),

    /// Cron job management
    #[command(subcommand)]
    Cron(cron::CronCommands),

    /// Gateway plugin management
    #[command(subcommand)]
    Gateway(gateway::GatewayCommands),

    /// MCP (Model Context Protocol) server management
    #[command(subcommand)]
    Mcp(mcp::McpCommands),

    /// Orchestration layer management (event routing, webhooks, file watching)
    #[command(subcommand)]
    Orchestration(orchestration::OrchestrationCommands),

    /// LLM Provider management
    #[command(subcommand)]
    Provider(provider::ProviderCommands),

    /// Update Pekobot to the latest version
    Update {
        /// Only check for updates, don't install
        #[arg(long)]
        check: bool,

        /// Force update without confirmation
        #[arg(long)]
        force: bool,
    },

    /// Generate shell completions
    Completions {
        /// Shell to generate completions for
        #[arg(value_enum)]
        shell: Shell,
    },
}

/// Global paths helper
pub struct GlobalPaths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
}

impl GlobalPaths {
    /// Build paths from CLI arguments
    #[must_use]
    pub fn from_cli(cli: &Cli) -> Self {
        let config_dir = cli
            .config_dir
            .clone()
            .or_else(|| dirs::home_dir().map(|d| d.join(".pekobot")))
            .unwrap_or_else(|| PathBuf::from(".").join(".pekobot"));

        let data_dir = cli
            .data_dir
            .clone()
            .or_else(|| dirs::data_dir().map(|d| d.join("pekobot")))
            .unwrap_or_else(|| config_dir.clone());

        let cache_dir = cli
            .cache_dir
            .clone()
            .or_else(|| dirs::cache_dir().map(|d| d.join("pekobot")))
            .unwrap_or_else(|| data_dir.join("cache"));

        // Ensure directories exist
        let _ = std::fs::create_dir_all(&config_dir);
        let _ = std::fs::create_dir_all(&data_dir);
        let _ = std::fs::create_dir_all(&cache_dir);

        Self {
            config_dir,
            data_dir,
            cache_dir,
        }
    }

    /// Get teams configuration directory
    #[must_use]
    pub fn teams_dir(&self) -> PathBuf {
        self.config_dir.join("teams")
    }

    /// Get a specific team's directory
    #[must_use]
    pub fn team_dir(&self, team: &str) -> PathBuf {
        self.teams_dir().join(team)
    }

    /// Get agents directory for a specific team
    #[must_use]
    pub fn agents_dir(&self, team: Option<&str>) -> PathBuf {
        let team = team.unwrap_or("default");
        self.team_dir(team).join("agents")
    }

    /// Get agent config file path
    ///
    /// If team is None, uses the "default" team
    #[must_use]
    pub fn agent_config(&self, name: &str, team: Option<&str>) -> PathBuf {
        self.agents_dir(team).join(name).join("config.toml")
    }

    /// Get agent sessions directory
    #[must_use]
    pub fn agent_sessions_dir(&self, name: &str, team: Option<&str>) -> PathBuf {
        self.agents_dir(team).join(name).join("sessions")
    }

    /// Get tools directory
    #[must_use]
    pub fn tools_dir(&self) -> PathBuf {
        self.data_dir.join("tools")
    }

    /// Get MCP configuration file path
    #[must_use]
    pub fn mcp_config(&self) -> PathBuf {
        self.config_dir.join("mcp.toml")
    }
}

/// Initialize logging
pub fn init_logging(verbosity: u8, quiet: bool) {
    if quiet {
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::ERROR)
            .init();
        return;
    }

    let level = match verbosity {
        0 => tracing::Level::WARN,  // Default: only warnings and errors
        1 => tracing::Level::INFO,  // -v: info level
        2 => tracing::Level::DEBUG, // -vv: debug level
        _ => tracing::Level::TRACE, // -vvv: trace level
    };

    tracing_subscriber::fmt().with_max_level(level).init();
}
