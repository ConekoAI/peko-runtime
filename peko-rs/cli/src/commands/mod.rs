//! CLI Command Module
//!
//! This module contains all CLI subcommands for Peko.
//! Each submodule handles a specific command category:
//!
//! - `principal`: Principal (top-level AI actor) lifecycle management
//! - `ext`: Extension management (tools, skills, MCP servers)
//! - `config`: Configuration management
//! - `system`: System diagnostics and maintenance
//! - `daemon`: Long-running daemon mode (cron engine + IPC server)

pub mod audit;
pub mod auth;
pub mod channel;
pub mod config;
pub mod credential;
// 2026-08-25: `peko cron` retired. The cron module was deleted;
// principals manage schedules through the `tool:Cron*` grants.
pub mod daemon;
pub mod log;
pub mod model;
pub mod principal;
pub mod principal_workspace;
pub mod quota;
pub mod registry;
pub mod runtime;
pub mod search;
pub mod send;
pub mod stop;
pub mod system;
pub mod tunnel;
pub mod vault;
pub mod version;

pub mod update;

use clap::{Parser, Subcommand};
use clap_complete::Shell;
use std::io::IsTerminal;
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

    /// Soft-stop the running turn on your thread with a Principal.
    ///
    /// The run cancels at the next agentic boundary and a
    /// `⏹ stopped by user` marker is posted to the thread. Idempotent:
    /// with no run in flight it reports "no running turn" and exits 0.
    /// Use `--peer user:<id>` (owner only) to stop another peer's
    /// thread.
    Stop(stop::StopArgs),

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

    /// Configuration management (advanced / hidden)
    #[command(subcommand, hide = true)]
    Config(config::ConfigCommands),

    /// System diagnostics and maintenance
    #[command(subcommand)]
    System(system::SystemCommands),

    /// Daemon management (for cron job execution)
    #[command(subcommand)]
    Daemon(daemon::DaemonCommands),

    /// Multi-principal chat primitive (channels) — read/create/post
    /// events across principals.
    ///
    /// Examples:
    ///   peko channel create alice "team alpha"
    ///   peko channel invite <chan_id> alice bob
    ///   peko channel post <chan_id> alice "hello"
    ///   peko channel peek <chan_id>
    ///   peko channel members <chan_id>
    #[command(subcommand)]
    Channel(channel::ChannelCommands),

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

    /// Audit log: tail the JSONL or list in-memory events (ADR-046).
    ///
    /// Two subcommands:
    ///   peko audit tail [--since 1h] [--principal foo] [--limit N] [--follow]
    ///     Reads `<data_dir>/runtime/audit/audit-YYYY-MM-DD.jsonl`
    ///     directly. Survives daemon restarts. `--follow` is
    ///     single-file (today's file).
    ///   peko audit list [--principal foo]
    ///     Sends an IPC query to the daemon and reads the in-memory
    ///     ring buffer. Fast path for "what just happened this
    ///     session"; never touches disk.
    ///
    /// Examples:
    ///   peko audit tail --since 30m
    ///   peko audit tail --principal alice --limit 20
    ///   peko audit list --type principal.config_drift
    #[command(subcommand)]
    Audit(audit::AuditCommands),

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

/// Recipient of `peko send` / `peko log` / `peko stop`: a principal
/// name, or a group channel in the `group:<slug>` wire form. Bare
/// `chan_<id>` channels are not recipient sugar — they stay on the
/// `peko channel` surface.
pub(crate) enum Recipient {
    Principal(String),
    /// Group slug (the part after `group:`).
    Group(String),
}

/// Split a recipient positional into principal vs group channel.
pub(crate) fn parse_recipient(s: &str) -> Recipient {
    match s.strip_prefix("group:") {
        Some(slug) if !slug.is_empty() => Recipient::Group(slug.to_string()),
        _ => Recipient::Principal(s.to_string()),
    }
}

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
    // Strip ANSI escape codes when stderr isn't a terminal — otherwise
    // redirects / pipes capture literal `\x1b[…m` sequences that
    // pollute log files (Bug C, filed 2026-08-01 v2).
    let ansi = std::io::stderr().is_terminal();
    if quiet {
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::ERROR)
            .with_ansi(ansi)
            .init();
        return;
    }

    let level = match verbosity {
        0 => tracing::Level::WARN,  // Default: only warnings and errors
        1 => tracing::Level::INFO,  // -v: info level
        2 => tracing::Level::DEBUG, // -vv: debug level
        _ => tracing::Level::TRACE, // -vvv: trace level
    };

    tracing_subscriber::fmt()
        .with_max_level(level)
        .with_ansi(ansi)
        .init();
}

#[cfg(test)]
mod tests {
    use super::{parse_recipient, Recipient};

    #[test]
    fn parse_recipient_splits_group_prefix() {
        assert!(matches!(
            parse_recipient("group:eng-standup"),
            Recipient::Group(slug) if slug == "eng-standup"
        ));
        assert!(matches!(
            parse_recipient("scout"),
            Recipient::Principal(name) if name == "scout"
        ));
        // Bare `group:` with an empty slug is not a group recipient.
        assert!(matches!(
            parse_recipient("group:"),
            Recipient::Principal(name) if name == "group:"
        ));
        // Bare `chan_<id>` forms stay principal-positioned (no sugar).
        assert!(matches!(
            parse_recipient("chan_abcdefgh"),
            Recipient::Principal(name) if name == "chan_abcdefgh"
        ));
    }
}
