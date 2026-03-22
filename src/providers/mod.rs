//! LLM Providers
//!
//! Provider architecture:
//! - **Base implementations**: OpenAI-compatible and Anthropic handle actual API calls
//! - **Registry**: Maps provider names to metadata (URL, auth, etc.)
//! - **Factory**: Routes to appropriate base implementation
//!
//! This means adding a new provider = adding a registry entry,
//! not a new file. 90% of providers are OpenAI-compatible.

pub mod anthropic;
pub mod openai;
pub mod openai_compatible;
pub mod registry;
pub mod sse;
pub mod traits;

// Legacy providers (deprecated, will be removed)
#[deprecated(since = "0.9.0", note = "Use OpenAICompatibleProvider::moonshot() instead")]
pub mod kimi;
#[deprecated(since = "0.9.0", note = "Use AnthropicProvider or OpenAICompatibleProvider::moonshot() instead")]
pub mod kimi_code;

pub use anthropic::{AnthropicConfig, AnthropicProvider};
pub use openai::{OpenAIConfig, OpenAIProvider};
pub use openai_compatible::{OpenAICompatibleConfig, OpenAICompatibleProvider};
pub use registry::{create_provider, get_provider_metadata, list_providers, ProviderRegistry};
pub use sse::{parse_sse_line, SseEvent, SseParser};
pub use traits::{
    ChatMessage, ChatOptions, ChatResponse, MessageRole, Provider, StopReason, StreamEvent,
    TokenUsage, ToolDefinition,
};
