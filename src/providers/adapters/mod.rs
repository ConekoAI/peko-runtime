//! API adapters - convert between unified types and provider-specific formats
//!
//! Each adapter handles the specific JSON schema and behavior of one API type:
//! - OpenAI: Chat Completions API
//! - Anthropic: Messages API
//! - OpenAI-Compatible: Same as OpenAI with different base URL

use crate::providers::transport::AuthConfig;
use crate::providers::types::*;
use anyhow::Result;
use serde_json::Value;

pub mod anthropic;
pub mod compat;
pub mod openai;

pub use anthropic::AnthropicAdapter;
pub use compat::OpenAiCompatibleAdapter;
pub use openai::OpenAiAdapter;

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

/// Helper function to convert unified MessageRole to string
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
