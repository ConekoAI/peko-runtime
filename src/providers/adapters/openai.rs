//! `OpenAI` API adapter
//!
//! Handles conversion between unified types and `OpenAI` Chat Completions API format.

use super::{extract_text_content, role_to_string, ToolCallAccumulator};
use crate::common::types::message::ImageSource;
use crate::providers::cache_retention::CacheRetention;
use crate::providers::traits::{
    ChatOptions, ChatResponse, ContentBlock, LlmMessage, MessageRole, StopReason, StreamEvent,
    TokenUsage, ToolChoice, ToolDefinition,
};
use crate::providers::transport::AuthConfig;
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
                // F28: emit content as a string for text-only blocks
                // (matches the pre-F28 wire shape byte-for-byte) or as
                // a content-part array when the message contains an
                // Image block. The array path is OpenAI's
                // multimodal-input shape per
                // https://platform.openai.com/docs/guides/vision
                let content = build_chat_completions_content(&m.content);

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
            body["tool_choice"] = json!(tool_choice_openai(&options.tool_choice));
        }

        // F26: `parallel_tool_calls: Some(false)` forces serialized
        // tool calls. Default (`None`) suppresses emission — the
        // server then defaults to parallel on supported models.
        if let Some(parallel) = options.parallel_tool_calls {
            body["parallel_tool_calls"] = json!(parallel);
        }

        // F26: `service_tier` only emits when the caller picked a
        // non-default tier (OpenAI's `None` for `ServiceTier::None`
        // means "do not emit", matching the pre-F26 wire shape).
        if let Some(tier) = options.service_tier.as_wire_str() {
            body["service_tier"] = json!(tier);
        }

        // Add stream_options to include usage in streaming responses
        if stream {
            body["stream_options"] = json!({"include_usage": true});
        }

        // F23: prompt-cache wiring. OpenAI Chat Completions accepts
        // `prompt_cache_key` (≤64 UTF-32 chars) and
        // `prompt_cache_retention` ("24h") on the body. `CacheRetention::None`
        // opts out; the engine loop already gates on
        // `Provider::supports_prompt_cache_control()` so an unsupported
        // adapter never reaches here with these fields set.
        if options.cache_retention.is_enabled() {
            if let Some(key) = options.prompt_cache_key.as_deref() {
                body["prompt_cache_key"] = json!(key);
            }
            if options.cache_retention == CacheRetention::Long {
                body["prompt_cache_retention"] = json!("24h");
            }
        }

        // F25: reasoning-effort knob. Maps to Chat Completions'
        // `reasoning_effort` string. `Adaptive` has no Chat
        // Completions counterpart — drop it (callers targeting
        // adaptive should use the Responses adapter instead).
        if let Some(effort) = options.thinking_effort.as_chat_completions_str() {
            body["reasoning_effort"] = json!(effort);
        }

        // F29: per-provider compat emit. Chat Completions speaks
        // both `reasoning_effort` (canonical) and the provider-
        // specific `extra_body.thinking` shape that DeepSeek's R1
        // family expects. Only DeepSeek is wired today — other
        // compat flavours (Qwen/Zai/OpenRouter/Together) are
        // surfaced via `compat_override` for follow-up PRs to land
        // the per-flavour wire rules without a flag-day refactor.
        //
        // Callers opt in by populating `ChatOptions::compat_override`
        // explicitly (typically driven by the resolver binding
        // `ModelConfig::compat` to the call). Defaults stay
        // unaffected — `compat_override: None` produces the pre-F29
        // wire shape byte-for-byte.
        if options.thinking_effort.is_enabled() {
            if let Some(compat) = options.compat_override {
                if compat.thinking_format == crate::providers::traits::ThinkingFormat::DeepSeek {
                    // DeepSeek-R1: `thinking: {type:"enabled"}` is the
                    // canonical toggle. Wire effort alongside via
                    // `reasoning_effort` so DeepSeek's router picks
                    // the right reasoning model variant.
                    body["thinking"] = json!({"type": "enabled"});
                }
                // Kimi/Zai/Qwen/OpenRouter/Together wire shapes are
                // concrete per-spec but land in F30+ so the F29 PR
                // stays focused on the type surface + DeepSeek wire.
                let _ = compat.deferred_tools_mode;
            }
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

        // F25: reasoning surface on the blocking path comes FIRST
        // so callers can render reasoning before the visible
        // answer. Probe in the same order as the streaming key
        // probe (`reasoning_content` → `reasoning_details` →
        // `reasoning`) and emit a `Thinking` block when present.
        let mut content = Vec::new();
        if let Some(text) = message
            .reasoning_content
            .as_deref()
            .filter(|s| !s.is_empty())
        {
            content.push(ContentBlock::Thinking {
                text: text.to_string(),
                signature: None,
            });
        } else if let Some(details) = message.reasoning_details.as_ref() {
            let text: String = details
                .iter()
                .filter_map(|d| d.text.as_deref())
                .collect::<Vec<_>>()
                .join("");
            if !text.is_empty() {
                content.push(ContentBlock::Thinking {
                    text,
                    signature: None,
                });
            }
        } else if let Some(value) = message.reasoning.as_ref() {
            let text = reasoning_text_from_reasoning_field(value);
            if !text.is_empty() {
                content.push(ContentBlock::Thinking {
                    text,
                    signature: None,
                });
            }
        }

        // Visible text comes after reasoning so the canonical order
        // matches the streaming delta path (which interleaves
        // ThinkingDelta before TextDelta in time).
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

        // F25: reasoning-content probe. Some providers (Moonshot/Kimi,
        // DeepSeek, OpenAI o-series) emit reasoning under different
        // keys: `reasoning_content` (string) or
        // `reasoning_details[*].text` (array). The server decides
        // which to use — we probe in a fixed order and surface the
        // first hit as a ThinkingDelta so the engine loop can
        // display it alongside the assistant's text.
        if let Some(text) = choice
            .reasoning_content
            .as_deref()
            .filter(|s| !s.is_empty())
        {
            return Ok(Some(StreamEvent::ThinkingDelta {
                content_index: 0,
                delta: text.to_string(),
            }));
        }
        if let Some(details) = choice.reasoning_details.as_ref() {
            let text: String = details
                .iter()
                .filter_map(|d| d.text.as_deref())
                .collect::<Vec<_>>()
                .join("");
            if !text.is_empty() {
                return Ok(Some(StreamEvent::ThinkingDelta {
                    content_index: 0,
                    delta: text,
                }));
            }
        }
        if let Some(value) = choice.reasoning.as_ref() {
            let text = reasoning_text_from_reasoning_field(value);
            if !text.is_empty() {
                return Ok(Some(StreamEvent::ThinkingDelta {
                    content_index: 0,
                    delta: text,
                }));
            }
        }

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
    /// F28: `Value` so we can emit either the pre-F28
    /// `content: "..."` string for text-only messages or the
    /// content-part array (`[{type:"text",...}, {type:"image_url",...}]`)
    /// when the message carries an `Image` block. Default serde
    /// behaviour keeps the existing string-shape serialisation when
    /// we pass `Value::String(s)`.
    content: Value,
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
    /// F25: reasoning surface. `reasoning_content` (Moonshot/Kimi)
    /// or `reasoning_details[*].text` (OpenAI o-series). Captured
    /// but not always surfaced — the engine loop currently ignores
    /// reasoning on the blocking path; we keep the field
    /// available for callers that want it.
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    reasoning_details: Option<Vec<OpenAiReasoningDetail>>,
    /// Some shims emit a bare `reasoning` value (string or array).
    #[serde(default)]
    reasoning: Option<Value>,
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
    /// F25: non-standard SSE extras some providers emit alongside
    /// the delta — `reasoning_content` (Moonshot/Kimi/DeepSeek),
    /// `reasoning_details` (OpenAI o-series). Field-level serde so
    /// unknown keys from other providers stay invisible.
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    reasoning_details: Option<Vec<OpenAiReasoningDetail>>,
    /// Some shims emit a bare `reasoning` field instead of the
    /// `*_content` / `*_details` variants. Captured here as a
    /// `Value` so the caller can probe string vs array.
    #[serde(default)]
    reasoning: Option<Value>,
}

#[derive(Debug, Default, Deserialize)]
struct OpenAiReasoningDetail {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    r#type: Option<String>,
}

/// Probe a reasoning field (`reasoning_content`, `reasoning_details`,
/// or a bare `reasoning` value) and flatten it into a single string.
/// Mirrors kimi-code's `openai-legacy.ts:77-89` key probe.
fn reasoning_text_from_reasoning_field(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Array(arr) => arr
            .iter()
            .filter_map(|v| v.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

/// F26: project `ToolChoice` onto Chat Completions' wire shape.
/// `Forced(name)` becomes `{type:"function", function:{name}}`;
/// every other variant is a plain string.
fn tool_choice_openai(choice: &ToolChoice) -> Value {
    match choice {
        ToolChoice::Auto => json!("auto"),
        ToolChoice::None => json!("none"),
        ToolChoice::Required => json!("required"),
        ToolChoice::Forced(name) => json!({
            "type": "function",
            "function": {"name": name},
        }),
    }
}

/// F28: project `ContentBlock` slices onto Chat Completions'
/// `content` field. Text-only messages emit the pre-F28 string
/// (`"hello"`); messages carrying an `Image` block emit a
/// content-part array of the form `[{type:"text",...}, {type:"image_url",
/// image_url:{url,...}}]` per OpenAI's Vision guide.
///
/// `ImageSource::Url { url }` is passed through verbatim;
/// `ImageSource::Base64 { data }` is joined with the block's
/// `mime_type` into a `data:` URL (the form OpenAI's chat.completions
/// vision accepts for inline images).
fn build_chat_completions_content(blocks: &[ContentBlock]) -> Value {
    let has_image = blocks
        .iter()
        .any(|b| matches!(b, ContentBlock::Image { .. }));
    if !has_image {
        // Pre-F28 wire shape — text-only messages still emit `content: "..."`.
        return Value::String(extract_text_content(blocks));
    }

    let mut parts: Vec<Value> = Vec::with_capacity(blocks.len());
    for block in blocks {
        match block {
            ContentBlock::Text { text } => {
                parts.push(json!({"type": "text", "text": text}));
            }
            ContentBlock::Image { source, mime_type } => {
                let url = match source {
                    ImageSource::Url { url } => url.clone(),
                    ImageSource::Base64 { data } => {
                        format!("data:{};base64,{}", mime_type, data)
                    }
                };
                parts.push(json!({
                    "type": "image_url",
                    "image_url": {"url": url},
                }));
            }
            // Tool-call / tool-result / thinking blocks do not appear
            // on user/assistant content arrays in Chat Completions
            // (they live on their own fields). Silently skip so a
            // future caller's accidental mix doesn't crash.
            _ => {}
        }
    }
    Value::Array(parts)
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
    use crate::providers::traits::{ServiceTier, ThinkingEffort};

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

    // ---------- F23: prompt-cache wiring on the request body ----------

    /// `prompt_cache_key` lands on the wire when the caller supplies it
    /// (and the retention policy is enabled — which `Default` is).
    #[test]
    fn test_build_request_prompt_cache_key_emitted() {
        let adapter = OpenAiAdapter::new();
        let messages = vec![LlmMessage::user("hi")];
        let options = ChatOptions {
            prompt_cache_key: Some("sess-1".to_string()),
            ..Default::default()
        };

        let (_, body) = adapter
            .build_request("gpt-4o-mini", &messages, None, &options, false)
            .unwrap();

        assert_eq!(body["prompt_cache_key"], "sess-1");
    }

    /// `CacheRetention::Long` sets `prompt_cache_retention = "24h"` on
    /// the body so OpenAI retains the cached prefix for a day.
    #[test]
    fn test_build_request_long_retention_sets_24h() {
        let adapter = OpenAiAdapter::new();
        let messages = vec![LlmMessage::user("hi")];
        let options = ChatOptions {
            cache_retention: CacheRetention::Long,
            prompt_cache_key: Some("sess-1".to_string()),
            ..Default::default()
        };

        let (_, body) = adapter
            .build_request("gpt-4o-mini", &messages, None, &options, false)
            .unwrap();

        assert_eq!(body["prompt_cache_key"], "sess-1");
        assert_eq!(body["prompt_cache_retention"], "24h");
    }

    /// `CacheRetention::Default` omits `prompt_cache_retention` so
    /// OpenAI uses its standard TTL (no override field).
    #[test]
    fn test_build_request_default_retention_omits_retention_field() {
        let adapter = OpenAiAdapter::new();
        let messages = vec![LlmMessage::user("hi")];
        let options = ChatOptions {
            prompt_cache_key: Some("sess-1".to_string()),
            ..Default::default()
        };

        let (_, body) = adapter
            .build_request("gpt-4o-mini", &messages, None, &options, false)
            .unwrap();

        assert_eq!(body["prompt_cache_key"], "sess-1");
        assert!(
            body.get("prompt_cache_retention").is_none(),
            "Default retention should not emit a retention field"
        );
    }

    /// `CacheRetention::None` strips both cache-key fields, so the
    /// request body matches the pre-F23 wire shape even if the caller
    /// somehow set a key (defensive — the engine loop already filters).
    #[test]
    fn test_build_request_retention_none_strips_cache_fields() {
        let adapter = OpenAiAdapter::new();
        let messages = vec![LlmMessage::user("hi")];
        let options = ChatOptions {
            cache_retention: CacheRetention::None,
            prompt_cache_key: Some("sess-1".to_string()),
            ..Default::default()
        };

        let (_, body) = adapter
            .build_request("gpt-4o-mini", &messages, None, &options, false)
            .unwrap();

        assert!(body.get("prompt_cache_key").is_none());
        assert!(body.get("prompt_cache_retention").is_none());
    }

    // ---------- F25: reasoning-effort wiring ----------

    /// Default `ChatOptions` keeps the request body identical to the
    /// pre-F25 shape — no `reasoning_effort` field, no streaming
    /// `extra` probes.
    #[test]
    fn test_build_request_thinking_effort_none_omits_field() {
        let adapter = OpenAiAdapter::new();
        let messages = vec![LlmMessage::user("hi")];
        let (_, body) = adapter
            .build_request(
                "gpt-4o-mini",
                &messages,
                None,
                &ChatOptions::default(),
                false,
            )
            .unwrap();

        assert!(
            body.get("reasoning_effort").is_none(),
            "thinking_effort:None must not emit reasoning_effort on the wire"
        );
    }

    /// `thinking_effort: Low` emits `reasoning_effort: "low"`.
    #[test]
    fn test_build_request_thinking_effort_low_emits_low() {
        let adapter = OpenAiAdapter::new();
        let messages = vec![LlmMessage::user("hi")];
        let options = ChatOptions {
            thinking_effort: ThinkingEffort::Low,
            ..Default::default()
        };
        let (_, body) = adapter
            .build_request("gpt-4o-mini", &messages, None, &options, false)
            .unwrap();
        assert_eq!(body["reasoning_effort"], "low");
    }

    /// `XHigh` and `Max` map to OpenAI's full vocabulary.
    #[test]
    fn test_build_request_thinking_effort_xhigh_and_max() {
        let adapter = OpenAiAdapter::new();
        let messages = vec![LlmMessage::user("hi")];

        for (effort, expected) in [
            (ThinkingEffort::Medium, "medium"),
            (ThinkingEffort::High, "high"),
            (ThinkingEffort::XHigh, "xhigh"),
            (ThinkingEffort::Max, "max"),
        ] {
            let options = ChatOptions {
                thinking_effort: effort,
                ..Default::default()
            };
            let (_, body) = adapter
                .build_request("gpt-4o-mini", &messages, None, &options, false)
                .unwrap();
            assert_eq!(body["reasoning_effort"], expected);
        }
    }

    /// `Adaptive` has no Chat Completions counterpart — the wire
    /// field is suppressed so callers that mistakenly target
    /// Chat Completions with adaptive don't surprise the API.
    #[test]
    fn test_build_request_thinking_effort_adaptive_omits_field() {
        let adapter = OpenAiAdapter::new();
        let messages = vec![LlmMessage::user("hi")];
        let options = ChatOptions {
            thinking_effort: ThinkingEffort::Adaptive,
            ..Default::default()
        };
        let (_, body) = adapter
            .build_request("gpt-4o-mini", &messages, None, &options, false)
            .unwrap();
        assert!(
            body.get("reasoning_effort").is_none(),
            "Adaptive should not emit a Chat Completions reasoning_effort string"
        );
    }

    // F29: per-provider compat emit. DeepSeek-R1 expects
    // `thinking: {type:"enabled"}` alongside the canonical
    // `reasoning_effort`. When `compat_override` selects the
    // DeepSeek flavour we emit both. Other compat flavours
    // (Qwen/Zai/OpenRouter/Together) stay unwired today; their
    // surface binds land in F30+.

    fn deepseek_compat() -> crate::providers::traits::ProviderCompat {
        crate::providers::traits::ProviderCompat {
            thinking_format: crate::providers::traits::ThinkingFormat::DeepSeek,
            deferred_tools_mode: crate::providers::traits::DeferredToolsMode::Off,
        }
    }

    #[test]
    fn test_build_request_f29_deepseek_compat_emits_thinking_block_alongside_effort() {
        let adapter = OpenAiAdapter::new();
        let messages = vec![LlmMessage::user("hi")];
        let options = ChatOptions {
            thinking_effort: ThinkingEffort::High,
            compat_override: Some(deepseek_compat()),
            ..Default::default()
        };
        let (_, body) = adapter
            .build_request("deepseek-reasoner", &messages, None, &options, false)
            .unwrap();
        // Canonical OpenAI-style effort — DeepSeek's router picks the
        // matching reasoning model variant from this string.
        assert_eq!(body["reasoning_effort"], "high");
        // Provider-specific toggle. DeepSeek-R1 lights up when this is
        // `{"type":"enabled"}`.
        assert_eq!(body["thinking"]["type"], "enabled");
    }

    #[test]
    fn test_build_request_f29_compat_without_thinking_effort_does_not_emit_thinking() {
        let adapter = OpenAiAdapter::new();
        let messages = vec![LlmMessage::user("hi")];
        let options = ChatOptions {
            thinking_effort: ThinkingEffort::None,
            compat_override: Some(deepseek_compat()),
            ..Default::default()
        };
        let (_, body) = adapter
            .build_request("deepseek-chat", &messages, None, &options, false)
            .unwrap();
        // Compat is present but the user did not opt into thinking —
        // gate stays closed so non-reasoning calls stay byte-for-byte
        // identical to pre-F29 Chat Completions traffic.
        assert!(
            body.get("reasoning_effort").is_none(),
            "thinking_effort:None must not emit reasoning_effort"
        );
        assert!(
            body.get("thinking").is_none(),
            "compat without an enabled effort must not emit thinking"
        );
    }

    #[test]
    fn test_build_request_f29_compat_override_other_flavours_stay_unwired() {
        // Kimi/Zai/Qwen/OpenRouter/Together are surfaced as enum
        // variants today but their wire shapes land in F30+. Pin
        // that the F29 emit path is a deliberate no-op for those
        // flavours so the PR stays scoped.
        let adapter = OpenAiAdapter::new();
        let messages = vec![LlmMessage::user("hi")];
        let flavours = [
            crate::providers::traits::ThinkingFormat::Kimi,
            crate::providers::traits::ThinkingFormat::Zai,
            crate::providers::traits::ThinkingFormat::Qwen,
            crate::providers::traits::ThinkingFormat::OpenRouter,
            crate::providers::traits::ThinkingFormat::Together,
        ];
        for flavour in flavours {
            let compat = crate::providers::traits::ProviderCompat {
                thinking_format: flavour,
                deferred_tools_mode: crate::providers::traits::DeferredToolsMode::Off,
            };
            let options = ChatOptions {
                thinking_effort: ThinkingEffort::High,
                compat_override: Some(compat),
                ..Default::default()
            };
            let (_, body) = adapter
                .build_request("model", &messages, None, &options, false)
                .unwrap();
            // Canonical OpenAI effort still emits; provider-specific
            // shape (the `thinking` block) is intentionally absent for
            // these flavours until F30+ lands their wire rules.
            assert_eq!(body["reasoning_effort"], "high");
            assert!(
                body.get("thinking").is_none(),
                "flavour {flavour:?} should not emit `thinking` block in F29"
            );
        }
    }

    /// Streaming delta carries `reasoning_content` (Moonshot/Kimi
    /// shape) → emit `ThinkingDelta`.
    #[test]
    fn test_parse_sse_thinking_delta_reasoning_content() {
        let adapter = OpenAiAdapter::new();
        let data = r#"{"choices":[{"delta":{"content":null},"reasoning_content":"step 1"}]}"#;
        let event = adapter.parse_sse_event("gpt-4o-mini", data).unwrap();
        match event {
            Some(crate::providers::StreamEvent::ThinkingDelta { delta, .. }) => {
                assert_eq!(delta, "step 1");
            }
            other => panic!("Expected ThinkingDelta, got {other:?}"),
        }
    }

    /// Streaming delta carries `reasoning_details[*].text` (OpenAI
    /// o-series shape) → fold into `ThinkingDelta`.
    #[test]
    fn test_parse_sse_thinking_delta_reasoning_details_array() {
        let adapter = OpenAiAdapter::new();
        let data = r#"{
            "choices":[{
                "delta":{"content":null},
                "reasoning_details":[
                    {"type":"reasoning.text","text":"plan "},
                    {"type":"reasoning.text","text":"ready"}
                ]
            }]
        }"#;
        let event = adapter.parse_sse_event("gpt-4o-mini", data).unwrap();
        match event {
            Some(crate::providers::StreamEvent::ThinkingDelta { delta, .. }) => {
                assert_eq!(delta, "plan ready");
            }
            other => panic!("Expected ThinkingDelta, got {other:?}"),
        }
    }

    /// Streaming delta carries bare `reasoning` (string) → probe and
    /// emit as `ThinkingDelta`.
    #[test]
    fn test_parse_sse_thinking_delta_bare_reasoning_string() {
        let adapter = OpenAiAdapter::new();
        let data = r#"{"choices":[{"delta":{"content":null},"reasoning":"thinking..."}]}"#;
        let event = adapter.parse_sse_event("gpt-4o-mini", data).unwrap();
        match event {
            Some(crate::providers::StreamEvent::ThinkingDelta { delta, .. }) => {
                assert_eq!(delta, "thinking...");
            }
            other => panic!("Expected ThinkingDelta, got {other:?}"),
        }
    }

    /// When a delta has both text content and a reasoning field, the
    /// reasoning wins (probed first) — the engine loop processes
    /// ThinkingDelta before TextDelta so the user sees reasoning
    /// before the answer.
    #[test]
    fn test_parse_sse_thinking_takes_precedence_over_text() {
        let adapter = OpenAiAdapter::new();
        let data = r#"{
            "choices":[{
                "delta":{"content":"answer"},
                "reasoning_content":"reasoning first"
            }]
        }"#;
        let event = adapter.parse_sse_event("gpt-4o-mini", data).unwrap();
        match event {
            Some(crate::providers::StreamEvent::ThinkingDelta { delta, .. }) => {
                assert_eq!(delta, "reasoning first");
            }
            other => panic!("Expected ThinkingDelta, got {other:?}"),
        }
    }

    /// Blocking `parse_response` surfaces `reasoning_content` as a
    /// `ContentBlock::Thinking`.
    #[test]
    fn test_parse_response_surfaces_reasoning_content() {
        let adapter = OpenAiAdapter::new();
        let response = json!({
            "choices": [{
                "message": {
                    "content": "Hello!",
                    "role": "assistant",
                    "reasoning_content": "I should greet."
                },
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
        });
        let parsed = adapter.parse_response("gpt-4o-mini", response).unwrap();
        // Two blocks: reasoning first, then visible text.
        assert_eq!(parsed.content.len(), 2);
        match &parsed.content[0] {
            ContentBlock::Thinking { text, signature } => {
                assert_eq!(text, "I should greet.");
                assert!(signature.is_none());
            }
            other => panic!("expected Thinking, got {other:?}"),
        }
        assert!(matches!(&parsed.content[1], ContentBlock::Text { text } if text == "Hello!"));
    }

    /// Blocking `parse_response` flattens `reasoning_details[*].text`.
    #[test]
    fn test_parse_response_surfaces_reasoning_details() {
        let adapter = OpenAiAdapter::new();
        let response = json!({
            "choices": [{
                "message": {
                    "content": "ok",
                    "role": "assistant",
                    "reasoning_details": [
                        {"type":"reasoning.text","text":"step "},
                        {"type":"reasoning.text","text":"by step"}
                    ]
                },
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        });
        let parsed = adapter.parse_response("gpt-4o-mini", response).unwrap();
        assert_eq!(parsed.content.len(), 2);
        match &parsed.content[0] {
            ContentBlock::Thinking { text, .. } => assert_eq!(text, "step by step"),
            other => panic!("expected Thinking, got {other:?}"),
        }
    }

    /// No reasoning field at all → no Thinking block emitted.
    #[test]
    fn test_parse_response_no_reasoning_field_no_thinking_block() {
        let adapter = OpenAiAdapter::new();
        let response = json!({
            "choices": [{
                "message": {"content": "ok", "role": "assistant"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        });
        let parsed = adapter.parse_response("gpt-4o-mini", response).unwrap();
        assert_eq!(parsed.content.len(), 1);
        assert!(matches!(&parsed.content[0], ContentBlock::Text { .. }));
    }

    /// `ThinkingEffort` mapping helpers are the source of truth for
    /// the wire vocabulary — pin them so a future enum reorder
    /// surfaces as a test failure rather than a wire-shape drift.
    #[test]
    fn test_thinking_effort_as_chat_completions_str() {
        assert_eq!(ThinkingEffort::None.as_chat_completions_str(), None);
        assert_eq!(ThinkingEffort::Low.as_chat_completions_str(), Some("low"));
        assert_eq!(
            ThinkingEffort::Medium.as_chat_completions_str(),
            Some("medium")
        );
        assert_eq!(ThinkingEffort::High.as_chat_completions_str(), Some("high"));
        assert_eq!(
            ThinkingEffort::XHigh.as_chat_completions_str(),
            Some("xhigh")
        );
        assert_eq!(ThinkingEffort::Max.as_chat_completions_str(), Some("max"));
        assert_eq!(ThinkingEffort::Adaptive.as_chat_completions_str(), None);
        assert!(!ThinkingEffort::None.is_enabled());
        assert!(ThinkingEffort::Low.is_enabled());
        assert!(ThinkingEffort::Adaptive.is_enabled());
    }

    /// Anthropic budget-token mapping pins the per-effort integer so
    /// the Anthropic adapter's emit logic stays grounded.
    #[test]
    fn test_thinking_effort_anthropic_budget_mapping() {
        assert_eq!(ThinkingEffort::Low.to_anthropic_budget_tokens(), 1024);
        assert_eq!(ThinkingEffort::Medium.to_anthropic_budget_tokens(), 4096);
        assert_eq!(ThinkingEffort::High.to_anthropic_budget_tokens(), 32_000);
        assert_eq!(ThinkingEffort::XHigh.to_anthropic_budget_tokens(), 64_000);
        assert_eq!(ThinkingEffort::Max.to_anthropic_budget_tokens(), 128_000);
        // Adaptive has no integer — caller drops the field.
        assert_eq!(ThinkingEffort::Adaptive.to_anthropic_budget_tokens(), 0);
    }

    // ---------- F26: OpenAI-compat small knobs (Chat Completions) ----------

    /// `ToolChoice::Required` emits the literal `"required"` so the
    /// server rejects a response that does not call a tool.
    #[test]
    fn test_build_request_tool_choice_required_emits_string() {
        let adapter = OpenAiAdapter::new();
        let tools = vec![ToolDefinition {
            name: "Read".to_string(),
            description: "Read a file".to_string(),
            parameters: json!({"type": "object"}),
        }];
        let options = ChatOptions {
            tool_choice: crate::providers::ToolChoice::Required,
            ..Default::default()
        };
        let (_, body) = adapter
            .build_request(
                "gpt-4o-mini",
                &[LlmMessage::user("hi")],
                Some(&tools),
                &options,
                false,
            )
            .unwrap();
        assert_eq!(body["tool_choice"], "required");
    }

    /// `ToolChoice::None` emits `"none"` — even when tools are
    /// registered — so the model answers without tool calls.
    #[test]
    fn test_build_request_tool_choice_none_emits_string() {
        let adapter = OpenAiAdapter::new();
        let tools = vec![ToolDefinition {
            name: "Read".to_string(),
            description: "Read a file".to_string(),
            parameters: json!({"type": "object"}),
        }];
        let options = ChatOptions {
            tool_choice: crate::providers::ToolChoice::None,
            ..Default::default()
        };
        let (_, body) = adapter
            .build_request(
                "gpt-4o-mini",
                &[LlmMessage::user("hi")],
                Some(&tools),
                &options,
                false,
            )
            .unwrap();
        assert_eq!(body["tool_choice"], "none");
    }

    /// `Forced("Read")` emits `{type:"function", function:{name:"Read"}}`.
    #[test]
    fn test_build_request_tool_choice_forced_emits_function_shape() {
        let adapter = OpenAiAdapter::new();
        let tools = vec![ToolDefinition {
            name: "Read".to_string(),
            description: "Read a file".to_string(),
            parameters: json!({"type": "object"}),
        }];
        let options = ChatOptions {
            tool_choice: crate::providers::ToolChoice::Forced("Read".to_string()),
            ..Default::default()
        };
        let (_, body) = adapter
            .build_request(
                "gpt-4o-mini",
                &[LlmMessage::user("hi")],
                Some(&tools),
                &options,
                false,
            )
            .unwrap();
        assert_eq!(body["tool_choice"]["type"], "function");
        assert_eq!(body["tool_choice"]["function"]["name"], "Read");
    }

    /// Default `ToolChoice::Auto` keeps the pre-F26 wire shape
    /// (`"auto"`) for callers that don't set the field.
    #[test]
    fn test_build_request_default_tool_choice_emits_auto() {
        let adapter = OpenAiAdapter::new();
        let tools = vec![ToolDefinition {
            name: "Read".to_string(),
            description: "Read a file".to_string(),
            parameters: json!({"type": "object"}),
        }];
        let (_, body) = adapter
            .build_request(
                "gpt-4o-mini",
                &[LlmMessage::user("hi")],
                Some(&tools),
                &ChatOptions::default(),
                false,
            )
            .unwrap();
        assert_eq!(body["tool_choice"], "auto");
        // No parallel_tool_calls when caller leaves the default.
        assert!(body.get("parallel_tool_calls").is_none());
        // No service_tier when caller leaves the default.
        assert!(body.get("service_tier").is_none());
    }

    /// `parallel_tool_calls: Some(false)` forces serialized tool
    /// calling on the Chat Completions adapter.
    #[test]
    fn test_build_request_parallel_tool_calls_false_emits_false() {
        let adapter = OpenAiAdapter::new();
        let tools = vec![ToolDefinition {
            name: "Read".to_string(),
            description: "Read a file".to_string(),
            parameters: json!({"type": "object"}),
        }];
        let options = ChatOptions {
            parallel_tool_calls: Some(false),
            ..Default::default()
        };
        let (_, body) = adapter
            .build_request(
                "gpt-4o-mini",
                &[LlmMessage::user("hi")],
                Some(&tools),
                &options,
                false,
            )
            .unwrap();
        assert_eq!(body["parallel_tool_calls"], false);
    }

    /// `ServiceTier::Flex` / `Priority` emit the literal wire
    /// strings; `None` (the default) suppresses emission entirely.
    #[test]
    fn test_build_request_service_tier_emits_when_set() {
        let adapter = OpenAiAdapter::new();
        for (tier, expected) in [
            (ServiceTier::Default, "default"),
            (ServiceTier::Auto, "auto"),
            (ServiceTier::Flex, "flex"),
            (ServiceTier::Priority, "priority"),
        ] {
            let options = ChatOptions {
                service_tier: tier,
                ..Default::default()
            };
            let (_, body) = adapter
                .build_request(
                    "gpt-4o-mini",
                    &[LlmMessage::user("hi")],
                    None,
                    &options,
                    false,
                )
                .unwrap();
            assert_eq!(body["service_tier"], expected);
        }

        // None → field absent.
        let (_, body) = adapter
            .build_request(
                "gpt-4o-mini",
                &[LlmMessage::user("hi")],
                None,
                &ChatOptions::default(),
                false,
            )
            .unwrap();
        assert!(body.get("service_tier").is_none());
    }

    /// `ServiceTier` `as_wire_str` pins the vocabulary so a future
    /// enum reorder surfaces as a test failure rather than a
    /// wire-shape drift.
    #[test]
    fn test_service_tier_as_wire_str() {
        assert_eq!(ServiceTier::None.as_wire_str(), None);
        assert_eq!(ServiceTier::Default.as_wire_str(), Some("default"));
        assert_eq!(ServiceTier::Auto.as_wire_str(), Some("auto"));
        assert_eq!(ServiceTier::Flex.as_wire_str(), Some("flex"));
        assert_eq!(ServiceTier::Priority.as_wire_str(), Some("priority"));
    }

    // ---------- F28: multimodal image content (Chat Completions) ----------

    /// F28 baseline: a text-only user message keeps the pre-F28 wire
    /// shape (`content: "..."` as a JSON string). The path through
    /// `build_chat_completions_content` short-circuits to
    /// `extract_text_content` when no Image blocks are present.
    #[test]
    fn test_build_request_text_only_message_emits_string_content() {
        let adapter = OpenAiAdapter::new();
        let (_, body) = adapter
            .build_request(
                "gpt-4o-mini",
                &[LlmMessage::user("hello")],
                None,
                &ChatOptions::default(),
                false,
            )
            .unwrap();
        assert_eq!(body["messages"][0]["content"], "hello");
        assert!(body["messages"][0]["content"].is_string());
    }

    /// F28: an image-only user message emits a single-element
    /// content-part array. `ImageSource::Url` is passed through
    /// verbatim on the wire (the platform host will fetch).
    #[test]
    fn test_build_request_image_url_emits_content_part_array() {
        let adapter = OpenAiAdapter::new();
        let msg = LlmMessage {
            role: MessageRole::User,
            content: vec![ContentBlock::Image {
                source: ImageSource::Url {
                    url: "https://example.com/cat.png".to_string(),
                },
                mime_type: "image/png".to_string(),
            }],
            timestamp: chrono::Utc::now(),
            metadata: std::collections::HashMap::new(),
            tool_call_id: None,
            usage: None,
        };
        let (_, body) = adapter
            .build_request("gpt-4o-mini", &[msg], None, &ChatOptions::default(), false)
            .unwrap();
        let parts = body["messages"][0]["content"]
            .as_array()
            .expect("content must be an array when an Image block is present");
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["type"], "image_url");
        assert_eq!(parts[0]["image_url"]["url"], "https://example.com/cat.png");
    }

    /// F28: `ImageSource::Base64 { data }` + `mime_type` form a
    /// `data:` URL on the wire — the shape OpenAI's chat.completions
    /// vision accepts for inline images.
    #[test]
    fn test_build_request_image_base64_emits_data_url() {
        let adapter = OpenAiAdapter::new();
        let msg = LlmMessage {
            role: MessageRole::User,
            content: vec![ContentBlock::Image {
                source: ImageSource::Base64 {
                    data: "aGVsbG8=".to_string(),
                },
                mime_type: "image/png".to_string(),
            }],
            timestamp: chrono::Utc::now(),
            metadata: std::collections::HashMap::new(),
            tool_call_id: None,
            usage: None,
        };
        let (_, body) = adapter
            .build_request("gpt-4o-mini", &[msg], None, &ChatOptions::default(), false)
            .unwrap();
        let parts = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(parts[0]["type"], "image_url");
        assert_eq!(
            parts[0]["image_url"]["url"],
            "data:image/png;base64,aGVsbG8="
        );
    }

    /// F28 mixed: a user message with text + image emits a multi-part
    /// content array. Order matches `m.content` order so callers can
    /// put text before / after the image as they wish.
    #[test]
    fn test_build_request_text_plus_image_emits_multi_part_array() {
        let adapter = OpenAiAdapter::new();
        let msg = LlmMessage {
            role: MessageRole::User,
            content: vec![
                ContentBlock::Text {
                    text: "what's in this picture?".to_string(),
                },
                ContentBlock::Image {
                    source: ImageSource::Url {
                        url: "https://example.com/cat.png".to_string(),
                    },
                    mime_type: "image/png".to_string(),
                },
            ],
            timestamp: chrono::Utc::now(),
            metadata: std::collections::HashMap::new(),
            tool_call_id: None,
            usage: None,
        };
        let (_, body) = adapter
            .build_request("gpt-4o-mini", &[msg], None, &ChatOptions::default(), false)
            .unwrap();
        let parts = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["type"], "text");
        assert_eq!(parts[0]["text"], "what's in this picture?");
        assert_eq!(parts[1]["type"], "image_url");
        assert_eq!(parts[1]["image_url"]["url"], "https://example.com/cat.png");
    }
}
