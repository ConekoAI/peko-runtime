//! `peko-daemon` — long-running background daemon binary.
//!
//! Phase 12 lifted this entry point into its own workspace member
//! crate (`crates/peko-daemon/`). It depends only on the root `peko`
//! lib for the daemon entry surface (`Daemon`, `DaemonConfig`,
//! `LaunchMode`) and the `PathResolver`. The CLI's `daemon start`
//! background-spawn path resolves this artifact next to its own
//! executable and prefers it over re-exec'ing the CLI binary
//! (Phase 11c).
//!
//! The CLI's foreground command (`peko daemon start --foreground`)
//! still constructs `Daemon` directly today; Phase 12's facade
//! cleanup turns it into a thin wrapper that re-execs `peko-daemon`
//! once the visibility story settles.

use anyhow::{Context, Result};
use std::time::Duration;

use peko::common::paths::PathResolver;
use peko::daemon::{Daemon, DaemonConfig, LaunchMode};

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    // `peko-daemon` is launched as a subprocess by the CLI's
    // `DaemonProcessService::spawn_daemon_with`. It accepts the
    // same flag set the CLI's `daemon start --foreground` accepts
    // — the CLI forwards them verbatim. The shared flag surface
    // lets `peko-daemon` and `peko daemon start --foreground`
    // remain interchangeable process shapes (one binary becoming
    // two artifacts over time).
    let mut interval_secs: u64 = 15;
    let mut max_reconnect_attempts: u32 = 50;
    let mut sidecar_mode = false;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--interval" | "-i" => {
                interval_secs = args
                    .get(i + 1)
                    .context("--interval requires a value")?
                    .parse()
                    .context("--interval must be a u64")?;
                i += 2;
            }
            "--max-reconnect-attempts" => {
                max_reconnect_attempts = args
                    .get(i + 1)
                    .context("--max-reconnect-attempts requires a value")?
                    .parse()
                    .context("--max-reconnect-attempts must be a u32")?;
                i += 2;
            }
            "--sidecar-mode" => {
                sidecar_mode = true;
                i += 1;
            }
            "--foreground" => {
                // Accepted for symmetry with the CLI's `daemon start
                // --foreground` path, but `peko-daemon` is always
                // foreground by definition (a daemon binary that
                // returned would not be a daemon).
                i += 1;
            }
            "--help" | "-h" => {
                print_help();
                return Ok(());
            }
            other => {
                eprintln!("peko-daemon: unknown argument: {other}");
                print_help();
                std::process::exit(2);
            }
        }
    }

    // Build the same `DaemonConfig` the CLI's foreground path
    // builds. `PathResolver` honours the `PEKO_CONFIG_DIR` /
    // `PEKO_DATA_DIR` env vars the CLI sets when spawning the
    // subprocess (see `spawn_daemon_with`).
    let resolver = PathResolver::new();
    let config_dir = resolver.config_dir().to_path_buf();
    let data_dir = resolver.data_dir().to_path_buf();

    let config = DaemonConfig {
        cron_db_path: data_dir.join("cron.json"),
        poll_interval: Duration::from_secs(interval_secs),
        config_dir: config_dir.clone(),
        data_dir: data_dir.clone(),
        maintenance_interval: Duration::from_hours(1),
        max_reconnect_attempts,
        launch_mode: if sidecar_mode {
            LaunchMode::Sidecar
        } else {
            LaunchMode::Headless
        },
    };

    eprintln!("🚀 peko-daemon starting (interval: {interval_secs}s, sidecar: {sidecar_mode})...");
    eprintln!("   Config dir: {}", config.config_dir.display());
    eprintln!("   Data dir: {}", config.data_dir.display());

    let daemon = Daemon::new(config)?;
    Box::pin(daemon.run()).await
}

fn print_help() {
    eprintln!(
        "peko-daemon — long-running background daemon for Peko.

USAGE:
    peko-daemon [OPTIONS]

OPTIONS:
    -i, --interval <SECS>              Polling interval in seconds (default: 15)
        --max-reconnect-attempts <N>   Maximum PekoHub tunnel reconnect attempts
                                       before degraded state (default: 50)
        --sidecar-mode                 Run in peko-desktop sidecar mode
                                       (uses desktop.lock instead of peko.pid)
    -h, --help                         Print this help

ENVIRONMENT:
    PEKO_CONFIG_DIR                    Override the config directory
    PEKO_DATA_DIR                      Override the data directory

NOTES:
    peko-daemon is normally launched by the CLI as a subprocess via
    `peko daemon start` (no flags). Direct invocation is supported
    for system-level service managers (systemd, launchd, etc.)."
    );
}
