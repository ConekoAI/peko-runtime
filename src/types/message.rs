//! Agent message types - abstraction layer for LLM and custom messages
//!
//! This module provides a unified message type system that supports:
//! - Standard LLM messages (system, user, assistant, tool)
//! - Custom application-specific messages (notifications, status, etc.)
//! - Context transformation hooks for context window management
//! - Conversion to LLM-compatible format

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// Unique identifier for messages
pub type MessageId = String;

/// Unique identifier for tool calls
pub type ToolCallId = String;

/// Content block types for messages
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    /// Plain text content
    Text { text: String },

    /// Image content (base64 or URL)
    Image { source: ImageSource, mime_type: String },

    /// Tool call request
    ToolCall {
        id: ToolCallId,
        name: String,
        arguments: Value,
    },

    /// Tool execution result
    ToolResult {
        tool_call_id: ToolCallId,
        name: String,
        content: Vec<ContentBlock>,
        is_error: bool,
    },

    /// Thinking/reasoning block
    Thinking { text: String, signature: Option<String> },
}

/// Image source for image content blocks
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "source_type", rename_all = "snake_case")]
pub enum ImageSource {
    /// Base64-encoded image data
    Base64 { data: String },
    /// URL to image
    Url { url: String },
}

/// Standard LLM message roles
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

/// Standard LLM message
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmMessage {
    pub role: MessageRole,
    pub content: Vec<ContentBlock>,
    pub timestamp: DateTime<Utc>,
    pub metadata: HashMap<String, Value>,
}

impl LlmMessage {
    /// Create a simple text message
    pub fn text(role: MessageRole, text: impl Into<String>) -> Self {
        Self {
            role,
            content: vec![ContentBlock::Text { text: text.into() }],
            timestamp: Utc::now(),
            metadata: HashMap::new(),
        }
    }

    /// Create a system message
    pub fn system(text: impl Into<String>) -> Self {
        Self::text(MessageRole::System, text)
    }

    /// Create a user message
    pub fn user(text: impl Into<String>) -> Self {
        Self::text(MessageRole::User, text)
    }

    /// Create an assistant message
    pub fn assistant(text: impl Into<String>) -> Self {
        Self::text(MessageRole::Assistant, text)
    }

    /// Create a tool result message
    pub fn tool_result(tool_call_id: impl Into<String>, name: impl Into<String>, result: impl Into<String>) -> Self {
        Self {
            role: MessageRole::Tool,
            content: vec![ContentBlock::ToolResult {
                tool_call_id: tool_call_id.into(),
                name: name.into(),
                content: vec![ContentBlock::Text { text: result.into() }],
                is_error: false,
            }],
            timestamp: Utc::now(),
            metadata: HashMap::new(),
        }
    }

    /// Add metadata to the message
    pub fn with_metadata(mut self, key: impl Into<String>, value: Value) -> Self {
        self.metadata.insert(key.into(), value);
        self
    }
}

/// Custom message types for application-specific needs
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "custom_type", rename_all = "snake_case")]
pub enum CustomMessage {
    /// UI notification (not sent to LLM)
    Notification {
        level: NotificationLevel,
        title: String,
        body: String,
    },

    /// Status update (not sent to LLM)
    Status {
        operation: String,
        status: String,
        progress_percent: Option<u8>,
    },

    /// Steering message - user input injected mid-execution
    Steering { text: String },

    /// Follow-up message for continuing conversation
    FollowUp { text: String },
}

/// Notification severity levels
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NotificationLevel {
    Info,
    Warning,
    Error,
    Success,
}

/// Unified agent message type
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "message_type", rename_all = "snake_case")]
pub enum AgentMessage {
    /// Standard LLM message
    Llm(LlmMessage),

    /// Custom application message
    Custom(CustomMessage),
}

impl AgentMessage {
    /// Create a system message
    pub fn system(text: impl Into<String>) -> Self {
        Self::Llm(LlmMessage::system(text))
    }

    /// Create a user message
    pub fn user(text: impl Into<String>) -> Self {
        Self::Llm(LlmMessage::user(text))
    }

    /// Create an assistant message
    pub fn assistant(text: impl Into<String>) -> Self {
        Self::Llm(LlmMessage::assistant(text))
    }

    /// Create a tool result message
    pub fn tool_result(tool_call_id: impl Into<String>, name: impl Into<String>, result: impl Into<String>) -> Self {
        Self::Llm(LlmMessage::tool_result(tool_call_id, name, result))
    }

    /// Create a notification message
    pub fn notification(level: NotificationLevel, title: impl Into<String>, body: impl Into<String>) -> Self {
        Self::Custom(CustomMessage::Notification {
            level,
            title: title.into(),
            body: body.into(),
        })
    }

    /// Create a steering message
    pub fn steering(text: impl Into<String>) -> Self {
        Self::Custom(CustomMessage::Steering { text: text.into() })
    }

    /// Create a follow-up message
    pub fn follow_up(text: impl Into<String>) -> Self {
        Self::Custom(CustomMessage::FollowUp { text: text.into() })
    }

    /// Check if this message should be sent to the LLM
    pub fn is_llm_visible(&self) -> bool {
        match self {
            Self::Llm(_) => true,
            Self::Custom(custom) => matches!(custom, CustomMessage::Steering { .. } | CustomMessage::FollowUp { .. }),
        }
    }

    /// Get timestamp if available
    pub fn timestamp(&self) -> Option<DateTime<Utc>> {
        match self {
            Self::Llm(msg) => Some(msg.timestamp),
            Self::Custom(_) => None,
        }
    }

    /// Convert to a simple text representation
    pub fn to_text(&self) -> String {
        match self {
            Self::Llm(msg) => {
                let mut texts: Vec<String> = msg
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text { text } => Some(text.clone()),
                        _ => None,
                    })
                    .collect();
                texts.join(" ")
            }
            Self::Custom(custom) => match custom {
                CustomMessage::Notification { body, .. } => body.clone(),
                CustomMessage::Status { status, .. } => status.clone(),
                CustomMessage::Steering { text } => text.clone(),
                CustomMessage::FollowUp { text } => text.clone(),
            },
        }
    }
}

impl Default for AgentMessage {
    fn default() -> Self {
        Self::user("")
    }
}

/// Message converter trait for transforming AgentMessage to LLM format
#[async_trait::async_trait]
pub trait MessageConverter: Send + Sync {
    /// Convert AgentMessage to provider-specific format
    async fn convert(&self, messages: Vec<AgentMessage>) -> anyhow::Result<Vec<Value>>;
}

/// Simple JSON converter for backward compatibility
pub struct JsonMessageConverter;

#[async_trait::async_trait]
impl MessageConverter for JsonMessageConverter {
    async fn convert(&self, messages: Vec<AgentMessage>) -> anyhow::Result<Vec<Value>> {
        let mut result = Vec::new();

        for msg in messages {
            if let AgentMessage::Llm(llm_msg) = msg {
                let content = match llm_msg.content.len() {
                    0 => Value::String("".to_string()),
                    1 => {
                        if let ContentBlock::Text { text } = &llm_msg.content[0] {
                            Value::String(text.clone())
                        } else {
                            serde_json::to_value(&llm_msg.content)?
                        }
                    }
                    _ => serde_json::to_value(&llm_msg.content)?,
                };

                result.push(serde_json::json!({
                    "role": match llm_msg.role {
                        MessageRole::System => "system",
                        MessageRole::User => "user",
                        MessageRole::Assistant => "assistant",
                        MessageRole::Tool => "tool",
                    },
                    "content": content,
                }));
            }
            // Custom messages are filtered out by default
        }

        Ok(result)
    }
}

/// Context for agent execution with message history
#[derive(Debug, Clone, Default)]
pub struct AgentContext {
    /// All messages in the conversation (including custom)
    pub messages: Vec<AgentMessage>,

    /// System prompt
    pub system_prompt: String,

    /// Context-level metadata
    pub metadata: HashMap<String, Value>,
}

impl AgentContext {
    /// Create a new context with a system prompt
    pub fn with_system_prompt(prompt: impl Into<String>) -> Self {
        let prompt_str = prompt.into();
        Self {
            messages: vec![AgentMessage::system(prompt_str.clone())],
            system_prompt: prompt_str,
            metadata: HashMap::new(),
        }
    }

    /// Add a message to the context
    pub fn add_message(&mut self, message: AgentMessage) -> &mut Self {
        self.messages.push(message);
        self
    }

    /// Get only LLM-visible messages
    pub fn llm_messages(&self) -> Vec<AgentMessage> {
        self.messages.iter().filter(|m| m.is_llm_visible()).cloned().collect()
    }

    /// Get messages for a specific role
    pub fn messages_by_role(&self, role: MessageRole) -> Vec<&LlmMessage> {
        self.messages
            .iter()
            .filter_map(|m| match m {
                AgentMessage::Llm(msg) if msg.role == role => Some(msg),
                _ => None,
            })
            .collect()
    }

    /// Estimate token count (rough approximation)
    pub fn estimate_tokens(&self) -> usize {
        self.messages.iter().map(|m| m.to_text().len() / 4).sum()
    }

    /// Clear messages except system prompt
    pub fn clear_conversation(&mut self) {
        self.messages.retain(|m| matches!(m, AgentMessage::Llm(LlmMessage { role: MessageRole::System, .. })));
    }
}

/// Callback for getting steering messages mid-execution
#[async_trait::async_trait]
pub trait SteeringProvider: Send + Sync {
    /// Check for user steering messages during tool execution
    ///
    /// Called after each tool execution. If messages are returned,
    /// remaining tool calls are skipped and these are injected
    /// before the next LLM call.
    async fn get_steering_messages(&self) -> Vec<AgentMessage>;

    /// Check for follow-up messages after agent would stop
    ///
    /// Called when the agent has no more tool calls.
    /// If messages are returned, the agent continues with another turn.
    async fn get_follow_up_messages(&self) -> Vec<AgentMessage>;
}

/// No-op steering provider (default behavior)
pub struct NoOpSteeringProvider;

#[async_trait::async_trait]
impl SteeringProvider for NoOpSteeringProvider {
    async fn get_steering_messages(&self) -> Vec<AgentMessage> {
        Vec::new()
    }

    async fn get_follow_up_messages(&self) -> Vec<AgentMessage> {
        Vec::new()
    }
}

/// Configuration for context window management
#[derive(Debug, Clone)]
pub struct ContextWindowConfig {
    /// Maximum tokens before pruning
    pub max_tokens: usize,

    /// Number of messages to keep when pruning
    pub keep_recent: usize,

    /// Whether to summarize pruned messages
    pub summarize: bool,
}

impl Default for ContextWindowConfig {
    fn default() -> Self {
        Self {
            max_tokens: 128_000,
            keep_recent: 10,
            summarize: false,
        }
    }
}

/// Context transformer for managing context window
#[async_trait::async_trait]
pub trait ContextTransformer: Send + Sync {
    /// Transform context before sending to LLM
    ///
    /// Use this for:
    /// - Context window management (pruning old messages)
    /// - Injecting external context
    /// - Summarizing conversation history
    async fn transform(&self, context: AgentContext) -> anyhow::Result<AgentContext>;
}

/// Default context transformer with token-based pruning
pub struct DefaultContextTransformer {
    config: ContextWindowConfig,
}

impl DefaultContextTransformer {
    pub fn new(config: ContextWindowConfig) -> Self {
        Self { config }
    }
}

#[async_trait::async_trait]
impl ContextTransformer for DefaultContextTransformer {
    async fn transform(&self, mut context: AgentContext) -> anyhow::Result<AgentContext> {
        let estimated_tokens = context.estimate_tokens();

        if estimated_tokens > self.config.max_tokens {
            // Keep system message + recent messages
            let mut pruned = Vec::new();

            // Always keep system message first
            if let Some(system) = context.messages.first() {
                if matches!(system, AgentMessage::Llm(LlmMessage { role: MessageRole::System, .. })) {
                    pruned.push(system.clone());
                }
            }

            // Keep recent messages
            let start = context.messages.len().saturating_sub(self.config.keep_recent);
            pruned.extend(context.messages[start..].iter().cloned());

            context.messages = pruned;
        }

        Ok(context)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_message_creation() {
        let system = AgentMessage::system("You are a helpful assistant");
        let user = AgentMessage::user("Hello");
        let assistant = AgentMessage::assistant("Hi there!");

        assert!(matches!(system, AgentMessage::Llm(LlmMessage { role: MessageRole::System, .. })));
        assert!(matches!(user, AgentMessage::Llm(LlmMessage { role: MessageRole::User, .. })));
        assert!(matches!(assistant, AgentMessage::Llm(LlmMessage { role: MessageRole::Assistant, .. })));
    }

    #[test]
    fn test_custom_message_visibility() {
        let notification = AgentMessage::notification(NotificationLevel::Info, "Title", "Body");
        let steering = AgentMessage::steering("Please change approach");

        assert!(!notification.is_llm_visible());
        assert!(steering.is_llm_visible());
    }

    #[test]
    fn test_context_management() {
        let mut context = AgentContext::with_system_prompt("System prompt");
        context.add_message(AgentMessage::user("Hello"));
        context.add_message(AgentMessage::assistant("Hi!"));

        assert_eq!(context.messages.len(), 3);
        assert_eq!(context.llm_messages().len(), 3);

        context.add_message(AgentMessage::notification(NotificationLevel::Info, "Test", "Body"));
        assert_eq!(context.messages.len(), 4);
        assert_eq!(context.llm_messages().len(), 3); // Notification filtered out
    }

    #[tokio::test]
    async fn test_json_converter() {
        let converter = JsonMessageConverter;
        let messages = vec![
            AgentMessage::system("System"),
            AgentMessage::user("Hello"),
            AgentMessage::assistant("Hi!"),
        ];

        let json = converter.convert(messages).await.unwrap();
        assert_eq!(json.len(), 3);
        assert_eq!(json[0]["role"], "system");
        assert_eq!(json[1]["role"], "user");
        assert_eq!(json[2]["role"], "assistant");
    }
}
