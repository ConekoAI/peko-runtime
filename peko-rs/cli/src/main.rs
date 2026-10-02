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

use crate::commands::{
    audit, auth, channel, config, credential, daemon, from_cli, init_logging, log, model,
    principal, quota, registry, runtime, search, send, stop, system, tunnel, update, vault,
    version, Cli, Commands, GlobalPaths,
};

// `peko-rs/cli/` is a binary-only crate (no `src/lib.rs`), so the
// `commands/` module must be declared here in the binary entry point.
// Phase 0.Z-B: this module used to live in `peko_core::commands` (root lib);
// after the lift it lives in the cli crate itself.
mod commands;
mod summary;

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
    let cli_registry = cli.registry.as_deref();
    let result = run_command(cli.command, &paths, cli.json, cli_registry).await;

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

async fn run_command(
    command: Commands,
    paths: &GlobalPaths,
    json: bool,
    _cli_registry: Option<&str>,
) -> anyhow::Result<()> {
    match command {
        // ADR-059: the lifecycle verbs are top level (`peko create`, ...).
        Commands::Peko(cmd) => principal::handle_principal(cmd, paths, json).await,
        Commands::Send(args) => send::handle_send(args, paths).await,
        Commands::Stop(args) => stop::handle_stop(args, paths).await,
        Commands::Log(cmd) => log::handle_log(cmd, paths, json).await,
        Commands::Auth(cmd) => auth::handle_auth(cmd, paths, json),
        Commands::Credential(cmd) => credential::execute(cmd, paths).await,
        Commands::Vault(cmd) => vault::execute(cmd, paths).await,
        Commands::Config(cmd) => config::handle_config(cmd, paths, json).await,
        Commands::System(cmd) => system::handle_system(cmd, paths, json).await,
        Commands::Daemon(cmd) => daemon::handle_daemon(cmd, paths, json).await,
        Commands::Channel(cmd) => channel::handle_channel(cmd, paths).await,
        Commands::Model(cmd) => model::execute(cmd, paths).await,
        Commands::Search(cmd) => search::handle_search(cmd, paths, json).await,
        Commands::Registry(cmd) => registry::handle_registry(cmd, paths, json),
        Commands::Runtime(cmd) => runtime::handle_runtime(cmd, paths, json).await,
        Commands::Tunnel(cmd) => tunnel::handle_tunnel(cmd, paths, json).await,
        Commands::Quota(cmd) => quota::handle_quota(cmd, paths, json).await,
        Commands::Audit(cmd) => audit::handle_audit(cmd, paths).await,
        Commands::Login { registry, api_key } => {
            let host = registry.unwrap_or_else(|| paths.registry_config().default);
            auth::handle_login(paths, &host, api_key)
        }
        Commands::Logout { registry } => {
            let host = registry.unwrap_or_else(|| paths.registry_config().default);
            auth::handle_logout(paths, &host)
        }
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
