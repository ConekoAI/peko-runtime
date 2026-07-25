//! Preset provider templates.
//!
//! These are the canonical "starter kits" the runtime offers via
//! `peko provider add --template <id>`. Each template describes a
//! known provider with its default base URL, API format, curated model
//! list, known context lengths, and any required headers.
//!
//! Templates are **not** runtime state. They are compiled-in
//! `&'static` data; running the runtime with no users still has
//! templates available. Users instantiate a `ProviderCatalogEntry`
//! from a template via `peko provider add --template <id>`, at which
//! point the entry is owned by the user and can be edited freely.
//!
//! ## When to add a template vs. a custom provider
//!
//! - **Template**: when a provider has stable, widely-known URL, format,
//!   and model IDs worth curating (Anthropic, OpenAI, Groq, Together,
//!   Ollama, …).
//! - **Custom** (`peko provider add --custom`): for one-off or
//!   self-hosted endpoints (a llama.cpp server, an internal proxy, a
//!   new vendor before we've blessed it as a template).
//!
//! ## Adding new templates
//!
//! Append to `BUILT_IN_TEMPLATES` with a unique lowercase id, a
//! non-empty model list, and a `default_model` that references one of
//! those models. Add a unit test asserting the template is findable.

use crate::catalog::ApiFormat;
use peko_provider_api::ProviderCompat;

/// One model declared by a provider template.
#[derive(Debug, Clone, Copy)]
pub struct ModelTemplate {
    /// Model id as it appears on the wire.
    pub id: &'static str,
    /// Human-readable display name (optional).
    pub display_name: Option<&'static str>,
    /// Maximum context length in tokens.
    pub context_length: Option<u32>,
    /// Maximum output tokens for a single response.
    pub max_output_tokens: Option<u32>,
}

/// One preset provider template.
#[derive(Debug, Clone, Copy)]
pub struct ProviderTemplate {
    /// Canonical lowercase provider id.
    pub id: &'static str,
    /// Human-readable display name.
    pub display_name: &'static str,
    /// Wire format.
    pub api_format: ApiFormat,
    /// Base URL for the API.
    pub base_url: &'static str,
    /// Whether an API key is required.
    pub requires_key: bool,
    /// Curated model list.
    pub models: &'static [ModelTemplate],
    /// Default model id (must reference one of `models`).
    pub default_model: &'static str,
    /// Optional extra HTTP headers.
    pub headers: &'static [(&'static str, &'static str)],
    /// F29: per-provider adapter hints. `None` keeps the F25 / F26 /
    /// F27 defaults (OpenAI `reasoning_effort`, Anthropic `thinking:
    /// {type, budget_tokens}`, Responses `reasoning: {effort,
    /// summary}`). When set, the OpenAiCompatibleAdapter projects
    /// `ChatOptions::thinking_effort` onto the wire shape named by
    /// `compat.thinking_format` (DeepSeek / Kimi / OpenRouter /
    /// Together / Qwen / Zai).
    pub compat: Option<ProviderCompat>,
}

/// Built-in provider templates. Add new templates at the end; never
/// reorder or remove existing entries — registry and CLI users may
/// reference the template id by name.
pub const BUILT_IN_TEMPLATES: &[ProviderTemplate] = &[
    // ── Native OpenAI ─────────────────────────────────────────────────
    ProviderTemplate {
        id: "openai",
        display_name: "OpenAI",
        api_format: ApiFormat::OpenaiCompletions,
        base_url: "https://api.openai.com/v1",
        requires_key: true,
        models: &[
            ModelTemplate {
                id: "gpt-4o",
                display_name: Some("GPT-4o"),
                context_length: Some(128_000),
                max_output_tokens: Some(16_384),
            },
            ModelTemplate {
                id: "gpt-4o-mini",
                display_name: Some("GPT-4o mini"),
                context_length: Some(128_000),
                max_output_tokens: Some(16_384),
            },
            ModelTemplate {
                id: "o1",
                display_name: Some("o1"),
                context_length: Some(200_000),
                max_output_tokens: Some(100_000),
            },
        ],
        default_model: "gpt-4o-mini",
        headers: &[],
        compat: None,
    },
    // ── Native Anthropic ──────────────────────────────────────────────
    ProviderTemplate {
        id: "anthropic",
        display_name: "Anthropic",
        api_format: ApiFormat::AnthropicMessages,
        base_url: "https://api.anthropic.com",
        requires_key: true,
        models: &[
            ModelTemplate {
                id: "claude-sonnet-4-5",
                display_name: Some("Claude Sonnet 4.5"),
                context_length: Some(200_000),
                max_output_tokens: Some(8_192),
            },
            ModelTemplate {
                id: "claude-3-5-sonnet-latest",
                display_name: Some("Claude 3.5 Sonnet"),
                context_length: Some(200_000),
                max_output_tokens: Some(8_192),
            },
            ModelTemplate {
                id: "claude-3-5-haiku-latest",
                display_name: Some("Claude 3.5 Haiku"),
                context_length: Some(200_000),
                max_output_tokens: Some(8_192),
            },
        ],
        default_model: "claude-sonnet-4-5",
        headers: &[],
        compat: Some(ProviderCompat {
            thinking_format: peko_provider_api::ThinkingFormat::Anthropic,
            deferred_tools_mode: peko_provider_api::DeferredToolsMode::Off,
        }),
    },
    // ── OpenAI-compatible providers (alphabetical) ────────────────────
    ProviderTemplate {
        id: "azure-openai",
        display_name: "Azure OpenAI",
        api_format: ApiFormat::OpenaiCompletions,
        base_url: "", // user must supply deployment URL
        requires_key: true,
        models: &[ModelTemplate {
            id: "gpt-4",
            display_name: Some("GPT-4 (deployment-specific)"),
            context_length: Some(8_192),
            max_output_tokens: None,
        }],
        default_model: "gpt-4",
        headers: &[],
        compat: None,
    },
    ProviderTemplate {
        id: "cohere",
        display_name: "Cohere",
        api_format: ApiFormat::OpenaiCompletions,
        base_url: "https://api.cohere.com/v2",
        requires_key: true,
        models: &[ModelTemplate {
            id: "command-r-plus",
            display_name: Some("Command R+"),
            context_length: Some(128_000),
            max_output_tokens: None,
        }],
        default_model: "command-r-plus",
        headers: &[],
        compat: None,
    },
    ProviderTemplate {
        id: "deepseek",
        display_name: "DeepSeek",
        api_format: ApiFormat::OpenaiCompletions,
        base_url: "https://api.deepseek.com/v1",
        requires_key: true,
        models: &[
            ModelTemplate {
                id: "deepseek-chat",
                display_name: Some("DeepSeek-V3"),
                context_length: Some(64_000),
                max_output_tokens: None,
            },
            ModelTemplate {
                id: "deepseek-reasoner",
                display_name: Some("DeepSeek-R1"),
                context_length: Some(64_000),
                max_output_tokens: None,
            },
        ],
        default_model: "deepseek-chat",
        headers: &[],
        // F29: DeepSeek-R1 distinguishes itself with explicit
        // `thinking: {type: "enabled"}` rather than the OpenAI
        // `reasoning_effort` shorthand. The Adapter projects
        // `thinking_effort` onto both `thinking` AND
        // `reasoning_effort` fields when compat kind is DeepSeek,
        // so callers get the same effort string as OpenAI plus the
        // DeepSeek-specific `type: "enabled"` marker.
        compat: Some(ProviderCompat {
            thinking_format: peko_provider_api::ThinkingFormat::DeepSeek,
            deferred_tools_mode: peko_provider_api::DeferredToolsMode::Off,
        }),
    },
    ProviderTemplate {
        id: "fireworks",
        display_name: "Fireworks AI",
        api_format: ApiFormat::OpenaiCompletions,
        base_url: "https://api.fireworks.ai/inference/v1",
        requires_key: true,
        models: &[ModelTemplate {
            id: "accounts/fireworks/models/llama-v3p1-70b-instruct",
            display_name: Some("Llama 3.1 70B (Fireworks)"),
            context_length: Some(131_072),
            max_output_tokens: None,
        }],
        default_model: "accounts/fireworks/models/llama-v3p1-70b-instruct",
        headers: &[],
        compat: None,
    },
    ProviderTemplate {
        id: "groq",
        display_name: "Groq",
        api_format: ApiFormat::OpenaiCompletions,
        base_url: "https://api.groq.com/openai/v1",
        requires_key: true,
        models: &[
            ModelTemplate {
                id: "llama-3.1-70b-versatile",
                display_name: Some("Llama 3.1 70B Versatile"),
                context_length: Some(131_072),
                max_output_tokens: None,
            },
            ModelTemplate {
                id: "llama-3.3-70b-versatile",
                display_name: Some("Llama 3.3 70B Versatile"),
                context_length: Some(131_072),
                max_output_tokens: None,
            },
        ],
        default_model: "llama-3.3-70b-versatile",
        headers: &[],
        // F29: Groq honours OpenAI's `reasoning_effort` namespace
        // for o-series models routed through Groq. The compat
        // annotation is here for clarity so the resolver binds it
        // to `ThinkingFormat::OpenAi` instead of relying on the
        // fallback (which is the same value).
        compat: Some(ProviderCompat {
            thinking_format: peko_provider_api::ThinkingFormat::OpenAi,
            deferred_tools_mode: peko_provider_api::DeferredToolsMode::Off,
        }),
    },
    ProviderTemplate {
        id: "moonshot",
        display_name: "Moonshot AI (Kimi)",
        api_format: ApiFormat::OpenaiCompletions,
        base_url: "https://api.moonshot.cn/v1",
        requires_key: true,
        models: &[ModelTemplate {
            id: "kimi-k2.5",
            display_name: Some("Kimi K2.5"),
            context_length: Some(128_000),
            max_output_tokens: None,
        }],
        default_model: "kimi-k2.5",
        headers: &[],
        // F29: Moonshot's `https://api.moonshot.cn/v1` endpoint is a
        // Chat-Completions-compatible surface that exposes Kimi's
        // `reasoning_content` block via the OpenAI-extended
        // `reasoning_effort` knob. Wire shape is identical to OpenAI
        // so we tag compat as `OpenAi`; the `Kimi` variant is
        // reserved for the Anthropic-compat endpoint
        // (`https://api.kimi.com/coding`) which uses a different
        // `extra_body.thinking` shape.
        compat: Some(ProviderCompat {
            thinking_format: peko_provider_api::ThinkingFormat::OpenAi,
            deferred_tools_mode: peko_provider_api::DeferredToolsMode::Off,
        }),
    },
    ProviderTemplate {
        id: "ollama",
        display_name: "Ollama (local)",
        api_format: ApiFormat::OpenaiCompletions,
        base_url: "http://localhost:11434/v1",
        requires_key: false,
        models: &[
            ModelTemplate {
                id: "llama3.1",
                display_name: Some("Llama 3.1"),
                context_length: Some(128_000),
                max_output_tokens: None,
            },
            ModelTemplate {
                id: "qwen2.5-coder",
                display_name: Some("Qwen 2.5 Coder"),
                context_length: Some(32_768),
                max_output_tokens: None,
            },
        ],
        default_model: "llama3.1",
        headers: &[],
        compat: None,
    },
    ProviderTemplate {
        id: "openrouter",
        display_name: "OpenRouter",
        api_format: ApiFormat::OpenaiCompletions,
        base_url: "https://openrouter.ai/api/v1",
        requires_key: true,
        models: &[
            ModelTemplate {
                id: "openai/gpt-4o-mini",
                display_name: Some("GPT-4o mini (via OpenRouter)"),
                context_length: Some(128_000),
                max_output_tokens: None,
            },
            ModelTemplate {
                id: "anthropic/claude-3.5-sonnet",
                display_name: Some("Claude 3.5 Sonnet (via OpenRouter)"),
                context_length: Some(200_000),
                max_output_tokens: None,
            },
        ],
        default_model: "openai/gpt-4o-mini",
        headers: &[],
        // F29: OpenRouter has its own `reasoning: {effort}` shape
        // (max = "high"). Wire emission for this variant lands in
        // F30+ (the per-compat reasoning helper). Today's compat
        // annotation lets the resolver bind the namespace so the
        // follow-up PR's emit does not silently fall back to OpenAI
        // and surprise users.
        compat: Some(ProviderCompat {
            thinking_format: peko_provider_api::ThinkingFormat::OpenRouter,
            deferred_tools_mode: peko_provider_api::DeferredToolsMode::Off,
        }),
    },
    ProviderTemplate {
        id: "perplexity",
        display_name: "Perplexity",
        api_format: ApiFormat::OpenaiCompletions,
        base_url: "https://api.perplexity.ai",
        requires_key: true,
        models: &[ModelTemplate {
            id: "llama-3.1-sonar-large-128k-online",
            display_name: Some("Llama 3.1 Sonar Large 128k Online"),
            context_length: Some(127_072),
            max_output_tokens: None,
        }],
        default_model: "llama-3.1-sonar-large-128k-online",
        headers: &[],
        compat: None,
    },
    ProviderTemplate {
        id: "together",
        display_name: "Together AI",
        api_format: ApiFormat::OpenaiCompletions,
        base_url: "https://api.together.xyz/v1",
        requires_key: true,
        models: &[ModelTemplate {
            id: "meta-llama/Meta-Llama-3.1-70B-Instruct-Turbo",
            display_name: Some("Llama 3.1 70B Instruct Turbo"),
            context_length: Some(131_072),
            max_output_tokens: None,
        }],
        default_model: "meta-llama/Meta-Llama-3.1-70B-Instruct-Turbo",
        headers: &[],
        // F29: Together's `reasoning: {enabled}` toggle. Wire
        // emission for this variant is documented in the plan;
        // defer concrete emit to F30+.
        compat: Some(ProviderCompat {
            thinking_format: peko_provider_api::ThinkingFormat::Together,
            deferred_tools_mode: peko_provider_api::DeferredToolsMode::Off,
        }),
    },
    ProviderTemplate {
        id: "xai",
        display_name: "xAI (Grok)",
        api_format: ApiFormat::OpenaiCompletions,
        base_url: "https://api.x.ai/v1",
        requires_key: true,
        models: &[
            ModelTemplate {
                id: "grok-beta",
                display_name: Some("Grok Beta"),
                context_length: Some(131_072),
                max_output_tokens: None,
            },
            ModelTemplate {
                id: "grok-2",
                display_name: Some("Grok 2"),
                context_length: Some(131_072),
                max_output_tokens: None,
            },
        ],
        default_model: "grok-2",
        headers: &[],
        compat: None,
    },
    // ── Anthropic-compatible providers ───────────────────────────────
    ProviderTemplate {
        id: "kimi",
        display_name: "Kimi (Kimi Code API)",
        api_format: ApiFormat::AnthropicMessages,
        base_url: "https://api.kimi.com/coding",
        requires_key: true,
        models: &[ModelTemplate {
            id: "kimi-for-coding",
            display_name: Some("Kimi for Coding"),
            context_length: Some(128_000),
            max_output_tokens: None,
        }],
        default_model: "kimi-for-coding",
        headers: &[],
        // F29: Kimi Code's Anthropic-compat surface uses
        // `extra_body.thinking = {type, effort, keep}` for
        // reasoning — distinct from Anthropic's native
        // `thinking: {type, budget_tokens}`. The
        // `DeferredToolsMode::Kimi` annotation tells the engine
        // loop's accumulator to wait for `Done { stop_reason:
        // ToolUse }` before surfacing tool calls. Wire emission for
        // this variant lands in F30+.
        compat: Some(ProviderCompat {
            thinking_format: peko_provider_api::ThinkingFormat::Kimi,
            deferred_tools_mode: peko_provider_api::DeferredToolsMode::Kimi,
        }),
    },
    ProviderTemplate {
        id: "minimax",
        display_name: "MiniMax",
        api_format: ApiFormat::AnthropicMessages,
        base_url: "https://api.minimaxi.com/anthropic",
        requires_key: true,
        models: &[ModelTemplate {
            id: "MiniMax-M3",
            display_name: Some("MiniMax M3"),
            context_length: Some(512_000),
            max_output_tokens: None,
        }],
        default_model: "MiniMax-M3",
        headers: &[],
        compat: Some(ProviderCompat {
            thinking_format: peko_provider_api::ThinkingFormat::Anthropic,
            deferred_tools_mode: peko_provider_api::DeferredToolsMode::Off,
        }),
    },
    // F29 new templates: zai + qwen
    ProviderTemplate {
        id: "zai",
        display_name: "Z.ai",
        api_format: ApiFormat::AnthropicMessages,
        base_url: "https://api.z.ai/api/anthropic",
        requires_key: true,
        models: &[
            ModelTemplate {
                id: "glm-4.6",
                display_name: Some("GLM-4.6"),
                context_length: Some(200_000),
                max_output_tokens: None,
            },
            ModelTemplate {
                id: "glm-4.5",
                display_name: Some("GLM-4.5"),
                context_length: Some(128_000),
                max_output_tokens: None,
            },
        ],
        default_model: "glm-4.6",
        headers: &[],
        // F29: Zai uses `thinking: {type, clear_thinking}` —
        // Anthropic-compat with `clear_thinking: "20251015"`. Wire
        // emission deferred to F30+; the compat binding is here so
        // the resolver returns `Zai` instead of `Anthropic`.
        compat: Some(ProviderCompat {
            thinking_format: peko_provider_api::ThinkingFormat::Zai,
            deferred_tools_mode: peko_provider_api::DeferredToolsMode::Off,
        }),
    },
    ProviderTemplate {
        id: "qwen",
        display_name: "Qwen (DashScope)",
        api_format: ApiFormat::OpenaiCompletions,
        base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1",
        requires_key: true,
        models: &[
            ModelTemplate {
                id: "qwen-plus",
                display_name: Some("Qwen Plus"),
                context_length: Some(128_000),
                max_output_tokens: None,
            },
            ModelTemplate {
                id: "qwen-turbo",
                display_name: Some("Qwen Turbo"),
                context_length: Some(128_000),
                max_output_tokens: None,
            },
        ],
        default_model: "qwen-plus",
        headers: &[],
        // F29: Qwen (DashScope) accepts
        // `extra_body.enable_thinking: bool`. Toggle only — no
        // effort levels. Wire emission deferred to F30+.
        compat: Some(ProviderCompat {
            thinking_format: peko_provider_api::ThinkingFormat::Qwen,
            deferred_tools_mode: peko_provider_api::DeferredToolsMode::Off,
        }),
    },
];

/// Find a template by its canonical id (case-insensitive).
#[must_use]
pub fn find_template(id: &str) -> Option<&'static ProviderTemplate> {
    let needle = id.to_lowercase();
    BUILT_IN_TEMPLATES
        .iter()
        .find(|t| t.id.eq_ignore_ascii_case(&needle))
}

/// Enumerate all built-in templates.
pub fn iter_templates() -> impl Iterator<Item = &'static ProviderTemplate> {
    BUILT_IN_TEMPLATES.iter()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_template_has_a_default_model_in_its_list() {
        for t in BUILT_IN_TEMPLATES {
            assert!(
                t.models.iter().any(|m| m.id == t.default_model),
                "template '{}' default_model '{}' is not in its models list",
                t.id,
                t.default_model
            );
        }
    }

    #[test]
    fn every_template_has_a_nonempty_model_list() {
        for t in BUILT_IN_TEMPLATES {
            assert!(
                !t.models.is_empty(),
                "template '{}' ships with no models",
                t.id
            );
        }
    }

    #[test]
    fn template_ids_are_unique_and_lowercase() {
        let mut seen = std::collections::HashSet::new();
        for t in BUILT_IN_TEMPLATES {
            assert_eq!(
                t.id,
                t.id.to_lowercase(),
                "template id '{}' not lowercase",
                t.id
            );
            assert!(seen.insert(t.id), "duplicate template id '{}'", t.id);
        }
    }

    #[test]
    fn find_template_is_case_insensitive() {
        assert!(find_template("Anthropic").is_some());
        assert!(find_template("ANTHROPIC").is_some());
        assert!(find_template("anthropic").is_some());
        assert!(find_template("nope").is_none());
    }

    // F29: per-template compat annotation sanity-checks. Each
    // specialty provider should land the expected ThinkingFormat +
    // DeferredToolsMode pairing so the resolver binds to the right
    // emit path. Generic providers (openai/azure/cohere/etc.) carry
    // `compat: None` and fall through to the adapter's built-in
    // defaults — verified separately in `resolver_compact_resolves_to_none`.
    fn compat_of(id: &str) -> Option<ProviderCompat> {
        find_template(id).and_then(|t| t.compat)
    }

    #[test]
    fn f29_deepseek_carries_deepseek_thinking_format() {
        let c = compat_of("deepseek").expect("deepseek template has compat");
        assert_eq!(
            c.thinking_format,
            peko_provider_api::ThinkingFormat::DeepSeek
        );
        assert_eq!(
            c.deferred_tools_mode,
            peko_provider_api::DeferredToolsMode::Off
        );
    }

    #[test]
    fn f29_moonshot_carries_openai_thinking_format() {
        // Moonshot's `https://api.moonshot.cn/v1` is a chat-completions
        // surface — same `reasoning_effort` field as OpenAI.
        let c = compat_of("moonshot").expect("moonshot template has compat");
        assert_eq!(c.thinking_format, peko_provider_api::ThinkingFormat::OpenAi);
    }

    #[test]
    fn f29_kimi_anthropic_compat_carries_kimi_thinking_format_and_deferred_tools() {
        let c = compat_of("kimi").expect("kimi template has compat");
        assert_eq!(c.thinking_format, peko_provider_api::ThinkingFormat::Kimi);
        assert_eq!(
            c.deferred_tools_mode,
            peko_provider_api::DeferredToolsMode::Kimi
        );
    }

    #[test]
    fn f29_openrouter_and_together_have_distinct_thinking_formats() {
        let or = compat_of("openrouter").expect("openrouter compat");
        let tg = compat_of("together").expect("together compat");
        assert_eq!(
            or.thinking_format,
            peko_provider_api::ThinkingFormat::OpenRouter
        );
        assert_eq!(
            tg.thinking_format,
            peko_provider_api::ThinkingFormat::Together
        );
    }

    #[test]
    fn f29_new_zai_and_qwen_templates_exist_with_distinct_formats() {
        let z = compat_of("zai").expect("zai template registered");
        let q = compat_of("qwen").expect("qwen template registered");
        assert_eq!(z.thinking_format, peko_provider_api::ThinkingFormat::Zai);
        assert_eq!(q.thinking_format, peko_provider_api::ThinkingFormat::Qwen);
        // Both flavours have non-Kimi deferred-tools behaviour.
        assert_eq!(
            z.deferred_tools_mode,
            peko_provider_api::DeferredToolsMode::Off
        );
        assert_eq!(
            q.deferred_tools_mode,
            peko_provider_api::DeferredToolsMode::Off
        );
    }

    #[test]
    fn f29_anthropic_compat_clones_have_distinct_thinking_format_from_kimi() {
        // Anthropic, MiniMax, and Zai all speak Anthropic messages —
        // but each carries a distinct ThinkingFormat that influences
        // the per-adapter emit path. Pin the distinction so a future
        // refactor can't accidentally collapse them.
        let anthropic = compat_of("anthropic").expect("anthropic compat");
        let zai = compat_of("zai").expect("zai compat");
        assert_eq!(
            anthropic.thinking_format,
            peko_provider_api::ThinkingFormat::Anthropic
        );
        assert_ne!(anthropic.thinking_format, zai.thinking_format);
    }

    #[test]
    fn f29_generic_providers_carry_no_compat_annotation() {
        // openai, azure-openai, cohere, fireworks, ollama, perplexity,
        // xai — vanilla Chat-Completions providers without specialty
        // reasoning knobs. Their compat is `None` so the resolver
        // returns the adapter's built-in defaults.
        for id in [
            "openai",
            "azure-openai",
            "cohere",
            "fireworks",
            "ollama",
            "perplexity",
            "xai",
        ] {
            assert!(
                compat_of(id).is_none(),
                "template '{id}' unexpectedly carries a compat annotation"
            );
        }
    }
}
