//! Per-principal spend aggregation (B5f, 2026-08-22).
//!
//! Peer billing was removed (B5) because a single agent may serve
//! many peers simultaneously, making peer attribution ambiguous.
//! Per-agent attribution replaced it: each
//! [`SubagentExecutor`](crate::meter::QuotaMeter) holds its own
//! `Arc<QuotaMeter>` (audit-only by default) that charges on every
//! LLM call (see
//! `core::agents::subagent_executor::SubagentExecutor::agent_meter`).
//!
//! The principal's per-cycle consumption is then `sum(agent_meters)`
//! across every agent ever spawned. This module provides the
//! summation primitive.
//!
//! The aggregate is itself a `QuotaState` with `cycle = Daily` (the
//! most-common default) and a window that spans the union of the
//! input windows. Callers that need the cycle-aligned window for
//! reporting should re-roll the window via
//! [`state::QuotaState::fresh`](crate::state::QuotaState::fresh)
//! and forward the totals — the sum only depends on the counter
//! fields, not the window.
//!
//! `cost_usd` is summed as `f64::sum`; precision is bounded by the
//! underlying `f64` representation (matches `QuotaConfig`/`QuotaState`
//! semantics elsewhere).

use crate::config::QuotaCycle;
use crate::meter::QuotaMeter;
use crate::state::QuotaState;
use std::sync::Arc;

/// Sum the per-agent meters into a single `QuotaState`.
///
/// Each input `Arc<QuotaMeter>` is snapshotted under its own
/// `Mutex<QuotaState>`, then the counter fields are summed
/// field-by-field. `None` is treated as an empty slice (returns
/// `QuotaState::fresh(Daily, now)` with all counters at zero).
///
/// Use this for per-principal audit reports: pass every agent's
/// `Arc<QuotaMeter>` and the result is the principal's total
/// per-cycle consumption.
///
/// # Example
///
/// ```ignore
/// use peko_quota::aggregate::sum_meters;
///
/// let total = sum_meters(&[
///     root_agent.executor().agent_meter().clone(),
///     spawned_a.executor().agent_meter().clone(),
///     spawned_b.executor().agent_meter().clone(),
/// ], chrono::Utc::now());
///
/// eprintln!("principal consumed: {} input / {} output / {} requests",
///     total.input_tokens, total.output_tokens, total.request_count);
/// ```
#[must_use]
pub fn sum_meters(meters: &[Arc<QuotaMeter>], now: chrono::DateTime<chrono::Utc>) -> QuotaState {
    if meters.is_empty() {
        return QuotaState::fresh(QuotaCycle::Daily, now);
    }
    let snapshots: Vec<QuotaState> = meters.iter().map(|m| m.snapshot()).collect();
    sum_states(&snapshots, now)
}

/// Sum a slice of `QuotaState` snapshots. Field-by-field because
/// `cost_usd: Option<f64>` doesn't implement `Add`. The cycle +
/// window fields of the returned `QuotaState` are taken from the
/// first non-empty snapshot (or `Daily` + `now` when the slice is
/// empty); counter fields are summed.
#[must_use]
pub fn sum_states(states: &[QuotaState], now: chrono::DateTime<chrono::Utc>) -> QuotaState {
    let Some((first, rest)) = states.split_first() else {
        return QuotaState::fresh(QuotaCycle::Daily, now);
    };
    let mut total = first.clone();
    for s in rest {
        total.input_tokens = total.input_tokens.saturating_add(s.input_tokens);
        total.output_tokens = total.output_tokens.saturating_add(s.output_tokens);
        total.request_count = total.request_count.saturating_add(s.request_count);
        total.cost_usd = match (total.cost_usd, s.cost_usd) {
            (Some(a), Some(b)) => Some(a + b),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{QuotaConfig, QuotaCycle};
    use chrono::TimeZone;

    fn ts(year: i32, month: u32, day: u32, hour: u32) -> chrono::DateTime<chrono::Utc> {
        chrono::Utc
            .with_ymd_and_hms(year, month, day, hour, 0, 0)
            .single()
            .unwrap()
    }

    /// `sum_states` on an empty slice returns the Daily / fresh
    /// baseline at `now`.
    #[test]
    fn sum_states_empty_returns_fresh_daily() {
        let now = ts(2026, 8, 22, 14);
        let total = sum_states(&[], now);
        assert_eq!(total.cycle, QuotaCycle::Daily);
        assert_eq!(total.input_tokens, 0);
        assert_eq!(total.output_tokens, 0);
        assert_eq!(total.request_count, 0);
    }

    /// `sum_states` on a single snapshot returns that snapshot
    /// unchanged.
    #[test]
    fn sum_states_single_returns_unchanged() {
        let now = ts(2026, 8, 22, 14);
        let s = QuotaState {
            window_start: ts(2026, 8, 22, 14),
            window_end: ts(2026, 8, 23, 0),
            cycle: QuotaCycle::Hourly,
            input_tokens: 100,
            output_tokens: 50,
            request_count: 3,
            cost_usd: Some(0.01),
        };
        let total = sum_states(&[s.clone()], now);
        assert_eq!(total.input_tokens, 100);
        assert_eq!(total.output_tokens, 50);
        assert_eq!(total.request_count, 3);
        assert_eq!(total.cost_usd, Some(0.01));
    }

    /// `sum_states` adds counters field-by-field and unions cost.
    #[test]
    fn sum_states_multi_sums_each_field() {
        let now = ts(2026, 8, 22, 14);
        let a = QuotaState {
            window_start: ts(2026, 8, 22, 14),
            window_end: ts(2026, 8, 23, 0),
            cycle: QuotaCycle::Hourly,
            input_tokens: 100,
            output_tokens: 50,
            request_count: 3,
            cost_usd: Some(0.01),
        };
        let b = QuotaState {
            window_start: ts(2026, 8, 22, 14),
            window_end: ts(2026, 8, 23, 0),
            cycle: QuotaCycle::Hourly,
            input_tokens: 200,
            output_tokens: 75,
            request_count: 5,
            cost_usd: Some(0.02),
        };
        let total = sum_states(&[a, b], now);
        assert_eq!(total.input_tokens, 300);
        assert_eq!(total.output_tokens, 125);
        assert_eq!(total.request_count, 8);
        assert!(
            (total.cost_usd.unwrap() - 0.03).abs() < 1e-9,
            "cost_usd should sum to 0.03"
        );
    }

    /// `sum_states` handles `cost_usd: None` (pre-Phase-3 state
    /// files) by promoting `None` + `Some(x)` to `Some(x)`.
    #[test]
    fn sum_states_handles_cost_usd_none() {
        let now = ts(2026, 8, 22, 14);
        let none_cost = QuotaState {
            window_start: ts(2026, 8, 22, 14),
            window_end: ts(2026, 8, 23, 0),
            cycle: QuotaCycle::Hourly,
            input_tokens: 100,
            output_tokens: 50,
            request_count: 3,
            cost_usd: None,
        };
        let some_cost = QuotaState {
            cost_usd: Some(0.05),
            ..none_cost.clone()
        };
        // None first, Some second → result is Some(0.05)
        let total = sum_states(&[none_cost.clone(), some_cost.clone()], now);
        assert_eq!(total.cost_usd, Some(0.05));
        // Some first, None second → result is Some(0.05)
        let total = sum_states(&[some_cost, none_cost], now);
        assert_eq!(total.cost_usd, Some(0.05));
    }

    /// `sum_meters` on an empty slice returns the Daily / fresh
    /// baseline at `now`.
    #[tokio::test]
    async fn sum_meters_empty_returns_fresh_daily() {
        let now = ts(2026, 8, 22, 14);
        let total = sum_meters(&[], now);
        assert_eq!(total.cycle, QuotaCycle::Daily);
        assert_eq!(total.request_count, 0);
    }

    /// `sum_meters` snapshots each meter and sums. Constructs two
    /// unlimited meters and charges them independently to verify
    /// the snapshot+sum composition.
    #[tokio::test]
    async fn sum_meters_sums_snapshots() {
        let now = ts(2026, 8, 22, 14);
        let a = Arc::new(QuotaMeter::new(QuotaConfig::default(), None, now));
        let b = Arc::new(QuotaMeter::new(QuotaConfig::default(), None, now));
        let usage = peko_message::TokenUsage {
            input: 100,
            output: 50,
            total: 150,
            ..Default::default()
        };
        a.charge(&usage).await.unwrap();
        b.charge(&usage).await.unwrap();

        let total = sum_meters(&[a, b], now);
        assert_eq!(total.input_tokens, 200);
        assert_eq!(total.output_tokens, 100);
        assert_eq!(total.request_count, 2);
    }
}