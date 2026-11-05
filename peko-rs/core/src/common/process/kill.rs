//! Graceful and forceful process termination

use anyhow::Result;
use std::time::Duration;

/// Check if a process with the given PID is currently running
///
/// Windows asks the kernel directly (`OpenProcess` + a zero-timeout wait
/// on the process handle). It used to start PowerShell for every check,
/// which takes one to several seconds on a busy machine; `peko daemon
/// stop` polls this, so a stop that the daemon finished in a second
/// still ran past ten.
#[must_use]
pub fn is_process_running(pid: u32) -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{
            CloseHandle, GetLastError, ERROR_ACCESS_DENIED, WAIT_TIMEOUT,
        };
        use windows_sys::Win32::System::Threading::{
            OpenProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION,
            PROCESS_SYNCHRONIZE,
        };
        // SAFETY: plain Win32 calls on a handle this block owns and closes.
        unsafe {
            let handle = OpenProcess(
                PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                0,
                pid,
            );
            if handle == 0 {
                // A process we may not open still exists; any other error
                // (ERROR_INVALID_PARAMETER) means no such PID.
                return GetLastError() == ERROR_ACCESS_DENIED;
            }
            let running = WaitForSingleObject(handle, 0) == WAIT_TIMEOUT;
            CloseHandle(handle);
            running
        }
    }
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }
}

/// Kill a process by PID using platform-specific tools
///
/// On Windows, `TerminateProcess` (what `taskkill /F` does, without
/// starting a process for it). On Unix, `kill` with SIGTERM (or SIGKILL
/// if `force` is true).
pub async fn kill_by_pid(pid: u32, force: bool) -> Result<()> {
    #[cfg(windows)]
    {
        let _ = force;
        use windows_sys::Win32::Foundation::{CloseHandle, GetLastError};
        use windows_sys::Win32::System::Threading::{
            OpenProcess, TerminateProcess, PROCESS_TERMINATE,
        };
        // SAFETY: plain Win32 calls on a handle this block owns and closes.
        unsafe {
            let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
            if handle == 0 {
                anyhow::bail!("cannot open PID {pid}: Win32 error {}", GetLastError());
            }
            let ok = TerminateProcess(handle, 1) != 0;
            let error = GetLastError();
            CloseHandle(handle);
            if !ok {
                anyhow::bail!("TerminateProcess failed for PID {pid}: Win32 error {error}");
            }
        }
    }
    #[cfg(unix)]
    {
        let signal = if force { "-9" } else { "-15" };
        tokio::process::Command::new("kill")
            .args([signal, &pid.to_string()])
            .output()
            .await?;
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
