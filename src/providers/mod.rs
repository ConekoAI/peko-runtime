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

// Re-export commonly used types from new architecture
pub use adapters::{AnthropicAdapter, ApiAdapter, OpenAiAdapter, OpenAiCompatibleAdapter};
pub use core::ProviderCore;
pub use registry::{create_provider, get_provider_metadata, list_providers, ProviderRegistry};
pub use transport::{AuthConfig, HttpClient, SseParser};
pub use types::{
    BlockType, ChatOptions, ChatResponse, ContentBlock, ContentBlockId, ContentDelta, Message,
    MessageRole, ProviderConfig, StopReason, StreamEvent, ThinkingBlock, TokenUsage, ToolCallBlock,
    ToolDefinition,
};

// Legacy trait imports for backward compatibility during migration
pub use crate::providers::traits::{
    ChatMessage, ChatOptions as LegacyChatOptions, ChatResponse as LegacyChatResponse,
    MessageRole as LegacyMessageRole, Provider, StopReason as LegacyStopReason,
    StreamEvent as LegacyStreamEvent, TokenUsage as LegacyTokenUsage,
    ToolDefinition as LegacyToolDefinition,
};

// Keep legacy traits module for compatibility
pub mod traits;

// Re-export legacy providers that haven't been migrated yet
// These will be removed once fully migrated to new architecture
pub use crate::providers::traits::{
    ChatMessage as ProviderChatMessage, ChatOptions as ProviderChatOptions,
    ChatResponse as ProviderChatResponse, TokenUsage as ProviderTokenUsage,
    ToolDefinition as ProviderToolDefinition,
};
