//! Anthropic provider implementation

use super::traits::Provider;
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::{debug, error, info};

/// Anthropic API configuration
#[derive(Debug, Clone)]
pub struct AnthropicConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub max_tokens: u32,
    pub temperature: f32,
    pub timeout_seconds: u64,
}

impl Default for AnthropicConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            base_url: "https://api.anthropic.com".to_string(),
            model: "claude-3-haiku-20240307".to_string(),
            max_tokens: 4096,
            temperature: 0.7,
            timeout_seconds: 60,
        }
    }
}

impl AnthropicConfig {
    /// Create config from environment
    pub fn from_env() -> anyhow::Result<Self> {
        let api_key = std::env::var("ANTHROPIC_API_KEY")
            .map_err(|_| anyhow::anyhow!("ANTHROPIC_API_KEY not set"))?;

        Ok(Self {
            api_key,
            ..Default::default()
        })
    }
}

/// Anthropic provider
pub struct AnthropicProvider {
    config: AnthropicConfig,
    client: Client,
}

impl AnthropicProvider {
    /// Create a new Anthropic provider
    pub fn new(config: AnthropicConfig) -> anyhow::Result<Self> {
        if config.api_key.is_empty() {
            return Err(anyhow::anyhow!("Anthropic API key is required"));
        }

        let client = Client::builder()
            .timeout(Duration::from_secs(config.timeout_seconds))
            .build()?;

        info!(
            "Anthropic provider initialized with model: {}",
            config.model
        );

        Ok(Self { config, client })
    }

    /// Create from environment
    pub fn from_env() -> anyhow::Result<Self> {
        Self::new(AnthropicConfig::from_env()?)
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn name(&self) -> &'static str {
        "anthropic"
    }

    async fn complete(&self, prompt: &str) -> anyhow::Result<String> {
        self.chat_with_system(
            None,
            prompt,
            &self.config.model,
            f64::from(self.config.temperature),
        )
        .await
    }

    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        let mut messages: Vec<Message> = Vec::new();

        // Add system message if provided
        if let Some(system) = system_prompt {
            messages.push(Message {
                role: "system".to_string(),
                content: system.to_string(),
            });
        }

        // Add user message
        messages.push(Message {
            role: "user".to_string(),
            content: message.to_string(),
        });

        let request = MessagesRequest {
            model: model.to_string(),
            max_tokens: self.config.max_tokens,
            temperature: Some(temperature as f32),
            messages,
            stream: None,
        };

        debug!("Sending request to Anthropic: model={}", model);

        let response = self
            .client
            .post(format!("{}/v1/messages", self.config.base_url))
            .header("x-api-key", &self.config.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            error!("Anthropic API error: {} - {}", status, error_text);
            return Err(anyhow::anyhow!(
                "Anthropic API error: {status} - {error_text}"
            ));
        }

        let completion: MessagesResponse = response.json().await?;

        let content = completion
            .content
            .into_iter()
            .next()
            .map(|c| c.text)
            .unwrap_or_default();

        debug!(
            "Received response from Anthropic: input_tokens={}, output_tokens={}",
            completion.usage.input_tokens, completion.usage.output_tokens
        );

        Ok(content)
    }

    async fn complete_stream(
        &self,
        prompt: &str,
        event_tx: tokio::sync::mpsc::Sender<crate::engine::AgenticEvent>,
        run_id: String,
    ) -> anyhow::Result<()> {
        use crate::engine::{AgenticEvent, LifecyclePhase};
        use crate::providers::SseParser;
        use futures::StreamExt;
        use tracing::error;

        // Emit start event
        let _ = event_tx
            .send(AgenticEvent::Lifecycle {
                run_id: run_id.clone(),
                phase: LifecyclePhase::Start,
                error: None,
            })
            .await;

        let mut messages: Vec<Message> = Vec::new();
        messages.push(Message {
            role: "user".to_string(),
            content: prompt.to_string(),
        });

        let request = MessagesRequest {
            model: self.config.model.clone(),
            max_tokens: self.config.max_tokens,
            temperature: Some(self.config.temperature),
            messages,
            stream: Some(true),
        };

        debug!(
            "Sending streaming request to Anthropic: model={}",
            self.config.model
        );

        // Emit running event
        let _ = event_tx
            .send(AgenticEvent::Lifecycle {
                run_id: run_id.clone(),
                phase: LifecyclePhase::Running,
                error: None,
            })
            .await;

        let response = self
            .client
            .post(format!("{}/v1/messages", self.config.base_url))
            .header("x-api-key", &self.config.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .json(&request)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            error!("Anthropic API error: {} - {}", status, error_text);

            let _ = event_tx
                .send(AgenticEvent::Lifecycle {
                    run_id: run_id.clone(),
                    phase: LifecyclePhase::Error,
                    error: Some(format!("Anthropic API error: {status} - {error_text}")),
                })
                .await;

            return Err(anyhow::anyhow!(
                "Anthropic API error: {status} - {error_text}"
            ));
        }

        let mut stream = response.bytes_stream();
        let mut parser = SseParser::new();
        let mut accumulated_text = String::new();

        while let Some(chunk_result) = stream.next().await {
            match chunk_result {
                Ok(bytes) => {
                    let text = String::from_utf8_lossy(&bytes);
                    let events = parser.feed(&text);

                    for event in events {
                        if event.is_done() {
                            break;
                        }

                        // Parse the SSE data as JSON
                        if let Ok(delta) = serde_json::from_str::<serde_json::Value>(&event.data) {
                            // Anthropic sends content_block_delta events
                            if let Some(content) = delta
                                .get("delta")
                                .and_then(|d| d.get("text"))
                                .and_then(|t| t.as_str())
                            {
                                accumulated_text.push_str(content);

                                // Emit text delta
                                let _ = event_tx
                                    .send(AgenticEvent::Assistant {
                                        run_id: run_id.clone(),
                                        text: content.to_string(),
                                        is_delta: true,
                                        is_final: false,
                                    })
                                    .await;
                            }
                        }
                    }
                }
                Err(e) => {
                    error!("Stream error: {}", e);
                    let _ = event_tx
                        .send(AgenticEvent::Lifecycle {
                            run_id: run_id.clone(),
                            phase: LifecyclePhase::Error,
                            error: Some(e.to_string()),
                        })
                        .await;
                    return Err(e.into());
                }
            }
        }

        // Emit final assistant event
        let _ = event_tx
            .send(AgenticEvent::Assistant {
                run_id: run_id.clone(),
                text: accumulated_text,
                is_delta: false,
                is_final: true,
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
}

// Anthropic API types

#[derive(Debug, Serialize)]
struct MessagesRequest {
    model: String,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct MessagesResponse {
    content: Vec<ContentBlock>,
    usage: Usage,
}

#[derive(Debug, Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    block_type: String,
    text: String,
}

#[derive(Debug, Deserialize)]
struct Usage {
    input_tokens: u32,
    output_tokens: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_anthropic_config_default() {
        let config = AnthropicConfig::default();
        assert_eq!(config.model, "claude-3-haiku-20240307");
        assert_eq!(config.max_tokens, 4096);
        assert_eq!(config.temperature, 0.7);
    }

    #[tokio::test]
    async fn test_anthropic_provider_creation() {
        // This will fail without API key - that's expected
        let result = AnthropicProvider::from_env();
        // We expect an error if key is not set
        if std::env::var("ANTHROPIC_API_KEY").is_err() {
            assert!(result.is_err());
        }
    }
}
