//! Daemon Management Commands
//!
//! Proper daemon lifecycle management with:
//! - Foreground mode: `peko daemon start` (blocks, runs in current terminal)
//! - The daemon binary is always started with --daemon flag (blocks forever)
//! - Background mode: CLI spawns daemon as child process and returns immediately

use crate::api::client::ApiClient;
use crate::commands::GlobalPaths;
use crate::daemon::{Daemon, DaemonConfig};
use clap::Subcommand;
use std::path::PathBuf;
use tokio::process::Command;

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
    match cmd {
        DaemonCommands::Start {
            foreground,
            interval,
        } => {
            // Check if daemon is already running
            if let Ok(true) = check_daemon_running().await {
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

            let silent = std::env::var("PEKOBOT_DAEMON_SILENT").is_ok();

            if foreground {
                if !silent {
                    println!("🚀 Starting daemon in foreground (interval: {interval}s)...");
                    println!("   Config dir: {}", config.config_dir.display());
                    println!("   Data dir: {}", config.data_dir.display());
                    println!("   API: http://{}:{}", config.host, config.port);
                }

                let daemon = Daemon::new(config)?;
                if let Err(e) = daemon.run().await {
                    eprintln!("Daemon error: {}", e);
                }
            } else {
                println!("🚀 Starting daemon in background (interval: {interval}s)...");
                println!("   Config dir: {}", config.config_dir.display());
                println!("   Data dir: {}", config.data_dir.display());
                println!();

                match spawn_daemon(paths, interval).await {
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
            if let Ok(false) = check_daemon_running().await {
                println!("ℹ️  Daemon is not running");
                return Ok(());
            }

            if force {
                println!("💀 Force stopping daemon...");
            } else {
                println!("🛑 Stopping daemon gracefully...");
            }

            match stop_daemon(force).await {
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
        DaemonCommands::Status { json } => show_daemon_status(json).await,
        DaemonCommands::Restart { interval } => {
            println!("🔄 Restarting daemon...");

            // Stop if running
            if let Ok(true) = check_daemon_running().await {
                if let Err(e) = stop_daemon(false).await {
                    eprintln!("⚠️  Failed to stop daemon: {e}");
                    eprintln!("   Trying to start anyway...");
                } else {
                    // Wait a moment for the port to be released
                    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                }
            }

            // Start again
            match spawn_daemon(paths, interval).await {
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
            if let Ok(false) = check_daemon_running().await {
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
async fn check_daemon_running() -> anyhow::Result<bool> {
    let client = ApiClient::new()?;
    match client.health_check().await {
        Ok(health) => {
            // Status "ok" means ready, any other status means starting or degraded
            Ok(health.status == "ok")
        }
        Err(_) => Ok(false),
    }
}

/// Check if a process with given PID is running
fn is_process_running(pid: u32) -> bool {
    #[cfg(windows)]
    {
        use std::process::Command;
        // Use PowerShell's Get-Process which is more reliable than tasklist
        let output = Command::new("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &format!("Get-Process -Id {} -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Id", pid),
            ])
            .output();
        match output {
            Ok(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                // If the PID appears in output, process is running
                stdout.lines().any(|line| line.trim() == pid.to_string())
            }
            Err(_) => false,
        }
    }
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }
}

/// Show daemon status
async fn show_daemon_status(json: bool) -> anyhow::Result<()> {
    let pid_file = get_pid_file_path()?;

    // Try to get health info first - this tells us if daemon is actually running
    let client_result = ApiClient::new();

    // If we can connect to the health endpoint, daemon is running (regardless of PID file)
    if let Ok(client) = &client_result {
        match client.health_check().await {
            Ok(health) => {
                let info = client.daemon_info().await;
                let pid = info.as_ref().map(|i| i.pid).unwrap_or(0);
                if json {
                    let output = serde_json::json!({
                        "running": true,
                        "status": health.status,
                        "version": health.version,
                        "uptime_seconds": health.uptime_seconds,
                        "instance_count": health.instance_count,
                        "team_count": health.team_count,
                        "pid": pid,
                        "ready": health.status == "ok",
                    });
                    println!("{}", serde_json::to_string_pretty(&output)?);
                } else {
                    let ready = if health.status == "ok" { "✅" } else { "⚠️" };
                    println!("📊 Daemon Status:");
                    println!("  Status: {ready} {}", match health.status.as_str() {
                        "ok" => "Running",
                        "starting" => "Starting...",
                        "degraded" => "Degraded",
                        s => s,
                    });
                    println!("  Version: {}", health.version);
                    println!("  Uptime: {}s", health.uptime_seconds);
                    if let Ok(info) = info {
                        println!("  PID: {}", info.pid);
                        println!("  Port: {}", info.port);
                        println!("  Workspace: {}", info.workspace);
                    }
                    println!("  Instances: {}", health.instance_count);
                    println!("  Teams: {}", health.team_count);
                }
                return Ok(());
            }
            Err(_) => {}
        }
    }

    // Health check failed - daemon is not running
    // Check if PID file exists and is stale
    if pid_file.exists() {
        let pid_str = std::fs::read_to_string(&pid_file).unwrap_or_default();
        let pid: u32 = pid_str.trim().parse().unwrap_or(0);
        if pid > 0 && is_process_running(pid) {
            // Process IS running but health check failed - strange state
            if json {
                println!("{{\"running\": false, \"error\": \"health check failed but process {} exists\"}}", pid);
            } else {
                println!("📊 Daemon Status:");
                println!("  Status: ❌ Not responding (process {} exists but API unreachable)", pid);
                println!("  PID: {}", pid);
            }
            return Ok(());
        }
        // Stale PID file - clean it up
        let _ = std::fs::remove_file(&pid_file);
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

/// Stop the daemon gracefully via API, or force kill if API fails
async fn stop_daemon(force: bool) -> anyhow::Result<()> {
    let pid_file = get_pid_file_path()?;

    // Try graceful shutdown via API first
    if !force {
        if let Ok(client) = ApiClient::new() {
            match client.shutdown(false).await {
                Ok(()) => {
                    // Wait for daemon to shut down (up to 5 seconds)
                    for i in 0..10 {
                        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                        let pid_file = get_pid_file_path()?;
                        let pid = get_running_pid(&pid_file);
                        let running = pid.map(is_process_running).unwrap_or(false);
                        if !running {
                            // Daemon stopped
                            let _ = std::fs::remove_file(&pid_file);
                            return Ok(());
                        }
                    }
                    eprintln!("⚠️  Daemon did not stop gracefully, force killing...");
                }
                Err(e) => {
                    eprintln!("⚠️  API shutdown failed: {e}, falling back to PID kill...");
                }
            }
        }
    }

    // Fall back to PID-based kill
    if let Some(pid) = get_running_pid(&pid_file) {
        #[cfg(windows)]
        {
            Command::new("taskkill")
                .args(["/F", "/PID", &pid.to_string()])
                .output()
                .await?;
        }
        #[cfg(unix)]
        {
            let signal = if force { "-9" } else { "-15" };
            Command::new("kill")
                .args(&[signal, &pid.to_string()])
                .output()
                .await?;
        }

        // Wait for process to actually terminate
        for _ in 0..10 {
            tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
            if !is_process_running(pid) {
                break;
            }
        }
    }

    // Clean up PID file
    let _ = std::fs::remove_file(&pid_file);
    let _ = std::fs::remove_file(pid_file.with_extension("lock"));

    Ok(())
}

/// Get the PID from the PID file if it exists and is valid
fn get_running_pid(pid_file: &PathBuf) -> Option<u32> {
    if pid_file.exists() {
        if let Ok(pid_str) = std::fs::read_to_string(pid_file) {
            if let Ok(pid) = pid_str.trim().parse::<u32>() {
                if is_process_running(pid) {
                    return Some(pid);
                }
            }
        }
    }
    None
}

/// Spawn the daemon as a child process
async fn spawn_daemon(paths: &GlobalPaths, interval: u64) -> anyhow::Result<()> {
    let exe_path = std::env::current_exe()?;

    // Spawn child process that runs the daemon
    let mut cmd = Command::new(&exe_path);
    cmd.arg("daemon")
        .arg("start")
        .arg("--foreground")
        .arg("--interval")
        .arg(interval.to_string())
        .env("PEKOBOT_CONFIG_DIR", &paths.config_dir)
        .env("PEKOBOT_DATA_DIR", &paths.data_dir)
        .env("PEKOBOT_DAEMON_SILENT", "1")
        // Detach so child outlives parent
        .kill_on_drop(false)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    let mut child = cmd.spawn()?;

    // Wait for daemon to be ready (up to 10 seconds)
    let daemon_ready = wait_for_daemon_ready().await;

    if !daemon_ready {
        // Daemon didn't become ready, clean up
        let _ = child.kill();
        return Err(anyhow::anyhow!("Daemon failed to start - not ready"));
    }

    // Daemon is ready, ensure PID file exists
    if let Some(pid) = child.id() {
        let pid_file = get_pid_file_path()?;
        if let Some(parent) = pid_file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&pid_file, pid.to_string());
    }

    Ok(())
}

/// Wait for daemon to be ready (health check returns status "ok")
async fn wait_for_daemon_ready() -> bool {
    for i in 0..40 {
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        if let Ok(client) = ApiClient::new() {
            match client.health_check().await {
                Ok(health) if health.status == "ok" => {
                    return true;
                }
                Ok(health) => {
                    eprintln!("   Daemon status: {} (waiting... {})", health.status, i);
                }
                Err(e) => {
                    eprintln!("   Health check attempt {} failed: {}", i, e);
                }
            }
        } else {
            eprintln!("   Attempt {}: Failed to create API client", i);
        }
    }
    false
}

/// Get path to PID file
fn get_pid_file_path() -> anyhow::Result<PathBuf> {
    let home = dirs::home_dir()
        .or_else(dirs::data_dir)
        .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;
    Ok(home.join(".pekobot").join("run").join("daemon.pid"))
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

    #[test]
    fn test_pid_file_path() {
        let path = get_pid_file_path().unwrap();
        assert!(path.to_string_lossy().contains(".pekobot"));
        assert!(path.to_string_lossy().contains("run"));
        assert!(path.to_string_lossy().contains("daemon.pid"));
    }
}
