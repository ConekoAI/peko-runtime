//! `ModelSpec` — model capability metadata shared by templates, the
//! on-disk catalog, and the IPC layer.
//!
//! PR 1 of `feature/model-first-config`. The goal is to replace ad-hoc
//! inference-side branching (`if api_format == anthropic && model_id
//! contains "claude-sonnet"`) with a small declarative descriptor that
//! every surface — engine, chat UI, CLI gallery, desktop gallery —
//! reads from. Engine gating lives in PR 2; the desktop gallery rework
//! is PR 4.
//!
//! Why not call this `Capabilities`? `Capabilities` already names the
//! principal capability system (different concept). `ModelSpec` keeps
//! the namespace disjoint.
//!
//! Every field on `ModelSpec` (and every nested type) is `serde-
//! default`, so an older `models.toml` written before this PR still
//! loads. New templates and new entries opt into richer metadata by
//! populating the relevant field.

use serde::{Deserialize, Serialize};

/// What kind of extended thinking the model supports.
///
/// `Disabled` covers models without any reasoning capability (most
/// chat models). `Required` covers the o-series / Claude 4.5+ default
/// where thinking always happens. `Optional` covers models where the
/// caller decides (most reasoning-capable models). `CustomBudget`
/// flags models that accept a caller-supplied token budget (Anthropic
/// `thinking.budget_tokens`, OpenAI `reasoning.effort` with a
/// numeric form).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingMode {
    /// Model has no thinking capability. The chat UI must hide the
    /// thinking toggle.
    #[default]
    Disabled,
    /// Caller may opt in via `ChatOptions::thinking_effort`. The
    /// chat UI shows the toggle but defaults it off.
    Optional,
    /// Model always thinks; the toggle is forced on. The chat UI
    /// shows the toggle locked-on.
    Required,
    /// Caller controls a numeric budget (Anthropic `budget_tokens`,
    /// OpenAI Responses `reasoning.max_tokens`). The chat UI shows
    /// a slider instead of a 3-state selector.
    CustomBudget,
}

/// Tool-calling support level.
///
/// `None` — the model does not understand tools; the engine must
/// refuse to register any. `FunctionCalling` — the standard
/// function-calling interface, sufficient for `peko`'s own tools.
/// `Full` — function calling plus server-side tools (web search,
/// code execution); out of scope for v1 engine work but reserved
/// for the future.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolSupport {
    #[default]
    None,
    FunctionCalling,
    Full,
}

/// Pricing hint. Optional on every entry — providers that don't
/// advertise public pricing, plus local / self-hosted models, just
/// leave it `None` and the gallery shows "—".
///
/// Stored as USD per 1M tokens (the unit the industry converged on).
/// Conversion to other currencies is the desktop's problem.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct PricingHint {
    /// USD per 1M input tokens. `None` = not advertised.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_per_million: Option<f64>,
    /// USD per 1M output tokens. `None` = not advertised.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_per_million: Option<f64>,
}

/// Declarative model capability descriptor.
///
/// Reads on every surface: engine (gates vision, audio, tools,
/// streaming, json_mode, thinking); chat UI (toggles); CLI gallery
/// (`peko model compare`); desktop gallery (badges). The same struct
/// flows template → catalog → IPC, so a single source of truth
/// drives them all.
///
/// All fields are optional / default-friendly. `from_template` on
/// `ModelConfig` populates them from the template when available;
/// templates that haven't been audited yet keep `ModelSpec::default()`
/// (text-only, no tools, no thinking, no streaming override) which
/// is conservative — better to hide a feature than ship a broken one.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct ModelSpec {
    /// Whether the model accepts image inputs. Drives the chat UI
    /// attachment picker and the engine's image-block gate.
    #[serde(default)]
    pub image_input: bool,
    /// Whether the model accepts audio inputs (input-side only;
    /// output speech is out of scope). Reserved for future voice
    /// providers; defaults off everywhere for now.
    #[serde(default)]
    pub audio_input: bool,
    /// Tool-calling support level. Engine refuses to surface
    /// `None` models in tool-using contexts (orchestrator,
    /// subagents).
    #[serde(default)]
    pub tool_support: ToolSupport,
    /// Whether the model supports server-sent-event streaming.
    /// Defaults true because every modern model does; explicit
    /// `false` is reserved for local / batch endpoints that
    /// only return a single JSON blob.
    #[serde(default = "default_streaming_true")]
    pub streaming: bool,
    /// Extended thinking behavior. `Disabled` hides the toggle
    /// entirely; the others gate the toggle visibility and
    /// default-on state.
    #[serde(default)]
    pub thinking: ThinkingMode,
    /// Whether the model supports `response_format: { type:
    /// "json_object" }` (or Anthropic's structured-output path).
    /// Drives the chat UI "JSON output" toggle.
    #[serde(default)]
    pub json_mode: bool,
    /// Pricing hint for the gallery. Always optional — providers
    /// that don't publish rates leave it `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing: Option<PricingHint>,
}

fn default_streaming_true() -> bool {
    true
}

// Manual Default so `streaming` defaults to `true` (the derive
// would default it to `false`, which would silently hide the SSE
// streaming button on every model). Also lets `ModelSpec::default()`
// be callable from a const context (the `BUILT_IN_TEMPLATES` table
// is a `const`).
impl Default for ModelSpec {
    fn default() -> Self {
        Self::text_only()
    }
}

impl ModelSpec {
    /// Convenience: text-only model with no tools, no thinking,
    /// streaming on. Matches a conservative pre-Feature template
    /// entry so backfill defaults are non-broken.
    #[must_use]
    pub const fn text_only() -> Self {
        Self {
            image_input: false,
            audio_input: false,
            tool_support: ToolSupport::None,
            streaming: true,
            thinking: ThinkingMode::Disabled,
            json_mode: false,
            pricing: None,
        }
    }

    /// Convenience: vision-capable, tools, streaming, JSON mode on,
    /// thinking optional. The standard "frontier chat model" profile
    /// used by most Claude / GPT-4.x entries.
    #[must_use]
    pub const fn frontier_chat() -> Self {
        Self {
            image_input: true,
            audio_input: false,
            tool_support: ToolSupport::FunctionCalling,
            streaming: true,
            thinking: ThinkingMode::Optional,
            json_mode: true,
            pricing: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_spec_is_text_only_with_streaming() {
        let s = ModelSpec::default();
        assert!(!s.image_input);
        assert!(!s.audio_input);
        assert_eq!(s.tool_support, ToolSupport::None);
        assert!(s.streaming);
        assert_eq!(s.thinking, ThinkingMode::Disabled);
        assert!(!s.json_mode);
        assert!(s.pricing.is_none());
    }

    #[test]
    fn text_only_helper_matches_default() {
        assert_eq!(ModelSpec::text_only(), ModelSpec::default());
    }

    #[test]
    fn frontier_chat_enables_vision_tools_json_thinking_optional() {
        let s = ModelSpec::frontier_chat();
        assert!(s.image_input);
        assert!(!s.audio_input);
        assert_eq!(s.tool_support, ToolSupport::FunctionCalling);
        assert!(s.streaming);
        assert_eq!(s.thinking, ThinkingMode::Optional);
        assert!(s.json_mode);
    }

    #[test]
    fn spec_roundtrips_via_json() {
        let s = ModelSpec {
            image_input: true,
            tool_support: ToolSupport::Full,
            thinking: ThinkingMode::CustomBudget,
            json_mode: true,
            pricing: Some(PricingHint {
                input_per_million: Some(3.0),
                output_per_million: Some(15.0),
            }),
            ..ModelSpec::default()
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: ModelSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn empty_object_deserializes_to_default_spec() {
        let back: ModelSpec = serde_json::from_str("{}").unwrap();
        assert_eq!(back, ModelSpec::default());
    }

    #[test]
    fn thinking_mode_accepts_snake_case_only() {
        // Confirms the wire format is locked to snake_case so the
        // desktop / CLI never see a PascalCase variant.
        for (input, expected) in [
            ("\"disabled\"", ThinkingMode::Disabled),
            ("\"optional\"", ThinkingMode::Optional),
            ("\"required\"", ThinkingMode::Required),
            ("\"custom_budget\"", ThinkingMode::CustomBudget),
        ] {
            let parsed: ThinkingMode = serde_json::from_str(input).unwrap();
            assert_eq!(parsed, expected);
        }
        assert!(serde_json::from_str::<ThinkingMode>("\"Disabled\"").is_err());
    }

    #[test]
    fn tool_support_accepts_snake_case_only() {
        for (input, expected) in [
            ("\"none\"", ToolSupport::None),
            ("\"function_calling\"", ToolSupport::FunctionCalling),
            ("\"full\"", ToolSupport::Full),
        ] {
            let parsed: ToolSupport = serde_json::from_str(input).unwrap();
            assert_eq!(parsed, expected);
        }
        assert!(serde_json::from_str::<ToolSupport>("\"FunctionCalling\"").is_err());
    }

    #[test]
    fn pricing_hint_omits_nones() {
        let p = PricingHint::default();
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(json, "{}");
    }
}
