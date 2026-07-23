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

/// Re-export shim. Canonical home is
/// `peko_provider_api::RetryableError` (Phase 9b.N.5b.8 lift).
///
/// The trait + `impl RetryableError for anyhow::Error` were lifted
/// from this module so the agentic loop (now in `peko-engine`) can
/// classify errors without taking a `peko-engine → root` dep edge.
/// The companion `RetryPolicy` (with `base_delay` /
/// `backoff_multiplier` / `max_delay`) and the `RetryExecutor` below
/// stay in root because they're coupled to
/// `crate::providers::transport::HttpClient`.
pub use peko_provider_api::RetryableError;

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

                    // Prefer the server's `Retry-After` hint when the
                    // upstream sent one (RFC 7231 §7.1.3). It is almost
                    // always a more accurate throttle window than our
                    // computed exponential backoff, and respecting it is
                    // what makes the difference between "engine overloaded"
                    // windows (Kimi, Anthropic) and the test succeeding.
                    // Cap at `max_delay` (default 30s) so a stale or
                    // hostile header can't pin us indefinitely.
                    let delay = e
                        .retry_after()
                        .map(|d| d.min(policy.max_delay))
                        .unwrap_or_else(|| policy.delay_for_attempt(attempt));
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    /// `RetryableError::retry_after` on `anyhow::Error` parses the
    /// `(retry_after=Ns)` token that `HttpClient::classify_http_error`
    /// embeds in the message. Round-trip tests cover each shape we
    /// emit or accept.
    #[test]
    fn anyhow_retry_after_parses_embedded_value() {
        let e = anyhow::anyhow!("HTTP error 429 (retry_after=7s): engine overloaded");
        assert_eq!(e.retry_after(), Some(Duration::from_secs(7)));
    }

    #[test]
    fn anyhow_retry_after_zero_is_treated_as_absent() {
        // Zero seconds is meaningless as a hint and would cause an
        // infinite-tight retry loop. The parser must drop it so the
        // executor falls back to its computed backoff.
        let e = anyhow::anyhow!("HTTP error 503 (retry_after=0s): try later");
        assert_eq!(e.retry_after(), None);
    }

    #[test]
    fn anyhow_retry_after_absent_returns_none() {
        // The pre-fix message format (no retry_after token) must still
        // parse cleanly — this is the no-regression test for providers
        // that don't emit the header.
        let e = anyhow::anyhow!("HTTP error 429: engine overloaded");
        assert_eq!(e.retry_after(), None);
    }

    #[test]
    fn anyhow_retry_after_garbage_value_returns_none() {
        // A malformed `(retry_after=abc)` token should NOT panic and
        // should fall back to the computed backoff.
        let e = anyhow::anyhow!("HTTP error 500 (retry_after=abc): oops");
        assert_eq!(e.retry_after(), None);
    }

    /// Wall-clock proof that the executor honors server-suggested delay.
    /// We use a hint longer than the computed backoff (5s hint vs the
    /// default 1s base), so the only way the test can complete in
    /// ~5s is if `retry_after()` is taking precedence. A short ceiling
    /// on the assertion catches regressions where the executor falls
    /// back to the wrong branch.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn executor_uses_retry_after_when_present() {
        let calls = Arc::new(AtomicU32::new(0));
        let calls_inner = calls.clone();
        let policy = RetryPolicy {
            max_retries: 1,
            base_delay: Duration::from_millis(50),
            ..RetryPolicy::default()
        };
        let start = std::time::Instant::now();
        let result: anyhow::Result<()> = RetryExecutor::execute(&policy, "test", || {
            let calls = calls_inner.clone();
            async move {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    Err(anyhow::anyhow!(
                        "HTTP error 429 (retry_after=2s): engine overloaded"
                    ))
                } else {
                    Ok(())
                }
            }
        })
        .await;
        let elapsed = start.elapsed();
        assert!(result.is_ok(), "executor should have retried and succeeded");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        // 2s server-suggested delay must dominate; allow generous slack
        // for scheduler jitter but fail loudly if computed backoff (50ms)
        // snuck in.
        assert!(
            elapsed >= Duration::from_millis(1900),
            "executor returned in {elapsed:?} — looks like computed backoff (50ms) \
             won over the server's 2s Retry-After hint"
        );
        assert!(
            elapsed < Duration::from_secs(4),
            "executor took {elapsed:?} — far longer than the 2s Retry-After hint"
        );
    }

    /// Wall-clock proof that the executor caps a huge server hint at
    /// `max_delay`. We configure max_delay=200ms and emit a Retry-After
    /// of 5s — the call must complete well under 5s.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn executor_caps_retry_after_at_max_delay() {
        let policy = RetryPolicy {
            max_retries: 1,
            base_delay: Duration::from_secs(5),
            max_delay: Duration::from_millis(200),
            ..RetryPolicy::default()
        };
        let calls = Arc::new(AtomicU32::new(0));
        let calls_inner = calls.clone();
        let start = std::time::Instant::now();
        let result: anyhow::Result<()> = RetryExecutor::execute(&policy, "test", || {
            let calls = calls_inner.clone();
            async move {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    // Hint of 5s would normally dominate; the cap at
                    // 200ms must shrink it.
                    Err(anyhow::anyhow!(
                        "HTTP error 429 (retry_after=5s): engine overloaded"
                    ))
                } else {
                    Ok(())
                }
            }
        })
        .await;
        let elapsed = start.elapsed();
        assert!(result.is_ok(), "executor should have retried and succeeded");
        // The cap means we waited ~200ms, NOT 5s. If this assertion
        // fails, the cap is being bypassed — that lets a hostile or
        // stale header pin us for arbitrary durations.
        assert!(
            elapsed < Duration::from_secs(2),
            "executor took {elapsed:?} — the max_delay cap is not being applied to Retry-After"
        );
    }

    /// Without a server hint, the executor must fall back to its
    /// computed exponential backoff. This is the no-regression path
    /// for providers that don't send `Retry-After`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn executor_falls_back_to_computed_backoff_when_no_hint() {
        let policy = RetryPolicy {
            max_retries: 1,
            base_delay: Duration::from_millis(100),
            ..RetryPolicy::default()
        };
        let calls = Arc::new(AtomicU32::new(0));
        let calls_inner = calls.clone();
        let start = std::time::Instant::now();
        let result: anyhow::Result<()> = RetryExecutor::execute(&policy, "test", || {
            let calls = calls_inner.clone();
            async move {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    Err(anyhow::anyhow!("HTTP error 429: engine overloaded"))
                } else {
                    Ok(())
                }
            }
        })
        .await;
        let elapsed = start.elapsed();
        assert!(result.is_ok());
        // ~100ms computed backoff (no server hint to override it).
        assert!(
            elapsed >= Duration::from_millis(90),
            "computed backoff should have waited ~100ms, took {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "executor took {elapsed:?}, suspiciously long for a 100ms backoff"
        );
    }
}
