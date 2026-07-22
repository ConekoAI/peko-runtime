//! `ProviderView` — narrow engine-facing surface of root's `crate::providers::Provider`.
//!
//! Phase 9b.N.5b.7 introduces this trait port to break `agentic_loop.rs`'s
//! direct borrow of `Arc<crate::providers::Provider>`. The trait exposes ONLY
//! the methods the loop actually calls today — the full `Provider` type is
//! root-only and lifting it would drag `AnyAdapter`, `HttpClient`, the
//! catalog/resolver, and the auth-rotation logic into the engine crate, all
//! of which depend on root-only types.
//!
//! This follows the same transient-scaffolding pattern as
//! [[workspace-phase9b-n4-compaction]]'s `CompactorBackend` trait, 9b.N.1's
//! `AsyncInboxLike`, 9b.N.2's `ToolFunnel`, 9b.N.3's `SessionView`, and
//! `AgentView` from 9b.N.5a. When `Provider` itself eventually lifts into a
//! `peko-providers` crate (deferred per Phase 6), this trait disappears
//! and the loop holds a direct `Arc<Provider>` again.
//!
//! # Why these methods
//!
//! Each method has exactly one consumer today: the
//! `src/engine/agentic_loop.rs` field access / method call. Sources:
//!
//! | Method | Used at |
//! |--------|---------|
//! | `name()` | lines 860, 2122 |
//! | `model_id()` | lines 197, 861, 1989, 4409, 4422 |
//! | `context_window()` | line 884 |
//! | `supports_prompt_cache_control()` | line 1202 |
//! | `supports_native_tools()` | line 2100 |
//! | `chat_with_tools()` | lines 2073, 2119 |
//! | `stream_with_tools()` | lines 2073, 2111 |
//!
//! **Note on `inner()`:** the loop currently calls `provider.inner().clone()`
//! at line 893 to feed `BackgroundCompactor::new(...)`. That escape hatch
//! is **NOT** part of this trait — instead, the construction site routes
//! through the sibling `BackgroundCompactorFactory` port
//! (`crates/engine/src/compaction/factory.rs`), and the factory's root impl
//! captures the inner provider from its own state. The trait stays free of
//! `crate::providers::Provider` so `peko-engine` doesn't reach back into
//! root (preserving the dep-graph Rule 11 providers→engine ban in reverse).
//!
//! # Orphan-rule design
//!
//! The trait lives in `peko-engine` and references only `peko_provider_api`
//! value types (`ChatOptions`, `ChatResponse`, `StreamEvent`, `ToolDefinition`)
//! + `peko_message::LlmMessage`. The impl lives at root via
//! `src/engine/provider_view_compat.rs`
//! (`impl ProviderView for crate::providers::Provider`) — the orphan rule
//! allows it because `crate::providers::Provider` is a root-local type.
//!
//! Following [[prefer-concrete-over-speculative-abstraction]]: trait stays
//! narrow until a second consumer appears.

use anyhow::Result;
use futures::Stream;
use peko_message::LlmMessage;
use peko_provider_api::{ChatOptions, ChatResponse, StreamEvent, ToolDefinition};
use std::pin::Pin;

/// Narrow engine-facing view of root's `crate::providers::Provider`.
///
/// Implemented by `crate::providers::Provider` via
/// `src/engine/provider_view_compat.rs` (orphan-rule-friendly — `Provider` is
/// root-only, so the impl lives in root). The loop holds
/// `Arc<dyn ProviderView>` after Phase 9b.N.5b.7 instead of
/// `Arc<crate::providers::Provider>`.
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
