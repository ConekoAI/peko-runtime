//! Pure-data types for the compaction subsystem.
//!
//! Phase 7 promotes these into the `peko-session` crate. The
//! persistence layer owns the data layout (`CompactionEntry`,
//! `CompactionState`, `ContextUsageEstimate`, `CompactionResult`,
//! `CompactionRequest`, `CompactionResponse`, `CompactionQuota`,
//! `CompactionConfig`), so they live alongside the persistence impl
//! that produces and consumes them. `peko-engine` re-exports them
//! via `peko_engine::compaction::{CompactionConfig, ...}` so the
//! pre-Phase-7 import paths keep compiling.
//!
//! Phase 9b.N.4 lifted these out of `src/session/compaction.rs` and
//! `src/session/compaction/background.rs` so the `CompactionOrchestrator`
//! could build them without a root dependency. Phase 7 moves them one
//! step further — into the crate that owns the persistence impl
//! itself — so the orchestrator's re-export is the only thing left in
//! `peko-engine` for the data layout.

use anyhow::Result;
use peko_message::{LlmMessage, TokenUsage};
use serde::{Deserialize, Serialize};

/// Compaction configuration
///
/// User-tunable settings read from `~/.peko/config.toml` under the
/// `[compaction]` block. See `load_compaction_config()` in this crate
/// for the loader.
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
    /// Phase 9b.N.4 widened this from
    /// `summary_format::CompactionDetails` to
    /// `serde_json::Value` so the orchestrator can store/persist
    /// the value without depending on the root-owned summary
    /// helper. Hooks see `serde_json::Value` blobs and degrade
    /// gracefully if the structure changes. The `summary_format`
    /// module (still in this crate) owns the strongly-typed
    /// `CompactionDetails` and is consumed only inside
    /// `Compactor::compact`.
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
/// `details` is `serde_json::Value` to avoid a hard dep on the
/// `summary_format::CompactionDetails` type from the orchestrator's
/// hot path (the orchestrator just forwards the value into the
/// session log). Strongly-typed access goes through
/// `serde_json::from_value::<summary_format::CompactionDetails>(v)`.
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

/// Compaction quota tracking — owned by `BackgroundCompactor`.
///
/// `BackgroundCompactor` reads/writes one of these; the orchestrator
/// consults the dual-threshold check separately so the quota state is
/// internal to the worker.
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

/// Request envelope passed to the [`super::CompactorBackend`].
///
/// This is the **public** shape — the orchestrator constructs one of
/// these per call and hands it to the backend. The internal
/// `background::CompactionRequest` (which carries the
/// `response_tx: oneshot::Sender<CompactionResponse>`) is the
/// worker-channel plumbing and stays inside `background.rs`. The
/// backend wrapper attaches the `response_tx` before forwarding, so
/// it stays internal to `background.rs`. Phase 7 deliberately does
/// not lift the internal type to keep the trait port minimal.
#[derive(Debug, Clone)]
pub struct CompactionRequest {
    /// Messages to potentially compact
    pub messages: Vec<LlmMessage>,
    /// Previous summary for cumulative updates (None for initial)
    pub previous_summary: Option<String>,
}

/// Completion outcome from the `BackgroundCompactor` worker.
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
