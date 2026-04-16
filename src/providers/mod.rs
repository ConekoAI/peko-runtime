//! LLM Providers
//!
//! Provider architecture with clean separation of concerns:
//! - **Types** (`types`): Unified internal representation
//! - **Transport** (`transport`): HTTP client and SSE parsing
//! - **Adapters** (`adapters`): Provider-specific API format conversion
//! - **Core** (`core`): Unified provider implementation
//! - **Registry** (`registry`): Provider metadata and factory
//!
//! Adding a new provider:
//! 1. If OpenAI-compatible: Add entry to registry with base URL
//! 2. If unique API: Implement `ApiAdapter` trait

pub mod adapters;
pub mod core;
pub mod registry;
pub mod transport;
pub mod types;

// Re-export commonly used types
pub use adapters::{
    AnthropicAdapter, AnyAdapter, ApiAdapter, OpenAiAdapter, OpenAiCompatibleAdapter,
};
pub use core::Provider;
pub use registry::{create_provider, get_provider_metadata, list_providers, ProviderRegistry};
pub use transport::{AuthConfig, HttpClient, SseParser};
pub use types::{
    BlockType, ChatMessage, ChatOptions, ChatResponse, ContentBlock, ContentBlockId, ContentDelta,
    Message, MessageRole, ProviderConfig, StopReason, StreamEvent, ThinkingBlock, TokenUsage,
    ToolCallBlock, ToolDefinition,
};

// Types still defined in traits.rs for historical reasons; imported via types::*
pub mod traits;
