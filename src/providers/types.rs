//! Unified provider types - shared across all implementations
//!
//! This module re-exports types from `providers::traits` for convenience.
//! The canonical types are defined in `providers::traits` to maintain
//! compatibility with the existing codebase.

// Re-export types from various modules
pub use crate::providers::traits::{
    BlockType, ChatMessage, ChatOptions, ChatResponse, ContentBlockId, ContentDelta, MessageRole,
    StopReason, StreamEvent, TokenUsage, ToolDefinition,
};
pub use crate::types::message::ContentBlock;

// Re-export ProviderConfig from types::provider
pub use crate::types::provider::ProviderConfig;

/// Tool call block for session storage
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolCallBlock {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// Thinking block for session storage  
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ThinkingBlock {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

/// Authentication configuration for HTTP client
#[derive(Debug, Clone)]
pub enum AuthConfig {
    Bearer { token: String },
    Header { name: String, value: String },
}

/// Unified message format for internal use (adapter layer)
#[derive(Debug, Clone)]
pub struct Message {
    pub role: MessageRole,
    pub content: Vec<ContentBlock>,
    pub tool_call_id: Option<String>, // For tool role messages
}
