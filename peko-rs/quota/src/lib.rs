//! Per-principal token quota (F18).
//!
//! Each Principal can opt into a token quota with three independent
//! limits — `input_tokens`, `output_tokens`, `request_count` — reset
//! on a calendar-aligned UTC cycle (Hourly / Daily / Weekly /
//! Monthly). When any limit trips, the next LLM call is rejected
//! with [`error::QuotaError`] and the run aborts mid-flight.
//!
//! ## Module map
//!
//! - [`config::QuotaConfig`] — TOML-deserializable limit block.
//! - [`config::QuotaCycle`] — calendar cycle enum + `next_boundary` math.
//! - [`state::QuotaState`] — runtime counters, persisted to JSON.
//! - [`meter::QuotaMeter`] — the runtime check / charge / reset engine.
//! - [`error::QuotaError`] — typed error variants for the three limits.
//!
//! ## F17 hook points
//!
//! [`meter::QuotaMeter::charge`] takes a
//! [`peko_message::TokenUsage`] and folds cache +
//! reasoning into the canonical `input` / `output` buckets via
//! [`peko_message::TokenUsage::accumulate`]. Callers
//! should never re-implement that folding inline — `accumulate` is
//! the single source of truth for "what counts toward quota".

pub mod config;
pub mod error;
pub mod meter;
pub mod scope;
pub mod state;

pub use config::{QuotaConfig, QuotaCycle};
pub use error::QuotaError;
pub use meter::QuotaMeter;
pub use scope::QuotaScope;
pub use state::QuotaState;
