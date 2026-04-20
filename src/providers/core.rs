//! Unified provider implementation
//!
//! This module provides a single provider implementation that works with
//! any `ApiAdapter`. All provider-specific logic is delegated to the adapter.

use crate::engine::{AgenticEvent, LifecyclePhase};
use crate::providers::adapters::{AnyAdapter, ApiAdapter};
use crate::providers::transport::HttpClient;
use crate::providers::types::{
    ChatMessage, ChatOptions, ChatResponse, ContentBlock, Message, MessageRole, StopReason,
    StreamEvent, TokenUsage, ToolDefinition,
};
use crate::types::provider::ProviderConfig;
use futures::StreamExt;
use std::pin::Pin;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{error, info};

/// Unified provider
///
/// Works with any `ApiAdapter` to provide LLM functionality.
/// All provider-specific formatting is handled by the adapter.
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

        let model_name = config
            .default_model_config()
            .map_or(adapter.default_model(), |m| m.name.as_str());

        info!(
            "{} provider initialized with model: {}",
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

    /// Check if this provider supports native tool calling
    #[must_use]
    pub fn supports_native_tools(&self) -> bool {
        self.adapter.supports_native_tools()
    }

    /// Complete a prompt (legacy/simple interface)
    pub async fn complete(&self, prompt: &str) -> anyhow::Result<String> {
        self.chat(prompt, &self.model(), 0.7).await
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
        let messages = if let Some(system) = system_prompt {
            vec![
                Message {
                    role: MessageRole::System,
                    content: vec![ContentBlock::Text {
                        text: system.to_string(),
                    }],
                    tool_call_id: None,
                },
                Message {
                    role: MessageRole::User,
                    content: vec![ContentBlock::Text {
                        text: message.to_string(),
                    }],
                    tool_call_id: None,
                },
            ]
        } else {
            vec![Message {
                role: MessageRole::User,
                content: vec![ContentBlock::Text {
                    text: message.to_string(),
                }],
                tool_call_id: None,
            }]
        };

        let options = ChatOptions {
            temperature: Some(temperature as f32),
            max_tokens: None,
            api_key: None,
            headers: std::collections::HashMap::new(),
        };

        let (path, body) = self
            .adapter
            .build_request(&messages, None, &options, false)?;
        let response: serde_json::Value = self.client.post_json(&path, &body).await?;
        let parsed = self.adapter.parse_response(response)?;

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
    pub async fn chat_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        options: &ChatOptions,
    ) -> anyhow::Result<ChatResponse> {
        let msgs = self.convert_chat_messages(messages);
        let tool_defs = self.convert_tools(tools);

        let opts = ChatOptions {
            temperature: options.temperature,
            max_tokens: options.max_tokens,
            api_key: options.api_key.clone(),
            headers: options.headers.clone(),
        };

        let (path, body) = self
            .adapter
            .build_request(&msgs, Some(&tool_defs), &opts, false)?;
        let response: serde_json::Value = self.client.post_json(&path, &body).await?;
        let parsed = self.adapter.parse_response(response)?;

        Ok(self.convert_response(parsed))
    }

    /// Stream chat with native tool calling support
    pub async fn stream_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        options: &ChatOptions,
    ) -> anyhow::Result<Pin<Box<dyn futures::Stream<Item = anyhow::Result<StreamEvent>> + Send>>>
    {
        let msgs = self.convert_chat_messages(messages);
        let tool_defs = self.convert_tools(tools);

        let opts = ChatOptions {
            temperature: options.temperature,
            max_tokens: options.max_tokens,
            api_key: options.api_key.clone(),
            headers: options.headers.clone(),
        };

        let (path, body) = self
            .adapter
            .build_request(&msgs, Some(&tool_defs), &opts, true)?;
        let stream = self.client.post_stream(&path, &body).await?;

        // Parse SSE and convert to StreamEvent using a channel-based approach
        let adapter = self.adapter.clone();
        let (tx, rx) = mpsc::channel::<anyhow::Result<StreamEvent>>(100);

        tokio::spawn(async move {
            let mut sse_stream = crate::providers::transport::sse::SseParser::parse_stream(stream);
            while let Some(result) = sse_stream.next().await {
                let output = match result {
                    Ok(event) => match adapter.parse_sse_event(&event.data) {
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
        // Emit start event
        let _ = event_tx
            .send(AgenticEvent::Lifecycle {
                run_id: run_id.clone(),
                phase: LifecyclePhase::Start,
                error: None,
            })
            .await;

        // Build simple completion request
        let messages = vec![Message {
            role: MessageRole::User,
            content: vec![ContentBlock::Text {
                text: prompt.to_string(),
            }],
            tool_call_id: None,
        }];

        let options = ChatOptions {
            temperature: Some(0.7),
            max_tokens: None,
            api_key: None,
            headers: std::collections::HashMap::new(),
        };

        let (path, body) = self
            .adapter
            .build_request(&messages, None, &options, true)?;

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
                Ok(event) => match self.adapter.parse_sse_event(&event.data) {
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

    /// Get the model name (config or default)
    fn model(&self) -> String {
        self.config.default_model_config().map_or_else(
            || self.adapter.default_model().to_string(),
            |m| m.name.clone(),
        )
    }

    /// Convert legacy `ChatMessage` to new Message format
    fn convert_chat_messages(&self, messages: &[ChatMessage]) -> Vec<Message> {
        messages
            .iter()
            .map(|m| {
                let role = match m.role {
                    super::MessageRole::System => MessageRole::System,
                    super::MessageRole::User => MessageRole::User,
                    super::MessageRole::Assistant => MessageRole::Assistant,
                    super::MessageRole::Tool => MessageRole::Tool,
                };

                // Convert content blocks
                let content: Vec<ContentBlock> = m
                    .content
                    .iter()
                    .map(|cb| match cb {
                        crate::types::message::ContentBlock::Text { text } => {
                            ContentBlock::Text { text: text.clone() }
                        }
                        crate::types::message::ContentBlock::ToolCall {
                            id,
                            name,
                            arguments,
                        } => ContentBlock::ToolCall {
                            id: id.clone(),
                            name: name.clone(),
                            arguments: arguments.clone(),
                        },
                        crate::types::message::ContentBlock::ToolResult {
                            tool_call_id,
                            name,
                            content,
                            is_error,
                        } => ContentBlock::ToolResult {
                            tool_call_id: tool_call_id.clone(),
                            name: name.clone(),
                            content: content
                                .iter()
                                .map(|c| match c {
                                    crate::types::message::ContentBlock::Text { text } => {
                                        ContentBlock::Text { text: text.clone() }
                                    }
                                    _ => ContentBlock::Text {
                                        text: String::new(),
                                    },
                                })
                                .collect(),
                            is_error: *is_error,
                        },
                        crate::types::message::ContentBlock::Thinking { text, signature } => {
                            ContentBlock::Thinking {
                                text: text.clone(),
                                signature: signature.clone(),
                            }
                        }
                        _ => ContentBlock::Text {
                            text: String::new(),
                        },
                    })
                    .collect();

                Message {
                    role,
                    content,
                    tool_call_id: m.tool_call_id.clone(),
                }
            })
            .collect()
    }

    /// Convert `ToolDefinition`
    fn convert_tools(&self, tools: &[ToolDefinition]) -> Vec<ToolDefinition> {
        tools
            .iter()
            .map(|t| ToolDefinition {
                name: t.name.clone(),
                description: t.description.clone(),
                parameters: t.parameters.clone(),
            })
            .collect()
    }

    /// Convert `ChatResponse` back to legacy format
    fn convert_response(&self, response: ChatResponse) -> ChatResponse {
        let content: Vec<crate::types::message::ContentBlock> = response
            .content
            .into_iter()
            .map(|cb| match cb {
                ContentBlock::Text { text } => crate::types::message::ContentBlock::Text { text },
                ContentBlock::ToolCall {
                    id,
                    name,
                    arguments,
                } => crate::types::message::ContentBlock::ToolCall {
                    id,
                    name,
                    arguments,
                },
                ContentBlock::ToolResult {
                    tool_call_id,
                    name,
                    content,
                    is_error,
                } => crate::types::message::ContentBlock::ToolResult {
                    tool_call_id,
                    name: name.clone(),
                    content: content
                        .into_iter()
                        .map(|c| match c {
                            ContentBlock::Text { text } => {
                                crate::types::message::ContentBlock::Text { text }
                            }
                            _ => crate::types::message::ContentBlock::Text {
                                text: String::new(),
                            },
                        })
                        .collect(),
                    is_error,
                },
                ContentBlock::Thinking { text, signature } => {
                    crate::types::message::ContentBlock::Thinking { text, signature }
                }
                _ => crate::types::message::ContentBlock::Text {
                    text: String::new(),
                },
            })
            .collect();

        let tool_calls: Vec<crate::types::message::ContentBlock> = response
            .tool_calls
            .into_iter()
            .map(|cb| match cb {
                ContentBlock::ToolCall {
                    id,
                    name,
                    arguments,
                } => crate::types::message::ContentBlock::ToolCall {
                    id,
                    name,
                    arguments,
                },
                _ => crate::types::message::ContentBlock::Text {
                    text: String::new(),
                },
            })
            .collect();

        let stop_reason = match response.stop_reason {
            StopReason::Stop => super::StopReason::Stop,
            StopReason::Length => super::StopReason::Length,
            StopReason::ToolUse => super::StopReason::ToolUse,
            StopReason::Error => super::StopReason::Error,
            StopReason::Aborted => super::StopReason::Aborted,
        };

        ChatResponse {
            content,
            tool_calls,
            stop_reason,
            usage: TokenUsage {
                input: response.usage.input,
                output: response.usage.output,
                total: response.usage.total,
            },
            provider: response.provider,
            model: response.model,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::adapters::openai::OpenAiAdapter;

    #[test]
    fn test_provider_creation() {
        let adapter = AnyAdapter::OpenAi(OpenAiAdapter::new("gpt-4o-mini"));
        let config = ProviderConfig::default();

        // This will fail without a real API key in tests
        // Just verify the structure is correct
        let result = Provider::new(adapter, "test_key", config);
        assert!(result.is_ok());
    }
}
