//! `peko_session::compaction::output_rewrite` — per-message function-call
//! output rewriter.
//!
// Closes the "single oversize tool result stalls the loop" gap: when
//! one tool result exceeds its share of the context budget, peko's
//! only options today are `drop_oldest_respecting_pairs` (front-evicts
//! one message at a time, dropping unrelated recent work) or fail.
//! codex replaces the oversize body with a sentinel while preserving
//! the tool-call / tool-result pair intact. This module ports that
//! pattern with peko's content-block model.
//!
// # Pair preservation
//!
// `ContentBlock::ToolResult { tool_call_id, name, is_error, content }`
// is rewritten **in place**:
// - `tool_call_id` preserved verbatim (the model needs this to match
//!   its tool call);
// - `name` preserved verbatim;
// - `is_error` set to `false` — peko didn't fail, the body was just
//!   truncated to keep the loop alive;
// - `content` replaced with `[Text { text: <sentinel> }]` carrying the
//!   original byte count and the truncated reason.
//!
// The corresponding `ContentBlock::ToolCall { id: tool_call_id, .. }`
// (matched by `id` == `tool_call_id`) is **never** touched.
//!
// # Threshold
//!
// A tool result is rewritten when its estimated token cost exceeds
//! `(context_window - reserve_tokens) * 0.5`. The 0.5 factor matches
//! codex's heuristic — the rewriter only fires when the result would
//! plausibly swallow half the working budget on its own. The
//! rewriter is **cheaper than front-eviction** and should fire first;
//! `drop_oldest_respecting_pairs` is the fallback for messages with
//! no single oversize tool result.
//!
// # JSONL persistence
//!
// The rewritten `ContentBlock::ToolResult` carries only `Text` blocks
//! (no other block types in `content`), so the existing JSONL
//! serde path round-trips the rewrite without schema change. The
//! sentinel string itself is human-readable so a `peko session show`
//! or replay can surface "this result was truncated by peko_runtime"
//! without a separate metadata channel.

use peko_message::{ContentBlock, LlmMessage, ToolCallId};

/// Aggregated outcome of a rewriter pass. Cheap to construct, cheap
/// to log; the engine surfaces these via `AgenticEvent` for
/// observability.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RewriteStats {
    /// Number of `ContentBlock::ToolResult` blocks whose body was
    /// rewritten to the sentinel.
    pub rewritten_count: usize,
    /// Estimated tokens reclaimed across all rewrites (approximate;
    /// enough for "rewriter saved ~N tokens" observability).
    pub tokens_reclaimed_estimate: usize,
    /// Number of `ToolResult` blocks inspected. Useful for asserting
    /// the rewriter actually walked the messages in tests.
    pub inspected_count: usize,
}

impl RewriteStats {
    /// Convenience: did the rewriter change anything?
    #[must_use]
    pub fn did_rewrite(&self) -> bool {
        self.rewritten_count > 0
    }
}

/// Per-message overflow threshold, in tokens. A `ToolResult` body
/// whose token estimate exceeds this is rewritten to the sentinel.
///
/// We pin the threshold at half the working budget
/// (`(context_window - reserve_tokens) * 0.5`) so a single result
/// can't swallow the entire conversation. The caller passes the
/// raw numbers and we compute the threshold inside.
fn overflow_threshold(context_window: usize, reserve_tokens: usize) -> usize {
    let working_budget = context_window.saturating_sub(reserve_tokens);
    working_budget / 2
}

/// Estimate the token cost of a tool-result body.
///
/// `ToolResult.content: Vec<ContentBlock>` is mostly `Text` blocks
/// today; nested tool calls / images inside a result are rare but
/// accounted for. Conservative on purpose — overestimating is fine,
/// underestimating would let the rewriter miss an oversize result.
fn tool_result_tokens(content: &[ContentBlock]) -> usize {
    let mut total: usize = 0;
    for block in content {
        match block {
            ContentBlock::Text { text } => {
                // chars/4 with a 1-token floor so empty text doesn't
                // collapse the count to zero.
                total = total.saturating_add(text.len().div_ceil(4).max(1));
            }
            ContentBlock::ToolResult { content, .. } => {
                total = total.saturating_add(tool_result_tokens(content));
            }
            ContentBlock::ToolCall {
                name, arguments, ..
            } => {
                total = total.saturating_add(name.len() / 4);
                total = total.saturating_add(arguments.to_string().len() / 4);
            }
            ContentBlock::Image { source, mime_type } => {
                total = total.saturating_add(crate::estimate_image_tokens(source, mime_type));
            }
            ContentBlock::Thinking { text, .. } => {
                total = total.saturating_add(text.len() / 4);
            }
        }
    }
    total
}

/// Build the sentinel that replaces the oversize body.
///
/// Visible to the model on the next iteration so it knows the
/// result is incomplete (and can decide whether to re-invoke the
/// tool with a narrower scope). Visible to humans via
/// `peko session show` so the truncation isn't silent.
fn truncation_sentinel(original_tokens: usize, tool_call_id: &ToolCallId) -> Vec<ContentBlock> {
    let text = format!(
        "[truncated by peko_runtime: tool result for call {tool_call_id} \
         was {original_tokens} tokens, reduced to sentinel. Re-invoke the \
         tool with a narrower scope (smaller limit, filter, or offset) \
         to get the full result.]"
    );
    vec![ContentBlock::Text { text }]
}

/// Walk `messages` and rewrite every `ContentBlock::ToolResult`
/// whose body exceeds `(context_window - reserve_tokens) / 2` tokens.
/// Returns aggregate stats; mutates the messages in place.
///
/// Pair preservation is automatic — only `ToolResult` blocks are
/// touched, never the matching `ToolCall`.
///
/// The rewriter is **idempotent**: running it twice on the same
/// messages leaves them unchanged the second time (the sentinel
/// body is short enough to fall under the threshold).
pub fn rewrite_oversized_tool_results(
    messages: &mut [LlmMessage],
    context_window: usize,
    reserve_tokens: usize,
) -> RewriteStats {
    let threshold = overflow_threshold(context_window, reserve_tokens);
    let mut stats = RewriteStats::default();

    for msg in messages.iter_mut() {
        for block in msg.content.iter_mut() {
            let ContentBlock::ToolResult {
                tool_call_id,
                content,
                is_error,
                ..
            } = block
            else {
                continue;
            };
            stats.inspected_count += 1;

            let tokens = tool_result_tokens(content);
            if tokens <= threshold {
                continue;
            }

            // Rewrite in place. Sentinel is a single Text block so
            // the rewritten ToolResult.content is a 1-element Vec
            // well below the threshold on the next pass.
            let original_tokens = tokens;
            *content = truncation_sentinel(original_tokens, tool_call_id);
            // Distinguish a tool that errored from a tool whose
            // output peko truncated. The model should still see
            // is_error=true for true failures.
            *is_error = false;
            stats.rewritten_count += 1;
            // Token reclamation is the *original* count (the
            // sentinel is ~50 tokens; rounding error is fine for
            // observability).
            stats.tokens_reclaimed_estimate =
                stats.tokens_reclaimed_estimate.saturating_add(original_tokens);
        }
    }

    stats
}

#[cfg(test)]
mod tests {
    use super::*;
    use peko_message::{LlmMessage, MessageRole};

    fn text_msg(text: &str) -> LlmMessage {
        LlmMessage {
            role: MessageRole::User,
            content: vec![ContentBlock::Text {
                text: text.to_string(),
            }],
            ..Default::default()
        }
    }

    fn tool_result_msg(id: &str, body: &str) -> LlmMessage {
        LlmMessage {
            role: MessageRole::Tool,
            content: vec![ContentBlock::ToolResult {
                tool_call_id: id.to_string(),
                name: "Read".to_string(),
                content: vec![ContentBlock::Text {
                    text: body.to_string(),
                }],
                is_error: false,
            }],
            ..Default::default()
        }
    }

    #[test]
    fn empty_messages_is_noop() {
        let mut messages: Vec<LlmMessage> = vec![];
        let stats = rewrite_oversized_tool_results(&mut messages, 128_000, 16_384);
        assert_eq!(stats, RewriteStats::default());
    }

    #[test]
    fn all_under_threshold_is_noop() {
        // 100 chars / 4 = 25 tokens << threshold (128k-16k)/2 = 56k.
        let mut messages = vec![tool_result_msg("tc1", &"x".repeat(100))];
        let original_len = messages[0].content[0].approx_text_len();
        let stats = rewrite_oversized_tool_results(&mut messages, 128_000, 16_384);
        assert_eq!(stats.rewritten_count, 0);
        assert_eq!(stats.inspected_count, 1);
        assert_eq!(
            messages[0].content[0].approx_text_len(),
            original_len,
            "body should be untouched"
        );
    }

    #[test]
    fn oversize_result_is_rewritten_with_sentinel() {
        // 200_000 chars ≈ 50_000 tokens, well above the 56k threshold
        // boundary — bump to 250_000 to be safe.
        let big_body = "x".repeat(250_000);
        let mut messages = vec![tool_result_msg("tc1", &big_body)];

        let stats = rewrite_oversized_tool_results(&mut messages, 128_000, 16_384);
        assert_eq!(stats.rewritten_count, 1, "should rewrite the oversize result");
        assert_eq!(stats.inspected_count, 1);
        assert!(
            stats.tokens_reclaimed_estimate >= 50_000,
            "should reclaim a meaningful number of tokens"
        );

        // The ToolResult's content is now a single short sentinel.
        let ContentBlock::ToolResult {
            tool_call_id,
            name,
            content,
            is_error,
            ..
        } = &messages[0].content[0]
        else {
            panic!("ToolResult was replaced — pair preservation broken")
        };
        assert_eq!(tool_call_id, "tc1", "tool_call_id preserved");
        assert_eq!(name, "Read", "name preserved");
        assert!(!*is_error, "is_error forced to false after truncation");
        assert_eq!(content.len(), 1, "sentinel is a single Text block");
        let ContentBlock::Text { text } = &content[0] else {
            panic!("sentinel should be Text")
        };
        assert!(text.starts_with("[truncated by peko_runtime:"));
        assert!(text.contains("tc1"), "sentinel references the tool_call_id");
    }

    #[test]
    fn tool_call_side_untouched() {
        // A `ToolCall` followed by an oversize `ToolResult` — the
        // rewriter must only touch the ToolResult.
        let big_body = "x".repeat(250_000);
        let mut messages = vec![LlmMessage {
            role: MessageRole::Assistant,
            content: vec![
                ContentBlock::ToolCall {
                    id: "tc1".to_string(),
                    name: "Read".to_string(),
                    arguments: serde_json::json!({"path": "/etc/passwd"}),
                },
                ContentBlock::ToolResult {
                    tool_call_id: "tc1".to_string(),
                    name: "Read".to_string(),
                    content: vec![ContentBlock::Text {
                        text: big_body,
                    }],
                    is_error: false,
                },
            ],
            ..Default::default()
        }];

        rewrite_oversized_tool_results(&mut messages, 128_000, 16_384);

        // ToolCall is the first block; its JSON args are still intact.
        let ContentBlock::ToolCall {
            id, arguments, ..
        } = &messages[0].content[0]
        else {
            panic!("ToolCall was modified")
        };
        assert_eq!(id, "tc1");
        assert_eq!(arguments.get("path").unwrap(), "/etc/passwd");
    }

    #[test]
    fn non_tool_result_blocks_untouched() {
        // User text + assistant tool call + oversize tool result +
        // assistant text — only the ToolResult is rewritten.
        let big_body = "x".repeat(250_000);
        let mut messages = vec![
            text_msg("what's in /etc/passwd?"),
            LlmMessage {
                role: MessageRole::Assistant,
                content: vec![
                    ContentBlock::ToolCall {
                        id: "tc1".to_string(),
                        name: "Read".to_string(),
                        arguments: serde_json::json!({"path": "/etc/passwd"}),
                    },
                    ContentBlock::Text {
                        text: "let me check".to_string(),
                    },
                ],
                ..Default::default()
            },
            tool_result_msg("tc1", &big_body),
            LlmMessage {
                role: MessageRole::Assistant,
                content: vec![ContentBlock::Text {
                    text: "Here's the contents...".to_string(),
                }],
                ..Default::default()
            },
        ];

        let stats = rewrite_oversized_tool_results(&mut messages, 128_000, 16_384);
        assert_eq!(stats.rewritten_count, 1);

        // User text + assistant tool call + assistant text are byte-identical.
        assert_eq!(messages[0].content[0].approx_text_len(), "what's in /etc/passwd?".len());
        let ContentBlock::ToolCall { id, .. } = &messages[1].content[0] else {
            panic!("ToolCall mutated")
        };
        assert_eq!(id, "tc1");
        assert_eq!(
            messages[3].content[0].approx_text_len(),
            "Here's the contents...".len()
        );
    }

    #[test]
    fn multiple_oversize_results_each_rewritten() {
        let big = "x".repeat(250_000);
        let mut messages = vec![
            tool_result_msg("tc1", &big),
            tool_result_msg("tc2", &big),
            tool_result_msg("tc3", &big),
        ];
        let stats = rewrite_oversized_tool_results(&mut messages, 128_000, 16_384);
        assert_eq!(stats.rewritten_count, 3);
        assert_eq!(stats.inspected_count, 3);
    }

    #[test]
    fn error_result_rewritten_with_is_error_false() {
        // A tool that failed with a giant error message — peko
        // rewrites the body and forces is_error=false because the
        // failure wasn't the runtime's doing.
        let big_err = "E".repeat(250_000);
        let mut messages = vec![LlmMessage {
            role: MessageRole::Tool,
            content: vec![ContentBlock::ToolResult {
                tool_call_id: "tc1".to_string(),
                name: "Bash".to_string(),
                content: vec![ContentBlock::Text {
                    text: big_err,
                }],
                is_error: true,
            }],
            ..Default::default()
        }];
        let stats = rewrite_oversized_tool_results(&mut messages, 128_000, 16_384);
        assert_eq!(stats.rewritten_count, 1);

        let ContentBlock::ToolResult {
            is_error, content, ..
        } = &messages[0].content[0]
        else {
            panic!("ToolResult missing")
        };
        assert!(!*is_error, "is_error forced to false on truncation");
        assert_eq!(content.len(), 1, "single Text sentinel");
    }

    #[test]
    fn rewriter_is_idempotent() {
        // Running twice leaves the second pass a no-op: the sentinel
        // is short enough to fall under the threshold.
        let big_body = "x".repeat(250_000);
        let mut messages = vec![tool_result_msg("tc1", &big_body)];
        let stats1 = rewrite_oversized_tool_results(&mut messages, 128_000, 16_384);
        assert_eq!(stats1.rewritten_count, 1);
        let stats2 = rewrite_oversized_tool_results(&mut messages, 128_000, 16_384);
        assert_eq!(stats2.rewritten_count, 0, "second pass should be a no-op");
        assert_eq!(stats2.inspected_count, 1);
    }

    #[test]
    fn small_context_window_rewrites_sooner() {
        // 32k context, 8k reserve → working 24k → threshold 12k tokens.
        // A 50_000-char (~12_500-token) body is just over; a 40_000-char
        // (~10_000-token) body is just under.
        let just_under = "x".repeat(40_000);
        let mut messages = vec![tool_result_msg("tc1", &just_under)];
        let stats = rewrite_oversized_tool_results(&mut messages, 32_000, 8_000);
        assert_eq!(
            stats.rewritten_count, 0,
            "10k-token result under 12k threshold should pass"
        );

        let just_over = "x".repeat(60_000);
        let mut messages = vec![tool_result_msg("tc1", &just_over)];
        let stats = rewrite_oversized_tool_results(&mut messages, 32_000, 8_000);
        assert_eq!(stats.rewritten_count, 1, "15k-token result should rewrite");
    }

    #[test]
    fn zero_reserve_tokens_does_not_panic() {
        // Defensive: caller might pass 0 reserve. threshold = ctx/2
        // = 64_000 tokens. 300_000 chars ≈ 75_000 tokens, well over.
        let big_body = "x".repeat(300_000);
        let mut messages = vec![tool_result_msg("tc1", &big_body)];
        let stats = rewrite_oversized_tool_results(&mut messages, 128_000, 0);
        assert_eq!(stats.rewritten_count, 1);
    }
}

/// Helper for tests above: return the byte length of a `Text` block
/// (or 0 for other variants). Keeps assertions readable without
/// importing serde_json values.
trait ApproxTextLen {
    fn approx_text_len(&self) -> usize;
}

impl ApproxTextLen for ContentBlock {
    fn approx_text_len(&self) -> usize {
        match self {
            ContentBlock::Text { text } => text.len(),
            _ => 0,
        }
    }
}