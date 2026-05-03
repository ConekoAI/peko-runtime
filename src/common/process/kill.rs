//! Graceful and forceful process termination

use anyhow::Result;
use std::time::Duration;
use tokio::process::Child;
use tokio::time::timeout;
use tracing::{debug, warn};

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
