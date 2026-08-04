//! Typed quota errors.
//!
//! Four variants — one per limit kind — so the CLI and log lines
//! can tell the user exactly which wall they hit and when it
//! resets. Flows through `anyhow::Error` upstream (no `From` impl
//! needed; `QuotaError` is `Send + Sync + 'static` via `thiserror`).
//!
//! Phase 3 of `feature/multi-model-subagents` adds the
//! `BudgetExceeded` variant for the cycle-window USD budget cap.
//! The pre-spawn per-call ceiling surfaces as a separate
//! `CostCeilingExceeded` returned directly from
//! `SubagentExecutor::execute_and_wait`, not via `QuotaMeter` —
//! the meter only enforces the *cycle* budget.

use chrono::{DateTime, Utc};

#[derive(Debug, thiserror::Error)]
pub enum QuotaError {
    #[error("input token quota exceeded: {used} / {limit} (resets at {window_end})")]
    InputTokensExceeded {
        used: u64,
        limit: u64,
        window_end: DateTime<Utc>,
    },
    #[error("output token quota exceeded: {used} / {limit} (resets at {window_end})")]
    OutputTokensExceeded {
        used: u64,
        limit: u64,
        window_end: DateTime<Utc>,
    },
    #[error("request count quota exceeded: {used} / {limit} (resets at {window_end})")]
    RequestCountExceeded {
        used: u64,
        limit: u64,
        window_end: DateTime<Utc>,
    },
    /// Phase 3 — aggregate USD budget across the cycle window
    /// exceeded. Surfaced when a call's accumulated cost pushes
    /// the running total past `budget_per_cycle`. Display
    /// rounds to 4 decimal places to keep log lines readable.
    #[error("USD budget exceeded: ${used:.4} / ${limit:.4} (resets at {window_end})")]
    BudgetExceeded {
        used: f64,
        limit: f64,
        window_end: DateTime<Utc>,
    },
}
