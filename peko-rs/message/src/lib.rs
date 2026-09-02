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

mod tool_call_info;
pub mod repair;

/// Lightweight tool call DTO with optional result (Phase 9b.1 lift).
///
/// Re-exported for callers that already use the
/// `peko_extension_host::principal_message::ToolCallInfo` path.
pub use tool_call_info::ToolCallInfo;

/// Session content blocks (`ToolCallBlock`, `ThinkingBlock`).
///
/// Lifted from `crate::session::events` in Phase 9b.N.5b.9b so
/// `peko_engine::SessionView` can accept them in its trait signature.
/// Pure data blocks with `serde` derives only — no behavior.
pub mod session_blocks;
pub use session_blocks::{ThinkingBlock, ToolCallBlock};

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
    Image {
        source: ImageSource,
        mime_type: String,
    },

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
    Thinking {
        text: String,
        signature: Option<String>,
    },
}

impl TokenUsage {
    /// Accumulate `other` into `self`, folding cache reads/writes
    /// into the canonical `input` bucket and reasoning tokens into
    /// `output`. Preserves the raw cache/reasoning sub-fields so the
    /// audit trail in the JSONL session file retains the breakdown.
    ///
    /// This mirrors the folding rule used by the engine loop's
    /// `iteration_usage` accumulator (`engine/agentic_loop.rs`) — a
    /// single source of truth for "what counts toward a 1M input
    /// tokens/day quota".
    pub fn accumulate(&mut self, other: &TokenUsage) {
        let cache_creation = other.cache_creation_input_tokens.unwrap_or(0);
        let cache_read = other.cache_read_input_tokens.unwrap_or(0);
        let reasoning = other.reasoning_output_tokens.unwrap_or(0);
        self.input += other.input + cache_creation + cache_read;
        self.output += other.output + reasoning;
        self.total += other.total + cache_creation + cache_read + reasoning;
        if cache_creation > 0 {
            *self.cache_creation_input_tokens.get_or_insert(0) += cache_creation;
        }
        if cache_read > 0 {
            *self.cache_read_input_tokens.get_or_insert(0) += cache_read;
        }
        if reasoning > 0 {
            *self.reasoning_output_tokens.get_or_insert(0) += reasoning;
        }
    }
}

/// Image dimensions in pixels.
///
/// PR 1 of the image-aware retention budget (`features/image-aware-retention`).
/// When present, the compaction estimator uses
/// `ceil(width * height / 750)` (OpenAI "high detail" tile math,
/// conservative). When absent the estimator falls back to base64 bytes
/// (`bytes / 0.75`) for `Base64` sources or a mime-type lookup table
/// for `Url` sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageDimensions {
    pub width: u32,
    pub height: u32,
}

impl ImageDimensions {
    /// Tokens consumed at OpenAI "high detail" tile granularity.
    /// 750 px²/token is the canonical "high detail" tile size
    /// (Anthropic uses a similar ~750 figure for Claude 3+ vision).
    /// Rounded up so a 1px sliver still counts.
    #[must_use]
    pub fn high_detail_tokens(&self) -> usize {
        let px = u64::from(self.width) * u64::from(self.height);
        // ceil(px / 750) without overflow on large images
        px.div_ceil(750) as usize
    }
}

/// Image source for image content blocks.
///
/// `dimensions` is optional — adapters populate it when they can
/// parse the source (base64 header magic bytes, data-URL metadata,
/// content-length + content-type probes, etc.). When absent the
/// estimator falls back to bytes or mime-type heuristics, so the
/// field is opt-in for callers and serde-default for JSONL
/// backward-compatibility.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "source_type", rename_all = "snake_case")]
pub enum ImageSource {
    /// Base64-encoded image data
    Base64 {
        data: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dimensions: Option<ImageDimensions>,
    },
    /// URL to image
    Url {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dimensions: Option<ImageDimensions>,
    },
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

/// Token usage statistics
///
/// `input` and `output` are the canonical wire-reported counts
/// (`input_tokens` / `output_tokens` on Anthropic, `prompt_tokens` /
/// `completion_tokens` on OpenAI). `total` is the provider's wire
/// `total_tokens` field when present (OpenAI), or `input + output` when
/// the provider does not report a separate total (Anthropic).
///
/// The three cache/reasoning sub-fields are populated only by adapters
/// that have the corresponding wire fields. They are folded into the
/// canonical `input` / `output` fields by the engine loop accumulator
/// for downstream quota accounting, but preserved verbatim here so the
/// session JSONL retains the raw breakdown for audit.
///
/// `#[serde(default)]` on each sub-field keeps old JSONL files
/// (pre-F17) loadable — missing fields deserialize as `None`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    /// Wire-reported prompt / input tokens (uncached, non-reasoning).
    pub input: u64,
    /// Wire-reported completion / output tokens (including reasoning /
    /// thinking tokens, which Anthropic folds into `output_tokens`).
    pub output: u64,
    /// Wire-reported `total_tokens` when the provider supplies one;
    /// otherwise the loop sets this to `input + output` after
    /// accumulation.
    pub total: u64,
    /// Anthropic `cache_creation_input_tokens`. Tokens billed at the
    /// cache-write rate for newly cached prompt prefixes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u64>,
    /// Anthropic `cache_read_input_tokens` / OpenAI
    /// `prompt_tokens_details.cached_tokens`. Tokens billed at the
    /// cache-read rate (typically ~10% of input).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u64>,
    /// OpenAI `completion_tokens_details.reasoning_tokens`. Subset of
    /// `output` billed at output rate; tracked separately so quota
    /// users can distinguish "thinking" from "visible text" output.
    /// Anthropic folds reasoning into `output_tokens` already.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_output_tokens: Option<u64>,
}

/// Standard LLM message
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmMessage {
    pub role: MessageRole,
    pub content: Vec<ContentBlock>,
    pub timestamp: DateTime<Utc>,
    pub metadata: HashMap<String, Value>,
    /// Tool call ID for tool-result messages
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Provider-reported token usage for this assistant turn. Populated on
    /// assistant messages by the engine loop and by `SessionMessage::to_llm_message`
    /// for replay from session storage. The compactor's
    /// `estimate_context_tokens` walks backward to find the most recent
    /// assistant message with `usage.is_some()` and anchors its size estimate
    /// there, char/4-estimating only the trailing slice. Pre-F21 JSONL files
    /// don't carry this field; `#[serde(default)]` deserialises them as `None`
    /// so old session state keeps loading.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<TokenUsage>,
}

impl Default for LlmMessage {
    fn default() -> Self {
        Self {
            role: MessageRole::User,
            content: Vec::new(),
            timestamp: Utc::now(),
            metadata: HashMap::new(),
            tool_call_id: None,
            usage: None,
        }
    }
}

impl LlmMessage {
    /// Create a simple text message
    pub fn text(role: MessageRole, text: impl Into<String>) -> Self {
        Self {
            role,
            content: vec![ContentBlock::Text { text: text.into() }],
            timestamp: Utc::now(),
            metadata: HashMap::new(),
            tool_call_id: None,
            usage: None,
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
    ///
    /// `is_error` propagates to the `ContentBlock::ToolResult.is_error` field
    /// (F32a) — false for a successful dispatch, true when the tool itself
    /// failed. Models that distinguish failed tool calls (e.g. Anthropic's
    /// `is_error` field on `tool_result` blocks) see the correct shape.
    pub fn tool_result(
        tool_call_id: impl Into<String>,
        name: impl Into<String>,
        result: impl Into<String>,
        is_error: bool,
    ) -> Self {
        let tool_call_id_str = tool_call_id.into();
        Self {
            role: MessageRole::Tool,
            content: vec![ContentBlock::ToolResult {
                tool_call_id: tool_call_id_str.clone(),
                name: name.into(),
                content: vec![ContentBlock::Text {
                    text: result.into(),
                }],
                is_error,
            }],
            timestamp: Utc::now(),
            metadata: HashMap::new(),
            tool_call_id: Some(tool_call_id_str),
            usage: None,
        }
    }

    /// Add metadata to the message
    pub fn with_metadata(mut self, key: impl Into<String>, value: Value) -> Self {
        self.metadata.insert(key.into(), value);
        self
    }

    /// Set the tool call ID
    pub fn with_tool_call_id(mut self, tool_call_id: impl Into<String>) -> Self {
        self.tool_call_id = Some(tool_call_id.into());
        self
    }

    /// Attach provider-reported token usage to this message.
    ///
    /// Only assistant turns carry usage today (user / system / tool
    /// messages always serialize with `usage: None`). Used by the
    /// engine loop at assistant-message construction so the
    /// compactor's `estimate_context_tokens` can anchor on real
    /// provider-reported token counts instead of falling back to
    /// chars/4. Accepts `Option<TokenUsage>` so callers can write
    /// `.with_usage(iteration_usage.clone())` directly — passing
    /// `None` is equivalent to leaving the field unset.
    pub fn with_usage(mut self, usage: impl Into<Option<TokenUsage>>) -> Self {
        self.usage = usage.into();
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
    pub fn tool_result(
        tool_call_id: impl Into<String>,
        name: impl Into<String>,
        result: impl Into<String>,
        is_error: bool,
    ) -> Self {
        Self::Llm(LlmMessage::tool_result(
            tool_call_id,
            name,
            result,
            is_error,
        ))
    }

    /// Create a notification message
    pub fn notification(
        level: NotificationLevel,
        title: impl Into<String>,
        body: impl Into<String>,
    ) -> Self {
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
    #[must_use]
    pub fn is_llm_visible(&self) -> bool {
        match self {
            Self::Llm(_) => true,
            Self::Custom(custom) => matches!(
                custom,
                CustomMessage::Steering { .. } | CustomMessage::FollowUp { .. }
            ),
        }
    }

    /// Get timestamp if available
    #[must_use]
    pub fn timestamp(&self) -> Option<DateTime<Utc>> {
        match self {
            Self::Llm(msg) => Some(msg.timestamp),
            Self::Custom(_) => None,
        }
    }

    /// Convert to a simple text representation
    #[must_use]
    pub fn to_text(&self) -> String {
        match self {
            Self::Llm(msg) => {
                let texts: Vec<String> = msg
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

    /// Estimate token count for this message
    ///
    /// Uses provider-agnostic estimation:
    /// - Base overhead: 4 tokens per message (role, formatting)
    /// - Text content: ~4 chars per token
    /// - Images: dimensions-based (`width * height / 750`) when
    ///   `ImageSource::dimensions` is populated, else 1000 tokens
    ///   legacy fallback. PR 1 (image-aware retention) plumbs
    ///   `dimensions` through the provider adapters so this estimate
    ///   reflects reality instead of a constant.
    /// - Tool calls/results: JSON token count
    #[must_use]
    pub fn estimate_tokens(&self) -> usize {
        match self {
            Self::Llm(msg) => {
                let base_overhead = 4; // Message formatting overhead
                let content_tokens: usize = msg
                    .content
                    .iter()
                    .map(|block| match block {
                        ContentBlock::Text { text } => estimate_text_tokens(text),
                        ContentBlock::Image { source, .. } => {
                            // PR 1: prefer dimensions-based estimate when
                            // available. Fall back to legacy 1000-token
                            // constant for backwards compatibility with
                            // JSONL written before PR 1.
                            image_token_estimate(source).unwrap_or(1000)
                        }
                        ContentBlock::ToolCall {
                            name, arguments, ..
                        } => {
                            // Tool call: name + JSON args
                            estimate_text_tokens(name) + estimate_json_tokens(arguments)
                        }
                        ContentBlock::ToolResult { content, .. } => {
                            // Tool result: sum of nested content
                            content
                                .iter()
                                .map(|c| match c {
                                    ContentBlock::Text { text } => estimate_text_tokens(text),
                                    _ => 0,
                                })
                                .sum()
                        }
                        ContentBlock::Thinking { text, .. } => estimate_text_tokens(text),
                    })
                    .sum();
                base_overhead + content_tokens
            }
            Self::Custom(_) => 0, // Custom messages not sent to LLM
        }
    }
}

/// Estimate tokens for text content
/// Uses ~4 characters per token average for English
fn estimate_text_tokens(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }

    // Count words and characters for better estimation
    let word_count = text.split_whitespace().count();
    let char_count = text.len();

    // Hybrid: max of word-based and char-based estimation
    // Words tend to be ~1.3 tokens each on average
    // Characters are ~4 per token
    let word_estimate = (word_count * 13) / 10;
    let char_estimate = char_count / 4;

    word_estimate.max(char_estimate).max(1)
}

/// Estimate tokens for JSON content
fn estimate_json_tokens(value: &Value) -> usize {
    // JSON is roughly 1 token per 2 characters (more verbose than text)
    let json_string = value.to_string();
    json_string.len() / 2
}

/// Image token estimate from an `ImageSource`. Returns `None` when
/// dimensions are not populated — caller decides the fallback
/// (legacy constant in peko-message, mime-type table in peko-session).
///
/// PR 1 helper so the message crate can do its own per-message
/// estimate without depending on `peko-session::image_budget`. The
/// session-side estimator wraps this and adds the more accurate
/// fallback table for URL sources.
fn image_token_estimate(source: &ImageSource) -> Option<usize> {
    let dimensions = match source {
        ImageSource::Base64 { dimensions, .. } | ImageSource::Url { dimensions, .. } => {
            *dimensions
        }
    };
    dimensions.map(|d| d.high_detail_tokens())
}

/// Best-effort dimension extraction from decoded image bytes.
///
/// Supports PNG (always — IHDR header at offset 16-23, big-endian u32 width
/// then height). JPEG/GIF/WebP are out of scope for this PR; the estimator
/// falls back to bytes/0.75 or mime-type tables when `None`.
///
/// Adapter callers should decode the base64 payload themselves and pass
/// the resulting bytes + `mime_type`. Returns `None` when the format isn't
/// recognized, the signature doesn't match, or the dimensions are zero.
pub fn extract_dimensions_from_base64(bytes: &[u8], mime_type: &str) -> Option<ImageDimensions> {
    if mime_type.eq_ignore_ascii_case("image/png") {
        // PNG signature: 89 50 4E 47 0D 0A 1A 0A
        const PNG_SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        if bytes.len() < 24 || bytes[..8] != PNG_SIGNATURE {
            return None;
        }
        // IHDR chunk: 4-byte length, "IHDR", then 13-byte payload starting
        // with width (BE u32) and height (BE u32). Bytes 16-23 are width+height.
        let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
        let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
        if width == 0 || height == 0 {
            return None;
        }
        Some(ImageDimensions { width, height })
    } else {
        None
    }
}

impl Default for AgentMessage {
    fn default() -> Self {
        Self::user("")
    }
}

/// Message converter trait for transforming `AgentMessage` to LLM format
#[async_trait::async_trait]
pub trait MessageConverter: Send + Sync {
    /// Convert `AgentMessage` to provider-specific format
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
                    0 => Value::String(String::new()),
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
pub struct MessageContext {
    /// All messages in the conversation (including custom)
    pub messages: Vec<AgentMessage>,

    /// System prompt
    pub system_prompt: String,

    /// Context-level metadata
    pub metadata: HashMap<String, Value>,
}

impl MessageContext {
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
    #[must_use]
    pub fn llm_messages(&self) -> Vec<AgentMessage> {
        self.messages
            .iter()
            .filter(|m| m.is_llm_visible())
            .cloned()
            .collect()
    }

    /// Get messages for a specific role
    #[must_use]
    pub fn messages_by_role(&self, role: MessageRole) -> Vec<&LlmMessage> {
        self.messages
            .iter()
            .filter_map(|m| match m {
                AgentMessage::Llm(msg) if msg.role == role => Some(msg),
                _ => None,
            })
            .collect()
    }

    /// Estimate token count using a more accurate approximation
    ///
    /// This uses a hybrid approach:
    /// - 4 characters per token for English text (GPT-3/4 average)
    /// - 1 token per word for whitespace-separated text
    /// - Additional overhead for message formatting (role, metadata)
    /// - Image content estimated at 1k tokens each (varies by provider)
    #[must_use]
    pub fn estimate_tokens(&self) -> usize {
        self.messages
            .iter()
            .map(AgentMessage::estimate_tokens)
            .sum()
    }

    /// Clear messages except system prompt
    pub fn clear_conversation(&mut self) {
        self.messages.retain(|m| {
            matches!(
                m,
                AgentMessage::Llm(LlmMessage {
                    role: MessageRole::System,
                    ..
                })
            )
        });
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
    async fn transform(&self, context: MessageContext) -> anyhow::Result<MessageContext>;
}

/// Default context transformer with token-based pruning
pub struct DefaultContextTransformer {
    config: ContextWindowConfig,
}

impl DefaultContextTransformer {
    #[must_use]
    pub fn new(config: ContextWindowConfig) -> Self {
        Self { config }
    }
}

#[async_trait::async_trait]
impl ContextTransformer for DefaultContextTransformer {
    async fn transform(&self, mut context: MessageContext) -> anyhow::Result<MessageContext> {
        let estimated_tokens = context.estimate_tokens();

        if estimated_tokens > self.config.max_tokens {
            // Keep system message + recent messages
            let mut pruned = Vec::new();

            // Always keep system message first
            if let Some(system) = context.messages.first() {
                if matches!(
                    system,
                    AgentMessage::Llm(LlmMessage {
                        role: MessageRole::System,
                        ..
                    })
                ) {
                    pruned.push(system.clone());
                }
            }

            // Keep recent messages
            let start = context
                .messages
                .len()
                .saturating_sub(self.config.keep_recent);
            pruned.extend(context.messages[start..].iter().cloned());

            context.messages = pruned;
        }

        Ok(context)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `TokenUsage::accumulate` folds cache reads + writes into the
    /// canonical `input` bucket and reasoning into `output`, while
    /// preserving the raw sub-fields for audit. Single source of truth
    /// for "what counts toward a quota limit" — used by both the
    /// engine loop accumulator and the compactor's multi-call rollup.
    #[test]
    fn test_token_usage_accumulate_folds_cache_and_reasoning() {
        let mut total = TokenUsage::default();
        let iter1 = TokenUsage {
            input: 100,
            output: 50,
            total: 150,
            cache_creation_input_tokens: Some(1024),
            cache_read_input_tokens: Some(4096),
            reasoning_output_tokens: Some(20),
        };
        total.accumulate(&iter1);
        // input folds cache_creation + cache_read in
        assert_eq!(total.input, 100 + 1024 + 4096);
        // output folds reasoning in
        assert_eq!(total.output, 50 + 20);
        // total adds the same fold
        assert_eq!(total.total, 150 + 1024 + 4096 + 20);
        // raw sub-fields preserved for audit
        assert_eq!(total.cache_creation_input_tokens, Some(1024));
        assert_eq!(total.cache_read_input_tokens, Some(4096));
        assert_eq!(total.reasoning_output_tokens, Some(20));

        let iter2 = TokenUsage {
            input: 50,
            output: 30,
            total: 80,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: Some(2048),
            reasoning_output_tokens: None,
        };
        total.accumulate(&iter2);
        assert_eq!(total.input, (100 + 1024 + 4096) + (50 + 2048));
        assert_eq!(total.output, (50 + 20) + 30);
        assert_eq!(total.cache_read_input_tokens, Some(4096 + 2048));
    }

    /// Accumulating zero usage (e.g. an empty stream) leaves the
    /// accumulator unchanged and does not insert `Some(0)` sub-fields.
    /// Sub-fields stay `None` when the added usage had no cache or
    /// reasoning tokens, so JSONL serialisation skips them.
    #[test]
    fn test_token_usage_accumulate_zero_does_not_promote_subfields() {
        let mut total = TokenUsage::default();
        let empty = TokenUsage::default();
        total.accumulate(&empty);
        assert_eq!(total, TokenUsage::default());
        assert_eq!(total.cache_creation_input_tokens, None);
        assert_eq!(total.cache_read_input_tokens, None);
        assert_eq!(total.reasoning_output_tokens, None);
    }

    /// Backwards-compat: serialise the pre-F17 shape (no cache or
    /// reasoning fields) and deserialise into the F17 struct. The
    /// sub-fields must load as `None` so old JSONL files keep working.
    #[test]
    fn test_token_usage_backwards_compat_serde() {
        let legacy = serde_json::json!({
            "input": 100,
            "output": 50,
            "total": 150,
        });
        let usage: TokenUsage = serde_json::from_value(legacy).unwrap();
        assert_eq!(usage.input, 100);
        assert_eq!(usage.output, 50);
        assert_eq!(usage.total, 150);
        assert_eq!(usage.cache_creation_input_tokens, None);
        assert_eq!(usage.cache_read_input_tokens, None);
        assert_eq!(usage.reasoning_output_tokens, None);
    }

    /// Round-trip: serialise an F17-shaped struct and deserialise it.
    /// `skip_serializing_if = "Option::is_none"` keeps the on-disk
    /// shape identical to pre-F17 when the sub-fields are unset, so
    /// existing tools that read session JSONL see no change.
    #[test]
    fn test_token_usage_roundtrip_with_cache_fields() {
        let usage = TokenUsage {
            input: 1000,
            output: 500,
            total: 1500,
            cache_creation_input_tokens: Some(1024),
            cache_read_input_tokens: Some(4096),
            reasoning_output_tokens: Some(200),
        };
        let json = serde_json::to_value(usage).unwrap();
        assert_eq!(json["input"], 1000);
        assert_eq!(json["cache_read_input_tokens"], 4096);
        let parsed: TokenUsage = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, usage);
    }

    /// When no sub-fields are populated, serialisation omits them so
    /// the JSONL shape stays identical to pre-F17.
    #[test]
    fn test_token_usage_serialisation_skips_none_subfields() {
        let usage = TokenUsage {
            input: 100,
            output: 50,
            total: 150,
            ..Default::default()
        };
        let json = serde_json::to_value(usage).unwrap();
        let obj = json.as_object().unwrap();
        assert!(!obj.contains_key("cache_creation_input_tokens"));
        assert!(!obj.contains_key("cache_read_input_tokens"));
        assert!(!obj.contains_key("reasoning_output_tokens"));
    }

    /// F21: round-trip an `LlmMessage` with `usage` populated. The
    /// `usage` field is what lets `estimate_context_tokens` anchor on
    /// real provider-reported token counts — without persistence
    /// working, session reloads would always fall back to chars/4.
    #[test]
    fn test_llm_message_usage_roundtrip() {
        let usage = TokenUsage {
            input: 1200,
            output: 600,
            total: 1800,
            cache_read_input_tokens: Some(4096),
            ..Default::default()
        };
        let msg = LlmMessage::assistant("hello").with_usage(usage);
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["usage"]["input"], 1200);
        assert_eq!(json["usage"]["cache_read_input_tokens"], 4096);
        let parsed: LlmMessage = serde_json::from_value(json).unwrap();
        assert_eq!(parsed.usage, Some(usage));
    }

    /// F21: back-compat — JSON without `usage` deserialises to `None`
    /// so pre-F21 session JSONL files keep loading.
    #[test]
    fn test_llm_message_usage_absent_is_none_on_deserialize() {
        let legacy = serde_json::json!({
            "role": "assistant",
            "content": [{"type": "text", "text": "hi"}],
            "timestamp": "2026-01-01T00:00:00Z",
            "metadata": {}
        });
        let msg: LlmMessage = serde_json::from_value(legacy).unwrap();
        assert_eq!(msg.usage, None);
    }

    /// F21: `skip_serializing_if = "Option::is_none"` keeps the JSONL
    /// shape identical to pre-F21 when no usage is attached.
    #[test]
    fn test_llm_message_usage_serialisation_skips_when_none() {
        let msg = LlmMessage::assistant("hi");
        let json = serde_json::to_value(&msg).unwrap();
        let obj = json.as_object().unwrap();
        assert!(!obj.contains_key("usage"));
    }

    #[test]
    fn test_agent_message_creation() {
        let system = AgentMessage::system("You are a helpful assistant");
        let user = AgentMessage::user("Hello");
        let assistant = AgentMessage::assistant("Hi there!");

        assert!(matches!(
            system,
            AgentMessage::Llm(LlmMessage {
                role: MessageRole::System,
                ..
            })
        ));
        assert!(matches!(
            user,
            AgentMessage::Llm(LlmMessage {
                role: MessageRole::User,
                ..
            })
        ));
        assert!(matches!(
            assistant,
            AgentMessage::Llm(LlmMessage {
                role: MessageRole::Assistant,
                ..
            })
        ));
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
        let mut context = MessageContext::with_system_prompt("System prompt");
        context.add_message(AgentMessage::user("Hello"));
        context.add_message(AgentMessage::assistant("Hi!"));

        assert_eq!(context.messages.len(), 3);
        assert_eq!(context.llm_messages().len(), 3);

        context.add_message(AgentMessage::notification(
            NotificationLevel::Info,
            "Test",
            "Body",
        ));
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

    // ===================== F32a: is_error propagation tests =====================

    /// Pin the wire shape: a successful tool call preserves `is_error: false`
    /// into the `ContentBlock::ToolResult` block.
    #[test]
    fn test_llm_message_tool_result_success_carries_is_error_false() {
        let msg = LlmMessage::tool_result("tc1", "Read", "file body", false);
        match &msg.content[0] {
            ContentBlock::ToolResult { is_error, .. } => {
                assert!(!*is_error, "success path must set is_error=false");
            }
            _ => panic!("expected ToolResult block"),
        }
    }

    /// Pin the wire shape: a failed tool execution (e.g. tool impl returned
    /// `Err(anyhow!(...))`, or the dispatch gate refused) must propagate
    /// `is_error: true` to the `ContentBlock::ToolResult` block. Pre-F32a
    /// the constructor hardcoded `false` regardless of dispatch outcome.
    #[test]
    fn test_llm_message_tool_result_failure_carries_is_error_true() {
        let msg = LlmMessage::tool_result("tc1", "Read", "Error: not found", true);
        match &msg.content[0] {
            ContentBlock::ToolResult { is_error, .. } => {
                assert!(*is_error, "failure path must set is_error=true");
            }
            _ => panic!("expected ToolResult block"),
        }
    }

    /// `AgentMessage::tool_result` (the `Self::Llm(...)` wrapper) must
    /// forward the flag through to the inner `LlmMessage`.
    #[test]
    fn test_agent_message_tool_result_forwards_is_error() {
        let ok = AgentMessage::tool_result("tc1", "Read", "body", false);
        let err = AgentMessage::tool_result("tc2", "Read", "Error", true);

        let AgentMessage::Llm(ok_inner) = ok else {
            panic!("expected Llm variant")
        };
        let AgentMessage::Llm(err_inner) = err else {
            panic!("expected Llm variant")
        };

        match &ok_inner.content[0] {
            ContentBlock::ToolResult { is_error, .. } => assert!(!*is_error),
            _ => panic!("expected ToolResult block"),
        }
        match &err_inner.content[0] {
            ContentBlock::ToolResult { is_error, .. } => assert!(*is_error),
            _ => panic!("expected ToolResult block"),
        }
    }

    // ===================== PR 1: image dimensions tests =====================

    /// `high_detail_tokens` matches OpenAI's `high` detail tile math
    /// (`ceil(width * height / 750)`):
    /// - 512×512 → ceil(262144/750) = 350
    /// - 1024×1024 → ceil(1048576/750) = 1398
    /// - 2048×2048 → ceil(4194304/750) = 5593
    /// - 1×1 → 1 (rounded up)
    #[test]
    fn test_image_dimensions_high_detail_tokens() {
        assert_eq!(
            ImageDimensions { width: 512, height: 512 }.high_detail_tokens(),
            350
        );
        assert_eq!(
            ImageDimensions { width: 1024, height: 1024 }.high_detail_tokens(),
            1399
        );
        assert_eq!(
            ImageDimensions { width: 2048, height: 2048 }.high_detail_tokens(),
            5593
        );
        // Edge: 1x1 image still costs 1 token (ceil(1/750))
        assert_eq!(
            ImageDimensions { width: 1, height: 1 }.high_detail_tokens(),
            1
        );
    }

    /// Image with `dimensions` populated costs `high_detail_tokens`,
    /// not the 1000-token legacy fallback. The 1024×1024 case costs
    /// ~1398 tokens — fixing the under-count that motivated PR 1.
    #[test]
    fn test_image_with_dimensions_estimates_realistic_tokens() {
        let img = ContentBlock::Image {
            source: ImageSource::Base64 {
                data: "ignored".to_string(),
                dimensions: Some(ImageDimensions { width: 1024, height: 1024 }),
            },
            mime_type: "image/png".to_string(),
        };
        let msg = LlmMessage {
            role: MessageRole::User,
            content: vec![img],
            ..Default::default()
        };
        let est = AgentMessage::Llm(msg).estimate_tokens();
        // base_overhead (4) + high_detail_tokens (1399) = 1403
        assert_eq!(est, 4 + 1399);
    }

    /// Image without `dimensions` falls back to the legacy 1000-token
    /// estimate so pre-PR-1 JSONL and URL-only sources keep working.
    #[test]
    fn test_image_without_dimensions_uses_legacy_estimate() {
        let img = ContentBlock::Image {
            source: ImageSource::Url {
                url: "https://example.com/x.png".to_string(),
                dimensions: None,
            },
            mime_type: "image/png".to_string(),
        };
        let msg = LlmMessage {
            role: MessageRole::User,
            content: vec![img],
            ..Default::default()
        };
        let est = AgentMessage::Llm(msg).estimate_tokens();
        // base_overhead (4) + legacy (1000) = 1004
        assert_eq!(est, 4 + 1000);
    }

    /// Round-trip: `ImageSource` with `dimensions` serialises with
    /// the field present and deserialises back to the same struct.
    #[test]
    fn test_image_source_dimensions_roundtrip() {
        let src = ImageSource::Base64 {
            data: "AAAA".to_string(),
            dimensions: Some(ImageDimensions { width: 800, height: 600 }),
        };
        let json = serde_json::to_value(&src).unwrap();
        assert_eq!(json["source_type"], "base64");
        assert_eq!(json["data"], "AAAA");
        assert_eq!(json["dimensions"]["width"], 800);
        assert_eq!(json["dimensions"]["height"], 600);
        let parsed: ImageSource = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, src);
    }

    /// Backwards-compat: pre-PR-1 JSONL without `dimensions`
    /// deserialises to `dimensions: None`. Verifies the serde-default
    /// on both variants.
    #[test]
    fn test_image_source_dimensions_legacy_loads_as_none() {
        let legacy = serde_json::json!({"source_type": "base64", "data": "AAAA"});
        let parsed: ImageSource = serde_json::from_value(legacy).unwrap();
        match parsed {
            ImageSource::Base64 { data, dimensions } => {
                assert_eq!(data, "AAAA");
                assert_eq!(dimensions, None);
            }
            _ => panic!("expected Base64 variant"),
        }

        let legacy_url = serde_json::json!({"source_type": "url", "url": "https://x"});
        let parsed_url: ImageSource = serde_json::from_value(legacy_url).unwrap();
        match parsed_url {
            ImageSource::Url { url, dimensions } => {
                assert_eq!(url, "https://x");
                assert_eq!(dimensions, None);
            }
            _ => panic!("expected Url variant"),
        }
    }

    /// When `dimensions: None`, the on-disk JSONL shape omits the
    /// `dimensions` key entirely (skip_serializing_if). Pre-PR-1
    /// readers see no schema change.
    #[test]
    fn test_image_source_no_dimensions_omits_key() {
        let src = ImageSource::Url {
            url: "https://example.com/x.png".to_string(),
            dimensions: None,
        };
        let json = serde_json::to_value(&src).unwrap();
        assert!(
            json.as_object().unwrap().get("dimensions").is_none(),
            "None dimensions should be skipped from serialisation"
        );
    }

    /// `extract_dimensions_from_base64` reads width/height from the
    /// PNG IHDR chunk. 1024×1024 is the canonical OpenAI "high detail"
    /// image size and should round-trip cleanly.
    #[test]
    fn test_extract_dimensions_from_png_1024() {
        // Hand-built 24-byte header: PNG signature + 8-byte IHDR prefix
        // (length + "IHDR" tag) + width + height. IHDR's first 8 bytes
        // follow the signature; we synthesise the prefix as zeros.
        let mut bytes = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        bytes.extend([0u8; 8]); // IHDR length + tag placeholder
        bytes.extend(1024u32.to_be_bytes());
        bytes.extend(1024u32.to_be_bytes());
        let dims = extract_dimensions_from_base64(&bytes, "image/png").unwrap();
        assert_eq!(dims.width, 1024);
        assert_eq!(dims.height, 1024);
        assert_eq!(dims.high_detail_tokens(), ((1024 * 1024 + 749) / 750) as usize);
    }

    /// Non-PNG mime types fall through to `None` (JPEG extraction is
    /// deferred to a follow-up — SOFn marker parsing is non-trivial).
    #[test]
    fn test_extract_dimensions_non_png_returns_none() {
        let bytes = vec![0u8; 32];
        assert!(extract_dimensions_from_base64(&bytes, "image/jpeg").is_none());
        assert!(extract_dimensions_from_base64(&bytes, "image/webp").is_none());
    }

    /// Bytes shorter than the PNG signature + IHDR payload are rejected.
    #[test]
    fn test_extract_dimensions_short_png_returns_none() {
        let bytes = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]; // 8 bytes
        assert!(extract_dimensions_from_base64(&bytes, "image/png").is_none());
    }

    /// PNG signature mismatch (truncated JPEG bytes labeled PNG)
    /// returns `None` rather than panic.
    #[test]
    fn test_extract_dimensions_bad_signature_returns_none() {
        let bytes = vec![0u8; 32];
        assert!(extract_dimensions_from_base64(&bytes, "image/png").is_none());
    }

    /// Zero-dimension PNG (corrupt IHDR) returns `None` rather than
    /// producing a 0×0 ImageDimensions that would zero the token count.
    #[test]
    fn test_extract_dimensions_zero_dimensions_returns_none() {
        let mut bytes = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        bytes.extend([0u8; 8]);
        bytes.extend(0u32.to_be_bytes());
        bytes.extend(0u32.to_be_bytes());
        assert!(extract_dimensions_from_base64(&bytes, "image/png").is_none());
    }

    // ===================== PR 2: tool result rewrite round-trip =====================

    /// PR 2: `ContentBlock::ToolResult` round-trips through serde
    /// after the rewriter replaces the body with a single Text
    /// sentinel. The output JSONL shape is what
    /// `peko_session::jsonl::text_content()` flattens; verify the
    /// nested Text block carries the full sentinel verbatim so a
    /// `peko session show` can surface "this result was truncated"
    /// without a separate metadata channel.
    #[test]
    fn test_rewritten_tool_result_round_trips() {
        let original = ContentBlock::ToolResult {
            tool_call_id: "tc1".to_string(),
            name: "Read".to_string(),
            content: vec![ContentBlock::Text {
                text: "[truncated by peko_runtime: tool result for call tc1 \
                       was 50000 tokens, reduced to sentinel. Re-invoke the \
                       tool with a narrower scope to get the full result.]"
                    .to_string(),
            }],
            is_error: false,
        };
        let json = serde_json::to_value(&original).unwrap();
        // serde tag = "type", variant = "tool_result"
        assert_eq!(json["type"], "tool_result");
        assert_eq!(json["tool_call_id"], "tc1");
        assert_eq!(json["name"], "Read");
        assert_eq!(json["is_error"], false);
        assert_eq!(
            json["content"][0]["text"]
                .as_str()
                .unwrap()
                .starts_with("[truncated by peko_runtime:"),
            true
        );

        let parsed: ContentBlock = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, original);
    }

    /// PR 2: the `is_error: true → false` flip survives serde so
    /// pre-existing JSONL readers see the truncated body as a
    /// non-error result.
    #[test]
    fn test_tool_result_is_error_flip_round_trips() {
        let original = ContentBlock::ToolResult {
            tool_call_id: "tc1".to_string(),
            name: "Bash".to_string(),
            content: vec![ContentBlock::Text {
                text: "[truncated by peko_runtime: ...]".to_string(),
            }],
            is_error: false,
        };
        let json = serde_json::to_string(&original).unwrap();
        let parsed: ContentBlock = serde_json::from_str(&json).unwrap();
        let ContentBlock::ToolResult { is_error, .. } = &parsed else {
            panic!("expected ToolResult")
        };
        assert!(!*is_error);
    }
}
