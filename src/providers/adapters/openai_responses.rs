//! OpenAI Responses API adapter (`POST /v1/responses`).
//!
//! The Responses API is the successor surface to Chat Completions:
//! - system prompt lives on a top-level `instructions` field
//! - the conversation is an `input: [...]` array of typed items
//!   (`message`, `function_call`, `function_call_output`)
//! - tool-call arguments are a JSON **string**, not a parsed object
//! - SSE events follow a `response.<thing>.<verb>` family distinct from
//!   Chat Completions' `choices[].delta.*`
//!
//! Wire format references:
//! - `codex/codex-rs/codex-api/src/common.rs` (`ResponsesApiRequest`)
//! - `codex/codex-rs/codex-api/src/sse/responses.rs` (event processor)
//! - `codex/codex-rs/tools/src/responses_api.rs` (tool definitions)
//!
//! Prompt-cache wiring (F23) is shared with the Chat Completions
//! adapter: `prompt_cache_key` (clamped to 64 UTF-32 chars via
//! `clamp_openai_prompt_cache_key`) and `prompt_cache_retention`
//! (`"24h"` when `CacheRetention::Long`).

use super::{extract_text_content, ToolCallAccumulator};
use crate::providers::cache_retention::CacheRetention;
use crate::providers::traits::{
    ChatOptions, ChatResponse, ContentBlock, LlmMessage, MessageRole, StopReason, StreamEvent,
    TokenUsage, ToolDefinition,
};
use crate::providers::transport::AuthConfig;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::debug;

/// OpenAI Responses API adapter
#[derive(Debug, Clone)]
pub struct OpenAiResponsesAdapter {
    base_url: String,
    /// Accumulates tool-call arguments across streaming deltas.
    tool_call_accumulator: ToolCallAccumulator,
}

impl OpenAiResponsesAdapter {
    /// Create a new Responses adapter pointing at the canonical
    /// `https://api.openai.com/v1` base URL.
    #[must_use]
    pub fn new() -> Self {
        Self {
            base_url: "https://api.openai.com/v1".to_string(),
            tool_call_accumulator: ToolCallAccumulator::new(),
        }
    }

    /// Create with custom base URL (Azure Responses endpoint,
    /// OpenRouter passthrough, etc.).
    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }
}

impl Default for OpenAiResponsesAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl OpenAiResponsesAdapter {
    /// Convert unified messages into Responses-API input items,
    /// lifting a leading System message out into `instructions`.
    ///
    /// Returns `(instructions, input_items)`. When the caller already
    /// has the system prompt in some other form (e.g. peko's engine
    /// loop puts it on `messages[0]`), we follow the convention of
    /// reading `messages[0]` as the system prompt and emitting it on
    /// the top-level `instructions` field — never as an input item.
    fn convert_messages(&self, messages: &[LlmMessage]) -> (String, Vec<ResponsesInputItem>) {
        let mut iter = messages.iter().peekable();
        let mut instructions = String::new();

        // Lift the first System message (if any) into `instructions`.
        // If the first message is *not* System, we simply skip this
        // block and let the loop below emit it as a normal input item.
        if let Some(first) = iter.peek() {
            if first.role == MessageRole::System {
                instructions = extract_text_content(&first.content);
                iter.next();
            }
        }

        let mut items: Vec<ResponsesInputItem> = Vec::with_capacity(messages.len());
        for msg in iter {
            items.extend(self.convert_one(msg));
        }
        (instructions, items)
    }

    /// Convert one `LlmMessage` to one or more input items.
    fn convert_one(&self, msg: &LlmMessage) -> Vec<ResponsesInputItem> {
        match msg.role {
            MessageRole::System => {
                // Should be lifted into instructions by convert_messages,
                // but if a System message appears mid-conversation (it
                // shouldn't on peko's path), surface its text as a
                // user-role message so the model still sees it.
                let text = extract_text_content(&msg.content);
                if text.is_empty() {
                    vec![]
                } else {
                    vec![ResponsesInputItem::Message {
                        role: "user".to_string(),
                        content: vec![ResponsesContentPart::InputText { text }],
                    }]
                }
            }
            MessageRole::User => {
                let text = extract_text_content(&msg.content);
                vec![ResponsesInputItem::Message {
                    role: "user".to_string(),
                    content: vec![ResponsesContentPart::InputText { text }],
                }]
            }
            MessageRole::Assistant => {
                let text = extract_text_content(&msg.content);
                let mut items = Vec::new();
                if !text.is_empty() {
                    items.push(ResponsesInputItem::Message {
                        role: "assistant".to_string(),
                        content: vec![ResponsesContentPart::OutputText { text }],
                    });
                }
                // Each ToolCall becomes a separate `function_call` item
                // with `arguments` as a JSON string (per Responses spec).
                for block in &msg.content {
                    if let ContentBlock::ToolCall {
                        id,
                        name,
                        arguments,
                    } = block
                    {
                        let args_str =
                            serde_json::to_string(arguments).unwrap_or_else(|_| "{}".to_string());
                        items.push(ResponsesInputItem::FunctionCall {
                            call_id: id.clone(),
                            name: name.clone(),
                            arguments: args_str,
                        });
                    }
                }
                items
            }
            MessageRole::Tool => {
                let call_id = msg.tool_call_id.clone().unwrap_or_default();
                let output = extract_text_content(&msg.content);
                vec![ResponsesInputItem::FunctionCallOutput { call_id, output }]
            }
        }
    }

    /// Convert unified tool definitions into the Responses API tool
    /// shape (matches codex's `ResponsesApiTool` minus the
    /// codex-only `strict` / `defer_loading` fields).
    fn convert_tools(&self, tools: &[ToolDefinition]) -> Vec<ResponsesTool> {
        tools
            .iter()
            .map(|t| ResponsesTool {
                tool_type: "function",
                name: t.name.clone(),
                description: t.description.clone(),
                parameters: t.parameters.clone(),
            })
            .collect()
    }
}

impl super::ApiAdapter for OpenAiResponsesAdapter {
    fn name(&self) -> &'static str {
        "openai_responses"
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }

    fn supports_native_tools(&self) -> bool {
        true
    }

    fn supports_prompt_cache_control(&self) -> bool {
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
        let (instructions, input) = self.convert_messages(messages);

        let mut body = json!({
            "model": model_id,
            "instructions": instructions,
            "input": input,
            "stream": stream,
        });

        if let Some(temp) = options.temperature {
            body["temperature"] = json!(temp);
        }

        if let Some(max_tokens) = options.max_tokens {
            body["max_output_tokens"] = json!(max_tokens);
        }

        if let Some(tools) = tools {
            body["tools"] = json!(self.convert_tools(tools));
            body["tool_choice"] = json!("auto");
            // Parallel tool calls are the Responses API default
            // (`true`). Most newer OpenAI models support this; we
            // hard-code true for now and gate via options in a
            // follow-up if a caller needs serial semantics.
            body["parallel_tool_calls"] = json!(true);
        }

        // F25: reasoning-effort knob. Maps to Responses API's
        // `reasoning: {effort, summary}` object. The string is the
        // same vocabulary as Chat Completions for `effort`
        // (`"low"|"medium"|"high"`, plus `"xhigh"`/`"max"` when
        // supported). `summary: "auto"` is the only value OpenAI
        // documents — it lets the server decide between
        // `concise`/`detailed` based on effort.
        if options.thinking_effort.is_enabled() {
            let effort = options
                .thinking_effort
                .as_chat_completions_str()
                .unwrap_or("medium"); // Adaptive → fall back to medium
            let mut reasoning = json!({"effort": effort});
            if options.thinking_summary == Some(true) {
                reasoning["summary"] = json!("auto");
            }
            body["reasoning"] = reasoning;
            if options.encrypted_reasoning {
                // The Responses API uses an `include` array to opt
                // into specific output surfaces.
                // `reasoning.encrypted_content` returns the encrypted
                // reasoning payload alongside the summary so callers
                // can pass it back into `previous_response_id` chains
                // without leaking plaintext.
                body["include"] = json!(["reasoning.encrypted_content"]);
            }
        }

        // F23: prompt-cache wiring. Same shape as Chat Completions:
        // `prompt_cache_key` (≤64 UTF-32 chars) and
        // `prompt_cache_retention` ("24h") only when Long. The
        // engine loop already gates emission on
        // `Provider::supports_prompt_cache_control()`, so by the
        // time we get here the caller has decided cache markers
        // are wanted.
        if options.cache_retention.is_enabled() {
            if let Some(key) = options.prompt_cache_key.as_deref() {
                body["prompt_cache_key"] = json!(key);
            }
            if options.cache_retention == CacheRetention::Long {
                body["prompt_cache_retention"] = json!("24h");
            }
        }

        debug!(
            "OpenAI Responses request: {}",
            serde_json::to_string_pretty(&body)?
        );

        Ok(("/responses".to_string(), body))
    }

    fn parse_response(&self, model_id: &str, response: Value) -> Result<ChatResponse> {
        debug!(
            "OpenAI Responses response: {}",
            serde_json::to_string_pretty(&response)?
        );

        let parsed: ResponsesApiResponseBody =
            serde_json::from_value(response).context("Failed to parse OpenAI Responses body")?;

        let mut content = Vec::new();
        let mut tool_calls = Vec::new();
        let mut stop_reason = StopReason::Stop;

        for item in parsed.output {
            match item {
                ResponsesOutputItem::Message { content: parts, .. } => {
                    for part in parts {
                        if let ResponsesContentPart::OutputText { text } = part {
                            if !text.is_empty() {
                                content.push(ContentBlock::Text { text });
                            }
                        }
                    }
                }
                ResponsesOutputItem::FunctionCall {
                    call_id,
                    name,
                    arguments,
                    ..
                } => {
                    let arguments = serde_json::from_str(&arguments).unwrap_or_else(|_| json!({}));
                    tool_calls.push(ContentBlock::ToolCall {
                        id: call_id,
                        name,
                        arguments,
                    });
                    stop_reason = StopReason::ToolUse;
                }
                ResponsesOutputItem::Reasoning { summary } => {
                    // F25: surface Reasoning items as Thinking
                    // blocks on the blocking path. Each entry in
                    // `summary` is a typed `summary_text` part;
                    // flatten the text fields in order so the
                    // engine loop's reasoning-before-text ordering
                    // is preserved.
                    let text = summary
                        .iter()
                        .filter_map(|entry| entry.get("text").and_then(|t| t.as_str()))
                        .collect::<Vec<_>>()
                        .join("");
                    if !text.is_empty() {
                        content.push(ContentBlock::Thinking {
                            text,
                            signature: None,
                        });
                    }
                }
                ResponsesOutputItem::Unknown => {
                    // Unknown variant from `#[serde(other)]` — skip.
                }
            }
        }

        // `end_turn` → Stop; `max_output_tokens` / `length` → Length;
        // explicit tool_calls above already set ToolUse.
        if stop_reason != StopReason::ToolUse {
            stop_reason = match parsed
                .incomplete_details
                .as_ref()
                .and_then(|d| d.reason.as_deref())
            {
                Some("max_output_tokens") => StopReason::Length,
                _ => StopReason::Stop,
            };
        }

        let usage = parsed.usage.unwrap_or_default();
        Ok(ChatResponse {
            content,
            tool_calls,
            stop_reason,
            usage: TokenUsage {
                input: i64_to_u64(usage.input_tokens),
                output: i64_to_u64(usage.output_tokens),
                total: i64_to_u64(usage.total_tokens),
                // Responses API is the first provider that surfaces
                // cache_write on the input side via
                // `input_tokens_details.cache_write_tokens`. Anthropic
                // uses a separate top-level `cache_creation_input_tokens`
                // field but the semantic is the same: tokens billed at
                // the cache-write rate. Peko's `TokenUsage` already
                // carries the field for that purpose.
                cache_creation_input_tokens: usage
                    .input_tokens_details
                    .as_ref()
                    .map(|d| i64_to_u64(d.cache_write_tokens))
                    .filter(|n| *n > 0),
                cache_read_input_tokens: usage
                    .input_tokens_details
                    .as_ref()
                    .map(|d| i64_to_u64(d.cached_tokens))
                    .filter(|n| *n > 0),
                reasoning_output_tokens: usage
                    .output_tokens_details
                    .as_ref()
                    .map(|d| i64_to_u64(d.reasoning_tokens))
                    .filter(|n| *n > 0),
            },
            provider: self.name().to_string(),
            model: model_id.to_string(),
        })
    }

    fn parse_sse_event(&self, _model_id: &str, data: &str) -> Result<Option<StreamEvent>> {
        let chunk: ResponsesStreamEvent =
            serde_json::from_str(data).context("Failed to parse OpenAI Responses SSE chunk")?;

        match chunk.kind.as_str() {
            "response.created" => {
                // Stream start signal — nothing to emit. The engine
                // loop's first LlmEvent::StreamStart has already
                // landed on a higher layer.
                Ok(None)
            }
            "response.output_text.delta" => {
                if let Some(delta) = chunk.delta {
                    if !delta.is_empty() {
                        return Ok(Some(StreamEvent::TextDelta {
                            content_index: 0,
                            delta,
                        }));
                    }
                }
                Ok(None)
            }
            "response.reasoning_text.delta" => {
                // F25: streaming raw reasoning text. The
                // `response.reasoning_text.delta` event carries a
                // `delta` string with the model's internal
                // reasoning — surface it as ThinkingDelta so the
                // engine loop can render the trace alongside the
                // answer. `output_index` carries the position of
                // the reasoning item in `output`, useful when
                // multiple reasoning blocks interleave with text.
                if let Some(delta) = chunk.delta {
                    if !delta.is_empty() {
                        return Ok(Some(StreamEvent::ThinkingDelta {
                            content_index: chunk.output_index.map(|i| i as usize).unwrap_or(0),
                            delta,
                        }));
                    }
                }
                Ok(None)
            }
            "response.reasoning_summary_text.delta" => {
                // F25: streaming summary-of-reasoning text. When the
                // request includes `reasoning: {summary: "auto"}`,
                // OpenAI emits a parallel summary stream —
                // surface as ThinkingDelta on a separate content
                // index so the caller can render trace + summary
                // side-by-side.
                if let Some(delta) = chunk.delta {
                    if !delta.is_empty() {
                        return Ok(Some(StreamEvent::ThinkingDelta {
                            content_index: chunk.output_index.map(|i| i as usize).unwrap_or(0),
                            delta,
                        }));
                    }
                }
                Ok(None)
            }
            "response.function_call_arguments.delta" => {
                if let Some(delta) = chunk.delta {
                    let idx = chunk.output_index.map(|i| i as usize).unwrap_or(0);
                    if let Some(item_id) = chunk.item_id.as_deref() {
                        // First time we see this call: register id
                        // *and* fold the first arg fragment into the
                        // buffer — the server sends the arg delta
                        // stream starting from the very first
                        // chunk, so dropping it loses the start of
                        // the JSON.
                        let is_new = self.tool_call_accumulator.is_new_call(idx, item_id);
                        let _ = self.tool_call_accumulator.accumulate(
                            idx,
                            Some(item_id.to_string()),
                            None,
                            Some(delta.clone()),
                        );
                        if is_new {
                            return Ok(Some(StreamEvent::ToolCallStart { content_index: idx }));
                        }
                        return Ok(Some(StreamEvent::ToolCallDelta {
                            content_index: idx,
                            delta,
                        }));
                    }
                }
                Ok(None)
            }
            "response.output_item.added" => {
                // First signal for a new output item. For a
                // function_call we register its identity (id + name)
                // in the accumulator so subsequent argument deltas
                // can complete it, and emit ToolCallStart so the
                // engine loop can open a content slot.
                if let Some(item) = chunk.item {
                    if item.item_type == "function_call" {
                        let idx = chunk.output_index.map(|i| i as usize).unwrap_or(0);
                        let id = item.call_id.clone();
                        let name = item.name.clone();
                        if id.is_some() || name.is_some() {
                            let _ = self.tool_call_accumulator.accumulate(idx, id, name, None);
                        }
                        return Ok(Some(StreamEvent::ToolCallStart { content_index: idx }));
                    }
                }
                Ok(None)
            }
            "response.output_item.done" => {
                if let Some(item) = chunk.item {
                    if item.item_type == "function_call" {
                        let idx = chunk.output_index.map(|i| i as usize).unwrap_or(0);
                        // Some servers only carry name/id in the
                        // `done` event; register anything we missed.
                        // `accumulate` returns the completed call
                        // once id+name+args are all present and the
                        // accumulated arguments parse as JSON — in
                        // that case we can emit ToolCallEnd directly
                        // without a separate `finalize()` round.
                        if let Some(call) = self.tool_call_accumulator.accumulate(
                            idx,
                            item.call_id,
                            item.name,
                            None,
                        ) {
                            return Ok(Some(StreamEvent::ToolCallEnd {
                                content_index: idx,
                                tool_call: call,
                            }));
                        }
                        // Buffer still pending — fall back to
                        // finalize for any case where the args never
                        // reached valid JSON.
                        if let Some(call) = self.tool_call_accumulator.finalize(idx) {
                            return Ok(Some(StreamEvent::ToolCallEnd {
                                content_index: idx,
                                tool_call: call,
                            }));
                        }
                    }
                }
                Ok(None)
            }
            "response.completed" => {
                // Reset the accumulator in case the next request
                // arrives on the same adapter instance.
                self.tool_call_accumulator.reset();

                if let Some(resp) = chunk.response {
                    let usage = resp.usage.unwrap_or_default();
                    return Ok(Some(StreamEvent::Usage {
                        input: i64_to_u64(usage.input_tokens),
                        output: i64_to_u64(usage.output_tokens),
                        total: i64_to_u64(usage.total_tokens),
                        cache_creation_input_tokens: usage
                            .input_tokens_details
                            .as_ref()
                            .map_or(0, |d| i64_to_u64(d.cache_write_tokens)),
                        cache_read_input_tokens: usage
                            .input_tokens_details
                            .as_ref()
                            .map_or(0, |d| i64_to_u64(d.cached_tokens)),
                        reasoning_output_tokens: usage
                            .output_tokens_details
                            .as_ref()
                            .map_or(0, |d| i64_to_u64(d.reasoning_tokens)),
                    }));
                }
                Ok(None)
            }
            "response.failed" => {
                self.tool_call_accumulator.reset();
                // Surface as Done with Error reason — there's no
                // fatal-error variant in StreamEvent today, and the
                // engine loop's error path handles this gracefully.
                Ok(Some(StreamEvent::Done {
                    stop_reason: StopReason::Error,
                }))
            }
            _ => {
                // Skip unknown event types (response.incomplete,
                // response.reasoning_*, response.refusal.delta, …).
                Ok(None)
            }
        }
    }

    fn auth_config(&self, api_key: &str) -> AuthConfig {
        AuthConfig::Bearer {
            token: api_key.to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// Wire types (private)
// ---------------------------------------------------------------------------

/// One entry in the `input` array of a Responses request.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ResponsesInputItem {
    Message {
        role: String,
        content: Vec<ResponsesContentPart>,
    },
    FunctionCall {
        call_id: String,
        name: String,
        arguments: String,
    },
    FunctionCallOutput {
        call_id: String,
        output: String,
    },
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ResponsesContentPart {
    InputText { text: String },
    OutputText { text: String },
}

#[derive(Debug, Serialize)]
struct ResponsesTool {
    #[serde(rename = "type")]
    tool_type: &'static str,
    name: String,
    description: String,
    parameters: Value,
}

/// Response body shape for the blocking call. Mirrors codex's
/// `ResponseCompletedUsage` projection.
#[derive(Debug, Default, Deserialize)]
struct ResponsesApiResponseBody {
    #[serde(default)]
    output: Vec<ResponsesOutputItem>,
    #[serde(default)]
    usage: Option<ResponsesUsage>,
    /// When the response was cut short, OpenAI sets
    /// `incomplete_details.reason` (e.g. `"max_output_tokens"`).
    #[serde(default)]
    incomplete_details: Option<ResponsesIncompleteDetails>,
}

#[derive(Debug, Default, Deserialize)]
struct ResponsesIncompleteDetails {
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ResponsesOutputItem {
    Message {
        #[serde(default)]
        content: Vec<ResponsesContentPart>,
        // `role` is part of the wire shape but peko's projection
        // doesn't need it (we synthesize output text); deserialize
        // for forward compatibility with future variants that might
        // surface it.
        #[serde(default)]
        #[allow(dead_code)]
        role: Option<String>,
    },
    FunctionCall {
        // `id` mirrors `call_id` on the wire; we key everything off
        // `call_id` so this stays deserialized-only for forward
        // compatibility.
        #[serde(default)]
        #[allow(dead_code)]
        id: Option<String>,
        call_id: String,
        name: String,
        arguments: String,
    },
    Reasoning {
        // Reasoning items are skipped (their text surfaces through
        // the streaming SSE delta path); the field stays here so a
        // future extension can reach it.
        #[serde(default)]
        #[allow(dead_code)]
        summary: Vec<Value>,
    },
    #[serde(other)]
    Unknown,
}

impl Default for ResponsesOutputItem {
    fn default() -> Self {
        Self::Unknown
    }
}

/// Convert a possibly-negative i64 token count into a u64. OpenAI
/// occasionally reports negative cache fields for cache invalidation
/// events; clamp those to zero rather than silently underflowing.
fn i64_to_u64(n: i64) -> u64 {
    if n < 0 {
        0
    } else {
        n as u64
    }
}

#[derive(Debug, Default, Deserialize)]
struct ResponsesUsage {
    #[serde(default)]
    input_tokens: i64,
    #[serde(default)]
    output_tokens: i64,
    #[serde(default)]
    total_tokens: i64,
    #[serde(default)]
    input_tokens_details: Option<ResponsesInputTokensDetails>,
    #[serde(default)]
    output_tokens_details: Option<ResponsesOutputTokensDetails>,
}

#[derive(Debug, Default, Deserialize)]
struct ResponsesInputTokensDetails {
    #[serde(default)]
    cached_tokens: i64,
    #[serde(default)]
    cache_write_tokens: i64,
}

#[derive(Debug, Default, Deserialize)]
struct ResponsesOutputTokensDetails {
    #[serde(default)]
    reasoning_tokens: i64,
}

/// SSE event envelope. The Responses API uses
/// `event: response.<thing>.<verb>` with JSON `data:` payloads; we
/// only need to discriminate on the `type` discriminator on each
/// payload. Optional `item`, `delta`, `output_index`, `item_id`,
/// and `response` fields are populated by specific event variants.
#[derive(Debug, Default, Deserialize)]
struct ResponsesStreamEvent {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    delta: Option<String>,
    #[serde(default)]
    output_index: Option<i64>,
    #[serde(default)]
    item_id: Option<String>,
    #[serde(default)]
    item: Option<ResponsesStreamItem>,
    #[serde(default)]
    response: Option<ResponsesStreamResponse>,
}

/// Subset of `ResponseItem` shape that shows up in
/// `response.output_item.added` / `response.output_item.done` events.
#[derive(Debug, Default, Deserialize)]
struct ResponsesStreamItem {
    #[serde(rename = "type", default)]
    item_type: String,
    // `call_id` and `name` are populated by the server in
    // `output_item.added` and `output_item.done`; the SSE handler
    // reads them to seed the tool-call accumulator before
    // finalizing.
    #[serde(default, rename = "call_id")]
    call_id: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ResponsesStreamResponse {
    #[serde(default)]
    usage: Option<ResponsesUsage>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::ApiAdapter;
    use super::*;
    use crate::common::types::message::{ContentBlock, MessageRole};
    use crate::providers::traits::{LlmMessage, ThinkingEffort};

    fn user_msg(text: &str) -> LlmMessage {
        LlmMessage {
            role: MessageRole::User,
            content: vec![ContentBlock::Text {
                text: text.to_string(),
            }],
            ..Default::default()
        }
    }

    fn system_msg(text: &str) -> LlmMessage {
        LlmMessage {
            role: MessageRole::System,
            content: vec![ContentBlock::Text {
                text: text.to_string(),
            }],
            ..Default::default()
        }
    }

    fn assistant_with_tool_call(id: &str, name: &str, args: Value) -> LlmMessage {
        LlmMessage {
            role: MessageRole::Assistant,
            content: vec![
                ContentBlock::Text {
                    text: "Calling tool".to_string(),
                },
                ContentBlock::ToolCall {
                    id: id.to_string(),
                    name: name.to_string(),
                    arguments: args,
                },
            ],
            ..Default::default()
        }
    }

    fn tool_result_msg(call_id: &str, text: &str) -> LlmMessage {
        LlmMessage {
            role: MessageRole::Tool,
            content: vec![ContentBlock::Text {
                text: text.to_string(),
            }],
            tool_call_id: Some(call_id.to_string()),
            ..Default::default()
        }
    }

    fn make_adapter() -> OpenAiResponsesAdapter {
        OpenAiResponsesAdapter::new()
    }

    // ---- build_request -------------------------------------------------

    #[test]
    fn build_request_endpoint_is_responses() {
        let adapter = make_adapter();
        let messages = vec![user_msg("hi")];
        let options = ChatOptions::default();
        let (path, _) = adapter
            .build_request("gpt-test", &messages, None, &options, false)
            .unwrap();
        assert_eq!(path, "/responses");
    }

    #[test]
    fn build_request_system_lifted_to_instructions() {
        let adapter = make_adapter();
        let messages = vec![system_msg("You are helpful."), user_msg("hi")];
        let options = ChatOptions::default();
        let (_, body) = adapter
            .build_request("gpt-test", &messages, None, &options, false)
            .unwrap();
        assert_eq!(body["instructions"], "You are helpful.");
        // No System item should appear in input.
        let input = body["input"].as_array().unwrap();
        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["role"], "user");
    }

    #[test]
    fn build_request_instructions_empty_when_no_system_message() {
        let adapter = make_adapter();
        let messages = vec![user_msg("hi")];
        let (_, body) = adapter
            .build_request("gpt-test", &messages, None, &options_default(), false)
            .unwrap();
        assert_eq!(body["instructions"], "");
    }

    #[test]
    fn build_request_user_message_uses_input_text_part() {
        let adapter = make_adapter();
        let messages = vec![user_msg("Hello there")];
        let (_, body) = adapter
            .build_request("gpt-test", &messages, None, &options_default(), false)
            .unwrap();
        let input = body["input"].as_array().unwrap();
        assert_eq!(input[0]["type"], "message");
        assert_eq!(input[0]["role"], "user");
        let content = input[0]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "input_text");
        assert_eq!(content[0]["text"], "Hello there");
    }

    #[test]
    fn build_request_assistant_tool_call_uses_function_call_item() {
        let adapter = make_adapter();
        let args = json!({"path": "/etc"});
        let messages = vec![
            user_msg("read"),
            assistant_with_tool_call("c1", "Read", args.clone()),
        ];
        let (_, body) = adapter
            .build_request("gpt-test", &messages, None, &options_default(), false)
            .unwrap();
        let input = body["input"].as_array().unwrap();
        assert_eq!(input.len(), 3);
        // assistant text message
        assert_eq!(input[1]["type"], "message");
        assert_eq!(input[1]["role"], "assistant");
        // function_call item with arguments as JSON STRING
        assert_eq!(input[2]["type"], "function_call");
        assert_eq!(input[2]["call_id"], "c1");
        assert_eq!(input[2]["name"], "Read");
        assert_eq!(input[2]["arguments"], serde_json::to_string(&args).unwrap());
    }

    #[test]
    fn build_request_tool_result_uses_function_call_output() {
        let adapter = make_adapter();
        let messages = vec![user_msg("do it"), tool_result_msg("c1", "ok")];
        let (_, body) = adapter
            .build_request("gpt-test", &messages, None, &options_default(), false)
            .unwrap();
        let input = body["input"].as_array().unwrap();
        assert_eq!(input[1]["type"], "function_call_output");
        assert_eq!(input[1]["call_id"], "c1");
        assert_eq!(input[1]["output"], "ok");
    }

    #[test]
    fn build_request_prompt_cache_key_emitted_when_set() {
        let adapter = make_adapter();
        let messages = vec![user_msg("hi")];
        let options = ChatOptions {
            prompt_cache_key: Some("sess-1".to_string()),
            // `options_default()` is `CacheRetention::None` (opt-out);
            // explicitly opt in so the gate in build_request fires.
            cache_retention: CacheRetention::Default,
            ..options_default()
        };
        let (_, body) = adapter
            .build_request("gpt-test", &messages, None, &options, false)
            .unwrap();
        assert_eq!(body["prompt_cache_key"], "sess-1");
    }

    #[test]
    fn build_request_long_retention_sets_24h() {
        let adapter = make_adapter();
        let messages = vec![user_msg("hi")];
        let options = ChatOptions {
            cache_retention: CacheRetention::Long,
            ..options_default()
        };
        let (_, body) = adapter
            .build_request("gpt-test", &messages, None, &options, false)
            .unwrap();
        assert_eq!(body["prompt_cache_retention"], "24h");
    }

    #[test]
    fn build_request_default_retention_omits_retention_field() {
        let adapter = make_adapter();
        let messages = vec![user_msg("hi")];
        let options = ChatOptions {
            prompt_cache_key: Some("sess-1".to_string()),
            cache_retention: CacheRetention::Default,
            ..options_default()
        };
        let (_, body) = adapter
            .build_request("gpt-test", &messages, None, &options, false)
            .unwrap();
        assert_eq!(body["prompt_cache_key"], "sess-1");
        assert!(body.get("prompt_cache_retention").is_none());
    }

    #[test]
    fn build_request_none_retention_omits_cache_fields() {
        let adapter = make_adapter();
        let messages = vec![user_msg("hi")];
        let options = ChatOptions {
            cache_retention: CacheRetention::None,
            prompt_cache_key: Some("sess-1".to_string()),
            ..options_default()
        };
        let (_, body) = adapter
            .build_request("gpt-test", &messages, None, &options, false)
            .unwrap();
        assert!(body.get("prompt_cache_key").is_none());
        assert!(body.get("prompt_cache_retention").is_none());
    }

    #[test]
    fn build_request_tools_emitted_with_parallel_default() {
        let adapter = make_adapter();
        let tools = vec![ToolDefinition {
            name: "Read".to_string(),
            description: "Read a file".to_string(),
            parameters: json!({"type": "object"}),
        }];
        let messages = vec![user_msg("hi")];
        let (_, body) = adapter
            .build_request(
                "gpt-test",
                &messages,
                Some(&tools),
                &options_default(),
                false,
            )
            .unwrap();
        assert_eq!(body["tool_choice"], "auto");
        assert_eq!(body["parallel_tool_calls"], true);
        let tools_json = body["tools"].as_array().unwrap();
        assert_eq!(tools_json.len(), 1);
        assert_eq!(tools_json[0]["type"], "function");
        assert_eq!(tools_json[0]["name"], "Read");
    }

    // ---------- F25: reasoning-effort wiring ----------

    /// Default options produce no `reasoning` / `include` fields on
    /// the wire — the pre-F25 shape is preserved when callers don't
    /// opt in.
    #[test]
    fn build_request_thinking_effort_none_omits_reasoning_fields() {
        let adapter = make_adapter();
        let messages = vec![user_msg("hi")];
        let (_, body) = adapter
            .build_request("gpt-test", &messages, None, &options_default(), false)
            .unwrap();
        assert!(body.get("reasoning").is_none());
        assert!(body.get("include").is_none());
    }

    /// `thinking_effort: High` emits `reasoning: {effort: "high"}`
    /// and the encrypted-reasoning include list.
    #[test]
    fn build_request_thinking_effort_high_emits_reasoning_block() {
        let adapter = make_adapter();
        let messages = vec![user_msg("hi")];
        let options = ChatOptions {
            thinking_effort: ThinkingEffort::High,
            encrypted_reasoning: true,
            ..options_default()
        };
        let (_, body) = adapter
            .build_request("gpt-test", &messages, None, &options, false)
            .unwrap();
        assert_eq!(body["reasoning"]["effort"], "high");
        assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
    }

    /// `thinking_summary: Some(true)` adds `reasoning.summary = "auto"`.
    #[test]
    fn build_request_thinking_summary_true_emits_auto() {
        let adapter = make_adapter();
        let messages = vec![user_msg("hi")];
        let options = ChatOptions {
            thinking_effort: ThinkingEffort::Medium,
            thinking_summary: Some(true),
            encrypted_reasoning: true,
            ..options_default()
        };
        let (_, body) = adapter
            .build_request("gpt-test", &messages, None, &options, false)
            .unwrap();
        assert_eq!(body["reasoning"]["effort"], "medium");
        assert_eq!(body["reasoning"]["summary"], "auto");
    }

    /// `encrypted_reasoning: false` suppresses the include list —
    /// callers that don't want reasoning payloads persisted can
    /// opt out cleanly.
    #[test]
    fn build_request_encrypted_reasoning_false_omits_include() {
        let adapter = make_adapter();
        let messages = vec![user_msg("hi")];
        let options = ChatOptions {
            thinking_effort: ThinkingEffort::Low,
            encrypted_reasoning: false,
            ..options_default()
        };
        let (_, body) = adapter
            .build_request("gpt-test", &messages, None, &options, false)
            .unwrap();
        assert_eq!(body["reasoning"]["effort"], "low");
        assert!(body.get("include").is_none());
    }

    /// `Adaptive` falls back to `medium` on the wire — Responses
    /// doesn't have a native adaptive knob, so callers that want
    /// the closest thing to Opus-4-6+ adaptive thinking should use
    /// the Anthropic adapter instead.
    #[test]
    fn build_request_thinking_effort_adaptive_falls_back_to_medium() {
        let adapter = make_adapter();
        let messages = vec![user_msg("hi")];
        let options = ChatOptions {
            thinking_effort: ThinkingEffort::Adaptive,
            ..options_default()
        };
        let (_, body) = adapter
            .build_request("gpt-test", &messages, None, &options, false)
            .unwrap();
        assert_eq!(body["reasoning"]["effort"], "medium");
    }

    // ---- parse_response ------------------------------------------------

    #[test]
    fn parse_response_text_only_message() {
        let adapter = make_adapter();
        let body = json!({
            "output": [{
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "Hello"}]
            }],
            "usage": {"input_tokens": 5, "output_tokens": 2, "total_tokens": 7}
        });
        let resp = adapter.parse_response("gpt-test", body).unwrap();
        assert_eq!(resp.content.len(), 1);
        match &resp.content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "Hello"),
            _ => panic!("expected Text block"),
        }
        assert!(resp.tool_calls.is_empty());
        assert_eq!(resp.stop_reason, StopReason::Stop);
        assert_eq!(resp.usage.input, 5);
        assert_eq!(resp.usage.output, 2);
        assert_eq!(resp.usage.total, 7);
        assert_eq!(resp.usage.cache_read_input_tokens, None);
        assert_eq!(resp.usage.cache_creation_input_tokens, None);
    }

    #[test]
    fn parse_response_function_call_arguments_parsed_from_string() {
        let adapter = make_adapter();
        let body = json!({
            "output": [{
                "type": "function_call",
                "call_id": "c1",
                "name": "Read",
                "arguments": "{\"path\":\"/etc/passwd\"}"
            }],
            "usage": {"input_tokens": 1, "output_tokens": 2, "total_tokens": 3}
        });
        let resp = adapter.parse_response("gpt-test", body).unwrap();
        assert_eq!(resp.tool_calls.len(), 1);
        match &resp.tool_calls[0] {
            ContentBlock::ToolCall {
                id,
                name,
                arguments,
            } => {
                assert_eq!(id, "c1");
                assert_eq!(name, "Read");
                assert_eq!(arguments, &json!({"path":"/etc/passwd"}));
            }
            _ => panic!("expected ToolCall block"),
        }
        assert_eq!(resp.stop_reason, StopReason::ToolUse);
    }

    #[test]
    fn parse_response_usage_cache_read_and_write() {
        let adapter = make_adapter();
        let body = json!({
            "output": [{
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "x"}]
            }],
            "usage": {
                "input_tokens": 100,
                "output_tokens": 5,
                "total_tokens": 105,
                "input_tokens_details": {"cached_tokens": 40, "cache_write_tokens": 5}
            }
        });
        let resp = adapter.parse_response("gpt-test", body).unwrap();
        assert_eq!(resp.usage.cache_read_input_tokens, Some(40));
        assert_eq!(resp.usage.cache_creation_input_tokens, Some(5));
    }

    #[test]
    fn parse_response_max_output_tokens_maps_to_length() {
        let adapter = make_adapter();
        let body = json!({
            "output": [{
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "..."}]
            }],
            "incomplete_details": {"reason": "max_output_tokens"},
            "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}
        });
        let resp = adapter.parse_response("gpt-test", body).unwrap();
        assert_eq!(resp.stop_reason, StopReason::Length);
    }

    // ---- parse_sse_event ----------------------------------------------

    #[test]
    fn parse_sse_event_text_delta() {
        let adapter = make_adapter();
        let data = r#"{"type":"response.output_text.delta","delta":"Hi"}"#;
        let ev = adapter.parse_sse_event("gpt-test", data).unwrap().unwrap();
        match ev {
            StreamEvent::TextDelta { delta, .. } => assert_eq!(delta, "Hi"),
            other => panic!("expected TextDelta, got {other:?}"),
        }
    }

    #[test]
    fn parse_sse_event_function_call_arguments_delta_accumulates() {
        let adapter = make_adapter();
        // First delta carries the call's identity (item_id) and a
        // first fragment of arguments. This should emit ToolCallStart.
        let data1 = r#"{"type":"response.function_call_arguments.delta","output_index":0,"item_id":"c1","delta":"{\"x\":"}"#;
        let ev1 = adapter.parse_sse_event("gpt-test", data1).unwrap().unwrap();
        assert!(
            matches!(ev1, StreamEvent::ToolCallStart { content_index: 0 }),
            "expected ToolCallStart, got {ev1:?}"
        );

        // Second delta is more arguments — emit ToolCallDelta.
        let data2 = r#"{"type":"response.function_call_arguments.delta","output_index":0,"item_id":"c1","delta":"1}"}"#;
        let ev2 = adapter.parse_sse_event("gpt-test", data2).unwrap().unwrap();
        match ev2 {
            StreamEvent::ToolCallDelta { delta, .. } => assert_eq!(delta, "1}"),
            other => panic!("expected ToolCallDelta, got {other:?}"),
        }

        // output_item.done with function_call type — emit ToolCallEnd
        // with the parsed arguments.
        let data3 = r#"{"type":"response.output_item.done","output_index":0,"item":{"type":"function_call","call_id":"c1","name":"foo"}}"#;
        let ev3 = adapter.parse_sse_event("gpt-test", data3).unwrap().unwrap();
        match ev3 {
            StreamEvent::ToolCallEnd { tool_call, .. } => match tool_call {
                ContentBlock::ToolCall {
                    id,
                    name,
                    arguments,
                } => {
                    assert_eq!(id, "c1");
                    assert_eq!(name, "foo");
                    assert_eq!(arguments, json!({"x": 1}));
                }
                _ => panic!("expected ToolCall block"),
            },
            other => panic!("expected ToolCallEnd, got {other:?}"),
        }
    }

    #[test]
    fn parse_sse_event_completed_emits_usage() {
        let adapter = make_adapter();
        let data = r#"{"type":"response.completed","response":{"usage":{"input_tokens":10,"output_tokens":5,"total_tokens":15,"input_tokens_details":{"cached_tokens":3}}}}"#;
        let ev = adapter.parse_sse_event("gpt-test", data).unwrap().unwrap();
        match ev {
            StreamEvent::Usage {
                input,
                output,
                total,
                cache_read_input_tokens,
                ..
            } => {
                assert_eq!(input, 10);
                assert_eq!(output, 5);
                assert_eq!(total, 15);
                assert_eq!(cache_read_input_tokens, 3);
            }
            other => panic!("expected Usage, got {other:?}"),
        }
    }

    #[test]
    fn parse_sse_event_failed_emits_done_with_error() {
        let adapter = make_adapter();
        let data = r#"{"type":"response.failed","response":{"error":{"code":"server_error","message":"oops"}}}"#;
        let ev = adapter.parse_sse_event("gpt-test", data).unwrap().unwrap();
        assert!(matches!(
            ev,
            StreamEvent::Done {
                stop_reason: StopReason::Error
            }
        ));
    }

    #[test]
    fn parse_sse_event_unknown_kind_returns_none() {
        let adapter = make_adapter();
        let data = r#"{"type":"response.incomplete","response":{}}"#;
        let ev = adapter.parse_sse_event("gpt-test", data).unwrap();
        assert!(ev.is_none());
    }

    // ---- F25 reasoning SSE / response ----------------------------------

    /// `response.reasoning_text.delta` surfaces as `ThinkingDelta`
    /// with the raw reasoning chunk.
    #[test]
    fn parse_sse_event_reasoning_text_delta() {
        let adapter = make_adapter();
        let data =
            r#"{"type":"response.reasoning_text.delta","output_index":0,"delta":"thinking step"}"#;
        let ev = adapter.parse_sse_event("gpt-test", data).unwrap().unwrap();
        match ev {
            StreamEvent::ThinkingDelta {
                content_index,
                delta,
            } => {
                assert_eq!(content_index, 0);
                assert_eq!(delta, "thinking step");
            }
            other => panic!("expected ThinkingDelta, got {other:?}"),
        }
    }

    /// `response.reasoning_summary_text.delta` is the summary
    /// stream emitted when the request includes
    /// `reasoning.summary = "auto"`. Same `ThinkingDelta` shape.
    #[test]
    fn parse_sse_event_reasoning_summary_text_delta() {
        let adapter = make_adapter();
        let data = r#"{"type":"response.reasoning_summary_text.delta","output_index":1,"delta":"Concise answer plan"}"#;
        let ev = adapter.parse_sse_event("gpt-test", data).unwrap().unwrap();
        match ev {
            StreamEvent::ThinkingDelta {
                content_index,
                delta,
            } => {
                assert_eq!(content_index, 1);
                assert_eq!(delta, "Concise answer plan");
            }
            other => panic!("expected ThinkingDelta, got {other:?}"),
        }
    }

    /// Blocking `parse_response` surfaces Reasoning items as
    /// `ContentBlock::Thinking` blocks, ordering them before any
    /// Message text in the same response.
    #[test]
    fn parse_response_surfaces_reasoning_summary() {
        let adapter = make_adapter();
        let body = json!({
            "output": [
                {
                    "type": "reasoning",
                    "summary": [
                        {"type": "summary_text", "text": "step one "},
                        {"type": "summary_text", "text": "step two"}
                    ]
                },
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "answer"}]
                }
            ],
            "usage": {"input_tokens": 5, "output_tokens": 3, "total_tokens": 8}
        });
        let parsed = adapter.parse_response("gpt-test", body).unwrap();
        assert_eq!(parsed.content.len(), 2);
        match &parsed.content[0] {
            ContentBlock::Thinking { text, signature } => {
                assert_eq!(text, "step one step two");
                assert!(signature.is_none());
            }
            other => panic!("expected Thinking first, got {other:?}"),
        }
        assert!(matches!(&parsed.content[1], ContentBlock::Text { text } if text == "answer"));
    }

    // ---- Capability flags ---------------------------------------------

    #[test]
    fn supports_native_and_prompt_cache_control_true() {
        let adapter = make_adapter();
        assert!(adapter.supports_native_tools());
        assert!(adapter.supports_prompt_cache_control());
    }

    #[test]
    fn auth_config_is_bearer() {
        let adapter = make_adapter();
        match adapter.auth_config("k") {
            AuthConfig::Bearer { token } => assert_eq!(token, "k"),
            other => panic!("expected Bearer auth, got {other:?}"),
        }
    }

    #[test]
    fn name_and_base_url_default() {
        let adapter = make_adapter();
        assert_eq!(adapter.name(), "openai_responses");
        assert_eq!(adapter.base_url(), "https://api.openai.com/v1");
    }

    #[test]
    fn with_base_url_overrides() {
        let adapter = OpenAiResponsesAdapter::new().with_base_url("https://proxy.example/v1");
        assert_eq!(adapter.base_url(), "https://proxy.example/v1");
    }

    // ---- helpers -------------------------------------------------------

    fn options_default() -> ChatOptions {
        ChatOptions {
            temperature: None,
            max_tokens: None,
            api_key: None,
            headers: Default::default(),
            cache_retention: CacheRetention::None,
            prompt_cache_key: None,
            // F25: default to no reasoning on the wire so existing
            // tests that don't set these fields keep producing the
            // pre-F25 request shape.
            thinking_effort: crate::providers::traits::ThinkingEffort::None,
            thinking_summary: None,
            encrypted_reasoning: false,
        }
    }
}
