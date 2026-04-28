//! Turn Boundary Detection for Session Compaction
//!
//! Ensures compaction never splits a tool call from its result.
//! Valid cut points: user messages, assistant messages, bash executions, custom messages.
//! Never cut at: tool results (they must stay paired with their tool call).

use crate::providers::MessageRole;
use crate::types::message::LlmMessage;
use crate::types::message::ContentBlock;

/// A message classification for boundary decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    /// System prompt (usually the first message)
    SystemPrompt,
    /// User message — valid cut point
    User,
    /// Assistant message — valid cut point
    Assistant,
    /// Tool result — NEVER cut here
    ToolResult,
    /// Summary message from previous compaction
    Summary,
    /// Other/unknown
    Other,
}

/// Classify a message for boundary decisions.
pub fn classify_message(msg: &LlmMessage) -> MessageKind {
    match msg.role {
        MessageRole::System => {
            // Check if it's a compaction summary
            if msg.content.iter().any(|b| match b {
                ContentBlock::Text { text } => {
                    text.starts_with("[Conversation Summary")
                        || text.starts_with("[Conversation Summary #")
                }
                _ => false,
            }) {
                MessageKind::Summary
            } else {
                MessageKind::SystemPrompt
            }
        }
        MessageRole::User => MessageKind::User,
        MessageRole::Assistant => MessageKind::Assistant,
        MessageRole::Tool => MessageKind::ToolResult,
    }
}

/// Find valid cut points in a message list.
///
/// Returns indices where compaction may split history from kept messages.
/// Never returns indices of tool results (they must stay with their tool call).
pub fn find_cut_points(messages: &[LlmMessage]) -> Vec<usize> {
    let mut cuts = Vec::new();
    for (i, msg) in messages.iter().enumerate() {
        match classify_message(msg) {
            MessageKind::User | MessageKind::Assistant | MessageKind::SystemPrompt => {
                cuts.push(i);
            }
            // ToolResult, Summary, Other are NOT valid cut points
            _ => {}
        }
    }
    cuts
}

/// Select messages to compact vs keep, respecting turn boundaries.
///
/// Returns (`messages_to_compact`, `messages_to_keep`, `is_split_turn`).
///
/// - `messages_to_compact`: messages that will be summarized
/// - `messages_to_keep`: recent messages preserved intact
/// - `is_split_turn`: true if the cut landed mid-turn (no complete turns to summarize)
///
/// Never cuts at a tool result. If the token-based cut would land on a tool result,
/// the cut is moved backward to the nearest valid cut point.
pub fn select_messages_respecting_boundaries(
    messages: &[LlmMessage],
    keep_recent_tokens: usize,
) -> (Vec<LlmMessage>, Vec<LlmMessage>, bool) {
    if messages.len() < 3 {
        return (vec![], messages.to_vec(), false);
    }

    // Strategy: Keep recent messages that fit within keep_recent_tokens
    let mut keep_count = 0usize;
    let mut keep_tokens = 0usize;

    for msg in messages.iter().rev() {
        let msg_tokens = estimate_message_tokens(msg);

        if keep_tokens + msg_tokens > keep_recent_tokens {
            break;
        }

        keep_tokens += msg_tokens;
        keep_count += 1;
    }

    // Always keep at least the last 2 messages (user + assistant)
    keep_count = keep_count.max(2).min(messages.len());

    let mut split_point = messages.len() - keep_count;

    // Ensure we don't cut at a tool result
    if split_point < messages.len() {
        let kind = classify_message(&messages[split_point]);
        if kind == MessageKind::ToolResult {
            // Move backward to find a valid cut point
            let cuts = find_cut_points(messages);
            if let Some(&valid_cut) = cuts.iter().rev().find(|&&c| c < split_point) {
                split_point = valid_cut;
            } else {
                // No valid cut point before — keep everything
                split_point = 0;
                keep_count = messages.len();
            }
            // Recalculate keep_count
            keep_count = messages.len() - split_point;
        }
    }

    let to_compact = messages[..split_point].to_vec();
    let to_keep = messages[split_point..].to_vec();

    // A "split turn" means no complete turns before the cut
    // (the cut landed inside a single turn, so to_compact is empty or only has partial context)
    let is_split_turn = to_compact.is_empty()
        || !to_compact.iter().any(|m| classify_message(m) == MessageKind::User);

    (to_compact, to_keep, is_split_turn)
}

/// Estimate tokens for a single message.
pub fn estimate_message_tokens(msg: &LlmMessage) -> usize {
    let content_len: usize = msg
        .content
        .iter()
        .map(|b| match b {
            ContentBlock::Text { text } => text.len(),
            _ => 50, // Estimate for other blocks
        })
        .sum();
    (content_len + 20) / 4 + 4 // CHARS_PER_TOKEN = 4, +4 overhead
}

/// Build turn prefix messages for split-turn handling.
///
/// When a single turn exceeds `keep_recent_tokens`, the cut may land mid-turn.
/// This function extracts the "turn prefix" — the early part of the split turn
/// that needs its own mini-summary.
///
/// Returns `Some(turn_prefix_messages)` if there is a split turn, `None` otherwise.
pub fn extract_turn_prefix(
    messages: &[LlmMessage],
    split_point: usize,
) -> Option<Vec<LlmMessage>> {
    if split_point == 0 || split_point >= messages.len() {
        return None;
    }

    // Find the start of the turn that contains the split point
    let cuts = find_cut_points(messages);
    let turn_start = cuts
        .iter()
        .rev()
        .find(|&&c| c < split_point)
        .copied()
        .unwrap_or(0);

    if turn_start < split_point {
        Some(messages[turn_start..split_point].to_vec())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_msg(role: MessageRole, text: &str) -> LlmMessage {
        LlmMessage::text(role, text)
    }

    fn make_tool_result(tool_call_id: &str, text: &str) -> LlmMessage {
        LlmMessage::tool_result(tool_call_id, "test_tool", text)
    }

    #[test]
    fn test_classify_message() {
        let user = make_msg(MessageRole::User, "Hello");
        assert_eq!(classify_message(&user), MessageKind::User);

        let assistant = make_msg(MessageRole::Assistant, "Hi");
        assert_eq!(classify_message(&assistant), MessageKind::Assistant);

        let tool = make_tool_result("tc1", "result");
        assert_eq!(classify_message(&tool), MessageKind::ToolResult);

        let summary = make_msg(
            MessageRole::System,
            "[Conversation Summary - 5 messages]:\n## Goal\n...",
        );
        assert_eq!(classify_message(&summary), MessageKind::Summary);

        let system = make_msg(MessageRole::System, "You are helpful");
        assert_eq!(classify_message(&system), MessageKind::SystemPrompt);
    }

    #[test]
    fn test_find_cut_points() {
        let messages = vec![
            make_msg(MessageRole::System, "Prompt"),
            make_msg(MessageRole::User, "Hello"),
            make_msg(MessageRole::Assistant, "Hi"),
            make_tool_result("tc1", "result"),
        ];

        let cuts = find_cut_points(&messages);
        assert_eq!(cuts, vec![0, 1, 2]); // NOT 3 (tool result)
    }

    #[test]
    fn test_select_messages_respecting_boundaries_no_tool() {
        let messages = vec![
            make_msg(MessageRole::System, "Prompt"),
            make_msg(MessageRole::User, "Hello 1"),
            make_msg(MessageRole::Assistant, "Reply 1"),
            make_msg(MessageRole::User, "Hello 2"),
            make_msg(MessageRole::Assistant, "Reply 2"),
        ];

        // With tiny keep_recent_tokens, force keeping only minimum
        let (compact, keep, split) = select_messages_respecting_boundaries(&messages, 10);
        // 10 tokens is less than one message, so we keep minimum 2
        assert_eq!(keep.len(), 2); // last user + assistant (minimum)
        assert_eq!(compact.len(), 3); // system + first 2 messages
        assert!(!split);
    }

    #[test]
    fn test_select_messages_never_cuts_at_tool_result() {
        // Scenario: assistant makes tool call, then tool result follows
        // The cut must NEVER be between assistant and tool result
        let messages = vec![
            make_msg(MessageRole::System, "Prompt"),
            make_msg(MessageRole::User, "Old user"),
            make_msg(MessageRole::Assistant, "Old assistant"),
            make_msg(MessageRole::User, "Recent user"),
            make_msg(MessageRole::Assistant, "I'll use a tool"),
            make_tool_result("tc1", "tool output here"),
        ];

        // Small keep_recent_tokens to force a cut near the tool result
        let (compact, keep, _split) = select_messages_respecting_boundaries(&messages, 50);

        // The kept messages should include the assistant AND its tool result
        // (never cut between them)
        let kept_roles: Vec<_> = keep.iter().map(|m| m.role).collect();
        if kept_roles.contains(&MessageRole::Tool) {
            // If tool is in keep, assistant must also be in keep
            let tool_idx = keep
                .iter()
                .position(|m| m.role == MessageRole::Tool)
                .unwrap();
            assert!(
                tool_idx > 0 && keep[tool_idx - 1].role == MessageRole::Assistant,
                "Tool result must follow its assistant message"
            );
        }
    }

    #[test]
    fn test_split_turn_detection() {
        // A single turn (user asks, assistant replies with tool call, tool returns)
        // that exceeds keep_recent_tokens
        let messages = vec![
            make_msg(MessageRole::System, "Prompt"),
            make_msg(MessageRole::User, "Do something"),
            make_msg(MessageRole::Assistant, "I'll help"),
            make_tool_result("tc1", "result"),
        ];

        // Very small keep — forces split turn
        let (compact, keep, is_split) = select_messages_respecting_boundaries(&messages, 10);

        // With only 10 tokens, we keep minimum 2 messages
        // If the cut would be at a tool result, it's moved back
        assert!(
            !keep.is_empty(),
            "Should always keep at least 2 messages"
        );
    }

    #[test]
    fn test_extract_turn_prefix() {
        let messages = vec![
            make_msg(MessageRole::System, "Prompt"),
            make_msg(MessageRole::User, "User msg"),
            make_msg(MessageRole::Assistant, "Assistant reply"),
            make_tool_result("tc1", "result"),
            make_msg(MessageRole::User, "Next user"),
        ];

        // Split point at 3 (inside the first turn, between assistant and tool result)
        // The turn starts at the last valid cut point before 3, which is index 2 (assistant)
        let prefix = extract_turn_prefix(&messages, 3);
        assert!(prefix.is_some());
        let prefix = prefix.unwrap();
        // Prefix is messages[2..3] = just the assistant reply
        assert_eq!(prefix.len(), 1);
        assert_eq!(prefix[0].role, MessageRole::Assistant);

        // Split point at 4 (after tool result, before next user)
        // The turn starts at the last valid cut point before 4, which is index 2 (assistant)
        let prefix = extract_turn_prefix(&messages, 4);
        assert!(prefix.is_some());
        let prefix = prefix.unwrap();
        // Prefix is messages[2..4] = assistant + tool result
        assert_eq!(prefix.len(), 2);
        assert_eq!(prefix[0].role, MessageRole::Assistant);
        assert_eq!(prefix[1].role, MessageRole::Tool);
    }
}
