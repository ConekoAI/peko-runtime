//! History repair for LLM request building.
//!
//! A session JSONL can accumulate structural damage when concurrent
//! writers (cron jobs, steering, channel traffic) append to it while a
//! turn is mid-flight — e.g. a user message landing between an
//! assistant `ToolCall` and its `ToolResult`, or consecutive same-role
//! messages (2026-08-07 field test, finding N1). Anthropic-style
//! provider APIs reject such histories outright ("tool call result does
//! not follow tool call"), permanently bricking the session because
//! every later request replays the same broken prefix.
//!
//! [`repair_history`] normalises a loaded history back into a shape the
//! providers accept. Storage stays faithful — this runs at consumption
//! time (engine intake), never mutating the JSONL.

use crate::{ContentBlock, LlmMessage, MessageRole};

/// Text used for synthetic results backfilled when an assistant
/// `ToolCall` has no recorded result anywhere in the history.
const INTERRUPTED_RESULT: &str =
    "[repair] tool execution was interrupted before a result was recorded";

/// Repair a loaded message history so it satisfies provider-side
/// structural invariants:
///
/// 1. **Pairing** — every assistant message carrying `ToolCall` blocks
///    is immediately followed by a single `Tool` message containing one
///    `ToolResult` per call id. Real results are reused wherever they
///    appeared later in the history (they are moved adjacent to their
///    call); calls with no recorded result get a synthetic
///    `is_error: true` result so the wire shape stays valid.
/// 2. **No orphans** — `ToolResult` blocks whose call id matches no
///    assistant `ToolCall` are dropped.
/// 3. **Alternation** — consecutive same-role messages are merged
///    (content concatenated), except that the synthesized
///    assistant → tool pairing is always preserved.
/// 4. **No empty messages** — messages with no content blocks are
///    dropped.
///
/// The function is idempotent: repairing an already-valid history is a
/// no-op (aside from the no-op merge pass).
#[must_use]
pub fn repair_history(messages: Vec<LlmMessage>) -> Vec<LlmMessage> {
    // Pass 1: merge consecutive same-role messages.
    let mut merged: Vec<LlmMessage> = Vec::with_capacity(messages.len());
    for msg in messages {
        if let Some(last) = merged.last_mut() {
            if last.role == msg.role {
                last.content.extend(msg.content);
                if last.usage.is_none() {
                    last.usage = msg.usage;
                }
                continue;
            }
        }
        merged.push(msg);
    }

    // Pass 2: lift every ToolResult block out of Tool messages into a
    // lookup keyed by call id, dropping the original Tool messages.
    // First occurrence wins so the pairing is deterministic. The
    // original message timestamp and message-level tool_call_id ride
    // along so an already-clean single-call history round-trips
    // byte-identically.
    let mut results: std::collections::HashMap<
        String,
        (ContentBlock, chrono::DateTime<chrono::Utc>, Option<String>),
    > = std::collections::HashMap::new();
    let mut without_tools: Vec<LlmMessage> = Vec::with_capacity(merged.len());
    for mut msg in merged {
        if msg.role == MessageRole::Tool {
            for block in msg.content {
                if let ContentBlock::ToolResult { tool_call_id, .. } = &block {
                    results
                        .entry(tool_call_id.clone())
                        .or_insert((block, msg.timestamp, msg.tool_call_id.clone()));
                }
            }
            continue;
        }
        // A non-Tool message can also carry ToolResult blocks in this
        // model (adapters sometimes pack them into user messages) —
        // lift those too so they don't violate block-level ordering.
        if msg
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolResult { .. }))
        {
            let mut kept = Vec::with_capacity(msg.content.len());
            for block in msg.content {
                match block {
                    ContentBlock::ToolResult { ref tool_call_id, .. } => {
                        results
                            .entry(tool_call_id.clone())
                            .or_insert((block, msg.timestamp, msg.tool_call_id.clone()));
                    }
                    other => kept.push(other),
                }
            }
            msg.content = kept;
        }
        without_tools.push(msg);
    }

    // Pass 3: after each assistant message with ToolCall blocks, insert
    // exactly one Tool message pairing every call id with its (real or
    // synthetic) result. Drop messages left with no content.
    let mut out: Vec<LlmMessage> = Vec::with_capacity(without_tools.len());
    for msg in without_tools {
        if msg.content.is_empty() {
            continue;
        }
        let call_ids: Vec<String> = if msg.role == MessageRole::Assistant {
            msg.content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::ToolCall { id, .. } => Some(id.clone()),
                    _ => None,
                })
                .collect()
        } else {
            Vec::new()
        };
        let ts = msg.timestamp;
        out.push(msg);
        if !call_ids.is_empty() {
            let single = call_ids.len() == 1;
            let mut result_ts = ts;
            let mut result_msg_id = None;
            let content = call_ids
                .into_iter()
                .map(|id| {
                    match results.remove(&id) {
                        Some((block, ts, msg_id)) => {
                            result_ts = ts;
                            result_msg_id = msg_id;
                            block
                        }
                        None => ContentBlock::ToolResult {
                            tool_call_id: id.clone(),
                            name: String::new(),
                            content: vec![ContentBlock::Text {
                                text: INTERRUPTED_RESULT.to_string(),
                            }],
                            is_error: true,
                        },
                    }
                })
                .collect();
            out.push(LlmMessage {
                role: MessageRole::Tool,
                content,
                timestamp: result_ts,
                tool_call_id: if single { result_msg_id } else { None },
                ..LlmMessage::default()
            });
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn text(role: MessageRole, t: &str) -> LlmMessage {
        LlmMessage {
            role,
            content: vec![ContentBlock::Text { text: t.to_string() }],
            timestamp: Utc::now(),
            ..LlmMessage::default()
        }
    }

    fn call(id: &str) -> LlmMessage {
        LlmMessage {
            role: MessageRole::Assistant,
            content: vec![ContentBlock::ToolCall {
                id: id.to_string(),
                name: "Bash".to_string(),
                arguments: serde_json::json!({}),
            }],
            timestamp: Utc::now(),
            ..LlmMessage::default()
        }
    }

    fn result(id: &str, body: &str) -> LlmMessage {
        LlmMessage {
            role: MessageRole::Tool,
            tool_call_id: Some(id.to_string()),
            content: vec![ContentBlock::ToolResult {
                tool_call_id: id.to_string(),
                name: "Bash".to_string(),
                content: vec![ContentBlock::Text { text: body.to_string() }],
                is_error: false,
            }],
            timestamp: Utc::now(),
            ..LlmMessage::default()
        }
    }

    fn roles(msgs: &[LlmMessage]) -> Vec<MessageRole> {
        msgs.iter().map(|m| m.role).collect()
    }

    #[test]
    fn clean_history_is_unchanged() {
        let h = vec![
            text(MessageRole::User, "hi"),
            call("a"),
            result("a", "ok"),
            text(MessageRole::Assistant, "done"),
        ];
        let out = repair_history(h.clone());
        assert_eq!(format!("{out:?}"), format!("{h:?}"));
    }

    #[test]
    fn interleaved_user_between_call_and_result_is_repaired() {
        // The N1 poison shape: cron injects a user message mid-turn.
        let out = repair_history(vec![
            text(MessageRole::User, "do it"),
            call("x"),
            text(MessageRole::User, "cron tick"),
            result("x", "slept 70s"),
            text(MessageRole::Assistant, "all good"),
        ]);
        assert_eq!(
            roles(&out),
            vec![
                MessageRole::User,
                MessageRole::Assistant,
                MessageRole::Tool,
                MessageRole::User,
                MessageRole::Assistant
            ]
        );
        // The real result content rides with the call, not dropped.
        match &out[2].content[0] {
            ContentBlock::ToolResult { content, is_error, .. } => {
                assert!(!is_error);
                assert!(matches!(&content[0], ContentBlock::Text { text } if text == "slept 70s"));
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[test]
    fn missing_result_gets_synthetic_error() {
        let out = repair_history(vec![text(MessageRole::User, "go"), call("y")]);
        assert_eq!(roles(&out), vec![MessageRole::User, MessageRole::Assistant, MessageRole::Tool]);
        match &out[2].content[0] {
            ContentBlock::ToolResult { is_error, .. } => assert!(is_error),
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[test]
    fn orphan_result_is_dropped() {
        let out = repair_history(vec![
            text(MessageRole::User, "hi"),
            result("ghost", "stale"),
            text(MessageRole::Assistant, "hello"),
        ]);
        assert_eq!(
            roles(&out),
            vec![MessageRole::User, MessageRole::Assistant]
        );
    }

    #[test]
    fn consecutive_same_role_merges() {
        let out = repair_history(vec![
            text(MessageRole::User, "a"),
            text(MessageRole::User, "b"),
            text(MessageRole::Assistant, "c"),
            text(MessageRole::Assistant, "d"),
        ]);
        assert_eq!(roles(&out), vec![MessageRole::User, MessageRole::Assistant]);
        assert_eq!(out[0].content.len(), 2);
        assert_eq!(out[1].content.len(), 2);
    }

    #[test]
    fn repair_is_idempotent() {
        let poisoned = vec![
            text(MessageRole::User, "do it"),
            call("x"),
            text(MessageRole::User, "tick"),
            result("x", "ok"),
            text(MessageRole::Assistant, "a"),
            text(MessageRole::Assistant, "b"),
            call("y"),
        ];
        let once = repair_history(poisoned);
        let twice = repair_history(once.clone());
        assert_eq!(format!("{once:?}"), format!("{twice:?}"));
    }
}
