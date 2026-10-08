//! Typed errors at the `AgenticLoop` boundary.
//!
//! F31c: the loop returns `Result<AgenticResult, anyhow::Error>` to
//! avoid breaking the existing public API, but the *internal* surface
//! should speak in terms of structured variants so callers (and tests)
//! can branch without string-matching. This module defines the
//! `AgenticError` enum that the loop uses for the two typed paths
//! that previously got downcast to `anyhow::anyhow!(existing_err)`:
//!
//! - **Quota errors** — `QuotaError` from `peko_quota::error`,
//!   which already carries `used` / `limit` / `window_end` for
//!   user-facing "what did I exceed?" UX. `#[from]` lets `?`
//!   propagate without an explicit wrapper.
//!
//! Other paths (tool errors, transport errors, subagent spawn
//! failures) remain `anyhow::Error` at the seam today; their
//! typed-error integration is deferred to a future PR (see audit
//! row 5 residual).

use peko_quota::error::QuotaError;

/// Typed errors that can be returned from the agentic loop at the
/// `peko_engine::AgenticLoop` reference path (the root shim re-exports
/// the loop until Phase 9b). Loosely modeled on codex
/// `protocol/src/error.rs:67` (`TurnAborted` and friends) but
/// scoped to the two cases where peko's loop currently has *fully
/// typed* data on hand. Variants are added as sub-system types
/// reach the loop boundary.
#[derive(Debug, thiserror::Error)]
pub enum AgenticError {
    /// The provider repeatedly exhausted its response output limit in one run.
    #[error("output limit recovery budget exhausted ({recoveries}/{max_recoveries}); split the remaining work into smaller responses")]
    OutputLimit {
        /// Recovery continuations already offered in this run.
        recoveries: usize,
        /// Maximum continuations for this failure mode.
        max_recoveries: usize,
    },
    /// Quota exceeded (input tokens, output tokens, or request count).
    /// The inner `QuotaError` carries `used` / `limit` / `window_end`
    /// so the CLI's quota-exceeded message can render "X / Y (resets
    /// at Z)" without re-parsing.
    #[error(transparent)]
    Quota(#[from] QuotaError),

    /// F31b lift: `stream_max_retries` exhausted on a transient
    /// mid-stream or start-stream error. Carries the original error
    /// verbatim as a `String` for diagnostics (the typed retry-cause
    /// wasn't preserved on the original path either — `RetryableError`
    /// is an extension trait on `anyhow::Error` with no structured
    /// return shape).
    #[error("streaming retry budget exhausted ({attempts}/{max_attempts}): {cause}")]
    RetryLimit {
        /// How many retries were attempted before the budget was
        /// exhausted.
        attempts: u32,
        /// Configured retry ceiling.
        max_attempts: u32,
        /// The upstream error message that triggered the final
        /// budget-exhaustion event (preserved verbatim).
        cause: String,
    },

    /// PR 2 / `feature/model-first-config`: an outgoing LLM call
    /// was refused because the bound model's `ModelSpec` does not
    /// declare the capability the request would hit (image /
    /// audio / tools / thinking). The inner `SpecGateError` carries
    /// the specific capability and a structured message suitable
    /// for the CLI / desktop UI. Callers branch on
    /// `.as_spec_violation()` for the typed surface or fall
    /// through to `Display` for the existing string path.
    #[error(transparent)]
    SpecViolation(#[from] crate::SpecGateError),
}

impl AgenticError {
    /// If this is a quota error, return a reference to it. Lets
    /// callers branch with `if let AgenticError::Quota(q) = err` or
    /// `.as_quota()` without a manual match.
    #[must_use]
    pub fn as_quota(&self) -> Option<&QuotaError> {
        match self {
            AgenticError::Quota(q) => Some(q),
            _ => None,
        }
    }

    /// If this is a streaming-retry exhaustion, return
    /// `(attempts, max_attempts, cause)`. Lets callers render
    /// "retried N/M times before giving up: <reason>" UX.
    #[must_use]
    pub fn as_retry_limit(&self) -> Option<(u32, u32, &str)> {
        match self {
            AgenticError::RetryLimit {
                attempts,
                max_attempts,
                cause,
            } => Some((*attempts, *max_attempts, cause)),
            _ => None,
        }
    }

    /// PR 2: if this is a spec-gate refusal, return a reference to
    /// the typed `SpecGateError`. Lets callers render
    /// "model X doesn't accept images" UX without matching the
    /// variant shape manually.
    #[must_use]
    pub fn as_spec_violation(&self) -> Option<&crate::SpecGateError> {
        match self {
            AgenticError::SpecViolation(e) => Some(e),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    #[test]
    fn test_quota_from_lift() {
        let q = QuotaError::InputTokensExceeded {
            used: 1_000_000,
            limit: 1_000_000,
            window_end: Utc.with_ymd_and_hms(2026, 7, 20, 0, 0, 0).unwrap(),
        };
        let ae: AgenticError = q.into();
        let lifted = ae.as_quota().unwrap();
        assert!(
            matches!(
                lifted,
                QuotaError::InputTokensExceeded {
                    used: 1_000_000,
                    ..
                }
            ),
            "Quota must round-trip through AgenticError::as_quota / From<QuotaError>"
        );
    }

    #[test]
    fn test_as_quota_returns_none_for_other_variants() {
        let ae = AgenticError::RetryLimit {
            attempts: 5,
            max_attempts: 5,
            cause: "connection reset".to_string(),
        };
        assert!(ae.as_quota().is_none());
    }

    #[test]
    fn test_retry_limit_accessor() {
        let ae = AgenticError::RetryLimit {
            attempts: 3,
            max_attempts: 3,
            cause: "connection refused".to_string(),
        };
        assert_eq!(ae.as_retry_limit(), Some((3, 3, "connection refused")));
    }

    #[test]
    fn test_quota_display_includes_used_limit_window() {
        let q = QuotaError::OutputTokensExceeded {
            used: 50_000,
            limit: 50_000,
            window_end: Utc.with_ymd_and_hms(2026, 7, 20, 0, 0, 0).unwrap(),
        };
        let ae: AgenticError = q.into();
        let s = ae.to_string();
        assert!(s.contains("output token quota exceeded"));
        assert!(s.contains("50000"));
    }

    /// F31c: the lift must propagate through `anyhow::Error` so the
    /// `agentic_loop.rs` pre-flight check sites can do
    /// `return Err(AgenticError::from(q).into())` and the caller can
    /// downcast via `err.downcast_ref::<AgenticError>()`. Verifies
    /// the cross-type path one-way (typed → Display on anyhow).
    #[test]
    fn test_quota_lift_through_anyhow_error_display() {
        let q = QuotaError::InputTokensExceeded {
            used: 999,
            limit: 100,
            window_end: Utc.with_ymd_and_hms(2026, 7, 20, 0, 0, 0).unwrap(),
        };
        let ae: AgenticError = q.into();
        let anyhow_err: anyhow::Error = ae.into();
        let s = anyhow_err.to_string();
        assert!(
            s.contains("input token quota exceeded"),
            "Round-trip through anyhow::Error must preserve the typed Display: {s}"
        );
        assert!(s.contains("999"), "used value must round-trip: {s}");
        assert!(s.contains("100"), "limit value must round-trip: {s}");
    }
}
