//! Typed quota errors.
//!
//! Three variants — one per limit kind — so the CLI and log lines
//! can tell the user exactly which wall they hit and when it
//! resets. Flows through `anyhow::Error` upstream (no `From` impl
//! needed; `QuotaError` is `Send + Sync + 'static` via `thiserror`).

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
}
