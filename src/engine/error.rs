//! Typed errors at the `AgenticLoop` boundary.
//!
//! F31c: the loop returns `Result<AgenticResult, anyhow::Error>` to
//! avoid breaking the existing public API, but the *internal* surface
//! should speak in terms of structured variants so callers (and tests)
//! can branch without string-matching. This module defines the
//! `AgenticError` enum that the loop uses for the two typed paths
//! that previously got downcast to `anyhow::anyhow!(existing_err)`:
//!
//! - **Quota errors** — `QuotaError` from `crate::quota::error`,
//!   which already carries `used` / `limit` / `window_end` for
//!   user-facing "what did I exceed?" UX. `#[from]` lets `?`
//!   propagate without an explicit wrapper.
//! - **Max-iteration cap-hit** — F31a's `LifecyclePhase::MaxIterations
//!   { iterations }` signal is lifted into `MaxIterationsReached
//!   { iterations }` for callers that branch on the typed error
//!   rather than the lifecycle event.
//!
//! Other paths (tool errors, transport errors, subagent spawn
//! failures) remain `anyhow::Error` at the seam today; their
//! typed-error integration is deferred to a future PR (see audit
//! row 5 residual).

use crate::quota::error::QuotaError;

/// Typed errors that can be returned from [`crate::engine::AgenticLoop`]
/// via `Result<_, anyhow::Error>`. Loosely modeled on codex
/// `protocol/src/error.rs:67` (`TurnAborted` and friends) but
/// scoped to the two cases where peko's loop currently has *fully
/// typed* data on hand. Variants are added as sub-system types
/// reach the loop boundary.
#[derive(Debug, thiserror::Error)]
pub enum AgenticError {
    /// Quota exceeded (input tokens, output tokens, or request count).
    /// The inner `QuotaError` carries `used` / `limit` / `window_end`
    /// so the CLI's quota-exceeded message can render "X / Y (resets
    /// at Z)" without re-parsing.
    #[error(transparent)]
    Quota(#[from] QuotaError),

    /// F31a lift: `LifecyclePhase::MaxIterations { iterations }` carried
    /// into the typed-error surface. `AgenticResult.success` will be
    /// `false` on this path (the existing cap-hit still returns
    /// `Ok(AgenticResult { success: false, ... })` — this variant is
    /// for callers that branch on the error stream rather than the
    /// result struct, e.g. tests asserting on `result.unwrap_err()`).
    #[error("max iterations reached ({iterations})")]
    MaxIterationsReached {
        /// Configured iteration ceiling.
        iterations: usize,
    },

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

    /// If this is a max-iterations cap-hit, return the configured
    /// ceiling. Lets callers check `err.max_iterations_cap()` to
    /// render "extend by N more rounds?" UX without matching
    /// `LifecyclePhase::MaxIterations` separately.
    #[must_use]
    pub fn max_iterations_cap(&self) -> Option<usize> {
        match self {
            AgenticError::MaxIterationsReached { iterations } => Some(*iterations),
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
}

/// F31a → F31c lift: let callers turn a `LifecyclePhase::MaxIterations`
/// straight into `AgenticError::MaxIterationsReached` so the typed
/// error and the lifecycle event can flow from the same source.
impl From<crate::engine::events::LifecyclePhase> for AgenticError {
    fn from(phase: crate::engine::events::LifecyclePhase) -> Self {
        match phase {
            crate::engine::events::LifecyclePhase::MaxIterations { iterations } => {
                AgenticError::MaxIterationsReached { iterations }
            }
            // Other phases don't represent typed errors. Map them to
            // a generic `RetryLimit`-style fallback isn't right — let
            // the caller decide. An `unimplemented!()` here would be
            // loud; logging via `tracing` is silent but at least
            // observable in production.
            other => {
                tracing::warn!(
                    "AgenticError::from(LifecyclePhase): unmapped phase {other:?}, \
                     callers should only convert MaxIterations variants"
                );
                // Fall back to a `RetryLimit` with `cause: "..."` —
                // there's no good answer for "what's an arbitrary
                // lifecycle phase as an error?" Returning the phase
                // name as a `String` debug-formatted message is the
                // least bad option. Production callers should not hit
                // this path.
                AgenticError::RetryLimit {
                    attempts: 0,
                    max_attempts: 0,
                    cause: format!("unmapped lifecycle phase: {other:?}"),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::events::LifecyclePhase;
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
    fn test_max_iterations_from_lifecycle_phase() {
        let phase = LifecyclePhase::MaxIterations { iterations: 7 };
        let ae: AgenticError = phase.into();
        assert_eq!(ae.max_iterations_cap(), Some(7));
    }

    #[test]
    fn test_as_quota_returns_none_for_other_variants() {
        let ae = AgenticError::MaxIterationsReached { iterations: 5 };
        assert!(ae.as_quota().is_none());
        assert_eq!(ae.max_iterations_cap(), Some(5));
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
