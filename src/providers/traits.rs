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

/// Reasoning-effort knob surfaced to callers (F25).
///
/// Each adapter maps this onto its provider-native field:
/// - OpenAI Chat Completions: `body["reasoning_effort"] = "low"|"medium"|"high"`
/// - OpenAI Responses:        `body["reasoning"] = {effort, summary}`
/// - Anthropic:               `thinking: {type:"adaptive"}` + `output_config`
///                            for Opus 4-6+ / Sonnet 5 / Fable 5+, otherwise
///                            `thinking: {type:"enabled", budget_tokens: N}`
///                            with an effort→budget mapping
///                            (low→1024, medium→4096, high→32_000).
///
/// `None` means "do not request reasoning" (default — most callers
/// won't have reasoning enabled). `Adaptive` is honored only on
/// adapters that detect adaptive-capable model ids; others fall
/// back to a `High` budget mapping.
/// `XHigh` and `Max` map to OpenAI's wire vocabulary (`"xhigh"`,
/// `"max"`) but are silently clamped on adapters that don't yet
/// support them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ThinkingEffort {
    #[default]
    None,
    Low,
    Medium,
    High,
    XHigh,
    Max,
    Adaptive,
}

impl ThinkingEffort {
    /// True when the caller asked for any reasoning field on the wire.
    /// `None` is the only "off" variant — every other value triggers
    /// the per-adapter reasoning emission.
    #[must_use]
    pub fn is_enabled(self) -> bool {
        !matches!(self, Self::None)
    }

    /// Wire-string for Chat Completions' `reasoning_effort` field.
    /// Returns `None` for `Self::None` (caller decides whether to
    /// emit the field at all) and `None` for `Adaptive` (Chat
    /// Completions has no adaptive mode — the caller should map it
    /// to `High` first if targeting Chat Completions).
    #[must_use]
    pub fn as_chat_completions_str(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::Low => Some("low"),
            Self::Medium => Some("medium"),
            Self::High => Some("high"),
            Self::XHigh => Some("xhigh"),
            Self::Max => Some("max"),
            Self::Adaptive => None,
        }
    }

    /// Effort → Anthropic `budget_tokens` (only honored for budget
    /// mode — adaptive mode ignores the integer). The mapping comes
    /// from codex-rs's `reasoning_effort_to_budget_tokens` table.
    #[must_use]
    pub fn to_anthropic_budget_tokens(self) -> u32 {
        match self {
            Self::None | Self::Adaptive => 0, // caller drops the field
            Self::Low => 1024,
            Self::Medium => 4096,
            Self::High => 32_000,
            Self::XHigh => 64_000,
            Self::Max => 128_000,
        }
    }
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
    /// F25: reasoning-effort knob. Defaults to `None` (no reasoning
    /// on the wire) so the per-adapter request bodies stay byte-for-
    /// byte identical to the pre-F25 shape.
    pub thinking_effort: ThinkingEffort,
    /// F25: Responses-only. When `Some(b)`, emit
    /// `reasoning.summary = "auto"` when `b == true`; when `None`,
    /// suppress the summary key entirely. Chat Completions and
    /// Anthropic ignore this field.
    pub thinking_summary: Option<bool>,
    /// F25: Responses-only. When `true` (the default for callers
    /// that set a `thinking_effort`), emit
    /// `include: ["reasoning.encrypted_content"]` so the model
    /// returns an encrypted reasoning payload that the caller can
    /// pass back into `previous_response_id` chains. Set to `false`
    /// to suppress — useful for low-log retention profiles.
    pub encrypted_reasoning: bool,
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
