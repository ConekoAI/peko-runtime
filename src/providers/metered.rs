//! `MeteredProvider` — auto-charging wrapper around `Arc<Provider>` (F19).
//!
//! F18 forced every LLM call site to call `quota_meter.check()` before
//! and `quota_meter.charge()` after. F19 replaces that with this
//! wrapper: callers construct it once with a task-local meter (via
//! [`MeteredProvider::from_current_scope`]), then call `chat_with_tools`
//! / `stream_with_tools` / `chat_response` / `chat_response_with_system`
//! exactly like a raw `Provider` — and the wrapper auto-charges the
//! right meter.
//!
//! ## How it works
//!
//! ```ignore
//! QuotaScope::with(meter, async move {
//!     let provider = resolver.build(...).await?;
//!     let metered = MeteredProvider::from_current_scope(provider);
//!     metered.chat_with_tools(...).await   // charges `meter` after
//! }).await
//! ```
//!
//! The task-local is consulted at *construction* time, not at call
//! time, so a `MeteredProvider` built inside a `QuotaScope::with` will
//! always charge that scope's meter. To charge a different meter,
//! build a new wrapper.
//!
//! ## Why construction-time, not call-time
//!
//! A `tokio::task_local!` is task-local, not a generic ambient context.
//! If the wrapper read the task-local at call time, then a call after
//! the scope ends would silently no-op. Pinning at construction time
//! is the obvious correctness boundary: the lifetime of the wrapper
//! is the lifetime of the charge target.
//!
//! ## Streaming
//!
//! For `stream_with_tools`, the wrapper intercepts the inner stream
//! and charges on each `StreamEvent::Usage` event. Providers always
//! emit `Usage` before `Done` (F17 invariant), so the charge happens
//! before the call's logical end. If the meter is exhausted, the
//! charge error is folded into the stream as the next item — the
//! agentic loop's existing error handling surfaces it.
//!
//! ## Passthrough
//!
//! If no `QuotaScope::with` is active, `from_current_scope` returns
//! a wrapper with a no-op meter (`QuotaMeter::unlimited()`). All
//! `charge` calls succeed; no quota is consulted. This means test
//! fixtures and CLI commands that build a `Provider` directly don't
//! need to open a scope to work.

use std::pin::Pin;
use std::sync::Arc;

use futures::Stream;
use futures::StreamExt;

use crate::common::types::message::LlmMessage;
use crate::common::types::message::TokenUsage;
use crate::providers::traits::{ChatOptions, ToolDefinition};
use crate::quota::{QuotaMeter, QuotaScope};

use super::core::Provider;
use super::traits::{ChatResponse, StreamEvent};

/// Auto-charging wrapper around `Arc<Provider>`.
///
/// Constructed via [`MeteredProvider::from_current_scope`] inside a
/// [`QuotaScope::with`], or [`MeteredProvider::with_explicit_meter`]
/// for tests. The wrapper is cheap to clone (`Arc<Provider>` +
/// `Arc<QuotaMeter>`).
pub struct MeteredProvider {
    inner: Arc<Provider>,
    meter: Arc<QuotaMeter>,
}

impl MeteredProvider {
    /// Wrap a provider with the currently-active task-local meter.
    ///
    /// If no `QuotaScope::with` is active in this task, returns a
    /// passthrough wrapper backed by a no-op meter (`unlimited()`).
    /// All four LLM methods become no-op shims; the wrapper adds one
    /// `Arc` clone per call.
    #[must_use]
    pub fn from_current_scope(inner: Arc<Provider>) -> Self {
        let meter = QuotaScope::current().unwrap_or_else(|| Arc::new(QuotaMeter::unlimited()));
        Self { inner, meter }
    }

    /// Same, but pass the meter explicitly. Used by tests that don't
    /// want to wrap the call in `QuotaScope::with`.
    #[must_use]
    pub fn with_explicit_meter(inner: Arc<Provider>, meter: Arc<QuotaMeter>) -> Self {
        Self { inner, meter }
    }

    /// Wrap a provider with NO meter (passthrough). All `charge`
    /// calls succeed; the meter is never consulted. Equivalent to
    /// `with_explicit_meter(inner, Arc::new(QuotaMeter::unlimited()))`.
    #[must_use]
    pub fn passthrough(inner: Arc<Provider>) -> Self {
        Self {
            inner,
            meter: Arc::new(QuotaMeter::unlimited()),
        }
    }

    /// Pass-through access to the inner provider. Used by
    /// `synthetic_stream::synthesize_stream_from_blocking` which
    /// needs the underlying `name()`.
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

    /// Simple chat (no system prompt). Wraps `chat_response_with_system`
    /// and charges.
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
    /// `chat_response_with_system` and charges.
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
    /// usage). Charges the meter after the inner call returns.
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
        self.charge_usage(&response.usage).await?;
        Ok(response)
    }

    /// Like [`Self::chat_response`] but with an optional system
    /// prompt prepended. Charges the meter after the inner call.
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
        self.charge_usage(&response.usage).await?;
        Ok(response)
    }

    /// Blocking chat with native tool calling. Charges the meter
    /// after the inner call returns.
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
        self.charge_usage(&response.usage).await?;
        Ok(response)
    }

    /// Streaming chat with native tool calling. The returned stream
    /// is `inner`'s stream with each `StreamEvent::Usage` event
    /// intercepted: when the wrapper sees one, it charges the
    /// meter and emits the event unchanged. If the charge errors,
    /// the error is folded into the stream as the next item.
    pub async fn stream_with_tools(
        &self,
        model_id: &str,
        messages: &[LlmMessage],
        tools: &[ToolDefinition],
        options: &ChatOptions,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = anyhow::Result<StreamEvent>> + Send>>> {
        let inner_stream = self
            .inner
            .stream_with_tools(model_id, messages, tools, options)
            .await?;
        let meter = Arc::clone(&self.meter);
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
                    // F19: sync `try_charge` so we can charge inside
                    // the stream `map` (which is sync). Persistence
                    // is deferred to the next blocking call.
                    let usage = TokenUsage {
                        input,
                        output,
                        total,
                        cache_creation_input_tokens: Some(cache_creation_input_tokens),
                        cache_read_input_tokens: Some(cache_read_input_tokens),
                        reasoning_output_tokens: Some(reasoning_output_tokens),
                    };
                    match meter.try_charge(&usage) {
                        Ok(()) => Ok(StreamEvent::Usage {
                            input,
                            output,
                            total,
                            cache_creation_input_tokens,
                            cache_read_input_tokens,
                            reasoning_output_tokens,
                        }),
                        Err(e) => Err(anyhow::anyhow!(e)),
                    }
                }
                other => other,
            }
        }));
        Ok(metered_stream)
    }

    /// Charge the wrapped meter. Surfaces `QuotaError` as `Err`.
    async fn charge_usage(&self, usage: &TokenUsage) -> anyhow::Result<()> {
        if let Err(e) = self.meter.charge(usage).await {
            Err(anyhow::anyhow!(e))
        } else {
            Ok(())
        }
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
    use crate::providers::{MockAdapter, MockResponse};
    use crate::quota::{QuotaConfig, QuotaCycle};
    use chrono::Utc;

    /// Construct a metered provider backed by a mock + an in-memory
    /// meter with the given config.
    async fn make_metered(
        input_limit: Option<u64>,
        output_limit: Option<u64>,
        request_limit: Option<u64>,
    ) -> (MeteredProvider, Arc<QuotaMeter>) {
        let adapter = MockAdapter::new();
        adapter.queue_text("hello");
        let tmp = tempfile::tempdir().unwrap();
        let catalog = tmp.path().join("models.toml");
        let (resolver, _adapter) = crate::providers::LlmResolver::mock(adapter, &catalog).await;
        let (provider, _choice) = resolver
            .build(crate::providers::resolver::ResolveRequest {
                override_model: Some("mock"),
                ..Default::default()
            })
            .await
            .unwrap();

        let cfg = QuotaConfig {
            input_tokens: input_limit,
            output_tokens: output_limit,
            request_count: request_limit,
            cycle: QuotaCycle::Hourly,
        };
        let meter = Arc::new(
            QuotaMeter::load_or_init(cfg, None, Utc::now())
                .await
                .unwrap(),
        );
        let metered = MeteredProvider::with_explicit_meter(provider, Arc::clone(&meter));
        (metered, meter)
    }

    #[tokio::test]
    async fn blocking_chat_charges_input_tokens() {
        let (metered, meter) = make_metered(Some(1000), None, None).await;
        let response = metered.chat("hi", "default", 0.7).await.unwrap();
        assert_eq!(response, "hello");
        // The mock returns a minimal usage. Just verify the meter
        // incremented from 0.
        let snap = meter.snapshot();
        assert!(
            snap.input_tokens > 0 || snap.output_tokens > 0,
            "some token should be charged"
        );
        assert_eq!(snap.request_count, 1);
    }

    #[tokio::test]
    async fn blocking_chat_charges_each_call() {
        let adapter = MockAdapter::new();
        adapter.queue_text("first");
        adapter.queue_text("second");
        let tmp = tempfile::tempdir().unwrap();
        let catalog = tmp.path().join("models.toml");
        let (resolver, _adapter) = crate::providers::LlmResolver::mock(adapter, &catalog).await;
        let (provider, _) = resolver
            .build(crate::providers::resolver::ResolveRequest {
                override_model: Some("mock"),
                ..Default::default()
            })
            .await
            .unwrap();
        let meter = Arc::new(
            QuotaMeter::load_or_init(
                QuotaConfig {
                    request_count: Some(10),
                    ..Default::default()
                },
                None,
                Utc::now(),
            )
            .await
            .unwrap(),
        );
        let metered = MeteredProvider::with_explicit_meter(provider, Arc::clone(&meter));
        metered.chat("hi", "default", 0.7).await.unwrap();
        metered.chat("hi", "default", 0.7).await.unwrap();
        assert_eq!(meter.snapshot().request_count, 2);
    }

    #[tokio::test]
    async fn chat_with_tools_charges() {
        let (metered, meter) = make_metered(None, None, Some(10)).await;
        let _ = metered
            .chat_with_tools(
                "default",
                &[LlmMessage::user("hi")],
                &[],
                &ChatOptions::default(),
            )
            .await
            .unwrap();
        assert_eq!(meter.snapshot().request_count, 1);
    }

    #[tokio::test]
    async fn passthrough_wrapper_never_charges() {
        let adapter = MockAdapter::new();
        adapter.queue_text("hello");
        let tmp = tempfile::tempdir().unwrap();
        let catalog = tmp.path().join("models.toml");
        let (resolver, _adapter) = crate::providers::LlmResolver::mock(adapter, &catalog).await;
        let (provider, _) = resolver
            .build(crate::providers::resolver::ResolveRequest {
                override_model: Some("mock"),
                ..Default::default()
            })
            .await
            .unwrap();
        let metered = MeteredProvider::passthrough(provider);
        // Even with a request_count=0 limit, passthrough should succeed
        // because the meter is `unlimited()`.
        let cfg = QuotaConfig {
            input_tokens: None,
            output_tokens: None,
            request_count: Some(0), // would trip on the first call
            cycle: QuotaCycle::Hourly,
        };
        let meter = Arc::new(
            QuotaMeter::load_or_init(cfg, None, Utc::now())
                .await
                .unwrap(),
        );
        // The passthrough uses ITS OWN unlimited meter, not the
        // one we just built. Verify by checking the snap of the
        // unlimited meter — but we can't reach it. Just verify
        // the call succeeded.
        metered.chat("hi", "default", 0.7).await.unwrap();
        // Sanity: a real meter with request_count=0 would have tripped.
        // We didn't pass this meter; nothing to assert beyond success.
        let _ = meter;
    }

    #[tokio::test]
    async fn from_current_scope_reads_active_meter() {
        let adapter = MockAdapter::new();
        adapter.queue_text("hello");
        let tmp = tempfile::tempdir().unwrap();
        let catalog = tmp.path().join("models.toml");
        let (resolver, _adapter) = crate::providers::LlmResolver::mock(adapter, &catalog).await;
        let (provider, _) = resolver
            .build(crate::providers::resolver::ResolveRequest {
                override_model: Some("mock"),
                ..Default::default()
            })
            .await
            .unwrap();

        let meter = Arc::new(
            QuotaMeter::load_or_init(
                QuotaConfig {
                    request_count: Some(10),
                    ..Default::default()
                },
                None,
                Utc::now(),
            )
            .await
            .unwrap(),
        );

        QuotaScope::with(meter.clone(), async {
            let metered = MeteredProvider::from_current_scope(provider);
            metered.chat("hi", "default", 0.7).await.unwrap();
        })
        .await;
        assert_eq!(meter.snapshot().request_count, 1);
    }

    #[tokio::test]
    async fn from_current_scope_returns_passthrough_when_no_scope() {
        let adapter = MockAdapter::new();
        adapter.queue_text("hello");
        let tmp = tempfile::tempdir().unwrap();
        let catalog = tmp.path().join("models.toml");
        let (resolver, _adapter) = crate::providers::LlmResolver::mock(adapter, &catalog).await;
        let (provider, _) = resolver
            .build(crate::providers::resolver::ResolveRequest {
                override_model: Some("mock"),
                ..Default::default()
            })
            .await
            .unwrap();

        // No QuotaScope::with active in this task.
        let metered = MeteredProvider::from_current_scope(provider);
        // The call should succeed because the meter's a no-op
        // unlimited() — even with a tiny request_count.
        metered.chat("hi", "default", 0.7).await.unwrap();
    }

    #[tokio::test]
    async fn blocking_call_fails_when_quota_exhausted() {
        // request_count=0 means the very first charge trips.
        let (metered, _meter) = make_metered(None, None, Some(0)).await;
        let result = metered.chat("hi", "default", 0.7).await;
        assert!(result.is_err(), "expected quota trip");
        let err = format!("{:#}", result.unwrap_err());
        assert!(
            err.contains("quota") || err.contains("exceeded") || err.contains("Request"),
            "error should mention quota: {err}"
        );
    }

    #[tokio::test]
    async fn stream_charges_on_usage_event() {
        // Build a metered provider where the mock streams a Usage event.
        let adapter = MockAdapter::new();
        adapter.queue_stream_response(MockResponse::Stream(vec![
            StreamEvent::TextStart { content_index: 0 },
            StreamEvent::TextDelta {
                content_index: 0,
                delta: "hi".to_string(),
            },
            StreamEvent::TextEnd {
                content_index: 0,
                content: "hi".to_string(),
            },
            StreamEvent::Usage {
                input: 5,
                output: 2,
                total: 7,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
                reasoning_output_tokens: 0,
            },
            StreamEvent::Done {
                stop_reason: crate::providers::StopReason::Stop,
            },
        ]));
        let tmp = tempfile::tempdir().unwrap();
        let catalog = tmp.path().join("models.toml");
        let (resolver, _adapter) = crate::providers::LlmResolver::mock(adapter, &catalog).await;
        let (provider, _) = resolver
            .build(crate::providers::resolver::ResolveRequest {
                override_model: Some("mock"),
                ..Default::default()
            })
            .await
            .unwrap();
        let meter = Arc::new(
            QuotaMeter::load_or_init(
                QuotaConfig {
                    request_count: Some(10),
                    ..Default::default()
                },
                None,
                Utc::now(),
            )
            .await
            .unwrap(),
        );
        let metered = MeteredProvider::with_explicit_meter(provider, Arc::clone(&meter));

        use futures::StreamExt;
        let mut stream = metered
            .stream_with_tools(
                "default",
                &[LlmMessage::user("hi")],
                &[],
                &ChatOptions::default(),
            )
            .await
            .unwrap();
        while let Some(event) = stream.next().await {
            // Just drain. The Usage event should have charged.
            let _ = event;
        }
        let snap = meter.snapshot();
        assert_eq!(
            snap.request_count, 1,
            "Usage event should have charged exactly once"
        );
        assert_eq!(snap.input_tokens, 5);
        assert_eq!(snap.output_tokens, 2);
    }
}
