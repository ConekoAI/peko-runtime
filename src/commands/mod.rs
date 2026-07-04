//! CLI Command Module
//!
//! This module contains all CLI subcommands for Peko.
//! Each submodule handles a specific command category:
//!
//! - `principal`: Principal (top-level AI actor) lifecycle management
//! - `ext`: Extension management (tools, skills, MCP servers)
//! - `config`: Configuration management
//! - `system`: System diagnostics and maintenance
//! - `daemon`: Daemon mode for cron job execution

pub mod agent_bootstrap;
pub mod auth;
pub mod config;
pub mod credential;
pub mod cron;
pub mod daemon;
pub mod ext;
pub mod mcp;
pub mod principal;
pub mod provider;
pub mod registry;
pub mod runtime;
pub mod search;
pub mod send;
pub mod system;
pub mod tunnel;
pub mod vault;

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
  peko principal create myprincipal          # Create a new Principal
  peko principal export myprincipal -o myprincipal.principal  # Export Principal
  peko send myprincipal \"Hello\"             # Send message to a Principal
  peko principal agent list myprincipal      # List agents in a Principal
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
    /// Principal management commands (AI Principal container)
    #[command(subcommand)]
    Principal(principal::PrincipalCommands),

    /// Send a message to a Principal (unified command)
    ///
    /// This is the primary way to interact with a Principal. Examples:
    ///   peko send myprincipal "Hello"
    ///   peko send myprincipal --file prompt.txt
    ///   echo "Hello" | peko send myprincipal --stdin
    ///   peko send myprincipal "Hello" --no-stream
    Send(send::SendArgs),

    /// Authentication and credential management
    #[command(subcommand)]
    Auth(auth::AuthCommands),

    /// Provider API key management (OS keychain backed)
    #[command(subcommand)]
    Credential(credential::CredentialCommands),

    /// Vault management (advanced / hidden)
    #[command(subcommand, hide = true)]
    Vault(vault::VaultCommands),

    /// Extension management commands (skills, MCP, tools, channels, hooks)
    #[command(subcommand)]
    Ext(ext::ExtCommands),

    /// Configuration management (advanced / hidden)
    #[command(subcommand, hide = true)]
    Config(config::ConfigCommands),

    /// System diagnostics and maintenance
    #[command(subcommand)]
    System(system::SystemCommands),

    /// Daemon management (for cron job execution)
    #[command(subcommand)]
    Daemon(daemon::DaemonCommands),

    /// Cron job management (advanced / hidden)
    #[command(subcommand, hide = true)]
    Cron(cron::CronCommands),

    /// LLM Provider management
    #[command(subcommand)]
    Provider(provider::ProviderCommands),

    /// Search the PekoHub registry for principals and extensions
    #[command(subcommand)]
    Search(search::SearchCommands),

    /// Registry management (advanced / hidden)
    #[command(subcommand, hide = true)]
    Registry(registry::RegistryCommands),

    /// Runtime identity and registry management (advanced / hidden)
    #[command(subcommand, hide = true)]
    Runtime(runtime::RuntimeCommands),

    /// PekoHub tunnel management (advanced / hidden)
    #[command(subcommand, hide = true)]
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

    /// Update Peko to the latest version
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

    /// Get the principals configuration directory
    #[must_use]
    pub fn principals_root_dir(&self) -> PathBuf {
        self.resolver.principals_root_dir()
    }

    /// Get a specific principal's directory
    #[must_use]
    pub fn principal_dir(&self, principal: &str) -> PathBuf {
        self.resolver.principal_dir(principal)
    }

    /// Get principal config file path
    #[must_use]
    pub fn principal_config(&self, principal: &str) -> PathBuf {
        self.resolver.principal_config(principal)
    }

    /// Get principal agent prompts directory
    #[must_use]
    pub fn principal_agents_dir(&self, principal: &str) -> PathBuf {
        self.resolver.principal_agents_dir(principal)
    }

    /// Get principal memory directory
    #[must_use]
    pub fn principal_memory_dir(&self, principal: &str) -> PathBuf {
        self.resolver.principal_memory_dir(principal)
    }

    /// Get principal sessions directory
    #[must_use]
    pub fn principal_sessions_dir(&self, principal: &str) -> PathBuf {
        self.resolver.principal_sessions_dir(principal)
    }

    /// Get agent config file path
    #[must_use]
    pub fn agent_config(&self, name: &str) -> PathBuf {
        self.resolver.agent_config(name)
    }

    /// Get agent sessions directory
    #[must_use]
    pub fn agent_sessions_dir(&self, name: &str) -> PathBuf {
        self.resolver.agent_sessions_dir(name)
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
    /// Format: `<data_dir>/workspaces/<agent>/personal`
    ///
    /// # Arguments
    /// * `agent` - The agent name
    #[must_use]
    pub fn agent_workspace(&self, agent: &str) -> PathBuf {
        self.resolver.agent_workspace(agent)
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
