//! Compatibility shim: implements `peko_engine::ProviderView` for
//! root's `crate::providers::Provider` so the agentic loop (Phase 9b.N.5b.8,
//! moving into `peko-engine`) can use the trait-object view without
//! naming the concrete root type.
//!
//! # Trait port rationale
//!
//! `ProviderView` (defined in `peko_engine::provider_view`) is a narrow
//! trait port that exposes ONLY the methods the agentic loop calls today:
//!
//! - `name()`, `model_id()`, `context_window()` — used at lines 860, 197+861, 884
//! - `supports_native_tools()`, `supports_prompt_cache_control()` — lines 2100, 1202
//! - `chat_with_tools()`, `stream_with_tools()` — lines 2119, 2111
//!
//! The trait deliberately omits `inner()` (the loop's line-893 `provider.inner().clone()`
//! construction site for `BackgroundCompactor` is routed through the sibling
//! `BackgroundCompactorFactory` port at `crates/engine/src/compaction/factory.rs`).
//!
//! The impl lives here (not in `peko-engine`) because of the orphan rule:
//! `peko_engine::ProviderView` is a foreign trait, and
//! `crate::providers::Provider` is a root-only type. The blanket
//! `impl ProviderView for Provider` form is allowed because `Provider` is
//! local to root (see the orphan rule's "local type before any uncovered
//! type parameter" clause).
//!
//! # Trait port lifetime
//!
//! The trait port mirrors the pattern established by Phase 9b.N.1
//! (`AsyncCompletionLike`), 9b.N.2 (`ToolFunnel`), 9b.N.3 (`SessionView`),
//! 9b.N.5a (`AgentView`). It disappears when a later phase lifts
//! `Provider` into a `peko-providers` crate (deferred per Phase 6's note).
//!
//! Module location: rooted at `src/engine/provider_view_compat.rs` so
//! `src/engine/mod.rs` declares it via `pub mod`, mirroring the
//! `src/engine/session_view_compat.rs` and
//! `src/engine/extension_core_funnel_compat.rs` patterns.

use crate::providers::Provider;
use anyhow::Result;
use futures::Stream;
use peko_engine::ProviderView;
use peko_message::LlmMessage;
use peko_provider_api::{ChatOptions, ChatResponse, StreamEvent, ToolDefinition};
use std::pin::Pin;

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
