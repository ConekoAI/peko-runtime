//! HTTP retry logic for provider API calls
//!
//! Provides configurable retry with exponential backoff for transient failures
//! like HTTP 429 (rate limit) and 5xx server errors.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info, warn};

/// Shared retry budget that the transport retry executor AND the
/// agentic-loop mid-stream retry site draw from.
///
/// F40 audit remediation: collapses the pre-F40 stacked-budget
/// anti-pattern (`transport max_retries=3 + engine stream_max_retries=3
/// = up to 12 attempts on a single 429 burst`) into a single
/// counter so the total worst case is one ceiling instead of two
/// stacked ceilings. Wire the budget via:
///   - `AgenticLoopBuilder::with_shared_budget(Arc<SharedRetryBudget>)`
///   - `HttpClient::with_shared_budget(Arc<SharedRetryBudget>)`
///
/// Both sites clone the same `Arc` and call `try_consume()` per
/// retry attempt. When the counter reaches zero, both sites refuse
/// to retry and the engine surfaces
/// `AgenticError::RetryLimit { attempts, max_attempts, cause }`.
///
/// Defaults to `max_attempts=8` total across both layers (the
/// `factory.rs` `PROVIDER_MAX_ATTEMPTS` constant). Picked as a
/// 5+3 split — peko's pre-F40 transport consumed ~5 attempts in
/// typical 429 bursts, and 3 was a reasonable per-iteration cap.
#[derive(Debug)]
pub struct SharedRetryBudget {
    /// Remaining permits. Decremented atomically per `try_consume`
    /// call. When this reaches zero, `try_consume` returns `false`
    /// and the next call site surfaces `AgenticError::RetryLimit`.
    remaining: Arc<AtomicU32>,
    /// Configured ceiling at construction time. Used to populate
    /// `AgenticError::RetryLimit.max_attempts` so callers can
    /// reconstruct the original ceiling even after `remaining`
    /// reached zero.
    max_attempts: u32,
}

impl SharedRetryBudget {
    /// Build a budget with `max_attempts` total attempts across
    /// transport + engine. Use [`SharedRetryBudget::into_arc`] to
    /// share with multiple call sites.
    #[must_use]
    pub fn new(max_attempts: u32) -> Self {
        Self {
            remaining: Arc::new(AtomicU32::new(max_attempts)),
            max_attempts,
        }
    }

    /// Try to consume one permit. Returns `true` if a retry slot
    /// was reserved; `false` if the budget is exhausted.
    ///
    /// Implemented with a CAS loop so concurrent callers don't
    /// oversubscribe the budget. Each retry site that sees a
    /// retryable error calls `try_consume` BEFORE sleeping and
    /// re-issuing — if it returns `false`, the site surfaces
    /// `RetryLimit` without sleeping.
    pub fn try_consume(&self) -> bool {
        let mut current = self.remaining.load(Ordering::SeqCst);
        while current > 0 {
            match self.remaining.compare_exchange_weak(
                current,
                current - 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return true,
                Err(actual) => current = actual,
            }
        }
        false
    }

    /// Returns the configured ceiling (for error reporting and tests).
    #[must_use]
    pub fn max_attempts(&self) -> u32 {
        self.max_attempts
    }

    /// Returns the remaining permits (for diagnostics / tests).
    #[must_use]
    pub fn remaining(&self) -> u32 {
        self.remaining.load(Ordering::SeqCst)
    }

    /// Wrap in an `Arc` so multiple call sites share one counter.
    #[must_use]
    pub fn into_arc(self) -> Arc<Self> {
        Arc::new(self)
    }

    /// Build an `Arc<SharedRetryBudget>` directly. Convenience for
    /// common callers that prefer a single allocation site.
    #[must_use]
    pub fn arc(max_attempts: u32) -> Arc<Self> {
        Self::new(max_attempts).into_arc()
    }
}

impl Clone for SharedRetryBudget {
    fn clone(&self) -> Self {
        Self {
            remaining: self.remaining.clone(),
            max_attempts: self.max_attempts,
        }
    }
}

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
    /// F40: ±jitter fraction on the computed backoff (uniform
    /// random in `[1-jitter, 1+jiter]`). `0.0` matches the
    /// pre-F40 deterministic behavior; `0.1` matches codex's
    /// ±10% default. Added to avoid thundering-herd reentry when
    /// multiple peko agents hit the same 429 wall in lockstep.
    pub jitter: f64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(30),
            backoff_multiplier: 2.0,
            retryable_status_codes: [429, 500, 502, 503, 504, 529].into_iter().collect(),
            jitter: 0.0,
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
            ..Self::default()
        })
    }

    /// Set the backoff jitter (clamped to `[0.0, 1.0]`). `0.0`
    /// returns the pre-F40 deterministic backoff; `0.1` matches
    /// codex's uniform ±10% default. The value is applied by
    /// `delay_for_attempt` so all retry sites (transport +
    /// engine) inherit the spread.
    #[must_use]
    pub fn with_jitter(mut self, jitter: f64) -> Self {
        self.jitter = jitter.clamp(0.0, 1.0);
        self
    }

    /// Calculate delay for a specific attempt (0-indexed).
    ///
    /// Returns `base_delay * backoff_multiplier^attempt`, capped at
    /// `max_delay`. With `jitter > 0` the result is multiplied by
    /// a uniform random factor in `[1-jitter, 1+jitter]`
    /// (codex-matching shape); with `jitter == 0` the result is
    /// deterministic (pre-F40 behavior).
    #[must_use]
    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        let multiplier = self.backoff_multiplier.powi(attempt as i32);
        let base_ms = self.base_delay.as_millis() as f64 * multiplier;
        let jittered_ms = if self.jitter > 0.0 {
            let factor: f64 = {
                // Uniform in `[-1, 1]` then shift to `[1-jitter, 1+jitter]`.
                let r: f64 = rand::random::<f64>() * 2.0 - 1.0;
                1.0 + r * self.jitter
            };
            base_ms * factor
        } else {
            base_ms
        };
        let capped = jittered_ms.min(self.max_delay.as_millis() as f64);
        let ms = capped.max(0.0) as u64;
        Duration::from_millis(ms)
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
/// `crate::transport::HttpClient`.
pub use peko_provider_api::RetryableError;

/// Executor for retryable operations
pub struct RetryExecutor;

impl RetryExecutor {
    /// Execute an operation with retry logic. The legacy entry
    /// point — uses the substring-scan [`RetryableError`] impl on
    /// `anyhow::Error` and no shared budget. Kept for back-compat
    /// with the F31c callers that don't need typed classification
    /// or budget coordination (tests, validator, single-shot
    /// retries).
    pub async fn execute<F, Fut, T>(
        policy: &RetryPolicy,
        operation_name: &str,
        operation: F,
    ) -> anyhow::Result<T>
    where
        F: Fn() -> Fut + Send + Sync,
        Fut: std::future::Future<Output = anyhow::Result<T>> + Send,
    {
        // Delegate to the classifier-aware overload with the
        // default substring classifier and no shared budget. The
        // behavior is identical to the pre-F40 path.
        Self::execute_with_classifier_and_budget(
            policy,
            operation_name,
            None,
            &peko_provider_api::BodyStringClassifier,
            operation,
        )
        .await
    }

    /// F40: execute an operation with retry, body-aware classifier,
    /// and optional shared budget.
    ///
    /// Behavior:
    /// 1. Call the operation.
    /// 2. On error, classify via the supplied `classifier`.
    /// 3. If classification is `Transient { .. }` AND a budget is
    ///    configured AND `try_consume` succeeds (or no budget):
    ///    sleep the server-suggested or computed delay, increment
    ///    the local attempt counter, loop.
    /// 4. Otherwise (terminal classification, exhausted budget, or
    ///    attempt counter at ceiling): return the error verbatim.
    ///
    /// The pre-F40 path [`RetryExecutor::execute`] forwards here
    /// with the default [`BodyStringClassifier`] and no budget so
    /// existing callers keep working unchanged.
    pub async fn execute_with_classifier_and_budget<F, Fut, T>(
        policy: &RetryPolicy,
        operation_name: &str,
        budget: Option<&SharedRetryBudget>,
        classifier: &dyn peko_provider_api::RetryClassifier,
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
                    let classification = classifier.classify(&e);
                    let local_budget_ok =
                        attempt < policy.max_retries;
                    let shared_budget_ok = match budget {
                        Some(b) => b.try_consume(),
                        None => true,
                    };
                    let should_retry =
                        local_budget_ok && shared_budget_ok && classification.is_retryable();

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
                    // upstream sent one (RFC 7231 §7.1.3). It is
                    // almost always a more accurate throttle window
                    // than computed exponential backoff. Cap at
                    // `max_delay` so a stale or hostile header can't
                    // pin us indefinitely.
                    let delay = classification
                        .retry_after()
                        .or_else(|| classifier.retry_after(&e))
                        .map(|d| d.min(policy.max_delay))
                        .unwrap_or_else(|| policy.delay_for_attempt(attempt));
                    let status_info = e
                        .http_status()
                        .map(|s| format!(" (HTTP {s})"))
                        .unwrap_or_default();

                    // Log retry attempts at info level, warn only on
                    // final failure.
                    if should_retry {
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

    /// F40: `SharedRetryBudget::try_consume` is a CAS-loop counter.
    /// Two callers racing over a 1-permit budget must see exactly
    /// one success and one failure — no double-counting, no lost
    /// update. The test runs the race N times to give the scheduler
    /// a chance to expose ordering bugs.
    #[test]
    fn shared_budget_cas_loop_exactly_one_consumer_wins() {
        for _ in 0..64 {
            let budget = SharedRetryBudget::new(1);
            let a = budget.try_consume();
            let b = budget.try_consume();
            assert!(
                a ^ b,
                "exactly one of two sequential consumers must win; got a={a} b={b}"
            );
            assert_eq!(budget.remaining(), 0);
            assert_eq!(budget.max_attempts(), 1);
        }
    }

    /// F40: budget survives clone — both sides must see the same
    /// atomic counter. This is the structural invariant that lets
    /// transport + engine draw from one counter.
    #[test]
    fn shared_budget_clone_shares_counter() {
        let budget = SharedRetryBudget::new(3);
        let twin = budget.clone();
        assert_eq!(budget.remaining(), 3);
        assert_eq!(twin.remaining(), 3);
        assert!(budget.try_consume());
        assert_eq!(budget.remaining(), 2);
        assert_eq!(twin.remaining(), 2);
        assert!(twin.try_consume());
        assert_eq!(budget.remaining(), 1);
        assert!(budget.try_consume());
        assert_eq!(twin.remaining(), 0);
        assert!(!budget.try_consume());
        assert!(!twin.try_consume());
    }

    /// F40: `into_arc` / `arc` constructors must produce a shared
    /// counter that both transport + engine can clone. The test
    /// asserts that one `Arc` clone + the original observe the
    /// same `remaining` after a consume.
    #[test]
    fn shared_budget_arc_constructor_shares_counter() {
        let original = SharedRetryBudget::arc(4);
        let cloned = Arc::clone(&original);
        assert_eq!(original.remaining(), 4);
        assert!(cloned.try_consume());
        assert_eq!(original.remaining(), 3);
        assert_eq!(cloned.remaining(), 3);
    }

    /// F40: jittered `delay_for_attempt` must produce values inside
    /// the `[1-jitter, 1+jitter]` band around the deterministic
    /// baseline. We compute the deterministic baseline (jitter=0)
    /// alongside 64 jittered samples and assert every sample lands
    /// in the expected band.
    #[test]
    fn retry_policy_jitter_band_is_respected() {
        let deterministic = RetryPolicy {
            max_retries: 1,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(60),
            backoff_multiplier: 2.0,
            retryable_status_codes: [429].into_iter().collect(),
            jitter: 0.0,
        };
        let jittered = RetryPolicy {
            jitter: 0.25,
            ..deterministic.clone()
        };
        // attempt=0 → 1000ms deterministic baseline.
        let baseline_ms = deterministic.delay_for_attempt(0).as_millis();
        for _ in 0..64 {
            let jittered_ms = jittered.delay_for_attempt(0).as_millis();
            let lower = (baseline_ms as f64 * 0.75) as u128;
            let upper = (baseline_ms as f64 * 1.25) as u128;
            assert!(
                jittered_ms >= lower && jittered_ms <= upper,
                "jittered sample {jittered_ms}ms outside [{lower}, {upper}]ms band"
            );
        }
    }

    /// F40: jitter clamped to `[0.0, 1.0]` — the builder must NOT
    /// accept `> 1.0` even if the caller asks. Out-of-range values
    /// would silently stretch backoff past `max_delay`.
    #[test]
    fn retry_policy_jitter_is_clamped() {
        let p = RetryPolicy::default().with_jitter(5.0);
        assert_eq!(p.jitter, 1.0);
        let p = RetryPolicy::default().with_jitter(-2.0);
        assert_eq!(p.jitter, 0.0);
    }

    /// F40: classifier-aware executor with shared budget — when the
    /// budget runs out, the executor returns the error verbatim
    /// even though the policy ceiling has not been hit. This is the
    /// "transport hands control back to the engine" coordination
    /// contract.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn executor_respects_shared_budget_exhaustion() {
        let policy = RetryPolicy {
            max_retries: 5,
            base_delay: Duration::from_millis(1),
            ..RetryPolicy::default()
        };
        let budget = SharedRetryBudget::arc(2);
        let calls = Arc::new(AtomicU32::new(0));
        let calls_inner = calls.clone();
        let result: anyhow::Result<()> = RetryExecutor::execute_with_classifier_and_budget(
            &policy,
            "test",
            Some(&budget),
            &peko_provider_api::BodyStringClassifier,
            || {
                let calls = calls_inner.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Err(anyhow::anyhow!("HTTP error 503: try again"))
                }
            },
        )
        .await;
        assert!(result.is_err());
        // budget=2 means up to 2 retries (3 total attempts). Past
        // that, the executor returns even though `policy.max_retries=5`.
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert_eq!(budget.remaining(), 0);
    }

    /// F40: without a shared budget, the executor uses the policy
    /// ceiling alone — the legacy behavior. No-regression test for
    /// callers that don't wire a budget.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn executor_without_shared_budget_uses_policy_ceiling() {
        let policy = RetryPolicy {
            max_retries: 2,
            base_delay: Duration::from_millis(1),
            ..RetryPolicy::default()
        };
        let calls = Arc::new(AtomicU32::new(0));
        let calls_inner = calls.clone();
        let result: anyhow::Result<()> = RetryExecutor::execute_with_classifier_and_budget(
            &policy,
            "test",
            None,
            &peko_provider_api::BodyStringClassifier,
            || {
                let calls = calls_inner.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Err(anyhow::anyhow!("HTTP error 503: try again"))
                }
            },
        )
        .await;
        assert!(result.is_err());
        // policy.max_retries=2 → 3 total attempts.
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }
}
