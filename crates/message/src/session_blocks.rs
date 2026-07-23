//! Session content blocks shared between `peko-engine` and root's
//! session implementation.
//!
//! Phase 9b.N.5b.9b lifted these from `crate::session::events` so
//! `peko_engine::SessionView::add_assistant_with_blocks` can accept
//! them in its trait signature. The engine crate cannot name root-only
//! types, and these are pure data blocks (`serde` derives only, no
//! behavior) — perfect for the neutral `peko-message` crate.
//!
//! # Why here, not in `peko-provider-api`
//!
//! `ToolCallBlock` and `ThinkingBlock` describe **session** storage
//! shape (what the JSONL writer persists + what the resumer reads
//! back), not provider wire format. Provider wire formats use their
//! own tool-call representations in `peko-provider-api`.
//!
//! # Backwards compatibility
//!
//! Root's `src/session/events.rs` re-exports these types via
//! `pub use peko_message::{ThinkingBlock, ToolCallBlock};` so existing
//! `crate::session::events::ToolCallBlock` import paths continue to
//! resolve unchanged. The original struct definitions at
//! `src/session/events.rs:332-346` are removed; the re-export takes
//! their place.

use serde::{Deserialize, Serialize};

/// Tool call block for LLM-native session storage.
///
/// Persists a tool invocation alongside the assistant message that
/// requested it, so a resumed session can replay the call against the
/// tool funnel without re-prompting the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallBlock {
    /// Stable id linking this block to the assistant tool-call entry.
    pub id: String,
    /// Canonical tool name (e.g. `"Bash"`, `"Read"`).
    pub name: String,
    /// Raw JSON arguments the LLM emitted for the call.
    pub arguments: serde_json::Value,
}

/// Thinking block for reasoning models.
///
/// Persists the model's reasoning trace alongside its assistant
/// message. `signature` is the provider-issued opaque token that lets
/// Anthropic (and similar providers) verify the trace on a resumed
/// turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinkingBlock {
    /// Reasoning text emitted by the model.
    pub text: String,
    /// Provider-issued signature for trace verification. `None` for
    /// providers that don't emit one.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub signature: Option<String>,
}
