//! `RetryableError` trait — classifies whether an error warrants a
//! network-level retry. Lifted from
//! `src/providers/transport/retry.rs` in Phase 9b.N.5b.8 so the
//! agentic loop (now in `peko-engine`) can call it without taking a
//! `peko-engine → root` dep edge.
//!
//! The companion `RetryPolicy` (with `base_delay` / `backoff_multiplier` /
//! `max_delay`) and the `RetryExecutor` (the loop that drives the
//! actual retries) stay in root because they're coupled to
//! `crate::providers::transport::HttpClient`. Only the trait +
//! `anyhow::Error` impl move here — they're pure value-level helpers
//! that operate on the error message string.
//!
//! Status-code scanning list (lifted verbatim from `retry.rs:90`):
//! - 429: rate-limited (retryable)
//! - 500/502/503/504: server errors (retryable)
//! - 529: Anthropic "site is overloaded" (retryable)
//!
//! Timeout/network-error string sniffing (`retry.rs:100-105`) also
//! lifts verbatim — these are the same triggers F31b's
//! `stream_retry` loop relies on.

use std::time::Duration;

/// Trait for errors that can be classified as retryable.
///
/// `RetryExecutor` (root-only) implements the actual retry policy and
/// uses this trait to decide whether to retry, how long to wait, and
/// when to give up. The trait is also used inline at
/// `crates/engine/src/agentic_loop.rs:1410, 1586` for the F31b
/// mid-stream retry path, so lifting it keeps that path independent
/// of root.
pub trait RetryableError {
    /// Returns true if this error warrants a retry.
    fn is_retryable(&self) -> bool;

    /// Extract HTTP status code if available.
    fn http_status(&self) -> Option<u16>;

    /// Server-suggested retry delay from the `Retry-After` header
    /// (RFC 7231 §7.1.3). When `Some`, [`RetryExecutor`] prefers
    /// this over computed exponential backoff — capped at the
    /// policy's `max_delay` so a hostile or stale header can't pin
    /// us forever. Defaults to `None`; implementers that produce
    /// raw upstream errors only need to override this when they
    /// can carry the hint.
    fn retry_after(&self) -> Option<Duration> {
        None
    }
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

        // Try to extract status code from common error patterns.
        // 401 is included here so `RotatingAuthProvider` can detect
        // auth failures; it is intentionally NOT in `is_retryable()`
        // because the HTTP retry policy should not retry 401s —
        // rotation handles them at the provider layer.
        // 400 + 413 are recognized so the F22 eviction loop can
        // detect `ContextWindowExceeded` (Anthropic 400 "prompt is
        // too long", OpenAI 400 "context_length_exceeded", some
        // Anthropic deployments surface 413 "request body too
        // large"). They're also excluded from `is_retryable()` —
        // recovery is a different mechanism (drop oldest and
        // retry), not a network retry.
        for code in [400u16, 401, 408, 413, 429, 500, 502, 503, 504, 529] {
            if msg.contains(&format!(" {code}"))
                || msg.contains(&format!("HTTP error {code}"))
                || msg.contains(&format!("status {code}"))
            {
                return Some(code);
            }
        }

        None
    }

    fn retry_after(&self) -> Option<Duration> {
        // `HttpClient` embeds the upstream `Retry-After` header into
        // the error message as `(retry_after=Ns)` when it's a positive
        // integer; see `classify_http_error` in client.rs. We pull it
        // back out here so the executor can wait the server-suggested
        // interval instead of guessing. A malformed or absent hint
        // yields `None` and the executor falls back to its computed
        // exponential backoff — no behavioral regression for providers
        // that don't send the header.
        let msg = self.to_string();
        let start = msg.find("(retry_after=")?;
        let after = &msg[start + "(retry_after=".len()..];
        let end = after.find("s)")?;
        let secs: u64 = after[..end].parse().ok()?;
        if secs == 0 {
            return None;
        }
        Some(Duration::from_secs(secs))
    }
}
