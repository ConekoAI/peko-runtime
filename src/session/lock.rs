//! File locking for session storage
//!
//! Provides advisory file locking with:
//! - Timeout-based lock acquisition
//! - Stale lock detection (based on PID and timestamp)
//! - Automatic cleanup on process termination
//! - Cross-platform support (Unix-style advisory locks)

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tracing::{debug, warn};

/// Default timeout for acquiring a lock
pub const DEFAULT_LOCK_TIMEOUT_MS: u64 = 10_000;

/// Default stale lock threshold (30 seconds)
pub const DEFAULT_STALE_LOCK_MS: u64 = 30_000;

/// Lock file content format
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct LockPayload {
    pid: u32,
    created_at: String, // ISO 8601
}

/// An acquired file lock that releases when dropped
pub struct FileLock {
    lock_path: PathBuf,
    released: bool,
}

impl FileLock {
    /// Acquire a lock on the given session file
    ///
    /// # Arguments
    /// * `session_path` - Path to the session file (not the lock file)
    /// * `timeout_ms` - Maximum time to wait for the lock
    ///
    /// # Returns
    /// * `Ok(FileLock)` - Lock acquired successfully
    /// * `Err` - Timeout or other error
    pub async fn acquire(session_path: impl AsRef<Path>, timeout_ms: u64) -> Result<Self> {
        let lock_path = Self::lock_path(&session_path);
        let start = SystemTime::now();
        let timeout = Duration::from_millis(timeout_ms);

        loop {
            // Try to acquire the lock
            match Self::try_acquire(&lock_path).await {
                Ok(lock) => return Ok(lock),
                Err(e) => {
                    let elapsed = start.elapsed().unwrap_or(Duration::MAX);
                    if elapsed >= timeout {
                        return Err(anyhow::anyhow!(
                            "Lock acquisition timeout after {timeout_ms}ms: {e}"
                        ));
                    }

                    // Wait a bit before retrying (exponential backoff)
                    let delay = std::cmp::min(100, 10 * (elapsed.as_millis() as u64 / 100 + 1));
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
            }
        }
    }

    /// Try to acquire the lock immediately (non-blocking)
    async fn try_acquire(lock_path: &Path) -> Result<FileLock> {
        // Check if lock exists and is stale
        if lock_path.exists() {
            match Self::is_lock_stale(lock_path).await {
                Ok(true) => {
                    warn!("Removing stale lock file: {}", lock_path.display());
                    fs::remove_file(lock_path)
                        .await
                        .context("Failed to remove stale lock file")?;
                }
                Ok(false) => {
                    return Err(anyhow::anyhow!("Lock file exists and is active"));
                }
                Err(e) => {
                    // If we can't read the lock file, assume it's corrupt and remove it
                    // Check if it's just a "file not found" error (race condition with another process cleaning up)
                    if e.to_string().contains("No such file")
                        || e.to_string().contains("entity not found")
                    {
                        debug!("Lock file was removed by another process (race condition), continuing...");
                    } else {
                        warn!("Could not read lock file (removing): {}", e);
                        let _ = fs::remove_file(lock_path).await;
                    }
                }
            }
        }

        // Create lock file with our PID
        let payload = LockPayload {
            pid: std::process::id(),
            created_at: chrono::Utc::now().to_rfc3339(),
        };

        let json = serde_json::to_string(&payload)?;

        // Write to temp file then rename for atomicity
        let temp_path = lock_path.with_extension("tmp");
        {
            let mut file = fs::File::create(&temp_path)
                .await
                .context("Failed to create temp lock file")?;
            file.write_all(json.as_bytes()).await?;
            file.flush().await?;
        }

        fs::rename(&temp_path, lock_path)
            .await
            .context("Failed to rename temp lock file")?;

        debug!("Acquired lock: {}", lock_path.display());

        Ok(FileLock {
            lock_path: lock_path.to_path_buf(),
            released: false,
        })
    }

    /// Check if an existing lock file is stale
    async fn is_lock_stale(lock_path: &Path) -> Result<bool> {
        let content = match fs::read_to_string(lock_path).await {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // File was removed between exists() check and now (race condition)
                return Err(anyhow::anyhow!("Lock file not found (race condition)"));
            }
            Err(e) => {
                return Err(anyhow::anyhow!("Failed to read lock file: {e}"));
            }
        };

        let payload: LockPayload =
            serde_json::from_str(&content).context("Failed to parse lock file")?;

        // Check if PID is still alive
        if !Self::is_pid_alive(payload.pid) {
            return Ok(true);
        }

        // Check if lock is old enough to be considered stale
        let created = chrono::DateTime::parse_from_rfc3339(&payload.created_at)
            .context("Failed to parse lock timestamp")?;
        let age = chrono::Utc::now().signed_duration_since(created);

        Ok(age.num_milliseconds() > DEFAULT_STALE_LOCK_MS as i64)
    }

    /// Check if a process with the given PID is alive
    #[cfg(unix)]
    fn is_pid_alive(pid: u32) -> bool {
        // On Unix, send signal 0 to check if process exists
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }

    #[cfg(windows)]
    fn is_pid_alive(pid: u32) -> bool {
        // On Windows, try to open the process
        use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
        use windows_sys::Win32::System::Threading::OpenProcess;
        use windows_sys::Win32::System::Threading::PROCESS_QUERY_INFORMATION;

        unsafe {
            let handle: HANDLE = OpenProcess(PROCESS_QUERY_INFORMATION, 0, pid);
            if handle == 0 {
                return false;
            }
            CloseHandle(handle);
            true
        }
    }

    /// Get the lock file path for a session file
    fn lock_path(session_path: impl AsRef<Path>) -> PathBuf {
        session_path.as_ref().with_extension("lock")
    }

    /// Explicitly release the lock
    pub async fn release(mut self) -> Result<()> {
        self.do_release().await
    }

    async fn do_release(&mut self) -> Result<()> {
        if self.released {
            return Ok(());
        }

        if self.lock_path.exists() {
            fs::remove_file(&self.lock_path)
                .await
                .context("Failed to remove lock file")?;
        }

        debug!("Released lock: {}", self.lock_path.display());
        self.released = true;
        Ok(())
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        if !self.released {
            // Use synchronous remove since we're in a sync context (Drop)
            // tokio::spawn might not complete before runtime shutdown
            let _ = std::fs::remove_file(&self.lock_path);
        }
    }
}

/// Lock manager for coordinating access to multiple session files.
///
/// Uses [`SimpleRegistry`] for tracking held locks to avoid hand-rolled
/// `Mutex<HashMap>` patterns.
pub struct LockManager {
    // Track held locks to prevent double-locking in the same process
    held: Mutex<crate::common::registry::SimpleRegistry<PathBuf, u32>>,
}

impl LockManager {
    #[must_use]
    pub fn new() -> Self {
        Self {
            held: Mutex::new(crate::common::registry::SimpleRegistry::new()),
        }
    }

    /// Acquire a lock, tracking it in the manager
    pub async fn acquire(
        &self,
        session_path: impl AsRef<Path>,
        timeout_ms: u64,
    ) -> Result<FileLock> {
        let path = session_path.as_ref().to_path_buf();

        // Check if we already hold this lock
        {
            let held = self.held.lock().unwrap();
            if held.contains(&path) {
                // Increment reference count
                drop(held);
                let mut held = self.held.lock().unwrap();
                *held.entry(path.clone()).or_insert(0) += 1;
            }
        }

        let lock = FileLock::acquire(&path, timeout_ms).await?;

        // Track the lock
        let mut held = self.held.lock().unwrap();
        *held.entry(path).or_insert(0) += 1;

        Ok(lock)
    }

    /// Release a lock through the manager
    pub async fn release(&self, lock: FileLock) -> Result<()> {
        let path = lock.lock_path.clone();
        lock.release().await?;

        let mut held = self.held.lock().unwrap();
        if let Some(count) = held.get_mut(&path) {
            *count -= 1;
            if *count == 0 {
                held.remove(&path);
            }
        }

        Ok(())
    }
}

impl Default for LockManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_lock_acquire_release() {
        let temp = TempDir::new().unwrap();
        let session_file = temp.path().join("test.jsonl");

        // Acquire lock
        let lock = FileLock::acquire(&session_file, 1000).await.unwrap();
        assert!(session_file.with_extension("lock").exists());

        // Release lock
        lock.release().await.unwrap();
        assert!(!session_file.with_extension("lock").exists());
    }

    #[tokio::test]
    async fn test_lock_timeout() {
        let temp = TempDir::new().unwrap();
        let session_file = temp.path().join("test.jsonl");

        // Create a lock file with a fake PID that doesn't exist
        let lock_path = session_file.with_extension("lock");
        let payload = LockPayload {
            pid: 99999, // Non-existent PID
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        fs::write(&lock_path, serde_json::to_string(&payload).unwrap())
            .await
            .unwrap();

        // Should be able to acquire (stale lock removal)
        let lock = FileLock::acquire(&session_file, 1000).await.unwrap();
        lock.release().await.unwrap();
    }
}
