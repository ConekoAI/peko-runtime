//! OpenAI-compatible provider for services like Groq, Together, Fireworks

use super::traits::Provider;
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::{debug, error, info};

/// OpenAI-compatible API configuration
#[derive(Debug, Clone)]
pub struct OpenAICompatibleConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub max_tokens: u32,
    pub temperature: f32,
    pub timeout_seconds: u64,
}

impl OpenAICompatibleConfig {
    /// Create a Groq configuration
    #[must_use]
    pub fn groq(api_key: &str, model: &str) -> Self {
        Self {
            api_key: api_key.to_string(),
            base_url: "https://api.groq.com/openai/v1".to_string(),
            model: model.to_string(),
            max_tokens: 4096,
            temperature: 0.7,
            timeout_seconds: 60,
        }
    }

    /// Create a Together AI configuration
    #[must_use]
    pub fn together(api_key: &str, model: &str) -> Self {
        Self {
            api_key: api_key.to_string(),
            base_url: "https://api.together.xyz/v1".to_string(),
            model: model.to_string(),
            max_tokens: 4096,
            temperature: 0.7,
            timeout_seconds: 60,
        }
    }

    /// Create a Fireworks AI configuration
    #[must_use]
    pub fn fireworks(api_key: &str, model: &str) -> Self {
        Self {
            api_key: api_key.to_string(),
            base_url: "https://api.fireworks.ai/inference/v1".to_string(),
            model: model.to_string(),
            max_tokens: 4096,
            temperature: 0.7,
            timeout_seconds: 60,
        }
    }

    /// Create from environment with custom prefix
    pub fn from_env(env_var: &str) -> anyhow::Result<Self> {
        let api_key = std::env::var(env_var).map_err(|_| anyhow::anyhow!("{env_var} not set"))?;

        Ok(Self {
            api_key,
            base_url: String::new(),
            model: String::new(),
            max_tokens: 4096,
            temperature: 0.7,
            timeout_seconds: 60,
        })
    }
}

/// OpenAI-compatible provider (works with Groq, Together, Fireworks, etc.)
pub struct OpenAICompatibleProvider {
    config: OpenAICompatibleConfig,
    client: Client,
    name: String,
}

impl OpenAICompatibleProvider {
    /// Create a new provider
    pub fn new(name: &str, config: OpenAICompatibleConfig) -> anyhow::Result<Self> {
        if config.api_key.is_empty() {
            return Err(anyhow::anyhow!("API key is required"));
        }

        if config.base_url.is_empty() {
            return Err(anyhow::anyhow!("Base URL is required"));
        }

        let client = Client::builder()
            .timeout(Duration::from_secs(config.timeout_seconds))
            .build()?;

        info!("{} provider initialized with model: {}", name, config.model);

        Ok(Self {
            config,
            client,
            name: name.to_string(),
        })
    }

    /// Create a Groq provider
    pub fn groq(api_key: &str, model: &str) -> anyhow::Result<Self> {
        Self::new("groq", OpenAICompatibleConfig::groq(api_key, model))
    }

    /// Create a Together AI provider
    pub fn together(api_key: &str, model: &str) -> anyhow::Result<Self> {
        Self::new("together", OpenAICompatibleConfig::together(api_key, model))
    }

    /// Create a Fireworks AI provider
    pub fn fireworks(api_key: &str, model: &str) -> anyhow::Result<Self> {
        Self::new(
            "fireworks",
            OpenAICompatibleConfig::fireworks(api_key, model),
        )
    }

    /// Create Groq from environment
    pub fn groq_from_env() -> anyhow::Result<Self> {
        let api_key =
            std::env::var("GROQ_API_KEY").map_err(|_| anyhow::anyhow!("GROQ_API_KEY not set"))?;
        let model =
            std::env::var("GROQ_MODEL").unwrap_or_else(|_| "llama-3.1-8b-instant".to_string());
        Self::groq(&api_key, &model)
    }

    /// Create Together from environment
    pub fn together_from_env() -> anyhow::Result<Self> {
        let api_key = std::env::var("TOGETHER_API_KEY")
            .map_err(|_| anyhow::anyhow!("TOGETHER_API_KEY not set"))?;
        let model = std::env::var("TOGETHER_MODEL")
            .unwrap_or_else(|_| "meta-llama/Llama-3.1-8B-Instruct-Turbo".to_string());
        Self::together(&api_key, &model)
    }

    /// Create Fireworks from environment
    pub fn fireworks_from_env() -> anyhow::Result<Self> {
        let api_key = std::env::var("FIREWORKS_API_KEY")
            .map_err(|_| anyhow::anyhow!("FIREWORKS_API_KEY not set"))?;
        let model = std::env::var("FIREWORKS_MODEL")
            .unwrap_or_else(|_| "accounts/fireworks/models/llama-v3p1-8b-instruct".to_string());
        Self::fireworks(&api_key, &model)
    }
}

#[async_trait]
impl Provider for OpenAICompatibleProvider {
    fn name(&self) -> &str {
        &self.name
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

        let request = ChatCompletionRequest {
            model: model.to_string(),
            messages,
            max_tokens: Some(self.config.max_tokens),
            temperature: Some(temperature as f32),
            stream: None,
        };

        debug!("Sending request to {}: model={}", self.name, model);

        let response = self
            .client
            .post(format!("{}/chat/completions", self.config.base_url))
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            error!("{} API error: {} - {}", self.name, status, error_text);
            return Err(anyhow::anyhow!(
                "{} API error: {} - {}",
                self.name,
                status,
                error_text
            ));
        }

        let completion: ChatCompletionResponse = response.json().await?;

        let content = completion
            .choices
            .into_iter()
            .next()
            .map(|c| c.message.content)
            .unwrap_or_default();

        debug!(
            "Received {} tokens from {}",
            completion.usage.total_tokens, self.name
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

        let messages = vec![Message {
            role: "user".to_string(),
            content: prompt.to_string(),
        }];

        let request = ChatCompletionRequest {
            model: self.config.model.clone(),
            messages,
            max_tokens: Some(self.config.max_tokens),
            temperature: Some(self.config.temperature),
            stream: Some(true),
        };

        debug!(
            "Sending streaming request to {}: model={}",
            self.name, self.config.model
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
            .post(format!("{}/chat/completions", self.config.base_url))
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .json(&request)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            error!("{} API error: {} - {}", self.name, status, error_text);

            let _ = event_tx
                .send(AgenticEvent::Lifecycle {
                    run_id: run_id.clone(),
                    phase: LifecyclePhase::Error,
                    error: Some(format!(
                        "{} API error: {} - {}",
                        self.name, status, error_text
                    )),
                })
                .await;

            return Err(anyhow::anyhow!(
                "{} API error: {} - {}",
                self.name,
                status,
                error_text
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
                        if let Ok(delta) = event.parse_json::<serde_json::Value>() {
                            if let Some(content) = delta
                                .get("choices")
                                .and_then(|c| c.get(0))
                                .and_then(|c| c.get("delta"))
                                .and_then(|d| d.get("content"))
                                .and_then(|c| c.as_str())
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

// OpenAI API types (compatible with Groq, Together, Fireworks)

#[derive(Debug, Serialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<Choice>,
    usage: Usage,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: Message,
}

#[derive(Debug, Deserialize)]
struct Usage {
    total_tokens: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_groq_config() {
        let config = OpenAICompatibleConfig::groq("test_key", "llama-3.1-8b-instant");
        assert_eq!(config.base_url, "https://api.groq.com/openai/v1");
        assert_eq!(config.model, "llama-3.1-8b-instant");
    }

    #[test]
    fn test_together_config() {
        let config =
            OpenAICompatibleConfig::together("test_key", "meta-llama/Llama-3.1-8B-Instruct-Turbo");
        assert_eq!(config.base_url, "https://api.together.xyz/v1");
        assert_eq!(config.model, "meta-llama/Llama-3.1-8B-Instruct-Turbo");
    }

    #[test]
    fn test_fireworks_config() {
        let config = OpenAICompatibleConfig::fireworks(
            "test_key",
            "accounts/fireworks/models/llama-v3p1-8b-instruct",
        );
        assert_eq!(config.base_url, "https://api.fireworks.ai/inference/v1");
        assert_eq!(
            config.model,
            "accounts/fireworks/models/llama-v3p1-8b-instruct"
        );
    }

    #[test]
    fn test_provider_creation_without_key_fails() {
        let config = OpenAICompatibleConfig {
            api_key: String::new(),
            base_url: "https://api.example.com".to_string(),
            model: "test-model".to_string(),
            max_tokens: 4096,
            temperature: 0.7,
            timeout_seconds: 60,
        };

        let result = OpenAICompatibleProvider::new("test", config);
        assert!(result.is_err());
    }

    #[test]
    fn test_provider_creation_without_url_fails() {
        let config = OpenAICompatibleConfig {
            api_key: "test_key".to_string(),
            base_url: String::new(),
            model: "test-model".to_string(),
            max_tokens: 4096,
            temperature: 0.7,
            timeout_seconds: 60,
        };

        let result = OpenAICompatibleProvider::new("test", config);
        assert!(result.is_err());
    }
}
