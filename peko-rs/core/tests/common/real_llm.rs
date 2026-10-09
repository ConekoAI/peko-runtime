//! Vendor-neutral real-LLM configuration for the opt-in real-LLM tier.
//!
//! The runtime ships no vendor templates, so tests describe the endpoint
//! entirely through the environment:
//!
//! | Variable | Required | Meaning |
//! |---|---|---|
//! | `LLM_API_KEY` | yes | API key, read by the resolver's env bootstrap |
//! | `LLM_BASE_URL` | yes | Endpoint base URL |
//! | `LLM_MODEL` | yes | Wire model id sent to the endpoint |
//! | `LLM_API_FORMAT` | no | `anthropic_messages` (default), `openai_completions`, or `openai_responses` |
//!
//! The seeded catalog entry's id is [`REAL_LLM_MODEL_ID`]; under
//! `PEKO_TEST_RESOLVER_BOOTSTRAP=1` the resolver derives the key variable
//! from that id (`llm` → `LLM_API_KEY`).
//!
//! Shared by the CLI integration tests and, via `#[path]`, the inline
//! `daemon::e2e_tests::tunnel_e2e` test.

#![allow(dead_code)]

use peko_providers::catalog::{ApiFormat, ModelConfig};
use std::collections::BTreeMap;

/// Catalog id of the seeded real-LLM entry.
pub const REAL_LLM_MODEL_ID: &str = "llm";
/// API-key variable the resolver's env bootstrap reads for [`REAL_LLM_MODEL_ID`].
pub const REAL_LLM_KEY_VAR: &str = "LLM_API_KEY";

/// A real LLM endpoint described by the environment.
#[derive(Debug, Clone)]
pub struct RealLlm {
    pub base_url: String,
    pub model: String,
    pub api_format: ApiFormat,
}

impl RealLlm {
    /// The configured endpoint, or `None` when any required variable is
    /// unset or empty (real-LLM tests skip in that case).
    pub fn from_env() -> Option<Self> {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        var(REAL_LLM_KEY_VAR)?;
        let api_format = match var("LLM_API_FORMAT").as_deref() {
            None | Some("anthropic_messages") => ApiFormat::AnthropicMessages,
            Some("openai_completions") => ApiFormat::OpenaiCompletions,
            Some("openai_responses") => ApiFormat::OpenAiResponses,
            Some(other) => panic!("unsupported LLM_API_FORMAT {other:?}"),
        };
        Some(Self {
            base_url: var("LLM_BASE_URL")?.trim_end_matches('/').to_string(),
            model: var("LLM_MODEL")?,
            api_format,
        })
    }

    /// Why [`Self::from_env`] returned `None`, for skip messages.
    pub fn missing_env() -> String {
        format!("{REAL_LLM_KEY_VAR}, LLM_BASE_URL, and LLM_MODEL must be set")
    }

    /// The catalog entry the daemon resolves for [`REAL_LLM_MODEL_ID`].
    pub fn catalog_entry(&self) -> ModelConfig {
        let now = chrono::Utc::now();
        ModelConfig {
            id: REAL_LLM_MODEL_ID.to_string(),
            display_name: format!("Real LLM ({})", self.model),
            template_id: None,
            api_format: self.api_format,
            base_url: self.base_url.clone(),
            model_id: self.model.clone(),
            context_window: None,
            max_output_tokens: None,
            headers: BTreeMap::new(),
            credential_id: None,
            requires_key: true,
            enabled: true,
            created_at: now,
            updated_at: now,
            compat: None,
            spec: None,
            note: None,
        }
    }
}
