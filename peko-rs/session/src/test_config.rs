//! Test configuration for session management
//!
//! This module provides test-friendly constants that can be overridden
//! via environment variables for integration testing. Only `rotate_bytes`
//! remains — the rest (prune_duration, max_sessions, lock_timeout_ms,
//! cache_ttl_ms, stale_lock_ms) were retired in B3 cleanup.

/// Get rotate bytes threshold with test override support
///
/// Environment variable: `SESSION_TEST_ROTATE_BYTES`
/// Default: 10MB
pub fn rotate_bytes() -> usize {
    if std::env::var("PEKO_TEST_MODE").is_ok() {
        std::env::var("SESSION_TEST_ROTATE_BYTES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1024usize) // 1KB for tests
    } else {
        10 * 1024 * 1024 // 10MB default
    }
}

/// Serializes tests that flip `PEKO_TEST_MODE` — the env var is
/// process-global, so two guard-holding tests running in parallel
/// would otherwise observe each other's set/restore (the first drop
/// removes the var out from under the second test). Both
/// `PekoTestModeGuard` copies (jsonl.rs, manager.rs) hold this lock
/// for the guard's lifetime.
#[cfg(test)]
pub static TEST_MODE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use crate::*;

    #[test]
    fn test_default_rotate_bytes() {
        let _lock = crate::test_config::TEST_MODE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // Without test mode, should return default
        assert_eq!(rotate_bytes(), 10 * 1024 * 1024);
    }
}
