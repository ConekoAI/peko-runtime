// Noise lint, consistent with the root crate's curated allow-list.
#![allow(clippy::too_many_arguments)]

use clap::Parser;
use clap_complete::generate;
use std::io::Write;

/// `peko` runtime version, lifted from a crate root constant so the CLI can
/// answer `peko version`, `peko update --check`, and the F33/F38 startup
/// banner without plumbing `CARGO_PKG_VERSION` through `Cli`. Defined
/// here because the cli crate owns the user-facing version surface.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The release this binary was built as, for people to read: `0.1.0-rc.6`
/// for a release candidate, where [`VERSION`] says only `0.1.0`.
///
/// The release workflow passes the tag as `PEKO_RELEASE_VERSION` at build
/// time. A build without it (a source build) or with a value that doesn't
/// belong to this crate version falls back to [`VERSION`]. Machine
/// consumers (the daemon's `PEKO_VERSION=` line, IPC status, snapshot
/// manifests) keep the crate version.
pub fn release_version() -> &'static str {
    release_version_from(option_env!("PEKO_RELEASE_VERSION"), VERSION)
}

fn release_version_from(tag: Option<&'static str>, crate_version: &'static str) -> &'static str {
    let Some(tag) = tag else {
        return crate_version;
    };
    let tag = tag.trim();
    let tag = tag.strip_prefix('v').unwrap_or(tag);
    // `0.1.0` or `0.1.0-<pre>` / `0.1.0+<build>`; anything else is not this version.
    match tag.strip_prefix(crate_version) {
        Some("") => crate_version,
        Some(rest) if rest.starts_with('-') || rest.starts_with('+') => tag,
        _ => crate_version,
    }
}

#[cfg(test)]
mod release_version_tests {
    use super::release_version_from;

    #[test]
    fn a_candidate_tag_names_the_candidate() {
        assert_eq!(
            release_version_from(Some("v0.1.0-rc.6"), "0.1.0"),
            "0.1.0-rc.6"
        );
        assert_eq!(
            release_version_from(Some("0.1.0-rc.6"), "0.1.0"),
            "0.1.0-rc.6"
        );
    }

    #[test]
    fn no_tag_or_a_foreign_tag_falls_back_to_the_crate_version() {
        assert_eq!(release_version_from(None, "0.1.0"), "0.1.0");
        assert_eq!(release_version_from(Some(""), "0.1.0"), "0.1.0");
        assert_eq!(release_version_from(Some("v0.1.0"), "0.1.0"), "0.1.0");
        assert_eq!(release_version_from(Some("v0.1.01"), "0.1.0"), "0.1.0");
        assert_eq!(release_version_from(Some("v0.2.0-rc.1"), "0.1.0"), "0.1.0");
        assert_eq!(release_version_from(Some("master"), "0.1.0"), "0.1.0");
    }
}

use crate::commands::{
    audit, auth, channel, config, credential, daemon, from_cli, init_logging, log, model,
    principal, quota, runtime, send, stop, system, tunnel, update, vault, version, Cli, Commands,
    GlobalPaths,
};

// `peko-rs/cli/` is a binary-only crate (no `src/lib.rs`), so the
// `commands/` module must be declared here in the binary entry point.
// Phase 0.Z-B: this module used to live in `peko_core::commands` (root lib);
// after the lift it lives in the cli crate itself.
mod commands;
mod summary;
#[cfg(test)]
mod test_support;

/// Peko - Lightweight Multi-Agent Runtime
#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // Initialize logging
    init_logging(cli.verbose, cli.quiet);

    // Set up global paths
    let paths = from_cli(&cli);

    // No global `ToolingRuntime` is pre-installed here (2026-09-27,
    // ADR-063 (P0-1) + D1): the CLI never executes tools
    // (ADR-021), so the core's async router had no live consumer on
    // this side, and the pre-installed core actively *pre-empted* the
    // correctly-wired one `AppState::new` builds for in-process daemon
    // runs (`peko daemon start --foreground`) — the daemon then reused
    // a router whose executor pushed completions into a standalone
    // inbox registry nothing drains. `AppState::new` installs the
    // daemon's own global core with the shared inbox registry.

    // Run the command and handle results/exit codes
    let result = run_command(cli.command, &paths, cli.json).await;

    match result {
        Ok(()) => std::process::exit(0),
        Err(e) => {
            // Print error message
            if cli.debug {
                // With --debug, show full indented error chain and backtrace
                // if available.
                eprintln!("❌ Error: {:?}", e);
            } else {
                // Default: print the error with the `:#` Display form so the
                // `Caused by:` chain reaches stdout without --debug. The top
                // level alone (e.g. "failed to load credential vault") is
                // unactionable for non-technical testers; the underlying
                // causes carry the actual instruction ("set
                // PEKO_MASTER_PASSPHRASE", "PEKO_UNLOCK_METHOD does not
                // match the vault's current mode", etc.).
                eprintln!("❌ Error: {:#}", e);
            }

            std::process::exit(1);
        }
    }
}

async fn run_command(command: Commands, paths: &GlobalPaths, json: bool) -> anyhow::Result<()> {
    match command {
        // ADR-059: the lifecycle verbs are top level (`peko create`, ...).
        Commands::Peko(cmd) => principal::handle_principal(cmd, paths, json).await,
        Commands::Send(args) => send::handle_send(args, paths).await,
        Commands::Stop(args) => stop::handle_stop(args, paths).await,
        Commands::Log(cmd) => log::handle_log(cmd, paths, json).await,
        Commands::Auth(cmd) => auth::handle_auth(cmd, paths, json).await,
        Commands::Credential(cmd) => credential::execute(cmd, paths).await,
        Commands::Vault(cmd) => vault::execute(cmd, paths).await,
        Commands::Config(cmd) => config::handle_config(cmd, paths, json).await,
        Commands::System(cmd) => system::handle_system(cmd, paths, json).await,
        Commands::Daemon(cmd) => daemon::handle_daemon(cmd, paths, json).await,
        Commands::Channel(cmd) => channel::handle_channel(cmd, paths).await,
        Commands::Model(cmd) => model::execute(cmd, paths).await,
        Commands::Runtime(cmd) => runtime::handle_runtime(cmd, paths, json).await,
        Commands::Tunnel(cmd) => tunnel::handle_tunnel(cmd, paths, json).await,
        Commands::Quota(cmd) => quota::handle_quota(cmd, paths, json).await,
        Commands::Audit(cmd) => audit::handle_audit(cmd, paths).await,
        Commands::Update { check, force } => update::handle_update(check, force).await,
        Commands::Completions { shell } => {
            // Render to a buffer first, then write to stdout. This keeps
            // `peko completions <shell> | head` from panicking: a
            // downstream SIGPIPE becomes a soft BrokenPipe on the
            // final `write_all` (which we silently swallow), instead
            // of bubbling up through clap_complete and crashing with
            // a stack trace (see e2e/reports/2026-08-01-non-technical-user-field-test.md
            // — "Bug 1: completions BrokenPipe panic").
            let mut cmd = <Cli as clap::CommandFactory>::command();
            let name = cmd.get_name().to_string();
            let mut buf: Vec<u8> = Vec::new();
            generate(shell, &mut cmd, name, &mut buf);
            match std::io::stdout().write_all(&buf) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
                Err(e) => Err(e.into()),
            }
        }
        Commands::Version(args) => version::handle_version(&args, json),
    }
}
