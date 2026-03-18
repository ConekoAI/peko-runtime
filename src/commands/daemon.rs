//! Daemon Management Commands

use crate::api::client::ApiClient;
use crate::commands::GlobalPaths;
use crate::daemon::{Daemon, DaemonConfig};
use clap::Subcommand;
use std::path::PathBuf;

/// Daemon management subcommands
///
/// The daemon is the core service that manages agents and provides the HTTP API.
/// It must be running before you can create or run agents.
///
/// Examples:
///   # Start the daemon in background (default)
///   pekobot daemon start
///
///   # Start in foreground to see logs
///   pekobot daemon start --foreground
///
///   # Check daemon status
///   pekobot daemon status
///
///   # Stop the daemon
///   pekobot daemon stop
///
///   # Restart the daemon
///   pekobot daemon restart
#[derive(Subcommand)]
#[command(disable_version_flag = true)]
pub enum DaemonCommands {
    /// Start the daemon
    Start {
        /// Run in foreground (don't detach)
        #[arg(short, long)]
        foreground: bool,
        /// Polling interval in seconds
        #[arg(short, long, default_value = "15")]
        interval: u64,
    },

    /// Stop the daemon
    Stop {
        /// Force stop (kill immediately)
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
    match cmd {
        DaemonCommands::Start {
            foreground,
            interval,
        } => {
            // Check if daemon is already running
            if check_daemon_running().await {
                println!("⚠️  Daemon is already running");
                return Ok(());
            }

            let config = DaemonConfig {
                cron_db_path: paths.data_dir.join("cron.db"),
                poll_interval: std::time::Duration::from_secs(interval),
                config_dir: paths.config_dir.clone(),
                data_dir: paths.data_dir.clone(),
                enable_isolated_execution: true,
                maintenance_interval: std::time::Duration::from_secs(3600), // 1 hour
                host: crate::api::DEFAULT_HOST.to_string(),
                port: crate::api::DEFAULT_PORT,
            };

            if foreground {
                println!("🚀 Starting daemon in foreground (interval: {interval}s)...");
                println!("   Config dir: {}", config.config_dir.display());
                println!("   Data dir: {}", config.data_dir.display());
                println!("   API: http://{}:{}", config.host, config.port);

                let (_tx, rx) = tokio::sync::mpsc::channel(10);
                let daemon = Daemon::new(config, rx)?;
                daemon.run().await?;
            } else {
                println!("🚀 Starting daemon in background (interval: {interval}s)...");
                println!("   Config dir: {}", config.config_dir.display());
                println!("   Data dir: {}", config.data_dir.display());
                println!();

                // For background mode, we need to spawn a detached process
                // This is platform-specific; for now, use the existing daemon infrastructure
                match spawn_background_daemon(paths, interval).await {
                    Ok(pid) => {
                        println!("✅ Daemon started with PID: {}", pid);
                        println!("   Check status: pekobot daemon status");
                    }
                    Err(e) => {
                        eprintln!("❌ Failed to start daemon: {}", e);
                        eprintln!("   Try running with --foreground for debugging");
                        return Err(e);
                    }
                }
            }
            Ok(())
        }
        DaemonCommands::Stop { force } => {
            if !check_daemon_running().await {
                println!("ℹ️  Daemon is not running");
                return Ok(());
            }

            if force {
                println!("💀 Force stopping daemon...");
            } else {
                println!("🛑 Stopping daemon gracefully...");
            }

            // Try to stop via API first (graceful shutdown)
            // If that fails or force is true, kill the process
            match stop_daemon(force).await {
                Ok(()) => {
                    println!("✅ Daemon stopped");
                    Ok(())
                }
                Err(e) => {
                    eprintln!("❌ Failed to stop daemon: {}", e);
                    Err(e)
                }
            }
        }
        DaemonCommands::Status { json } => show_daemon_status(json).await,
        DaemonCommands::Restart { interval } => {
            println!("🔄 Restarting daemon...");

            // Stop if running
            if check_daemon_running().await {
                if let Err(e) = stop_daemon(false).await {
                    eprintln!("⚠️  Failed to stop daemon: {}", e);
                    eprintln!("   Trying to start anyway...");
                } else {
                    // Wait a moment for the port to be released
                    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                }
            }

            // Start again
            match spawn_background_daemon(paths, interval).await {
                Ok(pid) => {
                    println!("✅ Daemon restarted with PID: {}", pid);
                    Ok(())
                }
                Err(e) => {
                    eprintln!("❌ Failed to restart daemon: {}", e);
                    Err(e)
                }
            }
        }
        DaemonCommands::Check => {
            if !check_daemon_running().await {
                println!("❌ Daemon is not running");
                return Err(anyhow::anyhow!("Daemon is not running"));
            }

            println!("🔍 Triggering cron check...");
            // TODO: Implement cron check API endpoint
            println!("   (Cron check API not yet implemented)");
            Ok(())
        }
    }
}

/// Check if daemon is running by trying to connect to its API
async fn check_daemon_running() -> bool {
    if let Ok(client) = ApiClient::new() {
        client.health_check().await.is_ok()
    } else {
        false
    }
}

/// Show daemon status
async fn show_daemon_status(json: bool) -> anyhow::Result<()> {
    let client = match ApiClient::new() {
        Ok(c) => c,
        Err(_) => {
            if json {
                println!("{{\"running\": false, \"error\": \"Failed to create API client\"}}");
            } else {
                println!("📊 Daemon Status:");
                println!("  Status: ❌ Not running");
                println!("  Start with: pekobot daemon start");
            }
            return Ok(());
        }
    };

    match client.health_check().await {
        Ok(health) => match client.daemon_info().await {
            Ok(info) => {
                if json {
                    let output = serde_json::json!({
                        "running": true,
                        "status": health.status,
                        "version": health.version,
                        "uptime_seconds": health.uptime_seconds,
                        "instance_count": health.instance_count,
                        "team_count": health.team_count,
                        "api_version": info.api_version,
                        "workspace": info.workspace,
                        "port": info.port,
                        "pid": info.pid,
                        "platform": info.platform,
                        "capabilities": info.capabilities,
                    });
                    println!("{}", serde_json::to_string_pretty(&output)?);
                } else {
                    println!("📊 Daemon Status:");
                    println!("  Status: ✅ Running ({})", health.status);
                    println!("  Version: {}", health.version);
                    println!("  API Version: {}", info.api_version);
                    println!("  Uptime: {}s", health.uptime_seconds);
                    println!("  PID: {}", info.pid);
                    println!("  Port: {}", info.port);
                    println!("  Platform: {}", info.platform);
                    println!("  Workspace: {}", info.workspace);
                    println!("  Instances: {}", health.instance_count);
                    println!("  Teams: {}", health.team_count);
                    println!("  Capabilities:");
                    println!(
                        "    - Streaming: {}",
                        if info.capabilities.streaming {
                            "✅"
                        } else {
                            "❌"
                        }
                    );
                    println!(
                        "    - WebSocket: {}",
                        if info.capabilities.websocket {
                            "✅"
                        } else {
                            "❌"
                        }
                    );
                    println!(
                        "    - Teams: {}",
                        if info.capabilities.teams {
                            "✅"
                        } else {
                            "❌"
                        }
                    );
                }
                Ok(())
            }
            Err(_) => {
                if json {
                    println!("{{\"running\": true, \"status\": \"{status}\", \"version\": \"{version}\"}}",
                            status = health.status,
                            version = health.version);
                } else {
                    println!("📊 Daemon Status:");
                    println!("  Status: ✅ Running ({})", health.status);
                    println!("  Version: {}", health.version);
                    println!("  Uptime: {}s", health.uptime_seconds);
                    println!("  Instances: {}", health.instance_count);
                    println!("  Teams: {}", health.team_count);
                }
                Ok(())
            }
        },
        Err(e) => {
            if json {
                println!("{{\"running\": false, \"error\": \"{}\"}}", e);
            } else {
                println!("📊 Daemon Status:");
                println!("  Status: ❌ Not responding");
                println!("  Error: {}", e);
                println!("  Start with: pekobot daemon start");
            }
            Ok(())
        }
    }
}

/// Stop the daemon
async fn stop_daemon(_force: bool) -> anyhow::Result<()> {
    // For now, we read the PID from a file and kill the process
    // In a full implementation, this would send a graceful shutdown signal
    // via the API, and force mode would use SIGKILL

    let pid_file = get_pid_file_path()?;

    if !pid_file.exists() {
        // Try to stop via API (graceful shutdown)
        if let Ok(client) = ApiClient::new() {
            // Note: We don't have a shutdown endpoint yet
            // This would be implemented as part of the full daemon lifecycle
            let _ = client;
        }
        return Err(anyhow::anyhow!("Daemon PID file not found"));
    }

    let pid_str = std::fs::read_to_string(&pid_file)?;
    let pid: u32 = pid_str.trim().parse()?;

    // Try graceful shutdown first (if API is available)
    // Otherwise, terminate the process

    #[cfg(unix)]
    {
        use std::process::Command;
        let signal = if _force { "-9" } else { "-15" };
        Command::new("kill")
            .args(&[signal, &pid.to_string()])
            .output()?;
    }

    #[cfg(windows)]
    {
        use std::process::Command;
        Command::new("taskkill")
            .args(&["/F", "/PID", &pid.to_string()])
            .output()?;
    }

    // Remove PID file
    let _ = std::fs::remove_file(&pid_file);

    // Also remove any lock file
    let lock_file = pid_file.with_extension("lock");
    let _ = std::fs::remove_file(&lock_file);

    Ok(())
}

/// Spawn daemon in background
async fn spawn_background_daemon(paths: &GlobalPaths, interval: u64) -> anyhow::Result<u32> {
    // For a full implementation, this would:
    // 1. Fork/spawn a new process
    // 2. Detach from terminal
    // 3. Write PID to file
    // 4. Start the daemon

    // For now, we'll create a simple implementation that uses tokio::process
    // Note: True daemonization requires platform-specific code

    use std::process::Stdio;
    use tokio::process::Command;

    let exe_path = std::env::current_exe()?;

    let mut cmd = Command::new(&exe_path);
    cmd.arg("daemon")
        .arg("start")
        .arg("--foreground")
        .arg("--interval")
        .arg(interval.to_string())
        .env("PEKOBOT_CONFIG_DIR", &paths.config_dir)
        .env("PEKOBOT_DATA_DIR", &paths.data_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(false);

    let child = cmd.spawn()?;
    let pid = child
        .id()
        .ok_or_else(|| anyhow::anyhow!("Failed to get process ID"))?;

    // Write PID to file
    let pid_file = get_pid_file_path()?;
    if let Some(parent) = pid_file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&pid_file, pid.to_string())?;

    // Wait a moment and verify the daemon is running
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    if !check_daemon_running().await {
        let _ = std::fs::remove_file(&pid_file);
        return Err(anyhow::anyhow!("Daemon failed to start"));
    }

    Ok(pid)
}

/// Get path to PID file
fn get_pid_file_path() -> anyhow::Result<PathBuf> {
    let home = dirs::home_dir()
        .or_else(|| dirs::data_dir())
        .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;
    Ok(home.join(".pekobot").join("run").join("daemon.pid"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_daemon_commands_enum() {
        // Test that the enum variants exist and have correct types
        let _cmd = DaemonCommands::Start {
            foreground: true,
            interval: 15,
        };
        let _cmd = DaemonCommands::Stop { force: false };
        let _cmd = DaemonCommands::Status { json: true };
        let _cmd = DaemonCommands::Restart { interval: 30 };
        let _cmd = DaemonCommands::Check;
    }

    #[test]
    fn test_pid_file_path() {
        let path = get_pid_file_path().unwrap();
        assert!(path.to_string_lossy().contains(".pekobot"));
        assert!(path.to_string_lossy().contains("run"));
        assert!(path.to_string_lossy().contains("daemon.pid"));
    }

    #[test]
    fn test_client_error_exit_codes_daemon_not_running() {
        let err = crate::api::client::ClientError::DaemonNotRunning {
            addr: "http://127.0.0.1:11435".to_string(),
        };
        assert_eq!(err.exit_code(), 1);
    }
}
