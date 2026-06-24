//! Unified provider implementation
//!
//! This module provides a single provider implementation that works with
//! any `ApiAdapter`. All provider-specific logic is delegated to the adapter.

use crate::common::types::provider::ProviderConfig;
use crate::engine::{AgenticEvent, LifecyclePhase};
use crate::providers::adapters::{AnyAdapter, ApiAdapter};
use crate::providers::transport::HttpClient;
use crate::providers::types::{
    ChatOptions, ChatResponse, ContentBlock, LlmMessage, StreamEvent, ToolDefinition,
};
use futures::StreamExt;
use std::pin::Pin;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{error, info};

/// Unified provider
///
/// Works with any `ApiAdapter` to provide LLM functionality.
/// All provider-specific formatting is handled by the adapter.
///
/// **Model is no longer stored on the adapter.** `Provider` retains a
/// `default_model_id` derived from its `ProviderConfig` for legacy
/// callers, but every public `chat*` method accepts an explicit
/// `model_id` parameter that is threaded into the adapter's
/// `build_request`/`parse_response`/`parse_sse_event` calls.
pub struct Provider {
    client: HttpClient,
    adapter: AnyAdapter,
    config: ProviderConfig,
}

impl Provider {
    /// Create a new provider
    pub fn new(
        adapter: AnyAdapter,
        api_key: impl Into<String>,
        config: ProviderConfig,
    ) -> anyhow::Result<Self> {
        let api_key = api_key.into();

        // Mock adapter does not need a real HTTP client or API key
        let client = if matches!(adapter, AnyAdapter::Mock(_)) {
            HttpClient::with_headers(
                adapter.base_url(),
                adapter.auth_config(&api_key),
                config.timeout_seconds,
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
                config.timeout_seconds,
                extra_headers,
            )?;

            // Wire retry configuration from ProviderConfig
            if let Some(retry_policy) = crate::providers::transport::RetryPolicy::from_config(
                config.max_retries,
                config.retry_delay_ms,
            ) {
                client = client.with_retry_policy(retry_policy);
            }
            client
        };

        let model_name = config
            .default_model_config()
            .map(|m| m.name.clone())
            .unwrap_or_else(|| {
                // No model configured at construction time. The
                // adapter no longer carries one; callers must pass
                // `model_id` on every request. We log this clearly.
                "<unset — pass model_id per request>".to_string()
            });

        info!(
            "{} provider initialized (default model: {})",
            adapter.name(),
            model_name
        );

        Ok(Self {
            client,
            adapter,
            config,
        })
    }

    /// Provider name
    #[must_use]
    pub fn name(&self) -> &str {
        self.adapter.name()
    }

    /// Resolve the model id this provider should use when callers
    /// don't pass one explicitly. Pulled from `ProviderConfig`'s
    /// default model configuration.
    #[must_use]
    pub fn model_id(&self) -> String {
        self.config
            .default_model_config()
            .map(|m| m.name.clone())
            .unwrap_or_default()
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

        let (path, body) = self
            .adapter
            .build_request(_model, &messages, None, &options, false)?;
        let response: serde_json::Value = self.client.post_json(&path, &body).await?;
        let parsed = self.adapter.parse_response(_model, response)?;

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
        // Short-circuit to mock adapter when testing
        if let AnyAdapter::Mock(mock) = &self.adapter {
            return mock.chat_with_tools(model_id, messages, Some(tools), options);
        }

        let (path, body) =
            self.adapter
                .build_request(model_id, messages, Some(tools), options, false)?;
        let response: serde_json::Value = self.client.post_json(&path, &body).await?;
        self.adapter.parse_response(model_id, response)
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
        // Short-circuit to mock adapter when testing
        if let AnyAdapter::Mock(mock) = &self.adapter {
            return mock.stream_with_tools(model_id, messages, Some(tools), options);
        }

        let (path, body) =
            self.adapter
                .build_request(model_id, messages, Some(tools), options, true)?;
        let stream = self.client.post_stream(&path, &body).await?;

        // Parse SSE and convert to StreamEvent using a channel-based approach
        let adapter = self.adapter.clone();
        let model_id_owned = model_id.to_string();
        let (tx, rx) = mpsc::channel::<anyhow::Result<StreamEvent>>(100);

        tokio::spawn(async move {
            let mut sse_stream = crate::providers::transport::sse::SseParser::parse_stream(stream);
            while let Some(result) = sse_stream.next().await {
                let output = match result {
                    Ok(event) => match adapter.parse_sse_event(&model_id_owned, &event.data) {
                        Ok(Some(stream_event)) => Some(Ok(stream_event)),
                        Ok(None) => None,
                        Err(e) => Some(Err(e)),
                    },
                    Err(e) => Some(Err(e)),
                };

                if let Some(event) = output {
                    if tx.send(event).await.is_err() {
                        break;
                    }
                }
            }
            // tx dropped here, closing the channel
        });

        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    /// Stream completion with events (legacy interface)
    pub async fn complete_stream(
        &self,
        prompt: &str,
        event_tx: mpsc::Sender<AgenticEvent>,
        run_id: String,
    ) -> anyhow::Result<()> {
        let model_id_owned = self.model_id();
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

        let (path, body) =
            self.adapter
                .build_request(&model_id_owned, &messages, None, &options, true)?;

        // Emit running event
        let _ = event_tx
            .send(AgenticEvent::Lifecycle {
                run_id: run_id.clone(),
                phase: LifecyclePhase::Running,
                error: None,
            })
            .await;

        let stream = self.client.post_stream(&path, &body).await?;
        let mut accumulated_text = String::new();
        let mut sequence = 0usize;

        use futures::StreamExt;
        let mut parser = crate::providers::transport::sse::SseParser::parse_stream(stream);

        while let Some(result) = parser.next().await {
            match result {
                Ok(event) => match self.adapter.parse_sse_event(&model_id_owned, &event.data) {
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

    #[test]
    fn test_provider_creation() {
        let adapter = AnyAdapter::OpenAi(OpenAiAdapter::new());
        let config = ProviderConfig::default();

        // This will fail without a real API key in tests
        // Just verify the structure is correct
        let result = Provider::new(adapter, "test_key", config);
        assert!(result.is_ok());
    }
}
