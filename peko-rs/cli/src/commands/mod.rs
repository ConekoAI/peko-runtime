//! CLI Command Module
//!
//! This module contains all CLI subcommands for Peko.
//! Each submodule handles a specific command category:
//!
//! - `principal`: peko (top-level AI actor) lifecycle management.
//!   The verbs are flattened to the top level (`peko create`, ...).
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
pub mod quota;
pub mod runtime;
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
#[command(about = "Peko: persistent AI agents (pekos) you own")]
#[command(propagate_version = true)]
#[command(after_help = "Examples:
  peko daemon start                          # Start the daemon
  peko create my-peko                        # Create a new peko
  peko export my-peko -o my-peko.peko        # Export a peko package
  peko send my-peko \"Hello\"                  # Send message to a peko
  peko log my-peko                           # Read peko activity (owner-root view)
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

    #[command(subcommand)]
    pub command: Commands,
}

/// Top-level commands
#[derive(Subcommand)]
pub enum Commands {
    /// Create, list, move, and remove pekos
    ///
    /// A peko is an AI agent you own: its identity, memory, rules, and
    /// roles are files in one folder. These commands sit at the top level
    /// (`peko create`, `peko list`, `peko export`, ...).
    #[command(flatten)]
    Peko(principal::PrincipalCommands),

    /// Send a message to a peko
    ///
    /// The main way to talk to a peko. Examples:
    ///   peko send my-peko "Hello"
    ///   peko send my-peko --file prompt.txt
    ///   echo "Hello" | peko send my-peko --stdin
    Send(send::SendArgs),

    /// Stop what a peko is doing in your conversation
    ///
    /// The peko stops at its next step and the conversation shows
    /// `⏹ stopped by user`. If it isn't working on anything, this says so
    /// and exits 0. With `--peer <address>` (owner only), such as
    /// `user:hub:<id>`, stop it in someone else's conversation instead.
    Stop(stop::StopArgs),

    /// Read your conversation with a peko
    ///
    /// Shows the messages between you and the peko, or with `--peer` a
    /// conversation it had with someone else. Reading never wakes the peko.
    Log(log::LogCommand),

    /// Authentication and credential management
    #[command(subcommand)]
    Auth(auth::AuthCommands),

    /// Provider API key management (OS keychain backed)
    #[command(subcommand)]
    Credential(credential::CredentialCommands),

    /// Encrypted key vault (advanced)
    #[command(subcommand, hide = true)]
    Vault(vault::VaultCommands),

    /// Runtime settings files (advanced)
    #[command(subcommand, hide = true)]
    Config(config::ConfigCommands),

    /// System diagnostics and maintenance
    #[command(subcommand)]
    System(system::SystemCommands),

    /// Daemon management (the background process that runs your pekos)
    #[command(subcommand)]
    Daemon(daemon::DaemonCommands),

    /// Direct messages and group chats with people and pekos
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

    /// This computer's identity and PekoHub registration (advanced)
    #[command(subcommand, hide = true)]
    Runtime(runtime::RuntimeCommands),

    /// PekoHub connection (advanced)
    #[command(subcommand, hide = true)]
    Tunnel(tunnel::TunnelCommands),

    /// How many tokens and requests a peko may use
    ///
    /// Show or change a peko's input, output, and request limits per
    /// cycle, and its spending budget.
    ///
    /// Examples:
    ///   peko quota status my-peko
    ///   peko quota set my-peko --input 1000000 --output 500000 --cycle daily
    ///   peko quota reset my-peko
    #[command(subcommand)]
    Quota(quota::QuotaCommands),

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

    /// What your pekos did: the audit log and each peko's tool ledger
    ///
    ///   peko audit tail [--since 1h] [--peko NAME] [--limit N] [--follow]
    ///     Read the audit log files (kept across restarts).
    ///   peko audit list [--peko NAME]
    ///     Recent events since the daemon started.
    ///   peko audit ledger NAME / peko audit verify NAME
    ///     Every tool call a peko made, and a check that none was altered.
    ///
    /// Examples:
    ///   peko audit tail --since 30m
    ///   peko audit tail --peko alice --limit 20
    ///   peko audit list --type tool.call
    #[command(subcommand)]
    Audit(audit::AuditCommands),

    /// Print the runtime version
    ///
    /// Like `peko --version`, plus `--json` output for scripts and the
    /// desktop app.
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
    // The payload is only read by the unit tests below; production
    // callers match on the `Group` arm and use the original arg
    // string for the principal path.
    #[allow(dead_code)]
    Principal(String),
    /// Group slug (the part after `group:`).
    Group(String),
}

/// Split a recipient positional into principal vs group channel.
/// An actor address as people read it: a peko's `principal:local:<id>` /
/// `principal:hub:<id>` (the wire and storage form) shows as `peko:…`.
/// Every other text passes through unchanged; `--json` output keeps the
/// wire form. Both spellings are accepted wherever an address is typed.
pub(crate) fn shown_address(address: &str) -> String {
    // The local user is stored as `user:local:local`; people know it as
    // `user:local`.
    if address == "user:local:local" {
        return "user:local".to_string();
    }
    match address.strip_prefix("principal:") {
        // A local route is written bare (`principal:prin_x`); show its scope
        // as `peko show` does.
        Some(rest) if rest.starts_with("local:") || rest.starts_with("hub:") => {
            format!("peko:{rest}")
        }
        Some(rest) => format!("peko:local:{rest}"),
        None => address.to_string(),
    }
}

/// One channel event as a line for people (`peko channel peek`).
pub(crate) fn shown_channel_event(event: &peko_protocol::channel::ChannelEvent) -> String {
    use peko_protocol::channel::ChannelEvent;
    match event {
        ChannelEvent::Created {
            creator, name, at, ..
        } => format!("[{at}] {} created \"{name}\"", shown_address(creator)),
        ChannelEvent::Posted {
            author, text, at, ..
        } => format!("[{at}] {}: {text}", shown_address(author)),
        ChannelEvent::MemberJoined { member, at, .. } => {
            format!("[{at}] {} joined", shown_address(member))
        }
        ChannelEvent::MemberLeft { member, at, .. } => {
            format!("[{at}] {} left", shown_address(member))
        }
        ChannelEvent::Received { at, .. } => format!("[{at}] (delivery receipt)"),
    }
}

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
    let config_dir = cli.config_dir.clone().unwrap_or_else(default_config_dir);
    // ADR-081 D3: the terminal is always the runtime's local user.
    GlobalPaths::new(
        config_dir,
        cli.data_dir.clone().unwrap_or_else(default_data_dir),
        cli.cache_dir.clone().unwrap_or_else(default_cache_dir),
        "local".to_string(),
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
    #[test]
    fn addresses_show_pekos_as_peko() {
        assert_eq!(
            super::shown_address("principal:local:prin_x"),
            "peko:local:prin_x"
        );
        assert_eq!(super::shown_address("principal:hub:abc"), "peko:hub:abc");
        assert_eq!(
            super::shown_address("principal:prin_x"),
            "peko:local:prin_x"
        );
        assert_eq!(super::shown_address("user:hub:abc"), "user:hub:abc");
        assert_eq!(super::shown_address("public"), "public");
        assert_eq!(super::shown_address("user:local:local"), "user:local");
    }

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
