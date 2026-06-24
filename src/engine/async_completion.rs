//! Async task completion message synthesis.
//!
//! The agentic loop drains completed async tasks at the start of each
//! iteration and surfaces them to the LLM as a single synthetic
//! user-role `LlmMessage`. This module owns that synthesis so it can be
//! tested in isolation and so `agentic_loop.rs` stays focused on the
//! loop itself.

use crate::common::types::message::{ContentBlock, LlmMessage, MessageRole};
use crate::extensions::framework::async_exec::executor::{AsyncTaskStatus, CompletionEvent};
use chrono::Utc;
use std::collections::HashMap;

/// Maximum size of a tool result to include verbatim in the synthetic
/// completion message. Results larger than this are truncated and the
/// model is told to call `task output` for the full content. Keeps the
/// LLM context window bounded when a long-running tool produces a large
/// payload.
const MAX_RESULT_PREVIEW_BYTES: usize = 2048;

/// Suffix appended to truncated previews.
const TRUNCATION_SUFFIX: &str =
    "\n\n... (truncated; use `task output` for full result)";

/// Truncate a result string to `MAX_RESULT_PREVIEW_BYTES`, respecting
/// UTF-8 char boundaries, and append a suffix pointing the model at
/// `task output` for the full content.
fn truncate_for_preview(text: &str) -> String {
    if text.len() <= MAX_RESULT_PREVIEW_BYTES {
        return text.to_string();
    }
    let mut end = MAX_RESULT_PREVIEW_BYTES;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = String::with_capacity(end + TRUNCATION_SUFFIX.len());
    out.push_str(&text[..end]);
    out.push_str(TRUNCATION_SUFFIX);
    out
}

/// Build a synthetic user-role `LlmMessage` from a list of completed
/// async-task events. Filters to events whose `parent_session_key`
/// matches the current session. Returns `None` if no events belong to
/// this session.
///
/// The synthetic message contains:
/// - One `Text` header summarizing how many tasks completed.
/// - One `ToolResult` block per event, with `tool_call_id` of the
///   form `synthetic:<task_id>` so the model can reference a specific
///   completed task in its next tool call.
/// - Large results are truncated via [`truncate_for_preview`].
pub(crate) fn build_async_completion_message(
    events: &[CompletionEvent],
    session_id: &str,
) -> Option<LlmMessage> {
    let for_session: Vec<&CompletionEvent> = events
        .iter()
        .filter(|e| e.parent_session_key == session_id)
        .collect();
    if for_session.is_empty() {
        return None;
    }

    let n = for_session.len();
    let mut content = vec![ContentBlock::Text {
        text: format!("[Async task results — {n} completed since last turn]"),
    }];
    for event in for_session {
        let is_error = matches!(
            event.status,
            AsyncTaskStatus::Failed { .. }
                | AsyncTaskStatus::TimedOut { .. }
                | AsyncTaskStatus::Cancelled
        );
        content.push(ContentBlock::ToolResult {
            tool_call_id: format!("synthetic:{}", event.task_id),
            name: event.tool_name.clone(),
            content: vec![ContentBlock::Text {
                text: truncate_for_preview(&event.result.to_string()),
            }],
            is_error,
        });
    }

    Some(LlmMessage {
        role: MessageRole::User,
        content,
        timestamp: Utc::now(),
        metadata: HashMap::new(),
        tool_call_id: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::core::ToolResult;

    fn make_completion_event_with_status(
        task_id: &str,
        tool_name: &str,
        session_key: &str,
        status: AsyncTaskStatus,
    ) -> CompletionEvent {
        CompletionEvent {
            task_id: task_id.to_string(),
            tool_name: tool_name.to_string(),
            result: serde_json::json!({"exit_code": 0, "stdout": "hello"}),
            status,
            completed_at: chrono::Utc::now(),
            output_path: std::path::PathBuf::from("/tmp/fake.ndjson"),
            parent_session_key: session_key.to_string(),
        }
    }

    fn make_completion_event(
        task_id: &str,
        tool_name: &str,
        session_key: &str,
    ) -> CompletionEvent {
        make_completion_event_with_status(
            task_id,
            tool_name,
            session_key,
            AsyncTaskStatus::Completed {
                result: ToolResult::success(serde_json::json!({"exit_code": 0, "stdout": "hello"})),
            },
        )
    }

    #[test]
    fn test_build_async_completion_message_no_events() {
        let events: Vec<CompletionEvent> = vec![];
        let msg = build_async_completion_message(&events, "session_a");
        assert!(msg.is_none(), "Zero events should return None");
    }

    #[test]
    fn test_build_async_completion_message_one_matching_event() {
        let events = vec![make_completion_event("shell:x", "shell", "session_a")];
        let msg = build_async_completion_message(&events, "session_a");
        let msg = msg.expect("one matching event should produce Some(msg)");

        assert!(matches!(msg.role, MessageRole::User));

        // First content block must be the header text.
        match &msg.content[0] {
            ContentBlock::Text { text } => {
                assert_eq!(text, "[Async task results — 1 completed since last turn]");
            }
            other => panic!("expected Text header, got {other:?}"),
        }

        // Second block must be a ToolResult with synthetic:<task_id>.
        match &msg.content[1] {
            ContentBlock::ToolResult {
                tool_call_id,
                name,
                content,
                is_error,
            } => {
                assert_eq!(tool_call_id, "synthetic:shell:x");
                assert_eq!(name, "shell");
                assert!(!(*is_error));
                assert_eq!(content.len(), 1);
                match &content[0] {
                    ContentBlock::Text { text } => {
                        // Full raw result JSON, not truncated.
                        assert!(text.contains("exit_code"));
                    }
                    other => panic!("expected Text inside ToolResult, got {other:?}"),
                }
            }
            other => panic!("expected ToolResult block, got {other:?}"),
        }

        assert_eq!(msg.content.len(), 2);
    }

    #[test]
    fn test_build_async_completion_message_two_matching_events() {
        let events = vec![
            make_completion_event("shell:x", "shell", "session_a"),
            make_completion_event("shell:y", "shell", "session_a"),
        ];
        let msg = build_async_completion_message(&events, "session_a");
        let msg = msg.expect("two matching events should produce Some(msg)");

        match &msg.content[0] {
            ContentBlock::Text { text } => {
                assert_eq!(text, "[Async task results — 2 completed since last turn]");
            }
            other => panic!("expected Text header, got {other:?}"),
        }

        assert_eq!(msg.content.len(), 3, "header + 2 tool result blocks");
        // Sanity-check the two tool_call_id values.
        let mut ids: Vec<String> = Vec::new();
        for block in &msg.content[1..] {
            if let ContentBlock::ToolResult { tool_call_id, .. } = block {
                ids.push(tool_call_id.clone());
            } else {
                panic!("expected only ToolResult blocks after header, got {block:?}");
            }
        }
        assert_eq!(ids, vec!["synthetic:shell:x", "synthetic:shell:y"]);
    }

    #[test]
    fn test_build_async_completion_message_error_statuses() {
        // Failed
        let events = vec![make_completion_event_with_status(
            "shell:f",
            "shell",
            "session_a",
            AsyncTaskStatus::Failed {
                error: "oops".to_string(),
            },
        )];
        let msg = build_async_completion_message(&events, "session_a");
        let msg = msg.expect("failed event should produce Some(msg)");
        match &msg.content[1] {
            ContentBlock::ToolResult { is_error, .. } => {
                assert!(*is_error, "Failed status should set is_error=true");
            }
            other => panic!("expected ToolResult block, got {other:?}"),
        }

        // TimedOut
        let events = vec![make_completion_event_with_status(
            "shell:t",
            "shell",
            "session_a",
            AsyncTaskStatus::TimedOut {
                error: "timed out".to_string(),
            },
        )];
        let msg = build_async_completion_message(&events, "session_a");
        let msg = msg.expect("timed-out event should produce Some(msg)");
        match &msg.content[1] {
            ContentBlock::ToolResult { is_error, .. } => {
                assert!(*is_error, "TimedOut status should set is_error=true");
            }
            other => panic!("expected ToolResult block, got {other:?}"),
        }

        // Cancelled
        let events = vec![make_completion_event_with_status(
            "shell:c",
            "shell",
            "session_a",
            AsyncTaskStatus::Cancelled,
        )];
        let msg = build_async_completion_message(&events, "session_a");
        let msg = msg.expect("cancelled event should produce Some(msg)");
        match &msg.content[1] {
            ContentBlock::ToolResult { is_error, .. } => {
                assert!(*is_error, "Cancelled status should set is_error=true");
            }
            other => panic!("expected ToolResult block, got {other:?}"),
        }

        // Completed
        let events = vec![make_completion_event_with_status(
            "shell:ok",
            "shell",
            "session_a",
            AsyncTaskStatus::Completed {
                result: ToolResult::success(serde_json::json!({"ok": true})),
            },
        )];
        let msg = build_async_completion_message(&events, "session_a");
        let msg = msg.expect("completed event should produce Some(msg)");
        match &msg.content[1] {
            ContentBlock::ToolResult { is_error, .. } => {
                assert!(!(*is_error), "Completed status should set is_error=false");
            }
            other => panic!("expected ToolResult block, got {other:?}"),
        }
    }

    #[test]
    fn test_truncate_for_preview_short_text_passes_through() {
        let text = "hello world";
        assert_eq!(truncate_for_preview(text), "hello world");
    }

    #[test]
    fn test_truncate_for_preview_truncates_long_text() {
        let text = "a".repeat(MAX_RESULT_PREVIEW_BYTES + 100);
        let out = truncate_for_preview(&text);
        // The output is the truncated body plus the suffix.
        assert!(out.starts_with(&"a".repeat(MAX_RESULT_PREVIEW_BYTES)));
        assert!(out.ends_with(TRUNCATION_SUFFIX));
        // And it is shorter than the original.
        assert!(out.len() < text.len());
        // The truncated body itself is at most MAX_RESULT_PREVIEW_BYTES.
        let body_len = out.len() - TRUNCATION_SUFFIX.len();
        assert_eq!(body_len, MAX_RESULT_PREVIEW_BYTES);
    }

    #[test]
    fn test_truncate_for_preview_respects_utf8_boundary() {
        // Build a string of multi-byte chars (each is 2 bytes) that
        // straddles the limit on a non-boundary. The function must not
        // panic and must end on a char boundary.
        let char_count = MAX_RESULT_PREVIEW_BYTES; // 2048 chars
        let text: String = "ñ".repeat(char_count + 5); // each "ñ" is 2 bytes
        let out = truncate_for_preview(&text);
        // The suffix is present because the text is over the limit.
        assert!(out.ends_with(TRUNCATION_SUFFIX));
        // The body is valid UTF-8 (no panic when slicing) and shorter
        // than the limit in bytes.
        let body = &out[..out.len() - TRUNCATION_SUFFIX.len()];
        assert!(body.is_char_boundary(body.len()));
    }

    #[test]
    fn test_build_async_completion_message_truncates_large_result() {
        let big = "x".repeat(MAX_RESULT_PREVIEW_BYTES + 500);
        let events = vec![CompletionEvent {
            task_id: "shell:big".to_string(),
            tool_name: "shell".to_string(),
            result: serde_json::json!({"stdout": big}),
            status: AsyncTaskStatus::Completed {
                result: ToolResult::success(serde_json::json!({"stdout": big})),
            },
            completed_at: chrono::Utc::now(),
            output_path: std::path::PathBuf::from("/tmp/fake.ndjson"),
            parent_session_key: "session_a".to_string(),
        }];

        let msg = build_async_completion_message(&events, "session_a")
            .expect("event should produce Some(msg)");
        match &msg.content[1] {
            ContentBlock::ToolResult { content, .. } => match &content[0] {
                ContentBlock::Text { text } => {
                    assert!(
                        text.ends_with(TRUNCATION_SUFFIX),
                        "large result should be truncated with suffix; got len {}",
                        text.len()
                    );
                }
                other => panic!("expected Text, got {other:?}"),
            },
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[test]
    fn test_build_async_completion_message_filters_other_sessions() {
        let events = vec![make_completion_event("shell:x", "shell", "session_b")];
        let msg = build_async_completion_message(&events, "session_a");
        assert!(
            msg.is_none(),
            "events from a different session must be filtered out"
        );
    }
}
