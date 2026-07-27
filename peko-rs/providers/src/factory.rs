//! One-shot provider construction from a configured model.
//!
//! `LlmResolver::build_provider` is the only call site; the function
//! translates the catalog-level view (`ModelConfig`) plus a resolved
//! API key into a fully-wired `Arc<Provider>` ready for an `Adapter`
//! to consume. Adapter selection is driven by `config.api_format`;
//! the model id is threaded per request.
//!
//! F40b / PR #3 Phase 2B: the retry / timeout knobs are no longer
//! hard-coded. `LlmResolver` carries a `ProviderRetryConfig` (default
//! built from `peko_provider_api::ProviderRetryConfig::default()`)
//! and threads it through here. The HTTP request timeout
//! (`PROVIDER_TIMEOUT_SECS = 5min`) stays a constant because it
//! doesn't vary across users — only retries do.
//!
//! F40 audit remediation: pre-F40 the same knob was 3 — too few for
//! realistic 429 bursts and not coordinated with the engine layer
//! (stacked transport+engine could compound to 12 attempts on a
//! single 429 wave). The new defaults are calibrated against codex's
//! `codex-client/src/retry.rs:42-47` retry policy and the empirical
//! distribution of provider 429 retry windows.

use anyhow::Result;
use std::sync::Arc;

use crate::adapters::{AnthropicAdapter, AnyAdapter, OpenAiAdapter, OpenAiResponsesAdapter};
use crate::catalog::ModelConfig;
use crate::core::{Provider, ProviderRuntimeOptions};

/// Default HTTP request timeout for outbound LLM calls, in seconds.
const PROVIDER_TIMEOUT_SECS: u64 = 300;

/// F40: total worst-case attempts across transport + engine per LLM
/// call. Sized as `transport.max_retries + engine.stream_max_retries`
/// so a single ceiling replaces the pre-F40 stacked-budget
/// anti-pattern. When the counter reaches zero, the engine surfaces
/// `AgenticError::RetryLimit { attempts, max_attempts, cause }` and
/// the caller can downcast instead of string-parsing.
///
/// Public so callers that build their own shared retry budget (e.g.
/// the agentic loop's mid-stream retry site) can read the default
/// without threading a separate config through.
pub fn default_max_attempts() -> u32 {
    peko_provider_api::ProviderRetryConfig::default().max_attempts
}

/// Build an `Arc<Provider>` from a configured model + API key + retry config.
///
/// `retry` is read-only; the factory does not mutate it. When the
/// caller passes `ProviderRetryConfig::default()` (the resolver does
/// this when no `[provider.retry]` block was supplied), every knob
/// inherits the F40 factory constants — preserving pre-F40b
/// behavior for daemons that haven't migrated their config.
pub fn create_provider_for_model(
    config: &ModelConfig,
    api_key: &str,
    retry: &peko_provider_api::ProviderRetryConfig,
) -> Result<Arc<Provider>> {
    let adapter = match config.api_format {
        crate::catalog::ApiFormat::OpenaiCompletions => {
            let a = if config.base_url.is_empty() {
                OpenAiAdapter::new()
            } else {
                OpenAiAdapter::new().with_base_url(&config.base_url)
            };
            AnyAdapter::OpenAi(a)
        }
        crate::catalog::ApiFormat::AnthropicMessages => {
            let a = if config.base_url.is_empty() {
                AnthropicAdapter::new()
            } else {
                AnthropicAdapter::new().with_base_url(&config.base_url)
            };
            AnyAdapter::Anthropic(a)
        }
        crate::catalog::ApiFormat::OpenAiResponses => {
            let a = if config.base_url.is_empty() {
                OpenAiResponsesAdapter::new()
            } else {
                OpenAiResponsesAdapter::new().with_base_url(&config.base_url)
            };
            AnyAdapter::OpenAiResponses(a)
        }
    };

    let options = ProviderRuntimeOptions {
        default_model_id: config.model_id.clone(),
        context_window: config.context_window,
        timeout_seconds: PROVIDER_TIMEOUT_SECS,
        max_retries: retry.max_retries,
        retry_delay_ms: retry.retry_delay_ms,
        extra_headers: config
            .headers
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        // F23: cache plumbing. The session id is plumbed by callers
        // (the agentic loop sets it via the new field). The factory
        // just needs to surface the field; an empty `session_id` is
        // equivalent to the legacy "rely on automatic prefix
        // detection" behavior.
        session_id: None,
        cache_retention: Default::default(),
        // F40b: thread the configured jitter band so every
        // provider the factory constructs inherits the same
        // ±N% backoff spread. `None` here means "deterministic
        // pre-F40 behavior"; `Some(0.1)` matches codex's default.
        retry_jitter: retry.retry_jitter,
    };

    Provider::new(adapter, api_key.to_string(), options).map(Arc::new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::ModelConfig;
    use crate::templates;
    use peko_provider_api::ProviderRetryConfig;

    fn anthropic_config() -> ModelConfig {
        ModelConfig::from_template(
            templates::find_template("anthropic").unwrap(),
            "anthropic-haiku",
            "claude-3-5-haiku-latest",
        )
    }

    #[test]
    fn builds_anthropic_provider_with_model_id() {
        let config = anthropic_config();
        let retry = ProviderRetryConfig::default();
        let provider = create_provider_for_model(&config, "sk-test", &retry).unwrap();
        assert_eq!(provider.model_id(), config.model_id);
        // Provider::name() is the adapter name, not the configured model id.
        assert_eq!(provider.name(), "anthropic");
    }

    #[test]
    fn empty_base_url_keeps_adapter_default() {
        let mut config = anthropic_config();
        config.base_url = String::new();
        config.id = "anthropic-empty".to_string();
        let retry = ProviderRetryConfig::default();
        let provider = create_provider_for_model(&config, "sk-test", &retry).unwrap();
        assert_eq!(provider.model_id(), config.model_id);
    }

    #[test]
    fn model_headers_propagate_to_provider() {
        // Catalog-level headers must land on the Provider's
        // `ProviderRuntimeOptions::extra_headers` so the HTTP
        // client attaches them on every outbound request.
        let mut config = anthropic_config();
        config.headers = std::collections::BTreeMap::from([
            (
                "anthropic-beta".to_string(),
                "interleaved-thinking-2025-05-08".to_string(),
            ),
            ("X-Org".to_string(), "acme".to_string()),
        ]);
        let retry = ProviderRetryConfig::default();
        let provider = create_provider_for_model(&config, "sk-test", &retry).unwrap();
        let opts = provider.options();
        assert!(opts
            .extra_headers
            .iter()
            .any(|(k, v)| k == "anthropic-beta" && v == "interleaved-thinking-2025-05-08"));
        assert!(opts
            .extra_headers
            .iter()
            .any(|(k, v)| k == "X-Org" && v == "acme"));
    }

    /// F40b: a non-default `ProviderRetryConfig` reaches
    /// `ProviderRuntimeOptions` so callers can bump `max_retries` /
    /// `retry_delay_ms` / `retry_jitter` without touching the
    /// daemon's hard-coded factory constants.
    #[test]
    fn retry_config_overrides_propagate_to_provider() {
        let config = anthropic_config();
        let retry = ProviderRetryConfig {
            max_retries: 9,
            retry_delay_ms: 250,
            retry_max_delay_ms: 7_500,
            retry_jitter: Some(0.25),
            max_attempts: 12,
        };
        retry.validate().expect("non-default values must validate");
        let provider = create_provider_for_model(&config, "sk-test", &retry).unwrap();
        let opts = provider.options();
        assert_eq!(opts.max_retries, 9, "max_retries override must propagate");
        assert_eq!(
            opts.retry_delay_ms, 250,
            "retry_delay_ms override must propagate"
        );
        assert_eq!(
            opts.retry_jitter,
            Some(0.25),
            "retry_jitter override must propagate"
        );
    }
}
