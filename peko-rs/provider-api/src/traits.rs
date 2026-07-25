//! Provider types
//!
//! Canonical type definitions for LLM provider interactions.
//! In the workspace these live in the `peko-provider-api` crate as
//! one cohesive contract; the root `peko` package re-exports them
//! through `crate::providers::traits::*` so existing import paths
//! keep resolving.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cache_retention::CacheRetention;

// Re-export the message-domain types that are part of the public
// provider surface so adapter modules can pull them all from
// `peko_provider_api::*` without an extra import.
pub use peko_message::{ContentBlock, LlmMessage, MessageRole, TokenUsage};

/// Unique content block ID for streaming correlation
pub type ContentBlockId = String;

/// Block type for streaming events
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockType {
    Text,
    ToolCall,
    Thinking,
}

/// Content delta for streaming
#[derive(Debug, Clone)]
pub enum ContentDelta {
    Text(String),
    ToolCall {
        name: Option<String>,
        arguments: Value,
    },
}

/// Tool definition for native tool calling
///
/// Providers translate this into their native tool schema format
/// (e.g., `OpenAI`'s function calling format, Anthropic's tool use)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    /// Tool name (must match the tool's registered name)
    pub name: String,
    /// Tool description for the model
    pub description: String,
    /// JSON Schema for tool parameters
    pub parameters: Value,
}

/// Streaming event from provider
///
/// Providers emit these events during streaming responses.
/// This allows the agent loop to handle incremental updates,
/// tool calls, and reasoning content.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// Stream started
    Start {
        /// Provider name
        provider: String,
        /// Model being used
        model: String,
    },
    /// Text content started
    TextStart {
        /// Index in the content array
        content_index: usize,
    },
    /// Text delta (incremental content)
    TextDelta {
        /// Index in the content array
        content_index: usize,
        /// Delta text
        delta: String,
    },
    /// Text content complete
    TextEnd {
        /// Index in the content array
        content_index: usize,
        /// Full text content
        content: String,
    },
    /// Thinking/reasoning started
    ThinkingStart {
        /// Index in the content array
        content_index: usize,
    },
    /// Thinking delta
    ThinkingDelta {
        /// Index in the content array
        content_index: usize,
        /// Delta thinking text
        delta: String,
    },
    /// Thinking complete
    ThinkingEnd {
        /// Index in the content array
        content_index: usize,
        /// Full thinking content
        content: String,
    },
    /// Tool call started
    ToolCallStart {
        /// Index in the content array
        content_index: usize,
    },
    /// Tool call delta (for streaming arguments)
    ToolCallDelta {
        /// Index in the content array
        content_index: usize,
        /// Delta (JSON fragment)
        delta: String,
    },
    /// Tool call complete
    ToolCallEnd {
        /// Index in the content array
        content_index: usize,
        /// Complete tool call
        tool_call: peko_message::ContentBlock,
    },
    /// Stream completed
    Done {
        /// Stop reason
        stop_reason: StopReason,
    },
    /// Token usage information (typically sent at end of stream)
    Usage {
        /// Input tokens (uncached, non-reasoning)
        input: u64,
        /// Output tokens (includes reasoning/thinking tokens, which the
        /// provider already folds into `completion_tokens` /
        /// `output_tokens`)
        output: u64,
        /// Total tokens (wire-reported `total_tokens` when present;
        /// otherwise `input + output`)
        total: u64,
        /// Tokens billed at cache-write rate (Anthropic only)
        cache_creation_input_tokens: u64,
        /// Tokens billed at cache-read rate (Anthropic + OpenAI)
        cache_read_input_tokens: u64,
        /// Reasoning tokens within `output` (OpenAI o-series)
        reasoning_output_tokens: u64,
    },
    /// Error occurred
    Error {
        /// Error message
        message: String,
    },
}

/// Why a response stopped
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// Normal completion
    Stop,
    /// Hit token limit
    Length,
    /// Tool use requested
    ToolUse,
    /// Error occurred
    Error,
    /// Aborted by user
    Aborted,
}

/// Reasoning-effort knob surfaced to callers (F25).
///
/// Each adapter maps this onto its provider-native field:
/// - OpenAI Chat Completions: `body["reasoning_effort"] = "low"|"medium"|"high"`
/// - OpenAI Responses:        `body["reasoning"] = {effort, summary}`
/// - Anthropic:               `thinking: {type:"adaptive"}` + `output_config`
///                            for Opus 4-6+ / Sonnet 5 / Fable 5+, otherwise
///                            `thinking: {type:"enabled", budget_tokens: N}`
///                            with an effort→budget mapping
///                            (low→1024, medium→4096, high→32_000).
///
/// `None` means "do not request reasoning" (default — most callers
/// won't have reasoning enabled). `Adaptive` is honored only on
/// adapters that detect adaptive-capable model ids; others fall
/// back to a `High` budget mapping.
/// `XHigh` and `Max` map to OpenAI's wire vocabulary (`"xhigh"`,
/// `"max"`) but are silently clamped on adapters that don't yet
/// support them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ThinkingEffort {
    #[default]
    None,
    Low,
    Medium,
    High,
    XHigh,
    Max,
    Adaptive,
}

/// Per-provider thinking-reasoning wire shape (F29).
///
/// Each variant maps `ChatOptions::thinking_effort` (F25) onto the
/// provider-native field the adapter emits on the request body.
/// `OpenAi`, `Anthropic`, and `OpenAiResponses` mirror the F25
/// adapter defaults — they are listed here so a single
/// `ProviderCompat` annotation can drive per-adapter emit even for
/// providers that already work today.
///
/// The specialty variants cover providers whose `extra_body.thinking`
/// shape is documented but unique (DeepSeek, Kimi, Qwen, Zai) plus
/// OpenRouter / Together whose reasoning namespace differs from
/// canonical OpenAI's. Wire shapes:
///
/// | Variant           | Wire field                                         |
/// |-------------------|----------------------------------------------------|
/// | `OpenAi`          | `body["reasoning_effort"] = "low\|medium\|high"`    |
/// | `Anthropic`       | `thinking: {type, budget_tokens}` (budget) or     |
/// |                   | `thinking: {type:"adaptive"}` + `output_config`    |
/// |                   | (adaptive on Opus 4-6+ / Sonnet 5 / Fable 5+)      |
/// | `OpenAiResponses` | `body["reasoning"] = {effort, summary}` +          |
/// |                   | `include: ["reasoning.encrypted_content"]`         |
/// | `Kimi`            | `extra_body.thinking = {type, effort, keep}` on    |
/// |                   | the Anthropic-shaped wire (kimi-code `anthropic.ts:392-462`) |
/// | `DeepSeek`        | `extra_body.thinking = {type:"enabled"}` +          |
/// |                   | canonical `reasoning_effort`                       |
/// | `Qwen`            | `extra_body.enable_thinking = bool` (toggle only)  |
/// | `Zai`             | `thinking: {type, clear_thinking}` — Anthropic-    |
/// |                   | compat; `clear_thinking: "20251015"` matches the   |
/// |                   | Anthropic `clear_thinking_20251015` pattern (F27)  |
/// | `OpenRouter`      | `reasoning: {effort: "low\|medium\|high"}`         |
/// | `Together`        | `reasoning: {enabled: bool}` (no effort levels)    |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ThinkingFormat {
    OpenAi,
    Anthropic,
    OpenAiResponses,
    Kimi,
    DeepSeek,
    Qwen,
    Zai,
    OpenRouter,
    Together,
}

/// Per-provider tool-deferral policy (F29).
///
/// Some OpenAI-compatible providers delay-emitting tool calls until
/// the model's reasoning block closes (Kimi notably). The engine
/// loop's tool-call accumulator respects this via
/// `ProviderCompat::deferred_tools_mode`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DeferredToolsMode {
    /// Fire tool calls as soon as the adapter surfaces them.
    #[default]
    Off,
    /// Kimi-specific deferral: `tool_call_id` and `arguments` may
    /// only land AFTER the trailing `reasoning` block closes. The
    /// accumulator should not surface a `ContentBlock::ToolCall`
    /// until `Done { stop_reason: ToolUse }` arrives.
    Kimi,
}

/// Per-provider adapter hints (F29).
///
/// `ModelConfig::compat: Option<ProviderCompat>` carries the hints
/// resolved from the template at catalog construction time;
/// `ChatOptions::compat_override` lets callers override per-call.
/// Both default to `None` so pre-F29 wire shapes (OpenAI
/// `reasoning_effort`, Anthropic `thinking: {type, budget_tokens}`)
/// stay unaffected — adapters fall back to their F25 built-in
/// defaults when compat is unset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCompat {
    pub thinking_format: ThinkingFormat,
    pub deferred_tools_mode: DeferredToolsMode,
}

impl ThinkingEffort {
    /// True when the caller asked for any reasoning field on the wire.
    /// `None` is the only "off" variant — every other value triggers
    /// the per-adapter reasoning emission.
    #[must_use]
    pub fn is_enabled(self) -> bool {
        !matches!(self, Self::None)
    }

    /// Wire-string for Chat Completions' `reasoning_effort` field.
    /// Returns `None` for `Self::None` (caller decides whether to
    /// emit the field at all) and `None` for `Adaptive` (Chat
    /// Completions has no adaptive mode — the caller should map it
    /// to `High` first if targeting Chat Completions).
    #[must_use]
    pub fn as_chat_completions_str(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::Low => Some("low"),
            Self::Medium => Some("medium"),
            Self::High => Some("high"),
            Self::XHigh => Some("xhigh"),
            Self::Max => Some("max"),
            Self::Adaptive => None,
        }
    }

    /// Effort → Anthropic `budget_tokens` (only honored for budget
    /// mode — adaptive mode ignores the integer). The mapping comes
    /// from codex-rs's `reasoning_effort_to_budget_tokens` table.
    #[must_use]
    pub fn to_anthropic_budget_tokens(self) -> u32 {
        match self {
            Self::None | Self::Adaptive => 0, // caller drops the field
            Self::Low => 1024,
            Self::Medium => 4096,
            Self::High => 32_000,
            Self::XHigh => 64_000,
            Self::Max => 128_000,
        }
    }
}

/// Tool-choice control surfaced to callers (F26).
///
/// Three of the four variants round-trip straight onto the wire;
///
/// * `Required`     → `"required"` everywhere — Responses / Chat
///                    Completions / Anthropic all share the same
///                    "must call a tool" semantic.
/// * `Forced(name)` → pick the named tool. Anthropic uses a
///                    different wire shape
///                    (`{type:"tool", name:"X"}`) than OpenAI
///                    (`{type:"function", function:{name:"X"}}`);
///                    each adapter emits its native form.
/// * `Auto` / `None` → "let the model decide" / "no tools, even if
///                    some are registered".
///
/// Anthropic's "must call any tool" wire value is `"any"` (not
/// `"required"`); `Required` is mapped at the adapter level. The
/// enum mirrors OpenAI's vocabulary so callers don't have to know
/// which provider they're targeting.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ToolChoice {
    #[default]
    Auto,
    /// Force NO tool call. On OpenAI this is `"none"`; on Anthropic
    /// it is also `"none"` (Anthropic's "no tool" sentinel).
    None,
    /// Require a tool call (any). Anthropic wire value is `"any"`.
    Required,
    /// Force a specific tool. Adapter picks the right native shape.
    Forced(String),
}

/// Service-tier control surfaced to callers (F26).
///
/// OpenAI Chat Completions and Responses both honor
/// `service_tier` on the request body. Anthropic has no equivalent;
/// the field is a no-op there. `None` means "do not emit the field"
/// (the OpenAI default, `auto`, is the server-side default when
/// absent). `Auto`, `Flex`, and `Priority` mirror OpenAI's
/// documented vocabulary; `Default` forces the literal `"default"`
/// per OpenAI's wire docs (used to opt out of `auto`-tier
/// behaviors).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ServiceTier {
    #[default]
    None,
    Default,
    Auto,
    Flex,
    Priority,
}

impl ServiceTier {
    /// Wire-string for `body["service_tier"]`. `None` (the default)
    /// returns `None` so the adapter can suppress emission entirely.
    #[must_use]
    pub fn as_wire_str(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::Default => Some("default"),
            Self::Auto => Some("auto"),
            Self::Flex => Some("flex"),
            Self::Priority => Some("priority"),
        }
    }
}

/// F27: how aggressively Anthropic should keep thinking blocks
/// across turns.
///
/// Anthropic introduced `context_management` (with
/// `clear_thinking_20251015`) to let callers strip thinking
/// content between turns so it doesn't pollute the next call's
/// prompt. `Off` (the default) emits no `context_management` block
/// — peko's pre-F27 behavior. `Turn` keeps only the last turn's
/// thinking; `All` keeps every turn. The Anthropic wire value for
/// `keep` is `"turn"` / `"all"`; `Off` collapses to no
/// `context_management` body field.
///
/// Mirrors kimi-code's `anthropic.ts:1234-1261`
/// `contextManagement` option.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ThinkingKeep {
    #[default]
    Off,
    Turn,
    All,
}

impl ThinkingKeep {
    /// Wire-string for the `keep` field of
    /// `clear_thinking_20251015`. `Off` returns `None` so callers
    /// can suppress emission of the entire `context_management`
    /// block when the caller doesn't opt in.
    #[must_use]
    pub fn as_wire_str(self) -> Option<&'static str> {
        match self {
            Self::Off => None,
            Self::Turn => Some("turn"),
            Self::All => Some("all"),
        }
    }
}

/// Options for chat completion
#[derive(Debug, Clone, Default)]
pub struct ChatOptions {
    /// Temperature (0.0 - 2.0)
    pub temperature: Option<f32>,
    /// Maximum tokens to generate
    pub max_tokens: Option<u32>,
    /// API key (optional - uses env var if not provided)
    pub api_key: Option<String>,
    /// Additional headers
    pub headers: std::collections::HashMap<String, String>,
    /// Prompt-cache retention policy (F23). `Default` lets the
    /// provider pick its own TTL; `Long` requests the longest TTL
    /// the provider supports; `None` disables cache markers and
    /// session-affinity fields entirely.
    pub cache_retention: CacheRetention,
    /// Stable session identifier used as the cache key. Anthropic
    /// adapters map this to `metadata.user_id`; OpenAI adapters map
    /// it to `prompt_cache_key`. When `None`, the caller relies on
    /// the provider's automatic prefix-detection only.
    pub prompt_cache_key: Option<String>,
    /// F25: reasoning-effort knob. Defaults to `None` (no reasoning
    /// on the wire) so the per-adapter request bodies stay byte-for-
    /// byte identical to the pre-F25 shape.
    pub thinking_effort: ThinkingEffort,
    /// F25: Responses-only. When `Some(b)`, emit
    /// `reasoning.summary = "auto"` when `b == true`; when `None`,
    /// suppress the summary key entirely. Chat Completions and
    /// Anthropic ignore this field.
    pub thinking_summary: Option<bool>,
    /// F25: Responses-only. When `true` (the default for callers
    /// that set a `thinking_effort`), emit
    /// `include: ["reasoning.encrypted_content"]` so the model
    /// returns an encrypted reasoning payload that the caller can
    /// pass back into `previous_response_id` chains. Set to `false`
    /// to suppress — useful for low-log retention profiles.
    pub encrypted_reasoning: bool,
    /// F26: tool-choice control. `Auto` (the default) preserves the
    /// pre-F26 wire shape (`"auto"` on OpenAI, `"auto"` on
    /// Anthropic). `Required` / `Forced` are how callers steer
    /// structured-output flows. Anthropic's `Forced` uses
    /// `{type:"tool", name}`; OpenAI uses
    /// `{type:"function", function:{name}}`.
    pub tool_choice: ToolChoice,
    /// F26: gate parallel tool calling. `Some(false)` forces
    /// serialized tool calls — the model emits one tool_use at a
    /// time. Anthropic has no parallel flag (parallel is the
    /// default there); the field is a no-op on the Anthropic
    /// adapter. `None` (the default) keeps each adapter's natural
    /// default: OpenAI emits no `parallel_tool_calls` key (server
    /// defaults to true on supported models).
    pub parallel_tool_calls: Option<bool>,
    /// F26: OpenAI service-tier selector. `None` (the default)
    /// suppresses emission so the server picks its standard tier.
    /// Anthropic ignores this field.
    pub service_tier: ServiceTier,
    /// F26: OpenAI Responses-only. Stable end-user identifier that
    /// OpenAI uses for abuse-detection across long-running flows.
    /// Hash the principal id before passing it in (per OpenAI's
    /// guidance). Other adapters ignore the field.
    pub safety_identifier: Option<String>,
    /// F27: Anthropic beta headers to opt into for this request.
    /// Joined with `,` and sent as the `anthropic-beta` request
    /// header on the Anthropic adapter. Other adapters ignore the
    /// field (OpenAI has no `betas` header). Empty (the default)
    /// suppresses emission so the pre-F27 wire shape is preserved.
    ///
    /// The Anthropic adapter already injects its own internal betas
    /// (e.g. `interleaved-thinking-2025-05-08` from F25) via the
    /// `extra_request_headers` trait method; caller-supplied betas
    /// here are concatenated onto that list.
    pub betas: Vec<String>,
    /// F27: when `true`, ALSO send the betas list as a body field
    /// (`body["betas"] = [...]`) on Anthropic requests in addition
    /// to the `anthropic-beta` header. Anthropic accepts both shapes
    /// (the body form is used by the official SDK's beta API); the
    /// header form is what every other tool emits. Most callers
    /// should leave this `false` (default) to avoid the wire-shape
    /// ambiguity. `false` (default) preserves the pre-F27 wire shape.
    pub beta_api: bool,
    /// F27: how aggressively to keep thinking blocks across turns
    /// on the Anthropic adapter. `Off` (default) emits no
    /// `context_management` block — pre-F27 behavior. `Turn` keeps
    /// only the last turn's thinking; `All` keeps every turn. When
    /// set, the Anthropic adapter also auto-adds the
    /// `context-management-2025-06-27` beta header. Other adapters
    /// ignore the field.
    pub thinking_keep: ThinkingKeep,
    /// F29: per-call override for the catalog-level
    /// `ModelConfig.compat` annotation. Resolved by the engine loop
    /// from a model id + cache, then plumbed in here per request.
    /// `None` (the default) means "use the model's catalog compat,
    /// falling back to the adapter's built-in F25 default". When
    /// the field is `Some`, the adapter projects
    /// `ChatOptions::thinking_effort` onto the wire shape named by
    /// `compat.thinking_format` (DeepSeek / Kimi / OpenRouter /
    /// Together / Qwen / Zai). Pre-F29 callers (compat_override =
    /// `None`) keep the F25 default wire shape byte-for-byte.
    pub compat_override: Option<ProviderCompat>,
}

/// Response from chat completion
#[derive(Debug, Clone)]
pub struct ChatResponse {
    /// Message content blocks
    pub content: Vec<ContentBlock>,
    /// Tool calls (if any)
    pub tool_calls: Vec<ContentBlock>,
    /// Stop reason
    pub stop_reason: StopReason,
    /// Token usage
    pub usage: TokenUsage,
    /// Provider name
    pub provider: String,
    /// Model used
    pub model: String,
}
