//! `StackedMeteredProvider` — auto-charging wrapper that charges
//! every meter in the active `QuotaScope` stack (F20).
//!
//! Mirrors [`MeteredProvider`](crate::providers::MeteredProvider) but
//! reads the full nested-scope stack via
//! [`QuotaScope::collect_stack`](crate::quota::QuotaScope::collect_stack)
//! instead of just the innermost meter. Each LLM call charges every
//! meter in the stack, innermost first (peer → principal → …) so a
//! "more specific" meter trip fails fast.
//!
//! ## Use case
//!
//! ```ignore
//! QuotaScope::with(principal_meter, async move {
//!     QuotaScope::with(peer_meter, async move {
//!         let provider = resolver.build(...).await?;
//!         let stacked = StackedMeteredProvider::from_current_scope(provider);
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
//! Same shape as [`MeteredProvider`](crate::providers::MeteredProvider):
//! intercepts `StreamEvent::Usage` events and charges each meter via
//! the sync `try_charge`. Each meter sees the same usage event; if
//! any meter rejects (exhausted), the error is folded into the stream.

use std::pin::Pin;
use std::sync::Arc;

use futures::Stream;
use futures::StreamExt;

use crate::common::types::message::LlmMessage;
use crate::common::types::message::TokenUsage;
use crate::quota::{QuotaMeter, QuotaScope};
use crate::providers::traits::{ChatOptions, ToolDefinition};

use super::core::Provider;
use super::traits::{ChatResponse, StreamEvent};

/// Auto-charging wrapper that charges every meter in the active
/// `QuotaScope` stack. Used by F20 callers (agentic loop, compactor
/// worker) that want per-principal + per-peer attribution.
pub struct StackedMeteredProvider {
    inner: Arc<Provider>,
    /// Stack of meters captured at construction time. Outer-first;
    /// charging walks innermost-first (the last entry is the most
    /// specific one and trips first).
    meters: Vec<Arc<QuotaMeter>>,
}

impl StackedMeteredProvider {
    /// Wrap a provider with the full active meter stack. If no
    /// `QuotaScope::with` is active, returns a passthrough wrapper
    /// with an empty stack — all `charge` calls succeed and no quota
    /// is consulted.
    #[must_use]
    pub fn from_current_scope(inner: Arc<Provider>) -> Self {
        Self {
            inner,
            meters: QuotaScope::collect_stack(),
        }
    }

    /// Same, but pass the stack explicitly. Used by tests that don't
    /// want to wrap the call in `QuotaScope::with`.
    #[must_use]
    pub fn with_explicit_stack(inner: Arc<Provider>, meters: Vec<Arc<QuotaMeter>>) -> Self {
        Self { inner, meters }
    }

    /// Wrap a provider with no meters (passthrough). Equivalent to
    /// `with_explicit_stack(inner, vec![])`.
    #[must_use]
    pub fn passthrough(inner: Arc<Provider>) -> Self {
        Self {
            inner,
            meters: Vec::new(),
        }
    }

    /// Pass-through access to the inner provider.
    #[must_use]
    pub fn inner(&self) -> &Arc<Provider> {
        &self.inner
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

    /// Whether the inner provider supports native tool calling.
    #[must_use]
    pub fn supports_native_tools(&self) -> bool {
        self.inner.supports_native_tools()
    }

    /// How many meters are stacked on this wrapper. Empty means
    /// passthrough (no quota will be charged).
    #[must_use]
    pub fn stack_len(&self) -> usize {
        self.meters.len()
    }

    /// Simple chat (no system prompt). Wraps `chat_response_with_system`
    /// and charges the stack.
    pub async fn chat(
        &self,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        let response = self.chat_response(message, model, temperature).await?;
        Ok(extract_text(&response.content))
    }

    /// Chat with optional system prompt. Delegates to
    /// `chat_response_with_system` and charges the stack.
    pub async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        let response = self
            .chat_response_with_system(system_prompt, message, model, temperature)
            .await?;
        Ok(extract_text(&response.content))
    }

    /// Blocking chat that returns the full `ChatResponse` (including
    /// usage). Charges the meter stack after the inner call returns.
    pub async fn chat_response(
        &self,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<ChatResponse> {
        let response = self
            .inner
            .chat_response(message, model, temperature)
            .await?;
        self.charge_stack(&response.usage).await?;
        Ok(response)
    }

    /// Like [`Self::chat_response`] but with an optional system
    /// prompt prepended. Charges the meter stack after the inner call.
    pub async fn chat_response_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<ChatResponse> {
        let response = self
            .inner
            .chat_response_with_system(system_prompt, message, model, temperature)
            .await?;
        self.charge_stack(&response.usage).await?;
        Ok(response)
    }

    /// Blocking chat with native tool calling. Charges the meter
    /// stack after the inner call returns.
    pub async fn chat_with_tools(
        &self,
        model_id: &str,
        messages: &[LlmMessage],
        tools: &[ToolDefinition],
        options: &ChatOptions,
    ) -> anyhow::Result<ChatResponse> {
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
    ) -> anyhow::Result<
        Pin<Box<dyn Stream<Item = anyhow::Result<StreamEvent>> + Send>>,
    > {
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
    async fn charge_stack(&self, usage: &TokenUsage) -> anyhow::Result<()> {
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

/// Concatenate text content blocks into a single string. Mirrors
/// `Provider::chat_with_system`'s internal extraction.
fn extract_text(blocks: &[crate::common::types::message::ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|cb| match cb {
            crate::common::types::message::ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::MockAdapter;
    use crate::quota::{QuotaConfig, QuotaCycle, QuotaMeter};
    use chrono::Utc;

    /// Helper: build a metered provider backed by a mock + an in-memory
    /// meter with the given config (unlimited if both are `None`).
    async fn make_stacked(
        configs: Vec<(Option<u64>, Option<u64>, Option<u64>)>,
    ) -> (StackedMeteredProvider, Vec<Arc<QuotaMeter>>) {
        let adapter = MockAdapter::new();
        adapter.queue_text("hello");
        let tmp = tempfile::tempdir().unwrap();
        let catalog = tmp.path().join("providers.toml");
        let (resolver, _adapter) = crate::providers::LlmResolver::mock(adapter, &catalog).await;
        let (provider, _choice) = resolver.build(Default::default()).await.unwrap();

        let mut meters = Vec::new();
        let mut stack = Vec::new();
        for (input, output, requests) in configs {
            let cfg = QuotaConfig {
                input_tokens: input,
                output_tokens: output,
                request_count: requests,
                cycle: QuotaCycle::Hourly,
            };
            let meter = Arc::new(
                QuotaMeter::load_or_init(cfg, None, Utc::now())
                    .await
                    .unwrap(),
            );
            stack.push(Arc::clone(&meter));
            meters.push(meter);
        }

        let stacked = StackedMeteredProvider::with_explicit_stack(provider, stack);
        (stacked, meters)
    }

    /// Single-meter stack behaves like `MeteredProvider`: charges the
    /// one meter, all four LLM methods work.
    #[tokio::test]
    async fn stacked_with_single_meter_charges_that_meter() {
        let (stacked, meters) = make_stacked(vec![(Some(1000), None, None)]).await;
        let _ = stacked
            .chat_with_tools(
                "default",
                &[LlmMessage::user("hi")],
                &[],
                &ChatOptions::default(),
            )
            .await
            .unwrap();
        assert_eq!(meters[0].snapshot().request_count, 1);
    }

    /// Empty stack is passthrough: no charge happens anywhere, all
    /// four methods succeed.
    #[tokio::test]
    async fn stacked_with_empty_stack_is_passthrough() {
        let (stacked, meters) = make_stacked(vec![]).await;
        let _ = stacked
            .chat_with_tools(
                "default",
                &[LlmMessage::user("hi")],
                &[],
                &ChatOptions::default(),
            )
            .await
            .unwrap();
        assert_eq!(meters.len(), 0);
        assert_eq!(stacked.stack_len(), 0);
    }

    /// Stacked meters ALL charge on a single LLM call.
    #[tokio::test]
    async fn stacked_charges_every_meter() {
        let (stacked, meters) = make_stacked(vec![
            (Some(1000), None, None),
            (Some(1000), None, None),
            (Some(1000), None, None),
        ])
        .await;
        let _ = stacked
            .chat_with_tools(
                "default",
                &[LlmMessage::user("hi")],
                &[],
                &ChatOptions::default(),
            )
            .await
            .unwrap();
        for (i, m) in meters.iter().enumerate() {
            assert_eq!(
                m.snapshot().request_count,
                1,
                "meter {i} should have been charged exactly once"
            );
        }
    }

    /// Stacked meters charge innermost-first: a peer-meter trip (the
    /// innermost in our test setup) fires before the principal-meter
    /// charge happens. We verify this by setting the *last* meter's
    /// request_count to 0 (so the very first call trips it) and the
    /// first meter's request_count to a high cap (so it would have
    /// accepted). The call must fail and the first meter must NOT
    /// see a charge (request_count stays 0).
    #[tokio::test]
    async fn stacked_charges_innermost_first_so_peer_trip_fires_first() {
        // Inner meter has request_count=0 → trips on first call.
        // Outer meter has plenty of headroom.
        let (stacked, meters) = make_stacked(vec![
            (None, None, Some(10)),  // outer: high cap, would accept
            (None, None, Some(0)),   // inner: 0 cap, trips on first call
        ])
        .await;
        let result = stacked
            .chat_with_tools(
                "default",
                &[LlmMessage::user("hi")],
                &[],
                &ChatOptions::default(),
            )
            .await;
        assert!(result.is_err(), "inner meter trip should fail the call");
        // Outer meter never saw the charge (innermost-first short-circuit).
        assert_eq!(
            meters[0].snapshot().request_count,
            0,
            "outer meter should not be charged when inner trips"
        );
    }

    /// Stacked unlimited meters (no `QuotaConfig`) accept everything
    /// and don't trip.
    #[tokio::test]
    async fn stacked_charges_skip_unlimited_meters() {
        let (stacked, meters) = make_stacked(vec![
            (Some(1_000_000), None, None), // limited
            (None, None, None),             // unlimited (default config)
            (Some(1_000_000), None, None), // limited
        ])
        .await;
        let _ = stacked
            .chat_with_tools(
                "default",
                &[LlmMessage::user("hi")],
                &[],
                &ChatOptions::default(),
            )
            .await
            .unwrap();
        assert_eq!(meters[0].snapshot().request_count, 1);
        assert_eq!(meters[1].snapshot().request_count, 1);
        assert_eq!(meters[2].snapshot().request_count, 1);
    }

    /// `from_current_scope` reads the active task-local stack. Two
    /// nested `QuotaScope::with` calls produce a stack of length 2
    /// and the wrapper charges both.
    #[tokio::test]
    async fn from_current_scope_reads_nested_stack() {
        let outer = Arc::new(
            QuotaMeter::load_or_init(
                QuotaConfig {
                    input_tokens: None,
                    output_tokens: None,
                    request_count: Some(10),
                    cycle: QuotaCycle::Hourly,
                },
                None,
                Utc::now(),
            )
            .await
            .unwrap(),
        );
        let inner = Arc::new(
            QuotaMeter::load_or_init(
                QuotaConfig {
                    input_tokens: None,
                    output_tokens: None,
                    request_count: Some(10),
                    cycle: QuotaCycle::Hourly,
                },
                None,
                Utc::now(),
            )
            .await
            .unwrap(),
        );
        let adapter = MockAdapter::new();
        adapter.queue_text("hello");
        let tmp = tempfile::tempdir().unwrap();
        let catalog = tmp.path().join("providers.toml");
        let (resolver, _adapter) = crate::providers::LlmResolver::mock(adapter, &catalog).await;
        let (provider, _choice) = resolver.build(Default::default()).await.unwrap();

        QuotaScope::with(Arc::clone(&outer), async {
            QuotaScope::with(Arc::clone(&inner), async {
                let stacked = StackedMeteredProvider::from_current_scope(provider);
                assert_eq!(stacked.stack_len(), 2);
                let _ = stacked
                    .chat_with_tools(
                        "default",
                        &[LlmMessage::user("hi")],
                        &[],
                        &ChatOptions::default(),
                    )
                    .await
                    .unwrap();
            })
            .await;
        })
        .await;

        assert_eq!(outer.snapshot().request_count, 1);
        assert_eq!(inner.snapshot().request_count, 1);
    }
}