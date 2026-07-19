//! Anthropic API adapter
//!
//! Handles conversion between unified types and Anthropic Messages API format.

use super::{extract_text_content, ToolCallAccumulator};
use crate::providers::cache_retention::CacheRetention;
use crate::providers::traits::{
    ChatOptions, ChatResponse, ContentBlock, LlmMessage, MessageRole, StopReason, StreamEvent,
    TokenUsage, ToolChoice, ToolDefinition,
};
use crate::providers::transport::AuthConfig;
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

/// F25: detect model ids that support Anthropic's *adaptive* thinking
/// mode (`thinking: {type: "adaptive"}` + `output_config: {effort}`).
///
/// Adaptive mode is opt-in on the following Claude-family model ids:
/// - `claude-opus-4-6`, `claude-opus-4-7`, `claude-opus-4-8`, ... (4-6+)
/// - `claude-sonnet-5`, `claude-sonnet-5-0`, ...
/// - `claude-fable-5`, `claude-fable-5-1`, ...
/// - `claude-mythos-5`, ...
///
/// Older models (Opus 4-1..4-5, Sonnet 4-5/4-7, Haiku) fall back to
/// the legacy `budget_tokens` mode. The detection is a pure prefix
/// check on the model id; it runs once per request (no caching) so a
/// misfire on a future model id is harmless — it just drops to
/// budget mode, which the server accepts on all thinking-capable
/// Claude models.
///
/// Mirrors codex-rs's `is_adaptive_thinking_model` heuristic at
/// `codex-api/src/provider.rs:341-358`.
#[must_use]
pub fn is_adaptive_thinking_model(model_id: &str) -> bool {
    let prefix = model_id.split('-').take(3).collect::<Vec<_>>().join("-");
    // `claude-opus-4-6` → `claude-opus-4`
    // `claude-sonnet-5` → `claude-sonnet-5`
    // `claude-fable-5` → `claude-fable-5`
    match prefix.as_str() {
        "claude-opus-4" => {
            // Only Opus 4-6+ (model-ids like `claude-opus-4-6-...`).
            // The 4th segment is the minor version; >= 6 is adaptive.
            let minor = model_id
                .split('-')
                .nth(3)
                .and_then(|s| s.split('.').next())
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(0);
            minor >= 6
        }
        "claude-sonnet-5" | "claude-fable-5" | "claude-mythos-5" => true,
        _ => false,
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
                                blocks.push(AnthropicContentBlock::Text {
                                    text: text.clone(),
                                    cache_control: None,
                                });
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
                                    cache_control: None,
                                });
                            }
                            _ => {}
                        }
                    }

                    let content = if blocks.len() == 1 {
                        match &blocks[0] {
                            AnthropicContentBlock::Text { text, .. } => Content::Text(text.clone()),
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
                                cache_control: None,
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
                                    ..
                                } => Content::Blocks(vec![AnthropicContentBlock::ToolResult {
                                    tool_use_id: tool_use_id.clone(),
                                    content: content.clone(),
                                    is_error: *is_error,
                                    cache_control: None,
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
                cache_control: None,
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

/// F26: project `ToolChoice` onto Anthropic's wire shape. Anthropic
/// differs from OpenAI on two points:
///
/// * `Required` → Anthropic uses the literal `"any"` (not
///   `"required"`).
/// * `Forced(name)` → Anthropic's wire shape is
///   `{type:"tool", name:name}` (no `function` wrapper). Forced-tool
///   references a tool that must already be registered under the
///   `tools` array.
///
/// `Auto`/`None` round-trip verbatim from `ToolChoice::Auto`/`None`.
fn tool_choice_anthropic(choice: &ToolChoice) -> serde_json::Value {
    match choice {
        ToolChoice::Auto => json!("auto"),
        ToolChoice::None => json!("none"),
        ToolChoice::Required => json!("any"),
        ToolChoice::Forced(name) => json!({
            "type": "tool",
            "name": name,
        }),
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
        let (system, mut anthropic_messages) = self.convert_messages(messages);

        let mut body = json!({
            "model": model_id,
            "messages": anthropic_messages,
            "max_tokens": options.max_tokens.unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS),
            "stream": stream,
        });

        // F23: switch `system` from a flat string to a `Vec<TextBlockParam>` so
        // we can attach `cache_control` to the sole block. Anthropic's API
        // accepts both shapes — the array form is required for the marker.
        if let Some(system_text) = system {
            let system_block = vec![AnthropicContentBlock::Text {
                text: system_text,
                cache_control: CacheControl::for_retention(options.cache_retention),
            }];
            body["system"] = json!(system_block);
        }

        if let Some(temp) = options.temperature {
            body["temperature"] = json!(temp);
        }

        let cache_marker = CacheControl::for_retention(options.cache_retention);

        if let Some(tools) = tools {
            let mut anthropic_tools = self.convert_tools(tools);
            // F23: stamp the marker on the last tool definition.
            if let Some(marker) = cache_marker.as_ref() {
                if let Some(last) = anthropic_tools.last_mut() {
                    last.cache_control = Some(marker.clone());
                }
            }
            body["tools"] = json!(anthropic_tools);
            // F26: project `ToolChoice` onto Anthropic's wire shape.
            // `Required` maps to `"any"` (Anthropic's "must call a
            // tool" sentinel); `Forced(name)` uses
            // `{type:"tool", name:name}` — different from OpenAI's
            // `{type:"function", function:{name}}` shape (Anthropic
            // has no `function` wrapper). `Auto`/`None` round-trip
            // verbatim.
            body["tool_choice"] = json!(tool_choice_anthropic(&options.tool_choice));
        }

        // F23: stamp the marker on the last message's trailing cacheable block
        // (skip `ToolUse`/`ToolResult` — Anthropic only honors the marker on
        // `text` blocks for prefix caching).
        if let Some(marker) = cache_marker.as_ref() {
            if let Some(last_msg) = anthropic_messages.last_mut() {
                if let Content::Blocks(blocks) = &mut last_msg.content {
                    for block in blocks.iter_mut().rev() {
                        if let AnthropicContentBlock::Text { cache_control, .. } = block {
                            *cache_control = Some(marker.clone());
                            break;
                        }
                    }
                }
            }
            body["messages"] = json!(anthropic_messages);
        }

        // F23: Anthropic's analog of `prompt_cache_key`. Sets
        // `metadata.user_id` so Anthropic can co-locate cached entries
        // for the same session across processes (kimi-code's
        // `provider-manager.ts:281-283` precedent).
        if let Some(key) = options.prompt_cache_key.as_deref() {
            body["metadata"] = json!({ "user_id": key });
        }

        // F27: `context_management` block when `thinking_keep != Off`.
        // Anthropic's `clear_thinking_20251015` edit type strips
        // thinking blocks from previous turns to avoid prompt
        // pollution; `keep` selects how many turns of thinking to
        // retain. The `context-management-2025-06-27` beta is
        // auto-attached via `extra_request_headers`.
        if let Some(keep) = options.thinking_keep.as_wire_str() {
            body["context_management"] = json!({
                "edits": [
                    {
                        "type": "clear_thinking_20251015",
                        "keep": keep,
                    }
                ]
            });
        }

        // F27: `beta_api: true` sends the betas list as a body field
        // in addition to the `anthropic-beta` header. The official
        // SDK uses this body shape for some beta surfaces. Default
        // (`false`) preserves the pre-F27 wire shape.
        if options.beta_api && !options.betas.is_empty() {
            body["betas"] = json!(options.betas);
        }

        // F25: thinking-mode emission. Two shapes:
        //
        // 1. Adaptive mode — Opus 4-6+, Sonnet 5, Fable 5+, Mythos 5.
        //    Body: `thinking: {type: "adaptive"}` + `output_config: {effort}`.
        //    Header: drop the `interleaved-thinking` beta (adaptive mode
        //    replaces the legacy interleaving).
        //
        // 2. Budget mode — everything else (and the case where the caller
        //    forced a numeric effort on a non-adaptive model).
        //    Body: `thinking: {type: "enabled", budget_tokens: N}`.
        //    Header: `anthropic-beta: interleaved-thinking-2025-05-08`
        //    so thinking can be interleaved with tool calls (added
        //    via `extra_request_headers` so `&self` stays immutable).
        //
        // `Adaptive` effort on a non-adaptive model → fall back to
        // budget mode with a `High` token allowance. `None` effort
        // → no thinking fields on the wire (default).
        if options.thinking_effort.is_enabled() {
            let adaptive = is_adaptive_thinking_model(model_id);
            match (options.thinking_effort, adaptive) {
                (effort, true) => {
                    // Adaptive mode: the `output_config.effort` field
                    // uses the same vocabulary as `ChatOptions`. For
                    // budget-style callers that set an integer effort
                    // (Low/Medium/High/XHigh/Max), pass the same string
                    // — Anthropic maps `Low → low`, `High → high`, etc.
                    let effort_str = match effort {
                        crate::providers::ThinkingEffort::Low => "low",
                        crate::providers::ThinkingEffort::Medium => "medium",
                        crate::providers::ThinkingEffort::High => "high",
                        crate::providers::ThinkingEffort::XHigh => "max",
                        crate::providers::ThinkingEffort::Max => "max",
                        crate::providers::ThinkingEffort::Adaptive => "high",
                        crate::providers::ThinkingEffort::None => "medium",
                    };
                    body["thinking"] = json!({"type": "adaptive"});
                    body["output_config"] = json!({"effort": effort_str});
                }
                (effort, false) => {
                    let budget = effort.to_anthropic_budget_tokens();
                    if budget > 0 {
                        body["thinking"] = json!({
                            "type": "enabled",
                            "budget_tokens": budget,
                        });
                        // Interleaved thinking beta is added via
                        // `extra_request_headers` (F25) so we don't
                        // need to mutate `self.extra_headers` here.
                    }
                }
            }
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
                cache_creation_input_tokens: result
                    .usage
                    .cache_creation_input_tokens
                    .map(u64::from),
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
                        // F25: Anthropic emits a `signature_delta`
                        // event after the final thinking chunk —
                        // the `signature` is an opaque token the
                        // model uses to verify the thinking block
                        // when the caller echoes it back. Peko
                        // doesn't currently echo thinking back, so
                        // we drop it on the floor (the blocking
                        // path captures signatures for any future
                        // echo path). Acknowledging the event
                        // keeps the SSE parser in a consistent
                        // state.
                        Some("signature_delta") => {
                            // intentional no-op
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
                    let cache_creation = delta_usage
                        .cache_creation_input_tokens
                        .unwrap_or(cache_creation);
                    let cache_read = delta_usage.cache_read_input_tokens.unwrap_or(cache_read);
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

    fn extra_request_headers(
        &self,
        model_id: &str,
        options: &ChatOptions,
    ) -> Vec<(String, String)> {
        // F25: attach `interleaved-thinking-2025-05-08` whenever
        // budget-mode thinking is enabled. Adaptive mode (Opus 4-6+,
        // Sonnet 5, Fable 5+, Mythos 5) replaces the legacy
        // interleaving and does not need this beta.
        //
        // F27: also fold in the caller-supplied `options.betas`
        // list and any beta auto-injected by `thinking_keep`
        // (`context-management-2025-06-27`). Multiple betas are
        // joined with `,` per Anthropic's documented header shape.
        let mut betas: Vec<String> = Vec::new();
        if options.thinking_effort.is_enabled() && !is_adaptive_thinking_model(model_id) {
            betas.push("interleaved-thinking-2025-05-08".to_string());
        }
        // F27: `thinking_keep != Off` opts the caller into the
        // context-management beta automatically. Without this beta,
        // Anthropic rejects `context_management: {...}` as an
        // unknown field.
        if options.thinking_keep != crate::providers::ThinkingKeep::Off {
            betas.push("context-management-2025-06-27".to_string());
        }
        // F27: caller-supplied betas (sorted last so the
        // caller can override the auto-injected ones if needed).
        for b in &options.betas {
            if !betas.contains(b) {
                betas.push(b.clone());
            }
        }

        if betas.is_empty() {
            return vec![];
        }
        vec![("anthropic-beta".to_string(), betas.join(","))]
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

/// Anthropic `cache_control` marker (F23).
///
/// Attached to a system block, the last tool, or the trailing block
/// of the last message to create a prompt-cache breakpoint. `Long`
/// retention requests Anthropic's 1-hour TTL; `Default` uses the
/// 5-minute ephemeral window. `CacheRetention::None` makes the marker
/// absent on every block — the wire shape matches the pre-F23 form.
#[derive(Debug, Clone, Serialize)]
struct CacheControl {
    #[serde(rename = "type")]
    cache_type: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    ttl: Option<&'static str>,
}

impl CacheControl {
    /// Build a `CacheControl` block for the given retention policy.
    /// Returns `None` when caching is disabled (`CacheRetention::None`).
    fn for_retention(retention: CacheRetention) -> Option<Self> {
        match retention {
            CacheRetention::None => None,
            CacheRetention::Default => Some(Self {
                cache_type: "ephemeral",
                ttl: None,
            }),
            CacheRetention::Long => Some(Self {
                cache_type: "ephemeral",
                ttl: Some("1h"),
            }),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicContentBlock {
    Text {
        text: String,
        /// F23: optional cache-control marker. Skipped from the wire
        /// when `None` (the common case for blocks that are not
        /// prompt-cache breakpoints).
        #[serde(skip_serializing_if = "Option::is_none", default)]
        cache_control: Option<CacheControl>,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        cache_control: Option<CacheControl>,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        cache_control: Option<CacheControl>,
    },
}

#[derive(Debug, Serialize)]
struct AnthropicTool {
    name: String,
    description: String,
    input_schema: Value,
    /// F23: cache-control marker on the last tool definition, marking
    /// the end of the cacheable prefix for the toolset. Skipped when
    /// `None`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    cache_control: Option<CacheControl>,
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
        let _ = adapter
            .parse_sse_event("claude-3-sonnet", start_data)
            .unwrap();

        // message_delta updates output_tokens; cache fields are not in
        // the delta here, so message_start values carry through.
        let delta_data = r#"{"type":"message_delta","usage":{"output_tokens":50}}"#;
        let event = adapter
            .parse_sse_event("claude-3-sonnet", delta_data)
            .unwrap();
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

    // ---------- F23: prompt-cache wiring on the request body ----------

    fn sample_messages() -> Vec<LlmMessage> {
        vec![
            LlmMessage::system("You are a helpful assistant."),
            LlmMessage::user("Hello."),
        ]
    }

    /// `system` becomes a `Vec<TextBlockParam>` (length 1) with
    /// `cache_control` on the sole block when caching is enabled.
    #[test]
    fn test_build_request_system_is_block_array_with_cache_control() {
        let adapter = AnthropicAdapter::new();
        let options = ChatOptions {
            cache_retention: CacheRetention::Long,
            ..Default::default()
        };

        let (_, body) = adapter
            .build_request("claude-3-sonnet", &sample_messages(), None, &options, false)
            .unwrap();

        let system = body["system"].as_array().expect("system is an array");
        assert_eq!(system.len(), 1);
        let block = &system[0];
        assert_eq!(block["type"], "text");
        assert_eq!(block["text"], "You are a helpful assistant.");
        assert_eq!(block["cache_control"]["type"], "ephemeral");
        assert_eq!(block["cache_control"]["ttl"], "1h");
    }

    /// `cache_control` lands on the last tool only — earlier tools
    /// remain marker-free so Anthropic only sees the trailing
    /// breakpoint.
    #[test]
    fn test_build_request_cache_control_on_last_tool() {
        let adapter = AnthropicAdapter::new();
        let tools = vec![
            ToolDefinition {
                name: "Read".to_string(),
                description: "Read a file".to_string(),
                parameters: json!({"type": "object"}),
            },
            ToolDefinition {
                name: "Write".to_string(),
                description: "Write a file".to_string(),
                parameters: json!({"type": "object"}),
            },
            ToolDefinition {
                name: "Edit".to_string(),
                description: "Edit a file".to_string(),
                parameters: json!({"type": "object"}),
            },
        ];
        let options = ChatOptions {
            cache_retention: CacheRetention::Default,
            ..Default::default()
        };

        let (_, body) = adapter
            .build_request(
                "claude-3-sonnet",
                &sample_messages(),
                Some(&tools),
                &options,
                false,
            )
            .unwrap();

        let tool_list = body["tools"].as_array().expect("tools is an array");
        assert_eq!(tool_list.len(), 3);
        assert!(tool_list[0].get("cache_control").is_none());
        assert!(tool_list[1].get("cache_control").is_none());
        assert_eq!(tool_list[2]["cache_control"]["type"], "ephemeral");
        assert!(tool_list[2]["cache_control"].get("ttl").is_none());
    }

    /// `cache_control` lands on the trailing Text block of the last
    /// message, skipping any preceding ToolUse blocks. Mirrors
    /// kimi-code's `injectCacheControlOnLastBlock` semantics.
    #[test]
    fn test_build_request_cache_control_on_last_message_text_block() {
        let adapter = AnthropicAdapter::new();
        // Last message ends with ToolUse then Text — the marker
        // must skip ToolUse and land on the trailing Text block.
        let messages = vec![
            LlmMessage::user("Find and summarize files."),
            LlmMessage {
                role: MessageRole::Assistant,
                content: vec![
                    ContentBlock::ToolCall {
                        id: "tool_1".to_string(),
                        name: "Read".to_string(),
                        arguments: json!({"path": "/tmp/x"}),
                    },
                    ContentBlock::Text {
                        text: "Here's what I found.".to_string(),
                    },
                ],
                ..Default::default()
            },
        ];
        let options = ChatOptions {
            cache_retention: CacheRetention::Default,
            ..Default::default()
        };

        let (_, body) = adapter
            .build_request("claude-3-sonnet", &messages, None, &options, false)
            .unwrap();

        let last_msg = body["messages"].as_array().unwrap().last().unwrap();
        let blocks = last_msg["content"].as_array().expect("multi-block content");
        assert_eq!(blocks.len(), 2);
        // ToolUse is first; no marker.
        assert_eq!(blocks[0]["type"], "tool_use");
        assert!(blocks[0].get("cache_control").is_none());
        // Trailing Text gets the marker.
        assert_eq!(blocks[1]["type"], "text");
        assert_eq!(blocks[1]["cache_control"]["type"], "ephemeral");
    }

    /// `CacheRetention::None` produces the pre-F23 wire shape — no
    /// `cache_control` on system, no `cache_control` on tools, no
    /// `cache_control` on any message block.
    #[test]
    fn test_build_request_cache_retention_none_strips_all_markers() {
        let adapter = AnthropicAdapter::new();
        let tools = vec![ToolDefinition {
            name: "Read".to_string(),
            description: "Read a file".to_string(),
            parameters: json!({"type": "object"}),
        }];
        let messages = vec![
            LlmMessage::system("sys"),
            LlmMessage::user("u"),
            LlmMessage {
                role: MessageRole::Assistant,
                content: vec![
                    ContentBlock::ToolCall {
                        id: "t".to_string(),
                        name: "Read".to_string(),
                        arguments: json!({}),
                    },
                    ContentBlock::Text {
                        text: "answer".to_string(),
                    },
                ],
                ..Default::default()
            },
        ];
        let options = ChatOptions {
            cache_retention: CacheRetention::None,
            ..Default::default()
        };

        let (_, body) = adapter
            .build_request("claude-3-sonnet", &messages, Some(&tools), &options, false)
            .unwrap();

        // System block exists but has no cache_control.
        let system = body["system"].as_array().unwrap();
        assert!(system[0].get("cache_control").is_none());
        // Tool has no cache_control.
        let tool_list = body["tools"].as_array().unwrap();
        assert!(tool_list[0].get("cache_control").is_none());
        // No message block carries cache_control.
        for msg in body["messages"].as_array().unwrap() {
            if let Some(blocks) = msg["content"].as_array() {
                for block in blocks {
                    assert!(
                        block.get("cache_control").is_none(),
                        "block unexpectedly carries cache_control: {block}"
                    );
                }
            }
        }
    }

    /// `prompt_cache_key` translates to `metadata.user_id` on the
    /// Anthropic request body (kimi-code's analog at
    /// `provider-manager.ts:281-283`).
    #[test]
    fn test_build_request_metadata_user_id_set_when_session_id_present() {
        let adapter = AnthropicAdapter::new();
        let options = ChatOptions {
            prompt_cache_key: Some("sess-123".to_string()),
            ..Default::default()
        };

        let (_, body) = adapter
            .build_request("claude-3-sonnet", &sample_messages(), None, &options, false)
            .unwrap();

        assert_eq!(body["metadata"]["user_id"], "sess-123");
    }

    /// `CacheControl::for_retention` returns the right shape for each
    /// variant — the helper is the source of truth for the wire form.
    #[test]
    fn test_cache_control_for_retention_shapes() {
        assert!(CacheControl::for_retention(CacheRetention::None).is_none());
        let default = CacheControl::for_retention(CacheRetention::Default).unwrap();
        assert_eq!(default.cache_type, "ephemeral");
        assert!(default.ttl.is_none());
        let long = CacheControl::for_retention(CacheRetention::Long).unwrap();
        assert_eq!(long.cache_type, "ephemeral");
        assert_eq!(long.ttl, Some("1h"));
    }

    // ---------- F25: reasoning-effort wiring ----------

    /// `is_adaptive_thinking_model` recognizes the adaptive-capable
    /// model ids exactly — pins the prefix table so a future
    /// rename surfaces as a test failure rather than a silent
    /// budget/adaptive flip in production.
    #[test]
    fn test_is_adaptive_thinking_model_prefixes() {
        // Adaptive (Opus 4-6+, Sonnet 5, Fable 5, Mythos 5)
        assert!(is_adaptive_thinking_model("claude-opus-4-6"));
        assert!(is_adaptive_thinking_model("claude-opus-4-7"));
        assert!(is_adaptive_thinking_model("claude-opus-4-6-20250101"));
        assert!(is_adaptive_thinking_model("claude-sonnet-5"));
        assert!(is_adaptive_thinking_model("claude-sonnet-5-0"));
        assert!(is_adaptive_thinking_model("claude-fable-5"));
        assert!(is_adaptive_thinking_model("claude-mythos-5"));

        // Budget (older Claude families)
        assert!(!is_adaptive_thinking_model("claude-opus-4-5"));
        assert!(!is_adaptive_thinking_model("claude-opus-4-1"));
        assert!(!is_adaptive_thinking_model("claude-sonnet-4-5"));
        assert!(!is_adaptive_thinking_model("claude-3-7-sonnet-20250219"));
        assert!(!is_adaptive_thinking_model("claude-3-5-sonnet-20240620"));
        assert!(!is_adaptive_thinking_model("claude-haiku-4-5"));

        // Unknown family
        assert!(!is_adaptive_thinking_model("gpt-5"));
        assert!(!is_adaptive_thinking_model(""));
    }

    /// Default `ChatOptions` keeps the wire shape unchanged — no
    /// `thinking` block, no `output_config`, no extra headers.
    #[test]
    fn test_build_request_thinking_effort_none_omits_thinking_block() {
        let adapter = AnthropicAdapter::new();
        let (_, body) = adapter
            .build_request(
                "claude-opus-4-5",
                &sample_messages(),
                None,
                &ChatOptions::default(),
                false,
            )
            .unwrap();
        assert!(body.get("thinking").is_none());
        assert!(body.get("output_config").is_none());
        // No interleaved-thinking beta header.
        assert!(adapter
            .extra_request_headers("claude-opus-4-5", &ChatOptions::default())
            .is_empty());
    }

    /// Budget mode on a non-adaptive model emits
    /// `thinking: {type: "enabled", budget_tokens: N}` and the
    /// interleaved-thinking beta header.
    #[test]
    fn test_build_request_budget_mode_emits_enabled_and_beta() {
        let adapter = AnthropicAdapter::new();
        let options = ChatOptions {
            thinking_effort: crate::providers::ThinkingEffort::High,
            ..Default::default()
        };
        let (_, body) = adapter
            .build_request("claude-opus-4-5", &sample_messages(), None, &options, false)
            .unwrap();
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["thinking"]["budget_tokens"], 32_000);
        assert!(body.get("output_config").is_none());
        let headers = adapter.extra_request_headers("claude-opus-4-5", &options);
        assert!(
            headers
                .iter()
                .any(|(k, v)| k == "anthropic-beta" && v == "interleaved-thinking-2025-05-08"),
            "expected interleaved-thinking beta, got {headers:?}"
        );
    }

    /// Adaptive mode on Opus 4-6+ emits
    /// `thinking: {type: "adaptive"}` + `output_config: {effort}`
    /// and does NOT attach the interleaved-thinking beta.
    #[test]
    fn test_build_request_adaptive_mode_on_opus_4_6() {
        let adapter = AnthropicAdapter::new();
        let options = ChatOptions {
            thinking_effort: crate::providers::ThinkingEffort::High,
            ..Default::default()
        };
        let (_, body) = adapter
            .build_request("claude-opus-4-6", &sample_messages(), None, &options, false)
            .unwrap();
        assert_eq!(body["thinking"]["type"], "adaptive");
        assert_eq!(body["output_config"]["effort"], "high");
        let headers = adapter.extra_request_headers("claude-opus-4-6", &options);
        assert!(
            !headers
                .iter()
                .any(|(k, v)| k == "anthropic-beta" && v.contains("interleaved")),
            "adaptive mode must not attach interleaved-thinking beta, got {headers:?}"
        );
    }

    /// `Adaptive` effort on a non-adaptive model falls back to
    /// budget mode with a `High` token allowance.
    #[test]
    fn test_build_request_adaptive_effort_on_non_adaptive_model_falls_back() {
        let adapter = AnthropicAdapter::new();
        let options = ChatOptions {
            thinking_effort: crate::providers::ThinkingEffort::Adaptive,
            ..Default::default()
        };
        let (_, body) = adapter
            .build_request("claude-opus-4-5", &sample_messages(), None, &options, false)
            .unwrap();
        // Falls back to budget mode — `to_anthropic_budget_tokens`
        // returns 0 for Adaptive (which the adapter suppresses), so
        // the field is dropped.
        assert!(body.get("thinking").is_none());
    }

    /// `signature_delta` events are accepted by the SSE parser
    /// without surfacing as a stream event (peko doesn't currently
    /// echo thinking back to the model).
    #[test]
    fn test_parse_sse_signature_delta_acknowledged() {
        let adapter = AnthropicAdapter::new();
        let data = r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"abc"}}"#;
        let event = adapter.parse_sse_event("claude-opus-4-6", data).unwrap();
        assert!(
            event.is_none(),
            "signature_delta should be a silent no-op, got {event:?}"
        );
    }

    /// `ThinkingStart` already exists from F22 — pin that the
    /// streaming path still emits it on `content_block_start`
    /// for `thinking` blocks.
    #[test]
    fn test_parse_sse_thinking_start_emitted() {
        let adapter = AnthropicAdapter::new();
        let data =
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking"}}"#;
        let event = adapter.parse_sse_event("claude-opus-4-6", data).unwrap();
        match event {
            Some(crate::providers::StreamEvent::ThinkingStart { content_index }) => {
                assert_eq!(content_index, 0);
            }
            other => panic!("expected ThinkingStart, got {other:?}"),
        }
    }

    // ---------- F26: tool_choice wiring on Anthropic ----------

    /// `ToolChoice::Auto` (the default) round-trips to the literal
    /// `"auto"`, preserving the pre-F26 wire shape for callers that
    /// don't set the field.
    #[test]
    fn test_build_request_default_tool_choice_emits_auto() {
        let adapter = AnthropicAdapter::new();
        let tools = vec![ToolDefinition {
            name: "Read".to_string(),
            description: "Read a file".to_string(),
            parameters: json!({"type": "object"}),
        }];
        let (_, body) = adapter
            .build_request(
                "claude-3-sonnet",
                &sample_messages(),
                Some(&tools),
                &ChatOptions::default(),
                false,
            )
            .unwrap();
        assert_eq!(body["tool_choice"], "auto");
    }

    /// `ToolChoice::Required` maps to Anthropic's `"any"` (different
    /// from OpenAI's `"required"` — Anthropic's wire vocabulary
    /// borrows the natural-language form).
    #[test]
    fn test_build_request_tool_choice_required_emits_any() {
        let adapter = AnthropicAdapter::new();
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
                "claude-3-sonnet",
                &sample_messages(),
                Some(&tools),
                &options,
                false,
            )
            .unwrap();
        assert_eq!(body["tool_choice"], "any");
    }

    /// `ToolChoice::Forced("Read")` emits Anthropic's
    /// `{type:"tool", name:"Read"}` shape (no `function` wrapper —
    /// Anthropic's tool_choice references tools directly).
    #[test]
    fn test_build_request_tool_choice_forced_emits_tool_shape() {
        let adapter = AnthropicAdapter::new();
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
                "claude-3-sonnet",
                &sample_messages(),
                Some(&tools),
                &options,
                false,
            )
            .unwrap();
        assert_eq!(body["tool_choice"]["type"], "tool");
        assert_eq!(body["tool_choice"]["name"], "Read");
    }

    /// Anthropic ignores `parallel_tool_calls`, `service_tier`, and
    /// `safety_identifier` (those are OpenAI-only fields). The
    /// request body must remain free of them even when callers set
    /// every F26 knob.
    #[test]
    fn test_build_request_ignores_openai_only_knobs() {
        let adapter = AnthropicAdapter::new();
        let tools = vec![ToolDefinition {
            name: "Read".to_string(),
            description: "Read a file".to_string(),
            parameters: json!({"type": "object"}),
        }];
        let options = ChatOptions {
            parallel_tool_calls: Some(false),
            safety_identifier: Some("user-hash".to_string()),
            tool_choice: crate::providers::ToolChoice::Required,
            ..Default::default()
        };
        let (_, body) = adapter
            .build_request(
                "claude-3-sonnet",
                &sample_messages(),
                Some(&tools),
                &options,
                false,
            )
            .unwrap();
        assert!(
            body.get("parallel_tool_calls").is_none(),
            "Anthropic must not emit parallel_tool_calls"
        );
        assert!(
            body.get("safety_identifier").is_none(),
            "Anthropic must not emit safety_identifier"
        );
        assert!(
            body.get("service_tier").is_none(),
            "Anthropic must not emit service_tier"
        );
        // tool_choice is honored.
        assert_eq!(body["tool_choice"], "any");
    }

    // ---------- F27: betas + beta_api + thinking_keep ----------

    /// Default `ChatOptions` (empty `betas`, `beta_api: false`,
    /// `thinking_keep: Off`) preserves the pre-F27 wire shape —
    /// no `anthropic-beta` header, no `betas` body field, no
    /// `context_management` block.
    #[test]
    fn test_f27_defaults_omit_all_betas_fields() {
        let adapter = AnthropicAdapter::new();
        let options = ChatOptions::default();
        let (_, body) = adapter
            .build_request("claude-3-sonnet", &sample_messages(), None, &options, false)
            .unwrap();
        assert!(body.get("betas").is_none());
        assert!(body.get("context_management").is_none());
        assert!(adapter
            .extra_request_headers("claude-3-sonnet", &options)
            .is_empty());
    }

    /// Caller-supplied `betas` list lands on the `anthropic-beta`
    /// header joined with `,` (Anthropic's documented wire shape).
    /// When `beta_api: false` (default), the body stays free of
    /// `betas`.
    #[test]
    fn test_f27_caller_betas_land_on_header_only() {
        let adapter = AnthropicAdapter::new();
        let options = ChatOptions {
            betas: vec!["prompt-caching-2024-07-31".to_string()],
            ..Default::default()
        };
        let (_, body) = adapter
            .build_request("claude-3-sonnet", &sample_messages(), None, &options, false)
            .unwrap();
        assert!(body.get("betas").is_none());
        let headers = adapter.extra_request_headers("claude-3-sonnet", &options);
        let beta = headers
            .iter()
            .find(|(k, _)| k == "anthropic-beta")
            .expect("anthropic-beta header present");
        assert_eq!(beta.1, "prompt-caching-2024-07-31");
    }

    /// `beta_api: true` ALSO emits the betas list as a body field.
    /// The header remains (no duplication rule between header and
    /// body for Anthropic — both shapes are valid for the official
    /// SDK).
    #[test]
    fn test_f27_beta_api_emits_body_field() {
        let adapter = AnthropicAdapter::new();
        let options = ChatOptions {
            betas: vec!["prompt-caching-2024-07-31".to_string()],
            beta_api: true,
            ..Default::default()
        };
        let (_, body) = adapter
            .build_request("claude-3-sonnet", &sample_messages(), None, &options, false)
            .unwrap();
        let body_betas = body["betas"].as_array().expect("betas is an array");
        assert_eq!(body_betas.len(), 1);
        assert_eq!(body_betas[0], "prompt-caching-2024-07-31");
        // Header also present.
        let headers = adapter.extra_request_headers("claude-3-sonnet", &options);
        assert!(
            headers
                .iter()
                .any(|(k, v)| k == "anthropic-beta" && v == "prompt-caching-2024-07-31"),
            "header should still carry the beta, got {headers:?}"
        );
    }

    /// `beta_api: true` with an empty `betas` list does NOT emit the
    /// body field (no point shipping `betas: []` — that would
    /// actually clear all server-side betas, which isn't what the
    /// caller asked for).
    #[test]
    fn test_f27_beta_api_with_empty_betas_emits_nothing() {
        let adapter = AnthropicAdapter::new();
        let options = ChatOptions {
            beta_api: true,
            ..Default::default()
        };
        let (_, body) = adapter
            .build_request("claude-3-sonnet", &sample_messages(), None, &options, false)
            .unwrap();
        assert!(body.get("betas").is_none());
    }

    /// `thinking_keep: Turn` emits `context_management.edits` with
    /// `keep: "turn"` and auto-attaches the
    /// `context-management-2025-06-27` beta header.
    #[test]
    fn test_f27_thinking_keep_turn_emits_context_management() {
        let adapter = AnthropicAdapter::new();
        let options = ChatOptions {
            thinking_keep: crate::providers::ThinkingKeep::Turn,
            ..Default::default()
        };
        let (_, body) = adapter
            .build_request("claude-3-sonnet", &sample_messages(), None, &options, false)
            .unwrap();
        let cm = body["context_management"]
            .as_object()
            .expect("context_management");
        let edits = cm["edits"].as_array().expect("edits array");
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0]["type"], "clear_thinking_20251015");
        assert_eq!(edits[0]["keep"], "turn");
        let headers = adapter.extra_request_headers("claude-3-sonnet", &options);
        assert!(
            headers
                .iter()
                .any(|(k, v)| k == "anthropic-beta" && v.contains("context-management-2025-06-27")),
            "expected context-management beta, got {headers:?}"
        );
    }

    /// `thinking_keep: All` emits `keep: "all"` on the body. The
    /// beta header is auto-attached the same way.
    #[test]
    fn test_f27_thinking_keep_all_emits_all_keep() {
        let adapter = AnthropicAdapter::new();
        let options = ChatOptions {
            thinking_keep: crate::providers::ThinkingKeep::All,
            ..Default::default()
        };
        let (_, body) = adapter
            .build_request("claude-3-sonnet", &sample_messages(), None, &options, false)
            .unwrap();
        assert_eq!(body["context_management"]["edits"][0]["keep"], "all");
    }

    /// F25 + F27 compose: budget-mode thinking on a non-adaptive
    /// model emits both `interleaved-thinking` AND any auto-injected
    /// betas from `thinking_keep` on a single comma-joined header.
    #[test]
    fn test_f27_combined_betas_join_with_comma() {
        let adapter = AnthropicAdapter::new();
        let options = ChatOptions {
            thinking_effort: crate::providers::ThinkingEffort::High,
            thinking_keep: crate::providers::ThinkingKeep::Turn,
            betas: vec!["prompt-caching-2024-07-31".to_string()],
            ..Default::default()
        };
        let headers = adapter.extra_request_headers("claude-opus-4-5", &options);
        let beta = headers
            .iter()
            .find(|(k, _)| k == "anthropic-beta")
            .expect("anthropic-beta header present");
        // All three betas present, joined with comma.
        let parts: Vec<&str> = beta.1.split(',').collect();
        assert!(parts.contains(&"interleaved-thinking-2025-05-08"));
        assert!(parts.contains(&"context-management-2025-06-27"));
        assert!(parts.contains(&"prompt-caching-2024-07-31"));
        assert_eq!(parts.len(), 3, "expected 3 distinct betas, got {parts:?}");
    }

    /// `ThinkingKeep::as_wire_str` pins the vocabulary so a future
    /// enum reorder surfaces as a test failure rather than a
    /// wire-shape drift.
    #[test]
    fn test_thinking_keep_as_wire_str() {
        assert_eq!(crate::providers::ThinkingKeep::Off.as_wire_str(), None);
        assert_eq!(
            crate::providers::ThinkingKeep::Turn.as_wire_str(),
            Some("turn")
        );
        assert_eq!(
            crate::providers::ThinkingKeep::All.as_wire_str(),
            Some("all")
        );
    }
}
