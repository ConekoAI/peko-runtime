//! Kimi (Moonshot) provider implementation with native tool calling
//!
//! Kimi uses OpenAI-compatible API, so we can reuse most of the `OpenAI` provider logic.

use crate::providers::{
    ChatMessage, ChatOptions, ChatResponse, MessageRole, Provider, StopReason, StreamEvent,
    TokenUsage, ToolDefinition,
};
use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::{Stream, StreamExt};
use serde_json::json;
use std::pin::Pin;
use tracing::{debug, error};

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

    /// Convert `ChatMessage` to Kimi format (OpenAI-compatible)
    fn convert_chat_messages(&self, messages: &[ChatMessage]) -> Vec<serde_json::Value> {
        messages
            .iter()
            .map(|m| {
                let role = match m.role {
                    MessageRole::System => "system",
                    MessageRole::User => "user",
                    MessageRole::Assistant => "assistant",
                    MessageRole::Tool => "tool",
                };

                let content = m
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        crate::types::message::ContentBlock::Text { text } => Some(text.clone()),
                        _ => None,
                    })
                    .collect::<String>();

                let mut msg = json!({
                    "role": role,
                    "content": content,
                });

                if let Some(tool_call_id) = &m.tool_call_id {
                    msg["tool_call_id"] = json!(tool_call_id);
                }

                msg
            })
            .collect()
    }

    /// Convert `ToolDefinition` to Kimi tool format (OpenAI-compatible)
    fn convert_tools(&self, tools: &[ToolDefinition]) -> Vec<serde_json::Value> {
        tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters
                    }
                })
            })
            .collect()
    }

    /// Build request for chat with tools
    fn build_request_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: Option<&[ToolDefinition]>,
        options: &ChatOptions,
    ) -> serde_json::Value {
        let kimi_messages = self.convert_chat_messages(messages);

        let mut request = json!({
            "model": self.model,
            "messages": kimi_messages,
            "temperature": f64::from(options.temperature.unwrap_or(0.7)),
            "stream": false,
        });

        if let Some(max_tokens) = options.max_tokens {
            request["max_tokens"] = json!(max_tokens);
        }

        if let Some(tools) = tools {
            request["tools"] = json!(self.convert_tools(tools));
            request["tool_choice"] = json!("auto");
        }

        request
    }
}

#[async_trait]
impl Provider for KimiProvider {
    fn name(&self) -> &'static str {
        "kimi"
    }

    fn supports_native_tools(&self) -> bool {
        true
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

    async fn chat_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        options: &ChatOptions,
    ) -> Result<ChatResponse> {
        let request = self.build_request_with_tools(messages, Some(tools), options);

        debug!("Sending tool-enabled request to Kimi");

        let response = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .context("Failed to send tool request to Kimi API")?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            error!("Kimi API error: {} - {}", status, error_text);
            anyhow::bail!("Kimi API error ({status}): {error_text}");
        }

        let result: serde_json::Value = response
            .json()
            .await
            .context("Failed to parse Kimi API response")?;

        // Parse response
        let choice = result
            .get("choices")
            .and_then(|c| c.get(0))
            .context("No choices in Kimi response")?;

        let message = choice
            .get("message")
            .context("No message in Kimi response")?;

        let content_text = message
            .get("content")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string();

        let finish_reason = choice
            .get("finish_reason")
            .and_then(|r| r.as_str())
            .unwrap_or("stop");

        let stop_reason = match finish_reason {
            "tool_calls" => StopReason::ToolUse,
            "length" => StopReason::Length,
            "stop" | _ => StopReason::Stop,
        };

        // Extract content blocks
        let content = if content_text.is_empty() {
            vec![]
        } else {
            vec![crate::types::message::ContentBlock::Text { text: content_text }]
        };

        // Extract tool calls
        let tool_calls: Vec<crate::types::message::ContentBlock> = message
            .get("tool_calls")
            .and_then(|t| t.as_array())
            .map(|calls| {
                calls
                    .iter()
                    .filter_map(|tc| {
                        let id = tc.get("id")?.as_str()?.to_string();
                        let function = tc.get("function")?;
                        let name = function.get("name")?.as_str()?.to_string();
                        let arguments_str = function.get("arguments")?.as_str()?;
                        let arguments = serde_json::from_str(arguments_str)
                            .unwrap_or_else(|_| serde_json::json!({"raw": arguments_str}));

                        Some(crate::types::message::ContentBlock::ToolCall {
                            id,
                            name,
                            arguments,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        // Extract usage
        let usage = result
            .get("usage")
            .map(|u| TokenUsage {
                input: u.get("prompt_tokens").and_then(serde_json::Value::as_u64).unwrap_or(0),
                output: u
                    .get("completion_tokens")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
                total: u.get("total_tokens").and_then(serde_json::Value::as_u64).unwrap_or(0),
            })
            .unwrap_or_default();

        Ok(ChatResponse {
            content,
            tool_calls,
            stop_reason,
            usage,
            provider: self.name().to_string(),
            model: self.model.clone(),
        })
    }

    async fn stream_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        options: &ChatOptions,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>> {
        let mut request = self.build_request_with_tools(messages, Some(tools), options);
        request["stream"] = json!(true);

        debug!("Sending streaming tool-enabled request to Kimi");

        let response = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .context("Failed to send streaming request to Kimi API")?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            error!("Kimi API error: {} - {}", status, error_text);
            anyhow::bail!("Kimi API error ({status}): {error_text}");
        }

        let model = self.model.clone();
        let provider = self.name().to_string();

        let stream = response
            .bytes_stream()
            .filter_map(|result| async move {
                match result {
                    Ok(bytes) => {
                        let text = String::from_utf8_lossy(&bytes);
                        Some(text.to_string())
                    }
                    Err(e) => {
                        error!("Kimi stream error: {}", e);
                        None
                    }
                }
            })
            .flat_map(|chunk| {
                // Parse SSE events
                let events = parse_kimi_sse_events(&chunk);
                futures::stream::iter(events)
            })
            .scan(
                KimiStreamState::new(model.clone(), provider.clone()),
                |state, event| {
                    let result = state.process_event(event);
                    futures::future::ready(Some(result))
                },
            )
            .filter_map(|result| futures::future::ready(result.transpose()));

        Ok(Box::pin(stream))
    }

    async fn complete_stream(
        &self,
        prompt: &str,
        event_tx: tokio::sync::mpsc::Sender<crate::engine::AgenticEvent>,
        run_id: String,
    ) -> Result<()> {
        // Use default implementation
        <Self as Provider>::complete_stream(self, prompt, event_tx, run_id).await
    }
}

// ============================================================================
// Stream State for Kimi
// ============================================================================

struct KimiStreamState {
    model: String,
    provider: String,
    text_buffer: String,
    tool_calls: Vec<PartialToolCall>,
}

struct PartialToolCall {
    index: usize,
    id: String,
    name: String,
    arguments: String,
}

impl KimiStreamState {
    fn new(model: String, provider: String) -> Self {
        Self {
            model,
            provider,
            text_buffer: String::new(),
            tool_calls: Vec::new(),
        }
    }

    fn process_event(&mut self, event: KimiSseEvent) -> anyhow::Result<Option<StreamEvent>> {
        if event.data.trim() == "[DONE]" {
            return Ok(Some(StreamEvent::Done {
                stop_reason: StopReason::Stop,
            }));
        }

        let chunk: serde_json::Value = serde_json::from_str(&event.data)
            .map_err(|e| anyhow::anyhow!("JSON parse error: {e}"))?;

        if let Some(choice) = chunk.get("choices").and_then(|c| c.get(0)) {
            let delta = choice.get("delta").unwrap_or(&serde_json::Value::Null);

            // Handle text content
            if let Some(text) = delta.get("content").and_then(|c| c.as_str()) {
                if !text.is_empty() {
                    if self.text_buffer.is_empty() {
                        return Ok(Some(StreamEvent::TextStart { content_index: 0 }));
                    }
                    self.text_buffer.push_str(text);
                    return Ok(Some(StreamEvent::TextDelta {
                        content_index: 0,
                        delta: text.to_string(),
                    }));
                }
            }

            // Handle tool calls
            if let Some(tool_calls_delta) = delta.get("tool_calls").and_then(|t| t.as_array()) {
                for tc_delta in tool_calls_delta {
                    let index =
                        tc_delta.get("index").and_then(serde_json::Value::as_u64).unwrap_or(0) as usize;

                    let tc_state =
                        if let Some(tc) = self.tool_calls.iter_mut().find(|t| t.index == index) {
                            tc
                        } else {
                            self.tool_calls.push(PartialToolCall {
                                index,
                                id: String::new(),
                                name: String::new(),
                                arguments: String::new(),
                            });
                            self.tool_calls.last_mut().unwrap()
                        };

                    if let Some(id) = tc_delta.get("id").and_then(|i| i.as_str()) {
                        tc_state.id.push_str(id);
                    }

                    if let Some(func) = tc_delta.get("function") {
                        if let Some(name) = func.get("name").and_then(|n| n.as_str()) {
                            tc_state.name.push_str(name);
                        }
                        if let Some(args) = func.get("arguments").and_then(|a| a.as_str()) {
                            tc_state.arguments.push_str(args);
                        }
                    }
                }
            }

            // Handle finish reason
            if let Some(reason) = choice.get("finish_reason").and_then(|r| r.as_str()) {
                let stop_reason = match reason {
                    "tool_calls" => StopReason::ToolUse,
                    "length" => StopReason::Length,
                    "stop" => StopReason::Stop,
                    _ => StopReason::Stop,
                };

                // Emit text end if we have text
                if !self.text_buffer.is_empty() {
                    return Ok(Some(StreamEvent::TextEnd {
                        content_index: 0,
                        content: self.text_buffer.clone(),
                    }));
                }

                // Emit tool call ends
                if let Some(tc) = self.tool_calls.first() {
                    let arguments = serde_json::from_str(&tc.arguments)
                        .unwrap_or_else(|_| serde_json::json!({}));
                    return Ok(Some(StreamEvent::ToolCallEnd {
                        content_index: tc.index,
                        tool_call: crate::types::message::ContentBlock::ToolCall {
                            id: tc.id.clone(),
                            name: tc.name.clone(),
                            arguments,
                        },
                    }));
                }

                return Ok(Some(StreamEvent::Done { stop_reason }));
            }
        }

        Ok(None)
    }
}

// ============================================================================
// SSE Parsing
// ============================================================================

#[derive(Debug, Clone)]
struct KimiSseEvent {
    data: String,
}

fn parse_kimi_sse_events(chunk: &str) -> Vec<KimiSseEvent> {
    let mut events = Vec::new();
    let mut current_data = String::new();

    for line in chunk.lines() {
        if line.is_empty() {
            if !current_data.is_empty() {
                events.push(KimiSseEvent {
                    data: current_data.clone(),
                });
                current_data.clear();
            }
        } else if let Some(data) = line.strip_prefix("data: ") {
            current_data.push_str(data);
        } else if line.starts_with(':') {
            // Comment, ignore
        }
    }

    if !current_data.is_empty() {
        events.push(KimiSseEvent { data: current_data });
    }

    events
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kimi_provider_creation() {
        let provider = KimiProvider::new("test-api-key".to_string()).with_model("kimi-k2.5");

        assert_eq!(provider.name(), "kimi");
        assert!(provider.supports_native_tools());
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
