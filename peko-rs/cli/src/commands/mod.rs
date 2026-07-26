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

pub mod auth;
pub mod capability;
pub mod config;
pub mod credential;
pub mod cron;
pub mod daemon;
pub mod ext;
pub mod interrupt;
pub mod log;
pub mod mcp;
pub mod model;
pub mod principal;
pub mod quota;
pub mod registry;
pub mod runtime;
pub mod search;
pub mod send;
pub mod system;
pub mod tunnel;
pub mod vault;
pub mod version;

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
  peko log myprincipal                       # Read principal activity (owner-root view)
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

    /// Soft-interrupt or steer a running `peko send --stream` run.
    ///
    /// The `request_id` is the integer printed to stderr by
    /// `peko send --stream` at start. Use `--steer "text"` to inject
    /// a new user turn into the run's session inbox instead of
    /// cancelling it.
    Interrupt(interrupt::InterruptArgs),

    /// Read a Principal's activity (owner-root view by default)
    ///
    /// There is no `peko session` command and there will never be one;
    /// this command is the only user-facing way to inspect a Principal's
    /// working state without running a turn.
    Log(log::LogCommand),

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

    /// Capability authority management commands (grant, revoke, list)
    #[command(subcommand)]
    Capability(capability::CapabilityCommands),

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

    /// LLM model management (runtime model catalog)
    #[command(subcommand)]
    Model(model::ModelCommands),

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

    /// Per-principal token quota management (F18)
    ///
    /// Inspect or replace a Principal's input / output / request
    /// limits. The daemon owns the live counters; the CLI is a thin
    /// IPC client.
    ///
    /// Examples:
    ///   peko quota status myprincipal
    ///   peko quota set myprincipal --input 1000000 --output 500000 --cycle daily
    ///   peko quota reset myprincipal
    #[command(subcommand)]
    Quota(quota::QuotaCommands),

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

    /// Print the runtime version
    ///
    /// Distinct from `peko --version` (handled by clap). This subcommand
    /// exists for programmatic consumption — notably peko-desktop's
    /// SidecarSupervisor (ADR-043) — and supports `--json` output.
    ///
    /// Examples:
    ///   peko version
    ///   peko version --json
    Version(version::VersionArgs),
}

// `GlobalPaths` lives in `peko_core::common::paths` (Phase 0.Z-B pre-flight)
// so the core lib (specifically `credentials_service`) and the CLI satellite
// can both reference it without a circular dep — `commands/` moves to the
// `peko-cli` crate in Phase 0.Z-B but core can't depend on it. Re-export here
// so the 21 command files that say `use crate::commands::GlobalPaths` keep
// compiling unchanged.
pub use peko_core::common::GlobalPaths;

/// Build a [`GlobalPaths`] from a parsed [`Cli`] argument struct.
///
/// Lives in the CLI crate (here in `commands/mod.rs`, then in
/// `peko-rs/cli/src/commands/mod.rs` after Phase 0.Z-B) because it's the
/// only caller that holds a `Cli`. Resolution order follows the docs on
/// [`peko_core::common::GlobalPaths::new`].
#[must_use]
pub fn from_cli(cli: &Cli) -> GlobalPaths {
    use peko_core::common::paths::{default_cache_dir, default_config_dir, default_data_dir};
    GlobalPaths::new(
        cli.config_dir.clone().unwrap_or_else(default_config_dir),
        cli.data_dir.clone().unwrap_or_else(default_data_dir),
        cli.cache_dir.clone().unwrap_or_else(default_cache_dir),
        cli.user.clone().unwrap_or_else(|| "local".to_string()),
    )
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
