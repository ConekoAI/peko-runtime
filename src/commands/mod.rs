//! CLI Command Module
//!
//! This module contains all CLI subcommands for Pekobot.
//! Each submodule handles a specific command category:
//!
//! - `agent`: Agent lifecycle management
//! - `ext`: Extension management (tools, skills, MCP servers)
//! - `session`: Session management and introspection
//! - `config`: Configuration management
//! - `system`: System diagnostics and maintenance
//! - `daemon`: Daemon mode for cron job execution
//! - `cron`: Cron job scheduling

pub mod agent;
pub mod agent_bootstrap;
pub mod auth;
pub mod config;
pub mod credential;
pub mod cron;
pub mod daemon;
pub mod ext;
pub mod orchestration;
pub mod provider;
pub mod registry;
pub mod runtime;
pub mod search;
pub mod send;
pub mod session;
pub mod system;
pub mod team;
pub mod tunnel;

pub mod update;

use clap::{Parser, Subcommand};
use clap_complete::Shell;
use std::path::PathBuf;

/// Global CLI structure
#[derive(Parser)]
#[command(name = "peko")]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(about = "Lightweight multi-agent runtime")]
#[command(propagate_version = true)]
#[command(after_help = "Examples:
  peko daemon start                          # Start the daemon
  peko team create myteam                    # Create a new team
  peko agent create myteam/my-agent --provider kimi  # Create agent in team
  peko agent export my-agent -o my-agent.agent   # Export agent to package
  peko run my-agent:v1.0                         # Run agent instance
  peko ps                                    # List instances
  peko session list myteam/my-agent          # List sessions
  peko send myteam/my-agent \"Hello\"         # Send message
")]
pub struct Cli {
    /// Configuration directory override
    #[arg(long, global = true, env = "PEKO_CONFIG_DIR")]
    pub config_dir: Option<PathBuf>,

    /// Data directory override
    #[arg(long, global = true, env = "PEKO_DATA_DIR")]
    pub data_dir: Option<PathBuf>,

    /// Cache directory override  
    #[arg(long, global = true, env = "PEKO_CACHE_DIR")]
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
    #[arg(long, global = true, env = "PEKO_DEBUG")]
    pub debug: bool,

    /// User identifier for session isolation
    #[arg(short = 'U', long, global = true)]
    pub user: Option<String>,

    /// Default registry URL for push/pull commands
    #[arg(long, global = true, env = "PEKO_REGISTRY")]
    pub registry: Option<String>,

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
    ///   peko send myagent "Hello"
    ///   peko send myagent --file prompt.txt
    ///   echo "Hello" | peko send myagent --stdin
    ///   peko send myagent "Hello" --session `sess_xxx`
    Send(send::SendArgs),

    /// Authentication and credential management
    #[command(subcommand)]
    Auth(auth::AuthCommands),

    /// Provider API key management (OS keychain backed)
    #[command(subcommand)]
    Credential(credential::CredentialCommands),

    /// Extension management commands (skills, MCP, tools, channels, hooks)
    #[command(subcommand)]
    Ext(ext::ExtCommands),

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

    /// Orchestration layer management (event routing, webhooks, file watching)
    #[command(subcommand)]
    Orchestration(orchestration::OrchestrationCommands),

    /// LLM Provider management
    #[command(subcommand)]
    Provider(provider::ProviderCommands),

    /// Search the PekoHub registry for agents, teams, and extensions
    #[command(subcommand)]
    Search(search::SearchCommands),

    /// Registry management (set-default, get-default, list)
    #[command(subcommand)]
    Registry(registry::RegistryCommands),

    /// Runtime identity and registry management (ADR-032)
    #[command(subcommand)]
    Runtime(runtime::RuntimeCommands),

    /// PekoHub tunnel management (ADR-035)
    #[command(subcommand)]
    Tunnel(tunnel::TunnelCommands),

    /// Log in to the PekoHub registry
    Login {
        /// Registry host (default: from config or pekohub.ai)
        #[arg(long)]
        registry: Option<String>,
        /// API key for authentication
        #[arg(long)]
        api_key: Option<String>,
    },

    /// Log out from the PekoHub registry
    Logout {
        /// Registry host to log out from (default: from config or pekohub.ai)
        #[arg(long)]
        registry: Option<String>,
    },

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
///
/// This struct wraps the common `PathResolver` to provide CLI-specific
/// path resolution. It delegates all path operations to the underlying
/// `PathResolver` for consistency with the API.
#[derive(Clone, Debug)]
pub struct GlobalPaths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
    resolver: crate::common::paths::PathResolver,
    services: crate::common::services::ServiceContainer,
    user: String,
}

impl GlobalPaths {
    /// Build paths from CLI arguments
    ///
    /// Path resolution order (highest to lowest precedence):
    ///   1. Explicit `--config-dir` / `--data-dir` / `--cache-dir` CLI args
    ///   2. The `PEKO_HOME` env var (delegated to `default_config_dir` /
    ///      `default_data_dir` so this matches what the rest of the codebase
    ///      — and external tools like the daemon's IPC layer — expect)
    ///   3. The XDG defaults (`~/.peko` for config, `~/.local/share/peko`
    ///      for data on Linux)
    ///
    /// Before this was fixed, the fallback was `dirs::home_dir()` directly,
    /// which silently bypassed `PEKO_HOME` and made the daemon's
    /// `data_dir` (used for `cron.json`, `announcements/`, agent state)
    /// resolve to the host default even when callers set `PEKO_HOME` to
    /// isolate the daemon in a tempdir. Caught by `tests/cli_cron.rs`'s
    /// `cron_announce_writes_file_on_run` (announcement file was being
    /// written to the host's `~/.local/share/peko/announcements/`, not
    /// the test tempdir).
    #[must_use]
    pub fn from_cli(cli: &Cli) -> Self {
        let config_dir = cli
            .config_dir
            .clone()
            .unwrap_or_else(crate::common::paths::default_config_dir);

        let data_dir = cli
            .data_dir
            .clone()
            .unwrap_or_else(crate::common::paths::default_data_dir);

        let cache_dir = cli
            .cache_dir
            .clone()
            .unwrap_or_else(crate::common::paths::default_cache_dir);

        // Ensure directories exist
        let _ = std::fs::create_dir_all(&config_dir);
        let _ = std::fs::create_dir_all(&data_dir);
        let _ = std::fs::create_dir_all(&cache_dir);

        let resolver = crate::common::paths::PathResolver::with_dirs(
            config_dir.clone(),
            data_dir.clone(),
            cache_dir.clone(),
        );

        let services = crate::common::services::ServiceContainer::new(resolver.clone());

        let user = cli.user.clone().unwrap_or_else(|| "default".to_string());

        Self {
            config_dir,
            data_dir,
            cache_dir,
            resolver,
            services,
            user,
        }
    }

    /// Get the underlying path resolver
    #[must_use]
    pub fn resolver(&self) -> &crate::common::paths::PathResolver {
        &self.resolver
    }

    /// Get the service container
    #[must_use]
    pub fn services(&self) -> &crate::common::services::ServiceContainer {
        &self.services
    }

    /// Get teams configuration directory
    #[must_use]
    pub fn teams_dir(&self) -> PathBuf {
        self.resolver.teams_dir()
    }

    /// Get a specific team's directory
    #[must_use]
    pub fn team_dir(&self, team: &str) -> PathBuf {
        self.resolver.team_dir(team)
    }

    /// Get the top-level agents root directory
    #[must_use]
    pub fn agents_root_dir(&self) -> PathBuf {
        self.resolver.agents_root_dir()
    }

    /// Get a specific agent's directory
    #[must_use]
    pub fn agent_dir(&self, agent: &str) -> PathBuf {
        self.resolver.agent_dir(agent)
    }

    /// Get agent config file path
    #[must_use]
    pub fn agent_config(&self, name: &str) -> PathBuf {
        self.resolver.agent_config(name)
    }

    /// Get agent sessions directory
    #[must_use]
    pub fn agent_sessions_dir(&self, name: &str, team: Option<&str>) -> PathBuf {
        self.resolver.agent_sessions_dir(name, team)
    }

    /// Get tools directory
    #[must_use]
    pub fn tools_dir(&self) -> PathBuf {
        self.resolver.tools_dir()
    }

    /// Get MCP configuration file path
    #[must_use]
    pub fn mcp_config(&self) -> PathBuf {
        self.resolver.mcp_config()
    }

    /// Get agent workspace directory
    ///
    /// Returns the path to an agent's workspace directory.
    /// Format: `<data_dir>/workspaces/<agent>/<context>`
    ///
    /// # Arguments
    /// * `agent` - The agent name
    /// * `team` - Optional team context (defaults to "personal")
    #[must_use]
    pub fn agent_workspace(&self, agent: &str, team: Option<&str>) -> PathBuf {
        self.resolver.agent_workspace(agent, team)
    }

    /// Get agent workspace directory with explicit team
    ///
    /// Same as `agent_workspace` but requires an explicit team.
    /// This is useful when the team is already known.
    ///
    /// # Arguments
    /// * `agent` - The agent name
    /// * `team` - The team name
    #[must_use]
    pub fn agent_workspace_with_team(&self, agent: &str, team: &str) -> PathBuf {
        self.resolver.agent_workspace_with_team(agent, team)
    }

    /// Get the user identifier for session isolation
    #[must_use]
    pub fn user(&self) -> &str {
        &self.user
    }

    /// Load registry configuration from the config directory
    ///
    /// Reads `[registry]` section from `~/.peko/config.toml`,
    /// falling back to defaults if the file or section doesn't exist.
    #[must_use]
    pub fn registry_config(&self) -> crate::registry::config::RegistryConfig {
        crate::registry::config::load_from_config_dir(&self.config_dir)
    }

    /// Get the runtime directory
    #[must_use]
    pub fn runtime_dir(&self) -> PathBuf {
        self.resolver.runtime_dir()
    }

    /// Get the runtime identity file path
    #[must_use]
    pub fn runtime_identity(&self) -> PathBuf {
        self.resolver.runtime_identity()
    }

    /// Get the runtime metadata file path
    #[must_use]
    pub fn runtime_metadata(&self) -> PathBuf {
        self.resolver.runtime_metadata()
    }

    /// Get the known runtimes file path
    #[must_use]
    pub fn known_runtimes(&self) -> PathBuf {
        self.resolver.known_runtimes()
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
