//! `LlmResolver` — chooses a configured model at request time and
//! builds a one-shot `Provider` instance.
//!
//! Every LLM call funnels through `LlmResolver::resolve`, which applies
//! the precedence rules and returns the configured `ModelConfig`. There
//! is no runtime default model: a model id must be supplied explicitly
//! or pinned to the Principal/agent.
//!
//! ## Precedence
//!
//! 1. **Explicit caller override** — passed via IPC / CLI /
//!    `peko send --model <configured-model-id>`. Wins unconditionally.
//! 2. **Principal-pinned choice** — set in `principal.toml` as
//!    `preferred_model_id`. The Principal must be created with a model.
//! 3. error — "no model configured for this call"
//!
//! ## Env-var bootstrap (CI / headless)
//!
//! On platforms without an OS keychain (or for CI), `LlmResolver`
//! can be started with `--bootstrap-env-keys`. In that mode, if the
//! credential vault has no credential for the configured model, the
//! resolver falls back to the conventional `*_API_KEY` env vars. This
//! is a read-only path: keys found this way are never written back.

use anyhow::{anyhow, Context, Result};
use secrecy::{ExposeSecret, SecretString};
use std::collections::HashMap;
use std::sync::Arc;

use crate::catalog::{ModelCatalog, ModelConfig};
use crate::core::Provider;
use crate::factory::create_provider_for_model;
use crate::secret_store::SecretStore;
use peko_provider_api::credentials::{CredentialError, CredentialProvider};

/// Inputs to `LlmResolver::resolve`.
#[derive(Debug, Default, Clone)]
pub struct ResolveRequest<'a> {
    /// Explicit caller override (`peko send --model ...`).
    pub override_model: Option<&'a str>,
    /// Session-pinned choice from a prior turn.
    pub session_model: Option<&'a str>,
    /// Principal/agent soft preference.
    pub agent_model: Option<&'a str>,
}

/// Outcome of a successful resolution.
#[derive(Debug, Clone)]
pub struct ResolvedChoice {
    pub config: ModelConfig,
    pub model_id: String,
    /// Which precedence level won. Useful for diagnostics / logging.
    pub source: ResolveSource,
}

/// Which precedence level produced the resolved choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveSource {
    ExplicitOverride,
    SessionPinned,
    AgentPreference,
}

impl ResolveSource {
    /// Short label for log lines.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::ExplicitOverride => "override",
            Self::SessionPinned => "session",
            Self::AgentPreference => "agent",
        }
    }
}

/// The runtime LLM resolver.
pub struct LlmResolver {
    catalog: Arc<ModelCatalog>,
    secrets: Arc<dyn SecretStore>,
    credentials: Option<Arc<dyn CredentialProvider>>,
    bootstrap_env_keys: bool,
    /// Test-only mock adapter. Set by [`LlmResolver::mock`] so the
    /// resolver returns a `MockAdapter`-backed `Provider` for the
    /// `"mock"` catalog model. Always `None` in production builds
    /// (the `#[cfg(test)]` gating happens in
    /// [`LlmResolver::build_provider`], which short-circuits when
    /// `mock_adapter` is `None` and instead builds the real adapter).
    mock_adapter: Option<crate::MockAdapter>,
}

impl LlmResolver {
    /// Create a new resolver backed by a generic secret store.
    #[must_use]
    pub fn new(catalog: Arc<ModelCatalog>, secrets: Arc<dyn SecretStore>) -> Self {
        Self {
            catalog,
            secrets,
            credentials: None,
            bootstrap_env_keys: false,
            mock_adapter: None,
        }
    }

    /// Attach a [`CredentialProvider`] so the resolver reads
    /// credential material from the v2 credential API. The concrete
    /// provider implementation lives in the root composition layer
    /// (`VaultCredentialProvider`); `peko-providers` only sees the
    /// trait.
    #[must_use]
    pub fn with_credential_provider(mut self, credentials: Arc<dyn CredentialProvider>) -> Self {
        self.credentials = Some(credentials);
        self
    }

    /// Enable the env-var bootstrap path. Intended for CI and headless
    /// deployments where the OS keychain is unavailable.
    #[must_use]
    pub fn with_env_bootstrap(mut self) -> Self {
        self.bootstrap_env_keys = true;
        self
    }

    /// Build a mock-backed resolver for tests.
    ///
    /// Public (not `#[cfg(test)]`) because integration tests in the
    /// root crate need to construct an `LlmResolver` wired to a
    /// `MockAdapter` (e.g. `src/extensions/mcp/protocol/sampling.rs`
    /// builds one for the sampling-createMessage handler tests).
    /// The `#[cfg(test)]` mock helper that lived in this crate's
    /// own test module referenced root-only `Vault` types, so it
    /// could not be called from outside. This helper uses
    /// [`InMemorySecretStore`](crate::secret_store::InMemorySecretStore)
    /// and a no-op credentials trait impl so it has no root-only
    /// deps.
    pub async fn mock(
        adapter: crate::MockAdapter,
        catalog_path: impl AsRef<std::path::Path>,
    ) -> (std::sync::Arc<Self>, crate::MockAdapter) {
        use crate::catalog::{ApiFormat, ModelConfig};
        use crate::ModelCatalog;

        let catalog = ModelCatalog::load_or_init(catalog_path)
            .await
            .expect("mock catalog init failed");

        let config = ModelConfig {
            id: "mock".to_string(),
            display_name: "Mock Model".to_string(),
            template_id: None,
            api_format: ApiFormat::OpenaiCompletions,
            base_url: String::new(),
            model_id: "mock-model".to_string(),
            context_window: None,
            max_output_tokens: None,
            headers: std::collections::BTreeMap::new(),
            credential_id: None,
            requires_key: false,
            enabled: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            compat: None,
        };
        catalog.upsert(config).await.expect("mock upsert failed");

        let secrets: Arc<dyn crate::secret_store::SecretStore> =
            Arc::new(crate::secret_store::InMemorySecretStore::default());
        let resolver = std::sync::Arc::new(Self {
            catalog,
            secrets,
            credentials: Some(Arc::new(NoopCredentialProvider)),
            bootstrap_env_keys: false,
            mock_adapter: Some(adapter.clone()),
        });
        (resolver, adapter)
    }

    /// Whether the env-var bootstrap path is enabled.
    #[must_use]
    pub fn bootstrap_env_keys(&self) -> bool {
        self.bootstrap_env_keys
    }

    /// Resolve a configured model per the precedence chain.
    pub async fn resolve(&self, req: ResolveRequest<'_>) -> Result<ResolvedChoice> {
        if let Some(id) = req.override_model {
            let config = self.require_enabled_model(id).await?;
            return Ok(ResolvedChoice {
                model_id: config.model_id.clone(),
                config,
                source: ResolveSource::ExplicitOverride,
            });
        }

        if let Some(id) = req.session_model {
            if let Some(config) = self.catalog.get_enabled(id).await {
                return Ok(ResolvedChoice {
                    model_id: config.model_id.clone(),
                    config,
                    source: ResolveSource::SessionPinned,
                });
            }
        }

        if let Some(id) = req.agent_model {
            if let Some(config) = self.catalog.get_enabled(id).await {
                return Ok(ResolvedChoice {
                    model_id: config.model_id.clone(),
                    config,
                    source: ResolveSource::AgentPreference,
                });
            }
        }

        anyhow::bail!("no model configured for this call")
    }

    /// Resolve a request then immediately build a `Provider` ready to serve.
    pub async fn build(&self, req: ResolveRequest<'_>) -> Result<(Arc<Provider>, ResolvedChoice)> {
        let choice = self.resolve(req).await?;
        let provider = self.build_provider(&choice.config).await?;
        Ok((provider, choice))
    }

    /// Build a one-shot `Provider` for the given configured model.
    pub async fn build_provider(&self, config: &ModelConfig) -> Result<Arc<Provider>> {
        let provider = if config.id == "mock" {
            // The mock adapter path is reserved for tests. Production
            // callers asking for a "mock" model id fall through to
            // `create_provider_for_model`, which builds the real
            // adapter (typically a `MockAdapter` anyway when running
            // with `MockAdapter::default` in catalog templates).
            if let Some(ref adapter) = self.mock_adapter {
                Self::build_mock_provider(adapter.clone(), config)?
            } else {
                create_provider_for_model(config, "mock-key")?
            }
        } else {
            let api_key = self
                .resolve_api_key(config)
                .with_context(|| format!("no API key available for model '{}'", config.id))?;
            create_provider_for_model(config, api_key.expose_secret())?
        };

        Ok(provider)
    }

    /// Test-only helper: build a `Provider` backed by the shared mock adapter.
    ///
    /// Always available (not `#[cfg(test)]`) so external integration
    /// tests can route through [`LlmResolver::mock`] without depending
    /// on the crate's test module being compiled.
    fn build_mock_provider(
        adapter: crate::MockAdapter,
        config: &ModelConfig,
    ) -> Result<Arc<Provider>> {
        use crate::adapters::AnyAdapter;
        use crate::core::ProviderRuntimeOptions;

        let options = ProviderRuntimeOptions {
            default_model_id: config.model_id.clone(),
            context_window: config.context_window,
            timeout_seconds: 300,
            max_retries: 3,
            retry_delay_ms: 1000,
            ..Default::default()
        };

        Provider::new(AnyAdapter::Mock(adapter), "mock-key".to_string(), options).map(Arc::new)
    }

    /// Internal: look up the API key for a configured model.
    fn resolve_api_key(&self, config: &ModelConfig) -> Result<SecretString> {
        // 1. Credential provider via credential_id.
        if let Some(id) = &config.credential_id {
            if let Some(provider) = &self.credentials {
                match provider.get_credential(id) {
                    Ok(Some(material)) => return Ok(material.material.clone()),
                    Ok(None) => {}
                    Err(CredentialError::Backend(msg)) => {
                        if !self.bootstrap_env_keys {
                            return Err(anyhow!(
                                "credential backend error for credential '{id}': {msg}"
                            ));
                        }
                    }
                }
            }
            match self.secrets.get(id) {
                Ok(Some(secret)) => return Ok(secret),
                Ok(None) => {}
                Err(e) => {
                    if !self.bootstrap_env_keys {
                        return Err(anyhow!("vault backend error for credential '{id}': {e}"));
                    }
                }
            }
        }

        // 2. If the model does not require a key, allow an empty key.
        if !config.requires_key {
            return Ok(SecretString::new(String::new().into()));
        }

        // 3. Env-var bootstrap keyed by template_id or model id.
        if self.bootstrap_env_keys {
            for var in env_var_candidates(config) {
                if let Ok(v) = std::env::var(var) {
                    if !v.trim().is_empty() {
                        return Ok(SecretString::from(v));
                    }
                }
            }
        }

        Err(anyhow!(
            "no key for model '{}' (env bootstrap {})",
            config.id,
            if self.bootstrap_env_keys { "on" } else { "off" }
        ))
    }

    /// Reference to the underlying catalog.
    #[must_use]
    pub fn catalog(&self) -> &Arc<ModelCatalog> {
        &self.catalog
    }

    /// Reference to the underlying secret store.
    #[must_use]
    pub fn secrets(&self) -> &Arc<dyn SecretStore> {
        &self.secrets
    }

    /// Try every key-resolution path and report which one(s) would
    /// have worked. For diagnostics / CLI display.
    pub fn probe(&self, config: &ModelConfig) -> KeyProbeReport {
        let mut report = KeyProbeReport::default();
        if let Some(id) = &config.credential_id {
            match self.secrets.get(id) {
                Ok(Some(_)) => report.secret_store = Some(true),
                Ok(None) => report.secret_store = Some(false),
                Err(e) => report.secret_store_error = Some(e.to_string()),
            }
        } else if config.requires_key {
            report.secret_store = Some(false);
        } else {
            report.secret_store = Some(true);
        }
        if self.bootstrap_env_keys {
            for var in env_var_candidates(config) {
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

/// Conventional env-var names checked during the bootstrap fallback.
fn env_var_candidates(config: &ModelConfig) -> Vec<&'static str> {
    let key = config.template_id.as_deref().unwrap_or(&config.id);
    match key {
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
            let upper = key.to_uppercase().replace('-', "_");
            vec![Box::leak(format!("{upper}_API_KEY").into_boxed_str())]
        }
    }
}

impl LlmResolver {
    async fn require_enabled_model(&self, id: &str) -> Result<ModelConfig> {
        self.catalog
            .get_enabled(id)
            .await
            .with_context(|| format!("model '{id}' not found or disabled in the catalog"))
    }
}

impl std::fmt::Display for ResolvedChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} / {} (via {})",
            self.config.id,
            self.model_id,
            self.source.label()
        )
    }
}

/// No-op `CredentialProvider` for mock-backed test resolvers.
///
/// The mock provider path doesn't go through the vault — the resolver
/// short-circuits to the in-memory `MockAdapter` for any model whose
/// `id == "mock"`. The credential lookup is never exercised, so we
/// only need a trait impl that returns `Ok(None)` for everything.
struct NoopCredentialProvider;

impl peko_provider_api::credentials::CredentialProvider for NoopCredentialProvider {
    fn get_credential(
        &self,
        _id: &str,
    ) -> std::result::Result<
        Option<std::sync::Arc<peko_provider_api::credentials::CredentialMaterial>>,
        peko_provider_api::credentials::CredentialError,
    > {
        Ok(None)
    }

    fn load_rotation_credentials(
        &self,
        _namespace: &str,
        _name: &str,
    ) -> std::result::Result<
        Vec<peko_provider_api::credentials::RotationEntry>,
        peko_provider_api::credentials::CredentialError,
    > {
        Ok(Vec::new())
    }

    fn record_test(&self, _credential_id: &str, _ok: bool) {}
}

/// Single-credential `CredentialProvider` for tests that need
/// `resolve_api_key` to return a known material without depending on
/// root's `Vault` type. Holds the material in a `SecretString` so the
/// resolver sees it through the trait just like a vault-backed impl.
struct StaticCredentialProvider {
    credential_id: String,
    material: secrecy::SecretString,
}

impl StaticCredentialProvider {
    fn new(credential_id: String, material: secrecy::SecretString) -> Self {
        Self {
            credential_id,
            material,
        }
    }
}

impl peko_provider_api::credentials::CredentialProvider for StaticCredentialProvider {
    fn get_credential(
        &self,
        id: &str,
    ) -> std::result::Result<
        Option<std::sync::Arc<peko_provider_api::credentials::CredentialMaterial>>,
        peko_provider_api::credentials::CredentialError,
    > {
        if id == self.credential_id {
            Ok(Some(std::sync::Arc::new(
                peko_provider_api::credentials::CredentialMaterial {
                    material: self.material.clone(),
                },
            )))
        } else {
            Ok(None)
        }
    }

    fn load_rotation_credentials(
        &self,
        _namespace: &str,
        _name: &str,
    ) -> std::result::Result<
        Vec<peko_provider_api::credentials::RotationEntry>,
        peko_provider_api::credentials::CredentialError,
    > {
        Ok(Vec::new())
    }

    fn record_test(&self, _credential_id: &str, _ok: bool) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret_store::InMemorySecretStore;
    use crate::templates;
    use secrecy::SecretString;
    use tempfile::tempdir;

    async fn tempdir_catalog() -> (tempfile::TempDir, Arc<ModelCatalog>) {
        let dir = tempdir().unwrap();
        let path = dir.path().join("models.toml");
        let cat = ModelCatalog::load_or_init(&path).await.unwrap();
        (dir, cat)
    }

    fn openai_config() -> ModelConfig {
        ModelConfig::from_template(
            templates::find_template("openai").unwrap(),
            "openai-gpt-4o",
            "gpt-4o",
        )
    }

    fn anthropic_config() -> ModelConfig {
        ModelConfig::from_template(
            templates::find_template("anthropic").unwrap(),
            "anthropic-sonnet",
            "claude-sonnet-4-5",
        )
    }

    async fn seeded_catalog() -> (tempfile::TempDir, Arc<ModelCatalog>) {
        let (dir, cat) = tempdir_catalog().await;
        cat.upsert(openai_config()).await.unwrap();
        cat.upsert(anthropic_config()).await.unwrap();
        (dir, cat)
    }

    fn resolver(cat: Arc<ModelCatalog>) -> LlmResolver {
        let secrets: Arc<dyn crate::secret_store::SecretStore> =
            Arc::new(crate::secret_store::InMemorySecretStore::default());
        let provider: Arc<dyn peko_provider_api::credentials::CredentialProvider> =
            Arc::new(NoopCredentialProvider);
        LlmResolver::new(cat, secrets).with_credential_provider(provider)
    }

    #[tokio::test]
    async fn explicit_override_wins() {
        let (_d, cat) = seeded_catalog().await;
        let r = resolver(cat);
        let choice = r
            .resolve(ResolveRequest {
                override_model: Some("anthropic-sonnet"),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(choice.config.id, "anthropic-sonnet");
        assert_eq!(choice.model_id, "claude-sonnet-4-5");
        assert_eq!(choice.source, ResolveSource::ExplicitOverride);
    }

    #[tokio::test]
    async fn principal_preference_beats_error() {
        let (_d, cat) = seeded_catalog().await;
        let r = resolver(cat);
        let choice = r
            .resolve(ResolveRequest {
                agent_model: Some("openai-gpt-4o"),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(choice.config.id, "openai-gpt-4o");
        assert_eq!(choice.source, ResolveSource::AgentPreference);
    }

    #[tokio::test]
    async fn override_beats_principal_preference() {
        let (_d, cat) = seeded_catalog().await;
        let r = resolver(cat);
        let choice = r
            .resolve(ResolveRequest {
                override_model: Some("anthropic-sonnet"),
                agent_model: Some("openai-gpt-4o"),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(choice.config.id, "anthropic-sonnet");
        assert_eq!(choice.source, ResolveSource::ExplicitOverride);
    }

    #[tokio::test]
    async fn unknown_override_errors() {
        let (_d, cat) = seeded_catalog().await;
        let r = resolver(cat);
        assert!(r
            .resolve(ResolveRequest {
                override_model: Some("nope"),
                ..Default::default()
            })
            .await
            .is_err());
    }

    #[tokio::test]
    async fn no_model_configured_errors() {
        let (_d, cat) = tempdir_catalog().await;
        let r = resolver(cat);
        assert!(r.resolve(ResolveRequest::default()).await.is_err());
    }

    #[tokio::test]
    async fn disabled_entries_are_skipped() {
        let (_d, cat) = seeded_catalog().await;
        {
            let mut cfg = openai_config();
            cfg.enabled = false;
            cat.upsert(cfg).await.unwrap();
        }
        let r = resolver(cat);
        assert!(r
            .resolve(ResolveRequest {
                agent_model: Some("openai-gpt-4o"),
                ..Default::default()
            })
            .await
            .is_err());
    }

    #[tokio::test]
    async fn resolve_api_key_reads_credential_by_id() {
        let (_d, cat) = seeded_catalog().await;
        let mut cfg = anthropic_config();
        let cred_id = "anthropic-cred-1".to_string();
        cfg.credential_id = Some(cred_id.clone());
        cat.upsert(cfg).await.unwrap();

        // Use a self-contained `StaticCredentialProvider` so the
        // providers crate's tests don't depend on root's `Vault` type.
        let secrets: Arc<dyn crate::secret_store::SecretStore> =
            Arc::new(crate::secret_store::InMemorySecretStore::default());
        let provider: Arc<dyn peko_provider_api::credentials::CredentialProvider> = Arc::new(
            StaticCredentialProvider::new(cred_id, SecretString::new("sk-ant-v2".into())),
        );
        let r = LlmResolver::new(cat, secrets).with_credential_provider(provider);
        let key = r
            .resolve_api_key(r.catalog.get("anthropic-sonnet").await.as_ref().unwrap())
            .unwrap();
        assert_eq!(key.expose_secret(), "sk-ant-v2");
    }

    #[tokio::test]
    async fn env_bootstrap_kicks_in_when_no_key() {
        let (_d, cat) = seeded_catalog().await;
        let mut cfg = anthropic_config();
        cfg.template_id = Some("anthropic".into());
        cat.upsert(cfg).await.unwrap();
        std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-from-env");
        let secrets = Arc::new(InMemorySecretStore::new());
        let r = LlmResolver::new(cat, secrets).with_env_bootstrap();
        let key = r
            .resolve_api_key(r.catalog.get("anthropic-sonnet").await.as_ref().unwrap())
            .unwrap();
        assert_eq!(key.expose_secret(), "sk-ant-from-env");
        std::env::remove_var("ANTHROPIC_API_KEY");
    }

    #[tokio::test]
    async fn env_bootstrap_off_by_default() {
        let (_d, cat) = seeded_catalog().await;
        let mut cfg = openai_config();
        cfg.template_id = Some("openai".into());
        cfg.credential_id = None;
        cat.upsert(cfg).await.unwrap();
        std::env::set_var("OPENAI_API_KEY", "sk-from-env");
        let secrets = Arc::new(InMemorySecretStore::new());
        let r = LlmResolver::new(cat, secrets);
        assert!(r
            .resolve_api_key(r.catalog.get("openai-gpt-4o").await.as_ref().unwrap())
            .is_err());
        std::env::remove_var("OPENAI_API_KEY");
    }

    #[tokio::test]
    async fn probe_reports_storage_and_env() {
        let (_d, cat) = seeded_catalog().await;
        let mut cfg = openai_config();
        cfg.template_id = Some("openai".into());
        cfg.credential_id = Some("cred-openai".into());
        cat.upsert(cfg).await.unwrap();
        std::env::set_var("OPENAI_API_KEY", "sk-from-env");
        let secrets = Arc::new(InMemorySecretStore::from_pairs(&[(
            "cred-openai",
            "sk-from-store",
        )]));
        let r = LlmResolver::new(cat, secrets).with_env_bootstrap();
        let config = r.catalog.get("openai-gpt-4o").await.unwrap();
        let probe = r.probe(&config);
        assert_eq!(probe.secret_store, Some(true));
        assert_eq!(probe.env_vars.get("OPENAI_API_KEY").copied(), Some(true));
        std::env::remove_var("OPENAI_API_KEY");
    }

    #[tokio::test]
    async fn env_candidates_for_known_providers() {
        let mut cfg = openai_config();
        cfg.template_id = Some("openai".into());
        assert_eq!(env_var_candidates(&cfg), vec!["OPENAI_API_KEY"]);
        let mut cfg = anthropic_config();
        cfg.template_id = Some("anthropic".into());
        assert_eq!(env_var_candidates(&cfg), vec!["ANTHROPIC_API_KEY"]);
    }

    #[tokio::test]
    async fn env_candidates_generic_fallback() {
        let mut cfg = ModelConfig::from_template(
            templates::find_template("openai").unwrap(),
            "my-custom",
            "my-model",
        );
        cfg.template_id = None;
        assert_eq!(env_var_candidates(&cfg), vec!["MY_CUSTOM_API_KEY"]);
    }

    #[tokio::test]
    async fn reload_arc_makes_resolver_observe_new_entries() {
        let (_dir, cat) = tempdir_catalog().await;
        cat.upsert(openai_config()).await.unwrap();
        let r = resolver(cat.clone());
        let choice = r
            .resolve(ResolveRequest {
                agent_model: Some("openai-gpt-4o"),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(choice.config.id, "openai-gpt-4o");

        let path = cat.path().to_path_buf();
        let mut new_file = crate::catalog::ModelCatalogFile::default();
        new_file
            .entries
            .insert("anthropic-sonnet".to_string(), anthropic_config());
        std::fs::write(&path, toml::to_string(&new_file).unwrap()).unwrap();

        let count = cat.reload().await.unwrap();
        assert_eq!(count, 1);

        let choice = r
            .resolve(ResolveRequest {
                agent_model: Some("anthropic-sonnet"),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(choice.config.id, "anthropic-sonnet");
    }

    // F29: compat-resolution regression pins. The templates seed
    // compat annotations on the specialty providers; `from_template`
    // must thread them through so the resolver-bound `ModelConfig`
    // carries the same hints. Pre-F29 callers had no `compat` field
    // and were unaffected — these pins fix the new shape.

    fn deepseek_config() -> ModelConfig {
        ModelConfig::from_template(
            templates::find_template("deepseek").unwrap(),
            "deepseek-chat-v3",
            "deepseek-chat",
        )
    }

    fn kimi_anthropic_config() -> ModelConfig {
        ModelConfig::from_template(
            templates::find_template("kimi").unwrap(),
            "kimi-coding",
            "kimi-for-coding",
        )
    }

    #[test]
    fn f29_from_template_threads_deepseek_compat() {
        let cfg = deepseek_config();
        let compat = cfg
            .compat
            .expect("deepseek template compat propagated into ModelConfig");
        assert_eq!(
            compat.thinking_format,
            peko_provider_api::ThinkingFormat::DeepSeek
        );
        assert_eq!(
            compat.deferred_tools_mode,
            peko_provider_api::DeferredToolsMode::Off
        );
    }

    #[test]
    fn f29_from_template_threads_kimi_compat_with_deferred_tools() {
        let cfg = kimi_anthropic_config();
        let compat = cfg
            .compat
            .expect("kimi Anthropic-compat template compat propagated");
        assert_eq!(
            compat.thinking_format,
            peko_provider_api::ThinkingFormat::Kimi
        );
        assert_eq!(
            compat.deferred_tools_mode,
            peko_provider_api::DeferredToolsMode::Kimi
        );
    }

    #[test]
    fn f29_from_template_threads_none_compat_for_generic_providers() {
        // OpenAI ships no compat annotation — the resolver returns
        // `None` and the adapter falls back to F25's defaults.
        let cfg = openai_config();
        assert!(cfg.compat.is_none());
    }

    #[tokio::test]
    async fn f29_resolved_choice_carries_compat_through_resolve() {
        let (_d, cat) = tempdir_catalog().await;
        cat.upsert(deepseek_config()).await.unwrap();
        cat.upsert(kimi_anthropic_config()).await.unwrap();
        let r = resolver(cat);
        let ds = r
            .resolve(ResolveRequest {
                override_model: Some("deepseek-chat-v3"),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(
            ds.config
                .compat
                .expect("deepseek compat survives resolve")
                .thinking_format,
            peko_provider_api::ThinkingFormat::DeepSeek
        );
        let kimi = r
            .resolve(ResolveRequest {
                override_model: Some("kimi-coding"),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(
            kimi.config
                .compat
                .expect("kimi compat survives resolve")
                .deferred_tools_mode,
            peko_provider_api::DeferredToolsMode::Kimi
        );
    }
}
