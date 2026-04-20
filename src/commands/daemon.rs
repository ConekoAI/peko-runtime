//! Daemon Management Commands
//!
//! Proper daemon lifecycle management with:
//! - Foreground mode: `peko daemon start` (blocks, runs in current terminal)
//! - The daemon binary is always started with --daemon flag (blocks forever)
//! - Background mode: CLI spawns daemon as child process and returns immediately

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

/// Check if daemon is running by trying to connect via IPC (no auto-start)
async fn check_daemon_running() -> anyhow::Result<bool> {
    use crate::ipc::{ConnectionManager, RequestPacket, ResponsePacket};

    match ConnectionManager::try_connect().await {
        Ok(conn) => {
            let ping = RequestPacket::Ping { request_id: 0 };
            if let Ok(bytes) = ping.to_bytes() {
                if conn.send(&bytes).await.is_ok() {
                    let mut buf = vec![0u8; 65536];
                    if let Ok(len) = conn.recv_timeout(&mut buf, std::time::Duration::from_secs(2)).await {
                        if let Ok(response) = ResponsePacket::from_bytes(&buf[..len]) {
                            if matches!(response, ResponsePacket::Pong { .. }) {
                                return Ok(true);
                            }
                        }
                    }
                }
            }
            Ok(false)
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

    // Try IPC (no auto-start)
    match crate::ipc::ConnectionManager::try_connect().await {
        Ok(conn) => {
            let ping = crate::ipc::RequestPacket::Ping { request_id: 0 };
            let pong = async {
                let bytes = ping.to_bytes().ok()?;
                conn.send(&bytes).await.ok()?;
                let mut buf = vec![0u8; 65536];
                let len = conn.recv_timeout(&mut buf, std::time::Duration::from_secs(2)).await.ok()?;
                let response = crate::ipc::ResponsePacket::from_bytes(&buf[..len]).ok()?;
                match response {
                    crate::ipc::ResponsePacket::Pong { uptime_secs, version, .. } => Some((uptime_secs, version)),
                    _ => None,
                }
            };
            if let Some((uptime_secs, version)) = pong.await {
                let pid = get_running_pid(&pid_file);
                if json {
                    let output = serde_json::json!({
                        "running": true,
                        "status": "ok",
                        "version": version,
                        "uptime_seconds": uptime_secs,
                        "pid": pid.unwrap_or(0),
                        "ready": true,
                    });
                    println!("{}", serde_json::to_string_pretty(&output)?);
                } else {
                    println!("📊 Daemon Status:");
                    println!("  Status: ✅ Running");
                    println!("  Version: {}", version);
                    println!("  Uptime: {}s", uptime_secs);
                    if let Some(pid) = pid {
                        println!("  PID: {}", pid);
                    }
                }
                return Ok(());
            }
        }
        Err(_) => {}
    }

    // Daemon is not running
    if pid_file.exists() {
        let pid_str = std::fs::read_to_string(&pid_file).unwrap_or_default();
        let pid: u32 = pid_str.trim().parse().unwrap_or(0);
        if pid > 0 && is_process_running(pid) {
            if json {
                println!("{{\"running\": false, \"error\": \"daemon not responding (process {} exists)\"}}", pid);
            } else {
                println!("📊 Daemon Status:");
                println!("  Status: ❌ Not responding (process {} exists but IPC unreachable)", pid);
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

/// Stop the daemon via IPC graceful shutdown, then PID-based kill as fallback
async fn stop_daemon(force: bool) -> anyhow::Result<()> {
    let pid_file = get_pid_file_path()?;

    // 1. Try graceful shutdown via IPC first (no auto-start)
    let mut shutdown_sent = false;
    if !force {
        if let Ok(conn) = crate::ipc::ConnectionManager::try_connect().await {
            let shutdown_req = crate::ipc::RequestPacket::Shutdown { request_id: 0, force: false };
            if let Ok(bytes) = shutdown_req.to_bytes() {
                if conn.send(&bytes).await.is_ok() {
                    let mut buf = vec![0u8; 65536];
                    if let Ok(len) = conn.recv_timeout(&mut buf, std::time::Duration::from_secs(3)).await {
                        if let Ok(crate::ipc::ResponsePacket::ShuttingDown { .. }) = crate::ipc::ResponsePacket::from_bytes(&buf[..len]) {
                            shutdown_sent = true;
                            println!("   Graceful shutdown request sent via IPC");
                        }
                    }
                }
            }
        }
    }

    // Wait for graceful shutdown to take effect (daemon needs time to clean up)
    if shutdown_sent && !force {
        println!("   Waiting for daemon to shut down gracefully...");
        // Poll for up to 5 seconds to give the daemon time to exit
        for i in 0..25 {
            tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
            if let Ok(false) = check_daemon_running().await {
                println!("   Daemon stopped gracefully after {}ms", (i + 1) * 200);
                // Clean up PID file and lock files
                let _ = std::fs::remove_file(&pid_file);
                let _ = std::fs::remove_file(pid_file.with_extension("lock"));
                return Ok(());
            }
        }
        println!("   Daemon still running after graceful request, falling back to PID kill...");
    } else if !force {
        println!("   Daemon not reachable via IPC, trying PID file...");
    }

    // 2. PID-based kill
    let mut killed = false;
    if let Some(pid) = get_running_pid(&pid_file) {
        println!("   Found PID file with PID: {pid}");
        #[cfg(windows)]
        {
            let output = Command::new("taskkill")
                .args(["/F", "/PID", &pid.to_string()])
                .output()
                .await?;
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            if output.status.success() {
                println!("   taskkill /F /PID {pid} succeeded");
            } else {
                println!("   taskkill /F /PID {pid} failed: {stderr}");
            }
            if !stderr.is_empty() {
                println!("   taskkill stderr: {stderr}");
            }
            if !stdout.is_empty() {
                println!("   taskkill stdout: {stdout}");
            }
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
        for i in 0..30 {
            tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
            if !is_process_running(pid) {
                killed = true;
                println!("   Process {pid} terminated after {}ms", (i + 1) * 200);
                break;
            }
        }
        if !killed {
            println!("   Warning: Process {pid} may still be running");
        }
    } else {
        println!("   No valid PID file found");
    }

    // Clean up PID file and lock files
    let _ = std::fs::remove_file(&pid_file);
    let _ = std::fs::remove_file(pid_file.with_extension("lock"));

    // 3. Final verification: ensure no daemon is responding on IPC
    println!("   Verifying daemon is stopped...");
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    if let Ok(conn) = crate::ipc::ConnectionManager::try_connect().await {
        let ping = crate::ipc::RequestPacket::Ping { request_id: 0 };
        if let Ok(bytes) = ping.to_bytes() {
            if conn.send(&bytes).await.is_ok() {
                let mut buf = vec![0u8; 65536];
                if let Ok(len) = conn.recv_timeout(&mut buf, std::time::Duration::from_secs(2)).await {
                    if let Ok(crate::ipc::ResponsePacket::Pong { .. }) = crate::ipc::ResponsePacket::from_bytes(&buf[..len]) {
                        // Daemon is still running — try to kill all pekobot/peko processes as fallback
                        println!("   Daemon still responding! Attempting fallback kill...");
                        #[cfg(windows)]
                        {
                            // Try both known binary names since the binary can be invoked as either
                            for im_arg in ["pekobot.exe", "peko.exe"] {
                                println!("   Running: taskkill /F /IM {im_arg}");
                                let _ = Command::new("taskkill")
                                    .args(["/F", "/IM", im_arg])
                                    .output()
                                    .await;
                            }
                        }
                        #[cfg(unix)]
                        {
                            let _ = Command::new("pkill")
                                .args(["-9", "-f", "pekobot daemon"])
                                .output()
                                .await;
                        }
                        return Err(anyhow::anyhow!(
                            "Daemon process is still running after stop attempt. \
                             Try: taskkill /F /IM pekobot.exe"
                        ));
                    }
                }
            }
        }
    }
    println!("   Daemon is stopped");

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
        // Suppress child output so it doesn't flood the parent terminal
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        // Detach so child outlives parent
        .kill_on_drop(false);

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

/// Wait for daemon to be ready (IPC ping returns Pong)
///
/// Uses `try_connect` to avoid auto-starting another daemon — the caller
/// is responsible for having already spawned the daemon process.
async fn wait_for_daemon_ready() -> bool {
    use crate::ipc::ConnectionManager;

    for i in 0..40 {
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        // Use try_connect to avoid auto-starting a second daemon
        match ConnectionManager::try_connect().await {
            Ok(conn) => {
                // Send a ping and wait for Pong
                let ping = crate::ipc::RequestPacket::Ping { request_id: 0 };
                if let Ok(ping_bytes) = ping.to_bytes() {
                    if conn.send(&ping_bytes).await.is_ok() {
                        let mut buf = vec![0u8; 65536];
                        if let Ok(len) = conn.recv_timeout(&mut buf, std::time::Duration::from_secs(2)).await {
                            if let Ok(response) = crate::ipc::ResponsePacket::from_bytes(&buf[..len]) {
                                match response {
                                    crate::ipc::ResponsePacket::Pong { .. } => {
                                        return true;
                                    }
                                    _ => {
                                        eprintln!("   Unexpected response to ping (waiting... {})", i);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Err(_) => {
                eprintln!("   Attempt {}: Daemon not yet ready", i);
            }
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
