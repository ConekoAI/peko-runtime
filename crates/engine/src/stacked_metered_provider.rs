//! `StackedMeteredProvider` — auto-charging wrapper that charges
//! every meter in the active `QuotaScope` stack (F20).
//!
//! Phase 9b.N.5b.8 lifts this from `crate::providers::StackedMeteredProvider`
//! (root-only) into `peko-engine` and refactors it to wrap
//! `Arc<dyn ProviderView>` instead of `Arc<crate::providers::Provider>`.
//! The trait port (Phase 9b.N.5b.7) is the engine-facing view of the
//! provider, so wrapping the trait object is the natural choice — the
//! wrapper doesn't need any of the root-only `Provider` methods
//! (`chat_response`, `chat_response_with_system`, `chat`, `chat_with_system`,
//! `inner`) that root still has.
//!
//! Mirrors [`MeteredProvider`](crate::providers::MeteredProvider) but
//! reads the full nested-scope stack via
//! [`QuotaScope::collect_stack`](peko_quota::QuotaScope::collect_stack)
//! instead of just the innermost meter. Each LLM call charges every
//! meter in the stack, innermost first (peer → principal → …) so a
//! "more specific" meter trip fails fast.
//!
//! ## Use case
//!
//! ```ignore
//! QuotaScope::with(principal_meter, async move {
//!     QuotaScope::with(peer_meter, async move {
//!         let view: Arc<dyn ProviderView> = ...;
//!         let stacked = StackedMeteredProvider::from_current_scope(view);
//!         stacked.chat_with_tools(...).await  // charges BOTH meters
//!     }).await
//! }).await
//! ```
//!
//! ## Charge order: innermost first
//!
//! The innermost meter is the most specific one for the current call
//! site (peer scope wraps principal scope). Failing fast on the most
//! specific dimension is the right UX — the peer's quota status is
//! the operator's most actionable signal.
//!
//! ## Streaming
//!
//! Intercepts `StreamEvent::Usage` events and charges each meter via
//! the sync `try_charge`. Each meter sees the same usage event; if
//! any meter rejects (exhausted), the error is folded into the stream.

use std::pin::Pin;
use std::sync::Arc;

use anyhow::Result;
use futures::Stream;
use futures::StreamExt;

use peko_message::{LlmMessage, TokenUsage};
use peko_provider_api::{ChatOptions, ChatResponse, StreamEvent, ToolDefinition};
use peko_quota::{QuotaMeter, QuotaScope};

use crate::provider_view::ProviderView;

/// Auto-charging wrapper that charges every meter in the active
/// `QuotaScope` stack. Used by F20 callers (agentic loop, compactor
/// worker) that want per-principal + per-peer attribution.
///
/// Phase 9b.N.5b.8: wraps `Arc<dyn ProviderView>` (engine-facing trait
/// port) instead of `Arc<crate::providers::Provider>` (root-only). The
/// methods dropped in this refactor (`chat_response`,
/// `chat_response_with_system`, `chat`, `chat_with_system`, `inner`)
/// were root-only `Provider` methods not part of the trait port —
/// their sole external caller (`src/session/compaction.rs:414`,
/// `BackgroundCompactor::summarize`) was updated to use
/// `chat_with_tools` with an empty tool list.
pub struct StackedMeteredProvider {
    inner: Arc<dyn ProviderView>,
    /// Stack of meters captured at construction time. Outer-first;
    /// charging walks innermost-first (the last entry is the most
    /// specific one and trips first).
    meters: Vec<Arc<QuotaMeter>>,
}

impl StackedMeteredProvider {
    /// Wrap a provider view with the full active meter stack. If no
    /// `QuotaScope::with` is active, returns a passthrough wrapper
    /// with an empty stack — all `charge` calls succeed and no quota
    /// is consulted.
    #[must_use]
    pub fn from_current_scope(inner: Arc<dyn ProviderView>) -> Self {
        Self {
            inner,
            meters: QuotaScope::collect_stack(),
        }
    }

    /// Same, but pass the stack explicitly. Used by tests that don't
    /// want to wrap the call in `QuotaScope::with`.
    #[must_use]
    pub fn with_explicit_stack(inner: Arc<dyn ProviderView>, meters: Vec<Arc<QuotaMeter>>) -> Self {
        Self { inner, meters }
    }

    /// Wrap a provider view with no meters (passthrough). Equivalent
    /// to `with_explicit_stack(inner, vec![])`.
    #[must_use]
    pub fn passthrough(inner: Arc<dyn ProviderView>) -> Self {
        Self {
            inner,
            meters: Vec::new(),
        }
    }

    /// Provider name (delegates to inner).
    #[must_use]
    pub fn name(&self) -> &str {
        self.inner.name()
    }

    /// Default model id (delegates to inner).
    #[must_use]
    pub fn model_id(&self) -> String {
        self.inner.model_id()
    }

    /// Context window (delegates to inner).
    #[must_use]
    pub fn context_window(&self) -> Option<u32> {
        self.inner.context_window()
    }

    /// Whether the inner provider supports native tool calling.
    #[must_use]
    pub fn supports_native_tools(&self) -> bool {
        self.inner.supports_native_tools()
    }

    /// Whether the inner provider supports prompt-cache markers.
    #[must_use]
    pub fn supports_prompt_cache_control(&self) -> bool {
        self.inner.supports_prompt_cache_control()
    }

    /// How many meters are stacked on this wrapper. Empty means
    /// passthrough (no quota will be charged).
    #[must_use]
    pub fn stack_len(&self) -> usize {
        self.meters.len()
    }

    /// Blocking chat with native tool calling. Charges the meter
    /// stack after the inner call returns.
    pub async fn chat_with_tools(
        &self,
        model_id: &str,
        messages: &[LlmMessage],
        tools: &[ToolDefinition],
        options: &ChatOptions,
    ) -> Result<ChatResponse> {
        let response = self
            .inner
            .chat_with_tools(model_id, messages, tools, options)
            .await?;
        self.charge_stack(&response.usage).await?;
        Ok(response)
    }

    /// Streaming chat with native tool calling. The returned stream
    /// is `inner`'s stream with each `StreamEvent::Usage` event
    /// intercepted: when the wrapper sees one, it charges every meter
    /// in the stack (innermost-first) and emits the event unchanged.
    /// If any meter rejects (exhausted), the error is folded into the
    /// stream as the next item — same behavior as
    /// [`MeteredProvider`](crate::providers::MeteredProvider).
    pub async fn stream_with_tools(
        &self,
        model_id: &str,
        messages: &[LlmMessage],
        tools: &[ToolDefinition],
        options: &ChatOptions,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>> {
        let inner_stream = self
            .inner
            .stream_with_tools(model_id, messages, tools, options)
            .await?;
        let meters = Arc::new(self.meters.clone());
        let metered_stream = Box::pin(inner_stream.map(move |event_result| {
            match event_result {
                Ok(StreamEvent::Usage {
                    input,
                    output,
                    total,
                    cache_creation_input_tokens,
                    cache_read_input_tokens,
                    reasoning_output_tokens,
                }) => {
                    let usage = TokenUsage {
                        input,
                        output,
                        total,
                        cache_creation_input_tokens: Some(cache_creation_input_tokens),
                        cache_read_input_tokens: Some(cache_read_input_tokens),
                        reasoning_output_tokens: Some(reasoning_output_tokens),
                    };
                    // Charge innermost-first so a peer trip fires
                    // before a principal trip. We `rev()` over the
                    // captured stack (which is outer-first per
                    // `QuotaScope::collect_stack`).
                    let mut first_error: Option<String> = None;
                    for meter in meters.iter().rev() {
                        if let Err(e) = meter.try_charge(&usage) {
                            first_error = Some(e.to_string());
                            break;
                        }
                    }
                    match first_error {
                        Some(msg) => Err(anyhow::anyhow!(msg)),
                        None => Ok(StreamEvent::Usage {
                            input,
                            output,
                            total,
                            cache_creation_input_tokens,
                            cache_read_input_tokens,
                            reasoning_output_tokens,
                        }),
                    }
                }
                other => other,
            }
        }));
        Ok(metered_stream)
    }

    /// Charge every meter in the stack, innermost-first. Returns the
    /// first rejection (peer trip) or `Ok(())` if every meter
    /// accepted. Unlimited meters (no `QuotaConfig`) accept any
    /// charge, so they never trip.
    async fn charge_stack(&self, usage: &TokenUsage) -> Result<()> {
        // Innermost is the last entry. Walk in reverse so a peer trip
        // surfaces before a principal charge is even attempted.
        for meter in self.meters.iter().rev() {
            if let Err(e) = meter.charge(usage).await {
                return Err(anyhow::anyhow!(e));
            }
        }
        Ok(())
    }
}
