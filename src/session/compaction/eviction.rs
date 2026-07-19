//! Front-eviction for `ContextWindowExceeded` recovery.
//!
//! When the provider rejects a request because the conversation exceeds its
//! context window, peko's loop drops the oldest message and retries. This module
//! holds the helper that performs the drop *while preserving tool-call / tool-result
//! pair boundaries* — the same invariant `turn_boundaries::select_messages_respecting_boundaries`
//! enforces on the compaction side.
//!
//! Mirrors codex's `remove_first_item` + `normalize::remove_corresponding_for`
//! pattern at `codex-rs/core/src/context_manager/history.rs:186-197` and
//! `codex-rs/core/src/context_manager/normalize.rs:222-291`. Peko's field names
//! differ from codex's: `ContentBlock::ToolCall { id, .. }` and
//! `ContentBlock::ToolResult { tool_call_id, .. }` (see `common/types/message.rs:33-46`).

use crate::common::types::message::{ContentBlock, LlmMessage, MessageRole};

/// Drop the oldest message and any paired counterpart.
///
/// Returns the number of messages removed (0 if `messages` was empty).
///
/// Pairing rules:
/// - If the dropped message is a `Tool` result, the immediately-following (older)
///   messages are searched for an `Assistant` containing a `ToolCall` with the same
///   `id`/`tool_call_id`. If found, that assistant message is removed too.
///   *(In practice the call always sits directly above the result, but codex's
///   `remove_corresponding_for` scans the whole list — we mirror that.)*
/// - If the dropped message is an `Assistant` with a `ToolCall`, the remaining
///   (newer) messages are searched for a `Tool` result with the same `id`, and
///   it is removed too.
///
/// Returns 0 when the list is empty (the eviction caller should not loop again).
pub fn drop_oldest_respecting_pairs(messages: &mut Vec<LlmMessage>) -> usize {
    if messages.is_empty() {
        return 0;
    }
    let oldest = messages.remove(0);
    let mut removed = 1;

    // Case 1: oldest is a tool result — drop the matching call above it.
    if let Some(call_id) = first_tool_result_call_id(&oldest) {
        if let Some(pos) = messages.iter().position(has_tool_call_with_id(&call_id)) {
            messages.remove(pos);
            removed += 1;
        }
    }

    // Case 2: oldest is an assistant with a tool call — drop the matching result below.
    if let Some(call_id) = first_tool_call_id(&oldest) {
        if let Some(pos) = messages.iter().position(has_tool_result_with_id(&call_id)) {
            messages.remove(pos);
            removed += 1;
        }
    }

    removed
}

/// Returns the `tool_call_id` of the first `ContentBlock::ToolResult` in `msg`, if any.
fn first_tool_result_call_id(msg: &LlmMessage) -> Option<String> {
    msg.content.iter().find_map(|b| match b {
        ContentBlock::ToolResult { tool_call_id, .. } => Some(tool_call_id.clone()),
        _ => None,
    })
}

/// Returns the `id` of the first `ContentBlock::ToolCall` in `msg`, if any.
fn first_tool_call_id(msg: &LlmMessage) -> Option<String> {
    msg.content.iter().find_map(|b| match b {
        ContentBlock::ToolCall { id, .. } => Some(id.clone()),
        _ => None,
    })
}

/// Closure-style predicate: returns true iff `msg` contains a `ToolCall` whose
/// `id` matches `call_id`.
fn has_tool_call_with_id(call_id: &str) -> impl FnMut(&LlmMessage) -> bool + '_ {
    move |msg: &LlmMessage| {
        if msg.role != MessageRole::Assistant {
            return false;
        }
        msg.content.iter().any(|b| match b {
            ContentBlock::ToolCall { id, .. } => id == call_id,
            _ => false,
        })
    }
}

/// Closure-style predicate: returns true iff `msg` is a `Tool` message containing
/// a `ToolResult` whose `tool_call_id` matches `call_id`.
fn has_tool_result_with_id(call_id: &str) -> impl FnMut(&LlmMessage) -> bool + '_ {
    move |msg: &LlmMessage| {
        if msg.role != MessageRole::Tool {
            return false;
        }
        msg.content.iter().any(|b| match b {
            ContentBlock::ToolResult { tool_call_id, .. } => tool_call_id == call_id,
            _ => false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user_msg(text: &str) -> LlmMessage {
        LlmMessage::user(text)
    }

    fn assistant_with_tool_call(id: &str, name: &str) -> LlmMessage {
        let mut msg = LlmMessage::assistant("");
        msg.content.push(ContentBlock::ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments: serde_json::json!({}),
        });
        msg
    }

    #[test]
    fn test_drop_oldest_respecting_pairs_returns_zero_on_empty() {
        let mut msgs: Vec<LlmMessage> = vec![];
        assert_eq!(drop_oldest_respecting_pairs(&mut msgs), 0);
        assert!(msgs.is_empty());
    }

    #[test]
    fn test_drop_oldest_drops_only_user_when_no_pair() {
        let mut msgs = vec![user_msg("hello"), user_msg("world")];
        let removed = drop_oldest_respecting_pairs(&mut msgs);
        assert_eq!(removed, 1);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, MessageRole::User);
    }

    #[test]
    fn test_drop_oldest_drops_only_assistant_call_when_no_result() {
        let mut msgs = vec![assistant_with_tool_call("tc1", "Read"), user_msg("next")];
        let removed = drop_oldest_respecting_pairs(&mut msgs);
        assert_eq!(removed, 1);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, MessageRole::User);
    }

    #[test]
    fn test_drop_oldest_drops_call_and_result_together() {
        // Oldest is an assistant with a tool call, and there's a matching
        // tool result later in the list — both should be evicted to avoid
        // orphaning a tool_call without its tool_result.
        let mut msgs = vec![
            assistant_with_tool_call("tc1", "Read"),
            user_msg("between"),
            LlmMessage::tool_result("tc1", "Read", "file contents"),
        ];
        let removed = drop_oldest_respecting_pairs(&mut msgs);
        assert_eq!(removed, 2, "should drop both call and result");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, MessageRole::User);
    }

    #[test]
    fn test_drop_oldest_drops_result_and_call_when_result_is_oldest() {
        // Oldest is a tool result — drop the matching call above it. The
        // scan searches the whole remaining list (not just position 1)
        // to mirror codex's `remove_corresponding_for` behavior.
        let mut msgs = vec![
            LlmMessage::tool_result("tc1", "Read", "file contents"),
            assistant_with_tool_call("tc1", "Read"),
            user_msg("next"),
        ];
        let removed = drop_oldest_respecting_pairs(&mut msgs);
        assert_eq!(removed, 2, "should drop both result and matching call");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, MessageRole::User);
    }

    #[test]
    fn test_drop_oldest_does_not_split_call_result_pair_with_different_id() {
        // The dropped assistant's tool_call_id doesn't match the tool result below.
        // Only the assistant should be dropped.
        let mut msgs = vec![
            assistant_with_tool_call("tc_old", "Read"),
            user_msg("between"),
            LlmMessage::tool_result("tc_new", "Read", "different result"),
        ];
        let removed = drop_oldest_respecting_pairs(&mut msgs);
        assert_eq!(removed, 1, "no pair match — only drop the oldest");
        assert_eq!(msgs.len(), 2);
    }
}
