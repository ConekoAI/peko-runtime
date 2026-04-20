//! HTTP retry logic for provider API calls
//!
//! Provides configurable retry with exponential backoff for transient failures
//! like HTTP 429 (rate limit) and 5xx server errors.

use std::collections::HashSet;
use std::time::Duration;
use tracing::{debug, info, warn};

/// Retry policy configuration
#[derive(Clone, Debug)]
pub struct RetryPolicy {
    /// Maximum number of retry attempts (0 = no retries)
    pub max_retries: u32,
    /// Base delay between retries
    pub base_delay: Duration,
    /// Maximum delay cap
    pub max_delay: Duration,
    /// Exponential backoff multiplier
    pub backoff_multiplier: f64,
    /// HTTP status codes that trigger retry
    pub retryable_status_codes: HashSet<u16>,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(30),
            backoff_multiplier: 2.0,
            retryable_status_codes: [429, 500, 502, 503, 504, 529].into_iter().collect(),
        }
    }
}

impl RetryPolicy {
    /// Create a policy from `max_retries` and `base_delay_ms` (for `ProviderConfig` compatibility)
    #[must_use]
    pub fn from_config(max_retries: u32, base_delay_ms: u64) -> Option<Self> {
        if max_retries == 0 {
            return None;
        }
        Some(Self {
            max_retries,
            base_delay: Duration::from_millis(base_delay_ms),
            ..Default::default()
        })
    }

    /// Calculate delay for a specific attempt (0-indexed)
    #[must_use]
    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        let multiplier = self.backoff_multiplier.powi(attempt as i32);
        let delay = self.base_delay.mul_f64(multiplier);
        delay.min(self.max_delay)
    }

    /// Check if a status code should trigger a retry
    #[must_use]
    pub fn is_retryable_status(&self, status: u16) -> bool {
        self.retryable_status_codes.contains(&status)
    }
}

/// Trait for errors that can be classified as retryable
pub trait RetryableError {
    /// Returns true if this error warrants a retry
    fn is_retryable(&self) -> bool;
    /// Extract HTTP status code if available
    fn http_status(&self) -> Option<u16>;
}

impl RetryableError for anyhow::Error {
    fn is_retryable(&self) -> bool {
        // Check if error message contains retryable HTTP status codes
        let msg = self.to_string();

        // Check for explicit status codes in error message
        // Format: "HTTP error 429: ..." or "429 Too Many Requests"
        for code in [429u16, 500, 502, 503, 504, 529] {
            if msg.contains(&format!(" {code}"))
                || msg.contains(&format!("HTTP error {code}"))
                || msg.contains(&format!("status {code}"))
            {
                return true;
            }
        }

        // Check for timeout/network-related errors
        if msg.contains("timeout")
            || msg.contains("connection")
            || msg.contains("reset")
            || msg.contains("refused")
        {
            return true;
        }

        false
    }

    fn http_status(&self) -> Option<u16> {
        let msg = self.to_string();

        // Try to extract status code from common error patterns
        for code in [429u16, 500, 502, 503, 504, 408, 504] {
            if msg.contains(&format!(" {code}"))
                || msg.contains(&format!("HTTP error {code}"))
                || msg.contains(&format!("status {code}"))
            {
                return Some(code);
            }
        }

        None
    }
}

/// Executor for retryable operations
pub struct RetryExecutor;

impl RetryExecutor {
    /// Execute an operation with retry logic
    pub async fn execute<F, Fut, T>(
        policy: &RetryPolicy,
        operation_name: &str,
        operation: F,
    ) -> anyhow::Result<T>
    where
        F: Fn() -> Fut + Send + Sync,
        Fut: std::future::Future<Output = anyhow::Result<T>> + Send,
    {
        let mut attempt: u32 = 0;

        loop {
            match operation().await {
                Ok(result) => {
                    if attempt > 0 {
                        debug!(
                            "{} succeeded after {} attempt(s)",
                            operation_name,
                            attempt + 1
                        );
                    }
                    return Ok(result);
                }
                Err(e) => {
                    let should_retry = attempt < policy.max_retries && e.is_retryable();

                    if !should_retry {
                        if attempt > 0 {
                            debug!(
                                "{} exhausted retries (attempt {}/{})",
                                operation_name,
                                attempt + 1,
                                policy.max_retries + 1
                            );
                        }
                        return Err(e);
                    }

                    let delay = policy.delay_for_attempt(attempt);
                    let status_info = e
                        .http_status()
                        .map(|s| format!(" (HTTP {s})"))
                        .unwrap_or_default();

                    // Log retry attempts at info level, warn only on final failure
                    if attempt < policy.max_retries {
                        info!(
                            "{} returned{} (attempt {}/{}), retrying in {:?}",
                            operation_name,
                            status_info,
                            attempt + 1,
                            policy.max_retries + 1,
                            delay
                        );
                    } else {
                        warn!(
                            "{} failed{} after {} attempts: {}",
                            operation_name,
                            status_info,
                            policy.max_retries + 1,
                            e
                        );
                    }

                    tokio::time::sleep(delay).await;
                    attempt += 1;
                }
            }
        }
    }
}
