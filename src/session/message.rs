//! Unified session message - wraps `types::message::LlmMessage` with session context
//!
//! This module provides a single, unified message type that replaces:
//! - `UserMessageEvent`
//! - `AssistantMessageEvent`
//! - `SystemMessageEvent`
//! - `MessageEvent` (legacy unified)
//! - `LlmMessageEvent` (legacy LLM-native)
//!
//! The new `SessionMessage` type uses SRP-compliant `RoleMetadata` to separate
//! role-specific concerns while reusing the existing `LlmMessage` from `types::message`.

use crate::session::events::EventEnvelope;
use crate::types::message::{ContentBlock, LlmMessage, MessageRole, TokenUsage};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Source of a user message
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum MessageSource {
    /// Typed by human
    #[default]
    User,
    /// Injected by hook trigger
    Hook,
    /// Sent via event bus (A2A)
    A2a,
    /// From spawning parent
    SpawnParent,
}

/// Role-specific metadata - SRP-compliant separation of concerns
///
/// This enum is stored alongside the message content but does NOT include
/// the role field (which comes from LlmMessage.role to avoid duplication).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RoleMetadata {
    /// User message metadata
    User { source: MessageSource },
    /// Assistant message metadata
    Assistant {
        provider: String,
        model: String,
        usage: TokenUsage,
    },
    /// System message metadata (none needed)
    System,
    /// Tool result metadata
    Tool { tool_call_id: String },
}

impl RoleMetadata {
    /// Get the message role for this metadata
    #[must_use]
    pub fn role(&self) -> MessageRole {
        match self {
            RoleMetadata::User { .. } => MessageRole::User,
            RoleMetadata::Assistant { .. } => MessageRole::Assistant,
            RoleMetadata::System => MessageRole::System,
            RoleMetadata::Tool { .. } => MessageRole::Tool,
        }
    }
}

/// Unified message event for session storage
///
/// This replaces: `UserMessageEvent`, `AssistantMessageEvent`, `SystemMessageEvent`,
/// `MessageEvent`, `LlmMessageEvent`
///
/// Uses SRP-compliant `RoleMetadata` to separate role-specific concerns.
/// Note: The role field is stored in `message.role` to avoid duplication.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMessage {
    #[serde(flatten)]
    pub envelope: EventEnvelope,

    /// Message ID (unique within session)
    pub message_id: String,

    /// The core message content (role, content, timestamp, metadata)
    /// Note: role is stored here to avoid duplication with `RoleMetadata`
    #[serde(flatten)]
    pub message: LlmMessage,

    /// Role-specific metadata (without the role field to avoid duplication)
    pub role_metadata: RoleMetadata,
}

impl SessionMessage {
    /// Create a user message
    pub fn user(content: impl Into<String>, source: MessageSource) -> Self {
        Self {
            envelope: EventEnvelope::new(),
            message: LlmMessage::user(content),
            message_id: generate_message_id(),
            role_metadata: RoleMetadata::User { source },
        }
    }

    /// Create an assistant message with content blocks
    pub fn assistant_with_blocks(
        content: Vec<ContentBlock>,
        provider: impl Into<String>,
        model: impl Into<String>,
        usage: TokenUsage,
    ) -> Self {
        Self {
            envelope: EventEnvelope::new(),
            message: LlmMessage {
                role: MessageRole::Assistant,
                content,
                timestamp: Utc::now(),
                metadata: HashMap::new(),
                tool_call_id: None,
            },
            message_id: generate_message_id(),
            role_metadata: RoleMetadata::Assistant {
                provider: provider.into(),
                model: model.into(),
                usage,
            },
        }
    }

    /// Create an assistant message with simple text content (convenience method)
    pub fn assistant_text(
        content: impl Into<String>,
        provider: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            envelope: EventEnvelope::new(),
            message: LlmMessage::assistant(content),
            message_id: generate_message_id(),
            role_metadata: RoleMetadata::Assistant {
                provider: provider.into(),
                model: model.into(),
                usage: TokenUsage {
                    input: 0,
                    output: 0,
                    total: 0,
                },
            },
        }
    }

    /// Create a system message
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            envelope: EventEnvelope::new(),
            message: LlmMessage::system(content),
            message_id: generate_message_id(),
            role_metadata: RoleMetadata::System,
        }
    }

    /// Create a tool result message
    pub fn tool_result(
        tool_call_id: impl Into<String>,
        tool_name: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        let tool_call_id_str = tool_call_id.into();
        let tool_name_str = tool_name.into();
        Self {
            envelope: EventEnvelope::new(),
            message: LlmMessage {
                role: MessageRole::Tool,
                content: vec![ContentBlock::ToolResult {
                    tool_call_id: tool_call_id_str.clone(),
                    name: tool_name_str,
                    content: vec![ContentBlock::Text {
                        text: content.into(),
                    }],
                    is_error: false,
                }],
                timestamp: Utc::now(),
                metadata: HashMap::new(),
                tool_call_id: Some(tool_call_id_str.clone()),
            },
            message_id: generate_message_id(),
            role_metadata: RoleMetadata::Tool {
                tool_call_id: tool_call_id_str,
            },
        }
    }

    /// Get the message role
    #[must_use]
    pub fn role(&self) -> MessageRole {
        self.message.role
    }

    /// Get text content (convenience)
    #[must_use]
    pub fn text_content(&self) -> String {
        self.message
            .content
            .iter()
            .flat_map(|b| match b {
                ContentBlock::Text { text } => vec![text.as_str()],
                ContentBlock::ToolResult { content, .. } => content
                    .iter()
                    .filter_map(|c| match c {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect(),
                _ => vec![],
            })
            .collect()
    }

    /// Get message source (if user message)
    #[must_use]
    pub fn source(&self) -> Option<MessageSource> {
        match &self.role_metadata {
            RoleMetadata::User { source } => Some(*source),
            _ => None,
        }
    }

    /// Get provider (if assistant message)
    #[must_use]
    pub fn provider(&self) -> Option<&str> {
        match &self.role_metadata {
            RoleMetadata::Assistant { provider, .. } => Some(provider),
            _ => None,
        }
    }

    /// Get model (if assistant message)
    #[must_use]
    pub fn model(&self) -> Option<&str> {
        match &self.role_metadata {
            RoleMetadata::Assistant { model, .. } => Some(model),
            _ => None,
        }
    }

    /// Get token usage (if assistant message)
    #[must_use]
    pub fn usage(&self) -> Option<&TokenUsage> {
        match &self.role_metadata {
            RoleMetadata::Assistant { usage, .. } => Some(usage),
            _ => None,
        }
    }

    /// Get tool call ID (if tool message)
    #[must_use]
    pub fn tool_call_id(&self) -> Option<&str> {
        match &self.role_metadata {
            RoleMetadata::Tool { tool_call_id } => Some(tool_call_id),
            _ => None,
        }
    }

    /// Convert to `LlmMessage` for provider API
    #[must_use]
    pub fn to_llm_message(&self) -> LlmMessage {
        let mut msg = self.message.clone();
        // Ensure tool_call_id is populated from role_metadata for tool messages
        if let RoleMetadata::Tool { tool_call_id } = &self.role_metadata {
            msg.tool_call_id = Some(tool_call_id.clone());
        }
        msg
    }

    /// Deprecated: use `to_llm_message()` instead
    #[must_use]
    pub fn to_chat_message(&self) -> LlmMessage {
        self.to_llm_message()
    }
}

/// Generate a unique message ID
fn generate_message_id() -> String {
    format!("msg_{}", uuid::Uuid::new_v4().simple())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_message_user() {
        let msg = SessionMessage::user("Hello", MessageSource::User);
        assert_eq!(msg.role(), MessageRole::User);
        assert_eq!(msg.text_content(), "Hello");
        assert_eq!(msg.source(), Some(MessageSource::User));
    }

    #[test]
    fn test_session_message_assistant() {
        let usage = TokenUsage {
            input: 10,
            output: 5,
            total: 15,
        };
        let msg = SessionMessage::assistant_with_blocks(
            vec![ContentBlock::Text {
                text: "Hi there".to_string(),
            }],
            "openai",
            "gpt-4",
            usage,
        );
        assert_eq!(msg.role(), MessageRole::Assistant);
        assert_eq!(msg.text_content(), "Hi there");
        assert_eq!(msg.provider(), Some("openai"));
        assert_eq!(msg.model(), Some("gpt-4"));
        assert_eq!(msg.usage().map(|u| u.total), Some(15));
    }

    #[test]
    fn test_session_message_system() {
        let msg = SessionMessage::system("You are a helpful assistant");
        assert_eq!(msg.role(), MessageRole::System);
        assert_eq!(msg.text_content(), "You are a helpful assistant");
    }

    #[test]
    fn test_session_message_tool_result() {
        let msg = SessionMessage::tool_result("call_123", "test_tool", "Result data");
        assert_eq!(msg.role(), MessageRole::Tool);
        assert_eq!(msg.tool_call_id(), Some("call_123"));
        assert_eq!(msg.text_content(), "Result data");
    }

    #[test]
    fn test_session_message_to_llm_message() {
        let msg = SessionMessage::user("Hello", MessageSource::User);
        let chat = msg.to_llm_message();
        assert_eq!(chat.role, MessageRole::User);
        assert_eq!(chat.content.len(), 1);
    }

    #[test]
    fn test_session_message_serde_roundtrip() {
        let msg = SessionMessage::user("Hello", MessageSource::User);
        let json = serde_json::to_string(&msg).unwrap();
        println!("User message JSON: {json}");
        let deserialized: SessionMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.role(), MessageRole::User);
        assert_eq!(deserialized.text_content(), "Hello");
    }

    #[test]
    fn test_assistant_message_json_format() {
        let msg = SessionMessage::assistant_text("Hi there", "openai", "gpt-4");
        let json = serde_json::to_string_pretty(&msg).unwrap();
        println!("Assistant message JSON:\n{json}");

        // Verify no duplicate "role" fields
        let role_count = json.matches("\"role\":").count();
        println!("Number of 'role' fields: {role_count}");
        assert_eq!(role_count, 1, "Should have exactly one 'role' field");

        // Verify it can be deserialized
        let deserialized: SessionMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.role(), MessageRole::Assistant);
    }

    #[test]
    fn test_session_event_message_v2_format() {
        use crate::session::events::SessionEvent;

        // Test user message
        let msg = SessionMessage::user("Hello", MessageSource::User);
        let event = SessionEvent::MessageV2(msg);
        let json = serde_json::to_string_pretty(&event).unwrap();
        println!("User SessionEvent::MessageV2 JSON:\n{json}");

        // Verify no duplicate "role" fields
        let role_count = json.matches("\"role\":").count();
        println!("Number of 'role' fields: {role_count}");
        assert_eq!(role_count, 1, "Should have exactly one 'role' field");

        // Verify it can be deserialized
        let deserialized: SessionEvent = serde_json::from_str(&json).unwrap();
        match deserialized {
            SessionEvent::MessageV2(m) => {
                assert_eq!(m.role(), MessageRole::User);
                assert_eq!(m.text_content(), "Hello");
            }
            _ => panic!("Expected MessageV2"),
        }

        // Test assistant message
        let msg = SessionMessage::assistant_text("Hi", "openai", "gpt-4");
        let event = SessionEvent::MessageV2(msg);
        let json = serde_json::to_string(&event).unwrap();
        let role_count = json.matches("\"role\":").count();
        assert_eq!(
            role_count, 1,
            "Assistant should have exactly one 'role' field"
        );

        // Test system message
        let msg = SessionMessage::system("You are helpful");
        let event = SessionEvent::MessageV2(msg);
        let json = serde_json::to_string(&event).unwrap();
        let role_count = json.matches("\"role\":").count();
        assert_eq!(role_count, 1, "System should have exactly one 'role' field");

        // Test tool message
        let msg = SessionMessage::tool_result("call_123", "test_tool", "Result");
        let event = SessionEvent::MessageV2(msg);
        let json = serde_json::to_string(&event).unwrap();
        let role_count = json.matches("\"role\":").count();
        assert_eq!(role_count, 1, "Tool should have exactly one 'role' field");
    }

    #[test]
    fn test_assistant_message_serde_roundtrip() {
        let msg = SessionMessage::assistant_text("Hi there", "openai", "gpt-4");
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: SessionMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.role(), MessageRole::Assistant);
        assert_eq!(deserialized.text_content(), "Hi there");
        assert_eq!(deserialized.provider(), Some("openai"));
    }
}
