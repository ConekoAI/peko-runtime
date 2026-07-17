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
pub mod catalog;
pub mod core;
pub mod factory;
pub mod metered;
pub mod mock;
pub mod resolver;
pub mod stacked_metered;
pub mod synthetic_stream;
pub mod templates;
pub mod transport;
pub mod validator;

// Re-export commonly used types
pub use adapters::{
    AnthropicAdapter, AnyAdapter, ApiAdapter, OpenAiAdapter, OpenAiCompatibleAdapter,
};
pub use catalog::{ApiFormat, ModelCatalog, ModelCatalogFile, ModelConfig};
pub use core::{Provider, ProviderRuntimeOptions};
pub use factory::create_provider_for_model;
pub use metered::MeteredProvider;
pub use mock::{MockAdapter, MockResponse};
pub use resolver::{KeyProbeReport, LlmResolver, ResolveRequest, ResolveSource, ResolvedChoice};
pub use stacked_metered::StackedMeteredProvider;
pub use templates::{find_template, iter_templates, ModelTemplate, ProviderTemplate};
pub use transport::{AuthConfig, HttpClient, SseParser};
// Domain types (canonical source: `common::types::message`)
pub use crate::common::types::message::{ContentBlock, LlmMessage, MessageRole, TokenUsage};
// Provider interface types (canonical source: `providers::traits`)
pub use traits::{
    BlockType, ChatOptions, ChatResponse, ContentBlockId, ContentDelta, StopReason, StreamEvent,
    ToolDefinition,
};

// Types still defined in traits.rs for historical reasons; imported via traits::*
pub mod traits;

/// Fallback for `ChatOptions::max_tokens` when neither the caller nor
/// the catalog supplies a value.
///
/// 4096 fits the lower bound of every Anthropic and OpenAI model that
/// supports tool use. The preferred source is
/// `ProviderCatalog::model_max_output_tokens` (when wired into the
/// caller) or `ModelInfo::max_output_tokens` from the catalog
/// directly. This constant exists so the bare `4096` literal does not
/// drift across `ChatOptions` construction sites.
///
/// See `crate::providers::catalog::ProviderCatalog::model_context_length`
/// for the analogous context-length defaulting pattern (PR-B / F15).
pub const DEFAULT_MAX_OUTPUT_TOKENS: u32 = 4096;
