//! Daemon Management Commands
//!
//! Proper daemon lifecycle management with:
//! - Foreground mode: `peko daemon start` (blocks, runs in current terminal)
//! - The daemon binary is always started with --daemon flag (blocks forever)
//! - Background mode: CLI spawns daemon as child process and returns immediately
//!
//! All process lifecycle logic is delegated to `DaemonProcessService`.

use crate::commands::GlobalPaths;
use crate::common::services::DaemonProcessService;
use crate::daemon::{Daemon, DaemonConfig};
use clap::Subcommand;

/// Daemon management subcommands
///
/// The daemon is the core service that manages agents and provides the HTTP API.
/// It must be running before you can create or run agents.
///
/// Examples:
///   # Start the daemon (spawns child process, CLI returns immediately)
///   peko daemon start
///
///   # Start in foreground (blocks in current terminal)
///   peko daemon start --foreground
///
///   # Check daemon status
///   peko daemon status
///
///   # Stop the daemon gracefully
///   peko daemon stop
///
///   # Restart the daemon
///   peko daemon restart
#[derive(Subcommand)]
#[command(disable_version_flag = true)]
pub enum DaemonCommands {
    /// Start the daemon
    Start {
        /// Run in foreground (don't spawn child process)
        #[arg(short, long)]
        foreground: bool,
        /// Polling interval in seconds
        #[arg(short, long, default_value = "15")]
        interval: u64,
        /// Maximum number of consecutive PekoHub tunnel reconnect attempts
        /// before reporting degraded state. With default exponential
        /// backoff (1/2/4/.../60s), 50 attempts ≈ 28 minutes of retries.
        /// Set to a very large value (e.g. 4294967295) to disable the cap
        /// and retry forever. See issue #8.
        #[arg(long, default_value = "50")]
        max_reconnect_attempts: u32,
        /// Run in sidecar mode for `peko-desktop`. Uses a distinct lockfile
        /// (`desktop.lock`) so the desktop's bundled engine cannot collide
        /// with a CLI-launched daemon in the same config dir. See ADR-043.
        /// Not user-facing — the desktop sets this when it spawns the
        /// bundled sidecar.
        #[arg(long, hide = true)]
        sidecar_mode: bool,
    },

    /// Stop the daemon
    Stop {
        /// Force stop (kill immediately without graceful shutdown)
        #[arg(short, long)]
        force: bool,
    },

    /// Check daemon status
    Status {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Restart the daemon
    Restart {
        /// Polling interval in seconds
        #[arg(short, long, default_value = "15")]
        interval: u64,
        /// Maximum number of consecutive PekoHub tunnel reconnect attempts
        /// before reporting degraded state (passed through to `start`).
        #[arg(long, default_value = "50")]
        max_reconnect_attempts: u32,
    },

    /// Trigger immediate cron check
    Check,
}

/// Handle daemon commands
pub async fn handle_daemon(
    cmd: DaemonCommands,
    paths: &GlobalPaths,
    _json: bool,
) -> anyhow::Result<()> {
    let service = DaemonProcessService::new(paths.resolver().clone());

    match cmd {
        DaemonCommands::Start {
            foreground,
            interval,
            max_reconnect_attempts,
            sidecar_mode,
        } => {
            if service.is_daemon_running().await? {
                println!("⚠️  Daemon is already running");
                return Ok(());
            }

            let config = DaemonConfig {
                cron_db_path: paths.data_dir.join("cron.json"),
                poll_interval: std::time::Duration::from_secs(interval),
                config_dir: paths.config_dir.clone(),
                data_dir: paths.data_dir.clone(),
                maintenance_interval: std::time::Duration::from_hours(1),
                max_reconnect_attempts,
            };

            if foreground {
                println!("🚀 Starting daemon in foreground (interval: {interval}s)...");
                println!("   Config dir: {}", config.config_dir.display());
                println!("   Data dir: {}", config.data_dir.display());

                let daemon = Daemon::new(config)?;
                if let Err(e) = Box::pin(daemon.run()).await {
                    eprintln!("Daemon error: {}", e);
                }
            } else {
                println!("🚀 Starting daemon in background (interval: {interval}s)...");
                println!("   Config dir: {}", config.config_dir.display());
                println!("   Data dir: {}", config.data_dir.display());
                if sidecar_mode {
                    println!("   Mode: sidecar (lockfile: desktop.lock)");
                }
                println!();

                match service
                    .spawn_daemon_with(interval, max_reconnect_attempts, sidecar_mode)
                    .await
                {
                    Ok(_) => {
                        println!("✅ Daemon started successfully");
                        println!("   Check status: peko daemon status");
                    }
                    Err(e) => {
                        eprintln!("❌ Failed to start daemon: {e}");
                        eprintln!("   Try running with --foreground for debugging");
                        return Err(e);
                    }
                }
            }
            Ok(())
        }
        DaemonCommands::Stop { force } => {
            if !service.is_daemon_running().await? {
                println!("ℹ️  Daemon is not running");
                return Ok(());
            }

            if force {
                println!("💀 Force stopping daemon...");
            } else {
                println!("🛑 Stopping daemon gracefully...");
            }

            match service.stop_daemon(force).await {
                Ok(()) => {
                    println!("✅ Daemon stopped");
                    Ok(())
                }
                Err(e) => {
                    eprintln!("❌ Failed to stop daemon: {e}");
                    Err(e)
                }
            }
        }
        DaemonCommands::Status { json } => show_daemon_status(&service, json).await,
        DaemonCommands::Restart {
            interval,
            max_reconnect_attempts,
        } => {
            println!("🔄 Restarting daemon...");

            if service.is_daemon_running().await? {
                if let Err(e) = service.stop_daemon(false).await {
                    eprintln!("⚠️  Failed to stop daemon: {e}");
                    eprintln!("   Trying to start anyway...");
                } else {
                    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                }
            }

            // Restart preserves the existing mode (daemon or sidecar) by
            // inheriting whatever lockfile is currently held. If neither is
            // held, fall back to the standard daemon path. We don't
            // propagate sidecar_mode here because `peko daemon restart` is
            // a user-invoked command — the desktop drives its own restart
            // through SidecarSupervisor in PR D.
            let sidecar_mode = service.is_sidecar_lock_held();
            match service
                .spawn_daemon_with(interval, max_reconnect_attempts, sidecar_mode)
                .await
            {
                Ok(_) => {
                    println!("✅ Daemon restarted");
                    Ok(())
                }
                Err(e) => {
                    eprintln!("❌ Failed to restart daemon: {e}");
                    Err(e)
                }
            }
        }
        DaemonCommands::Check => {
            if !service.is_daemon_running().await? {
                println!("❌ Daemon is not running");
                return Err(anyhow::anyhow!("Daemon is not running"));
            }

            println!("🔍 Triggering cron check...");
            println!("   (Cron check API not yet implemented)");
            Ok(())
        }
    }
}

/// Show daemon status
async fn show_daemon_status(service: &DaemonProcessService, json: bool) -> anyhow::Result<()> {
    let status = service.get_daemon_status().await?;

    if status.responding {
        if json {
            let mut output = serde_json::json!({
                "running": true,
                "status": "ok",
                "version": status.version,
                "uptime_seconds": status.uptime_secs,
                "pid": status.pid.unwrap_or(0),
                "ready": true,
            });
            if let Some(t) = &status.tunnel {
                output["tunnel"] = serde_json::json!({
                    "state": t.state,
                    "reconnect_attempts": t.reconnect_attempts,
                    "last_error": t.last_error,
                    "degraded": t.degraded,
                });
            }
            println!("{}", serde_json::to_string_pretty(&output)?);
        } else {
            println!("📊 Daemon Status:");
            println!("  Status: ✅ Running");
            if let Some(v) = status.version {
                println!("  Version: {v}");
            }
            if let Some(u) = status.uptime_secs {
                println!("  Uptime: {u}s");
            }
            if let Some(pid) = status.pid {
                println!("  PID: {pid}");
            }
            if let Some(t) = &status.tunnel {
                let emoji = match t.state.as_str() {
                    "connected" => "✅",
                    "disabled" => "➖",
                    "degraded" => "⚠️ ",
                    _ => "❌",
                };
                println!(
                    "  Tunnel: {} {} (attempts: {})",
                    emoji, t.state, t.reconnect_attempts
                );
                if let Some(err) = &t.last_error {
                    println!("    Last error: {err}");
                }
            }
        }
        return Ok(());
    }

    if status.process_exists {
        if json {
            println!(
                "{{\"running\": false, \"error\": \"{}\"}}",
                status.error.unwrap_or_default()
            );
        } else {
            println!("📊 Daemon Status:");
            println!(
                "  Status: ❌ Not responding (process {} exists but IPC unreachable)",
                status.pid.unwrap_or(0)
            );
            if let Some(pid) = status.pid {
                println!("  PID: {pid}");
            }
        }
        return Ok(());
    }

    if json {
        println!("{{\"running\": false, \"reason\": \"no_daemon\"}}");
    } else {
        println!("📊 Daemon Status:");
        println!("  Status: ❌ Not running");
        println!("  Start with: peko daemon start");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_daemon_commands_enum() {
        let _cmd = DaemonCommands::Start {
            foreground: true,
            interval: 15,
            max_reconnect_attempts: 50,
            sidecar_mode: false,
        };
        let _cmd = DaemonCommands::Stop { force: false };
        let _cmd = DaemonCommands::Status { json: true };
        let _cmd = DaemonCommands::Restart {
            interval: 30,
            max_reconnect_attempts: 50,
        };
        let _cmd = DaemonCommands::Check;
    }
}
