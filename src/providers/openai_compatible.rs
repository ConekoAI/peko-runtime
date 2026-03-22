//! OpenAI-compatible provider base for all OpenAI-compatible APIs
//!
//! This module provides a reusable base for providers that use the OpenAI API format,
//! including: OpenAI, Groq, Together, Fireworks, and Moonshot (Kimi).

use super::traits::{
    ChatMessage, ChatOptions, ChatResponse, MessageRole, Provider, StopReason, StreamEvent,
    TokenUsage, ToolDefinition,
};
use async_trait::async_trait;
use futures::{Stream, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::pin::Pin;
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

impl Default for OpenAICompatibleConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            base_url: String::new(),
            model: String::new(),
            max_tokens: 4096,
            temperature: 0.7,
            timeout_seconds: 60,
        }
    }
}

impl OpenAICompatibleConfig {
    /// Create a configuration for OpenAI
    #[must_use]
    pub fn openai(api_key: &str, model: &str) -> Self {
        Self {
            api_key: api_key.to_string(),
            base_url: "https://api.openai.com/v1".to_string(),
            model: model.to_string(),
            max_tokens: 4096,
            temperature: 0.7,
            timeout_seconds: 60,
        }
    }

    /// Create a configuration for Groq
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

    /// Create a configuration for Together AI
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

    /// Create a configuration for Fireworks AI
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

    /// Create a configuration for Moonshot (Kimi)
    #[must_use]
    pub fn moonshot(api_key: &str, model: &str) -> Self {
        Self {
            api_key: api_key.to_string(),
            base_url: "https://api.moonshot.cn/v1".to_string(),
            model: model.to_string(),
            max_tokens: 4096,
            temperature: 0.7,
            timeout_seconds: 60,
        }
    }
}

/// OpenAI-compatible provider base
/// 
/// This struct provides full OpenAI API compatibility including tool calling.
/// All OpenAI-compatible providers (Groq, Together, Fireworks, Kimi) use this.
pub struct OpenAICompatibleProvider {
    config: OpenAICompatibleConfig,
    client: Client,
    name: String,
}

impl OpenAICompatibleProvider {
    /// Create a new provider with the given configuration
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
        Self::new("fireworks", OpenAICompatibleConfig::fireworks(api_key, model))
    }

    /// Create a Moonshot (Kimi) provider
    pub fn moonshot(api_key: &str, model: &str) -> anyhow::Result<Self> {
        Self::new("kimi", OpenAICompatibleConfig::moonshot(api_key, model))
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

    /// Create Moonshot (Kimi) from environment
    pub fn moonshot_from_env() -> anyhow::Result<Self> {
        let api_key = std::env::var("KIMI_API_KEY")
            .or_else(|_| std::env::var("MOONSHOT_API_KEY"))
            .map_err(|_| anyhow::anyhow!("KIMI_API_KEY or MOONSHOT_API_KEY not set"))?;
        let model = std::env::var("KIMI_MODEL").unwrap_or_else(|_| "kimi-k2.5".to_string());
        Self::moonshot(&api_key, &model)
    }

    /// Convert ChatMessage to OpenAI format
    fn convert_messages(&self, messages: &[ChatMessage]) -> Vec<Value> {
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

    /// Convert ToolDefinition to OpenAI format
    fn convert_tools(&self, tools: &[ToolDefinition]) -> Vec<Value> {
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
    fn build_request(
        &self,
        messages: &[ChatMessage],
        tools: Option<&[ToolDefinition]>,
        options: &ChatOptions,
        stream: bool,
    ) -> Value {
        let openai_messages = self.convert_messages(messages);

        let mut request = json!({
            "model": self.config.model,
            "messages": openai_messages,
            "temperature": f64::from(options.temperature.unwrap_or(self.config.temperature)),
            "stream": stream,
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

    /// Parse OpenAI response into ChatResponse
    fn parse_response(&self, response: OpenAIChatResponse) -> ChatResponse {
        let choice = response.choices.into_iter().next();

        let stop_reason = choice
            .as_ref()
            .and_then(|c| c.finish_reason.as_deref())
            .map(|r| match r {
                "tool_calls" => StopReason::ToolUse,
                "length" => StopReason::Length,
                _ => StopReason::Stop,
            })
            .unwrap_or(StopReason::Stop);

        let message = choice.map(|c| c.message);

        // Extract content blocks
        let content = message
            .as_ref()
            .and_then(|m| {
                if m.content.is_empty() {
                    None
                } else {
                    Some(vec![crate::types::message::ContentBlock::Text {
                        text: m.content.clone(),
                    }])
                }
            })
            .unwrap_or_default();

        // Extract tool calls
        let tool_calls: Vec<crate::types::message::ContentBlock> = message
            .and_then(|m| m.tool_calls)
            .map(|calls| {
                calls
                    .into_iter()
                    .map(|tc| {
                        let arguments = serde_json::from_str(&tc.function.arguments)
                            .unwrap_or_else(|_| json!({"raw": tc.function.arguments}));
                        crate::types::message::ContentBlock::ToolCall {
                            id: tc.id,
                            name: tc.function.name,
                            arguments,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        ChatResponse {
            content,
            tool_calls,
            stop_reason,
            usage: TokenUsage {
                input: u64::from(response.usage.prompt_tokens),
                output: u64::from(response.usage.completion_tokens),
                total: u64::from(response.usage.total_tokens),
            },
            provider: self.name.clone(),
            model: self.config.model.clone(),
        }
    }
}

#[async_trait]
impl Provider for OpenAICompatibleProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn supports_native_tools(&self) -> bool {
        true
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

        if let Some(system) = system_prompt {
            messages.push(Message {
                role: "system".to_string(),
                content: system.to_string(),
            });
        }

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

    async fn chat_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        options: &ChatOptions,
    ) -> anyhow::Result<ChatResponse> {
        let request = self.build_request(messages, Some(tools), options, false);

        debug!("Sending tool-enabled request to {}", self.name);

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

        let completion: OpenAIChatResponse = response.json().await?;
        Ok(self.parse_response(completion))
    }

    async fn stream_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        options: &ChatOptions,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = anyhow::Result<StreamEvent>> + Send>>> {
        let request = self.build_request(messages, Some(tools), options, true);

        debug!("Sending streaming tool-enabled request to {}", self.name);

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
            return Err(anyhow::anyhow!(
                "{} API error: {} - {}",
                self.name,
                status,
                error_text
            ));
        }

        let model = self.config.model.clone();
        let provider = self.name.clone();

        let stream = response
            .bytes_stream()
            .filter_map(move |result| async move {
                match result {
                    Ok(bytes) => {
                        let text = String::from_utf8_lossy(&bytes);
                        Some(text.to_string())
                    }
                    Err(e) => {
                        error!("Stream error: {}", e);
                        None
                    }
                }
            })
            .flat_map(|chunk| {
                let events = parse_openai_sse(&chunk);
                futures::stream::iter(events)
            })
            .scan(
                OpenAIStreamState::new(model.clone(), provider.clone()),
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
        use crate::engine::{AgenticEvent, LifecyclePhase};
        use futures::StreamExt;

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
        let mut accumulated_text = String::new();

        while let Some(chunk_result) = stream.next().await {
            match chunk_result {
                Ok(bytes) => {
                    let text = String::from_utf8_lossy(&bytes);
                    let events = parse_openai_sse(&text);

                    for event in events {
                        if let Ok(delta) = serde_json::from_str::<Value>(&event.data) {
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

// ============================================================================
// SSE Parsing
// ============================================================================

#[derive(Debug, Clone)]
struct SseEvent {
    data: String,
}

/// Parse SSE events from a chunk of text
fn parse_openai_sse(chunk: &str) -> Vec<SseEvent> {
    let mut events = Vec::new();
    let mut current_data = String::new();

    for line in chunk.lines() {
        if line.is_empty() {
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

    if !current_data.is_empty() {
        events.push(SseEvent { data: current_data });
    }

    events
}

// ============================================================================
// Stream State
// ============================================================================

struct OpenAIStreamState {
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

impl OpenAIStreamState {
    fn new(model: String, provider: String) -> Self {
        Self {
            model,
            provider,
            text_buffer: String::new(),
            tool_calls: Vec::new(),
        }
    }

    fn process_event(&mut self, event: SseEvent) -> anyhow::Result<Option<StreamEvent>> {
        if event.data.trim() == "[DONE]" {
            return Ok(Some(StreamEvent::Done {
                stop_reason: StopReason::Stop,
            }));
        }

        let chunk: Value = serde_json::from_str(&event.data)
            .map_err(|e| anyhow::anyhow!("JSON parse error: {e}"))?;

        if let Some(choice) = chunk.get("choices").and_then(|c| c.get(0)) {
            let delta = choice.get("delta").unwrap_or(&Value::Null);

            // Handle text content
            if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
                if !content.is_empty() {
                    if self.text_buffer.is_empty() {
                        return Ok(Some(StreamEvent::TextStart { content_index: 0 }));
                    }
                    self.text_buffer.push_str(content);
                    return Ok(Some(StreamEvent::TextDelta {
                        content_index: 0,
                        delta: content.to_string(),
                    }));
                }
            }

            // Handle tool calls
            if let Some(tool_calls_delta) = delta.get("tool_calls").and_then(|t| t.as_array()) {
                for tc_delta in tool_calls_delta {
                    let index = tc_delta
                        .get("index")
                        .and_then(Value::as_u64)
                        .unwrap_or(0) as usize;

                    let tc_state = if let Some(tc) = self.tool_calls.iter_mut().find(|t| t.index == index) {
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
                        .unwrap_or_else(|_| json!({}));
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

#[derive(Debug, Deserialize)]
struct OpenAIChatResponse {
    choices: Vec<OpenAIChatChoice>,
    usage: OpenAIUsage,
}

#[derive(Debug, Deserialize)]
struct OpenAIChatChoice {
    message: OpenAIResponseMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAIResponseMessage {
    #[serde(default)]
    content: String,
    #[serde(default)]
    tool_calls: Option<Vec<OpenAIToolCall>>,
}

#[derive(Debug, Deserialize)]
struct OpenAIToolCall {
    id: String,
    #[serde(rename = "type")]
    _type: String,
    function: OpenAIFunctionCall,
}

#[derive(Debug, Deserialize)]
struct OpenAIFunctionCall {
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct OpenAIUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_openai_config() {
        let config = OpenAICompatibleConfig::openai("test_key", "gpt-4o-mini");
        assert_eq!(config.base_url, "https://api.openai.com/v1");
        assert_eq!(config.model, "gpt-4o-mini");
    }

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
    fn test_moonshot_config() {
        let config = OpenAICompatibleConfig::moonshot("test_key", "kimi-k2.5");
        assert_eq!(config.base_url, "https://api.moonshot.cn/v1");
        assert_eq!(config.model, "kimi-k2.5");
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

    #[test]
    fn test_sse_parsing() {
        let chunk = "data: {\"test\": 1}\n\ndata: {\"test\": 2}\n\n";
        let events = parse_openai_sse(chunk);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].data, "{\"test\": 1}");
        assert_eq!(events[1].data, "{\"test\": 2}");
    }
}
