//! Agent message types - abstraction layer for LLM messages
//!
//! This module provides the message type system used across the runtime:
//! - Standard LLM messages (system, user, assistant, tool)
//! - Content blocks (text, image, tool call/result, thinking)
//! - Token usage accounting
//!
//! History repair lives in [`repair`]; session-storage content blocks
//! live in [`session_blocks`].

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

pub mod repair;
mod tool_call_info;

/// Lightweight tool call DTO with optional result.
///
/// Defined here in the message crate so `peko-engine` can hold
/// `Vec<ToolCallInfo>` without pulling in a heavier crate for a
/// 4-field DTO.
pub use tool_call_info::ToolCallInfo;

/// Session content blocks (`ToolCallBlock`, `ThinkingBlock`).
///
/// Lifted from `crate::session::events` in Phase 9b.N.5b.9b so
/// `peko_engine::SessionView` can accept them in its trait signature.
/// Pure data blocks with `serde` derives only — no behavior.
pub mod session_blocks;
pub use session_blocks::{ThinkingBlock, ToolCallBlock};

/// Unique identifier for tool calls
pub type ToolCallId = String;

/// Content block types for messages
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    /// Plain text content
    Text { text: String },

    /// Image content (base64 or URL)
    Image {
        source: ImageSource,
        mime_type: String,
    },

    /// Tool call request
    ToolCall {
        id: ToolCallId,
        name: String,
        arguments: Value,
    },

    /// Tool execution result
    ToolResult {
        tool_call_id: ToolCallId,
        name: String,
        content: Vec<ContentBlock>,
        is_error: bool,
    },

    /// Thinking/reasoning block
    Thinking {
        text: String,
        signature: Option<String>,
    },
}

impl TokenUsage {
    /// Accumulate `other` into `self`, folding cache reads/writes
    /// into the canonical `input` bucket and reasoning tokens into
    /// `output`. Preserves the raw cache/reasoning sub-fields so the
    /// audit trail in the JSONL session file retains the breakdown.
    ///
    /// This mirrors the folding rule used by the engine loop's
    /// `iteration_usage` accumulator (`engine/agentic_loop.rs`) — a
    /// single source of truth for "what counts toward a 1M input
    /// tokens/day quota".
    pub fn accumulate(&mut self, other: &TokenUsage) {
        let cache_creation = other.cache_creation_input_tokens.unwrap_or(0);
        let cache_read = other.cache_read_input_tokens.unwrap_or(0);
        let reasoning = other.reasoning_output_tokens.unwrap_or(0);
        self.input += other.input + cache_creation + cache_read;
        self.output += other.output + reasoning;
        self.total += other.total + cache_creation + cache_read + reasoning;
        if cache_creation > 0 {
            *self.cache_creation_input_tokens.get_or_insert(0) += cache_creation;
        }
        if cache_read > 0 {
            *self.cache_read_input_tokens.get_or_insert(0) += cache_read;
        }
        if reasoning > 0 {
            *self.reasoning_output_tokens.get_or_insert(0) += reasoning;
        }
    }
}

/// Image dimensions in pixels.
///
/// PR 1 of the image-aware retention budget (`features/image-aware-retention`).
/// When present, the compaction estimator uses
/// `ceil(width * height / 750)` (OpenAI "high detail" tile math,
/// conservative). When absent the estimator falls back to base64 bytes
/// (`bytes / 0.75`) for `Base64` sources or a mime-type lookup table
/// for `Url` sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageDimensions {
    pub width: u32,
    pub height: u32,
}

impl ImageDimensions {
    /// Tokens consumed at OpenAI "high detail" tile granularity.
    /// 750 px²/token is the canonical "high detail" tile size
    /// (Anthropic uses a similar ~750 figure for Claude 3+ vision).
    /// Rounded up so a 1px sliver still counts.
    #[must_use]
    pub fn high_detail_tokens(&self) -> usize {
        let px = u64::from(self.width) * u64::from(self.height);
        // ceil(px / 750) without overflow on large images
        px.div_ceil(750) as usize
    }
}

/// Image source for image content blocks.
///
/// `dimensions` is optional — adapters populate it when they can
/// parse the source (base64 header magic bytes, data-URL metadata,
/// content-length + content-type probes, etc.). When absent the
/// estimator falls back to bytes or mime-type heuristics, so the
/// field is opt-in for callers and serde-default for JSONL
/// backward-compatibility.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "source_type", rename_all = "snake_case")]
pub enum ImageSource {
    /// Base64-encoded image data
    Base64 {
        data: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dimensions: Option<ImageDimensions>,
    },
    /// URL to image
    Url {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dimensions: Option<ImageDimensions>,
    },
}

/// Standard LLM message roles
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

/// Token usage statistics
///
/// `input` and `output` are the canonical wire-reported counts
/// (`input_tokens` / `output_tokens` on Anthropic, `prompt_tokens` /
/// `completion_tokens` on OpenAI). `total` is the provider's wire
/// `total_tokens` field when present (OpenAI), or `input + output` when
/// the provider does not report a separate total (Anthropic).
///
/// The three cache/reasoning sub-fields are populated only by adapters
/// that have the corresponding wire fields. They are folded into the
/// canonical `input` / `output` fields by the engine loop accumulator
/// for downstream quota accounting, but preserved verbatim here so the
/// session JSONL retains the raw breakdown for audit.
///
/// `#[serde(default)]` on each sub-field keeps old JSONL files
/// (pre-F17) loadable — missing fields deserialize as `None`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    /// Wire-reported prompt / input tokens (uncached, non-reasoning).
    pub input: u64,
    /// Wire-reported completion / output tokens (including reasoning /
    /// thinking tokens, which Anthropic folds into `output_tokens`).
    pub output: u64,
    /// Wire-reported `total_tokens` when the provider supplies one;
    /// otherwise the loop sets this to `input + output` after
    /// accumulation.
    pub total: u64,
    /// Anthropic `cache_creation_input_tokens`. Tokens billed at the
    /// cache-write rate for newly cached prompt prefixes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u64>,
    /// Anthropic `cache_read_input_tokens` / OpenAI
    /// `prompt_tokens_details.cached_tokens`. Tokens billed at the
    /// cache-read rate (typically ~10% of input).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u64>,
    /// OpenAI `completion_tokens_details.reasoning_tokens`. Subset of
    /// `output` billed at output rate; tracked separately so quota
    /// users can distinguish "thinking" from "visible text" output.
    /// Anthropic folds reasoning into `output_tokens` already.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_output_tokens: Option<u64>,
}

/// Standard LLM message
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmMessage {
    pub role: MessageRole,
    pub content: Vec<ContentBlock>,
    pub timestamp: DateTime<Utc>,
    pub metadata: HashMap<String, Value>,
    /// Tool call ID for tool-result messages
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Provider-reported token usage for this assistant turn. Populated on
    /// assistant messages by the engine loop and by `SessionMessage::to_llm_message`
    /// for replay from session storage. The compactor's
    /// `estimate_context_tokens` walks backward to find the most recent
    /// assistant message with `usage.is_some()` and anchors its size estimate
    /// there, char/4-estimating only the trailing slice. Pre-F21 JSONL files
    /// don't carry this field; `#[serde(default)]` deserialises them as `None`
    /// so old session state keeps loading.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<TokenUsage>,
}

impl Default for LlmMessage {
    fn default() -> Self {
        Self {
            role: MessageRole::User,
            content: Vec::new(),
            timestamp: Utc::now(),
            metadata: HashMap::new(),
            tool_call_id: None,
            usage: None,
        }
    }
}

impl LlmMessage {
    /// Create a simple text message
    pub fn text(role: MessageRole, text: impl Into<String>) -> Self {
        Self {
            role,
            content: vec![ContentBlock::Text { text: text.into() }],
            timestamp: Utc::now(),
            metadata: HashMap::new(),
            tool_call_id: None,
            usage: None,
        }
    }

    /// Create a system message
    pub fn system(text: impl Into<String>) -> Self {
        Self::text(MessageRole::System, text)
    }

    /// Create a user message
    pub fn user(text: impl Into<String>) -> Self {
        Self::text(MessageRole::User, text)
    }

    /// Create an assistant message
    pub fn assistant(text: impl Into<String>) -> Self {
        Self::text(MessageRole::Assistant, text)
    }

    /// Create a tool result message
    ///
    /// `is_error` propagates to the `ContentBlock::ToolResult.is_error` field
    /// (F32a) — false for a successful dispatch, true when the tool itself
    /// failed. Models that distinguish failed tool calls (e.g. Anthropic's
    /// `is_error` field on `tool_result` blocks) see the correct shape.
    pub fn tool_result(
        tool_call_id: impl Into<String>,
        name: impl Into<String>,
        result: impl Into<String>,
        is_error: bool,
    ) -> Self {
        let tool_call_id_str = tool_call_id.into();
        Self {
            role: MessageRole::Tool,
            content: vec![ContentBlock::ToolResult {
                tool_call_id: tool_call_id_str.clone(),
                name: name.into(),
                content: vec![ContentBlock::Text {
                    text: result.into(),
                }],
                is_error,
            }],
            timestamp: Utc::now(),
            metadata: HashMap::new(),
            tool_call_id: Some(tool_call_id_str),
            usage: None,
        }
    }

    /// Add metadata to the message
    pub fn with_metadata(mut self, key: impl Into<String>, value: Value) -> Self {
        self.metadata.insert(key.into(), value);
        self
    }

    /// Set the tool call ID
    pub fn with_tool_call_id(mut self, tool_call_id: impl Into<String>) -> Self {
        self.tool_call_id = Some(tool_call_id.into());
        self
    }

    /// Attach provider-reported token usage to this message.
    ///
    /// Only assistant turns carry usage today (user / system / tool
    /// messages always serialize with `usage: None`). Used by the
    /// engine loop at assistant-message construction so the
    /// compactor's `estimate_context_tokens` can anchor on real
    /// provider-reported token counts instead of falling back to
    /// chars/4. Accepts `Option<TokenUsage>` so callers can write
    /// `.with_usage(iteration_usage.clone())` directly — passing
    /// `None` is equivalent to leaving the field unset.
    pub fn with_usage(mut self, usage: impl Into<Option<TokenUsage>>) -> Self {
        self.usage = usage.into();
        self
    }
}

/// Best-effort dimension extraction from decoded image bytes.
///
/// Supports PNG (always — IHDR header at offset 16-23, big-endian u32 width
/// then height). JPEG/GIF/WebP are out of scope for this PR; the estimator
/// falls back to bytes/0.75 or mime-type tables when `None`.
///
/// Adapter callers should decode the base64 payload themselves and pass
/// the resulting bytes + `mime_type`. Returns `None` when the format isn't
/// recognized, the signature doesn't match, or the dimensions are zero.
pub fn extract_dimensions_from_base64(bytes: &[u8], mime_type: &str) -> Option<ImageDimensions> {
    if mime_type.eq_ignore_ascii_case("image/png") {
        // PNG signature: 89 50 4E 47 0D 0A 1A 0A
        const PNG_SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        if bytes.len() < 24 || bytes[..8] != PNG_SIGNATURE {
            return None;
        }
        // IHDR chunk: 4-byte length, "IHDR", then 13-byte payload starting
        // with width (BE u32) and height (BE u32). Bytes 16-23 are width+height.
        let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
        let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
        if width == 0 || height == 0 {
            return None;
        }
        Some(ImageDimensions { width, height })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `TokenUsage::accumulate` folds cache reads + writes into the
    /// canonical `input` bucket and reasoning into `output`, while
    /// preserving the raw sub-fields for audit. Single source of truth
    /// for "what counts toward a quota limit" — used by both the
    /// engine loop accumulator and the compactor's multi-call rollup.
    #[test]
    fn test_token_usage_accumulate_folds_cache_and_reasoning() {
        let mut total = TokenUsage::default();
        let iter1 = TokenUsage {
            input: 100,
            output: 50,
            total: 150,
            cache_creation_input_tokens: Some(1024),
            cache_read_input_tokens: Some(4096),
            reasoning_output_tokens: Some(20),
        };
        total.accumulate(&iter1);
        // input folds cache_creation + cache_read in
        assert_eq!(total.input, 100 + 1024 + 4096);
        // output folds reasoning in
        assert_eq!(total.output, 50 + 20);
        // total adds the same fold
        assert_eq!(total.total, 150 + 1024 + 4096 + 20);
        // raw sub-fields preserved for audit
        assert_eq!(total.cache_creation_input_tokens, Some(1024));
        assert_eq!(total.cache_read_input_tokens, Some(4096));
        assert_eq!(total.reasoning_output_tokens, Some(20));

        let iter2 = TokenUsage {
            input: 50,
            output: 30,
            total: 80,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: Some(2048),
            reasoning_output_tokens: None,
        };
        total.accumulate(&iter2);
        assert_eq!(total.input, (100 + 1024 + 4096) + (50 + 2048));
        assert_eq!(total.output, (50 + 20) + 30);
        assert_eq!(total.cache_read_input_tokens, Some(4096 + 2048));
    }

    /// Accumulating zero usage (e.g. an empty stream) leaves the
    /// accumulator unchanged and does not insert `Some(0)` sub-fields.
    /// Sub-fields stay `None` when the added usage had no cache or
    /// reasoning tokens, so JSONL serialisation skips them.
    #[test]
    fn test_token_usage_accumulate_zero_does_not_promote_subfields() {
        let mut total = TokenUsage::default();
        let empty = TokenUsage::default();
        total.accumulate(&empty);
        assert_eq!(total, TokenUsage::default());
        assert_eq!(total.cache_creation_input_tokens, None);
        assert_eq!(total.cache_read_input_tokens, None);
        assert_eq!(total.reasoning_output_tokens, None);
    }

    /// Backwards-compat: serialise the pre-F17 shape (no cache or
    /// reasoning fields) and deserialise into the F17 struct. The
    /// sub-fields must load as `None` so old JSONL files keep working.
    #[test]
    fn test_token_usage_backwards_compat_serde() {
        let legacy = serde_json::json!({
            "input": 100,
            "output": 50,
            "total": 150,
        });
        let usage: TokenUsage = serde_json::from_value(legacy).unwrap();
        assert_eq!(usage.input, 100);
        assert_eq!(usage.output, 50);
        assert_eq!(usage.total, 150);
        assert_eq!(usage.cache_creation_input_tokens, None);
        assert_eq!(usage.cache_read_input_tokens, None);
        assert_eq!(usage.reasoning_output_tokens, None);
    }

    /// Round-trip: serialise an F17-shaped struct and deserialise it.
    /// `skip_serializing_if = "Option::is_none"` keeps the on-disk
    /// shape identical to pre-F17 when the sub-fields are unset, so
    /// existing tools that read session JSONL see no change.
    #[test]
    fn test_token_usage_roundtrip_with_cache_fields() {
        let usage = TokenUsage {
            input: 1000,
            output: 500,
            total: 1500,
            cache_creation_input_tokens: Some(1024),
            cache_read_input_tokens: Some(4096),
            reasoning_output_tokens: Some(200),
        };
        let json = serde_json::to_value(usage).unwrap();
        assert_eq!(json["input"], 1000);
        assert_eq!(json["cache_read_input_tokens"], 4096);
        let parsed: TokenUsage = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, usage);
    }

    /// When no sub-fields are populated, serialisation omits them so
    /// the JSONL shape stays identical to pre-F17.
    #[test]
    fn test_token_usage_serialisation_skips_none_subfields() {
        let usage = TokenUsage {
            input: 100,
            output: 50,
            total: 150,
            ..Default::default()
        };
        let json = serde_json::to_value(usage).unwrap();
        let obj = json.as_object().unwrap();
        assert!(!obj.contains_key("cache_creation_input_tokens"));
        assert!(!obj.contains_key("cache_read_input_tokens"));
        assert!(!obj.contains_key("reasoning_output_tokens"));
    }

    /// F21: round-trip an `LlmMessage` with `usage` populated. The
    /// `usage` field is what lets `estimate_context_tokens` anchor on
    /// real provider-reported token counts — without persistence
    /// working, session reloads would always fall back to chars/4.
    #[test]
    fn test_llm_message_usage_roundtrip() {
        let usage = TokenUsage {
            input: 1200,
            output: 600,
            total: 1800,
            cache_read_input_tokens: Some(4096),
            ..Default::default()
        };
        let msg = LlmMessage::assistant("hello").with_usage(usage);
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["usage"]["input"], 1200);
        assert_eq!(json["usage"]["cache_read_input_tokens"], 4096);
        let parsed: LlmMessage = serde_json::from_value(json).unwrap();
        assert_eq!(parsed.usage, Some(usage));
    }

    /// F21: back-compat — JSON without `usage` deserialises to `None`
    /// so pre-F21 session JSONL files keep loading.
    #[test]
    fn test_llm_message_usage_absent_is_none_on_deserialize() {
        let legacy = serde_json::json!({
            "role": "assistant",
            "content": [{"type": "text", "text": "hi"}],
            "timestamp": "2026-01-01T00:00:00Z",
            "metadata": {}
        });
        let msg: LlmMessage = serde_json::from_value(legacy).unwrap();
        assert_eq!(msg.usage, None);
    }

    /// F21: `skip_serializing_if = "Option::is_none"` keeps the JSONL
    /// shape identical to pre-F21 when no usage is attached.
    #[test]
    fn test_llm_message_usage_serialisation_skips_when_none() {
        let msg = LlmMessage::assistant("hi");
        let json = serde_json::to_value(&msg).unwrap();
        let obj = json.as_object().unwrap();
        assert!(!obj.contains_key("usage"));
    }

    // ===================== F32a: is_error propagation tests =====================

    /// Pin the wire shape: a successful tool call preserves `is_error: false`
    /// into the `ContentBlock::ToolResult` block.
    #[test]
    fn test_llm_message_tool_result_success_carries_is_error_false() {
        let msg = LlmMessage::tool_result("tc1", "Read", "file body", false);
        match &msg.content[0] {
            ContentBlock::ToolResult { is_error, .. } => {
                assert!(!*is_error, "success path must set is_error=false");
            }
            _ => panic!("expected ToolResult block"),
        }
    }

    /// Pin the wire shape: a failed tool execution (e.g. tool impl returned
    /// `Err(anyhow!(...))`, or the dispatch gate refused) must propagate
    /// `is_error: true` to the `ContentBlock::ToolResult` block. Pre-F32a
    /// the constructor hardcoded `false` regardless of dispatch outcome.
    #[test]
    fn test_llm_message_tool_result_failure_carries_is_error_true() {
        let msg = LlmMessage::tool_result("tc1", "Read", "Error: not found", true);
        match &msg.content[0] {
            ContentBlock::ToolResult { is_error, .. } => {
                assert!(*is_error, "failure path must set is_error=true");
            }
            _ => panic!("expected ToolResult block"),
        }
    }

    // ===================== PR 1: image dimensions tests =====================

    /// `high_detail_tokens` matches OpenAI's `high` detail tile math
    /// (`ceil(width * height / 750)`):
    /// - 512×512 → ceil(262144/750) = 350
    /// - 1024×1024 → ceil(1048576/750) = 1398
    /// - 2048×2048 → ceil(4194304/750) = 5593
    /// - 1×1 → 1 (rounded up)
    #[test]
    fn test_image_dimensions_high_detail_tokens() {
        assert_eq!(
            ImageDimensions {
                width: 512,
                height: 512
            }
            .high_detail_tokens(),
            350
        );
        assert_eq!(
            ImageDimensions {
                width: 1024,
                height: 1024
            }
            .high_detail_tokens(),
            1399
        );
        assert_eq!(
            ImageDimensions {
                width: 2048,
                height: 2048
            }
            .high_detail_tokens(),
            5593
        );
        // Edge: 1x1 image still costs 1 token (ceil(1/750))
        assert_eq!(
            ImageDimensions {
                width: 1,
                height: 1
            }
            .high_detail_tokens(),
            1
        );
    }

    /// Round-trip: `ImageSource` with `dimensions` serialises with
    /// the field present and deserialises back to the same struct.
    #[test]
    fn test_image_source_dimensions_roundtrip() {
        let src = ImageSource::Base64 {
            data: "AAAA".to_string(),
            dimensions: Some(ImageDimensions {
                width: 800,
                height: 600,
            }),
        };
        let json = serde_json::to_value(&src).unwrap();
        assert_eq!(json["source_type"], "base64");
        assert_eq!(json["data"], "AAAA");
        assert_eq!(json["dimensions"]["width"], 800);
        assert_eq!(json["dimensions"]["height"], 600);
        let parsed: ImageSource = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, src);
    }

    /// Backwards-compat: pre-PR-1 JSONL without `dimensions`
    /// deserialises to `dimensions: None`. Verifies the serde-default
    /// on both variants.
    #[test]
    fn test_image_source_dimensions_legacy_loads_as_none() {
        let legacy = serde_json::json!({"source_type": "base64", "data": "AAAA"});
        let parsed: ImageSource = serde_json::from_value(legacy).unwrap();
        match parsed {
            ImageSource::Base64 { data, dimensions } => {
                assert_eq!(data, "AAAA");
                assert_eq!(dimensions, None);
            }
            _ => panic!("expected Base64 variant"),
        }

        let legacy_url = serde_json::json!({"source_type": "url", "url": "https://x"});
        let parsed_url: ImageSource = serde_json::from_value(legacy_url).unwrap();
        match parsed_url {
            ImageSource::Url { url, dimensions } => {
                assert_eq!(url, "https://x");
                assert_eq!(dimensions, None);
            }
            _ => panic!("expected Url variant"),
        }
    }

    /// When `dimensions: None`, the on-disk JSONL shape omits the
    /// `dimensions` key entirely (skip_serializing_if). Pre-PR-1
    /// readers see no schema change.
    #[test]
    fn test_image_source_no_dimensions_omits_key() {
        let src = ImageSource::Url {
            url: "https://example.com/x.png".to_string(),
            dimensions: None,
        };
        let json = serde_json::to_value(&src).unwrap();
        assert!(
            json.as_object().unwrap().get("dimensions").is_none(),
            "None dimensions should be skipped from serialisation"
        );
    }

    /// `extract_dimensions_from_base64` reads width/height from the
    /// PNG IHDR chunk. 1024×1024 is the canonical OpenAI "high detail"
    /// image size and should round-trip cleanly.
    #[test]
    fn test_extract_dimensions_from_png_1024() {
        // Hand-built 24-byte header: PNG signature + 8-byte IHDR prefix
        // (length + "IHDR" tag) + width + height. IHDR's first 8 bytes
        // follow the signature; we synthesise the prefix as zeros.
        let mut bytes = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        bytes.extend([0u8; 8]); // IHDR length + tag placeholder
        bytes.extend(1024u32.to_be_bytes());
        bytes.extend(1024u32.to_be_bytes());
        let dims = extract_dimensions_from_base64(&bytes, "image/png").unwrap();
        assert_eq!(dims.width, 1024);
        assert_eq!(dims.height, 1024);
        assert_eq!(
            dims.high_detail_tokens(),
            ((1024 * 1024 + 749) / 750) as usize
        );
    }

    /// Non-PNG mime types fall through to `None` (JPEG extraction is
    /// deferred to a follow-up — SOFn marker parsing is non-trivial).
    #[test]
    fn test_extract_dimensions_non_png_returns_none() {
        let bytes = vec![0u8; 32];
        assert!(extract_dimensions_from_base64(&bytes, "image/jpeg").is_none());
        assert!(extract_dimensions_from_base64(&bytes, "image/webp").is_none());
    }

    /// Bytes shorter than the PNG signature + IHDR payload are rejected.
    #[test]
    fn test_extract_dimensions_short_png_returns_none() {
        let bytes = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]; // 8 bytes
        assert!(extract_dimensions_from_base64(&bytes, "image/png").is_none());
    }

    /// PNG signature mismatch (truncated JPEG bytes labeled PNG)
    /// returns `None` rather than panic.
    #[test]
    fn test_extract_dimensions_bad_signature_returns_none() {
        let bytes = vec![0u8; 32];
        assert!(extract_dimensions_from_base64(&bytes, "image/png").is_none());
    }

    /// Zero-dimension PNG (corrupt IHDR) returns `None` rather than
    /// producing a 0×0 ImageDimensions that would zero the token count.
    #[test]
    fn test_extract_dimensions_zero_dimensions_returns_none() {
        let mut bytes = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        bytes.extend([0u8; 8]);
        bytes.extend(0u32.to_be_bytes());
        bytes.extend(0u32.to_be_bytes());
        assert!(extract_dimensions_from_base64(&bytes, "image/png").is_none());
    }

    // ===================== PR 2: tool result rewrite round-trip =====================

    /// PR 2: `ContentBlock::ToolResult` round-trips through serde
    /// after the rewriter replaces the body with a single Text
    /// sentinel. The output JSONL shape is what
    /// `peko_session::jsonl::text_content()` flattens; verify the
    /// nested Text block carries the full sentinel verbatim so a
    /// `peko session show` can surface "this result was truncated"
    /// without a separate metadata channel.
    #[test]
    fn test_rewritten_tool_result_round_trips() {
        let original = ContentBlock::ToolResult {
            tool_call_id: "tc1".to_string(),
            name: "Read".to_string(),
            content: vec![ContentBlock::Text {
                text: "[truncated by peko_runtime: tool result for call tc1 \
                       was 50000 tokens, reduced to sentinel. Re-invoke the \
                       tool with a narrower scope to get the full result.]"
                    .to_string(),
            }],
            is_error: false,
        };
        let json = serde_json::to_value(&original).unwrap();
        // serde tag = "type", variant = "tool_result"
        assert_eq!(json["type"], "tool_result");
        assert_eq!(json["tool_call_id"], "tc1");
        assert_eq!(json["name"], "Read");
        assert_eq!(json["is_error"], false);
        assert!(json["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("[truncated by peko_runtime:"),);

        let parsed: ContentBlock = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, original);
    }

    /// PR 2: the `is_error: true → false` flip survives serde so
    /// pre-existing JSONL readers see the truncated body as a
    /// non-error result.
    #[test]
    fn test_tool_result_is_error_flip_round_trips() {
        let original = ContentBlock::ToolResult {
            tool_call_id: "tc1".to_string(),
            name: "Bash".to_string(),
            content: vec![ContentBlock::Text {
                text: "[truncated by peko_runtime: ...]".to_string(),
            }],
            is_error: false,
        };
        let json = serde_json::to_string(&original).unwrap();
        let parsed: ContentBlock = serde_json::from_str(&json).unwrap();
        let ContentBlock::ToolResult { is_error, .. } = &parsed else {
            panic!("expected ToolResult")
        };
        assert!(!*is_error);
    }
}
