//! Async task completion message synthesis.
//!
//! The agentic loop drains completed async tasks at the start of each
//! iteration and surfaces them to the LLM as a single synthetic
//! user-role `LlmMessage`. This module owns that synthesis so it can be
//! tested in isolation and so `agentic_loop.rs` stays focused on the
//! loop itself.
//!
//! Phase 9b.N.1: lifted from `src/engine/async_completion.rs`. The two
//! imports that couple this file to root — `AsyncTaskStatus` and
//! `CompletionEvent` — are already workspace crate types
//! (`peko_extension_api::AsyncTaskStatus` and
//! `peko_extension_api::CompletionEvent`), and the remaining
//! dependencies (`peko_message`, `peko_tools_core`) are already
//! `peko-engine` deps. No trait ports or session-coupled shims needed.

use crate::SessionView;
use chrono::Utc;
use peko_extension_api::AsyncTaskStatus;
use peko_message::{ContentBlock, LlmMessage, MessageRole};
use peko_session::events::MessageSource;
use std::collections::HashMap;

/// View trait over a completed async-task event used to build the
/// synthetic user-role message at iteration start.
///
/// Phase 9b.N.1 introduces this trait to avoid coupling
/// `peko-engine::async_completion` to a single concrete
/// `CompletionEvent` struct. Two structurally-identical types exist
/// side-by-side because the Phase 8 split moved one copy to
/// `peko-extension-host` (`peko_extension_api::CompletionEvent`)
/// while `peko_extension_api::CompletionEvent`
/// remains the legacy root-owned copy. Both implement this trait so the
/// synthesis function works against either path without forcing the
/// agentic loop to convert. Consolidating the two structs into one is
/// deferred to the Phase 8 follow-up bulk move.
pub trait AsyncCompletionLike {
    fn task_id(&self) -> &str;
    fn tool_name(&self) -> &str;
    fn result(&self) -> &serde_json::Value;
    fn status(&self) -> &AsyncTaskStatus;
    fn parent_session_key(&self) -> &str;
}

impl AsyncCompletionLike for peko_extension_api::CompletionEvent {
    fn task_id(&self) -> &str {
        &self.task_id
    }
    fn tool_name(&self) -> &str {
        &self.tool_name
    }
    fn result(&self) -> &serde_json::Value {
        &self.result
    }
    fn status(&self) -> &AsyncTaskStatus {
        &self.status
    }
    fn parent_session_key(&self) -> &str {
        &self.parent_session_key
    }
}

/// Phase 7 envelope impl: `AsyncInboxItem::Completion` now carries the
/// API crate's [`CompletionEnvelope`] (down from
/// `peko_extension_api::CompletionEvent`). This impl lets
/// `build_async_completion_message` consume envelope-form events
/// directly without an intermediate conversion back to the host type.
impl AsyncCompletionLike for peko_extension_api::CompletionEnvelope {
    fn task_id(&self) -> &str {
        &self.task_id
    }
    fn tool_name(&self) -> &str {
        &self.tool_name
    }
    fn result(&self) -> &serde_json::Value {
        &self.result
    }
    fn status(&self) -> &AsyncTaskStatus {
        &self.status
    }
    fn parent_session_key(&self) -> &str {
        &self.parent_session_key
    }
}

/// Maximum size of a tool result to include verbatim in the synthetic
/// completion message. Results larger than this are truncated and the
/// model is told to call `AsyncOutput` for the full content. Keeps the
/// LLM context window bounded when a long-running tool produces a large
/// payload.
const MAX_RESULT_PREVIEW_BYTES: usize = 2048;

/// Suffix appended to truncated previews.
const TRUNCATION_SUFFIX: &str = "\n\n... (truncated; use `AsyncOutput` for full result)";

/// Truncate a result string to `MAX_RESULT_PREVIEW_BYTES`, respecting
/// UTF-8 char boundaries, and append a suffix pointing the model at
/// `AsyncOutput` for the full content.
///
/// Public for use from `agentic_loop::run_inner`'s inbox-drain
/// persistence branch (WS3 — implicit session management).
pub fn truncate_for_preview(text: &str) -> String {
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
///
/// Generic over [`AsyncCompletionLike`] so the function works against
/// `peko_extension_api::CompletionEvent` (used directly in
/// `crates/engine` tests) and the legacy root-owned
/// `peko_extension_api::CompletionEvent`
/// (drained by `src/engine/agentic_loop.rs` from
/// `SharedSessionInbox`). Both are structurally identical.
pub fn build_async_completion_message<E: AsyncCompletionLike>(
    events: &[E],
    session_id: &str,
) -> Option<LlmMessage> {
    let for_session: Vec<&E> = events
        .iter()
        .filter(|e| e.parent_session_key() == session_id)
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
            event.status(),
            AsyncTaskStatus::Failed { .. }
                | AsyncTaskStatus::TimedOut { .. }
                | AsyncTaskStatus::Cancelled
        );
        content.push(ContentBlock::ToolResult {
            tool_call_id: format!("synthetic:{}", event.task_id()),
            name: event.tool_name().to_string(),
            content: vec![ContentBlock::Text {
                text: truncate_for_preview(&event.result().to_string()),
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
        usage: None,
    })
}

/// Persist subagent completions into the parent's live session JSONL.
///
/// WS3 (implicit session management): the agentic loop's inbox drain
/// injects a synthetic `LlmMessage` for the next LLM turn via
/// [`build_async_completion_message`], but that message lives only in
/// memory and is lost on reload. This helper writes the same payload
/// out — tagged with [`MessageSource::Agent`] — so the helper's output
/// is part of the parent's permanent transcript. Filtered to events
/// whose `parent_session_key` matches and whose `tool_name` is the
/// subagent dispatcher (`Agent`); other completions (cron, shell,
/// a2a) are out of scope for this hook and continue to live only in
/// the in-memory `messages` slice.
///
/// Returns `Ok(())` even when individual appends fail — log-and-continue
/// matches the steering-branch behavior in `agentic_loop::run_inner`'s
/// inbox drain. A torn append is recoverable via the
/// `torn_last_line_filtered` invariant in `peko_session::jsonl`.
pub async fn persist_subagent_completions<E: AsyncCompletionLike>(
    completions: &[E],
    session: &dyn SessionView,
    session_id: &str,
) {
    for event in completions {
        if event.parent_session_key() != session_id {
            continue;
        }
        if event.tool_name() != "Agent" {
            continue;
        }
        let persisted = format!(
            "📨 [Helper: {}] {}",
            event.tool_name(),
            truncate_for_preview(&event.result().to_string())
        );
        if let Err(e) = session
            .add_user_with_source(persisted, MessageSource::Agent)
            .await
        {
            tracing::warn!(
                "AgenticLoop: failed to persist subagent completion (task {}): {e}",
                event.task_id()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peko_tools_core::ToolResult;

    fn make_completion_event_with_status(
        task_id: &str,
        tool_name: &str,
        session_key: &str,
        status: AsyncTaskStatus,
    ) -> peko_extension_api::CompletionEvent {
        peko_extension_api::CompletionEvent {
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
    ) -> peko_extension_api::CompletionEvent {
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
        let events: Vec<peko_extension_api::CompletionEvent> = vec![];
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
        let events = vec![peko_extension_api::CompletionEvent {
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

    /// WS3 persistence helper: filters to events matching
    /// `parent_session_key` AND `tool_name == "Agent"`, then writes a
    /// `📨 [Helper: Agent] <result>` line to the live session tagged
    /// with `MessageSource::Agent`. The test exercises the public
    /// helper through `Arc<RwLock<Session>>` (the production wrapper
    /// type) and verifies the source tag round-trips through the
    /// native event loader.
    #[tokio::test]
    async fn test_persist_subagent_completions_filters_and_tags_source() {
        use peko_session::events::{MessageSource, SessionEvent, SessionMessage};
        use peko_session::{Arc as SessionArc, RwLock as SessionRwLock, Session};
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let storage = peko_session::jsonl::SessionStorage::new(temp_dir.path().to_path_buf());
        let peer = peko_subject::Subject::User("default".to_string());
        let session_id = "test-persist-completions";

        storage.create_session(session_id, None).await.unwrap();

        let session: SessionArc<SessionRwLock<Session>> = SessionArc::new(SessionRwLock::new(
            Session::open_by_id("test-agent", session_id, temp_dir.path(), Some(&peer))
                .await
                .unwrap(),
        ));

        // Mixed completion set:
        //   - one matching parent + Agent tool → must persist
        //   - one matching parent but `shell` tool → must NOT persist
        //   - one for a different session_key → must NOT persist
        let events = vec![
            make_completion_event("agent:x", "Agent", session_id),
            make_completion_event("shell:y", "shell", session_id),
            make_completion_event("agent:z", "Agent", "other-session"),
        ];
        // The blanket impl `impl<T> SessionView for Arc<tokio::sync::RwLock<T>>`
        // applies; `&session` is `&Arc<RwLock<Session>>` which coerces to
        // `&dyn SessionView` via unsized coercion at the function boundary.
        persist_subagent_completions(&events, &session, session_id).await;

        // Reload raw events and look for our `📨 [Helper: Agent]` line.
        let events = storage.load_events(session_id).await.unwrap();
        let agent_persisted: Vec<&SessionMessage> = events
            .iter()
            .filter_map(|ev| match ev {
                SessionEvent::MessageV2(m) => Some(m),
                _ => None,
            })
            .filter(|m| {
                matches!(
                    m.role_metadata,
                    peko_session::message::RoleMetadata::User {
                        source: MessageSource::Agent
                    }
                )
            })
            .collect();
        assert_eq!(
            agent_persisted.len(),
            1,
            "exactly one Agent-source entry should be persisted (Agent+session match), got {agent_persisted:?}"
        );
        // And the source tag is Agent (not User).
        assert!(matches!(
            agent_persisted[0].role_metadata,
            peko_session::message::RoleMetadata::User {
                source: MessageSource::Agent
            }
        ));
        // The body carries the `📨 [Helper: Agent]` prefix.
        if let peko_message::ContentBlock::Text { text } = &agent_persisted[0].message.content[0] {
            assert!(
                text.starts_with("📨 [Helper: Agent]"),
                "persisted body should carry the 📨 [Helper: Agent] prefix; got: {text}"
            );
        } else {
            panic!("persisted message body should be a Text block");
        }
    }

    /// `persist_subagent_completions` is log-and-continue: an append
    /// failure on one event does not abort the loop. Today this is
    /// hard to exercise without a failing append sink — covered here
    /// by a happy-path-only smoke test that asserts no panics on an
    /// empty event list (the common steady-state at runtime).
    #[tokio::test]
    async fn test_persist_subagent_completions_empty_noop() {
        use peko_session::{Arc as SessionArc, RwLock as SessionRwLock, Session};
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let storage = peko_session::jsonl::SessionStorage::new(temp_dir.path().to_path_buf());
        let peer = peko_subject::Subject::User("default".to_string());
        let session_id = "test-persist-noop";

        storage.create_session(session_id, None).await.unwrap();
        let session: SessionArc<SessionRwLock<Session>> = SessionArc::new(SessionRwLock::new(
            Session::open_by_id("test-agent", session_id, temp_dir.path(), Some(&peer))
                .await
                .unwrap(),
        ));
        let events: Vec<peko_extension_api::CompletionEvent> = vec![];
        persist_subagent_completions(&events, &session, session_id).await;

        // Nothing appended on the helper's side. `SessionCreated` /
        // other lifecycle events emitted by `open_by_id` are not in
        // scope for this assertion.
        let events = storage.load_events(session_id).await.unwrap();
        let message_v2_count = events
            .iter()
            .filter(|ev| {
                matches!(
                    ev,
                    peko_session::events::SessionEvent::MessageV2(_)
                )
            })
            .count();
        assert_eq!(
            message_v2_count, 0,
            "persist_subagent_completions must not append any MessageV2 events when the event list is empty"
        );
    }
}
