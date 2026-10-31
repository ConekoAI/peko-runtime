//! Disruptions (ADR-089 D1, D4): which errors waiting fixes, and how
//! long to wait. Shared by parked conversation runs
//! (`principal::parking`) and runs that wait in place (subagent tasks,
//! `agents::subagent_executor`).

use std::time::Duration;

use chrono::{DateTime, Utc};

/// The peko's own spending limit.
pub const PEKO_LIMIT: &str = "peko_limit";
/// The provider's quota, plan, or billing limit.
pub const PROVIDER_QUOTA: &str = "provider_quota";
/// The provider can't be reached, or is overloaded.
pub const PROVIDER_UNREACHABLE: &str = "provider_unreachable";
/// A failure waiting will not fix.
pub const FAILED: &str = "failed";

/// First wait after the provider's quota ran out; it doubles per failed
/// attempt up to [`QUOTA_CAP`].
pub const QUOTA_BASE: Duration = Duration::from_mins(1);
pub const QUOTA_CAP: Duration = Duration::from_hours(24);
/// First wait after the provider was unreachable; doubles up to
/// [`UNREACHABLE_CAP`].
pub const UNREACHABLE_BASE: Duration = Duration::from_secs(30);
pub const UNREACHABLE_CAP: Duration = Duration::from_hours(1);
/// When the peko's own limit names no reset, look again after this.
const LIMIT_FALLBACK: Duration = Duration::from_hours(1);

/// A disruption, ready to wait on.
#[derive(Debug, Clone)]
pub struct Disrupted {
    pub kind: &'static str,
    pub message: String,
    /// The peko's limit resets at (from its `QuotaError`).
    pub resets_at: Option<DateTime<Utc>>,
}

/// Which disruption an error is, or [`FAILED`].
#[must_use]
pub fn failure_kind(error: &anyhow::Error) -> &'static str {
    if error
        .chain()
        .any(|cause| cause.is::<peko_quota::QuotaError>())
        || matches!(
            error.downcast_ref::<peko_engine::AgenticError>(),
            Some(peko_engine::AgenticError::Quota(_))
        )
        || is_peko_limit_text(&error.to_string())
    {
        return PEKO_LIMIT;
    }
    use peko_provider_api::RetryClassifier as _;
    peko_provider_api::BodyStringClassifier
        .classify(error)
        .disruption()
        .map_or(FAILED, peko_provider_api::Disruption::as_str)
}

/// The disruption an error is — `None` for a failure.
#[must_use]
pub fn disrupted(error: &anyhow::Error) -> Option<Disrupted> {
    let kind = failure_kind(error);
    (kind != FAILED).then(|| Disrupted {
        kind,
        message: error.to_string(),
        resets_at: (kind == PEKO_LIMIT).then(|| resets_at(error)).flatten(),
    })
}

/// The peko's own limit, when only its text survived (the stream carries
/// the error as text): every `QuotaError` reads "… quota exceeded: … /
/// … (resets at …)" or "USD budget exceeded: … (resets at …)".
fn is_peko_limit_text(message: &str) -> bool {
    (message.contains("quota exceeded: ") || message.contains("budget exceeded: "))
        && message.contains("(resets at ")
}

/// When the peko's own limit resets, from its `QuotaError` or, when only
/// the text survived, its "(resets at …)".
fn resets_at(error: &anyhow::Error) -> Option<DateTime<Utc>> {
    use peko_quota::QuotaError as Q;
    let typed = error.chain().find_map(|cause| {
        cause.downcast_ref::<Q>().map(|q| match q {
            Q::InputTokensExceeded { window_end, .. }
            | Q::OutputTokensExceeded { window_end, .. }
            | Q::RequestCountExceeded { window_end, .. }
            | Q::BudgetExceeded { window_end, .. } => *window_end,
        })
    });
    typed.or_else(|| {
        let text = error.to_string();
        let start = text.find("(resets at ")? + "(resets at ".len();
        let end = text[start..].find(')')? + start;
        let stamp = text[start..end].trim().trim_end_matches(" UTC");
        chrono::NaiveDateTime::parse_from_str(stamp, "%Y-%m-%d %H:%M:%S%.f")
            .ok()
            .map(|naive| naive.and_utc())
    })
}

/// When to try again after `attempts` failed attempts (ADR-089 D4).
#[must_use]
pub fn next_attempt(disrupted: &Disrupted, attempts: u32, now: DateTime<Utc>) -> DateTime<Utc> {
    let wait = match disrupted.kind {
        PEKO_LIMIT => {
            return disrupted
                .resets_at
                .filter(|at| *at > now)
                .unwrap_or_else(|| {
                    now + chrono::Duration::from_std(LIMIT_FALLBACK).unwrap_or_default()
                });
        }
        PROVIDER_QUOTA => backoff(QUOTA_BASE, attempts, QUOTA_CAP),
        _ => backoff(UNREACHABLE_BASE, attempts, UNREACHABLE_CAP),
    };
    now + chrono::Duration::from_std(jitter(wait)).unwrap_or_default()
}

/// How long to wait after `attempts` failed attempts.
#[must_use]
pub fn wait_after(disrupted: &Disrupted, attempts: u32) -> Duration {
    let now = Utc::now();
    (next_attempt(disrupted, attempts, now) - now)
        .to_std()
        .unwrap_or_default()
}

/// `base` doubled per attempt after the first, capped.
#[must_use]
pub fn backoff(base: Duration, attempts: u32, cap: Duration) -> Duration {
    let doublings = attempts.saturating_sub(1).min(20);
    base.saturating_mul(1u32 << doublings).min(cap)
}

/// ±10 %, so sessions and pekos don't retry in lockstep.
fn jitter(wait: Duration) -> Duration {
    use rand::Rng;
    let factor = rand::thread_rng().gen_range(0.9..=1.1);
    wait.mul_f64(factor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_are_sorted_by_what_fixes_them() {
        let kind = |msg: &str| failure_kind(&anyhow::anyhow!("{msg}"));
        assert_eq!(
            kind("USD budget exceeded: $0.0510 / $0.0500 (resets at 2026-11-01 00:00:00 UTC)"),
            PEKO_LIMIT
        );
        assert_eq!(
            kind("request count quota exceeded: 2 / 1 (resets at 2026-11-01 00:00:00 UTC)"),
            PEKO_LIMIT
        );
        assert_eq!(kind("HTTP error 429: insufficient_quota"), PROVIDER_QUOTA);
        assert_eq!(
            kind("error sending request for url (https://api.example.com/v1)"),
            PROVIDER_UNREACHABLE
        );
        assert_eq!(kind("HTTP error 400: invalid_request_error"), FAILED);
    }

    #[test]
    fn the_peko_limit_reports_when_it_resets() {
        let parked = disrupted(&anyhow::anyhow!(
            "USD budget exceeded: $0.0510 / $0.0500 (resets at 2026-11-01 00:00:00 UTC)"
        ))
        .unwrap();
        assert_eq!(parked.kind, PEKO_LIMIT);
        assert_eq!(
            parked.resets_at.unwrap().to_rfc3339(),
            "2026-11-01T00:00:00+00:00"
        );
        assert!(disrupted(&anyhow::anyhow!("HTTP error 400: bad")).is_none());
    }

    #[test]
    fn waits_grow_per_attempt_up_to_their_cap() {
        assert_eq!(
            backoff(UNREACHABLE_BASE, 1, UNREACHABLE_CAP),
            Duration::from_secs(30)
        );
        assert_eq!(
            backoff(UNREACHABLE_BASE, 3, UNREACHABLE_CAP),
            Duration::from_mins(2)
        );
        assert_eq!(
            backoff(UNREACHABLE_BASE, 30, UNREACHABLE_CAP),
            UNREACHABLE_CAP
        );
        assert_eq!(backoff(QUOTA_BASE, 1, QUOTA_CAP), Duration::from_mins(1));
        assert_eq!(backoff(QUOTA_BASE, 40, QUOTA_CAP), QUOTA_CAP);
    }
}
