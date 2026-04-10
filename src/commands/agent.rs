//! Agent Management Commands
#![allow(dead_code)]

use crate::commands::GlobalPaths;
use clap::Subcommand;

// New unified handlers
mod handlers;

/// Agent management subcommands
///
/// Examples:
///   # Initialize a new agent directory
///   pekobot agent init ./my-agent --provider openai
///
///   # Send a message to an agent (non-interactive only)
///   pekobot agent start my-agent --message "Hello"
///
///   # List all agents
///   pekobot agent list
///
///   # Create agent in a specific team
///   pekobot agent create myteam/my-agent --provider kimi
///   pekobot agent create my-agent --team myteam --provider kimi
#[derive(Subcommand)]
#[command(disable_version_flag = true)]
pub enum AgentCommands {
    /// Send a single message to an agent (non-interactive)
    ///
    /// **DEPRECATED:** Use `pekobot send <agent> <message>` instead.
    #[command(alias = "run")]
    Start {
        /// Agent name (defaults to 'peko' if not specified)
        name: Option<String>,
        /// Custom configuration file path (optional, defaults to ~/.pekobot/teams/default/agents/{name}/config.toml)
        #[arg(short, long)]
        config: Option<String>,
        /// LLM provider (openai, anthropic, ollama, kimi, `kimi_code`) - only used when creating default config
        #[arg(short, long, default_value = "minimax")]
        provider: String,
        /// Model name - only used when creating default config
        #[arg(short, long)]
        model: Option<String>,
        /// Database path for memory
        #[arg(long)]
        db: Option<String>,
        /// Message to send (required, non-interactive only)
        #[arg(short = 'M', long, required = true)]
        message: String,
        /// Start a new session (don't resume existing CLI session)
        #[arg(short, long)]
        new: bool,
    },

    /// List all configured agents
    List {
        /// Show detailed information
        #[arg(short, long)]
        long: bool,
    },

    /// Show detailed agent information
    Show {
        /// Agent name or team/agent format
        name: String,
        /// Team to look in (overrides team/ prefix if both provided)
        #[arg(short, long)]
        team: Option<String>,
    },

    /// Create a new agent
    Create {
        /// Agent name or team/agent format
        name: String,
        /// Team to create agent in (overrides team/ prefix if both provided)
        #[arg(short, long)]
        team: Option<String>,
        /// Provider to use
        #[arg(short, long, default_value = "minimax")]
        provider: String,
        /// Force overwrite if agent already exists (non-interactive)
        #[arg(short, long)]
        force: bool,
    },

    /// Remove an agent and its configuration
    #[command(alias = "delete")]
    Remove {
        /// Agent name or team/agent format
        name: String,
        /// Team to remove from (overrides team/ prefix if both provided)
        #[arg(short, long)]
        team: Option<String>,
        /// Also remove identity
        #[arg(long)]
        purge: bool,
        /// Skip confirmation
        #[arg(short, long)]
        force: bool,
    },

    /// Move/rename an agent
    #[command(alias = "rename")]
    Move {
        /// Current agent name or team/agent format
        old_name: String,
        /// New agent name (just the name, no team prefix)
        new_name: String,
        /// Team of the existing agent (overrides team/ prefix if both provided)
        #[arg(short, long)]
        team: Option<String>,
        /// Target team for cross-team move (optional)
        #[arg(long)]
        to_team: Option<String>,
    },

    /// Export agent to .agent package
    Export {
        /// Agent name or team/agent format
        #[arg(short, long)]
        name: String,
        /// Team to export from (overrides team/ prefix if both provided)
        #[arg(long)]
        team: Option<String>,
        /// Output path
        #[arg(short, long)]
        output: Option<String>,
    },

    /// Import agent from .agent package
    Import {
        /// Path to .agent file
        #[arg(short, long)]
        file: String,
        /// New name for imported agent
        #[arg(short, long)]
        name: Option<String>,
        /// Target team for imported agent
        #[arg(short, long)]
        team: Option<String>,
    },

    /// Inspect .agent package without importing
    Inspect {
        /// Path to .agent file
        file: String,
    },

    /// Initialize a new agent directory with minimal structure
    Init {
        /// Directory path to initialize (creates if doesn't exist)
        path: String,
        /// Agent name (defaults to directory name)
        #[arg(short, long)]
        name: Option<String>,
        /// Provider to use
        #[arg(short, long, default_value = "minimax")]
        provider: String,
        /// Model name
        #[arg(short, long)]
        model: Option<String>,
        /// Force overwrite if directory exists (non-interactive)
        #[arg(short, long)]
        force: bool,
    },

    /// Manage agent configuration
    #[command(subcommand)]
    Config(AgentConfigCommands),
}

/// Agent configuration subcommands
#[derive(Subcommand)]
#[command(disable_version_flag = true)]
pub enum AgentConfigCommands {
    /// Get a configuration value by dot-notation path
    Get {
        /// Agent name or team/agent format
        name: String,
        /// Configuration key path (e.g., "tools.enabled")
        key: String,
        /// Team to look in (overrides team/ prefix if both provided)
        #[arg(short, long)]
        team: Option<String>,
    },

    /// Set a configuration value by dot-notation path
    Set {
        /// Agent name or team/agent format
        name: String,
        /// Configuration key path (e.g., "tools.enabled")
        key: String,
        /// Value to set (JSON for arrays/objects, plain text for strings)
        value: String,
        /// Team to look in (overrides team/ prefix if both provided)
        #[arg(short, long)]
        team: Option<String>,
    },
}

/// Handle agent commands
pub async fn handle_agent(
    cmd: AgentCommands,
    paths: &GlobalPaths,
    json: bool,
) -> anyhow::Result<()> {
    match cmd {
        AgentCommands::Start {
            name,
            config,
            provider,
            model,
            db,
            message,
            new,
        } => {
            // DEPRECATED: Use `pekobot send` instead
            eprintln!("⚠️  Warning: 'pekobot agent start --message' is deprecated.");
            eprintln!("   Use 'pekobot send <agent> \"<message>\"' instead.");
            Ok(())
        }
        AgentCommands::List { long } => handlers::handle_agent_list(paths, long, json).await,
        AgentCommands::Show { name, team } => {
            handlers::handle_agent_show(paths, name, team, json).await
        }
        AgentCommands::Create {
            name,
            team,
            provider,
            force,
        } => handlers::handle_agent_create(paths, name, team, provider, force).await,
        AgentCommands::Remove {
            name,
            team,
            purge,
            force,
        } => handlers::handle_agent_remove(paths, name, team, purge, force, json).await,
        AgentCommands::Move {
            old_name,
            new_name,
            team,
            to_team,
        } => handlers::handle_agent_move(paths, old_name, new_name, team, to_team, json).await,
        AgentCommands::Export {
            name,
            team,
            output,
        } => handlers::handle_agent_export(paths, name, team, output).await,
        AgentCommands::Import { file, name, team } => {
            handlers::handle_agent_import(paths, file, name, team).await
        }
        AgentCommands::Inspect { file } => handlers::handle_agent_inspect(file, json).await,
        AgentCommands::Init {
            path,
            name,
            provider,
            model,
            force,
        } => handlers::handle_agent_init(paths, path, name, provider, model, force, json).await,
        AgentCommands::Config(cmd) => handlers::handle_agent_config(cmd, paths, json).await,
    }
}
