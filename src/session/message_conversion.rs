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

use crate::common::types::message::ContentBlock;
use crate::common::types::message::{LlmMessage, MessageRole};
use crate::session::events::SessionEvent;
#[cfg(test)]
use crate::session::events::SessionMessage;
use crate::session::NormalizedEntry;

/// Convert a `SessionEvent` to an `LlmMessage`
///
/// This function handles the conversion from internal event format to
/// provider-agnostic `LlmMessage` format.
///
/// Uses the unified `as_message()` method to support both the new `MessageV2`
/// format and all legacy formats seamlessly.
pub(crate) fn event_to_llm_message(event: &SessionEvent) -> Option<LlmMessage> {
    // Use unified conversion for all message types (handles MessageV2 and legacy)
    if let Some(msg) = event.as_message() {
        return Some(msg.to_llm_message());
    }

    // Non-message events return None
    None
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
pub(crate) fn entries_to_context_text(entries: &[NormalizedEntry]) -> String {
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
    use super::*;
    use crate::session::events::{EventEnvelope, MessageSource, SessionCreatedEvent};
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
            trigger: crate::session::events::SessionTrigger::User,
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
}
