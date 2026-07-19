//! One-shot provider construction from a configured model.
//!
//! `LlmResolver::build_provider` is the only call site; the function
//! translates the catalog-level view (`ModelConfig`) plus a resolved
//! API key into a fully-wired `Arc<Provider>` ready for an `Adapter`
//! to consume. Adapter selection is driven by `config.api_format`;
//! the model id is threaded per request.
//!
//! The retry / timeout knobs are hard-coded to the catalog's standard
//! policy (5-minute request timeout, 3 retries with 1s initial
//! backoff).

use anyhow::Result;
use std::sync::Arc;

use crate::providers::adapters::{AnthropicAdapter, AnyAdapter, OpenAiAdapter};
use crate::providers::catalog::ModelConfig;
use crate::providers::core::{Provider, ProviderRuntimeOptions};

/// Default HTTP request timeout for outbound LLM calls, in seconds.
const PROVIDER_TIMEOUT_SECS: u64 = 300;

/// Default retry count for transient transport errors.
const PROVIDER_MAX_RETRIES: u32 = 3;

/// Default initial backoff between retries, in milliseconds.
const PROVIDER_RETRY_DELAY_MS: u64 = 1000;

/// Build an `Arc<Provider>` from a configured model + API key.
pub fn create_provider_for_model(config: &ModelConfig, api_key: &str) -> Result<Arc<Provider>> {
    let adapter = match config.api_format {
        crate::providers::catalog::ApiFormat::OpenaiCompletions => {
            let a = if config.base_url.is_empty() {
                OpenAiAdapter::new()
            } else {
                OpenAiAdapter::new().with_base_url(&config.base_url)
            };
            AnyAdapter::OpenAi(a)
        }
        crate::providers::catalog::ApiFormat::AnthropicMessages => {
            let a = if config.base_url.is_empty() {
                AnthropicAdapter::new()
            } else {
                AnthropicAdapter::new().with_base_url(&config.base_url)
            };
            AnyAdapter::Anthropic(a)
        }
    };

    let options = ProviderRuntimeOptions {
        default_model_id: config.model_id.clone(),
        context_window: config.context_window,
        timeout_seconds: PROVIDER_TIMEOUT_SECS,
        max_retries: PROVIDER_MAX_RETRIES,
        retry_delay_ms: PROVIDER_RETRY_DELAY_MS,
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
    };

    Provider::new(adapter, api_key.to_string(), options).map(Arc::new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::catalog::ModelConfig;
    use crate::providers::templates;

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
        let provider = create_provider_for_model(&config, "sk-test").unwrap();
        assert_eq!(provider.model_id(), config.model_id);
        // Provider::name() is the adapter name, not the configured model id.
        assert_eq!(provider.name(), "anthropic");
    }

    #[test]
    fn empty_base_url_keeps_adapter_default() {
        let mut config = anthropic_config();
        config.base_url = String::new();
        config.id = "anthropic-empty".to_string();
        let provider = create_provider_for_model(&config, "sk-test").unwrap();
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
        let provider = create_provider_for_model(&config, "sk-test").unwrap();
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
}
