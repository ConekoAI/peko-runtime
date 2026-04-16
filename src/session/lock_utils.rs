//! Lock Timeout Utilities
//!
//! Provides timeout-based lock acquisition to prevent deadlocks and
//! enable better diagnostics when contention occurs.
//!
//! # Usage
//!
//! ```rust,ignore
//! use crate::session::lock_utils::{try_write_lock, try_read_lock, DEFAULT_WRITE_TIMEOUT};
//!
//! // Acquire write lock with timeout
//! let guard = try_write_lock(&lock, DEFAULT_WRITE_TIMEOUT, "my_lock").await?;
//!
//! // Acquire read lock with timeout
//! let guard = try_read_lock(&lock, DEFAULT_READ_TIMEOUT, "my_lock").await?;
//! ```

use std::time::Duration;
use tokio::sync::RwLock;
use tokio::time::timeout;

/// Default timeout for write lock acquisition (5 seconds)
pub const DEFAULT_WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// Default timeout for read lock acquisition (10 seconds)
pub const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Error type for lock acquisition failures
#[derive(Debug, Clone, PartialEq)]
pub enum LockError {
    /// Lock acquisition timed out
    Timeout {
        /// Name of the lock for diagnostic purposes
        lock_name: String,
        /// Duration waited before timeout
        duration: Duration,
    },
    /// Lock was poisoned (holder panicked)
    Poisoned {
        /// Name of the lock
        lock_name: String,
    },
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LockError::Timeout {
                lock_name,
                duration,
            } => {
                write!(
                    f,
                    "Timeout acquiring lock '{}' after {:?}",
                    lock_name, duration
                )
            }
            LockError::Poisoned { lock_name } => {
                write!(f, "Lock '{}' was poisoned (holder panicked)", lock_name)
            }
        }
    }
}

impl std::error::Error for LockError {}

/// Convert LockError to anyhow::Error
///
/// Note: This is provided as a helper function rather than a From impl
/// to avoid conflicting with anyhow's blanket impl.
pub fn into_anyhow(err: LockError) -> anyhow::Error {
    anyhow::anyhow!(err.to_string())
}

/// Acquire write lock with timeout
///
/// Attempts to acquire a write lock on the given RwLock, waiting up to
/// the specified timeout duration. Returns an error if the timeout is
/// exceeded.
///
/// # Arguments
/// * `lock` - The RwLock to acquire
/// * `timeout_duration` - Maximum time to wait for the lock
/// * `lock_name` - Name of the lock for diagnostic error messages
///
/// # Example
/// ```rust,ignore
/// use crate::session::lock_utils::{try_write_lock, DEFAULT_WRITE_TIMEOUT};
///
/// let guard = try_write_lock(&my_lock, DEFAULT_WRITE_TIMEOUT, "session_cache").await?;
/// // Use guard...
/// ```
pub async fn try_write_lock<'a, T>(
    lock: &'a RwLock<T>,
    timeout_duration: Duration,
    lock_name: &str,
) -> Result<tokio::sync::RwLockWriteGuard<'a, T>, LockError> {
    match timeout(timeout_duration, lock.write()).await {
        Ok(guard) => Ok(guard),
        Err(_) => Err(LockError::Timeout {
            lock_name: lock_name.to_string(),
            duration: timeout_duration,
        }),
    }
}

/// Acquire read lock with timeout
///
/// Attempts to acquire a read lock on the given RwLock, waiting up to
/// the specified timeout duration. Returns an error if the timeout is
/// exceeded.
///
/// # Arguments
/// * `lock` - The RwLock to acquire
/// * `timeout_duration` - Maximum time to wait for the lock
/// * `lock_name` - Name of the lock for diagnostic error messages
///
/// # Example
/// ```rust,ignore
/// use crate::session::lock_utils::{try_read_lock, DEFAULT_READ_TIMEOUT};
///
/// let guard = try_read_lock(&my_lock, DEFAULT_READ_TIMEOUT, "session_cache").await?;
/// // Use guard (read-only)...
/// ```
pub async fn try_read_lock<'a, T>(
    lock: &'a RwLock<T>,
    timeout_duration: Duration,
    lock_name: &str,
) -> Result<tokio::sync::RwLockReadGuard<'a, T>, LockError> {
    match timeout(timeout_duration, lock.read()).await {
        Ok(guard) => Ok(guard),
        Err(_) => Err(LockError::Timeout {
            lock_name: lock_name.to_string(),
            duration: timeout_duration,
        }),
    }
}

/// Acquire write lock with default timeout
///
/// Convenience wrapper that uses [`DEFAULT_WRITE_TIMEOUT`].
pub async fn try_write_lock_default<'a, T>(
    lock: &'a RwLock<T>,
    lock_name: &str,
) -> Result<tokio::sync::RwLockWriteGuard<'a, T>, LockError> {
    try_write_lock(lock, DEFAULT_WRITE_TIMEOUT, lock_name).await
}

/// Acquire read lock with default timeout
///
/// Convenience wrapper that uses [`DEFAULT_READ_TIMEOUT`].
pub async fn try_read_lock_default<'a, T>(
    lock: &'a RwLock<T>,
    lock_name: &str,
) -> Result<tokio::sync::RwLockReadGuard<'a, T>, LockError> {
    try_read_lock(lock, DEFAULT_READ_TIMEOUT, lock_name).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::time::{sleep, Duration};

    #[tokio::test]
    async fn test_try_write_lock_success() {
        let lock = RwLock::new(42);

        let guard = try_write_lock(&lock, Duration::from_secs(1), "test_lock").await;
        assert!(guard.is_ok());
        assert_eq!(*guard.unwrap(), 42);
    }

    #[tokio::test]
    async fn test_try_read_lock_success() {
        let lock = RwLock::new(42);

        let guard = try_read_lock(&lock, Duration::from_secs(1), "test_lock").await;
        assert!(guard.is_ok());
        assert_eq!(*guard.unwrap(), 42);
    }

    #[tokio::test]
    async fn test_try_write_lock_timeout() {
        let lock = Arc::new(RwLock::new(42));
        let lock2 = lock.clone();

        // Hold write lock in a separate task
        let _holder = tokio::spawn(async move {
            let _guard = lock2.write().await;
            sleep(Duration::from_secs(5)).await;
        });

        // Give the holder time to acquire
        sleep(Duration::from_millis(50)).await;

        // Try to acquire with short timeout - should fail
        let result = try_write_lock(&lock, Duration::from_millis(100), "test_lock").await;

        assert!(matches!(
            result,
            Err(LockError::Timeout {
                lock_name,
                duration
            }) if lock_name == "test_lock" && duration == Duration::from_millis(100)
        ));
    }

    #[tokio::test]
    async fn test_try_read_lock_timeout() {
        let lock = Arc::new(RwLock::new(42));
        let lock2 = lock.clone();

        // Hold write lock in a separate task (prevents any read locks)
        let _holder = tokio::spawn(async move {
            let _guard = lock2.write().await;
            sleep(Duration::from_secs(5)).await;
        });

        // Give the holder time to acquire
        sleep(Duration::from_millis(50)).await;

        // Try to acquire read lock with short timeout - should fail
        let result = try_read_lock(&lock, Duration::from_millis(100), "test_lock").await;

        assert!(matches!(
            result,
            Err(LockError::Timeout {
                lock_name,
                duration
            }) if lock_name == "test_lock" && duration == Duration::from_millis(100)
        ));
    }

    #[tokio::test]
    async fn test_multiple_read_locks_allowed() {
        let lock = Arc::new(RwLock::new(42));

        // Multiple read locks should be allowed simultaneously
        let guard1 = try_read_lock(&lock, Duration::from_secs(1), "test_lock").await;
        let guard2 = try_read_lock(&lock, Duration::from_secs(1), "test_lock").await;
        let guard3 = try_read_lock(&lock, Duration::from_secs(1), "test_lock").await;

        assert!(guard1.is_ok());
        assert!(guard2.is_ok());
        assert!(guard3.is_ok());
    }

    #[tokio::test]
    async fn test_default_timeouts() {
        // Test write lock with default timeout
        let lock = RwLock::new(42);
        let write_result = try_write_lock_default(&lock, "test_lock").await;
        assert!(write_result.is_ok());
        // Drop the write guard explicitly
        drop(write_result);

        // Test read lock with default timeout (after write guard is dropped)
        let read_result = try_read_lock_default(&lock, "test_lock").await;
        assert!(read_result.is_ok());
    }

    #[tokio::test]
    async fn test_lock_error_display() {
        let timeout_err = LockError::Timeout {
            lock_name: "my_lock".to_string(),
            duration: Duration::from_secs(5),
        };
        assert_eq!(
            timeout_err.to_string(),
            "Timeout acquiring lock 'my_lock' after 5s"
        );

        let poisoned_err = LockError::Poisoned {
            lock_name: "my_lock".to_string(),
        };
        assert_eq!(
            poisoned_err.to_string(),
            "Lock 'my_lock' was poisoned (holder panicked)"
        );
    }

    #[tokio::test]
    async fn test_lock_error_into_anyhow() {
        let err = LockError::Timeout {
            lock_name: "test".to_string(),
            duration: Duration::from_secs(1),
        };
        let anyhow_err = into_anyhow(err);
        assert!(anyhow_err.to_string().contains("Timeout acquiring lock"));
    }
}
