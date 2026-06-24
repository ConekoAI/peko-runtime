//! `LlmResolver` — chooses a provider/model at request time and
//! builds a one-shot `Provider` instance.
//!
//! This is the structural piece that lets agents carry no provider
//! state. Every LLM call funnels through `LlmResolver::resolve`,
//! which applies the precedence rules and returns the
//! `(ProviderCatalogEntry, ModelInfo)` pair needed for the call.
//! `LlmResolver::build_provider` then looks up the API key from the
//! `SecretStore` and constructs an `Arc<Provider>`.
//!
//! ## Precedence
//!
//! 1. **Explicit caller override** — passed via IPC / CLI /
//!    `peko send --provider X --model Y`. Wins unconditionally.
//! 2. **Session-pinned choice** — set by a prior turn via
//!    `peko session.set-model`. Carries between turns, not within.
//! 3. **Agent preference** — `agent.toml` `preferred_provider_id` /
//!    `preferred_model_id`. Soft hint only.
//! 4. **Runtime default** — `peko provider set-default <id>`.
//! 5. **First enabled catalog entry** — last-resort fallback.
//!
//! All four earlier levels may be `None`; the resolver walks down
//! until one matches an enabled entry.
//!
//! ## Env-var bootstrap (CI / headless)
//!
//! On platforms without an OS keychain (or for CI), `LlmResolver`
//! can be started with `--bootstrap-env-keys`. In that mode, if the
//! secret store returns `Backend` (not `None`) or the requested
//! provider has no key, the resolver falls back to the conventional
//! `*_API_KEY` env vars. This is a read-only path: keys found this
//! way are never written back.

use anyhow::{anyhow, Context, Result};
use secrecy::{ExposeSecret, SecretString};
use std::collections::HashMap;
use std::sync::Arc;

use crate::common::secret_store::SecretStore;
use crate::providers::catalog::{ModelInfo, ProviderCatalog, ProviderCatalogEntry};
use crate::providers::core::Provider;
use crate::providers::registry::create_provider_for_entry;

/// Inputs to `LlmResolver::resolve`.
///
/// All fields are optional; the resolver walks the precedence chain
/// until one matches an enabled catalog entry.
#[derive(Debug, Default, Clone)]
pub struct ResolveRequest<'a> {
    /// Explicit caller override (`peko send --provider ... --model ...`).
    pub override_provider: Option<&'a str>,
    pub override_model: Option<&'a str>,
    /// Session-pinned choice from a prior turn.
    pub session_provider: Option<&'a str>,
    pub session_model: Option<&'a str>,
    /// Agent soft preferences.
    pub agent_provider: Option<&'a str>,
    pub agent_model: Option<&'a str>,
}

/// Outcome of a successful resolution.
#[derive(Debug, Clone)]
pub struct ResolvedChoice {
    pub entry: ProviderCatalogEntry,
    pub model: ModelInfo,
    /// Which precedence level won. Useful for diagnostics / logging.
    pub source: ResolveSource,
}

/// Which precedence level produced the resolved choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveSource {
    ExplicitOverride,
    SessionPinned,
    AgentPreference,
    RuntimeDefault,
    FirstEnabled,
}

impl ResolveSource {
    /// Short label for log lines.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::ExplicitOverride => "override",
            Self::SessionPinned => "session",
            Self::AgentPreference => "agent",
            Self::RuntimeDefault => "default",
            Self::FirstEnabled => "first-enabled",
        }
    }
}

/// The runtime LLM resolver.
///
/// One instance is shared across the runtime via `Arc<LlmResolver>`.
/// It is stateless apart from references to the catalog and secret
/// store; multiple resolvers can coexist.
pub struct LlmResolver {
    catalog: Arc<ProviderCatalog>,
    secrets: Arc<dyn SecretStore>,
    /// If true, the resolver falls back to conventional `*_API_KEY`
    /// env vars when the secret store has no entry. Read-only path;
    /// keys found via env are never persisted.
    bootstrap_env_keys: bool,
}

impl LlmResolver {
    /// Create a new resolver.
    #[must_use]
    pub fn new(catalog: Arc<ProviderCatalog>, secrets: Arc<dyn SecretStore>) -> Self {
        Self {
            catalog,
            secrets,
            bootstrap_env_keys: false,
        }
    }

    /// Enable the env-var bootstrap path. Intended for CI and
    /// headless deployments where the OS keychain is unavailable.
    #[must_use]
    pub fn with_env_bootstrap(mut self) -> Self {
        self.bootstrap_env_keys = true;
        self
    }

    /// Whether the env-var bootstrap path is enabled.
    #[must_use]
    pub fn bootstrap_env_keys(&self) -> bool {
        self.bootstrap_env_keys
    }

    /// Resolve a `(provider_id, model_id)` request per the
    /// precedence chain. Returns the catalog entry and model plus
    /// which level won.
    pub async fn resolve(&self, req: ResolveRequest<'_>) -> Result<ResolvedChoice> {
        // 1. Explicit override.
        if let Some(pid) = req.override_provider {
            let entry = self.require_enabled_entry(pid).await?;
            let model = resolve_model_on(&entry, req.override_model)?;
            return Ok(ResolvedChoice {
                entry,
                model,
                source: ResolveSource::ExplicitOverride,
            });
        }

        // 2. Session-pinned.
        if let Some(pid) = req.session_provider {
            if let Some(entry) = self.catalog.get_enabled(pid).await {
                let model = resolve_model_on(&entry, req.session_model)?;
                return Ok(ResolvedChoice {
                    entry,
                    model,
                    source: ResolveSource::SessionPinned,
                });
            }
        }

        // 3. Agent preference.
        if let Some(pid) = req.agent_provider {
            if let Some(entry) = self.catalog.get_enabled(pid).await {
                let model = resolve_model_on(&entry, req.agent_model)?;
                return Ok(ResolvedChoice {
                    entry,
                    model,
                    source: ResolveSource::AgentPreference,
                });
            }
        }

        // 4. Runtime default.
        let (default_pid, default_model_id) = self.catalog.get_default().await;
        if let Some(pid) = default_pid {
            if let Some(entry) = self.catalog.get_enabled(&pid).await {
                let model = resolve_model_on(&entry, default_model_id.as_deref())?;
                return Ok(ResolvedChoice {
                    entry,
                    model,
                    source: ResolveSource::RuntimeDefault,
                });
            }
        }

        // 5. First enabled entry.
        let enabled = self.catalog.list_enabled().await;
        let entry = enabled
            .first()
            .with_context(|| "no enabled providers in the catalog")?;
        let model = resolve_model_on(entry, None)?;
        Ok(ResolvedChoice {
            entry: entry.clone(),
            model,
            source: ResolveSource::FirstEnabled,
        })
    }

    /// Resolve a request then immediately build a `Provider` ready to
    /// serve. This is the hot path used by `Agent::run*`.
    pub async fn build(&self, req: ResolveRequest<'_>) -> Result<(Arc<Provider>, ResolvedChoice)> {
        let choice = self.resolve(req).await?;
        let provider = self.build_provider(&choice.entry, &choice.model).await?;
        Ok((provider, choice))
    }

    /// Build a one-shot `Provider` for the given entry + model.
    ///
    /// Looks up the API key from the `SecretStore` (with optional
    /// env-var fallback). The adapter is constructed from the
    /// catalog entry's `api_format` and `base_url` — it does not
    /// carry a model id; the model is threaded per call.
    pub async fn build_provider(
        &self,
        entry: &ProviderCatalogEntry,
        model: &ModelInfo,
    ) -> Result<Arc<Provider>> {
        let api_key = self
            .resolve_api_key(&entry.id)
            .with_context(|| format!("no API key available for provider '{}'", entry.id))?;
        create_provider_for_entry(entry, api_key.expose_secret(), model)
    }

    /// Internal: look up the API key for a provider.
    ///
    /// Resolution order:
    /// 1. Secret store (OS keychain or test backend).
    /// 2. If `bootstrap_env_keys` is enabled, conventional `*_API_KEY`
    ///    env vars (e.g. `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`).
    fn resolve_api_key(&self, account: &str) -> Result<SecretString> {
        // 1. Keychain / secret store.
        match self.secrets.get(account) {
            Ok(Some(secret)) => return Ok(secret),
            Ok(None) => {} // not present — fall through to env
            Err(e) => {
                // Backend error (e.g. OS keychain unavailable). Only
                // try the env fallback if explicitly enabled.
                if !self.bootstrap_env_keys {
                    return Err(anyhow!("secret store backend error for '{account}': {e}"));
                }
            }
        }

        // 2. Env-var bootstrap.
        if self.bootstrap_env_keys {
            for var in env_var_candidates(account) {
                if let Ok(v) = std::env::var(var) {
                    if !v.trim().is_empty() {
                        return Ok(SecretString::from(v));
                    }
                }
            }
        }

        Err(anyhow!(
            "no key for '{account}' (env bootstrap {})",
            if self.bootstrap_env_keys { "on" } else { "off" }
        ))
    }

    /// Reference to the underlying catalog.
    #[must_use]
    pub fn catalog(&self) -> &Arc<ProviderCatalog> {
        &self.catalog
    }

    /// Reference to the underlying secret store.
    #[must_use]
    pub fn secrets(&self) -> &Arc<dyn SecretStore> {
        &self.secrets
    }

    /// Verify the chosen entry has an API key. Returns `Ok(Some(true))`
    /// if present, `Ok(Some(false))` if shape looks wrong, `Ok(None)`
    /// if missing. Used by `credential.test`.
    pub fn test_key(&self, account: &str) -> Result<Option<bool>> {
        match self.secrets.test_format(account) {
            Ok(result) => Ok(result),
            Err(e) => Err(anyhow!("secret store backend error: {e}")),
        }
    }

    /// Try every key-resolution path and report which one(s) would
    /// have worked. For diagnostics / CLI display.
    pub fn probe(&self, account: &str) -> KeyProbeReport {
        let mut report = KeyProbeReport::default();
        match self.secrets.get(account) {
            Ok(Some(_)) => report.secret_store = Some(true),
            Ok(None) => report.secret_store = Some(false),
            Err(e) => report.secret_store_error = Some(e.to_string()),
        }
        if self.bootstrap_env_keys {
            for var in env_var_candidates(account) {
                if let Ok(v) = std::env::var(var) {
                    if !v.trim().is_empty() {
                        report.env_vars.insert(var.to_string(), true);
                    } else {
                        report.env_vars.insert(var.to_string(), false);
                    }
                }
            }
        }
        report
    }
}

/// Diagnostic output of `LlmResolver::probe`.
#[derive(Debug, Default, Clone)]
pub struct KeyProbeReport {
    /// `Some(true)` if a key was found in the secret store,
    /// `Some(false)` if the store is healthy but no key exists,
    /// `None` if the lookup was not attempted.
    pub secret_store: Option<bool>,
    /// Set when the secret store backend itself errored (e.g. OS
    /// keychain unavailable).
    pub secret_store_error: Option<String>,
    /// Env-var bootstrap probe results, only populated when
    /// `bootstrap_env_keys` is on.
    pub env_vars: HashMap<String, bool>,
}

/// Resolve a model on a given entry. `model_id == None` falls back
/// to the entry's declared default. The literal string `"default"` is
/// treated as a sentinel meaning "use the provider's default model" —
/// this matches the convention used in agent configs that predate the
/// v3 provider catalog (e.g. `preferred_model_id = "default"`).
fn resolve_model_on(entry: &ProviderCatalogEntry, model_id: Option<&str>) -> Result<ModelInfo> {
    let mid = model_id.filter(|m| !m.is_empty() && *m != "default");
    if let Some(mid) = mid {
        if let Some(m) = entry.model(mid) {
            return Ok(m.clone());
        }
        anyhow::bail!(
            "model '{mid}' is not declared on provider '{}' (declared: {})",
            entry.id,
            entry
                .models
                .iter()
                .map(|m| m.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    entry
        .model(&entry.default_model_id)
        .cloned()
        .with_context(|| {
            format!(
                "provider '{}' has no default model ({} declared models)",
                entry.id,
                entry.models.len()
            )
        })
}

/// Conventional env-var names checked during the bootstrap fallback.
fn env_var_candidates(provider_id: &str) -> Vec<&'static str> {
    match provider_id {
        "openai" => vec!["OPENAI_API_KEY"],
        "anthropic" => vec!["ANTHROPIC_API_KEY"],
        "azure-openai" | "azure" => vec!["AZURE_OPENAI_API_KEY"],
        "cohere" => vec!["COHERE_API_KEY"],
        "deepseek" => vec!["DEEPSEEK_API_KEY"],
        "fireworks" => vec!["FIREWORKS_API_KEY"],
        "groq" => vec!["GROQ_API_KEY"],
        "moonshot" => vec!["MOONSHOT_API_KEY", "KIMI_API_KEY"],
        "openrouter" => vec!["OPENROUTER_API_KEY"],
        "perplexity" => vec!["PERPLEXITY_API_KEY"],
        "together" => vec!["TOGETHER_API_KEY"],
        "xai" | "grok" => vec!["XAI_API_KEY"],
        "kimi" => vec!["KIMI_API_KEY"],
        "minimax" => vec!["MINIMAX_API_KEY"],
        _ => {
            // Generic fallback: <UPPER_ID>_API_KEY
            let upper = provider_id.to_uppercase().replace('-', "_");
            vec![Box::leak(format!("{upper}_API_KEY").into_boxed_str())]
        }
    }
}

impl LlmResolver {
    /// Helper used by tests / direct callers that need to look up an
    /// enabled entry by id with a clear error message.
    async fn require_enabled_entry(&self, id: &str) -> Result<ProviderCatalogEntry> {
        self.catalog
            .get_enabled(id)
            .await
            .with_context(|| format!("provider '{id}' not found or disabled in the catalog"))
    }
}

impl std::fmt::Display for ResolvedChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} / {} (via {})",
            self.entry.id,
            self.model.id,
            self.source.label()
        )
    }
}

/// Convenience: pick a sensible default `model_name` for a catalog
/// entry, used by the legacy `default_model_for_entry` adapter
/// factory below.
pub fn default_model_id(entry: &ProviderCatalogEntry) -> &str {
    &entry.default_model_id
}

#[doc(hidden)]
pub fn _model_for_entry<'a>(entry: &'a ProviderCatalogEntry) -> &'a ModelInfo {
    // Caller guarantees the default model exists; if not, this
    // panics — that's a programming error in the catalog seeder.
    entry
        .model(&entry.default_model_id)
        .expect("catalog entry missing its declared default_model_id")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::secret_store::InMemorySecretStore;
    use crate::providers::catalog::ApiFormat;
    use crate::providers::templates;
    use tempfile::tempdir;

    async fn tempdir_catalog() -> (tempfile::TempDir, Arc<ProviderCatalog>) {
        let dir = tempdir().unwrap();
        let path = dir.path().join("providers.toml");
        let cat = ProviderCatalog::load_or_init(&path).await.unwrap();
        (dir, cat)
    }

    async fn seeded_catalog() -> (tempfile::TempDir, Arc<ProviderCatalog>) {
        let (dir, cat) = tempdir_catalog().await;
        let openai = ProviderCatalogEntry::from_template(
            templates::find_template("openai").unwrap(),
            "openai",
            None,
        );
        let anthropic = ProviderCatalogEntry::from_template(
            templates::find_template("anthropic").unwrap(),
            "anthropic",
            None,
        );
        cat.upsert(openai).await.unwrap();
        cat.upsert(anthropic).await.unwrap();
        cat.set_default(Some("openai".into()), None).await.unwrap();
        (dir, cat)
    }

    fn resolver(cat: Arc<ProviderCatalog>) -> LlmResolver {
        let secrets = Arc::new(InMemorySecretStore::from_pairs(&[("openai", "sk-openai")]));
        LlmResolver::new(cat, secrets)
    }

    #[tokio::test]
    async fn explicit_override_wins() {
        let (_d, cat) = seeded_catalog().await;
        let r = resolver(cat);
        let choice = r
            .resolve(ResolveRequest {
                override_provider: Some("anthropic"),
                override_model: Some("claude-sonnet-4-5"),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(choice.entry.id, "anthropic");
        assert_eq!(choice.model.id, "claude-sonnet-4-5");
        assert_eq!(choice.source, ResolveSource::ExplicitOverride);
    }

    #[tokio::test]
    async fn session_pinned_beats_agent_and_default() {
        let (_d, cat) = seeded_catalog().await;
        let r = resolver(cat);
        let choice = r
            .resolve(ResolveRequest {
                session_provider: Some("anthropic"),
                session_model: None,
                agent_provider: Some("openai"),
                agent_model: None,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(choice.entry.id, "anthropic");
        assert_eq!(choice.source, ResolveSource::SessionPinned);
    }

    #[tokio::test]
    async fn agent_preference_beats_default() {
        let (_d, cat) = seeded_catalog().await;
        let r = resolver(cat);
        let choice = r
            .resolve(ResolveRequest {
                agent_provider: Some("anthropic"),
                agent_model: Some("claude-3-5-haiku-latest"),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(choice.entry.id, "anthropic");
        assert_eq!(choice.model.id, "claude-3-5-haiku-latest");
        assert_eq!(choice.source, ResolveSource::AgentPreference);
    }

    #[tokio::test]
    async fn agent_model_default_sentinel_falls_back_to_provider_default() {
        let (_d, cat) = seeded_catalog().await;
        let r = resolver(cat);
        let choice = r
            .resolve(ResolveRequest {
                agent_provider: Some("anthropic"),
                agent_model: Some("default"),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(choice.entry.id, "anthropic");
        assert_eq!(choice.model.id, "claude-sonnet-4-5");
        assert_eq!(choice.source, ResolveSource::AgentPreference);
    }

    #[tokio::test]
    async fn runtime_default_used_when_no_overrides() {
        let (_d, cat) = seeded_catalog().await;
        let r = resolver(cat);
        let choice = r.resolve(ResolveRequest::default()).await.unwrap();
        assert_eq!(choice.entry.id, "openai");
        assert_eq!(choice.source, ResolveSource::RuntimeDefault);
    }

    #[tokio::test]
    async fn unknown_override_errors() {
        let (_d, cat) = seeded_catalog().await;
        let r = resolver(cat);
        assert!(r
            .resolve(ResolveRequest {
                override_provider: Some("nope"),
                ..Default::default()
            })
            .await
            .is_err());
    }

    #[tokio::test]
    async fn override_model_must_exist_on_provider() {
        let (_d, cat) = seeded_catalog().await;
        let r = resolver(cat);
        assert!(r
            .resolve(ResolveRequest {
                override_provider: Some("openai"),
                override_model: Some("gpt-99-imaginary"),
                ..Default::default()
            })
            .await
            .is_err());
    }

    #[tokio::test]
    async fn first_enabled_used_when_no_default() {
        let (dir, cat) = tempdir_catalog().await;
        cat.upsert(ProviderCatalogEntry::from_template(
            templates::find_template("anthropic").unwrap(),
            "anthropic",
            None,
        ))
        .await
        .unwrap();
        let r = resolver(cat);
        let choice = r.resolve(ResolveRequest::default()).await.unwrap();
        assert_eq!(choice.entry.id, "anthropic");
        assert_eq!(choice.source, ResolveSource::FirstEnabled);
        drop(dir);
    }

    #[tokio::test]
    async fn empty_catalog_errors() {
        let (_d, cat) = tempdir_catalog().await;
        let r = resolver(cat);
        assert!(r.resolve(ResolveRequest::default()).await.is_err());
    }

    #[tokio::test]
    async fn disabled_entries_are_skipped() {
        let (_d, cat) = seeded_catalog().await;
        // Disable the default (openai)
        {
            let e = cat.get("openai").await.unwrap();
            cat.upsert(ProviderCatalogEntry {
                enabled: false,
                ..e
            })
            .await
            .unwrap();
        }
        let r = resolver(cat);
        let choice = r.resolve(ResolveRequest::default()).await.unwrap();
        // Default openai is disabled; resolver falls back to anthropic.
        assert_eq!(choice.entry.id, "anthropic");
        assert_eq!(choice.source, ResolveSource::FirstEnabled);
    }

    #[tokio::test]
    async fn env_bootstrap_kicks_in_when_no_key() {
        let (_d, cat) = seeded_catalog().await;
        std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-from-env");
        let secrets = Arc::new(InMemorySecretStore::new()); // empty
        let r = LlmResolver::new(cat, secrets).with_env_bootstrap();
        let key = r.resolve_api_key("anthropic").unwrap();
        assert_eq!(key.expose_secret(), "sk-ant-from-env");
        std::env::remove_var("ANTHROPIC_API_KEY");
    }

    #[tokio::test]
    async fn env_bootstrap_off_by_default() {
        let (_d, cat) = seeded_catalog().await;
        std::env::set_var("OPENAI_API_KEY", "sk-from-env");
        let secrets = Arc::new(InMemorySecretStore::new());
        let r = LlmResolver::new(cat, secrets); // bootstrap OFF
        assert!(r.resolve_api_key("openai").is_err());
        std::env::remove_var("OPENAI_API_KEY");
    }

    #[tokio::test]
    async fn probe_reports_storage_and_env() {
        let (_d, cat) = seeded_catalog().await;
        std::env::set_var("OPENAI_API_KEY", "sk-from-env");
        let secrets = Arc::new(InMemorySecretStore::from_pairs(&[(
            "openai",
            "sk-from-store",
        )]));
        let r = LlmResolver::new(cat, secrets).with_env_bootstrap();
        let probe = r.probe("openai");
        assert_eq!(probe.secret_store, Some(true));
        assert_eq!(probe.env_vars.get("OPENAI_API_KEY").copied(), Some(true));
        std::env::remove_var("OPENAI_API_KEY");
    }

    #[tokio::test]
    async fn env_candidates_for_known_providers() {
        assert_eq!(env_var_candidates("openai"), vec!["OPENAI_API_KEY"]);
        assert_eq!(env_var_candidates("anthropic"), vec!["ANTHROPIC_API_KEY"]);
        assert!(env_var_candidates("moonshot").contains(&"MOONSHOT_API_KEY"));
    }

    #[tokio::test]
    async fn env_candidates_generic_fallback() {
        assert_eq!(env_var_candidates("my-custom"), vec!["MY_CUSTOM_API_KEY"]);
    }

    // ensure we cover the unsupported-feature in cargo features
    #[allow(dead_code)]
    fn _format_check(_: ApiFormat) {}
}
