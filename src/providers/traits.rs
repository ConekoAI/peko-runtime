//! Provider types
//!
//! Canonical type definitions for LLM provider interactions.
//! The provider implementation itself has moved to `providers::core::Provider`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::common::types::message::{ContentBlock, TokenUsage};

/// Unique content block ID for streaming correlation
pub type ContentBlockId = String;

/// Block type for streaming events
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockType {
    Text,
    ToolCall,
    Thinking,
}

/// Content delta for streaming
#[derive(Debug, Clone)]
pub enum ContentDelta {
    Text(String),
    ToolCall {
        name: Option<String>,
        arguments: Value,
    },
}

/// Tool definition for native tool calling
///
/// Providers translate this into their native tool schema format
/// (e.g., `OpenAI`'s function calling format, Anthropic's tool use)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    /// Tool name (must match the tool's registered name)
    pub name: String,
    /// Tool description for the model
    pub description: String,
    /// JSON Schema for tool parameters
    pub parameters: Value,
}

/// Streaming event from provider
///
/// Providers emit these events during streaming responses.
/// This allows the agent loop to handle incremental updates,
/// tool calls, and reasoning content.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// Stream started
    Start {
        /// Provider name
        provider: String,
        /// Model being used
        model: String,
    },
    /// Text content started
    TextStart {
        /// Index in the content array
        content_index: usize,
    },
    /// Text delta (incremental content)
    TextDelta {
        /// Index in the content array
        content_index: usize,
        /// Delta text
        delta: String,
    },
    /// Text content complete
    TextEnd {
        /// Index in the content array
        content_index: usize,
        /// Full text content
        content: String,
    },
    /// Thinking/reasoning started
    ThinkingStart {
        /// Index in the content array
        content_index: usize,
    },
    /// Thinking delta
    ThinkingDelta {
        /// Index in the content array
        content_index: usize,
        /// Delta thinking text
        delta: String,
    },
    /// Thinking complete
    ThinkingEnd {
        /// Index in the content array
        content_index: usize,
        /// Full thinking content
        content: String,
    },
    /// Tool call started
    ToolCallStart {
        /// Index in the content array
        content_index: usize,
    },
    /// Tool call delta (for streaming arguments)
    ToolCallDelta {
        /// Index in the content array
        content_index: usize,
        /// Delta (JSON fragment)
        delta: String,
    },
    /// Tool call complete
    ToolCallEnd {
        /// Index in the content array
        content_index: usize,
        /// Complete tool call
        tool_call: crate::common::types::message::ContentBlock,
    },
    /// Stream completed
    Done {
        /// Stop reason
        stop_reason: StopReason,
    },
    /// Token usage information (typically sent at end of stream)
    Usage {
        /// Input tokens
        input: u64,
        /// Output tokens
        output: u64,
        /// Total tokens
        total: u64,
    },
    /// Error occurred
    Error {
        /// Error message
        message: String,
    },
}

/// Why a response stopped
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// Normal completion
    Stop,
    /// Hit token limit
    Length,
    /// Tool use requested
    ToolUse,
    /// Error occurred
    Error,
    /// Aborted by user
    Aborted,
}

/// Options for chat completion
#[derive(Debug, Clone, Default)]
pub struct ChatOptions {
    /// Temperature (0.0 - 2.0)
    pub temperature: Option<f32>,
    /// Maximum tokens to generate
    pub max_tokens: Option<u32>,
    /// API key (optional - uses env var if not provided)
    pub api_key: Option<String>,
    /// Additional headers
    pub headers: std::collections::HashMap<String, String>,
}

/// Response from chat completion
#[derive(Debug, Clone)]
pub struct ChatResponse {
    /// Message content blocks
    pub content: Vec<ContentBlock>,
    /// Tool calls (if any)
    pub tool_calls: Vec<ContentBlock>,
    /// Stop reason
    pub stop_reason: StopReason,
    /// Token usage
    pub usage: TokenUsage,
    /// Provider name
    pub provider: String,
    /// Model used
    pub model: String,
}
