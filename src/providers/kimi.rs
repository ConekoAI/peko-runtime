//! Kimi provider implementation

use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::json;

use crate::providers::Provider;

/// Kimi (Moonshot) provider
pub struct KimiProvider {
    api_key: String,
    model: String,
    base_url: String,
    client: reqwest::Client,
}

impl KimiProvider {
    /// Create new Kimi provider from environment
    pub fn from_env() -> Result<Self> {
        let api_key = std::env::var("KIMI_API_KEY")
            .or_else(|_| std::env::var("MOONSHOT_API_KEY"))
            .context("KIMI_API_KEY or MOONSHOT_API_KEY environment variable required")?;

        Ok(Self::new(api_key))
    }

    /// Create new Kimi provider with API key
    #[must_use]
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            model: "kimi-k2.5".to_string(),
            base_url: "https://api.moonshot.cn/v1".to_string(),
            client: reqwest::Client::new(),
        }
    }

    /// Set model
    #[must_use]
    pub fn with_model(mut self, model: &str) -> Self {
        self.model = model.to_string();
        self
    }

    /// Build request body
    fn build_request_body(
        &self,
        messages: Vec<serde_json::Value>,
        model: &str,
        temperature: f64,
        stream: bool,
    ) -> serde_json::Value {
        json!({
            "model": model,
            "messages": messages,
            "temperature": temperature,
            "stream": stream
        })
    }

    /// Build messages from system prompt and user message
    fn build_messages(&self, system_prompt: Option<&str>, message: &str) -> Vec<serde_json::Value> {
        let mut messages: Vec<serde_json::Value> = Vec::new();

        // Add system message if provided
        if let Some(system) = system_prompt {
            messages.push(json!({
                "role": "system",
                "content": system
            }));
        }

        // Add user message
        messages.push(json!({
            "role": "user",
            "content": message
        }));

        messages
    }
}

#[async_trait]
impl Provider for KimiProvider {
    fn name(&self) -> &'static str {
        "kimi"
    }

    async fn complete(&self, prompt: &str) -> Result<String> {
        self.chat_with_system(None, prompt, &self.model, 0.7).await
    }

    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> Result<String> {
        let messages = self.build_messages(system_prompt, message);
        let body = self.build_request_body(messages, model, temperature, false);

        let response = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .context("Failed to send request to Kimi API")?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            anyhow::bail!("Kimi API error ({status}): {error_text}");
        }

        let result: serde_json::Value = response
            .json()
            .await
            .context("Failed to parse Kimi API response")?;

        let content = result
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .context("No content in Kimi response")?;

        Ok(content.to_string())
    }

    async fn complete_stream(
        &self,
        prompt: &str,
        event_tx: tokio::sync::mpsc::Sender<crate::engine::AgenticEvent>,
        run_id: String,
    ) -> Result<()> {
        use crate::engine::{AgenticEvent, LifecyclePhase};
        use crate::providers::SseParser;
        use tracing::{debug, error};

        // Emit start event
        let _ = event_tx
            .send(AgenticEvent::Lifecycle {
                run_id: run_id.clone(),
                phase: LifecyclePhase::Start,
                error: None,
            })
            .await;

        let messages = self.build_messages(None, prompt);
        let body = self.build_request_body(messages, &self.model, 0.7, true);

        debug!("Sending streaming request to Kimi: model={}", self.model);

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
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .json(&body)
            .send()
            .await
            .context("Failed to send streaming request to Kimi API")?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            error!("Kimi API error: {} - {}", status, error_text);

            let _ = event_tx
                .send(AgenticEvent::Lifecycle {
                    run_id: run_id.clone(),
                    phase: LifecyclePhase::Error,
                    error: Some(format!("Kimi API error: {status} - {error_text}")),
                })
                .await;

            anyhow::bail!("Kimi API error ({status}): {error_text}");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kimi_provider_creation() {
        let provider = KimiProvider::new("test-api-key".to_string()).with_model("kimi-k2.5");

        assert_eq!(provider.name(), "kimi");
    }

    #[test]
    fn test_build_request_body() {
        let provider = KimiProvider::new("test".to_string());
        let messages = vec![json!({"role": "user", "content": "Hello"})];

        let body = provider.build_request_body(messages, "kimi-k2.5", 0.7, false);
        assert_eq!(body["model"], "kimi-k2.5");
        assert!(body["messages"].as_array().unwrap().len() > 0);
    }
}
