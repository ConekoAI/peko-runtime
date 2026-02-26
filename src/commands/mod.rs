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
pub mod config;
pub mod cron;
pub mod daemon;
pub mod gateway;
pub mod session;
pub mod system;
pub mod tool;

use clap::{Parser, Subcommand};
use clap_complete::Shell;
use std::path::PathBuf;

/// Global CLI structure
#[derive(Parser)]
#[command(name = "pekobot")]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(about = "Lightweight multi-agent runtime")]
#[command(propagate_version = true)]
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
    
    /// Enable verbose logging
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    #[command(subcommand)]
    pub command: Commands,
}

/// Top-level commands
#[derive(Subcommand)]
pub enum Commands {
    /// Agent management commands
    #[command(subcommand)]
    Agent(agent::AgentCommands),
    
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
        let config_dir = cli.config_dir.clone()
            .or_else(|| dirs::config_dir().map(|d| d.join("pekobot")))
            .unwrap_or_else(|| PathBuf::from(".").join(".pekobot"));
            
        let data_dir = cli.data_dir.clone()
            .or_else(|| dirs::data_dir().map(|d| d.join("pekobot")))
            .unwrap_or_else(|| config_dir.clone());
            
        let cache_dir = cli.cache_dir.clone()
            .or_else(|| dirs::cache_dir().map(|d| d.join("pekobot")))
            .unwrap_or_else(|| data_dir.join("cache"));
        
        // Ensure directories exist
        let _ = std::fs::create_dir_all(&config_dir);
        let _ = std::fs::create_dir_all(&data_dir);
        let _ = std::fs::create_dir_all(&cache_dir);
        
        Self { config_dir, data_dir, cache_dir }
    }
    
    /// Get agents configuration directory
    #[must_use] 
    pub fn agents_dir(&self) -> PathBuf {
        self.config_dir.join("agents")
    }
    
    /// Get agent config file path
    #[must_use] 
    pub fn agent_config(&self, name: &str) -> PathBuf {
        self.agents_dir().join(format!("{name}.toml"))
    }
    
    /// Get tools directory
    #[must_use] 
    pub fn tools_dir(&self) -> PathBuf {
        self.data_dir.join("tools")
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
        0 => tracing::Level::INFO,
        1 => tracing::Level::DEBUG,
        _ => tracing::Level::TRACE,
    };
    
    tracing_subscriber::fmt()
        .with_max_level(level)
        .init();
}
