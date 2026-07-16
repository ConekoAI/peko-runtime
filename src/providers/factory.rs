//! One-shot provider construction from a catalog entry.
//!
//! `LlmResolver::build_provider` is the only call site; the function
//! translates the catalog-level view (`ProviderCatalogEntry` +
//! `ModelInfo`) plus a resolved API key into a fully-wired
//! `Arc<Provider>` ready for an `Adapter` to consume. Adapter
//! selection is driven by `entry.api_format`; the model id is *not*
//! baked into the adapter (it's threaded per request), but is held
//! in `Provider::default_model_id` so legacy callers and `Provider::model_id()`
//! continue to return the entry's curated default.
//!
//! The retry / timeout knobs are hard-coded to the catalog's standard
//! policy (5-minute request timeout, 3 retries with 1s initial
//! backoff). They were historically configurable via the old
//! `ProviderConfig`; with the catalog as the single source of truth
//! there is no per-provider user surface for them today, so they
//! live as constants here and the struct passed to `Provider::new`
//! is intentionally narrow.

use anyhow::{Context, Result};
use std::sync::Arc;

use crate::providers::adapters::{AnthropicAdapter, AnyAdapter, OpenAiAdapter};
use crate::providers::catalog::{ModelInfo, ProviderCatalogEntry};
use crate::providers::core::{Provider, ProviderRuntimeOptions};

/// Default HTTP request timeout for outbound LLM calls, in seconds.
const PROVIDER_TIMEOUT_SECS: u64 = 300;

/// Default retry count for transient transport errors.
const PROVIDER_MAX_RETRIES: u32 = 3;

/// Default initial backoff between retries, in milliseconds.
const PROVIDER_RETRY_DELAY_MS: u64 = 1000;

/// Build an `Arc<Provider>` from a catalog entry + API key + chosen model.
///
/// The returned `Provider` carries `default_model_id == entry.default_model_id`
/// (not the resolved `model.id`), so legacy callers that consult
/// `Provider::model_id()` without an explicit override see the
/// catalog's curated default rather than a per-call selection. The
/// `model` argument's `id` is the value the catalog declares for
/// that specific model row; the wiring here is consistent with
/// `create_provider_for_entry`'s prior behavior so existing
/// integration tests continue to pass.
pub fn create_provider_for_entry(
    entry: &ProviderCatalogEntry,
    api_key: &str,
    model: &ModelInfo,
) -> Result<Arc<Provider>> {
    let adapter = match entry.api_format {
        crate::providers::catalog::ApiFormat::OpenaiCompletions => {
            let a = if entry.base_url.is_empty() {
                OpenAiAdapter::new()
            } else {
                OpenAiAdapter::new().with_base_url(&entry.base_url)
            };
            AnyAdapter::OpenAi(a)
        }
        crate::providers::catalog::ApiFormat::AnthropicMessages => {
            let a = if entry.base_url.is_empty() {
                AnthropicAdapter::new()
            } else {
                AnthropicAdapter::new().with_base_url(&entry.base_url)
            };
            AnyAdapter::Anthropic(a)
        }
    };

    // Sanity-check: the resolver only invokes this once
    // `entry.model(model.id)` returned `Some`, so this lookup should
    // always succeed. Surface a clean error if a caller in the future
    // bypasses the resolver.
    let _ = entry.model(&model.id).with_context(|| {
        format!(
            "create_provider_for_entry: model '{}' is not declared on provider '{}'",
            model.id, entry.id
        )
    })?;

    let options = ProviderRuntimeOptions {
        default_model_id: entry.default_model_id.clone(),
        timeout_seconds: PROVIDER_TIMEOUT_SECS,
        max_retries: PROVIDER_MAX_RETRIES,
        retry_delay_ms: PROVIDER_RETRY_DELAY_MS,
    };

    Provider::new(adapter, api_key.to_string(), options).map(Arc::new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::catalog::{ModelInfo, ProviderCatalogEntry};
    use crate::providers::templates;

    fn anthropic_entry() -> ProviderCatalogEntry {
        ProviderCatalogEntry::from_template(
            templates::find_template("anthropic").unwrap(),
            "anthropic",
            None,
        )
    }

    #[test]
    fn builds_anthropic_provider_with_curated_default_model_id() {
        let entry = anthropic_entry();
        let model = ModelInfo::new(entry.default_model_id.clone());
        let provider = create_provider_for_entry(&entry, "sk-test", &model).unwrap();
        // default_model_id is the catalog's curated choice, not the
        // (often-identical) passed-in `model.id`.
        assert_eq!(provider.model_id(), entry.default_model_id);
        assert_eq!(provider.name(), "anthropic");
    }

    #[test]
    fn errors_on_undeclared_model() {
        let entry = anthropic_entry();
        let bogus = ModelInfo::new("claude-imaginary");
        assert!(create_provider_for_entry(&entry, "sk-test", &bogus).is_err());
    }

    #[test]
    fn empty_base_url_keeps_adapter_default() {
        // Templates may ship with an empty base_url (e.g. azure-openai
        // requires a deployment URL). The factory must still build a
        // provider without erroring — the user fills in base_url via
        // the desktop's Edit Provider modal before any call is made.
        let mut entry = anthropic_entry();
        entry.base_url = String::new();
        entry.id = "anthropic-empty".to_string();
        let model = ModelInfo::new(entry.default_model_id.clone());
        let provider = create_provider_for_entry(&entry, "sk-test", &model).unwrap();
        assert_eq!(provider.model_id(), entry.default_model_id);
    }
}
