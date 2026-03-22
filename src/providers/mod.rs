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

pub mod moonshot;

// Legacy provider (deprecated, will be removed)
#[deprecated(
    since = "0.9.0",
    note = "Use AnthropicProvider or OpenAICompatibleProvider::moonshot() instead"
)]
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

// Backward compatibility: KimiProvider is now MoonshotProvider
#[deprecated(
    since = "0.9.0",
    note = "Use MoonshotProvider or OpenAICompatibleProvider::moonshot() instead"
)]
pub type KimiProvider = OpenAICompatibleProvider;
