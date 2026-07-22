//! Provider contract — wire-agnostic types shared by every provider
//! adapter, the catalog, the resolver, the metered wrapper, and the
//! agentic loop.
//!
//! This crate is intentionally a thin types layer. The concrete
//! `Provider` implementation, HTTP client, and per-adapter request
//! builders all live in the root `peko` crate today; a later phase
//! will extract those into `peko-providers`. The boundary rule for
//! `peko-provider-api` is:
//!
//! * **may** depend on `peko-message` (for `ContentBlock`, `LlmMessage`,
//!   `MessageRole`, `TokenUsage`).
//! * **must not** depend on any concrete adapter (`adapters::*`),
//!   any HTTP transport, `peko-engine`, `peko-tools-builtin`, the
//!   extension host, the daemon, the CLI, or session internals.
//!
//! Adding a new wire-shaped value type? Place it in `traits.rs` if
//! the call site spans adapters + catalogs + the agentic loop;
//! place it in a sibling submodule if it belongs to a single wire
//! format's vocabulary.
//!
//! Pure value-level helpers used by the agentic loop that don't
//! depend on transport or concrete adapters (Phase 9b.N.5b.5 lifted
//! them here from `src/providers/*` so the loop can move into
//! `peko-engine` without dragging the root crate's concrete impls):
//!
//! * `clamp_openai_prompt_cache_key` — OpenAI's 64-UTF-32-char
//!   prompt-cache-key cap. See `prompt_cache.rs`.
//! * `is_context_window_exceeded` — pure bool classifier over
//!   `anyhow::Error` for F22's front-evict + retry path.
//!   See `context_window_error.rs`.

pub mod cache_retention;
pub mod context_window_error;
pub mod prompt_cache;
pub mod traits;

// Re-export the agentic message contract so adapter code can write
// `use peko_provider_api::{ContentBlock, LlmMessage, ...}` without
// needing an extra `use peko_message::...` line.
pub use peko_message::{ContentBlock, LlmMessage, MessageRole, TokenUsage};

// Re-export every wire-shape type so consumers can use the flat
// (`peko_provider_api::ChatOptions`) or submodule
// (`peko_provider_api::traits::ChatOptions`) form interchangeably.
pub use cache_retention::CacheRetention;
pub use context_window_error::is_context_window_exceeded;
pub use prompt_cache::clamp_openai_prompt_cache_key;
pub use traits::{
    BlockType, ChatOptions, ChatResponse, ContentBlockId, ContentDelta, DeferredToolsMode,
    ProviderCompat, ServiceTier, StopReason, StreamEvent, ThinkingEffort, ThinkingFormat,
    ThinkingKeep, ToolChoice, ToolDefinition,
};
