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
        #[arg(short, long, default_value = "kimi_code")]
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

    /// Create a new agent from template
    Create {
        /// Agent name or team/agent format
        name: String,
        /// Team to create agent in (overrides team/ prefix if both provided)
        #[arg(short, long)]
        team: Option<String>,
        /// Use template (minimal, coding, research, full)
        #[arg(short = 'T', long, default_value = "minimal")]
        template: String,
        /// Provider to use
        #[arg(short, long, default_value = "kimi_code")]
        provider: String,
        /// Force overwrite if agent already exists (non-interactive)
        #[arg(short, long)]
        force: bool,
    },

    /// Delete an agent and its configuration
    Delete {
        /// Agent name or team/agent format
        name: String,
        /// Team to delete from (overrides team/ prefix if both provided)
        #[arg(short, long)]
        team: Option<String>,
        /// Also delete identity
        #[arg(long)]
        purge: bool,
        /// Skip confirmation
        #[arg(short, long)]
        force: bool,
    },

    /// Rename an agent
    Rename {
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
        /// Encrypt with passphrase
        #[arg(long)]
        encrypt: bool,
    },

    /// Import agent from .agent package
    Import {
        /// Path to .agent file
        #[arg(short, long)]
        file: String,
        /// New name for imported agent
        #[arg(short, long)]
        name: Option<String>,
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
        #[arg(short, long, default_value = "kimi_code")]
        provider: String,
        /// Model name
        #[arg(short, long)]
        model: Option<String>,
        /// Force overwrite if directory exists (non-interactive)
        #[arg(short, long)]
        force: bool,
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
        AgentCommands::List { long } => {
            handlers::handle_agent_list(paths, long, json).await
        }
        AgentCommands::Show { name, team } => {
            handlers::handle_agent_show(paths, name, team, json).await
        }
        AgentCommands::Create {
            name,
            team,
            template,
            provider,
            force,
        } => {
            handlers::handle_agent_create(
                paths, name, team, template, provider, force,
            )
            .await
        }
        AgentCommands::Delete {
            name,
            team,
            purge,
            force,
        } => {
            handlers::handle_agent_delete(paths, name, team, purge, force)
                .await
        }
        AgentCommands::Rename {
            old_name,
            new_name,
            team,
            to_team,
        } => {
            handlers::handle_agent_rename(
                paths, old_name, new_name, team, to_team, json,
            )
            .await
        }
        AgentCommands::Export {
            name,
            team,
            output,
            encrypt,
        } => {
            handlers::handle_agent_export(
                paths, name, team, output, encrypt,
            )
            .await
        }
        AgentCommands::Import { file, name } => {
            handlers::handle_agent_import(paths, file, name).await
        }
        AgentCommands::Inspect { file } => {
            handlers::handle_agent_inspect(file, json).await
        }
        AgentCommands::Init {
            path,
            name,
            provider,
            model,
            force,
        } => {
            handlers::handle_agent_init(
                paths, path, name, provider, model, force, json,
            )
            .await
        }
    }
}
