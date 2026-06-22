//! In-memory sliding-window rate limiter

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

/// A single rate-limit bucket entry
pub struct RateLimitEntry {
    /// Timestamp of the current window start
    window_start: Instant,
    /// Request count in the current window
    count: u32,
    /// Burst allowance remaining
    burst_remaining: u32,
}

/// In-memory sliding-window rate limiter
///
/// Tracks per-identity request counts. Restarting the daemon resets counters.
#[derive(Clone)]
pub struct RateLimiter {
    inner: Arc<RwLock<HashMap<String, RateLimitEntry>>>,
    /// Requests per minute for JWT users
    jwt_limit: u32,
    /// Requests per minute for API keys
    api_key_limit: u32,
    /// Burst allowance for JWT users
    jwt_burst: u32,
    /// Burst allowance for API keys
    api_key_burst: u32,
    /// Window duration
    window: Duration,
}

impl RateLimiter {
    /// Create a new rate limiter with the given configuration
    #[must_use]
    pub fn new(jwt_limit: u32, api_key_limit: u32, jwt_burst: u32, api_key_burst: u32) -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
            jwt_limit,
            api_key_limit,
            jwt_burst,
            api_key_burst,
            window: Duration::from_mins(1),
        }
    }

    /// Create with default limits
    #[must_use]
    pub fn default_limits() -> Self {
        Self::new(30, 100, 10, 20)
    }

    /// Check if a request from the given bucket is allowed.
    ///
    /// Returns `true` if the request should be allowed, `false` if rate limited.
    pub async fn check(&self, bucket: &str, is_jwt: bool) -> bool {
        let limit = if is_jwt {
            self.jwt_limit
        } else {
            self.api_key_limit
        };
        let burst = if is_jwt {
            self.jwt_burst
        } else {
            self.api_key_burst
        };

        let mut map = self.inner.write().await;
        let now = Instant::now();

        match map.get_mut(bucket) {
            Some(entry) => {
                if now.duration_since(entry.window_start) >= self.window {
                    // New window
                    entry.window_start = now;
                    entry.count = 1;
                    entry.burst_remaining = burst;
                    true
                } else if entry.count < limit {
                    entry.count += 1;
                    true
                } else if entry.burst_remaining > 0 {
                    entry.burst_remaining -= 1;
                    true
                } else {
                    false
                }
            }
            None => {
                map.insert(
                    bucket.to_string(),
                    RateLimitEntry {
                        window_start: now,
                        count: 1,
                        burst_remaining: burst,
                    },
                );
                true
            }
        }
    }

    /// Get current count for a bucket (for diagnostics)
    pub async fn current_count(&self, bucket: &str) -> u32 {
        let map = self.inner.read().await;
        map.get(bucket).map(|e| e.count).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_rate_limit_allows_under_limit() {
        let limiter = RateLimiter::new(5, 10, 2, 3);
        for _ in 0..5 {
            assert!(limiter.check("user:a", true).await);
        }
    }

    #[tokio::test]
    async fn test_rate_limit_blocks_over_limit() {
        let limiter = RateLimiter::new(2, 10, 0, 3);
        assert!(limiter.check("user:a", true).await);
        assert!(limiter.check("user:a", true).await);
        assert!(!limiter.check("user:a", true).await);
    }

    #[tokio::test]
    async fn test_rate_limit_burst() {
        let limiter = RateLimiter::new(2, 10, 1, 3);
        assert!(limiter.check("user:a", true).await);
        assert!(limiter.check("user:a", true).await);
        // Third request uses burst
        assert!(limiter.check("user:a", true).await);
        // Fourth request blocked
        assert!(!limiter.check("user:a", true).await);
    }

    #[tokio::test]
    async fn test_rate_limit_isolated_buckets() {
        let limiter = RateLimiter::new(2, 10, 0, 3);
        assert!(limiter.check("user:a", true).await);
        assert!(limiter.check("user:b", true).await);
        assert!(limiter.check("user:a", true).await);
        assert!(limiter.check("user:b", true).await);
        // Both at limit now
        assert!(!limiter.check("user:a", true).await);
        assert!(!limiter.check("user:b", true).await);
    }

    #[tokio::test]
    async fn test_rate_limit_api_key_vs_jwt_limits() {
        let limiter = RateLimiter::new(1, 5, 0, 0);
        // JWT limit is 1
        assert!(limiter.check("jwt:user", true).await);
        assert!(!limiter.check("jwt:user", true).await);

        // API key limit is 5
        for _ in 0..5 {
            assert!(limiter.check("api:key", false).await);
        }
        assert!(!limiter.check("api:key", false).await);
    }
}
