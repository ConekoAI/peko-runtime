//! Unified provider implementation
//!
//! This module provides a single provider implementation that works with
//! any `ApiAdapter`. All provider-specific logic is delegated to the adapter.

use crate::engine::{AgenticEvent, LifecyclePhase};
use crate::providers::adapters::{AnyAdapter, ApiAdapter};
use crate::providers::cache_retention::CacheRetention;
use crate::providers::openai_prompt_cache::clamp_openai_prompt_cache_key;
use crate::providers::traits::{
    ChatOptions, ChatResponse, ContentBlock, LlmMessage, StreamEvent, ToolDefinition,
};
use crate::providers::transport::HttpClient;
use futures::StreamExt;
use std::pin::Pin;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{error, info};

/// Slim options carried alongside the HTTP client.
///
/// Replaces the old `ProviderConfig` shape. The catalog is the
/// single source of truth for models; the only fields the
/// `Provider` struct itself needs are the six below.
#[derive(Debug, Clone)]
pub struct ProviderRuntimeOptions {
    /// Catalog-declared default model id, surfaced through
    /// `Provider::model_id()` for legacy callers.
    pub default_model_id: String,
    /// Catalog-declared context window, used by the agentic loop.
    pub context_window: Option<u32>,
    /// Per-request HTTP timeout, in seconds.
    pub timeout_seconds: u64,
    /// Number of retries for transient transport failures.
    pub max_retries: u32,
    /// Initial backoff between retries, in milliseconds.
    pub retry_delay_ms: u64,
    /// Per-model extra HTTP headers from the catalog entry, e.g.
    /// `anthropic-beta: interleaved-thinking-2025-05-08` or
    /// `OpenAI-Organization`. Merged with the adapter's built-in
    /// headers (e.g. `anthropic-version`); model-level headers
    /// win on name conflict so a user override is honored.
    pub extra_headers: Vec<(String, String)>,
    /// Stable session identifier used as the prompt-cache key
    /// (`prompt_cache_key` on OpenAI, `metadata.user_id` on
    /// Anthropic). When `None`, the provider's automatic
    /// prefix-detection is relied on. F23.
    pub session_id: Option<String>,
    /// Prompt-cache retention policy (F23). `Default` lets the
    /// provider pick its own TTL; `Long` requests the longest TTL
    /// the provider supports; `None` disables cache markers and
    /// session-affinity fields entirely.
    pub cache_retention: CacheRetention,
}

impl Default for ProviderRuntimeOptions {
    fn default() -> Self {
        Self {
            default_model_id: String::new(),
            context_window: None,
            timeout_seconds: 300,
            max_retries: 3,
            retry_delay_ms: 1000,
            extra_headers: Vec::new(),
            session_id: None,
            cache_retention: CacheRetention::Default,
        }
    }
}

/// Unified provider
///
/// Works with any `ApiAdapter` to provide LLM functionality.
/// All provider-specific formatting is handled by the adapter.
///
/// **Model is no longer stored on the adapter.** `Provider` retains
/// `default_model_id` from `ProviderRuntimeOptions` for legacy
/// callers, but every public `chat*` method accepts an explicit
/// `model_id` parameter that is threaded into the adapter's
/// `build_request`/`parse_response`/`parse_sse_event` calls.
#[derive(Clone)]
pub struct Provider {
    client: HttpClient,
    adapter: AnyAdapter,
    options: ProviderRuntimeOptions,
}

/// Merge the adapter's built-in headers with the catalog entry's
/// per-model overrides. Adapter headers come first; model headers
/// come second and win on header-name conflict so a user override
/// (e.g. a newer `anthropic-version`) is honored. Comparison is
/// case-insensitive to match HTTP/1.1 header semantics.
fn merge_extra_headers(
    adapter: &AnyAdapter,
    model_headers: &[(String, String)],
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = adapter.extra_headers();
    for (name, value) in model_headers {
        let needle = name.to_ascii_lowercase();
        if let Some(existing) = out
            .iter_mut()
            .find(|(n, _)| n.to_ascii_lowercase() == needle)
        {
            existing.1 = value.clone();
        } else {
            out.push((name.clone(), value.clone()));
        }
    }
    out
}

/// Build the prompt-cache fields on `ChatOptions` from the runtime
/// options. When the adapter does not support cache control, both
/// fields are left at their defaults (`CacheRetention::Default`,
/// `prompt_cache_key: None`) so legacy callers keep their current
/// wire shape.
///
/// OpenAI's `prompt_cache_key` is clamped to 64 UTF-32 chars per
/// OpenAI's spec; Anthropic accepts any length and we forward the
/// session id verbatim (its adapters map to `metadata.user_id`).
fn project_cache_options(
    adapter: &AnyAdapter,
    options: &ProviderRuntimeOptions,
) -> (CacheRetention, Option<String>) {
    if !adapter.supports_prompt_cache_control() {
        return (CacheRetention::Default, None);
    }
    let prompt_cache_key = options
        .session_id
        .as_deref()
        .map(clamp_openai_prompt_cache_key);
    (options.cache_retention, prompt_cache_key)
}

impl Provider {
    /// Create a new provider
    pub fn new(
        adapter: AnyAdapter,
        api_key: impl Into<String>,
        options: ProviderRuntimeOptions,
    ) -> anyhow::Result<Self> {
        let api_key = api_key.into();

        // Mock adapter does not need a real HTTP client or API key
        let merged_headers = merge_extra_headers(&adapter, &options.extra_headers);
        let client = if matches!(adapter, AnyAdapter::Mock(_)) {
            HttpClient::with_headers(
                adapter.base_url(),
                adapter.auth_config(&api_key),
                options.timeout_seconds,
                merged_headers,
            )?
        } else {
            if api_key.is_empty() {
                return Err(anyhow::anyhow!("API key is required"));
            }

            let auth = adapter.auth_config(&api_key);
            let mut client = HttpClient::with_headers(
                adapter.base_url(),
                auth,
                options.timeout_seconds,
                merged_headers,
            )?;

            // Wire retry policy from the runtime options.
            if let Some(retry_policy) = crate::providers::transport::RetryPolicy::from_config(
                options.max_retries,
                options.retry_delay_ms,
            ) {
                client = client.with_retry_policy(retry_policy);
            }
            client
        };

        let model_name = if options.default_model_id.is_empty() {
            "<unset — pass model_id per request>".to_string()
        } else {
            options.default_model_id.clone()
        };

        info!(
            "{} provider initialized (default model: {})",
            adapter.name(),
            model_name
        );

        Ok(Self {
            client,
            adapter,
            options,
        })
    }

    /// Run an operation once. Rotation bindings were removed in the
    /// model-first migration; this helper is kept so call sites stay
    /// readable.
    async fn with_auth_rotation<F, Fut, T>(&self, operation: F) -> anyhow::Result<T>
    where
        F: Fn(&Self) -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<T>> + Send,
    {
        operation(self).await
    }

    /// Provider name
    #[must_use]
    pub fn name(&self) -> &str {
        self.adapter.name()
    }

    /// Resolve the model id this provider should use when callers
    /// don't pass one explicitly. Pulled from the runtime options
    /// (which the factory sets to the catalog entry's declared
    /// `model_id`).
    #[must_use]
    pub fn model_id(&self) -> String {
        self.options.default_model_id.clone()
    }

    /// Catalog-declared context window, if any.
    #[must_use]
    pub fn context_window(&self) -> Option<u32> {
        self.options.context_window
    }

    /// Borrow the resolved runtime options (model id, context
    /// window, headers, …). Useful for tests that want to assert
    /// how the catalog entry was projected onto the live provider.
    #[must_use]
    pub fn options(&self) -> &ProviderRuntimeOptions {
        &self.options
    }

    /// Check if this provider supports native tool calling
    #[must_use]
    pub fn supports_native_tools(&self) -> bool {
        self.adapter.supports_native_tools()
    }

    /// Whether this provider's adapter emits prompt-cache markers
    /// (`cache_control` blocks for Anthropic, `prompt_cache_key` for
    /// OpenAI) when the caller supplies a `prompt_cache_key` or
    /// `cache_retention != None`. Mock adapters return `false`.
    #[must_use]
    pub fn supports_prompt_cache_control(&self) -> bool {
        self.adapter.supports_prompt_cache_control()
    }

    /// Complete a prompt (legacy/simple interface)
    pub async fn complete(&self, prompt: &str) -> anyhow::Result<String> {
        let m = self.model_id();
        self.chat(prompt, &m, 0.7).await
    }

    /// Simple chat interface
    pub async fn chat(
        &self,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        self.chat_with_system(None, message, model, temperature)
            .await
    }

    /// Like [`chat`] but returns the full [`ChatResponse`] including
    /// token usage. Used by internal callers that need to account for
    /// the LLM call's cost (e.g. the background compactor, which would
    /// otherwise have its summarization LLM call drop on the floor).
    pub async fn chat_response(
        &self,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<ChatResponse> {
        self.chat_response_with_system(None, message, model, temperature)
            .await
    }

    /// Warm up the HTTP connection pool
    pub async fn warmup(&self) -> anyhow::Result<()> {
        Ok(())
    }

    /// Chat with optional system prompt (zeroclaw-compatible interface)
    pub async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        _model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        let parsed = self
            .chat_response_with_system(system_prompt, message, _model, temperature)
            .await?;

        // Extract text from content
        let text: String = parsed
            .content
            .into_iter()
            .filter_map(|cb| match cb {
                ContentBlock::Text { text } => Some(text),
                _ => None,
            })
            .collect();

        Ok(text)
    }

    /// Like [`chat_with_system`] but returns the full [`ChatResponse`]
    /// (including usage). Kept private by convention; callers should
    /// prefer [`chat_response`] when no system prompt is needed.
    pub async fn chat_response_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        _model: &str,
        temperature: f64,
    ) -> anyhow::Result<ChatResponse> {
        self.with_auth_rotation(|provider| {
            let provider = provider.clone();
            async move {
                let messages: Vec<LlmMessage> = if let Some(system) = system_prompt {
                    vec![LlmMessage::system(system), LlmMessage::user(message)]
                } else {
                    vec![LlmMessage::user(message)]
                };

                let (cache_retention, prompt_cache_key) =
                    project_cache_options(&provider.adapter, &provider.options);
                let options = ChatOptions {
                    temperature: Some(temperature as f32),
                    cache_retention,
                    prompt_cache_key,
                    ..Default::default()
                };

                if let AnyAdapter::Mock(mock) = &provider.adapter {
                    return mock.chat_with_tools(_model, &messages, Some(&[]), &options);
                }

                let (path, body) = provider
                    .adapter
                    .build_request(_model, &messages, None, &options, false)?;
                let per_request_headers = provider.adapter.extra_request_headers(_model, &options);
                let response: serde_json::Value = provider
                    .client
                    .post_json(&path, &body, &per_request_headers)
                    .await?;
                provider.adapter.parse_response(_model, response)
            }
        })
        .await
    }

    /// Chat with native tool calling support (blocking)
    ///
    /// `model_id` is the wire-format model identifier; it is threaded
    /// into the adapter for this call only.
    pub async fn chat_with_tools(
        &self,
        model_id: &str,
        messages: &[LlmMessage],
        tools: &[ToolDefinition],
        options: &ChatOptions,
    ) -> anyhow::Result<ChatResponse> {
        self.with_auth_rotation(|provider| {
            let provider = provider.clone();
            async move {
                // Short-circuit to mock adapter when testing
                if let AnyAdapter::Mock(mock) = &provider.adapter {
                    return mock.chat_with_tools(model_id, messages, Some(tools), options);
                }

                let (path, body) = provider.adapter.build_request(
                    model_id,
                    messages,
                    Some(tools),
                    options,
                    false,
                )?;
                let per_request_headers = provider.adapter.extra_request_headers(model_id, options);
                let response: serde_json::Value = provider
                    .client
                    .post_json(&path, &body, &per_request_headers)
                    .await?;
                provider.adapter.parse_response(model_id, response)
            }
        })
        .await
    }

    /// Stream chat with native tool calling support
    pub async fn stream_with_tools(
        &self,
        model_id: &str,
        messages: &[LlmMessage],
        tools: &[ToolDefinition],
        options: &ChatOptions,
    ) -> anyhow::Result<Pin<Box<dyn futures::Stream<Item = anyhow::Result<StreamEvent>> + Send>>>
    {
        self.with_auth_rotation(|provider| {
            let provider = provider.clone();
            async move {
                // Short-circuit to mock adapter when testing
                if let AnyAdapter::Mock(mock) = &provider.adapter {
                    return mock.stream_with_tools(model_id, messages, Some(tools), options);
                }

                let (path, body) = provider.adapter.build_request(
                    model_id,
                    messages,
                    Some(tools),
                    options,
                    true,
                )?;
                let per_request_headers = provider.adapter.extra_request_headers(model_id, options);
                let stream = provider
                    .client
                    .post_stream(&path, &body, &per_request_headers)
                    .await?;

                // Parse SSE and convert to StreamEvent using a channel-based approach
                let adapter = provider.adapter.clone();
                let model_id_owned = model_id.to_string();
                let (tx, rx) = mpsc::channel::<anyhow::Result<StreamEvent>>(100);

                tokio::spawn(async move {
                    let mut sse_stream =
                        crate::providers::transport::sse::SseParser::parse_stream(stream);
                    while let Some(result) = sse_stream.next().await {
                        // The OpenAI-style `[DONE]` sentinel and the Anthropic-style
                        // `message_stop` event (which `parse_sse_event` maps to
                        // `StreamEvent::Done`) both mark the logical end of the
                        // stream. Some providers hold the HTTP connection open
                        // (keep-alive) after emitting them instead of closing the
                        // byte stream, so relying on `sse_stream.next()` returning
                        // `None` to terminate can block forever — stalling the
                        // agentic loop and hanging `peko send` after the final
                        // token. Stop once the canonical Done has been forwarded.
                        // Usage chunks arrive *before* Done, so they are still
                        // delivered. Providers that neither emit a Done event nor
                        // close the connection will still hang; the only safe
                        // mitigation there is the per-request HTTP timeout.
                        let is_openai_done =
                            matches!(&result, Ok(event) if event.data.trim() == "[DONE]");

                        let output = match result {
                            Ok(event) => {
                                match adapter.parse_sse_event(&model_id_owned, &event.data) {
                                    Ok(Some(stream_event)) => Some(Ok(stream_event)),
                                    Ok(None) => None,
                                    Err(e) => Some(Err(e)),
                                }
                            }
                            Err(e) => Some(Err(e)),
                        };

                        let is_done_event = matches!(
                            &output,
                            Some(Ok(crate::providers::StreamEvent::Done { .. }))
                        );

                        if let Some(event) = output {
                            if tx.send(event).await.is_err() {
                                break;
                            }
                        }

                        if is_openai_done || is_done_event {
                            break;
                        }
                    }
                    // tx dropped here, closing the channel
                });

                Ok(Box::pin(ReceiverStream::new(rx)))
            }
        })
        .await
    }

    /// Stream completion with events (legacy interface)
    pub async fn complete_stream(
        &self,
        prompt: &str,
        event_tx: mpsc::Sender<AgenticEvent>,
        run_id: String,
    ) -> anyhow::Result<()> {
        self.with_auth_rotation(|provider| {
            let provider = provider.clone();
            let event_tx = event_tx.clone();
            let run_id = run_id.clone();
            async move {
                let model_id_owned = provider.model_id();
                // Emit start event
                let _ = event_tx
                    .send(AgenticEvent::Lifecycle {
                        run_id: run_id.clone(),
                        phase: LifecyclePhase::Start,
                        error: None,
                    })
                    .await;

                // Build simple completion request
                let messages = vec![LlmMessage::user(prompt)];

                let (cache_retention, prompt_cache_key) =
                    project_cache_options(&provider.adapter, &provider.options);
                let options = ChatOptions {
                    temperature: Some(0.7),
                    cache_retention,
                    prompt_cache_key,
                    ..Default::default()
                };

                let (path, body) = provider.adapter.build_request(
                    &model_id_owned,
                    &messages,
                    None,
                    &options,
                    true,
                )?;
                let per_request_headers = provider
                    .adapter
                    .extra_request_headers(&model_id_owned, &options);

                // Emit running event
                let _ = event_tx
                    .send(AgenticEvent::Lifecycle {
                        run_id: run_id.clone(),
                        phase: LifecyclePhase::Running,
                        error: None,
                    })
                    .await;

                let stream = provider
                    .client
                    .post_stream(&path, &body, &per_request_headers)
                    .await?;
                let mut accumulated_text = String::new();
                let mut sequence = 0usize;

                use futures::StreamExt;
                let mut parser = crate::providers::transport::sse::SseParser::parse_stream(stream);

                while let Some(result) = parser.next().await {
                    match result {
                        Ok(event) => match provider
                            .adapter
                            .parse_sse_event(&model_id_owned, &event.data)
                        {
                            Ok(Some(StreamEvent::TextDelta { delta, .. })) => {
                                accumulated_text.push_str(&delta);
                                sequence += 1;
                                let _ = event_tx
                                    .send(AgenticEvent::AssistantDelta {
                                        run_id: run_id.clone(),
                                        text: delta,
                                        sequence,
                                        is_interstitial: false,
                                    })
                                    .await;
                            }
                            Ok(Some(StreamEvent::Done { .. })) => break,
                            Ok(Some(StreamEvent::Error { message })) => {
                                error!("Stream error: {}", message);
                                let _ = event_tx
                                    .send(AgenticEvent::Lifecycle {
                                        run_id: run_id.clone(),
                                        phase: LifecyclePhase::Error,
                                        error: Some(message.clone()),
                                    })
                                    .await;
                                return Err(anyhow::anyhow!("Stream error: {message}"));
                            }
                            _ => {}
                        },
                        Err(e) => {
                            let err_msg = e.to_string();
                            error!("Stream error: {}", err_msg);
                            let _ = event_tx
                                .send(AgenticEvent::Lifecycle {
                                    run_id: run_id.clone(),
                                    phase: LifecyclePhase::Error,
                                    error: Some(err_msg),
                                })
                                .await;
                            return Err(e);
                        }
                    }
                }

                // Emit final assistant event using new event type
                let _ = event_tx
                    .send(AgenticEvent::AssistantText {
                        run_id: run_id.clone(),
                        text: accumulated_text,
                        sequence: sequence.saturating_add(1),
                        is_interstitial: false, // Final answer
                    })
                    .await;

                // Emit end event
                let _ = event_tx
                    .send(AgenticEvent::Lifecycle {
                        run_id,
                        phase: LifecyclePhase::End,
                        error: None,
                    })
                    .await;

                Ok(())
            }
        })
        .await
    }

    /// Legacy alias for `model_id`. Kept so older internal call sites
    /// that referenced `self.model()` continue to compile.
    #[must_use]
    pub fn model(&self) -> String {
        self.model_id()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::adapters::openai::OpenAiAdapter;
    use crate::providers::mock::MockAdapter;

    fn runtime_options() -> ProviderRuntimeOptions {
        ProviderRuntimeOptions {
            default_model_id: "gpt-4o-mini".to_string(),
            context_window: None,
            timeout_seconds: 300,
            max_retries: 3,
            retry_delay_ms: 1000,
            ..Default::default()
        }
    }

    #[test]
    fn test_provider_creation() {
        let adapter = AnyAdapter::OpenAi(OpenAiAdapter::new());
        // A non-empty key is required even though no real network call is made.
        let result = Provider::new(adapter, "test_key", runtime_options());
        assert!(result.is_ok());
    }

    #[test]
    fn test_provider_creation_rejects_empty_key() {
        let adapter = AnyAdapter::OpenAi(OpenAiAdapter::new());
        let result = Provider::new(adapter, "", runtime_options());
        assert!(result.is_err(), "empty API key must error on construction");
    }

    #[test]
    fn model_id_returns_default_from_options() {
        let adapter = AnyAdapter::OpenAi(OpenAiAdapter::new());
        let opts = ProviderRuntimeOptions {
            default_model_id: "gpt-5-test".to_string(),
            context_window: None,
            timeout_seconds: 60,
            max_retries: 1,
            retry_delay_ms: 100,
            ..Default::default()
        };
        let provider = Provider::new(adapter, "test_key", opts).unwrap();
        assert_eq!(provider.model_id(), "gpt-5-test");
    }

    #[tokio::test]
    async fn mock_chat_with_tools_works() {
        let adapter = MockAdapter::new();
        adapter.queue_text("hello from mock");
        let provider = Provider::new(
            AnyAdapter::Mock(adapter.clone()),
            "any-key",
            runtime_options(),
        )
        .unwrap();

        let response = provider
            .chat_with_tools("mock-model", &[], &[], &ChatOptions::default())
            .await
            .unwrap();
        let text: String = response
            .content
            .into_iter()
            .filter_map(|cb| match cb {
                ContentBlock::Text { text } => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(text, "hello from mock");
        assert_eq!(adapter.recorded_requests().len(), 1);
    }
}
