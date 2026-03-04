//! LLM Providers
//!
//! Provider architecture:
//! - **Base implementations**: OpenAI and Anthropic handle actual API calls
//! - **Registry**: Maps provider names to metadata (URL, auth, etc.)
//! - **Factory**: Routes to appropriate base implementation
//!
//! This means adding a new provider = adding a registry entry,
//! not a new file. 90% of providers are OpenAI-compatible.

pub mod anthropic;
pub mod openai;
pub mod registry;
pub mod sse;
pub mod traits;

// Legacy providers (to be removed after migration)
pub mod kimi;
pub mod kimi_code;

pub use anthropic::{AnthropicConfig, AnthropicProvider};
pub use openai::{OpenAIConfig, OpenAIProvider};
pub use registry::{create_provider, get_provider_metadata, list_providers, ProviderRegistry};
pub use sse::{parse_sse_line, SseEvent, SseParser};
pub use traits::{
    ChatMessage, ChatOptions, ChatResponse, MessageRole, Provider, StopReason, StreamEvent,
    TokenUsage, ToolDefinition,
};
