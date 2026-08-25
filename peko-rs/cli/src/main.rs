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
    audit, auth, channel, config, credential, cron, daemon, from_cli,
    init_logging, interrupt, log, model, principal, quota, registry, runtime, search, send,
    system, tunnel, update, vault, version, Cli, Commands, GlobalPaths,
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

    // Initialize global ExtensionCore with the appropriate async transport
    // BEFORE running any command that might create agents.
    // - Daemon commands use LocalAsyncTransport (daemon owns task execution)
    // - CLI commands use DaemonHttpTransport if daemon is reachable;
    //   otherwise UnavailableAsyncTransport so async tools fail fast with a clear error.
    //   ADR-020: No in-process fallback. The old tokio::spawn path is removed from CLI.
    init_extension_core(&cli.command).await;

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

/// Initialize the global ExtensionCore with the appropriate transport
///
/// - Daemon commands: LocalAsyncTransport (daemon executes tasks locally)
/// - CLI commands: DaemonHttpTransport if daemon is reachable, else UnavailableAsyncTransport
///   so that async tools fail fast with a clear error instead of falling back to
///   in-process execution that would be dropped on CLI exit (ADR-020).
async fn init_extension_core(command: &Commands) {
    use peko_core::extensions::framework::core::{
        init_global_core, ExtensionCore, ExtensionServices,
    };
    use peko_core::extensions::framework::transport::async_router::AsyncExecutionRouter;
    use peko_core::extensions::framework::transport::async_transport::{
        create_local_transport, UnavailableAsyncTransport,
    };
    use std::sync::Arc;

    let is_daemon_cmd = matches!(command, Commands::Daemon(_));

    let router = if is_daemon_cmd {
        tracing::info!("Initializing ExtensionCore with LocalAsyncTransport (daemon mode)");
        AsyncExecutionRouter::with_transport(create_local_transport())
    } else {
        tracing::info!("Auto-detecting async transport for CLI mode");
        match peko_core::ipc::create_transport::create_transport().await {
            Ok(transport) => AsyncExecutionRouter::with_transport(transport),
            Err(_) => {
                // Daemon does not auto-start; user must start it manually.
                AsyncExecutionRouter::with_transport(std::sync::Arc::new(
                    UnavailableAsyncTransport::new(
                        "peko daemon is not running. Async tool execution requires the daemon.\n\
                         Start it with: peko daemon start\n\
                         Or wait for the task to complete via AsyncOutput.",
                    ),
                ))
            }
        }
    };

    let services = ExtensionServices::with_async_router(Arc::new(router));
    let core = Arc::new(ExtensionCore::with_services(Arc::new(services)));
    init_global_core(core);
    tracing::debug!("Initialized global ExtensionCore with async transport");
}

async fn run_command(
    command: Commands,
    paths: &GlobalPaths,
    json: bool,
    cli_registry: Option<&str>,
) -> anyhow::Result<()> {
    match command {
        Commands::Principal(cmd) => principal::handle_principal(cmd, paths, json).await,
        Commands::Send(args) => send::handle_send(args, paths, json).await,
        Commands::Interrupt(args) => interrupt::handle_interrupt(args, paths, json).await,
        Commands::Log(cmd) => log::handle_log(cmd, paths, json).await,
        Commands::Auth(cmd) => auth::handle_auth(cmd, paths, json),
        Commands::Credential(cmd) => credential::execute(cmd, paths).await,
        Commands::Vault(cmd) => vault::execute(cmd, paths).await,
        Commands::Config(cmd) => config::handle_config(cmd, paths, json).await,
        Commands::System(cmd) => system::handle_system(cmd, paths, json).await,
        Commands::Daemon(cmd) => daemon::handle_daemon(cmd, paths, json).await,
        Commands::Cron(cmd) => cron::handle_cron(cmd, paths, json).await,
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
