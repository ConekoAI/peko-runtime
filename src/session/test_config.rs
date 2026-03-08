//! Test configuration for session management
//!
//! This module provides test-friendly constants that can be overridden
//! via environment variables for integration testing.

/// Get prune duration with test override support
/// 
/// Environment variable: `SESSION_TEST_PRUNE_DAYS`
/// Default: 30 days
pub fn prune_duration() -> std::time::Duration {
    let days = if std::env::var("PEKOBOT_TEST_MODE").is_ok() {
        std::env::var("SESSION_TEST_PRUNE_DAYS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1u64)
    } else {
        30u64
    };
    std::time::Duration::from_secs(days * 24 * 60 * 60)
}

/// Get max sessions per agent with test override support
///
/// Environment variable: `SESSION_TEST_MAX_SESSIONS`
/// Default: 500
pub fn max_sessions() -> usize {
    if std::env::var("PEKOBOT_TEST_MODE").is_ok() {
        std::env::var("SESSION_TEST_MAX_SESSIONS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(3usize)
    } else {
        500usize
    }
}

/// Get rotate bytes threshold with test override support
///
/// Environment variable: `SESSION_TEST_ROTATE_BYTES`
/// Default: 10MB
pub fn rotate_bytes() -> usize {
    if std::env::var("PEKOBOT_TEST_MODE").is_ok() {
        std::env::var("SESSION_TEST_ROTATE_BYTES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1024usize) // 1KB for tests
    } else {
        10 * 1024 * 1024 // 10MB default
    }
}

/// Get lock timeout with test override support
///
/// Environment variable: `SESSION_TEST_LOCK_TIMEOUT_MS`
/// Default: 10000ms
pub fn lock_timeout_ms() -> u64 {
    if std::env::var("PEKOBOT_TEST_MODE").is_ok() {
        std::env::var("SESSION_TEST_LOCK_TIMEOUT_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(100u64) // 100ms for tests
    } else {
        10000u64 // 10s default
    }
}

/// Get cache TTL with test override support
///
/// Environment variable: `SESSION_TEST_CACHE_TTL_MS`
/// Default: 45000ms (45s)
pub fn cache_ttl_ms() -> u64 {
    if std::env::var("PEKOBOT_TEST_MODE").is_ok() {
        std::env::var("SESSION_TEST_CACHE_TTL_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(100u64) // 100ms for tests
    } else {
        45000u64 // 45s default
    }
}

/// Get stale lock threshold with test override support
///
/// Environment variable: `SESSION_TEST_STALE_LOCK_MS`
/// Default: 30000ms (30s)
pub fn stale_lock_ms() -> u64 {
    if std::env::var("PEKOBOT_TEST_MODE").is_ok() {
        std::env::var("SESSION_TEST_STALE_LOCK_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(500u64) // 500ms for tests
    } else {
        30000u64 // 30s default
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_defaults() {
        // Without test mode, should return defaults
        assert_eq!(max_sessions(), 500);
        assert_eq!(rotate_bytes(), 10 * 1024 * 1024);
        assert_eq!(lock_timeout_ms(), 10000);
    }

    #[test]
    fn test_prune_duration() {
        let duration = prune_duration();
        // Should be approximately 30 days
        assert!(duration.as_secs() >= 30 * 24 * 60 * 60);
    }
}
