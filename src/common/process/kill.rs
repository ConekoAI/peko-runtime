//! Graceful and forceful process termination

use anyhow::Result;
use std::time::Duration;
use tokio::process::Child;
use tokio::time::timeout;
use tracing::{debug, warn};

/// Check if a process with the given PID is currently running
#[must_use]
pub fn is_process_running(pid: u32) -> bool {
    #[cfg(windows)]
    {
        use std::process::Command;
        let output = Command::new("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &format!(
                    "Get-Process -Id {} -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Id",
                    pid
                ),
            ])
            .output();
        match output {
            Ok(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout);
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

/// Kill a process by PID using platform-specific tools
///
/// On Windows, uses `taskkill`. On Unix, uses `kill` with SIGTERM
/// (or SIGKILL if `force` is true).
pub async fn kill_by_pid(pid: u32, force: bool) -> Result<()> {
    #[cfg(windows)]
    {
        let output = tokio::process::Command::new("taskkill")
            .args(["/F", "/PID", &pid.to_string()])
            .output()
            .await?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("taskkill failed for PID {pid}: {stderr}");
        }
    }
    #[cfg(unix)]
    {
        let signal = if force { "-9" } else { "-15" };
        tokio::process::Command::new("kill")
            .args(&[signal, &pid.to_string()])
            .output()
            .await?;
    }
    Ok(())
}

/// Kill all processes matching a given name pattern (fallback kill)
///
/// On Windows, kills both `pekobot.exe` and `peko.exe`.
/// On Unix, uses `pkill -9 -f` with the pattern.
pub async fn kill_all_by_name() -> Result<()> {
    #[cfg(windows)]
    {
        for im_arg in ["pekobot.exe", "peko.exe"] {
            let _ = tokio::process::Command::new("taskkill")
                .args(["/F", "/IM", im_arg])
                .output()
                .await;
        }
    }
    #[cfg(unix)]
    {
        let _ = tokio::process::Command::new("pkill")
            .args(["-9", "-f", "pekobot daemon"])
            .output()
            .await;
    }
    Ok(())
}

/// Wait for a process to terminate, polling every `interval`
///
/// Returns `Ok(true)` if the process terminated within `timeout`.
/// Returns `Ok(false)` if the process is still running after `timeout`.
pub async fn wait_for_exit(pid: u32, timeout: Duration, interval: Duration) -> Result<bool> {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if !is_process_running(pid) {
            return Ok(true);
        }
        tokio::time::sleep(interval).await;
    }
    Ok(!is_process_running(pid))
}

/// Gracefully shut down a child process
///
/// First attempts graceful shutdown by closing stdin, then waits for the process
/// to exit. If it doesn't exit within the timeout, it force kills the process.
pub async fn graceful_shutdown(mut child: Child, kill_timeout: Duration, pid: u32) -> Result<()> {
    // Take stdin to close it and signal EOF to the process
    drop(child.stdin.take());

    // Wait for process to exit with timeout
    match timeout(kill_timeout, child.wait()).await {
        Ok(Ok(status)) => {
            debug!("Process[{}] exited gracefully: {:?}", pid, status);
            Ok(())
        }
        Ok(Err(e)) => {
            warn!("Process[{}] error waiting: {}", pid, e);
            force_kill_child(&mut child, pid).await
        }
        Err(_) => {
            warn!(
                "Process[{}] did not exit within {:?}, force killing",
                pid, kill_timeout
            );
            force_kill_child(&mut child, pid).await
        }
    }
}

/// Force kill a child process immediately
pub async fn force_kill_child(child: &mut Child, pid: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::process::ChildExt;
        // Send SIGTERM first for graceful termination
        if let Some(id) = child.id() {
            unsafe {
                libc::kill(id as i32, libc::SIGTERM);
            }
        }
        // Brief pause for graceful shutdown
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    match child.kill().await {
        Ok(()) => {
            debug!("Process[{}] force killed", pid);
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::InvalidInput => {
            // Process already exited
            debug!("Process[{}] already exited", pid);
            Ok(())
        }
        Err(e) => Err(anyhow::anyhow!("Failed to kill process[{pid}]: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_process_running_self() {
        // Our own process should be running
        let own_pid = std::process::id();
        assert!(is_process_running(own_pid));
    }

    #[test]
    fn test_is_process_running_invalid() {
        // PID 0 is typically the scheduler/init, but on Windows Get-Process
        // may fail. Use a very high PID that's unlikely to exist.
        assert!(!is_process_running(999_999));
    }
}
