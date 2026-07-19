//! Provider types
//!
//! Canonical type definitions for LLM provider interactions.
//! The provider implementation itself has moved to `providers::core::Provider`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::providers::cache_retention::CacheRetention;

// Re-export the message-domain types that are part of the public
// provider surface so adapter modules can pull them all from
// `crate::providers::traits::*` without an extra import.
pub use crate::common::types::message::{ContentBlock, LlmMessage, MessageRole, TokenUsage};

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
        /// Input tokens (uncached, non-reasoning)
        input: u64,
        /// Output tokens (includes reasoning/thinking tokens, which the
        /// provider already folds into `completion_tokens` /
        /// `output_tokens`)
        output: u64,
        /// Total tokens (wire-reported `total_tokens` when present;
        /// otherwise `input + output`)
        total: u64,
        /// Tokens billed at cache-write rate (Anthropic only)
        cache_creation_input_tokens: u64,
        /// Tokens billed at cache-read rate (Anthropic + OpenAI)
        cache_read_input_tokens: u64,
        /// Reasoning tokens within `output` (OpenAI o-series)
        reasoning_output_tokens: u64,
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
    /// Prompt-cache retention policy (F23). `Default` lets the
    /// provider pick its own TTL; `Long` requests the longest TTL
    /// the provider supports; `None` disables cache markers and
    /// session-affinity fields entirely.
    pub cache_retention: CacheRetention,
    /// Stable session identifier used as the cache key. Anthropic
    /// adapters map this to `metadata.user_id`; OpenAI adapters map
    /// it to `prompt_cache_key`. When `None`, the caller relies on
    /// the provider's automatic prefix-detection only.
    pub prompt_cache_key: Option<String>,
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
