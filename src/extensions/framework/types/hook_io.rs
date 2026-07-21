//! Compatibility re-export for the `peko_extension_api::hook_io` module.
//!
//! `HookInput`, `HookOutput`, `HookResult`, and `tool_result_from_hook`
//! moved into the `peko-extension-api` workspace crate in Phase 7.
//! The `From<ToolResult> for HookOutput` impl also moved there
//! (orphan rule: both `ToolResult` and `HookOutput` are now in crates
//! the API crate owns/depends on, so the impl lives there).
//!
//! This shim re-exports the API crate's `hook_io::*` and adds
//! ergonomic `HookInput::CompactionPreparation` /
//! `HookInput::CompactionResult` constructors that accept the typed
//! payload from the engine and serialize to the `serde_json::Value`
//! fields the API crate's variants carry.

pub use peko_extension_api::hook_io::*;

use peko_message::LlmMessage;
use serde_json::Value;
use std::path::PathBuf;

/// Payload struct for the `HookInput::CompactionPreparation` variant.
///
/// Phase 7 introduces this so the engine can construct the variant
/// with typed data; the variant itself carries the fields as
/// `serde_json::Value` blobs (the API crate must not depend on
/// `crate::session::compaction::*`).
#[derive(Debug, Clone)]
pub struct CompactionPreparationPayload {
    pub messages_to_summarize: Vec<LlmMessage>,
    pub turn_prefix_messages: Vec<LlmMessage>,
    pub is_split_turn: bool,
    pub previous_summary: Option<String>,
    pub file_ops: Value,
    pub estimated_tokens: usize,
    pub threshold_tokens: usize,
    pub model_context_limit: usize,
    pub settings: Value,
}

impl CompactionPreparationPayload {
    /// Construct a `HookInput::CompactionPreparation` from typed
    /// engine-side data. The non-trivial fields are serialized to JSON.
    #[must_use]
    pub fn into_hook_input(self) -> HookInput {
        HookInput::CompactionPreparation {
            messages_to_summarize: serde_json::to_value(&self.messages_to_summarize)
                .unwrap_or(Value::Null),
            turn_prefix_messages: serde_json::to_value(&self.turn_prefix_messages)
                .unwrap_or(Value::Null),
            is_split_turn: self.is_split_turn,
            previous_summary: self.previous_summary,
            file_ops: self.file_ops,
            estimated_tokens: self.estimated_tokens,
            threshold_tokens: self.threshold_tokens,
            model_context_limit: self.model_context_limit,
            settings: self.settings,
        }
    }

    /// Decode the messages back to `Vec<LlmMessage>`. Companion to
    /// `into_hook_input` for hook consumers that need typed data.
    pub fn decode_messages_to_summarize(hook_input: &HookInput) -> Option<Vec<LlmMessage>> {
        if let HookInput::CompactionPreparation {
            ref messages_to_summarize,
            ..
        } = *hook_input
        {
            serde_json::from_value(messages_to_summarize.clone()).ok()
        } else {
            None
        }
    }

    pub fn decode_turn_prefix_messages(hook_input: &HookInput) -> Option<Vec<LlmMessage>> {
        if let HookInput::CompactionPreparation {
            ref turn_prefix_messages,
            ..
        } = *hook_input
        {
            serde_json::from_value(turn_prefix_messages.clone()).ok()
        } else {
            None
        }
    }
}

/// Payload struct for the `HookInput::CompactionResult` variant.
#[derive(Debug, Clone)]
pub struct CompactionResultPayload {
    pub summary: String,
    pub messages_compacted: usize,
    pub tokens_before: usize,
    pub tokens_after: usize,
    pub compaction_number: usize,
    pub details: Option<Value>,
    pub messages_after: Vec<LlmMessage>,
}

impl CompactionResultPayload {
    /// Construct a `HookInput::CompactionResult` from typed engine-side data.
    #[must_use]
    pub fn into_hook_input(self) -> HookInput {
        HookInput::CompactionResult {
            summary: self.summary,
            messages_compacted: self.messages_compacted,
            tokens_before: self.tokens_before,
            tokens_after: self.tokens_after,
            compaction_number: self.compaction_number,
            details: self.details,
            messages_after: serde_json::to_value(&self.messages_after).unwrap_or(Value::Null),
        }
    }

    /// Decode the messages-after array back to `Vec<LlmMessage>`.
    pub fn decode_messages_after(hook_input: &HookInput) -> Option<Vec<LlmMessage>> {
        if let HookInput::CompactionResult {
            ref messages_after, ..
        } = *hook_input
        {
            serde_json::from_value(messages_after.clone()).ok()
        } else {
            None
        }
    }
}

// Re-export `PathBuf` so consumers that import it through this module
// (rare, but possible) keep working. The API crate does not re-export
// `PathBuf` itself because `HookInput` no longer carries a `PathBuf`
// field — `PathBuf` was only used in the pre-Phase-7
// `PromptBuildState` which has moved too. The re-export here is a
// safety net for any downstream `use` that still names the path.
#[allow(dead_code)]
fn _force_pathbuf_link(_p: PathBuf) {}
