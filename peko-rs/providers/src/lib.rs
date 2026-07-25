//! LLM Providers
//!
//! Provider architecture with clean separation of concerns:
//! - **Types** (`traits` re-exported from `peko-provider-api`):
//!   Unified internal representation.
//! - **Transport** (`transport`): HTTP client and SSE parsing.
//! - **Adapters** (`adapters`): Provider-specific API format conversion.
//! - **Core** (`core`): Unified provider implementation.
//! - **Catalog / Templates** (`catalog`, `templates`): On-disk model
//!   metadata and built-in provider templates.
//! - **Resolver** (`resolver`): `LlmResolver` plus rotation state for
//!   automatic 401-driven credential rotation.
//! - **Mock** (`mock`): `MockAdapter` for tests and CLI dry-runs.
//! - **Metered** (`metered`): `MeteredProvider` wrapper that charges
//!   the current `QuotaScope`.
//! - **Validator** (`validator`): Cheap authenticated probe of an
//!   API key without paying for a real chat call.
//! - **SecretStore** (`secret_store`): Read-side secret-store trait +
//!   `InMemorySecretStore` for tests; production impls (`VaultSecretStore`)
//!   live in the root composition layer.
//!
//! Adding a new provider:
//! 1. If OpenAI-compatible: Add entry to `templates` with a custom
//!    base URL.
//! 2. If unique API: Implement `peko_provider_api::ApiAdapter` and
//!    add a `factory::create_provider_for_model` arm.

pub mod adapters;
pub mod catalog;
pub mod core;
pub mod factory;
pub mod metered;
pub mod mock;
pub mod provider_view;
pub mod resolver;
pub mod rotating_auth;
pub mod secret_store;
pub mod templates;
pub mod transport;
pub mod validator;

// Re-export commonly used types so consumers can use the flat
// (`crate::LlmResolver`) form without an extra
// `crate::resolver::LlmResolver` segment.
pub use adapters::{
    AnthropicAdapter, AnyAdapter, ApiAdapter, OpenAiAdapter, OpenAiCompatibleAdapter,
};
pub use catalog::{ApiFormat, ModelCatalog, ModelCatalogFile, ModelConfig};
pub use core::{Provider, ProviderRuntimeOptions};
pub use factory::create_provider_for_model;
pub use metered::MeteredProvider;
pub use mock::{MockAdapter, MockResponse};
pub use provider_view::ProviderView;
pub use resolver::{KeyProbeReport, LlmResolver, ResolveRequest, ResolveSource, ResolvedChoice};
pub use rotating_auth::RotationState;
pub use secret_store::{InMemorySecretStore, SecretStore, SecretStoreError};
pub use templates::{find_template, iter_templates, ModelTemplate, ProviderTemplate};
pub use transport::{AuthConfig, HttpClient, SseParser};

// Domain types re-exported from `peko-message` and `peko-provider-api`
// so adapter code can pull them all from `crate::*` without
// extra imports.
pub use peko_message::{ContentBlock, LlmMessage, MessageRole, TokenUsage};
pub use peko_provider_api::{
    BlockType, CacheRetention, ChatOptions, ChatResponse, ContentBlockId, ContentDelta,
    CredentialError, CredentialMaterial, CredentialProvider, ProviderCompat, RotationEntry,
    ServiceTier, StopReason, StreamEvent, ThinkingEffort, ThinkingFormat, ThinkingKeep, ToolChoice,
    ToolDefinition, DEFAULT_MAX_OUTPUT_TOKENS,
};
