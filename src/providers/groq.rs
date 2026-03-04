//! Groq provider implementation
//! Ultra-fast LLM inference API

use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::json;

use crate::providers::Provider;

/// Groq provider for fast inference
pub struct GroqProvider {
    api_key: String,
    model: String,
    client: reqwest::Client,
}

impl GroqProvider {
    /// Create new Groq provider from API key
    #[must_use]
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            model: "llama-3.1-70b-versatile".to_string(),
            client: reqwest::Client::new(),
        }
    }

    /// Create from environment variable
    pub fn from_env() -> Result<Self> {
        let api_key =
            std::env::var("GROQ_API_KEY").context("GROQ_API_KEY environment variable not set")?;
        Ok(Self::new(api_key))
    }

    /// Set model
    #[must_use]
    pub fn with_model(mut self, model: &str) -> Self {
        self.model = model.to_string();
        self
    }

    /// Build messages from system prompt and user message
    fn build_messages(&self, system_prompt: Option<&str>, message: &str) -> Vec<serde_json::Value> {
        let mut messages = Vec::new();

        if let Some(sys) = system_prompt {
            messages.push(json!({
                "role": "system",
                "content": sys
            }));
        }

        messages.push(json!({
            "role": "user",
            "content": message
        }));

        messages
    }
}

#[async_trait]
impl Provider for GroqProvider {
    fn name(&self) -> &'static str {
        "groq"
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

        let body = json!({
            "model": model,
            "messages": messages,
            "temperature": temperature,
            "max_tokens": 4096
        });

        let response = self
            .client
            .post("https://api.groq.com/openai/v1/chat/completions")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .context("Failed to send request to Groq API")?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            anyhow::bail!("Groq API error ({status}): {error_text}");
        }

        let result: serde_json::Value = response
            .json()
            .await
            .context("Failed to parse Groq API response")?;

        let content = result
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .context("No content in Groq response")?;

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
        use tracing::error;

        // Emit start event
        let _ = event_tx
            .send(AgenticEvent::Lifecycle {
                run_id: run_id.clone(),
                phase: LifecyclePhase::Start,
                error: None,
            })
            .await;

        let messages = self.build_messages(None, prompt);

        let body = json!({
            "model": self.model,
            "messages": messages,
            "temperature": 0.7,
            "max_tokens": 4096,
            "stream": true
        });

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
            .post("https://api.groq.com/openai/v1/chat/completions")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .json(&body)
            .send()
            .await
            .context("Failed to send streaming request to Groq API")?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            error!("Groq API error: {} - {}", status, error_text);

            let _ = event_tx
                .send(AgenticEvent::Lifecycle {
                    run_id: run_id.clone(),
                    phase: LifecyclePhase::Error,
                    error: Some(format!("Groq API error: {status} - {error_text}")),
                })
                .await;

            anyhow::bail!("Groq API error ({status}): {error_text}");
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
    fn test_groq_provider_creation() {
        let provider = GroqProvider::new("test-key".to_string()).with_model("llama-3.1-8b");

        assert_eq!(provider.name(), "groq");
    }
}
