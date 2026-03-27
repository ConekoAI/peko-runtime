//! Unified session message - wraps types::message::LlmMessage with session context
//!
//! This module provides a single, unified message type that replaces:
//! - UserMessageEvent
//! - AssistantMessageEvent
//! - SystemMessageEvent
//! - MessageEvent (legacy unified)
//! - LlmMessageEvent (legacy LLM-native)
//!
//! The new `SessionMessage` type uses SRP-compliant `RoleMetadata` to separate
//! role-specific concerns while reusing the existing `LlmMessage` from types::message.

use crate::session::events::EventEnvelope;
use crate::types::message::{ContentBlock, LlmMessage, MessageRole};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Source of a user message
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageSource {
    /// Typed by human
    User,
    /// Injected by hook trigger
    Hook,
    /// Sent via event bus (A2A)
    A2a,
    /// From spawning parent
    SpawnParent,
}

impl Default for MessageSource {
    fn default() -> Self {
        MessageSource::User
    }
}

/// Token usage statistics
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub total_tokens: u32,
}

/// Role-specific metadata - SRP-compliant separation of concerns
///
/// Each role has exactly the metadata it needs. This enum is flattened
/// into SessionMessage serialization with the "role" tag.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum RoleMetadata {
    /// User message metadata
    User {
        source: MessageSource,
    },
    /// Assistant message metadata
    Assistant {
        provider: String,
        model: String,
        usage: TokenUsage,
    },
    /// System message metadata (none needed)
    System,
    /// Tool result metadata
    Tool {
        tool_call_id: String,
    },
}

impl RoleMetadata {
    /// Get the message role for this metadata
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
/// This replaces: UserMessageEvent, AssistantMessageEvent, SystemMessageEvent,
/// MessageEvent, LlmMessageEvent
///
/// Uses SRP-compliant RoleMetadata to separate role-specific concerns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMessage {
    #[serde(flatten)]
    pub envelope: EventEnvelope,

    /// The core message content (from types::message)
    #[serde(flatten)]
    pub message: LlmMessage,

    /// Message ID (unique within session)
    pub message_id: String,

    /// Role-specific metadata (SRP-compliant)
    #[serde(flatten)]
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
                    input_tokens: 0,
                    output_tokens: 0,
                    total_tokens: 0,
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
    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        let tool_call_id_str = tool_call_id.into();
        Self {
            envelope: EventEnvelope::new(),
            message: LlmMessage {
                role: MessageRole::Tool,
                content: vec![ContentBlock::ToolResult {
                    tool_call_id: tool_call_id_str.clone(),
                    name: String::new(), // Tool name not stored at message level
                    content: vec![ContentBlock::Text { text: content.into() }],
                    is_error: false,
                }],
                timestamp: Utc::now(),
                metadata: HashMap::new(),
            },
            message_id: generate_message_id(),
            role_metadata: RoleMetadata::Tool {
                tool_call_id: tool_call_id_str,
            },
        }
    }

    /// Get the message role
    pub fn role(&self) -> MessageRole {
        self.message.role
    }

    /// Get text content (convenience)
    pub fn text_content(&self) -> String {
        self.message
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Get message source (if user message)
    pub fn source(&self) -> Option<MessageSource> {
        match &self.role_metadata {
            RoleMetadata::User { source } => Some(*source),
            _ => None,
        }
    }

    /// Get provider (if assistant message)
    pub fn provider(&self) -> Option<&str> {
        match &self.role_metadata {
            RoleMetadata::Assistant { provider, .. } => Some(provider),
            _ => None,
        }
    }

    /// Get model (if assistant message)
    pub fn model(&self) -> Option<&str> {
        match &self.role_metadata {
            RoleMetadata::Assistant { model, .. } => Some(model),
            _ => None,
        }
    }

    /// Get token usage (if assistant message)
    pub fn usage(&self) -> Option<&TokenUsage> {
        match &self.role_metadata {
            RoleMetadata::Assistant { usage, .. } => Some(usage),
            _ => None,
        }
    }

    /// Get tool call ID (if tool message)
    pub fn tool_call_id(&self) -> Option<&str> {
        match &self.role_metadata {
            RoleMetadata::Tool { tool_call_id } => Some(tool_call_id),
            _ => None,
        }
    }

    /// Convert to ChatMessage for provider APIs
    pub fn to_chat_message(&self) -> crate::providers::ChatMessage {
        use crate::providers::MessageRole as ProviderRole;
        let role = match self.message.role {
            MessageRole::System => ProviderRole::System,
            MessageRole::User => ProviderRole::User,
            MessageRole::Assistant => ProviderRole::Assistant,
            MessageRole::Tool => ProviderRole::Tool,
        };
        crate::providers::ChatMessage {
            role,
            content: self.message.content.clone(),
            tool_calls: None, // Extract from content blocks if needed
            tool_call_id: self.tool_call_id().map(|s| s.to_string()),
        }
    }
}

/// Generate a new message ID
fn generate_message_id() -> String {
    format!("msg_{}", uuid::Uuid::new_v4().to_string().replace('-', ""))
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
        assert!(msg.provider().is_none());
    }

    #[test]
    fn test_session_message_assistant() {
        let usage = TokenUsage {
            input_tokens: 10,
            output_tokens: 5,
            total_tokens: 15,
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
        assert_eq!(msg.usage().map(|u| u.total_tokens), Some(15));
    }

    #[test]
    fn test_session_message_system() {
        let msg = SessionMessage::system("You are a helpful assistant");
        assert_eq!(msg.role(), MessageRole::System);
        assert_eq!(msg.text_content(), "You are a helpful assistant");
    }

    #[test]
    fn test_session_message_tool_result() {
        let msg = SessionMessage::tool_result("tc_123", "File contents here");
        assert_eq!(msg.role(), MessageRole::Tool);
        assert_eq!(msg.tool_call_id(), Some("tc_123"));
    }

    #[test]
    fn test_serialization_roundtrip() {
        let msg = SessionMessage::user("Test message", MessageSource::Hook);
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: SessionMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg.text_content(), deserialized.text_content());
        assert_eq!(msg.role(), deserialized.role());
    }

    #[test]
    fn test_role_metadata_serialization() {
        let metadata = RoleMetadata::Assistant {
            provider: "anthropic".to_string(),
            model: "claude-3".to_string(),
            usage: TokenUsage {
                input_tokens: 100,
                output_tokens: 50,
                total_tokens: 150,
            },
        };
        let json = serde_json::to_string(&metadata).unwrap();
        assert!(json.contains("anthropic"));
        assert!(json.contains("claude-3"));
        assert!(json.contains("assistant")); // The role tag
    }
}
