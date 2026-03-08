//! `OpenAI` provider implementation with native tool calling
//!
//! Supports:
//! - OpenAI direct API (api.openai.com)
//! - Azure OpenAI
//! - Any OpenAI-compatible API (Groq, Together, etc.)

use super::traits::{
    ChatMessage, ChatOptions, ChatResponse, MessageRole, Provider, StopReason, StreamEvent,
    TokenUsage, ToolDefinition,
};
use async_trait::async_trait;
use futures::{Stream, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use serde_json::Value;
use std::pin::Pin;
use std::time::Duration;
use tracing::{debug, error, info};

/// `OpenAI` API configuration
#[derive(Debug, Clone)]
pub struct OpenAIConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub max_tokens: u32,
    pub temperature: f32,
    pub timeout_seconds: u64,
}

impl Default for OpenAIConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            base_url: "https://api.openai.com/v1".to_string(),
            model: "gpt-4o-mini".to_string(),
            max_tokens: 4096,
            temperature: 0.7,
            timeout_seconds: 60,
        }
    }
}

impl OpenAIConfig {
    /// Create config from environment
    pub fn from_env() -> anyhow::Result<Self> {
        let api_key = std::env::var("OPENAI_API_KEY")
            .map_err(|_| anyhow::anyhow!("OPENAI_API_KEY not set"))?;

        Ok(Self {
            api_key,
            ..Default::default()
        })
    }
}

/// `OpenAI` provider
pub struct OpenAIProvider {
    config: OpenAIConfig,
    client: Client,
}

impl OpenAIProvider {
    /// Create a new `OpenAI` provider
    pub fn new(config: OpenAIConfig) -> anyhow::Result<Self> {
        if config.api_key.is_empty() {
            return Err(anyhow::anyhow!("OpenAI API key is required"));
        }

        let client = Client::builder()
            .timeout(Duration::from_secs(config.timeout_seconds))
            .build()?;

        info!("OpenAI provider initialized with model: {}", config.model);

        Ok(Self { config, client })
    }

    /// Create from environment
    pub fn from_env() -> anyhow::Result<Self> {
        Self::new(OpenAIConfig::from_env()?)
    }

    /// Build messages from system prompt and user message
    fn build_messages(&self, system_prompt: Option<&str>, message: &str) -> Vec<Message> {
        let mut messages = Vec::new();

        if let Some(sys) = system_prompt {
            messages.push(Message {
                role: "system".to_string(),
                content: sys.to_string(),
            });
        }

        messages.push(Message {
            role: "user".to_string(),
            content: message.to_string(),
        });

        messages
    }

    /// Convert ChatMessage to OpenAI format
    fn convert_chat_messages(&self, messages: &[ChatMessage]) -> Vec<OpenAIMessage> {
        messages
            .iter()
            .map(|m| {
                let role = match m.role {
                    MessageRole::System => "system",
                    MessageRole::User => "user",
                    MessageRole::Assistant => "assistant",
                    MessageRole::Tool => "tool",
                };

                // Convert content blocks to content string
                let content = m
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        crate::types::message::ContentBlock::Text { text } => Some(text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");

                OpenAIMessage {
                    role: role.to_string(),
                    content,
                    tool_calls: None,
                    tool_call_id: m.tool_call_id.clone(),
                }
            })
            .collect()
    }

    /// Convert ToolDefinition to OpenAI tool format
    fn convert_tools(&self, tools: &[ToolDefinition]) -> Vec<OpenAITool> {
        tools
            .iter()
            .map(|t| OpenAITool {
                tool_type: "function".to_string(),
                function: OpenAIFunction {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    parameters: t.parameters.clone(),
                },
            })
            .collect()
    }

    /// Build the request body for chat completions
    fn build_request(
        &self,
        messages: &[ChatMessage],
        tools: Option<&[ToolDefinition]>,
        options: &ChatOptions,
    ) -> OpenAIChatRequest {
        let openai_messages = self.convert_chat_messages(messages);
        let openai_tools = tools.map(|t| self.convert_tools(t));

        OpenAIChatRequest {
            model: self.config.model.clone(),
            messages: openai_messages,
            tools: openai_tools,
            tool_choice: if tools.is_some() {
                Some(json!("auto"))
            } else {
                None
            },
            max_tokens: options.max_tokens.or(Some(self.config.max_tokens)),
            temperature: options.temperature.or(Some(self.config.temperature)),
            stream: false,
        }
    }
}

#[async_trait]
impl Provider for OpenAIProvider {
    fn name(&self) -> &'static str {
        "openai"
    }

    fn supports_native_tools(&self) -> bool {
        true
    }

    async fn complete(&self, prompt: &str) -> anyhow::Result<String> {
        self.chat(
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
        let request = ChatCompletionRequest {
            model: model.to_string(),
            messages: self.build_messages(system_prompt, message),
            max_tokens: Some(self.config.max_tokens),
            temperature: Some(temperature as f32),
            stream: None,
        };

        debug!("Sending request to OpenAI: model={}", model);

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
            error!("OpenAI API error: {} - {}", status, error_text);
            return Err(anyhow::anyhow!("OpenAI API error: {status} - {error_text}"));
        }

        let completion: ChatCompletionResponse = response.json().await?;

        let content = completion
            .choices
            .into_iter()
            .next()
            .map(|c| c.message.content)
            .unwrap_or_default();

        debug!(
            "Received {} tokens from OpenAI",
            completion.usage.total_tokens
        );

        Ok(content)
    }

    async fn chat_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        options: &ChatOptions,
    ) -> anyhow::Result<ChatResponse> {
        let request = self.build_request(messages, Some(tools), options);

        debug!("Sending tool-enabled request to OpenAI");

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
            error!("OpenAI API error: {} - {}", status, error_text);
            return Err(anyhow::anyhow!("OpenAI API error: {status} - {error_text}"));
        }

        let completion: OpenAIChatResponse = response.json().await?;

        let choice = completion
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("No choices in OpenAI response"))?;

        let stop_reason = match choice.finish_reason.as_deref() {
            Some("tool_calls") => StopReason::ToolUse,
            Some("length") => StopReason::Length,
            Some("stop") | None => StopReason::Stop,
            _ => StopReason::Stop,
        };

        let message = choice.message;

        // Extract content blocks
        let content = if message.content.is_empty() {
            vec![]
        } else {
            vec![crate::types::message::ContentBlock::Text {
                text: message.content,
            }]
        };

        // Extract tool calls
        let tool_calls: Vec<crate::types::message::ContentBlock> = message
            .tool_calls
            .into_iter()
            .flatten()
            .map(|tc| {
                let arguments = serde_json::from_str(&tc.function.arguments)
                    .unwrap_or_else(|_| serde_json::json!({}));
                crate::types::message::ContentBlock::ToolCall {
                    id: tc.id,
                    name: tc.function.name,
                    arguments,
                }
            })
            .collect();

        Ok(ChatResponse {
            content,
            tool_calls,
            stop_reason,
            usage: TokenUsage {
                input: completion.usage.prompt_tokens as u64,
                output: completion.usage.completion_tokens as u64,
                total: completion.usage.total_tokens as u64,
            },
            provider: self.name().to_string(),
            model: self.config.model.clone(),
        })
    }

    async fn stream_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        options: &ChatOptions,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = anyhow::Result<StreamEvent>> + Send>>> {
        let mut request = self.build_request(messages, Some(tools), options);
        request.stream = true;

        debug!("Sending streaming tool-enabled request to OpenAI");

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
            error!("OpenAI API error: {} - {}", status, error_text);
            return Err(anyhow::anyhow!("OpenAI API error: {status} - {error_text}"));
        }

        let model = self.config.model.clone();
        let provider = self.name().to_string();

        // Create SSE stream
        let stream = response
            .bytes_stream()
            .filter_map(|result| async move {
                match result {
                    Ok(bytes) => {
                        let text = String::from_utf8_lossy(&bytes);
                        Some(text.to_string())
                    }
                    Err(e) => {
                        error!("SSE stream error: {}", e);
                        None
                    }
                }
            })
            .flat_map(|chunk| {
                // Parse SSE events from chunk
                let events = parse_sse_events(&chunk);
                futures::stream::iter(events)
            })
            .scan(
                StreamState::new(model.clone(), provider.clone()),
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
    ) -> anyhow::Result<()> {
        // Use default implementation (blocking with events)
        <Self as Provider>::complete_stream(self, prompt, event_tx, run_id).await
    }
}

// ============================================================================
// Stream State for tracking partial responses
// ============================================================================

struct StreamState {
    model: String,
    provider: String,
    content: Vec<crate::types::message::ContentBlock>,
    tool_calls: Vec<PartialToolCall>,
    text_buffer: String,
    thinking_buffer: String,
}

struct PartialToolCall {
    index: usize,
    id: String,
    name: String,
    arguments: String,
}

impl StreamState {
    fn new(model: String, provider: String) -> Self {
        Self {
            model,
            provider,
            content: Vec::new(),
            tool_calls: Vec::new(),
            text_buffer: String::new(),
            thinking_buffer: String::new(),
        }
    }

    fn process_event(&mut self, event: SseEvent) -> anyhow::Result<Option<StreamEvent>> {
        if event.data.trim() == "[DONE]" {
            return Ok(Some(StreamEvent::Done {
                stop_reason: StopReason::Stop,
            }));
        }

        let chunk: OpenAIStreamChunk = serde_json::from_str(&event.data)
            .map_err(|e| anyhow::anyhow!("JSON parse error: {e}"))?;

        if let Some(choice) = chunk.choices.into_iter().next() {
            let delta = choice.delta;

            // Handle text content
            if let Some(text) = delta.content {
                if !text.is_empty() {
                    if self.text_buffer.is_empty() {
                        let idx = self.content.len();
                        self.content
                            .push(crate::types::message::ContentBlock::Text {
                                text: String::new(),
                            });
                        return Ok(Some(StreamEvent::TextStart { content_index: idx }));
                    }
                    self.text_buffer.push_str(&text);
                    return Ok(Some(StreamEvent::TextDelta {
                        content_index: self.content.len() - 1,
                        delta: text,
                    }));
                }
            }

            // Handle tool calls
            if let Some(tool_calls_delta) = delta.tool_calls {
                for tc_delta in tool_calls_delta {
                    let index = tc_delta.index as usize;

                    // Find or create tool call state
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

                    if let Some(id) = tc_delta.id {
                        tc_state.id.push_str(&id);
                    }

                    if let Some(func) = tc_delta.function {
                        if let Some(name) = func.name {
                            tc_state.name.push_str(&name);
                        }
                        if let Some(args) = func.arguments {
                            tc_state.arguments.push_str(&args);
                        }
                    }
                }
            }

            // Handle finish reason
            if let Some(reason) = choice.finish_reason {
                let stop_reason = match reason.as_str() {
                    "tool_calls" => StopReason::ToolUse,
                    "length" => StopReason::Length,
                    "stop" => StopReason::Stop,
                    _ => StopReason::Stop,
                };

                // Emit text end if we have text
                if !self.text_buffer.is_empty() {
                    if let Some(idx) = self.content.len().checked_sub(1) {
                        return Ok(Some(StreamEvent::TextEnd {
                            content_index: idx,
                            content: self.text_buffer.clone(),
                        }));
                    }
                }

                // Emit tool call ends
                for tc in &self.tool_calls {
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
struct SseEvent {
    data: String,
}

fn parse_sse_events(chunk: &str) -> Vec<SseEvent> {
    let mut events = Vec::new();
    let mut current_data = String::new();

    for line in chunk.lines() {
        if line.is_empty() {
            // Empty line signals end of event
            if !current_data.is_empty() {
                events.push(SseEvent {
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

    // Don't forget the last event if chunk doesn't end with empty line
    if !current_data.is_empty() {
        events.push(SseEvent { data: current_data });
    }

    events
}

// ============================================================================
// OpenAI API Types
// ============================================================================

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

// New types for native tool calling

#[derive(Debug, Serialize)]
struct OpenAIChatRequest {
    model: String,
    messages: Vec<OpenAIMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<OpenAITool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    stream: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct OpenAIMessage {
    role: String,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OpenAIToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct OpenAITool {
    #[serde(rename = "type")]
    tool_type: String,
    function: OpenAIFunction,
}

#[derive(Debug, Serialize, Deserialize)]
struct OpenAIFunction {
    name: String,
    description: String,
    parameters: Value,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct OpenAIToolCall {
    id: String,
    #[serde(rename = "type")]
    tool_type: String,
    function: OpenAIFunctionCall,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct OpenAIFunctionCall {
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct OpenAIChatResponse {
    choices: Vec<OpenAIChatChoice>,
    usage: OpenAIUsage,
}

#[derive(Debug, Deserialize)]
struct OpenAIChatChoice {
    message: OpenAIResponseMessage,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAIResponseMessage {
    role: String,
    content: String,
    #[serde(default)]
    tool_calls: Option<Vec<OpenAIToolCall>>,
}

#[derive(Debug, Deserialize)]
struct OpenAIUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
}

// Streaming types
#[derive(Debug, Deserialize)]
struct OpenAIStreamChunk {
    id: String,
    object: String,
    choices: Vec<OpenAIStreamChoice>,
}

#[derive(Debug, Deserialize)]
struct OpenAIStreamChoice {
    index: u32,
    delta: OpenAIDelta,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct OpenAIDelta {
    role: Option<String>,
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<OpenAIToolCallDelta>>,
}

#[derive(Debug, Deserialize)]
struct OpenAIToolCallDelta {
    index: u32,
    id: Option<String>,
    #[serde(rename = "type")]
    tool_type: Option<String>,
    function: Option<OpenAIDeltaFunction>,
}

#[derive(Debug, Deserialize)]
struct OpenAIDeltaFunction {
    name: Option<String>,
    arguments: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_openai_config_default() {
        let config = OpenAIConfig::default();
        assert_eq!(config.model, "gpt-4o-mini");
        assert_eq!(config.max_tokens, 4096);
        assert_eq!(config.temperature, 0.7);
    }

    // Note: Tests requiring actual API calls are skipped without OPENAI_API_KEY
    #[tokio::test]
    async fn test_openai_provider_creation() {
        // This will fail without API key - that's expected
        let result = OpenAIProvider::from_env();
        // We expect an error if key is not set
        if std::env::var("OPENAI_API_KEY").is_err() {
            assert!(result.is_err());
        }
    }
}
