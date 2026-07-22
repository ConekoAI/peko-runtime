//! Pure-data types for the compaction subsystem.
//!
//! Phase 9b.N.4 lifted these out of `src/session/compaction.rs` and
//! `src/session/compaction/background.rs` so the `CompactionOrchestrator`
//! (also lifted in 9b.N.4) can build them without a root dependency.
//! The structs/enums are pure data: no behavior, no provider/session
//! coupling. The `BackgroundCompactor` and `Compactor` logic that
//! produces and consumes them stays in root — the orchestrator only
//! needs the shapes for its bookkeeping and the trait-port signatures.
//!
//! Types lifted:
//! - [`CompactionConfig`] — user-tunable settings
//!   (`src/session/compaction.rs:107`).
//! - [`CompactionEntry`] — record of one compaction run
//!   (`src/session/compaction.rs:173`).
//! - [`CompactionState`] — running counters
//!   (`src/session/compaction.rs:195`).
//! - [`ContextUsageEstimate`] — F21 hybrid token estimator result
//!   (`src/session/compaction.rs:207`).
//! - [`CompactionResult`] — completion payload from the compactor
//!   (`src/session/compaction.rs:243`).
//! - [`CompactionQuota`] — max-compactions-per-session + cooldown
//!   tracking (`src/session/compaction/background.rs:44`).
//! - [`CompactionRequest`] — request envelope
//!   (`src/session/compaction/background.rs:65`).
//! - [`CompactionResponse`] — completion outcome (the orchestrator
//!   polls this enum via `tokio::sync::oneshot`)
//!   (`src/session/compaction/background.rs:76`).
//!
//! The root crate re-exports these names from
//! `src/session/compaction.rs` so pre-Phase-9b.N.4 import paths
//! (`crate::session::compaction::CompactionConfig`, etc.) keep
//! resolving. `CompactionDetails` (file-ops accumulator) stays in
//! `src/session/compaction/summary_format.rs` because it's only
//! consumed inside `Compactor::compact`, which the orchestrator never
//! touches.

use anyhow::Result;
use peko_message::{LlmMessage, TokenUsage};
use serde::{Deserialize, Serialize};

/// Compaction configuration
///
/// User-tunable settings read from `~/.peko/config.toml` under the
/// `[compaction]` block. See `CompactionOrchestrator::load_config`
/// (root) for the loader.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionConfig {
    /// Enable auto-compaction
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Auto-compaction trigger threshold as percent of context window (0-100)
    #[serde(default = "default_auto_threshold_percent")]
    pub auto_threshold_percent: u8,
    /// Tokens to reserve for LLM response headroom
    #[serde(default = "default_reserve_tokens")]
    pub reserve_tokens: usize,
    /// Minimum recent conversation to preserve during compaction
    #[serde(default = "default_keep_recent_tokens")]
    pub keep_recent_tokens: usize,
    /// Maximum compactions per session (quota)
    #[serde(default = "default_max_compactions_per_session")]
    pub max_compactions_per_session: usize,
    /// Cooldown between compactions in seconds
    #[serde(default = "default_cooldown_seconds")]
    pub cooldown_seconds: u64,
}

fn default_enabled() -> bool {
    true
}

fn default_auto_threshold_percent() -> u8 {
    85
}

fn default_reserve_tokens() -> usize {
    16_384
}

fn default_keep_recent_tokens() -> usize {
    20_000
}

fn default_max_compactions_per_session() -> usize {
    100
}

fn default_cooldown_seconds() -> u64 {
    60
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            auto_threshold_percent: default_auto_threshold_percent(),
            reserve_tokens: default_reserve_tokens(),
            keep_recent_tokens: default_keep_recent_tokens(),
            max_compactions_per_session: default_max_compactions_per_session(),
            cooldown_seconds: default_cooldown_seconds(),
        }
    }
}

/// A compaction entry in the conversation history
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionEntry {
    /// When compaction occurred
    pub timestamp: chrono::DateTime<chrono::Utc>,
    /// Summary text (structured format)
    pub summary: String,
    /// Entry ID of first kept message (for reference)
    pub first_kept_entry_id: String,
    /// Number of messages that were compacted
    pub messages_compacted: usize,
    /// Approximate tokens before compaction
    pub tokens_before: usize,
    /// Approximate tokens after compaction
    pub tokens_after: usize,
    /// Compaction number (1st, 2nd, etc.)
    pub compaction_number: usize,
    /// Tracked file operations from compacted messages.
    ///
    /// Defined in `src/session/compaction/summary_format.rs` and
    /// re-exported here as `Box<dyn Any>` would break serde — the
    /// orchestrator never deserializes this directly; it only
    /// forwards it to the session log. We type it as `serde_json::Value`
    /// here (the wire shape is what the compactor emits) and the root
    /// crate re-exports the concrete type alongside for callers that
    /// want to inspect details.
    ///
    /// TODO(phase9b-n4-cleanup): if root callers end up needing
    /// strongly-typed access to details, lift `summary_format` +
    /// `CompactionDetails` into `peko-engine` and switch this field
    /// back to the concrete type. Out of scope for this PR — the
    /// orchestrator only stores/passes the value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

/// Tracks compaction state for a session
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompactionState {
    /// Number of compactions performed
    pub compaction_count: usize,
    /// Total tokens saved through compaction
    pub total_tokens_saved: usize,
    /// Last compaction timestamp
    pub last_compaction_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Detailed token usage estimate with breakdown (F21 hybrid estimator).
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ContextUsageEstimate {
    /// Total estimated tokens
    pub tokens: usize,
    /// Tokens from the last assistant usage record
    pub usage_tokens: usize,
    /// Tokens estimated for trailing messages after last usage
    pub trailing_tokens: usize,
    /// Index of the last assistant message with usage data
    pub last_usage_index: Option<usize>,
}

/// Result of a compaction operation.
///
/// Mirrors `src/session/compaction.rs:243` with the `details` field
/// widened to `serde_json::Value` (see [`CompactionEntry`] docstring
/// for the rationale).
#[derive(Debug, Clone)]
pub struct CompactionResult {
    /// Messages after compaction (summary + kept messages)
    pub messages: Vec<LlmMessage>,
    /// Compaction entry for persistence
    pub entry: CompactionEntry,
    /// State update
    pub state: CompactionState,
    /// Token usage consumed by the summarization LLM call(s).
    /// Previously dropped on the floor; tracked here so the engine
    /// loop can add it to `total_usage` for accurate downstream
    /// quota / billing accounting.
    pub usage: TokenUsage,
}

/// Compaction quota tracking (`src/session/compaction/background.rs:44`).
///
/// `BackgroundCompactor` owns one of these; the orchestrator reads
/// it through the [`crate::compaction::CompactorBackend`] trait port
/// to decide whether to ask for another compaction.
///
/// `Copy` matches the original in `background.rs` (no heap state).
#[derive(Debug, Clone, Copy, Default)]
pub struct CompactionQuota {
    /// Minimum time between compactions (seconds).
    pub cooldown_seconds: u64,
    /// Maximum compactions per session.
    pub max_compactions_per_session: usize,
    /// Maximum consecutive auto-compactions before forcing a manual trigger.
    pub max_consecutive_auto: usize,
}

/// Request envelope passed to the [`CompactorBackend`]
/// (`src/session/compaction/background.rs:65`).
///
/// This is the **public** shape — the orchestrator constructs one
/// of these per call and hands it to the backend. The original
/// `background::CompactionRequest` (which carries the
/// `response_tx: oneshot::Sender<CompactionResponse>`) is the
/// **internal** shape used to ferry the request through the
/// worker's `mpsc` channel; the backend wrapper attaches the
/// `response_tx` before forwarding, so it stays inside `background.rs`.
/// Phase 9b.N.4 deliberately does not lift the internal type to
/// keep the trait port minimal.
#[derive(Debug, Clone)]
pub struct CompactionRequest {
    /// Messages to potentially compact
    pub messages: Vec<LlmMessage>,
    /// Previous summary for cumulative updates (None for initial)
    pub previous_summary: Option<String>,
}

/// Completion outcome from the `BackgroundCompactor` worker
/// (`src/session/compaction/background.rs:76`).
///
/// The orchestrator polls the oneshot receiver and pattern-matches
/// on this enum. Variants:
///
/// - `Completed(CompactionResult)` — compactor produced a summary;
///   the orchestrator folds the result into `messages`, records the
///   entry into the session, and emits the post-compaction hook.
/// - `NotNeeded` — compactor decided no compaction was required.
/// - `Skipped(reason)` — compactor skipped (e.g. cooldown active,
///   quota reached). Reason is a free-form debug string.
/// - `Failed(err)` — compactor errored; surfaced as a warn log.
///   `err` is the formatted error message (the orchestrator treats
///   it as opaque display text).
///
/// `Clone` matches the original in `background.rs:75` so the existing
/// worker-task `mpsc` plumbing continues to type-check.
#[derive(Debug, Clone)]
pub enum CompactionResponse {
    Completed(CompactionResult),
    NotNeeded,
    Skipped(String),
    Failed(String),
}

impl CompactionResponse {
    /// Convenience helper for the orchestrator's polling loop —
    /// matches `Completed` and yields the inner result, otherwise
    /// returns `None`.
    #[must_use]
    pub fn into_completed(self) -> Option<CompactionResult> {
        match self {
            CompactionResponse::Completed(r) => Some(r),
            _ => None,
        }
    }
}

// Re-export the result type alias so callers can name
// `Result<CompactionResponse>` if they want without the anyhow path.
pub type CompactionResponseResult = Result<CompactionResponse>;
