//! Unified provider implementation
//!
//! This module provides a single provider implementation that works with
//! any `ApiAdapter`. All provider-specific logic is delegated to the adapter.

use crate::engine::{AgenticEvent, LifecyclePhase};
use crate::providers::adapters::{AnyAdapter, ApiAdapter};
use crate::providers::rotating_auth::{is_auth_failure, RotationState};
use crate::providers::transport::HttpClient;
use crate::providers::traits::{
    ChatOptions, ChatResponse, ContentBlock, LlmMessage, StreamEvent, ToolDefinition,
};
use secrecy::ExposeSecret;
use futures::StreamExt;
use std::pin::Pin;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{error, info};

/// Slim options carried alongside the HTTP client.
///
/// Replaces the old `ProviderConfig` shape. The catalog is the
/// single source of truth for providers; the only fields the
/// `Provider` struct itself needs are the four below.
#[derive(Debug, Clone)]
pub struct ProviderRuntimeOptions {
    /// Catalog-declared default model id, surfaced through
    /// `Provider::model_id()` for legacy callers.
    pub default_model_id: String,
    /// Per-request HTTP timeout, in seconds.
    pub timeout_seconds: u64,
    /// Number of retries for transient transport failures.
    pub max_retries: u32,
    /// Initial backoff between retries, in milliseconds.
    pub retry_delay_ms: u64,
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
    /// Optional rotation state. When present, 401 responses advance
    /// to the next credential in the binding and retry.
    rotation: Option<RotationState>,
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
        let client = if matches!(adapter, AnyAdapter::Mock(_)) {
            HttpClient::with_headers(
                adapter.base_url(),
                adapter.auth_config(&api_key),
                options.timeout_seconds,
                adapter.extra_headers(),
            )?
        } else {
            if api_key.is_empty() {
                return Err(anyhow::anyhow!("API key is required"));
            }

            let auth = adapter.auth_config(&api_key);
            let extra_headers = adapter.extra_headers();
            let mut client = HttpClient::with_headers(
                adapter.base_url(),
                auth,
                options.timeout_seconds,
                extra_headers,
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
            // No model configured at construction time. The
            // adapter no longer carries one; callers must pass
            // `model_id` on every request. We log this clearly.
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
            rotation: None,
        })
    }

    /// Rebuild the provider with a different API key, preserving the
    /// adapter, options, and any rotation state.
    ///
    /// Used by the auth-rotation path after a 401 advances the cursor
    /// to the next credential in a binding.
    pub fn rebuild_with_material(
        &self,
        api_key: impl Into<String>,
    ) -> anyhow::Result<Self> {
        let mut rebuilt = Self::new(self.adapter.clone(), api_key, self.options.clone())?;
        rebuilt.rotation = self.rotation.clone();
        Ok(rebuilt)
    }

    /// Attach a rotation state to this provider.
    #[must_use]
    pub fn with_rotation(mut self, rotation: RotationState) -> Self {
        self.rotation = Some(rotation);
        self
    }

    /// Run an operation, retrying on HTTP 401 when a rotation binding is
    /// configured. Advances through bound credentials and rebuilds the
    /// provider with each new material until the operation succeeds, a
    /// non-401 error occurs, or all credentials are exhausted.
    async fn with_auth_rotation<F, Fut, T>(&self, operation: F) -> anyhow::Result<T>
    where
        F: Fn(&Self) -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<T>> + Send,
    {
        let start_index = self.rotation.as_ref().map(|r| r.current_index());
        let mut provider: Option<Self> = None;

        loop {
            let current = provider.as_ref().unwrap_or(self);
            match operation(current).await {
                Ok(v) => {
                    if let Some(rotation) = current.rotation.as_ref() {
                        rotation.record_current_test(true);
                    }
                    return Ok(v);
                }
                Err(e) => {
                    if !is_auth_failure(&e) {
                        return Err(e);
                    }
                    let Some(rotation) = current.rotation.as_ref() else {
                        return Err(e);
                    };
                    rotation.record_current_test(false);
                    rotation.advance();
                    if Some(rotation.current_index()) == start_index {
                        return Err(e);
                    }
                    let Some(material) = rotation.current_material() else {
                        return Err(e);
                    };
                    provider = Some(
                        current
                            .rebuild_with_material(material.expose_secret())?,
                    );
                }
            }
        }
    }

    /// Provider name
    #[must_use]
    pub fn name(&self) -> &str {
        self.adapter.name()
    }

    /// Resolve the model id this provider should use when callers
    /// don't pass one explicitly. Pulled from the runtime options
    /// (which the factory sets to the catalog entry's declared
    /// `default_model_id`).
    #[must_use]
    pub fn model_id(&self) -> String {
        self.options.default_model_id.clone()
    }

    /// Check if this provider supports native tool calling
    #[must_use]
    pub fn supports_native_tools(&self) -> bool {
        self.adapter.supports_native_tools()
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

                let options = ChatOptions {
                    temperature: Some(temperature as f32),
                    max_tokens: None,
                    api_key: None,
                    headers: std::collections::HashMap::new(),
                };

                if let AnyAdapter::Mock(mock) = &provider.adapter {
                    return mock.chat_with_tools(_model, &messages, Some(&[]), &options);
                }

                let (path, body) = provider
                    .adapter
                    .build_request(_model, &messages, None, &options, false)?;
                let response: serde_json::Value = provider.client.post_json(&path, &body).await?;
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

                let (path, body) = provider
                    .adapter
                    .build_request(model_id, messages, Some(tools), options, false)?;
                let response: serde_json::Value =
                    provider.client.post_json(&path, &body).await?;
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

                let (path, body) = provider
                    .adapter
                    .build_request(model_id, messages, Some(tools), options, true)?;
                let stream = provider.client.post_stream(&path, &body).await?;

                // Parse SSE and convert to StreamEvent using a channel-based approach
                let adapter = provider.adapter.clone();
                let model_id_owned = model_id.to_string();
                let (tx, rx) = mpsc::channel::<anyhow::Result<StreamEvent>>(100);

                tokio::spawn(async move {
                    let mut sse_stream = crate::providers::transport::sse::SseParser::parse_stream(stream);
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
                        let is_openai_done = matches!(&result, Ok(event) if event.data.trim() == "[DONE]");

                        let output = match result {
                            Ok(event) => match adapter.parse_sse_event(&model_id_owned, &event.data) {
                                Ok(Some(stream_event)) => Some(Ok(stream_event)),
                                Ok(None) => None,
                                Err(e) => Some(Err(e)),
                            },
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

                let options = ChatOptions {
                    temperature: Some(0.7),
                    max_tokens: None,
                    api_key: None,
                    headers: std::collections::HashMap::new(),
                };

                let (path, body) = provider
                    .adapter
                    .build_request(&model_id_owned, &messages, None, &options, true)?;

                // Emit running event
                let _ = event_tx
                    .send(AgenticEvent::Lifecycle {
                        run_id: run_id.clone(),
                        phase: LifecyclePhase::Running,
                        error: None,
                    })
                    .await;

                let stream = provider.client.post_stream(&path, &body).await?;
                let mut accumulated_text = String::new();
                let mut sequence = 0usize;

                use futures::StreamExt;
                let mut parser = crate::providers::transport::sse::SseParser::parse_stream(stream);

                while let Some(result) = parser.next().await {
                    match result {
                        Ok(event) => match provider.adapter.parse_sse_event(&model_id_owned, &event.data) {
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
    use crate::common::vault::{
        Credential, CredentialKind, RotationBinding, RotationStrategy, Vault,
    };
    use crate::providers::adapters::openai::OpenAiAdapter;
    use crate::providers::mock::MockAdapter;
    use secrecy::SecretString;
    use std::sync::Arc;

    fn runtime_options() -> ProviderRuntimeOptions {
        ProviderRuntimeOptions {
            default_model_id: "gpt-4o-mini".to_string(),
            timeout_seconds: 300,
            max_retries: 3,
            retry_delay_ms: 1000,
        }
    }

    fn rotating_provider_with_two_keys() -> (tempfile::TempDir, Provider, MockAdapter) {
        let dir = tempfile::tempdir().unwrap();
        let vault = Arc::new(Vault::for_test(dir.path(), "rotation-test"));

        let c1 = Credential::now(
            "provider:mock",
            "default",
            CredentialKind::ApiKey,
            SecretString::new("key-1".into()),
        );
        let c2 = Credential::now(
            "provider:mock",
            "default",
            CredentialKind::ApiKey,
            SecretString::new("key-2".into()),
        );
        let id1 = c1.id.clone();
        let id2 = c2.id.clone();
        vault.set_credential(&c1).unwrap();
        vault.set_credential(&c2).unwrap();
        vault
            .set_binding(
                &RotationBinding::slot_key("provider:mock", "default"),
                &RotationBinding {
                    strategy: RotationStrategy::RoundRobin,
                    ordered_credential_ids: vec![id1, id2],
                },
            )
            .unwrap();

        let adapter = MockAdapter::new();
        let rotation = RotationState::new(vault, "provider:mock".into(), "default".into()).unwrap();
        let provider = Provider::new(AnyAdapter::Mock(adapter.clone()), "any-key", runtime_options())
            .unwrap()
            .with_rotation(rotation);
        (dir, provider, adapter)
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
            timeout_seconds: 60,
            max_retries: 1,
            retry_delay_ms: 100,
        };
        let provider = Provider::new(adapter, "test_key", opts).unwrap();
        assert_eq!(provider.model_id(), "gpt-5-test");
    }

    #[tokio::test]
    async fn on_401_advances_to_next_credential_and_retries() {
        let (_dir, provider, adapter) = rotating_provider_with_two_keys();
        adapter.queue_error("HTTP error 401: invalid key");
        adapter.queue_text("success after rotation");

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
        assert_eq!(text, "success after rotation");

        // Two requests were made: one that 401'd, one that succeeded.
        assert_eq!(adapter.recorded_requests().len(), 2);
    }

    #[tokio::test]
    async fn on_all_401_returns_last_error() {
        let (_dir, provider, adapter) = rotating_provider_with_two_keys();
        adapter.queue_error("HTTP error 401: invalid key");
        adapter.queue_error("HTTP error 401: still invalid");

        let err = provider
            .chat_with_tools("mock-model", &[], &[], &ChatOptions::default())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("still invalid"));
        // Two attempts: initial + one retry after rotation exhausts the
        // two-credential binding.
        assert_eq!(adapter.recorded_requests().len(), 2);
    }

    #[tokio::test]
    async fn non_401_error_does_not_rotate() {
        let (_dir, provider, adapter) = rotating_provider_with_two_keys();
        adapter.queue_error("HTTP error 429: rate limited");

        let err = provider
            .chat_with_tools("mock-model", &[], &[], &ChatOptions::default())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("rate limited"));
        assert_eq!(adapter.recorded_requests().len(), 1);
    }
}
