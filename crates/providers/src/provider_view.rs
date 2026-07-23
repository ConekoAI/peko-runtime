//! `ProviderView` — narrow engine-facing surface of [`Provider`].
//!
//! Introduced in Phase 9b.N.5b.7 to break `agentic_loop.rs`'s direct
//! borrow of `Arc<Provider>`. The trait exposes ONLY the methods the
//! loop actually calls today, all of which forward straight to the
//! underlying `Provider` impl.
//!
//! Before Phase 6 lifted `Provider` into `peko-providers`, this trait
//! lived in `peko-engine` and the impl lived at root
//! (`src/engine/provider_view_compat.rs`). The orphan rule allowed the
//! impl because `Provider` was then root-local. Now that `Provider` is
//! a `peko-providers` type, the orphan rule forbids an `impl
//! ProviderView for Provider` from any other crate, so the trait moves
//! here next to `Provider` and the impl lives next to both.
//!
//! `peko-engine` keeps a back-compat re-export
//! (`peko_engine::ProviderView`) so the many `Arc<dyn ProviderView>`
//! sites that pre-date Phase 6 don't need to be rewritten. New callers
//! should prefer the canonical `peko_providers::ProviderView` path.
//!
//! `#[async_trait::async_trait]` is required so the trait is
//! `dyn`-compatible (native `async fn` in traits is not yet object-safe
//! in stable Rust; the `async_trait` macro rewrites async methods into
//! `Pin<Box<dyn Future>>` returning forms, which ARE object-safe). The
//! `Arc<dyn ProviderView>` field type at
//! `crates/engine/src/stacked_metered_provider.rs:71` requires this.

use anyhow::Result;
use futures::Stream;
use peko_message::LlmMessage;
use peko_provider_api::{ChatOptions, ChatResponse, StreamEvent, ToolDefinition};
use std::pin::Pin;

use crate::core::Provider;

/// Narrow engine-facing view of [`Provider`].
///
/// The loop holds `Arc<dyn ProviderView>` after Phase 9b.N.5b.7 instead
/// of `Arc<Provider>`. The trait describes the methods `peko-engine`
/// (specifically `agentic_loop.rs`, `stacked_metered_provider.rs`, and
/// `compaction/factory.rs`) needs to call on a provider without naming
/// the concrete root type.
#[async_trait::async_trait]
pub trait ProviderView: Send + Sync + 'static {
    /// Provider name (e.g. `"anthropic"`, `"openai"`). Used for hook payload
    /// attribution + `StreamEvent::Start { provider }` at line 2122.
    fn name(&self) -> &str;

    /// Default model id (the provider's baked-in default, NOT the resolver's
    /// per-call override). Field access at `agentic_loop.rs:197` —
    /// `provider.model_id()`.
    fn model_id(&self) -> String;

    /// Maximum context window in tokens (None ⇒ caller uses a fallback).
    /// Field access at `agentic_loop.rs:884` —
    /// `provider.context_window().map(|n| n as usize)`.
    fn context_window(&self) -> Option<u32>;

    /// Whether the provider emits native tool-calling blocks (vs requiring
    /// `synthesize_stream_from_blocking`). Field access at
    /// `agentic_loop.rs:2100` — `provider.supports_native_tools()`.
    fn supports_native_tools(&self) -> bool;

    /// Whether the provider supports prompt-cache markers
    /// (`cache_control`, `prompt_cache_key`). Field access at
    /// `agentic_loop.rs:1202` — `provider.supports_prompt_cache_control()`.
    fn supports_prompt_cache_control(&self) -> bool;

    /// Blocking chat that returns the full `ChatResponse` (including usage).
    /// Used at line 2119 (`provider.chat_with_tools(...)`) when the provider
    /// doesn't support streaming — the loop synthesizes a stream from the
    /// blocking response.
    async fn chat_with_tools(
        &self,
        model_id: &str,
        messages: &[LlmMessage],
        tools: &[ToolDefinition],
        options: &ChatOptions,
    ) -> Result<ChatResponse>;

    /// Streaming chat. Used at line 2111 (`provider.stream_with_tools(...)`)
    /// when the provider supports native streaming.
    async fn stream_with_tools(
        &self,
        model_id: &str,
        messages: &[LlmMessage],
        tools: &[ToolDefinition],
        options: &ChatOptions,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>>;
}

#[async_trait::async_trait]
impl ProviderView for Provider {
    fn name(&self) -> &str {
        Provider::name(self)
    }

    fn model_id(&self) -> String {
        Provider::model_id(self)
    }

    fn context_window(&self) -> Option<u32> {
        Provider::context_window(self)
    }

    fn supports_native_tools(&self) -> bool {
        Provider::supports_native_tools(self)
    }

    fn supports_prompt_cache_control(&self) -> bool {
        Provider::supports_prompt_cache_control(self)
    }

    async fn chat_with_tools(
        &self,
        model_id: &str,
        messages: &[LlmMessage],
        tools: &[ToolDefinition],
        options: &ChatOptions,
    ) -> Result<ChatResponse> {
        Provider::chat_with_tools(self, model_id, messages, tools, options).await
    }

    async fn stream_with_tools(
        &self,
        model_id: &str,
        messages: &[LlmMessage],
        tools: &[ToolDefinition],
        options: &ChatOptions,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>> {
        Provider::stream_with_tools(self, model_id, messages, tools, options).await
    }
}
