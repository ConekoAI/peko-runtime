//! `OpenAI` API adapter
//!
//! Handles conversion between unified types and `OpenAI` Chat Completions API format.

use super::{extract_text_content, role_to_string, ToolCallAccumulator};
use crate::providers::transport::AuthConfig;
use crate::providers::traits::{
    ChatOptions, ChatResponse, ContentBlock, LlmMessage, MessageRole, StopReason, StreamEvent,
    TokenUsage, ToolDefinition,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::debug;

/// `OpenAI` API adapter
#[derive(Debug, Clone)]
pub struct OpenAiAdapter {
    base_url: String,
    /// Accumulates tool call parts during streaming
    tool_call_accumulator: ToolCallAccumulator,
}

impl OpenAiAdapter {
    /// Create a new `OpenAI` adapter pointing at the canonical
    /// `https://api.openai.com/v1` base URL.
    #[must_use]
    pub fn new() -> Self {
        Self {
            base_url: "https://api.openai.com/v1".to_string(),
            tool_call_accumulator: ToolCallAccumulator::new(),
        }
    }

    /// Create with custom base URL (for Azure, OpenRouter, etc.)
    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }
}

impl Default for OpenAiAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl OpenAiAdapter {
    /// Convert unified messages to `OpenAI` format
    fn convert_messages(&self, messages: &[LlmMessage]) -> Vec<OpenAiMessage> {
        messages
            .iter()
            .map(|m| {
                let role = role_to_string(m.role);
                let content = extract_text_content(&m.content);

                // Extract tool calls from content blocks
                let tool_calls: Option<Vec<OpenAiToolCall>> = if m.role == MessageRole::Assistant {
                    let calls: Vec<_> = m
                        .content
                        .iter()
                        .filter_map(|b| match b {
                            ContentBlock::ToolCall {
                                id,
                                name,
                                arguments,
                            } => Some(OpenAiToolCall {
                                id: id.clone(),
                                tool_type: "function".to_string(),
                                function: OpenAiFunctionCall {
                                    name: name.clone(),
                                    arguments: arguments.to_string(),
                                },
                            }),
                            _ => None,
                        })
                        .collect();
                    if calls.is_empty() {
                        None
                    } else {
                        Some(calls)
                    }
                } else {
                    None
                };

                OpenAiMessage {
                    role: role.to_string(),
                    content,
                    tool_calls,
                    tool_call_id: m.tool_call_id.clone(),
                }
            })
            .collect()
    }

    /// Convert tool definitions to `OpenAI` format
    fn convert_tools(&self, tools: &[ToolDefinition]) -> Vec<OpenAiTool> {
        tools
            .iter()
            .map(|t| OpenAiTool {
                tool_type: "function".to_string(),
                function: OpenAiFunction {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    parameters: t.parameters.clone(),
                },
            })
            .collect()
    }
}

impl super::ApiAdapter for OpenAiAdapter {
    fn name(&self) -> &'static str {
        "openai"
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }

    fn supports_native_tools(&self) -> bool {
        true
    }

    fn build_request(
        &self,
        model_id: &str,
        messages: &[LlmMessage],
        tools: Option<&[ToolDefinition]>,
        options: &ChatOptions,
        stream: bool,
    ) -> Result<(String, Value)> {
        let openai_messages = self.convert_messages(messages);

        let mut body = json!({
            "model": model_id,
            "messages": openai_messages,
            "stream": stream,
        });

        if let Some(temp) = options.temperature {
            body["temperature"] = json!(temp);
        }

        if let Some(max_tokens) = options.max_tokens {
            body["max_tokens"] = json!(max_tokens);
        }

        if let Some(tools) = tools {
            body["tools"] = json!(self.convert_tools(tools));
            body["tool_choice"] = json!("auto");
        }

        // Add stream_options to include usage in streaming responses
        if stream {
            body["stream_options"] = json!({"include_usage": true});
        }

        debug!("OpenAI request: {}", serde_json::to_string_pretty(&body)?);

        Ok(("/chat/completions".to_string(), body))
    }

    fn parse_response(&self, model_id: &str, response: Value) -> Result<ChatResponse> {
        debug!(
            "OpenAI response: {}",
            serde_json::to_string_pretty(&response)?
        );

        let completion: OpenAiChatResponse =
            serde_json::from_value(response).context("Failed to parse OpenAI response")?;

        let choice = completion
            .choices
            .into_iter()
            .next()
            .context("No choices in OpenAI response")?;

        let stop_reason = match choice.finish_reason.as_deref() {
            Some("tool_calls") => StopReason::ToolUse,
            Some("length") => StopReason::Length,
            Some("stop") | None => StopReason::Stop,
            _ => StopReason::Stop,
        };

        let message = choice.message;

        // Extract content blocks
        let mut content = Vec::new();
        if !message.content.is_empty() {
            content.push(ContentBlock::Text {
                text: message.content,
            });
        }

        // Extract tool calls
        let tool_calls: Vec<ContentBlock> = message
            .tool_calls
            .into_iter()
            .flatten()
            .map(|tc| {
                let arguments =
                    serde_json::from_str(&tc.function.arguments).unwrap_or_else(|_| json!({}));
                ContentBlock::ToolCall {
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
                input: u64::from(completion.usage.prompt_tokens),
                output: u64::from(completion.usage.completion_tokens),
                total: u64::from(completion.usage.total_tokens),
                // OpenAI does not have cache_creation; only cache reads
                // via `prompt_tokens_details.cached_tokens`.
                cache_creation_input_tokens: None,
                cache_read_input_tokens: completion
                    .usage
                    .prompt_tokens_details
                    .as_ref()
                    .and_then(|d| d.cached_tokens)
                    .map(u64::from),
                reasoning_output_tokens: completion
                    .usage
                    .completion_tokens_details
                    .as_ref()
                    .and_then(|d| d.reasoning_tokens)
                    .map(u64::from),
            },
            provider: self.name().to_string(),
            model: model_id.to_string(),
        })
    }

    fn parse_sse_event(&self, _model_id: &str, data: &str) -> Result<Option<StreamEvent>> {
        if data.trim() == "[DONE]" {
            // Clear accumulator when stream ends
            self.tool_call_accumulator.reset();
            return Ok(Some(StreamEvent::Done {
                stop_reason: StopReason::Stop,
            }));
        }

        let chunk: OpenAiStreamChunk =
            serde_json::from_str(data).context("Failed to parse OpenAI SSE chunk")?;

        // Check for usage first (final chunk has usage but empty choices)
        if let Some(usage) = chunk.usage {
            return Ok(Some(StreamEvent::Usage {
                input: u64::from(usage.prompt_tokens),
                output: u64::from(usage.completion_tokens),
                total: u64::from(usage.total_tokens),
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: usage
                    .prompt_tokens_details
                    .as_ref()
                    .and_then(|d| d.cached_tokens)
                    .map_or(0, u64::from),
                reasoning_output_tokens: usage
                    .completion_tokens_details
                    .as_ref()
                    .and_then(|d| d.reasoning_tokens)
                    .map_or(0, u64::from),
            }));
        }

        let choice = match chunk.choices.into_iter().next() {
            Some(c) => c,
            None => return Ok(None),
        };

        let delta = choice.delta;

        // Handle text content
        if let Some(text) = delta.content {
            if !text.is_empty() {
                return Ok(Some(StreamEvent::TextDelta {
                    content_index: 0,
                    delta: text,
                }));
            }
        }

        // Handle tool calls - use shared accumulator
        if let Some(tool_calls) = delta.tool_calls {
            for tc in tool_calls {
                let idx = tc.index as usize;
                let id = tc.id.clone();
                let name = tc.function.as_ref().and_then(|f| f.name.clone());
                let arguments = tc.function.as_ref().and_then(|f| f.arguments.clone());

                // Check if this is a new tool call
                let is_new_call = id
                    .as_ref()
                    .is_some_and(|id_str| self.tool_call_accumulator.is_new_call(idx, id_str));

                // If this is the start of a new tool call, emit ToolCallStart first
                if is_new_call {
                    let _ = self.tool_call_accumulator.accumulate(
                        idx,
                        id.clone(),
                        name.clone(),
                        arguments.clone(),
                    );
                    return Ok(Some(StreamEvent::ToolCallStart { content_index: idx }));
                }

                // Accumulate parts and check for completion
                let complete =
                    self.tool_call_accumulator
                        .accumulate(idx, id, name, arguments.clone());

                // If we have a complete tool call, emit ToolCallEnd
                if let Some(complete_tool) = complete {
                    return Ok(Some(StreamEvent::ToolCallEnd {
                        content_index: idx,
                        tool_call: complete_tool,
                    }));
                }

                // Still accumulating, emit delta for progress tracking
                if let Some(args) = arguments {
                    return Ok(Some(StreamEvent::ToolCallDelta {
                        content_index: idx,
                        delta: args,
                    }));
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

            // If finish reason is tool_calls, clear the accumulator
            if reason == "tool_calls" {
                self.tool_call_accumulator.reset();
            }

            return Ok(Some(StreamEvent::Done { stop_reason }));
        }

        Ok(None)
    }

    fn auth_config(&self, api_key: &str) -> AuthConfig {
        AuthConfig::Bearer {
            token: api_key.to_string(),
        }
    }
}

// OpenAI API types

#[derive(Debug, Serialize)]
struct OpenAiMessage {
    role: String,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OpenAiToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct OpenAiTool {
    #[serde(rename = "type")]
    tool_type: String,
    function: OpenAiFunction,
}

#[derive(Debug, Serialize)]
struct OpenAiFunction {
    name: String,
    description: String,
    parameters: Value,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct OpenAiToolCall {
    id: String,
    #[serde(rename = "type")]
    tool_type: String,
    function: OpenAiFunctionCall,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct OpenAiFunctionCall {
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct OpenAiChatResponse {
    choices: Vec<OpenAiChoice>,
    usage: OpenAiUsage,
}

#[derive(Debug, Deserialize)]
struct OpenAiChoice {
    message: OpenAiResponseMessage,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiResponseMessage {
    #[serde(default)]
    content: String,
    #[serde(default)]
    tool_calls: Option<Vec<OpenAiToolCall>>,
}

#[derive(Debug, Deserialize)]
struct OpenAiUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
    /// OpenAI `prompt_tokens_details.cached_tokens`. Optional; only
    /// present on requests that opt into prompt caching. Billed at a
    /// discounted rate but still consumes input quota.
    #[serde(default)]
    prompt_tokens_details: Option<OpenAiPromptTokensDetails>,
    /// OpenAI `completion_tokens_details.reasoning_tokens`. Subset of
    /// `completion_tokens` for o-series reasoning. Optional.
    #[serde(default)]
    completion_tokens_details: Option<OpenAiCompletionTokensDetails>,
}

#[derive(Debug, Deserialize)]
struct OpenAiPromptTokensDetails {
    #[serde(default)]
    cached_tokens: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct OpenAiCompletionTokensDetails {
    #[serde(default)]
    reasoning_tokens: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct OpenAiStreamChunk {
    choices: Vec<OpenAiStreamChoice>,
    #[serde(default)]
    usage: Option<OpenAiUsage>, // Final chunk has usage + empty choices
}

#[derive(Debug, Deserialize)]
struct OpenAiStreamChoice {
    delta: OpenAiDelta,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct OpenAiDelta {
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<OpenAiToolCallDelta>>,
}

#[derive(Debug, Deserialize)]
struct OpenAiToolCallDelta {
    index: u32,
    id: Option<String>,
    function: Option<OpenAiDeltaFunction>,
}

#[derive(Debug, Deserialize)]
struct OpenAiDeltaFunction {
    name: Option<String>,
    arguments: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::adapters::ApiAdapter;

    #[test]
    fn test_adapter_creation() {
        let adapter = OpenAiAdapter::new();
        assert_eq!(adapter.name(), "openai");
        assert_eq!(adapter.name(), "openai");
    }

    #[test]
    fn test_convert_messages() {
        let adapter = OpenAiAdapter::new();
        let messages = vec![
            LlmMessage::system("You are helpful"),
            LlmMessage::user("Hello"),
        ];

        let (path, body) = adapter
            .build_request(
                "gpt-4o-mini",
                &messages,
                None,
                &ChatOptions::default(),
                false,
            )
            .unwrap();
        assert_eq!(path, "/chat/completions");
        assert_eq!(body["model"], "gpt-4o-mini");
    }

    #[test]
    fn test_parse_response() {
        let adapter = OpenAiAdapter::new();
        let response = json!({
            "choices": [{
                "message": {
                    "content": "Hello!",
                    "role": "assistant"
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 15
            }
        });

        let parsed = adapter.parse_response("gpt-4o-mini", response).unwrap();
        assert_eq!(parsed.content.len(), 1);
        assert!(matches!(parsed.content[0], ContentBlock::Text { .. }));
        assert_eq!(parsed.usage.total, 15);
    }

    #[test]
    fn test_parse_sse_text_delta() {
        let adapter = OpenAiAdapter::new();
        let data = r#"{"choices":[{"delta":{"content":"Hello"},"index":0}]}"#;

        let event = adapter.parse_sse_event("gpt-4o-mini", data).unwrap();
        assert!(matches!(
            event,
            Some(crate::providers::StreamEvent::TextDelta {
                content_index: 0,
                delta: _,
            })
        ));
    }

    #[test]
    fn test_parse_sse_with_usage() {
        let adapter = OpenAiAdapter::new();
        // Final chunk with usage and empty choices
        let data = r#"{"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}"#;

        let event = adapter.parse_sse_event("gpt-4o-mini", data).unwrap();
        match event {
            Some(crate::providers::StreamEvent::Usage {
                input,
                output,
                total,
                cache_creation_input_tokens,
                cache_read_input_tokens,
                reasoning_output_tokens,
            }) => {
                assert_eq!(input, 10);
                assert_eq!(output, 5);
                assert_eq!(total, 15);
                // No cache or reasoning fields in this fixture.
                assert_eq!(cache_creation_input_tokens, 0);
                assert_eq!(cache_read_input_tokens, 0);
                assert_eq!(reasoning_output_tokens, 0);
            }
            _ => panic!("Expected Usage event, got {event:?}"),
        }
    }

    /// `prompt_tokens_details.cached_tokens` and
    /// `completion_tokens_details.reasoning_tokens` surface in the
    /// streaming Usage event when the final chunk carries them.
    #[test]
    fn test_parse_sse_cached_and_reasoning_tokens() {
        let adapter = OpenAiAdapter::new();
        let data = r#"{
            "choices":[],
            "usage":{
                "prompt_tokens":1000,
                "completion_tokens":500,
                "total_tokens":1500,
                "prompt_tokens_details":{"cached_tokens":800},
                "completion_tokens_details":{"reasoning_tokens":200}
            }
        }"#;
        let event = adapter.parse_sse_event("gpt-4o-mini", data).unwrap();
        match event {
            Some(crate::providers::StreamEvent::Usage {
                input,
                output,
                cache_creation_input_tokens,
                cache_read_input_tokens,
                reasoning_output_tokens,
                ..
            }) => {
                assert_eq!(input, 1000);
                assert_eq!(output, 500);
                assert_eq!(cache_creation_input_tokens, 0);
                assert_eq!(cache_read_input_tokens, 800);
                assert_eq!(reasoning_output_tokens, 200);
            }
            _ => panic!("Expected Usage event"),
        }
    }

    /// Non-streaming parse_response populates cache + reasoning.
    #[test]
    fn test_parse_response_cached_and_reasoning_tokens() {
        let adapter = OpenAiAdapter::new();
        let response = json!({
            "choices": [{"message": {"content": "ok", "role": "assistant"}, "finish_reason": "stop"}],
            "usage": {
                "prompt_tokens": 1000,
                "completion_tokens": 500,
                "total_tokens": 1500,
                "prompt_tokens_details": {"cached_tokens": 800},
                "completion_tokens_details": {"reasoning_tokens": 200}
            }
        });
        let parsed = adapter.parse_response("gpt-4o-mini", response).unwrap();
        assert_eq!(parsed.usage.input, 1000);
        assert_eq!(parsed.usage.output, 500);
        assert_eq!(parsed.usage.cache_creation_input_tokens, None);
        assert_eq!(parsed.usage.cache_read_input_tokens, Some(800));
        assert_eq!(parsed.usage.reasoning_output_tokens, Some(200));
    }
}
