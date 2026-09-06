//! Message conversion utilities
//!
//! Provides pure functions for converting between session storage formats
//! and LLM message formats. This module is stateless and has no side effects.
//!
//! ## Responsibility
//!
//! - Convert `SessionEvent` → `LlmMessage`
//! - Convert `NormalizedEntry` → `LlmMessage`
//! - Convert `NormalizedEntry` slice → context text
//!
//! ## Design Principles
//!
//! - **Pure functions**: No mutable state, deterministic output
//! - **SRP**: Only conversion logic, no persistence or I/O
//! - **DRY**: Single source of truth for all message format conversions

use crate::compaction::summary_format::{format_summary_with_file_ops, CompactionDetails};
use crate::events::SessionEvent;
use crate::jsonl::NormalizedEntry;
use peko_message::ContentBlock;
use peko_message::{LlmMessage, MessageRole};

/// Convert a `SessionEvent` to an `LlmMessage`
///
/// This function handles the conversion from internal event format to
/// provider-agnostic `LlmMessage` format.
///
/// Uses the unified `as_message()` method to support both the new `MessageV2`
/// format and all legacy formats seamlessly.
pub fn event_to_llm_message(event: &SessionEvent) -> Option<LlmMessage> {
    // Use unified conversion for all message types (handles MessageV2 and legacy)
    if let Some(msg) = event.as_message() {
        return Some(msg.to_llm_message());
    }

    // Non-message events return None
    None
}

/// Metadata key stamped on the compaction-boundary summary `LlmMessage`
/// produced by [`compaction_summary_message`].
///
/// The engine's resume seeding treats a leading System-role message as
/// the system-prompt slot and lets the `PromptRenderer` overwrite
/// `messages[0]` on every iteration. Without this marker, a boundary
/// summary landing at index 0 would be mistaken for that slot and
/// silently destroyed on the first iteration after resume.
pub const COMPACTION_BOUNDARY_METADATA_KEY: &str = "peko.compaction_boundary";

/// Whether `msg` is a compaction-boundary summary message emitted by the
/// resume path (see [`COMPACTION_BOUNDARY_METADATA_KEY`]).
#[must_use]
pub fn is_compaction_boundary_message(msg: &LlmMessage) -> bool {
    msg.metadata
        .get(COMPACTION_BOUNDARY_METADATA_KEY)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

/// Find the newest compaction boundary in a stitched event list.
///
/// Returns the index of the last `SessionEvent::System` with
/// `event == "compaction"`. `SessionStorage::load_events` stitches
/// rotated pages oldest→newest, so a reverse scan finds the newest
/// boundary regardless of paging.
#[must_use]
pub fn latest_compaction_boundary(events: &[SessionEvent]) -> Option<usize> {
    events
        .iter()
        .rposition(|e| matches!(e, SessionEvent::System(sys) if sys.event == "compaction"))
}

/// Shared prefix of the compaction summary message text
/// (`"[Conversation Summary - {N} messages]:\n…"`). Both construction
/// sites use it — the live path (`compaction::compaction_top`) and the
/// resume path ([`compaction_summary_message`]) — and the engine's
/// compaction driver uses it to locate the installed summary message
/// when appending the ADR-051 archived-pages footer.
pub const COMPACTION_SUMMARY_PREFIX: &str = "[Conversation Summary";

/// Render the summary message for a compaction boundary event.
///
/// Reconstructs the same text the live compaction path produces
/// (`Compactor::compact`): the `"[Conversation Summary - {N} messages]:\n"`
/// prefix + summary, with `<read-files>` / `<modified-files>` blocks
/// and the fix-#6 `<user-messages>` block appended via
/// [`format_summary_with_file_ops`] when the persisted `details` blob
/// deserializes. Missing or malformed fields degrade gracefully (zero
/// defaults / summary-only body); pre-fix-#6 events (no
/// `details.user_messages`) render exactly as before.
///
/// ADR-051 D4: an `<archived-pages>` catalog footer is appended via
/// [`crate::pages::render_page_catalog`], enumerated over
/// `events[..=boundary_idx]` so the catalog for boundary k lists
/// exactly pages 1..=k (the page this boundary closes included). The
/// footer is derived, never stored; the live path appends the
/// identical text.
///
/// Returns `None` if `events[boundary_idx]` is not a compaction
/// boundary.
#[must_use]
pub fn compaction_summary_message(
    events: &[SessionEvent],
    boundary_idx: usize,
) -> Option<LlmMessage> {
    let event = events.get(boundary_idx)?;
    let SessionEvent::System(sys) = event else {
        return None;
    };
    if sys.event != "compaction" {
        return None;
    }

    let detail = &sys.detail;
    let summary = detail.get("summary").and_then(|v| v.as_str()).unwrap_or("");
    let messages_compacted = detail
        .get("messages_compacted")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let details = detail
        .get("details")
        .and_then(|v| serde_json::from_value::<CompactionDetails>(v.clone()).ok());

    // `format_summary_with_file_ops` trims the summary and appends the
    // file-ops blocks; without details the trimmed summary is the body.
    let mut body = match details {
        Some(d) => format_summary_with_file_ops(summary, &d),
        None => summary.trim().to_string(),
    };

    // ADR-051 D4: archived-pages catalog footer (derived, never
    // stored). Pages are enumerated over the events up to and
    // including this boundary, so a mid-history boundary never lists
    // pages closed by LATER compactions.
    let pages = crate::pages::list_pages(&events[..=boundary_idx]);
    if let Some(footer) = crate::pages::render_page_catalog(&pages) {
        body.push_str("\n\n");
        body.push_str(&footer);
    }

    let text = format!("{COMPACTION_SUMMARY_PREFIX} - {messages_compacted} messages]:\n{body}");

    let mut metadata = std::collections::HashMap::new();
    metadata.insert(
        COMPACTION_BOUNDARY_METADATA_KEY.to_string(),
        serde_json::Value::Bool(true),
    );
    Some(LlmMessage {
        role: MessageRole::System,
        content: vec![ContentBlock::Text { text }],
        timestamp: sys.envelope.ts,
        metadata,
        tool_call_id: None,
        usage: None,
    })
}

/// Convert a `NormalizedEntry` to an `LlmMessage`
///
/// Used by `build_context()` to reconstruct the LLM message list from
/// normalized session entries.
pub(crate) fn normalized_entry_to_llm_message(entry: &NormalizedEntry) -> Option<LlmMessage> {
    match entry {
        NormalizedEntry::UserMessage { content, .. } => Some(LlmMessage::user(content)),
        NormalizedEntry::AssistantMessage { content, .. } => Some(LlmMessage::assistant(content)),
        NormalizedEntry::SystemMessage { content, .. } => Some(LlmMessage::system(content)),
        NormalizedEntry::ToolResult {
            tool_call_id,
            tool_name,
            content,
            is_error,
        } => Some(LlmMessage {
            role: MessageRole::Tool,
            content: vec![ContentBlock::ToolResult {
                tool_call_id: tool_call_id.clone(),
                name: tool_name.clone(),
                content: vec![ContentBlock::Text {
                    text: content.clone(),
                }],
                is_error: *is_error,
            }],
            timestamp: chrono::Utc::now(),
            metadata: std::collections::HashMap::new(),
            tool_call_id: Some(tool_call_id.clone()),
            usage: None,
        }),
        // Session header, compaction, model change, custom — not chat messages
        _ => None,
    }
}

/// Convert a slice of `NormalizedEntry` to context text
///
/// This function extracts text content from normalized entries for LLM context.
pub fn entries_to_context_text(entries: &[NormalizedEntry]) -> String {
    let mut context = String::new();

    for entry in entries {
        match entry {
            NormalizedEntry::UserMessage { content, .. } => {
                if !content.is_empty() {
                    context.push_str(&format!("user: {content}\n\n"));
                }
            }
            NormalizedEntry::AssistantMessage { content, .. } => {
                if !content.is_empty() {
                    context.push_str(&format!("assistant: {content}\n\n"));
                }
            }
            NormalizedEntry::SystemMessage { content, .. } => {
                if !content.is_empty() {
                    context.push_str(&format!("system: {content}\n\n"));
                }
            }
            NormalizedEntry::ToolResult {
                content, tool_name, ..
            } => {
                context.push_str(&format!("tool: [{tool_name} result: {content}]\n\n"));
            }
            // Other entry types don't contribute to context text
            _ => {}
        }
    }

    context
}

// ====================================================================================
// Tests
// ====================================================================================

#[cfg(test)]
mod tests {
    use crate::events::{EventEnvelope, MessageSource, SessionCreatedEvent};
    use crate::*;
    use chrono::Utc;

    #[test]
    fn test_event_to_llm_message_assistant() {
        let event =
            SessionEvent::MessageV2(SessionMessage::assistant_text("Hello!", "openai", "gpt-4"));

        let msg = event_to_llm_message(&event).unwrap();
        assert_eq!(msg.role, MessageRole::Assistant);
        assert_eq!(msg.content.len(), 1);
    }

    #[test]
    fn test_event_to_llm_message_user() {
        let event = SessionEvent::MessageV2(SessionMessage::user("Hi there", MessageSource::User));

        let msg = event_to_llm_message(&event).unwrap();
        assert_eq!(msg.role, MessageRole::User);
        assert_eq!(msg.content.len(), 1);
    }

    #[test]
    fn test_event_to_llm_message_system() {
        let event = SessionEvent::MessageV2(SessionMessage::system("System prompt"));

        let msg = event_to_llm_message(&event).unwrap();
        assert_eq!(msg.role, MessageRole::System);
    }

    #[test]
    fn test_event_to_llm_message_unhandled() {
        let event = SessionEvent::SessionCreated(SessionCreatedEvent {
            instance_id: "instance-1".to_string(),
            image_digest: "sha256:abc".to_string(),
            parent_session_id: None,
            trigger: crate::events::SessionTrigger::User,
            envelope: EventEnvelope {
                id: "test-4".to_string(),
                ts: Utc::now(),
            },
        });

        // SessionCreated events should be ignored
        assert!(event_to_llm_message(&event).is_none());
    }

    #[test]
    fn test_entries_to_context_text() {
        let entries = vec![
            NormalizedEntry::UserMessage {
                id: "1".to_string(),
                content: "Hello".to_string(),
                timestamp: Utc::now(),
                source: MessageSource::User,
            },
            NormalizedEntry::AssistantMessage {
                id: "2".to_string(),
                content: "Hi there".to_string(),
                timestamp: Utc::now(),
                input_tokens: 10,
                output_tokens: 5,
            },
            NormalizedEntry::SystemMessage {
                content: "System info".to_string(),
                timestamp: Utc::now(),
            },
        ];

        let context = entries_to_context_text(&entries);
        assert!(context.contains("user: Hello"));
        assert!(context.contains("assistant: Hi there"));
        assert!(context.contains("system: System info"));
    }

    #[test]
    fn test_entries_to_context_text_with_tool_result() {
        let entries = vec![NormalizedEntry::ToolResult {
            tool_call_id: "1".to_string(),
            tool_name: "Read".to_string(),
            content: "File contents".to_string(),
            is_error: false,
        }];

        let context = entries_to_context_text(&entries);
        assert!(context.contains("tool: [Read result: File contents]"));
    }

    #[test]
    fn test_entries_to_context_text_empty_content_skipped() {
        let entries = vec![NormalizedEntry::UserMessage {
            id: "1".to_string(),
            content: String::new(),
            timestamp: Utc::now(),
            source: MessageSource::User,
        }];

        let context = entries_to_context_text(&entries);
        assert!(context.is_empty());
    }

    // ==================================================================
    // Compaction-boundary resume helpers
    // ==================================================================

    use crate::events::SystemEvent;

    fn compaction_event(detail: serde_json::Value) -> SessionEvent {
        SessionEvent::System(SystemEvent {
            envelope: EventEnvelope {
                id: "compact_test".to_string(),
                ts: Utc::now(),
            },
            event: "compaction".to_string(),
            detail,
        })
    }

    #[test]
    fn test_latest_compaction_boundary_finds_newest() {
        let events = vec![
            SessionEvent::MessageV2(SessionMessage::user("a", MessageSource::User)),
            compaction_event(serde_json::json!({"summary": "first", "messages_compacted": 1})),
            SessionEvent::MessageV2(SessionMessage::user("b", MessageSource::User)),
            compaction_event(serde_json::json!({"summary": "second", "messages_compacted": 2})),
            SessionEvent::MessageV2(SessionMessage::user("c", MessageSource::User)),
        ];

        let idx = latest_compaction_boundary(&events).unwrap();
        assert_eq!(idx, 3, "newest compaction boundary wins");
    }

    #[test]
    fn test_latest_compaction_boundary_none_without_compaction() {
        let events = vec![
            SessionEvent::MessageV2(SessionMessage::user("a", MessageSource::User)),
            SessionEvent::System(SystemEvent {
                envelope: EventEnvelope {
                    id: "evt".to_string(),
                    ts: Utc::now(),
                },
                event: "model_change".to_string(),
                detail: serde_json::json!({}),
            }),
        ];
        assert!(latest_compaction_boundary(&events).is_none());
    }

    #[test]
    fn test_compaction_summary_message_renders_file_ops() {
        let events = vec![compaction_event(serde_json::json!({
            "summary": "Did the thing",
            "messages_compacted": 7,
            "details": {
                "read_files": ["a.rs"],
                "modified_files": ["b.rs"],
            },
        }))];

        let msg = compaction_summary_message(&events, 0).unwrap();
        assert_eq!(msg.role, MessageRole::System);
        assert!(is_compaction_boundary_message(&msg));
        let text = match &msg.content[0] {
            ContentBlock::Text { text } => text.as_str(),
            other => panic!("expected text block, got {other:?}"),
        };
        assert!(text.starts_with("[Conversation Summary - 7 messages]:\n"));
        assert!(text.contains("Did the thing"));
        assert!(text.contains("<read-files>\na.rs\n</read-files>"));
        assert!(text.contains("<modified-files>\nb.rs\n</modified-files>"));
    }

    #[test]
    fn test_compaction_summary_message_degrades_on_missing_fields() {
        let events = vec![compaction_event(serde_json::json!({}))];

        let msg = compaction_summary_message(&events, 0).unwrap();
        let text = match &msg.content[0] {
            ContentBlock::Text { text } => text.as_str(),
            other => panic!("expected text block, got {other:?}"),
        };
        assert!(
            text.starts_with("[Conversation Summary - 0 messages]:\n"),
            "missing fields fall back to defaults, got: {text}"
        );
    }

    /// Compaction audit fix #6: a persisted `details.user_messages`
    /// array re-renders as the same `<user-messages>` block the live
    /// compaction path produces (both go through
    /// `format_summary_with_file_ops`).
    #[test]
    fn test_compaction_summary_message_renders_user_messages() {
        let events = vec![compaction_event(serde_json::json!({
            "summary": "Did the thing",
            "messages_compacted": 7,
            "details": {
                "read_files": [],
                "modified_files": [],
                "user_messages": ["first ask", "second correction"],
            },
        }))];

        let msg = compaction_summary_message(&events, 0).unwrap();
        let text = match &msg.content[0] {
            ContentBlock::Text { text } => text.as_str(),
            other => panic!("expected text block, got {other:?}"),
        };
        assert!(text.contains("<user-messages>"));
        assert!(text.contains("<message>\nfirst ask\n</message>"));
        assert!(text.contains("<message>\nsecond correction\n</message>"));
    }

    /// Fix #6 backward compat: pre-fix-#6 compaction events (no
    /// `user_messages` field in `details`) render without the block —
    /// byte-identical to the pre-fix-#6 reconstruction. The ADR-051
    /// `<archived-pages>` footer is appended to ALL reconstructions
    /// (pre- and post-ADR-051 sessions alike); it is pinned here via
    /// the shared `render_page_catalog` helper so this test also locks
    /// the live/resume identity property.
    #[test]
    fn test_compaction_summary_message_legacy_event_has_no_user_block() {
        let events = vec![compaction_event(serde_json::json!({
            "summary": "Did the thing",
            "messages_compacted": 7,
            "details": {
                "read_files": ["a.rs"],
                "modified_files": [],
            },
        }))];

        let msg = compaction_summary_message(&events, 0).unwrap();
        let text = match &msg.content[0] {
            ContentBlock::Text { text } => text.as_str(),
            other => panic!("expected text block, got {other:?}"),
        };
        let footer = crate::pages::render_page_catalog(&crate::pages::list_pages(&events)).unwrap();
        assert_eq!(
            text,
            format!(
                "[Conversation Summary - 7 messages]:\nDid the thing\n\n<read-files>\na.rs\n</read-files>\n\n{footer}"
            )
        );
        assert!(!text.contains("<user-messages>"));
    }

    /// ADR-051 D4: the reconstructed summary lists the archived pages
    /// — including the page this boundary just closed — and excludes
    /// anything after it.
    #[test]
    fn test_compaction_summary_message_appends_archived_pages_footer() {
        let events = vec![
            SessionEvent::MessageV2(SessionMessage::user("first ask", MessageSource::User)),
            compaction_event(serde_json::json!({
                "summary": "first summary",
                "messages_compacted": 1,
                "compaction_number": 1,
            })),
            SessionEvent::MessageV2(SessionMessage::user("second ask", MessageSource::User)),
            compaction_event(serde_json::json!({
                "summary": "second summary",
                "messages_compacted": 2,
                "compaction_number": 2,
            })),
            SessionEvent::MessageV2(SessionMessage::user("live ask", MessageSource::User)),
        ];

        // The newest boundary's summary lists pages 1 and 2.
        let msg = compaction_summary_message(&events, 3).unwrap();
        let text = match &msg.content[0] {
            ContentBlock::Text { text } => text.as_str(),
            other => panic!("expected text block, got {other:?}"),
        };
        assert!(text.starts_with("[Conversation Summary - 2 messages]:\nsecond summary"));
        assert!(text.contains("<archived-pages>"), "{text}");
        assert!(
            text.contains("This session has 2 archived page(s)"),
            "{text}"
        );
        assert!(text.contains("- page 1 (compaction #1, ~"), "{text}");
        assert!(text.contains(": \"first ask\""), "{text}");
        assert!(text.contains("- page 2 (compaction #2, ~"), "{text}");
        assert!(text.contains(": \"second ask\""), "{text}");
        assert!(!text.contains("live ask"), "live page is not archived");

        // An older boundary reconstructs with ONLY the pages it had
        // closed at the time — later compactions are invisible to it.
        let msg = compaction_summary_message(&events, 1).unwrap();
        let text = match &msg.content[0] {
            ContentBlock::Text { text } => text.as_str(),
            other => panic!("expected text block, got {other:?}"),
        };
        assert!(
            text.contains("This session has 1 archived page(s)"),
            "{text}"
        );
        assert!(!text.contains("page 2"), "{text}");
    }

    #[test]
    fn test_compaction_summary_message_ignores_non_compaction_events() {
        let msg_event = SessionEvent::MessageV2(SessionMessage::user("hi", MessageSource::User));
        assert!(compaction_summary_message(&[msg_event], 0).is_none());

        let other_system = SessionEvent::System(SystemEvent {
            envelope: EventEnvelope {
                id: "evt".to_string(),
                ts: Utc::now(),
            },
            event: "cwd".to_string(),
            detail: serde_json::json!({"path": "/tmp"}),
        });
        assert!(compaction_summary_message(&[other_system], 0).is_none());

        // Out-of-range boundary index is a clean `None`.
        assert!(compaction_summary_message(&[], 0).is_none());
    }
}
