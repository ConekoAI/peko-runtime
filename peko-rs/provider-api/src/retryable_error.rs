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
//!
//! ## F40 typed classifier (PR #1 of the 429-rewrite workstream)
//!
//! The pre-F40 `is_retryable() -> bool` API flattens every retryable
//! error into one bucket, so a real "usage_limit_reached" (terminal)
//! and a transient 429 retry through identical mechanics. F40
//! introduces [`RetryClassification`] — a typed enum that separates
//! the semantic categories — plus a pluggable
//! [`RetryClassifier`] trait with a substring-based default
//! ([`BodyStringClassifier`]). The agentic loop pattern-matches on
//! the enum to distinguish terminal (no retry) from transient
//! (retry with delay). Existing `is_retryable` callers keep
//! working unchanged — the new classification path is opt-in
//! for sites that need it (the agentic loop's mid-stream retry
//! and the transport `RetryExecutor`).

use std::sync::Arc;
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

/// Semantic classification of a 4xx/5xx/throttle error.
///
/// F40 typed classifier (PR #1 of the 429-rewrite workstream):
/// replaces the pre-F40 `is_retryable: bool` with an enum the
/// agentic loop can pattern-match on. Why the richer shape?
///
/// - **Body-subtype differentiation**: a 429 carrying
///   `usage_limit_reached` (terminal — codex
///   [`CodexErr::UsageLimitReached`] analog) is NOT the same kind of
///   failure as a 429 carrying `rate_limit_exceeded` (transient,
///   retryable). The pre-F40 `is_retryable: bool` collapsed both
///   into "retry", so users with exhausted quota burned the full
///   retry budget before seeing the error.
///
/// - **Codex parity**: codex's `ApiError` / `CodexErr` enumerates
///   `RateLimit`, `QuotaExceeded`, `UsageLimitReached`, etc. The
///   [`RetryClassification`] enum is peko's lean equivalent — flat,
///   `Clone`+`Eq`, no provider-specific state.
///
/// Note: order of variants matters for the `PartialEq` derived impl;
/// the enum is ordered from "happens-frequently-but-OK-to-retry"
/// to "happens-frequently-and-terminal" so a quick
/// `matches!(c, RetryClassification::Transient { .. })` is the
/// common path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetryClassification {
    /// Transient failure with an optional server-suggested delay.
    /// Retry sites sleep `retry_after` (capped at the policy's
    /// `max_delay`) and re-issue the request; if `retry_after` is
    /// `None` they fall back to computed exponential backoff.
    ///
    /// Triggers: generic 429, generic 500/502/503/504, generic 529,
    /// network timeouts, connection reset / refused. These match
    /// the `RetryableError::is_retryable` substring set so the two
    /// classifiers agree on "is this a retry candidate?" — the
    /// difference is `is_retryable` returns `true` and stops there
    /// while `classify` returns the explicit variant.
    Transient {
        /// Server-suggested delay (`Retry-After` header parsed).
        /// `None` means the provider did not emit the header and
        /// the caller should fall back to computed backoff.
        retry_after: Option<Duration>,
    },
    /// HTTP 401 / Unauthorized. NOT a retry candidate —
    /// `RotatingAuthProvider` handles this by selecting the next
    /// credential; the loop just surfaces it.
    AuthFailure,
    /// OpenAI Responses `usage_limit_reached` body code. Terminal —
    /// the user has exhausted their plan. (codex analog:
    /// `CodexErr::UsageLimitReached`.)
    UsageLimitReached,
    /// OpenAI Responses `usage_not_included` body code. Terminal —
    /// the user must add billing. (codex analog:
    /// `CodexErr::UsageNotIncluded`.)
    UsageNotIncluded,
    /// Billing / quota family: `insufficient_quota`,
    /// `quota_exceeded`, `billing_hard_limit`. Terminal. (codex
    /// analog: `ApiError::QuotaExceeded`.)
    QuotaExceeded,
    /// 400 / 413 / `invalid_request`. Terminal — recovery requires
    /// changing the request shape (smaller prompt, different
    /// schema, etc.) and is out of scope for retry.
    InvalidRequest,
    /// Server explicitly returned `server_is_overloaded` /
    /// `slow_down` (OpenAI Responses body codes). Terminal in
    /// codex — the test suite treats the server's "we are
    /// overloaded" signal as authoritative and surfaces it without
    /// retrying.
    RateLimitedTerminal,
    /// Context-window exceeded: `prompt is too long`,
    /// `context_length_exceeded`, etc. Handled by F22's
    /// front-evict path in `agentic_loop::stream_with_eviction`,
    /// not by retry. Surfaced here so the retry classifier can
    /// pass-through without triggering a retry.
    ContextWindowExceeded,
}

impl RetryClassification {
    /// `true` if this classification allows the retry site to
    /// re-issue the request after sleeping. The classifier's order
    /// means terminal variants (`QuotaExceeded`,
    /// `UsageLimitReached`, etc.) all return `false` — they must
    /// NOT retry.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Transient { .. })
    }

    /// Server-suggested delay (only meaningful for `Transient`).
    /// Convenience accessor so retry sites don't have to
    /// pattern-match on the inner `Option` themselves.
    #[must_use]
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::Transient { retry_after } => *retry_after,
            _ => None,
        }
    }
}

/// Pluggable classifier that maps `&anyhow::Error` into a
/// [`RetryClassification`].
///
/// The default impl ([`BodyStringClassifier`]) scans the error
/// message string (built by
/// `peko_providers::transport::client`'s `classify_http_error`,
/// which embeds the status code AND the error body verbatim).
/// Wiring this trait object through `Arc<dyn RetryClassifier>` lets
/// future per-adapter classifiers (Responses-API error codes,
/// Anthropic unified headers, etc.) replace the substring path
/// without changing call sites.
///
/// ## Why pass through `anyhow::Error`
///
/// The transport layer builds `anyhow::Error` chains that carry
/// the upstream `error_text` body AND the parsed status code AND
/// the `Retry-After` hint all in one structured string. A
/// substring classifier sees all three signals. Per-adapter
/// classifiers can override `classify` to parse the JSON body
/// directly once the adapter has full JSON access (Responses API,
/// Anthropic Messages JSON body).
pub trait RetryClassifier: Send + Sync {
    /// Classify the error into a [`RetryClassification`].
    ///
    /// Implementations MUST inspect the error chain
    /// (`err.chain()` or `err.to_string()`). Returning `Transient`
    /// for an unrecognized error is the safe fallback; returning
    /// `InvalidRequest` is the loud fallback.
    fn classify(&self, err: &anyhow::Error) -> RetryClassification;

    /// Server-suggested retry delay (RFC 7231 §7.1.3 header value,
    /// body-regex like "try again in Ns", or wire-shape-specific
    /// parsing). Default `None` — only re-implement when you have
    /// a richer source than the substring `(retry_after=Ns)`
    /// marker that `classify_http_error` emits.
    fn retry_after(&self, err: &anyhow::Error) -> Option<Duration> {
        let _ = err;
        None
    }
}

/// Default [`RetryClassifier`] impl: substring-scan the
/// `anyhow::Error` chain for known status codes and vendor-specific
/// error body substrings.
///
/// Order is **most-specific-first** so a message like
/// `"HTTP error 429: usage_limit_reached"` resolves to
/// `UsageLimitReached` (terminal), not `Transient { retry_after }`.
/// A naive "429 → transient" rule would re-iterate the audit's P0
/// finding for quota-exhausted users; the precedence order closes
/// that gap at the classifier level so both transport and engine
/// retry sites agree.
///
/// Matches [`RetryableError::is_retryable`]'s substring set (429 /
/// 500 / 502 / 503 / 504 / 529, "timeout", "connection reset",
/// "refused") — every error that the legacy bool classifier
/// flagged as retryable, `classify` flags as `Transient`, just
/// with the richer variant shape.
#[derive(Debug, Default, Clone, Copy)]
pub struct BodyStringClassifier;

impl RetryClassification {
    fn from_msg_substring(msg: &str) -> Self {
        // Per the docstring, most-specific vendor body codes win
        // over generic status codes.
        if msg.contains("usage_limit_reached") {
            return Self::UsageLimitReached;
        }
        if msg.contains("usage_not_included") {
            return Self::UsageNotIncluded;
        }
        if msg.contains("server_is_overloaded") || msg.contains("slow_down") {
            return Self::RateLimitedTerminal;
        }
        if contains_any(
            msg,
            &[
                "insufficient_quota",
                "quota_exceeded",
                "billing_hard_limit",
            ],
        ) {
            return Self::QuotaExceeded;
        }
        if contains_any(
            msg,
            &[
                "prompt is too long",
                "context_length_exceeded",
                "maximum context length",
                "context window",
            ],
        ) {
            return Self::ContextWindowExceeded;
        }
        // 401 must be detected before generic 4xx so the body-string
        // classifier can distinguish auth failure from a generic
        // 400. The pre-fix exclusion (`!contains "HTTP error 4"`)
        // was wrong: `"HTTP error 401"` DOES contain `"HTTP error 4"`
        // (both 400 and 401 share the prefix), which meant the
        // guard excluded every `HTTP error 4XX` message and forced
        // them all to `InvalidRequest`. We match `"401"` and
        // `"Unauthorized"` as AuthFailure; the generic `"400"` /
        // `"413"` / `"Bad Request"` strings below land on
        // `InvalidRequest` and do NOT collide because none of
        // them is a substring of `401`.
        if contains_any(msg, &["401", "Unauthorized"]) {
            return Self::AuthFailure;
        }
        if contains_any(
            msg,
            &[
                "400",
                "Bad Request",
                "413",
                "Payload Too Large",
                "invalid_request",
            ],
        ) {
            return Self::InvalidRequest;
        }

        // Generic status codes → transient. We deliberately do NOT
        // carry `(retry_after=Ns)` here —
        // `RetryClassifier::retry_after` reads it on demand, so
        // callers see the most up-to-date header-driven value.
        if contains_any(msg, &["429", "Too Many Requests", "rate limit"]) {
            return Self::Transient { retry_after: None };
        }
        if contains_any(msg, &["529", "Overloaded"]) {
            return Self::Transient { retry_after: None };
        }
        if contains_any(msg, &["500", "502", "503", "504"]) {
            return Self::Transient { retry_after: None };
        }
        // Network-shape keywords match the existing
        // `RetryableError::is_retryable` set; classified as
        // Transient with no server-suggested delay (the source is
        // connection-level, not throttle-level).
        if contains_any(
            msg,
            &[
                "timeout",
                "connection reset",
                "refused",
                "connection error",
            ],
        ) {
            return Self::Transient { retry_after: None };
        }

        // Unknown error — return InvalidRequest so retry sites
        // surface immediately instead of looping on a possibly-
        // unrecoverable condition.
        Self::InvalidRequest
    }
}

impl RetryClassifier for BodyStringClassifier {
    fn classify(&self, err: &anyhow::Error) -> RetryClassification {
        RetryClassification::from_msg_substring(&err.to_string())
    }

    fn retry_after(&self, err: &anyhow::Error) -> Option<Duration> {
        // Mirror `RetryableError::retry_after for anyhow::Error`:
        // parse the `(retry_after=Ns)` substring that
        // `classify_http_error` emits when the upstream sent a
        // positive-integer `Retry-After` header.
        let msg = err.to_string();
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

/// Convenience: build an `Arc<dyn RetryClassifier>` for the default
/// substring-scan classifier. The engine and transport wire this
/// into their retry sites; per-adapter overrides happen by passing
/// a different `Arc<dyn RetryClassifier>` at construction.
#[must_use]
pub fn default_classifier() -> Arc<dyn RetryClassifier> {
    Arc::new(BodyStringClassifier)
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| haystack.contains(n))
}

#[cfg(test)]
mod classification_tests {
    use super::*;

    fn classify(s: &str) -> RetryClassification {
        BodyStringClassifier.classify(&anyhow::anyhow!("{s}"))
    }

    #[test]
    fn terminal_usage_limit_reached_wins_over_generic_429() {
        // Audit P0: a 429 + `usage_limit_reached` body MUST NOT
        // retry.
        let c = classify("HTTP error 429 (retry_after=1s): usage_limit_reached");
        assert_eq!(c, RetryClassification::UsageLimitReached);
        assert!(!c.is_retryable());
    }

    #[test]
    fn terminal_usage_not_included() {
        let c = classify("HTTP error 429: usage_not_included");
        assert_eq!(c, RetryClassification::UsageNotIncluded);
    }

    #[test]
    fn terminal_insufficient_quota() {
        let c = classify("HTTP error 429: insufficient_quota");
        assert_eq!(c, RetryClassification::QuotaExceeded);
    }

    #[test]
    fn terminal_server_is_overloaded() {
        let c = classify("HTTP error 529: server_is_overloaded");
        assert_eq!(c, RetryClassification::RateLimitedTerminal);
    }

    #[test]
    fn terminal_slow_down() {
        let c = classify("HTTP error 429: slow_down");
        assert_eq!(c, RetryClassification::RateLimitedTerminal);
    }

    #[test]
    fn transient_429_carries_retry_after_via_method() {
        let c = classify("HTTP error 429 (retry_after=2s): rate limit exceeded");
        assert!(c.is_retryable(), "plain 429 should still retry: {c:?}");
        // The (retry_after=Ns) substring is parsed by retry_after(),
        // not by classify() — keeping classify() synchronous / pure.
        let hint = BodyStringClassifier.retry_after(&anyhow::anyhow!(
            "HTTP error 429 (retry_after=2s): rate limit exceeded"
        ));
        assert_eq!(hint, Some(Duration::from_secs(2)));
    }

    #[test]
    fn transient_529_overloaded() {
        let c = classify("HTTP error 529: site overloaded, try later");
        assert!(c.is_retryable());
    }

    #[test]
    fn transient_5xx_family() {
        for body in [
            "HTTP error 500",
            "HTTP error 502 bad gateway",
            "HTTP error 503",
            "HTTP error 504",
        ] {
            let c = classify(body);
            assert!(c.is_retryable(), "5xx should retry: {body} => {c:?}");
        }
    }

    #[test]
    fn transient_network_keywords() {
        for body in [
            "connection refused",
            "connection reset by peer",
            "request timeout",
            "connection error",
        ] {
            let c = classify(body);
            assert!(c.is_retryable(), "{body} should retry, got {c:?}");
        }
    }

    #[test]
    fn auth_401_separate_variant() {
        let c = classify("HTTP error 401: invalid api key");
        assert_eq!(c, RetryClassification::AuthFailure);
        assert!(!c.is_retryable());
    }

    #[test]
    fn context_window_substrings() {
        for body in [
            "HTTP error 400: prompt is too long",
            "HTTP error 400: context_length_exceeded",
            "HTTP error 400: maximum context length",
            "HTTP error 400: context window exceeded",
        ] {
            let c = classify(body);
            assert_eq!(
                c,
                RetryClassification::ContextWindowExceeded,
                "{body} should classify as context-window: {c:?}"
            );
        }
    }

    #[test]
    fn invalid_request_400_or_413() {
        for body in [
            "HTTP error 400",
            "HTTP error 413: Payload Too Large",
            "invalid_request: foo",
        ] {
            let c = classify(body);
            assert_eq!(c, RetryClassification::InvalidRequest);
        }
    }

    #[test]
    fn unknown_error_falls_through_to_invalid_request() {
        // Unrecognized body — loud fallback so retry doesn't loop
        // forever on a malformed error.
        let c = classify("totally unrecognized: xyzabc");
        assert_eq!(c, RetryClassification::InvalidRequest);
    }

    #[test]
    fn retry_after_zero_is_dropped_to_none() {
        let hint = BodyStringClassifier
            .retry_after(&anyhow::anyhow!("HTTP error 429 (retry_after=0s): nope"));
        assert_eq!(hint, None);
    }

    #[test]
    fn retry_after_absent_string_returns_none() {
        let hint = BodyStringClassifier
            .retry_after(&anyhow::anyhow!("HTTP error 429: no hint"));
        assert_eq!(hint, None);
    }

    #[test]
    fn retry_after_garbage_returns_none() {
        let hint = BodyStringClassifier
            .retry_after(&anyhow::anyhow!("HTTP error 429 (retry_after=abc): bad"));
        assert_eq!(hint, None);
    }

    #[test]
    fn default_classifier_returns_arc() {
        // Smoke test: factory function returns an Arc<dyn
        // RetryClassifier> usable from sites that don't carry the
        // concrete struct.
        let c = default_classifier();
        let c2: Arc<dyn RetryClassifier> = c.clone();
        let cls = c2.classify(&anyhow::anyhow!("HTTP error 429"));
        assert!(cls.is_retryable());
    }
}
