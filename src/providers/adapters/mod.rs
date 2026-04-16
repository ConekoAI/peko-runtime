//! API adapters - convert between unified types and provider-specific formats
//!
//! Each adapter handles the specific JSON schema and behavior of one API type:
//! - `OpenAI`: Chat Completions API
//! - Anthropic: Messages API
//! - OpenAI-Compatible: Same as `OpenAI` with different base URL

use crate::providers::transport::AuthConfig;
use crate::providers::types::{ContentBlock, Message, ToolDefinition, ChatOptions, ChatResponse, StreamEvent, MessageRole};
use anyhow::Result;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

pub mod anthropic;
pub mod compat;
pub mod openai;

pub use anthropic::AnthropicAdapter;
pub use compat::OpenAiCompatibleAdapter;
pub use openai::OpenAiAdapter;

/// Accumulates partial tool call data during streaming across multiple SSE events.
///
/// This component handles the stateful accumulation of tool call parts (id, name, arguments)
/// that arrive in separate chunks from streaming LLM responses. It provides a clean
/// separation between event parsing (adapter responsibility) and state accumulation.
#[derive(Debug, Clone)]
pub struct ToolCallAccumulator {
    /// Maps content index to partial tool call data
    buffer: Arc<Mutex<HashMap<usize, PartialToolCall>>>,
}

/// Internal state for a tool call being accumulated
#[derive(Debug, Default)]
struct PartialToolCall {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

impl ToolCallAccumulator {
    /// Create a new empty accumulator
    #[must_use] 
    pub fn new() -> Self {
        Self {
            buffer: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Reset the accumulator, clearing all pending tool calls.
    /// Call this at the start of a new stream.
    pub fn reset(&self) {
        if let Ok(mut buffer) = self.buffer.lock() {
            buffer.clear();
        }
    }

    /// Accumulate a partial tool call part and return the complete tool call if finished.
    ///
    /// # Arguments
    /// * `index` - The content index (position) of this tool call
    /// * `id` - Tool call ID (usually provided in first chunk)
    /// * `name` - Tool name (usually provided in first chunk)
    /// * `arguments` - Partial JSON arguments (accumulated across chunks)
    ///
    /// # Returns
    /// * `Some(ContentBlock::ToolCall)` when all parts are received and JSON is valid
    /// * `None` if still accumulating or on error
    #[must_use] 
    pub fn accumulate(
        &self,
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments: Option<String>,
    ) -> Option<ContentBlock> {
        if let Ok(mut buffer) = self.buffer.lock() {
            let entry = buffer.entry(index).or_default();

            if let Some(id) = id {
                entry.id = Some(id);
            }
            if let Some(name) = name {
                entry.name = Some(name);
            }
            if let Some(args) = arguments {
                entry.arguments.push_str(&args);
            }

            // Check if we have a complete tool call
            if let (Some(id), Some(name)) = (&entry.id, &entry.name) {
                // Try to parse arguments as valid JSON
                if let Ok(arguments) = serde_json::from_str(&entry.arguments) {
                    // Remove from buffer and return complete tool call
                    let complete = ContentBlock::ToolCall {
                        id: id.clone(),
                        name: name.clone(),
                        arguments,
                    };
                    buffer.remove(&index);
                    return Some(complete);
                }
            }
        }
        None
    }

    /// Check if a tool call at the given index is new (not yet in buffer).
    #[must_use] 
    pub fn is_new_call(&self, index: usize, id: &str) -> bool {
        if let Ok(buffer) = self.buffer.lock() {
            !buffer.contains_key(&index)
                || buffer.get(&index).and_then(|p| p.id.as_deref()) != Some(id)
        } else {
            false
        }
    }

    /// Finalize any pending tool call at the given index.
    /// Call this when receiving a "stop" or "end" event for a content block.
    ///
    /// # Returns
    /// * `Some(ContentBlock::ToolCall)` if a pending tool call exists (even with empty/invalid args)
    /// * `None` if no pending tool call at this index
    #[must_use] 
    pub fn finalize(&self, index: usize) -> Option<ContentBlock> {
        if let Ok(mut buffer) = self.buffer.lock() {
            if let Some(entry) = buffer.remove(&index) {
                if let (Some(id), Some(name)) = (entry.id, entry.name) {
                    // Parse arguments, fallback to empty object if invalid
                    let arguments = serde_json::from_str(&entry.arguments)
                        .unwrap_or_else(|_| serde_json::json!({}));
                    return Some(ContentBlock::ToolCall {
                        id,
                        name,
                        arguments,
                    });
                }
            }
        }
        None
    }
}

impl Default for ToolCallAccumulator {
    fn default() -> Self {
        Self::new()
    }
}

/// API format adapter trait
///
/// Implementations convert between unified internal types and provider-specific
/// request/response formats. Each adapter is stateless and can be cheaply cloned.
pub trait ApiAdapter: Send + Sync {
    /// Provider name (e.g., "openai", "anthropic")
    fn name(&self) -> &str;

    /// Default model for this provider
    fn default_model(&self) -> &str;

    /// Base URL for API requests (without trailing slash)
    fn base_url(&self) -> &str;

    /// Build request for chat completion
    ///
    /// Returns (path, body) where path is the API endpoint (e.g., "/chat/completions")
    fn build_request(
        &self,
        messages: &[Message],
        tools: Option<&[ToolDefinition]>,
        options: &ChatOptions,
        stream: bool,
    ) -> Result<(String, Value)>;

    /// Parse non-streaming response into unified format
    fn parse_response(&self, response: Value) -> Result<ChatResponse>;

    /// Parse SSE event data into unified stream event
    ///
    /// Returns None if the event should be skipped (e.g., keep-alive)
    fn parse_sse_event(&self, data: &str) -> Result<Option<StreamEvent>>;

    /// Get authentication configuration
    fn auth_config(&self, api_key: &str) -> AuthConfig;

    /// Get extra headers to add to requests (e.g., anthropic-version)
    fn extra_headers(&self) -> Vec<(String, String)> {
        vec![]
    }

    /// Check if this provider supports native tool calling
    fn supports_native_tools(&self) -> bool {
        true
    }
}

/// Helper function to convert unified `MessageRole` to string
fn role_to_string(role: MessageRole) -> &'static str {
    match role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
    }
}

/// Helper function to extract text from content blocks
fn extract_text_content(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

/// Type-erased adapter enum for concrete provider cores.
///
/// Delegates all `ApiAdapter` methods to the underlying adapter variant,
/// allowing `Provider` to be a single concrete type instead of generic.
#[derive(Debug, Clone)]
pub enum AnyAdapter {
    OpenAi(OpenAiAdapter),
    Anthropic(AnthropicAdapter),
    OpenAiCompatible(OpenAiCompatibleAdapter),
}

impl ApiAdapter for AnyAdapter {
    fn name(&self) -> &str {
        match self {
            Self::OpenAi(a) => a.name(),
            Self::Anthropic(a) => a.name(),
            Self::OpenAiCompatible(a) => a.name(),
        }
    }

    fn default_model(&self) -> &str {
        match self {
            Self::OpenAi(a) => a.default_model(),
            Self::Anthropic(a) => a.default_model(),
            Self::OpenAiCompatible(a) => a.default_model(),
        }
    }

    fn base_url(&self) -> &str {
        match self {
            Self::OpenAi(a) => a.base_url(),
            Self::Anthropic(a) => a.base_url(),
            Self::OpenAiCompatible(a) => a.base_url(),
        }
    }

    fn build_request(
        &self,
        messages: &[Message],
        tools: Option<&[ToolDefinition]>,
        options: &ChatOptions,
        stream: bool,
    ) -> Result<(String, Value)> {
        match self {
            Self::OpenAi(a) => a.build_request(messages, tools, options, stream),
            Self::Anthropic(a) => a.build_request(messages, tools, options, stream),
            Self::OpenAiCompatible(a) => a.build_request(messages, tools, options, stream),
        }
    }

    fn parse_response(&self, response: Value) -> Result<ChatResponse> {
        match self {
            Self::OpenAi(a) => a.parse_response(response),
            Self::Anthropic(a) => a.parse_response(response),
            Self::OpenAiCompatible(a) => a.parse_response(response),
        }
    }

    fn parse_sse_event(&self, data: &str) -> Result<Option<StreamEvent>> {
        match self {
            Self::OpenAi(a) => a.parse_sse_event(data),
            Self::Anthropic(a) => a.parse_sse_event(data),
            Self::OpenAiCompatible(a) => a.parse_sse_event(data),
        }
    }

    fn auth_config(&self, api_key: &str) -> AuthConfig {
        match self {
            Self::OpenAi(a) => a.auth_config(api_key),
            Self::Anthropic(a) => a.auth_config(api_key),
            Self::OpenAiCompatible(a) => a.auth_config(api_key),
        }
    }

    fn extra_headers(&self) -> Vec<(String, String)> {
        match self {
            Self::OpenAi(a) => a.extra_headers(),
            Self::Anthropic(a) => a.extra_headers(),
            Self::OpenAiCompatible(a) => a.extra_headers(),
        }
    }

    fn supports_native_tools(&self) -> bool {
        match self {
            Self::OpenAi(a) => a.supports_native_tools(),
            Self::Anthropic(a) => a.supports_native_tools(),
            Self::OpenAiCompatible(a) => a.supports_native_tools(),
        }
    }
}
