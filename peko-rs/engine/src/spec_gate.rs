//! `SpecGate` — engine-level enforcement of the declarative
//! [`ModelSpec`] (PR 2 of `feature/model-first-config`).
//!
//! PR 1 wired `ModelSpec` from the catalog through to
//! `ProviderView::spec()`. PR 2 makes the engine *use* that
//! descriptor: every outgoing LLM call is checked against the spec
//! before it's dispatched. If a request would hit a capability the
//! model does not declare, the gate refuses with a structured
//! [`SpecGateError`] — never silently dropping the request and
//! never pretending the model can handle it.
//!
//! The design rule the user picked at planning time was
//! "hard-refuse with structured error", not "soft-fail" and not
//! "pass-through-and-hope-the-provider-handles-it". A chat UI
//! uploading an image to a text-only model needs to know the
//! reason for the failure, not a 400 from the provider with a
//! confusing error shape. Structured refusal keeps the cause
//! machine-readable so the desktop gallery can render a precise
//! hint ("claude-3-5-haiku doesn't accept images") and the CLI
//! can suggest the closest vision-capable alternative.
//!
//! The gate is intentionally narrow. It checks the four signals a
//! request can carry:
//!
//! - **multimodal content blocks** in `messages` (image today;
//!   audio is reserved for F28+ when `ContentBlock::Audio` lands)
//! - **tool definitions** (non-empty `tool_defs` ⇒ tools required)
//! - **reasoning-effort requests** (`options.thinking_effort !=
//!   ThinkingEffort::None` ⇒ model must declare non-Disabled
//!   thinking)
//!
//! It does NOT enforce `streaming` or `json_mode` — neither is
//! plumbed to a per-call caller signal in `ChatOptions` today.
//! When F26+ exposes `request_json`, the gate can extend with a
//! third axis without API churn.
//!
//! When `provider.spec()` returns `None` (catalog entries written
//! before PR 1), the gate is a no-op. Pre-PR-2 setups keep working
//! unchanged. This is the same pre-PR-1 / pre-spec-gate behavior
//! callers relied on; new spec-bearing entries opt into the gate
//! by virtue of having populated the field.

use peko_message::{ContentBlock, LlmMessage};
use peko_provider_api::{ChatOptions, ToolDefinition};
use peko_providers::spec::{ModelSpec, ThinkingMode, ToolSupport};
use thiserror::Error;

/// Structured refusal for a request that would hit a capability
/// the bound model does not declare. Carries the specific
/// capability and a message suitable for both logs and
/// user-facing CLI/desktop UI.
///
/// `SpecGateError` is intentionally NOT an `AgenticError` variant
/// — the gate sits at the LLM-call boundary inside
/// `stream_with_eviction`, not at the public loop boundary, so the
/// public type stays focused on quota / max-iterations / retry
/// signals. The caller maps `SpecGateError` to
/// `AgenticError::SpecViolation` (added in PR 2) before bubbling
/// it up.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum SpecGateError {
    /// The request contains an image `ContentBlock` but the bound
    /// model's spec does not declare `image_input: true`.
    #[error(
        "model {model_id} ({provider}) does not accept image inputs \
         (spec.image_input == false); pick a vision-capable model or \
         strip the image attachment before sending"
    )]
    ImageInputUnsupported {
        /// Bound model id (e.g. `claude-3-5-haiku-latest`).
        model_id: String,
        /// Adapter name (e.g. `anthropic`).
        provider: String,
    },

    /// The request contains an audio `ContentBlock` but the bound
    /// model's spec does not declare `audio_input: true`. Audio
    /// input is reserved (no `ContentBlock::Audio` variant exists
    /// yet); this variant is reachable once audio support lands
    /// so we don't need to revisit the gate.
    #[error(
        "model {model_id} ({provider}) does not accept audio inputs \
         (spec.audio_input == false)"
    )]
    AudioInputUnsupported {
        model_id: String,
        provider: String,
    },

    /// The request supplies at least one tool definition but the
    /// bound model's spec declares `tool_support == None`.
    #[error(
        "model {model_id} ({provider}) does not support tool calling \
         (spec.tool_support == none); pick a function-calling model or \
         disable tool dispatch for this conversation"
    )]
    ToolsUnsupported {
        model_id: String,
        provider: String,
    },

    /// The caller asked for reasoning
    /// (`thinking_effort != ThinkingEffort::None`) but the bound
    /// model's spec declares `thinking == Disabled`.
    #[error(
        "model {model_id} ({provider}) does not support reasoning \
         (spec.thinking == disabled); pick a reasoning-capable model \
         or disable the thinking toggle"
    )]
    ThinkingUnsupported {
        model_id: String,
        provider: String,
    },
}

impl SpecGateError {
    /// Short machine-readable tag (`"image"`, `"audio"`, `"tools"`,
    /// `"thinking"`). Useful for telemetry classification and
    /// tests that want to assert on the failure shape without
    /// matching `Display`.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::ImageInputUnsupported { .. } => "image",
            Self::AudioInputUnsupported { .. } => "audio",
            Self::ToolsUnsupported { .. } => "tools",
            Self::ThinkingUnsupported { .. } => "thinking",
        }
    }
}

/// Check whether a request is permitted by the bound model's
/// `ModelSpec`.
///
/// Returns `Ok(())` if either:
///
/// - `spec` is `None` (catalog entry written before PR 1; no gate
///   is installed and the pre-PR-2 behavior is preserved), or
/// - every signal in the request matches a declared capability.
///
/// Returns `Err(SpecGateError)` for the first violation found.
/// Order is stable (`image → audio → tools → thinking`) so tests
/// can rely on a single failure shape per fixture.
pub fn check(
    spec: Option<ModelSpec>,
    model_id: &str,
    provider: &str,
    messages: &[LlmMessage],
    tool_defs: &[ToolDefinition],
    options: &ChatOptions,
) -> Result<(), SpecGateError> {
    let Some(spec) = spec else {
        // Pre-PR-1 catalog entry: no spec, no gate. This is the
        // intentional compat path — the gate is opt-in by way of
        // the catalog author populating `ModelSpec`.
        return Ok(());
    };

    if has_image_block(messages) && !spec.image_input {
        return Err(SpecGateError::ImageInputUnsupported {
            model_id: model_id.to_string(),
            provider: provider.to_string(),
        });
    }

    if has_audio_block(messages) && !spec.audio_input {
        return Err(SpecGateError::AudioInputUnsupported {
            model_id: model_id.to_string(),
            provider: provider.to_string(),
        });
    }

    if !tool_defs.is_empty() && spec.tool_support == ToolSupport::None {
        return Err(SpecGateError::ToolsUnsupported {
            model_id: model_id.to_string(),
            provider: provider.to_string(),
        });
    }

    if options.thinking_effort.is_enabled() && spec.thinking == ThinkingMode::Disabled {
        return Err(SpecGateError::ThinkingUnsupported {
            model_id: model_id.to_string(),
            provider: provider.to_string(),
        });
    }

    Ok(())
}

fn has_image_block(messages: &[LlmMessage]) -> bool {
    messages
        .iter()
        .flat_map(|m| m.content.iter())
        .any(|b| matches!(b, ContentBlock::Image { .. }))
}

/// Reserved for the future `ContentBlock::Audio` variant. Today
/// the type has no audio arm, so the helper is a constant `false`.
/// Keeping it as a function (rather than inlining the `false`)
/// means the gate doesn't need to be touched when audio lands —
/// only this helper changes.
fn has_audio_block(_messages: &[LlmMessage]) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use peko_message::{ContentBlock, ImageSource, LlmMessage, MessageRole};
    use peko_provider_api::{ChatOptions, ToolDefinition, ThinkingEffort};

    fn text_message(text: &str) -> LlmMessage {
        LlmMessage {
            role: MessageRole::User,
            content: vec![ContentBlock::Text {
                text: text.to_string(),
            }],
            ..LlmMessage::default()
        }
    }

    fn image_message() -> LlmMessage {
        LlmMessage {
            role: MessageRole::User,
            content: vec![ContentBlock::Image {
                source: ImageSource::Base64 {
                    data: "aGVsbG8=".to_string(),
                    dimensions: None,
                },
                mime_type: "image/png".to_string(),
            }],
            ..LlmMessage::default()
        }
    }

    fn tool_def(name: &str) -> ToolDefinition {
        ToolDefinition {
            name: name.to_string(),
            description: format!("{name} test tool"),
            parameters: serde_json::json!({"type": "object"}),
        }
    }

    fn default_options() -> ChatOptions {
        ChatOptions::default()
    }

    #[test]
    fn none_spec_is_no_op() {
        // Pre-PR-1 entries skip the gate entirely. Even a
        // vision-requesting, tool-using, reasoning-asking payload
        // against `spec: None` is permitted.
        let messages = vec![image_message()];
        let tools = vec![tool_def("noop")];
        let options = ChatOptions {
            thinking_effort: ThinkingEffort::High,
            ..default_options()
        };
        let result = check(None, "any-model", "any-provider", &messages, &tools, &options);
        assert!(result.is_ok(), "no spec must be a no-op gate: {result:?}");
    }

    #[test]
    fn text_only_request_passes_text_only_spec() {
        let spec = ModelSpec::text_only();
        let messages = vec![text_message("hello")];
        let tools: Vec<ToolDefinition> = vec![];
        let options = default_options();
        let result = check(
            Some(spec),
            "claude-3-5-haiku-latest",
            "anthropic",
            &messages,
            &tools,
            &options,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn image_block_on_text_only_spec_refuses() {
        let spec = ModelSpec::text_only();
        let messages = vec![image_message()];
        let tools: Vec<ToolDefinition> = vec![];
        let options = default_options();
        let err = check(
            Some(spec),
            "claude-3-5-haiku-latest",
            "anthropic",
            &messages,
            &tools,
            &options,
        )
        .unwrap_err();
        assert_eq!(err.kind(), "image");
        assert!(matches!(err, SpecGateError::ImageInputUnsupported { .. }));
    }

    #[test]
    fn image_block_on_vision_spec_passes() {
        let spec = ModelSpec::frontier_chat(); // image_input: true
        let messages = vec![image_message(), text_message("describe this")];
        let tools: Vec<ToolDefinition> = vec![];
        let options = default_options();
        let result = check(
            Some(spec),
            "claude-sonnet-4-5",
            "anthropic",
            &messages,
            &tools,
            &options,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn tools_on_none_tool_support_spec_refuses() {
        let spec = ModelSpec::text_only(); // tool_support: None
        let messages = vec![text_message("hi")];
        let tools = vec![tool_def("read_file")];
        let options = default_options();
        let err = check(
            Some(spec),
            "claude-3-5-haiku-latest",
            "anthropic",
            &messages,
            &tools,
            &options,
        )
        .unwrap_err();
        assert_eq!(err.kind(), "tools");
        assert!(matches!(err, SpecGateError::ToolsUnsupported { .. }));
    }

    #[test]
    fn tools_on_function_calling_spec_passes() {
        let spec = ModelSpec::frontier_chat(); // tool_support: FunctionCalling
        let messages = vec![text_message("hi")];
        let tools = vec![tool_def("read_file")];
        let options = default_options();
        let result = check(
            Some(spec),
            "claude-sonnet-4-5",
            "anthropic",
            &messages,
            &tools,
            &options,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn thinking_request_on_disabled_spec_refuses() {
        let spec = ModelSpec::text_only(); // thinking: Disabled
        let messages = vec![text_message("hi")];
        let tools: Vec<ToolDefinition> = vec![];
        let options = ChatOptions {
            thinking_effort: ThinkingEffort::High,
            ..default_options()
        };
        let err = check(
            Some(spec),
            "claude-3-5-haiku-latest",
            "anthropic",
            &messages,
            &tools,
            &options,
        )
        .unwrap_err();
        assert_eq!(err.kind(), "thinking");
        assert!(matches!(err, SpecGateError::ThinkingUnsupported { .. }));
    }

    #[test]
    fn thinking_none_on_disabled_spec_passes() {
        // thinking_effort == None means "don't request reasoning";
        // the gate does not require the model to support thinking
        // when the caller didn't ask for any. This is the F25+
        // default for every pre-existing ChatOptions call site.
        let spec = ModelSpec::text_only();
        let options = ChatOptions {
            thinking_effort: ThinkingEffort::None,
            ..default_options()
        };
        let messages = vec![text_message("hi")];
        let tools: Vec<ToolDefinition> = vec![];
        let result = check(
            Some(spec),
            "claude-3-5-haiku-latest",
            "anthropic",
            &messages,
            &tools,
            &options,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn thinking_request_on_optional_spec_passes() {
        let spec = ModelSpec::frontier_chat(); // thinking: Optional
        let options = ChatOptions {
            thinking_effort: ThinkingEffort::Medium,
            ..default_options()
        };
        let messages = vec![text_message("hi")];
        let tools: Vec<ToolDefinition> = vec![];
        let result = check(
            Some(spec),
            "claude-sonnet-4-5",
            "anthropic",
            &messages,
            &tools,
            &options,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn check_returns_first_violation_in_stable_order() {
        // Both image AND tools are disallowed. The gate should
        // report the image violation first (stable ordering).
        let spec = ModelSpec::text_only();
        let messages = vec![image_message()];
        let tools = vec![tool_def("read_file")];
        let options = default_options();
        let err = check(
            Some(spec),
            "claude-3-5-haiku-latest",
            "anthropic",
            &messages,
            &tools,
            &options,
        )
        .unwrap_err();
        assert_eq!(
            err.kind(),
            "image",
            "image must precede tools in the gate ordering"
        );
    }

    #[test]
    fn error_carries_model_id_and_provider_for_logging() {
        let spec = ModelSpec::text_only();
        let messages = vec![image_message()];
        let err = check(
            Some(spec),
            "my-custom-model",
            "my-custom-provider",
            &messages,
            &[],
            &default_options(),
        )
        .unwrap_err();
        let s = err.to_string();
        assert!(s.contains("my-custom-model"));
        assert!(s.contains("my-custom-provider"));
    }
}
