//! Anthropic API adapter
//!
//! Handles conversion between unified types and Anthropic Messages API format.

use super::{extract_text_content, ToolCallAccumulator};
use crate::providers::transport::AuthConfig;
use crate::providers::types::{
    ChatOptions, ChatResponse, ContentBlock, LlmMessage, MessageRole, StopReason, StreamEvent,
    TokenUsage, ToolDefinition,
};
use crate::providers::DEFAULT_MAX_OUTPUT_TOKENS;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tracing::debug;

/// Anthropic API adapter
#[derive(Debug, Clone)]
pub struct AnthropicAdapter {
    base_url: String,
    extra_headers: Vec<(String, String)>,
    /// Accumulates input + cache tokens from `message_start` for usage
    /// tracking. Carries the full `AnthropicUsage` block so the
    /// `message_delta` path can pair its `output_tokens` with the
    /// `input_tokens` / `cache_*_tokens` reported at stream start.
    pending_input_tokens: Arc<Mutex<Option<AnthropicUsage>>>,
    /// Accumulates tool call parts during streaming
    tool_call_accumulator: ToolCallAccumulator,
}

impl AnthropicAdapter {
    /// Create a new Anthropic adapter.
    #[must_use]
    pub fn new() -> Self {
        Self {
            base_url: "https://api.anthropic.com".to_string(),
            extra_headers: vec![("anthropic-version".to_string(), "2023-06-01".to_string())],
            pending_input_tokens: Arc::new(Mutex::new(None)),
            tool_call_accumulator: ToolCallAccumulator::new(),
        }
    }

    /// Create with custom base URL (for Kimi Code and other Anthropic-compatible providers)
    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// Add an extra header
    #[must_use]
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.extra_headers.push((name.into(), value.into()));
        self
    }
}

impl Default for AnthropicAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl AnthropicAdapter {
    /// Convert unified messages to Anthropic format
    ///
    /// Anthropic uses a different format:
    /// - System prompt is separate from messages
    /// - Messages only have "user" and "assistant" roles
    /// - Tool results are sent as user messages with `tool_result` content
    fn convert_messages(&self, messages: &[LlmMessage]) -> (Option<String>, Vec<AnthropicMessage>) {
        let mut system_prompt = None;
        let mut anthropic_messages = Vec::new();

        for msg in messages {
            match msg.role {
                MessageRole::System => {
                    // System messages go in the system parameter, not messages array
                    system_prompt = Some(extract_text_content(&msg.content));
                }
                MessageRole::User => {
                    anthropic_messages.push(AnthropicMessage {
                        role: "user".to_string(),
                        content: Content::Text(extract_text_content(&msg.content)),
                    });
                }
                MessageRole::Assistant => {
                    let mut blocks = Vec::new();

                    // Add text content
                    for block in &msg.content {
                        match block {
                            ContentBlock::Text { text } => {
                                blocks.push(AnthropicContentBlock::Text { text: text.clone() });
                            }
                            ContentBlock::ToolCall {
                                id,
                                name,
                                arguments,
                            } => {
                                blocks.push(AnthropicContentBlock::ToolUse {
                                    id: id.clone(),
                                    name: name.clone(),
                                    input: arguments.clone(),
                                });
                            }
                            _ => {}
                        }
                    }

                    let content = if blocks.len() == 1 {
                        match &blocks[0] {
                            AnthropicContentBlock::Text { text } => Content::Text(text.clone()),
                            _ => Content::Blocks(blocks),
                        }
                    } else {
                        Content::Blocks(blocks)
                    };

                    anthropic_messages.push(AnthropicMessage {
                        role: "assistant".to_string(),
                        content,
                    });
                }
                MessageRole::Tool => {
                    // Tool results become user messages with tool_result blocks
                    let mut tool_result_blocks = Vec::new();

                    for block in &msg.content {
                        if let ContentBlock::ToolResult {
                            tool_call_id,
                            name: _,
                            content,
                            is_error,
                        } = block
                        {
                            let result_text = extract_text_content(content);
                            tool_result_blocks.push(AnthropicContentBlock::ToolResult {
                                tool_use_id: tool_call_id.clone(),
                                content: result_text,
                                is_error: Some(*is_error),
                            });
                        }
                    }

                    if !tool_result_blocks.is_empty() {
                        let content = if tool_result_blocks.len() == 1 {
                            match &tool_result_blocks[0] {
                                AnthropicContentBlock::ToolResult {
                                    tool_use_id,
                                    content,
                                    is_error,
                                } => Content::Blocks(vec![AnthropicContentBlock::ToolResult {
                                    tool_use_id: tool_use_id.clone(),
                                    content: content.clone(),
                                    is_error: *is_error,
                                }]),
                                _ => Content::Blocks(tool_result_blocks),
                            }
                        } else {
                            Content::Blocks(tool_result_blocks)
                        };

                        anthropic_messages.push(AnthropicMessage {
                            role: "user".to_string(),
                            content,
                        });
                    }
                }
            }
        }

        (system_prompt, anthropic_messages)
    }

    /// Convert tool definitions to Anthropic format.
    ///
    /// Strips JSON-Schema combinators (`anyOf`, `oneOf`, `allOf`) from
    /// `input_schema` before sending. The upstream Anthropic API accepts
    /// these keywords, but several Anthropic-compatible providers don't:
    /// Kimi's `https://api.minimaxi.com/anthropic` shim returns 429
    /// "engine overloaded" (instead of a proper 400) when a tool's
    /// `input_schema` contains `anyOf`. The combinators are a
    /// documentation nicety for our tools (`CronDelete` and `TaskUpdate`
    /// use them to say "either id or label"), not a functional
    /// requirement — the validation lives on the server side anyway
    /// because our tool executor rejects missing required fields with a
    /// clear message. See tests/cli_providers.rs (the kimi smoke test
    /// failed for ~60s straight before this strip) and the reproduce
    /// script in `scripts/bisect_kimi_anyof.py` for the original
    /// diagnostic.
    fn convert_tools(&self, tools: &[ToolDefinition]) -> Vec<AnthropicTool> {
        tools
            .iter()
            .map(|t| AnthropicTool {
                name: t.name.clone(),
                description: t.description.clone(),
                input_schema: strip_schema_combinators(&t.parameters),
            })
            .collect()
    }
}

/// Remove JSON-Schema combinators (`anyOf`, `oneOf`, `allOf`) from a
/// value, recursively. Only strips at the same level it sees them;
/// nested object schemas are walked. Returns the same JSON value
/// shape, just without the combinator keys.
fn strip_schema_combinators(value: &serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    match value {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                if matches!(k.as_str(), "anyOf" | "oneOf" | "allOf") {
                    continue;
                }
                out.insert(k.clone(), strip_schema_combinators(v));
            }
            Value::Object(out)
        }
        Value::Array(arr) => Value::Array(arr.iter().map(strip_schema_combinators).collect()),
        other => other.clone(),
    }
}

impl super::ApiAdapter for AnthropicAdapter {
    fn name(&self) -> &'static str {
        "anthropic"
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
        let (system, anthropic_messages) = self.convert_messages(messages);

        let mut body = json!({
            "model": model_id,
            "messages": anthropic_messages,
            "max_tokens": options.max_tokens.unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS),
            "stream": stream,
        });

        if let Some(system) = system {
            body["system"] = json!(system);
        }

        if let Some(temp) = options.temperature {
            body["temperature"] = json!(temp);
        }

        if let Some(tools) = tools {
            body["tools"] = json!(self.convert_tools(tools));
        }

        debug!(
            "Anthropic request: {}",
            serde_json::to_string_pretty(&body)?
        );

        Ok(("/v1/messages".to_string(), body))
    }

    fn parse_response(&self, model_id: &str, response: Value) -> Result<ChatResponse> {
        debug!(
            "Anthropic response: {}",
            serde_json::to_string_pretty(&response)?
        );

        let result: AnthropicResponse =
            serde_json::from_value(response).context("Failed to parse Anthropic response")?;

        let stop_reason = match result.stop_reason.as_deref() {
            Some("tool_use") => StopReason::ToolUse,
            Some("max_tokens") => StopReason::Length,
            _ => StopReason::Stop,
        };

        // Parse content blocks
        let mut content = Vec::new();
        let mut tool_calls = Vec::new();

        for block in result.content {
            match block {
                AnthropicResponseBlock::Text { text } => {
                    content.push(ContentBlock::Text { text });
                }
                AnthropicResponseBlock::ToolUse { id, name, input } => {
                    tool_calls.push(ContentBlock::ToolCall {
                        id,
                        name,
                        arguments: input,
                    });
                }
                AnthropicResponseBlock::Thinking {
                    thinking,
                    signature,
                } => {
                    // Store thinking blocks but don't include in regular content
                    // They will be handled separately via Thinking events
                    content.push(ContentBlock::Thinking {
                        text: thinking,
                        signature,
                    });
                }
            }
        }

        Ok(ChatResponse {
            content,
            tool_calls,
            stop_reason,
            usage: TokenUsage {
                input: u64::from(result.usage.input_tokens),
                output: u64::from(result.usage.output_tokens),
                total: u64::from(result.usage.input_tokens + result.usage.output_tokens),
                cache_creation_input_tokens: result.usage.cache_creation_input_tokens.map(u64::from),
                cache_read_input_tokens: result.usage.cache_read_input_tokens.map(u64::from),
                // Anthropic folds thinking/reasoning into output_tokens already;
                // no separate `reasoning_output_tokens` from the wire.
                reasoning_output_tokens: None,
            },
            provider: self.name().to_string(),
            model: model_id.to_string(),
        })
    }

    fn parse_sse_event(&self, model_id: &str, data: &str) -> Result<Option<StreamEvent>> {
        debug!("Parsing Anthropic SSE event: {}", data);
        let event: AnthropicSseEvent =
            serde_json::from_str(data).context("Failed to parse Anthropic SSE event")?;

        match event.event_type.as_deref() {
            Some("message_start") => {
                // Clear accumulator at start of new stream
                self.tool_call_accumulator.reset();
                // Store the full usage block (input + cache) for later
                // combination with output tokens emitted in `message_delta`.
                if let Some(usage) = event.message.and_then(|m| m.usage) {
                    *self.pending_input_tokens.lock().unwrap() = Some(usage);
                }
                Ok(Some(StreamEvent::Start {
                    provider: self.name().to_string(),
                    model: model_id.to_string(),
                }))
            }
            Some("content_block_start") => {
                if let Some(block) = event.content_block {
                    let idx = event.index.unwrap_or(0) as usize;
                    match block.block_type.as_str() {
                        "text" => Ok(Some(StreamEvent::TextStart { content_index: idx })),
                        "tool_use" => {
                            // Tool use start - store id and name via accumulator
                            if let (Some(id), Some(name)) = (block.id, block.name) {
                                // Only store input if it's non-empty and not just {}
                                let input_str = block.input.and_then(|v| {
                                    let s = v.to_string();
                                    if s.is_empty() || s == "{}" {
                                        None
                                    } else {
                                        Some(s)
                                    }
                                });
                                let _ = self.tool_call_accumulator.accumulate(
                                    idx,
                                    Some(id),
                                    Some(name),
                                    input_str,
                                );
                            }
                            Ok(Some(StreamEvent::ToolCallStart { content_index: idx }))
                        }
                        "thinking" => Ok(Some(StreamEvent::ThinkingStart { content_index: idx })),
                        _ => Ok(None),
                    }
                } else {
                    Ok(None)
                }
            }
            Some("content_block_delta") => {
                if let Some(delta) = event.delta {
                    let idx = event.index.unwrap_or(0) as usize;
                    match delta.delta_type.as_deref() {
                        Some("text_delta") => {
                            if let Some(text) = delta.text {
                                return Ok(Some(StreamEvent::TextDelta {
                                    content_index: idx,
                                    delta: text,
                                }));
                            }
                        }
                        Some("input_json_delta") => {
                            if let Some(partial) = delta.partial_json {
                                // Accumulate arguments and check if complete
                                let partial_clone = partial.clone();
                                if let Some(complete_tool) = self.tool_call_accumulator.accumulate(
                                    idx,
                                    None,
                                    None,
                                    Some(partial),
                                ) {
                                    return Ok(Some(StreamEvent::ToolCallEnd {
                                        content_index: idx,
                                        tool_call: complete_tool,
                                    }));
                                }
                                return Ok(Some(StreamEvent::ToolCallDelta {
                                    content_index: idx,
                                    delta: partial_clone,
                                }));
                            }
                        }
                        Some("thinking_delta") => {
                            if let Some(thinking) = delta.thinking {
                                return Ok(Some(StreamEvent::ThinkingDelta {
                                    content_index: idx,
                                    delta: thinking,
                                }));
                            }
                        }
                        _ => {}
                    }
                }
                Ok(None)
            }
            Some("content_block_stop") => {
                // Content block end - finalize any pending tool calls for this index
                let idx = event.index.unwrap_or(0) as usize;
                if let Some(tool_call) = self.tool_call_accumulator.finalize(idx) {
                    return Ok(Some(StreamEvent::ToolCallEnd {
                        content_index: idx,
                        tool_call,
                    }));
                }
                Ok(None)
            }
            Some("message_delta") => {
                // Check for usage output tokens first
                if let Some(delta_usage) = event.usage {
                    // The cached `message_start` usage block carries
                    // input_tokens + cache_creation + cache_read. Pull
                    // them and combine with the delta's output_tokens
                    // (and any cache fields the delta updates).
                    let pending = self.pending_input_tokens.lock().unwrap().clone();
                    let (input, cache_creation, cache_read) = match pending {
                        Some(u) => (
                            u.input_tokens,
                            u.cache_creation_input_tokens.unwrap_or(0),
                            u.cache_read_input_tokens.unwrap_or(0),
                        ),
                        None => (0, 0, 0),
                    };
                    // Delta may refresh cache fields; prefer delta values
                    // when present, fall back to `message_start`.
                    let cache_creation =
                        delta_usage.cache_creation_input_tokens.unwrap_or(cache_creation);
                    let cache_read =
                        delta_usage.cache_read_input_tokens.unwrap_or(cache_read);
                    let output = delta_usage.output_tokens;
                    return Ok(Some(StreamEvent::Usage {
                        input: u64::from(input),
                        output: u64::from(output),
                        total: u64::from(input + output),
                        cache_creation_input_tokens: u64::from(cache_creation),
                        cache_read_input_tokens: u64::from(cache_read),
                        reasoning_output_tokens: 0,
                    }));
                }
                if let Some(stop_reason) = event.stop_reason {
                    let reason = match stop_reason.as_str() {
                        "tool_use" => StopReason::ToolUse,
                        "max_tokens" => StopReason::Length,
                        _ => StopReason::Stop,
                    };
                    Ok(Some(StreamEvent::Done {
                        stop_reason: reason,
                    }))
                } else {
                    Ok(None)
                }
            }
            Some("message_stop") => {
                // Clear accumulator at end of stream
                self.tool_call_accumulator.reset();
                Ok(Some(StreamEvent::Done {
                    stop_reason: StopReason::Stop,
                }))
            }
            _ => Ok(None),
        }
    }

    fn auth_config(&self, api_key: &str) -> AuthConfig {
        AuthConfig::Header {
            name: "x-api-key".to_string(),
            value: api_key.to_string(),
        }
    }

    fn extra_headers(&self) -> Vec<(String, String)> {
        self.extra_headers.clone()
    }
}

// Anthropic API types

#[derive(Debug, Serialize)]
struct AnthropicMessage {
    role: String,
    content: Content,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum Content {
    Text(String),
    Blocks(Vec<AnthropicContentBlock>),
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
}

#[derive(Debug, Serialize)]
struct AnthropicTool {
    name: String,
    description: String,
    input_schema: Value,
}

#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    content: Vec<AnthropicResponseBlock>,
    stop_reason: Option<String>,
    usage: AnthropicUsage,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicResponseBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    /// Thinking block for reasoning models (Claude 3.7, Kimi K2.5, etc.)
    Thinking {
        thinking: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
}

#[derive(Debug, Clone, Deserialize)]
struct AnthropicUsage {
    input_tokens: u32,
    output_tokens: u32,
    /// Tokens billed at the cache-write rate for newly cached prompt
    /// prefixes. Optional; only present on requests that opt into
    /// prompt caching.
    #[serde(default)]
    cache_creation_input_tokens: Option<u32>,
    /// Tokens billed at the cache-read rate for cached prompt prefix
    /// matches. Optional; only present on cache-hit requests.
    #[serde(default)]
    cache_read_input_tokens: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct AnthropicMessageStartInfo {
    usage: Option<AnthropicUsage>,
}

#[derive(Debug, Deserialize)]
struct AnthropicDeltaUsage {
    output_tokens: u32,
    /// Cache fields may be updated on the delta event itself
    /// (Anthropic's `message_delta.usage` reports the *final* cache
    /// breakdown, which can differ from the `message_start` snapshot).
    #[serde(default)]
    cache_creation_input_tokens: Option<u32>,
    #[serde(default)]
    cache_read_input_tokens: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct AnthropicSseEvent {
    #[serde(rename = "type")]
    event_type: Option<String>,
    index: Option<u32>,
    #[serde(rename = "content_block")]
    content_block: Option<AnthropicContentBlockInfo>,
    delta: Option<AnthropicDelta>,
    #[serde(rename = "stop_reason")]
    stop_reason: Option<String>,
    // New fields for usage tracking:
    message: Option<AnthropicMessageStartInfo>,
    usage: Option<AnthropicDeltaUsage>,
}

#[derive(Debug, Deserialize)]
struct AnthropicContentBlockInfo {
    #[serde(rename = "type")]
    block_type: String,
    /// Tool call ID (for `tool_use` blocks)
    id: Option<String>,
    /// Tool name (for `tool_use` blocks)
    name: Option<String>,
    /// Tool input (for `tool_use` blocks)
    input: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct AnthropicDelta {
    #[serde(rename = "type")]
    delta_type: Option<String>, // Made optional - message_delta events don't have this
    text: Option<String>,
    #[serde(rename = "partial_json")]
    partial_json: Option<String>,
    /// Thinking content for reasoning models (Kimi K2.5, Claude 3.7, etc.)
    thinking: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::adapters::ApiAdapter;

    #[test]
    fn test_adapter_creation() {
        let adapter = AnthropicAdapter::new();
        assert_eq!(adapter.name(), "anthropic");
        assert_eq!(adapter.name(), "anthropic");
        assert_eq!(adapter.base_url(), "https://api.anthropic.com");
    }

    #[test]
    fn test_convert_messages_with_system() {
        let adapter = AnthropicAdapter::new();
        let messages = vec![
            LlmMessage::system("You are helpful"),
            LlmMessage::user("Hello"),
        ];

        let (system, anthropic_msgs) = adapter.convert_messages(&messages);
        assert_eq!(system, Some("You are helpful".to_string()));
        assert_eq!(anthropic_msgs.len(), 1);
        assert_eq!(anthropic_msgs[0].role, "user");
    }

    #[test]
    fn test_auth_config() {
        let adapter = AnthropicAdapter::new();
        let auth = adapter.auth_config("test_key");
        match auth {
            AuthConfig::Header { name, value } => {
                assert_eq!(name, "x-api-key");
                assert_eq!(value, "test_key");
            }
            _ => panic!("Expected Header auth"),
        }
    }

    #[test]
    fn test_parse_response() {
        let adapter = AnthropicAdapter::new();
        let response = json!({
            "content": [{
                "type": "text",
                "text": "Hello!"
            }],
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 10,
                "output_tokens": 5
            }
        });

        let parsed = adapter.parse_response("claude-3-sonnet", response).unwrap();
        assert_eq!(parsed.content.len(), 1);
        assert!(matches!(parsed.content[0], ContentBlock::Text { .. }));
        assert_eq!(parsed.usage.input, 10);
        assert_eq!(parsed.usage.output, 5);
    }

    #[test]
    fn test_parse_response_with_tool_use() {
        let adapter = AnthropicAdapter::new();
        let response = json!({
            "content": [
                {
                    "type": "text",
                    "text": "I'll help you with that."
                },
                {
                    "type": "tool_use",
                    "id": "tool_123",
                    "name": "search",
                    "input": {"query": "test"}
                }
            ],
            "stop_reason": "tool_use",
            "usage": {
                "input_tokens": 20,
                "output_tokens": 15
            }
        });

        let parsed = adapter.parse_response("claude-3-sonnet", response).unwrap();
        assert_eq!(parsed.content.len(), 1);
        assert_eq!(parsed.tool_calls.len(), 1);
        assert!(matches!(
            parsed.tool_calls[0],
            ContentBlock::ToolCall { .. }
        ));
        assert!(matches!(parsed.stop_reason, StopReason::ToolUse));
    }

    #[test]
    fn test_message_start_usage_extraction() {
        let adapter = AnthropicAdapter::new();
        // message_start event with usage info
        let data =
            r#"{"type":"message_start","message":{"usage":{"input_tokens":25,"output_tokens":0}}}"#;

        let event = adapter.parse_sse_event("claude-3-sonnet", data).unwrap();
        // Should return Start event
        assert!(matches!(
            event,
            Some(crate::providers::StreamEvent::Start { .. })
        ));
        // The full usage block should be cached for the matching
        // `message_delta` to combine with `output_tokens`.
        let stored = adapter.pending_input_tokens.lock().unwrap().clone();
        assert_eq!(stored.as_ref().map(|u| u.input_tokens), Some(25));
    }

    #[test]
    fn test_message_delta_usage_extraction() {
        let adapter = AnthropicAdapter::new();
        // First set up the input tokens (as if message_start was processed)
        *adapter.pending_input_tokens.lock().unwrap() = Some(AnthropicUsage {
            input_tokens: 25,
            output_tokens: 0,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        });

        // message_delta event with output tokens
        let data = r#"{"type":"message_delta","usage":{"output_tokens":12}}"#;

        let event = adapter.parse_sse_event("claude-3-sonnet", data).unwrap();
        match event {
            Some(crate::providers::StreamEvent::Usage {
                input,
                output,
                total,
                cache_creation_input_tokens,
                cache_read_input_tokens,
                reasoning_output_tokens,
            }) => {
                assert_eq!(input, 25);
                assert_eq!(output, 12);
                assert_eq!(total, 37);
                assert_eq!(cache_creation_input_tokens, 0);
                assert_eq!(cache_read_input_tokens, 0);
                assert_eq!(reasoning_output_tokens, 0);
            }
            _ => panic!("Expected Usage event, got {event:?}"),
        }
    }

    /// Cache tokens surface end-to-end through the streaming path.
    /// `message_start` carries the cache breakdown for the request;
    /// `message_delta` combines it with output_tokens and may also
    /// refresh cache fields with final values.
    #[test]
    fn test_anthropic_cache_tokens_round_trip() {
        let adapter = AnthropicAdapter::new();

        // message_start reports cache creation + read counts
        let start_data = r#"{
            "type":"message_start",
            "message":{"usage":{
                "input_tokens":100,
                "output_tokens":0,
                "cache_creation_input_tokens":1024,
                "cache_read_input_tokens":4096
            }}
        }"#;
        let _ = adapter.parse_sse_event("claude-3-sonnet", start_data).unwrap();

        // message_delta updates output_tokens; cache fields are not in
        // the delta here, so message_start values carry through.
        let delta_data = r#"{"type":"message_delta","usage":{"output_tokens":50}}"#;
        let event = adapter.parse_sse_event("claude-3-sonnet", delta_data).unwrap();
        match event {
            Some(crate::providers::StreamEvent::Usage {
                input,
                output,
                cache_creation_input_tokens,
                cache_read_input_tokens,
                ..
            }) => {
                assert_eq!(input, 100);
                assert_eq!(output, 50);
                assert_eq!(cache_creation_input_tokens, 1024);
                assert_eq!(cache_read_input_tokens, 4096);
            }
            _ => panic!("Expected Usage event"),
        }

        // A second delta with explicit cache values overrides the
        // message_start snapshot (Anthropic's contract: the delta
        // carries the *final* breakdown).
        let delta2 = r#"{"type":"message_delta","usage":{
            "output_tokens":10,
            "cache_creation_input_tokens":2048,
            "cache_read_input_tokens":8192
        }}"#;
        let event = adapter.parse_sse_event("claude-3-sonnet", delta2).unwrap();
        match event {
            Some(crate::providers::StreamEvent::Usage {
                cache_creation_input_tokens,
                cache_read_input_tokens,
                ..
            }) => {
                assert_eq!(cache_creation_input_tokens, 2048);
                assert_eq!(cache_read_input_tokens, 8192);
            }
            _ => panic!("Expected Usage event"),
        }
    }

    /// Non-streaming `parse_response` populates cache fields when the
    /// wire response includes them.
    #[test]
    fn test_anthropic_parse_response_cache_fields() {
        let adapter = AnthropicAdapter::new();
        let response = json!({
            "content": [{"type": "text", "text": "ok"}],
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 50,
                "output_tokens": 20,
                "cache_creation_input_tokens": 500,
                "cache_read_input_tokens": 2000
            }
        });
        let parsed = adapter.parse_response("claude-3-sonnet", response).unwrap();
        assert_eq!(parsed.usage.input, 50);
        assert_eq!(parsed.usage.output, 20);
        assert_eq!(parsed.usage.cache_creation_input_tokens, Some(500));
        assert_eq!(parsed.usage.cache_read_input_tokens, Some(2000));
        assert_eq!(parsed.usage.reasoning_output_tokens, None);
    }
}
