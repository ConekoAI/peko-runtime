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
                enable_isolated_execution: true,
                maintenance_interval: std::time::Duration::from_secs(3600),
            };

            if foreground {
                println!("🚀 Starting daemon in foreground (interval: {interval}s)...");
                println!("   Config dir: {}", config.config_dir.display());
                println!("   Data dir: {}", config.data_dir.display());

                let daemon = Daemon::new(config)?;
                if let Err(e) = daemon.run().await {
                    eprintln!("Daemon error: {}", e);
                }
            } else {
                println!("🚀 Starting daemon in background (interval: {interval}s)...");
                println!("   Config dir: {}", config.config_dir.display());
                println!("   Data dir: {}", config.data_dir.display());
                println!();

                match service.spawn_daemon(interval).await {
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
        DaemonCommands::Restart { interval } => {
            println!("🔄 Restarting daemon...");

            if service.is_daemon_running().await? {
                if let Err(e) = service.stop_daemon(false).await {
                    eprintln!("⚠️  Failed to stop daemon: {e}");
                    eprintln!("   Trying to start anyway...");
                } else {
                    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                }
            }

            match service.spawn_daemon(interval).await {
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
            let output = serde_json::json!({
                "running": true,
                "status": "ok",
                "version": status.version,
                "uptime_seconds": status.uptime_secs,
                "pid": status.pid.unwrap_or(0),
                "ready": true,
            });
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
        };
        let _cmd = DaemonCommands::Stop { force: false };
        let _cmd = DaemonCommands::Status { json: true };
        let _cmd = DaemonCommands::Restart { interval: 30 };
        let _cmd = DaemonCommands::Check;
    }
}
