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
}
