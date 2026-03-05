//! Anthropic provider implementation with native tool calling

use super::traits::{
    ChatMessage, ChatOptions, ChatResponse, MessageRole, Provider, StopReason, StreamEvent,
    TokenUsage, ToolDefinition,
};
use async_trait::async_trait;
use futures::{Stream, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::pin::Pin;
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

    /// Convert ChatMessage to Anthropic format
    fn convert_messages(&self, messages: &[ChatMessage]) -> Vec<AnthropicMessage> {
        messages
            .iter()
            .filter_map(|m| {
                // Handle tool results FIRST (before the role match that filters them out)
                if m.role == MessageRole::Tool {
                    debug!("Converting Tool message with {} content blocks", m.content.len());
                    
                    // Find ToolResult content block
                    for (idx, block) in m.content.iter().enumerate() {
                        debug!("  Content block [{}]: {:?}", idx, std::mem::discriminant(block));
                        if let crate::types::message::ContentBlock::ToolResult { tool_call_id, content, .. } = block {
                            // Extract text from the content blocks inside ToolResult
                            let content_text: String = content
                                .iter()
                                .filter_map(|b| match b {
                                    crate::types::message::ContentBlock::Text { text } => Some(text.clone()),
                                    _ => None,
                                })
                                .collect::<Vec<_>>()
                                .join("");
                            
                            debug!("  Found ToolResult: tool_call_id={}, content_len={}", tool_call_id, content_text.len());
                            
                            return Some(AnthropicMessage {
                                role: "user".to_string(),
                                content: Content::Blocks(vec![ContentBlock::ToolResult {
                                    tool_use_id: tool_call_id.clone(),
                                    content: content_text,
                                }]),
                            });
                        }
                    }
                    
                    // Fallback: try to extract text directly from content
                    debug!("  No ToolResult block found, trying fallback");
                    let content_text = m
                        .content
                        .iter()
                        .filter_map(|b| match b {
                            crate::types::message::ContentBlock::Text { text } => Some(text.clone()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("");
                    
                    let tool_use_id = m.tool_call_id.clone().unwrap_or_default();
                    debug!("  Fallback: tool_use_id={}, content_len={}", tool_use_id, content_text.len());
                    
                    return Some(AnthropicMessage {
                        role: "user".to_string(),
                        content: Content::Blocks(vec![ContentBlock::ToolResult {
                            tool_use_id,
                            content: content_text,
                        }]),
                    });
                }

                // Now handle other roles
                let role = match m.role {
                    MessageRole::User => "user",
                    MessageRole::Assistant => "assistant",
                    _ => return None,  // Filter out System (handled separately)
                };

                // Extract text content and tool calls
                let mut text_parts = Vec::new();
                let mut tool_calls = Vec::new();

                for block in &m.content {
                    match block {
                        crate::types::message::ContentBlock::Text { text } => {
                            text_parts.push(text.clone());
                        }
                        crate::types::message::ContentBlock::ToolCall {
                            id, name, arguments
                        } => {
                            tool_calls.push(AnthropicToolUse {
                                tool_type: "tool_use".to_string(),
                                id: id.clone(),
                                name: name.clone(),
                                input: arguments.clone(),
                            });
                        }
                        _ => {}
                    }
                }

                // Build content - can be text, tool_use, or tool_result
                if tool_calls.is_empty() {
                    Some(AnthropicMessage {
                        role: role.to_string(),
                        content: Content::Text(text_parts.join(" ")),
                    })
                } else {
                    // For assistant with tool calls, we need content blocks
                    let mut blocks: Vec<ContentBlock> = vec![];

                    // Add thinking text if present
                    if !text_parts.is_empty() {
                        blocks.push(ContentBlock::Text {
                            text: text_parts.join(" "),
                        });
                    }

                    // Add tool_use blocks
                    for tc in tool_calls {
                        blocks.push(ContentBlock::ToolUse {
                            id: tc.id,
                            name: tc.name,
                            input: tc.input,
                        });
                    }

                    Some(AnthropicMessage {
                        role: role.to_string(),
                        content: Content::Blocks(blocks),
                    })
                }
            })
            .collect()
    }

    /// Convert ToolDefinition to Anthropic format
    fn convert_tools(&self, tools: &[ToolDefinition]) -> Vec<AnthropicTool> {
        tools
            .iter()
            .map(|t| AnthropicTool {
                name: t.name.clone(),
                description: t.description.clone(),
                input_schema: t.parameters.clone(),
            })
            .collect()
    }

    /// Build request for chat with tools
    fn build_request(
        &self,
        messages: &[ChatMessage],
        tools: Option<&[ToolDefinition]>,
        options: &ChatOptions,
    ) -> AnthropicRequest {
        let anthropic_messages = self.convert_messages(messages);
        let anthropic_tools = tools.map(|t| self.convert_tools(t));

        // Debug: log converted messages
        debug!("Anthropic request messages:");
        for (i, msg) in anthropic_messages.iter().enumerate() {
            let content_preview = match &msg.content {
                Content::Text(text) => format!("[Text: {}]", text.chars().take(50).collect::<String>()),
                Content::Blocks(blocks) => {
                    let block_strs: Vec<String> = blocks.iter().map(|b| match b {
                        ContentBlock::Text { text } => format!("[Text: {}]", text.chars().take(30).collect::<String>()),
                        ContentBlock::ToolUse { id, name, .. } => format!("[ToolUse: {}]", name),
                        ContentBlock::ToolResult { tool_use_id, content } => format!("[ToolResult: {} -> {}]", tool_use_id, content.chars().take(30).collect::<String>()),
                    }).collect();
                    format!("[Blocks: {}]", block_strs.join(", "))
                }
            };
            debug!("  [{}] {}: {}", i, msg.role, content_preview);
        }

        let request = AnthropicRequest {
            model: self.config.model.clone(),
            messages: anthropic_messages,
            max_tokens: options.max_tokens.unwrap_or(self.config.max_tokens),
            temperature: options.temperature.or(Some(self.config.temperature)),
            tools: anthropic_tools,
            stream: false,
        };
        
        // Debug: log full request as JSON
        if let Ok(json) = serde_json::to_string_pretty(&request) {
            debug!("Anthropic API request:\n{}", json);
        }
        
        request
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn name(&self) -> &'static str {
        "anthropic"
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
        _system_prompt: Option<&str>,
        message: &str,
        _model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        let messages = vec![ChatMessage {
            role: MessageRole::User,
            content: vec![crate::types::message::ContentBlock::Text {
                text: message.to_string(),
            }],
            tool_calls: None,
            tool_call_id: None,
        }];

        let options = ChatOptions {
            temperature: Some(temperature as f32),
            max_tokens: Some(self.config.max_tokens),
            api_key: None,
            headers: std::collections::HashMap::new(),
        };

        let response = self.chat_with_tools(&messages, &[], &options).await?;

        // Extract text from response
        let text = response
            .content
            .iter()
            .filter_map(|b| match b {
                crate::types::message::ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ");

        Ok(text)
    }

    async fn chat_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        options: &ChatOptions,
    ) -> anyhow::Result<ChatResponse> {
        let request = self.build_request(messages, Some(tools), options);

        debug!("Sending tool-enabled request to Anthropic");

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

        let result: AnthropicResponse = response.json().await?;

        // Parse content blocks
        let mut content_blocks = Vec::new();
        let mut tool_calls = Vec::new();

        for block in result.content {
            match block {
                AnthropicContentBlock::Text { text } => {
                    content_blocks.push(crate::types::message::ContentBlock::Text { text });
                }
                AnthropicContentBlock::ToolUse { id, name, input } => {
                    tool_calls.push(crate::types::message::ContentBlock::ToolCall {
                        id,
                        name,
                        arguments: input,
                    });
                }
            }
        }

        // Determine stop reason
        let stop_reason = match result.stop_reason.as_deref() {
            Some("tool_use") => StopReason::ToolUse,
            Some("max_tokens") => StopReason::Length,
            _ => StopReason::Stop,
        };

        Ok(ChatResponse {
            content: content_blocks,
            tool_calls,
            stop_reason,
            usage: TokenUsage {
                input: result.usage.input_tokens as u64,
                output: result.usage.output_tokens as u64,
                total: (result.usage.input_tokens + result.usage.output_tokens) as u64,
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

        debug!("Sending streaming tool-enabled request to Anthropic");

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
            return Err(anyhow::anyhow!(
                "Anthropic API error: {status} - {error_text}"
            ));
        }

        let model = self.config.model.clone();
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
                        error!("Anthropic stream error: {}", e);
                        None
                    }
                }
            })
            .flat_map(|chunk| {
                // Parse SSE events
                let events = parse_anthropic_sse_events(&chunk);
                futures::stream::iter(events)
            })
            .scan(
                AnthropicStreamState::new(model.clone(), provider.clone()),
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
        // Use default implementation
        <Self as Provider>::complete_stream(self, prompt, event_tx, run_id).await
    }
}

// ============================================================================
// Stream State for Anthropic
// ============================================================================

struct AnthropicStreamState {
    model: String,
    provider: String,
    text_buffer: String,
    current_tool_id: Option<String>,
    current_tool_name: Option<String>,
    current_tool_input: String,
}

impl AnthropicStreamState {
    fn new(model: String, provider: String) -> Self {
        Self {
            model,
            provider,
            text_buffer: String::new(),
            current_tool_id: None,
            current_tool_name: None,
            current_tool_input: String::new(),
        }
    }

    fn process_event(
        &mut self,
        event: AnthropicSseEvent,
    ) -> anyhow::Result<Option<StreamEvent>> {
        if event.data.trim() == "[DONE]" {
            return Ok(Some(StreamEvent::Done {
                stop_reason: StopReason::Stop,
            }));
        }

        let event_json: Value = serde_json::from_str(&event.data)
            .map_err(|e| anyhow::anyhow!("JSON parse error: {e}"))?;

        let event_type = event_json.get("type").and_then(|t| t.as_str());

        match event_type {
            Some("content_block_start") => {
                if let Some(block) = event_json.get("content_block") {
                    let block_type = block.get("type").and_then(|t| t.as_str());
                    match block_type {
                        Some("text") => {
                            return Ok(Some(StreamEvent::TextStart { content_index: 0 }));
                        }
                        Some("tool_use") => {
                            self.current_tool_id = block.get("id").and_then(|i| i.as_str()).map(String::from);
                            self.current_tool_name = block.get("name").and_then(|n| n.as_str()).map(String::from);
                            return Ok(Some(StreamEvent::ToolCallStart { content_index: 1 }));
                        }
                        _ => {}
                    }
                }
            }
            Some("content_block_delta") => {
                if let Some(delta) = event_json.get("delta") {
                    let delta_type = delta.get("type").and_then(|t| t.as_str());
                    match delta_type {
                        Some("text_delta") => {
                            if let Some(text) = delta.get("text").and_then(|t| t.as_str()) {
                                self.text_buffer.push_str(text);
                                return Ok(Some(StreamEvent::TextDelta {
                                    content_index: 0,
                                    delta: text.to_string(),
                                }));
                            }
                        }
                        Some("input_json_delta") => {
                            if let Some(partial) = delta.get("partial_json").and_then(|p| p.as_str()) {
                                self.current_tool_input.push_str(partial);
                            }
                        }
                        _ => {}
                    }
                }
            }
            Some("content_block_stop") => {
                // Check if we were building a tool call
                if let (Some(id), Some(name)) = (
                    self.current_tool_id.take(),
                    self.current_tool_name.take(),
                ) {
                    let input = serde_json::from_str(&self.current_tool_input)
                        .unwrap_or_else(|_| serde_json::json!({}));
                    self.current_tool_input.clear();

                    return Ok(Some(StreamEvent::ToolCallEnd {
                        content_index: 1,
                        tool_call: crate::types::message::ContentBlock::ToolCall {
                            id,
                            name,
                            arguments: input,
                        },
                    }));
                } else if !self.text_buffer.is_empty() {
                    return Ok(Some(StreamEvent::TextEnd {
                        content_index: 0,
                        content: self.text_buffer.clone(),
                    }));
                }
            }
            Some("message_stop") => {
                return Ok(Some(StreamEvent::Done {
                    stop_reason: StopReason::Stop,
                }));
            }
            _ => {}
        }

        Ok(None)
    }
}

// ============================================================================
// SSE Parsing
// ============================================================================

#[derive(Debug, Clone)]
struct AnthropicSseEvent {
    data: String,
}

fn parse_anthropic_sse_events(chunk: &str) -> Vec<AnthropicSseEvent> {
    let mut events = Vec::new();
    let mut current_data = String::new();

    for line in chunk.lines() {
        if line.is_empty() {
            if !current_data.is_empty() {
                events.push(AnthropicSseEvent {
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
        events.push(AnthropicSseEvent { data: current_data });
    }

    events
}

// ============================================================================
// Anthropic API Types
// ============================================================================

#[derive(Debug, Serialize)]
struct AnthropicRequest {
    model: String,
    messages: Vec<AnthropicMessage>,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<AnthropicTool>>,
    stream: bool,
}

#[derive(Debug, Serialize)]
struct AnthropicMessage {
    role: String,
    content: Content,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum Content {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentBlock {
    Text { text: String },
    ToolUse { id: String, name: String, input: Value },
    ToolResult { tool_use_id: String, content: String },
}

#[derive(Debug, Serialize)]
struct AnthropicTool {
    name: String,
    description: String,
    #[serde(rename = "input_schema")]
    input_schema: Value,
}

#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    content: Vec<AnthropicContentBlock>,
    stop_reason: Option<String>,
    usage: AnthropicUsage,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicContentBlock {
    Text { text: String },
    ToolUse { id: String, name: String, input: Value },
}

#[derive(Debug, Deserialize)]
struct AnthropicUsage {
    input_tokens: u32,
    output_tokens: u32,
}

#[derive(Debug)]
struct AnthropicToolUse {
    tool_type: String,
    id: String,
    name: String,
    input: Value,
}

// Legacy types for backward compatibility
#[derive(Debug, Serialize, Deserialize)]
struct LegacyMessage {
    role: String,
    content: String,
}

#[derive(Debug, Serialize)]
struct LegacyMessagesRequest {
    model: String,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    messages: Vec<LegacyMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct LegacyMessagesResponse {
    content: Vec<LegacyContentBlock>,
    usage: LegacyUsage,
}

#[derive(Debug, Deserialize)]
struct LegacyContentBlock {
    #[serde(rename = "type")]
    block_type: String,
    text: String,
}

#[derive(Debug, Deserialize)]
struct LegacyUsage {
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
