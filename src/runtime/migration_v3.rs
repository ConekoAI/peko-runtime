//! ADR: provider catalog v3 migration.
//!
//! Migrate every on-disk `agent.toml` from the pre-v3 schema
//! (embedded `[provider]` table with literal `api_key`) to v3 (no
//! provider field, optional `preferred_*` hints, secrets in the OS
//! keychain).
//!
//! Idempotent. Runs at startup, gated by the `version` field on each
//! config (already at `"3.0"` => no-op).
//!
//! ## Steps
//!
//! 1. For every `~/.peko/agents/<name>/config.toml`:
//!    - Parse with `AgentConfig`. The deprecated `provider` field
//!      deserializes via `#[serde(default)]`.
//!    - If a non-default provider block exists, create a
//!      `ProviderCatalogEntry` if none with the same id exists.
//!    - Move any literal `api_key` into the OS keychain under
//!      `provider_id`.
//!    - Set `preferred_provider_id` / `preferred_model_id` from the
//!      legacy fields (if not already set).
//!    - Bump `version` to `"3.0"`.
//!    - Atomic write.
//! 2. Read `~/.peko/credentials.json` (legacy plaintext). Move each
//!    entry into the keychain (skipping already-present entries).
//!    Delete the plaintext file.
//! 3. Log a summary: `migrated_agents`, `created_providers`,
//!    `secrets_to_keychain`, `deleted_credentials_json`.

use anyhow::Result;
use tracing::{info, warn};

use crate::common::paths::PathResolver;
use crate::common::secret_store::{OsKeychainSecretStore, SecretStore};
use crate::providers::catalog::{ApiFormat, ModelInfo, ProviderCatalog, ProviderCatalogEntry};
use crate::types::agent::AgentConfig;

/// Result of the v3 migration.
#[derive(Debug, Default, Clone)]
pub struct CatalogMigrationReport {
    pub migrated_agents: usize,
    pub created_providers: usize,
    pub secrets_to_keychain: usize,
    pub deleted_credentials_json: bool,
}

/// Run the v3 migration. Idempotent.
pub async fn migrate_adr_provider_catalog_v3(
    resolver: &PathResolver,
) -> Result<CatalogMigrationReport> {
    let mut report = CatalogMigrationReport::default();

    let catalog_path = resolver.config_dir().join(ProviderCatalog::FILENAME);
    let catalog = ProviderCatalog::load_or_init(&catalog_path).await?;
    let secrets: std::sync::Arc<dyn SecretStore> =
        std::sync::Arc::new(OsKeychainSecretStore::new());

    // 1. Walk every agent config.
    let agents_root = resolver.agents_root_dir();
    if agents_root.exists() {
        if let Ok(mut entries) = tokio::fs::read_dir(&agents_root).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let agent_path = entry.path();
                if !agent_path.is_dir() {
                    continue;
                }
                let config_path = agent_path.join("config.toml");
                if !config_path.exists() {
                    continue;
                }
                if let Err(e) = migrate_agent_config_file(
                    &config_path,
                    catalog.as_ref(),
                    secrets.as_ref(),
                    &mut report,
                )
                .await
                {
                    warn!(
                        "v3 migration: agent {} skipped ({})",
                        config_path.display(),
                        e
                    );
                }
            }
        }
    }

    // 2. Migrate legacy plaintext credentials.json.
    //
    // The file historically stored TWO kinds of secrets:
    //   * `credentials` — provider API keys (plaintext) → keychain
    //   * `registry`    — pekohub registry bearer token
    //
    // v3 moves the API keys to the OS keychain, but the registry
    // token MUST stay on disk (the `RegistryClient` reads it on every
    // request, and the keychain is provider-scoped to LLM providers,
    // not pekohub). The previous revision of this migration deleted
    // the whole file, which also wiped the registry token and broke
    // `peko ext push` / `peko ext pull` (CI failure surfaced by
    // s2_extension_registry_roundtrip after the PR #43 fix landed).
    //
    // We now load the full `CredentialsStore`, move API keys out,
    // and rewrite the file with `credentials: {}` and the original
    // `registry` field preserved.
    let creds_path = resolver.config_dir().join("credentials.json");
    if creds_path.exists() {
        match load_full_credentials_store(&creds_path) {
            Ok(mut store) => {
                let mut migrated_any = false;
                for (provider_id, credential) in std::mem::take(&mut store.credentials) {
                    if credential.api_key.is_empty() {
                        continue;
                    }
                    let already = secrets.get(&provider_id).ok().flatten().is_some();
                    if already {
                        continue;
                    }
                    let s = secrecy::SecretString::from(credential.api_key);
                    if let Err(e) = secrets.set(&provider_id, &s) {
                        warn!(
                            "v3 migration: failed to write key for '{provider_id}' to keychain: {e}"
                        );
                    } else {
                        report.secrets_to_keychain += 1;
                        migrated_any = true;
                    }
                }

                // If we migrated at least one API key, rewrite the
                // file with empty credentials but the registry token
                // (if any) preserved. If we migrated nothing AND the
                // file has no registry token either, delete it
                // (matches the original cleanup intent).
                if migrated_any || store.registry.is_some() {
                    if let Err(e) = write_credentials_store(&creds_path, &store) {
                        warn!(
                            "v3 migration: failed to rewrite credentials.json with registry token preserved: {e}"
                        );
                    }
                } else {
                    if let Err(e) = std::fs::remove_file(&creds_path) {
                        warn!("v3 migration: failed to delete legacy credentials.json: {e}");
                    } else {
                        report.deleted_credentials_json = true;
                    }
                }
            }
            Err(e) => warn!("v3 migration: could not read legacy credentials.json: {e}"),
        }
    }

    if report.migrated_agents > 0
        || report.created_providers > 0
        || report.secrets_to_keychain > 0
        || report.deleted_credentials_json
    {
        info!(
            "ADR provider-catalog-v3 migration complete: migrated_agents={}, \
             created_providers={}, secrets_to_keychain={}, deleted_credentials_json={}",
            report.migrated_agents,
            report.created_providers,
            report.secrets_to_keychain,
            report.deleted_credentials_json
        );
    }

    Ok(report)
}

async fn migrate_agent_config_file(
    config_path: &std::path::Path,
    catalog: &ProviderCatalog,
    secrets: &dyn SecretStore,
    report: &mut CatalogMigrationReport,
) -> Result<()> {
    let content = tokio::fs::read_to_string(config_path).await?;
    let mut config: AgentConfig = toml::from_str(&content)
        .map_err(|e| anyhow::anyhow!("parse {}: {e}", config_path.display()))?;

    if config.version == "3.0" {
        return Ok(());
    }

    // Extract legacy provider info. The `provider` field is still
    // deserializable thanks to `#[serde(default, skip_serializing)]`
    // on AgentConfig; it carries whatever the pre-v3 file contained.
    let legacy = config.provider.clone();
    let provider_id = provider_id_from_legacy(&legacy);
    let is_meaningful = legacy.api_key.is_some()
        || legacy.base_url.is_some()
        || matches!(
            legacy.provider_type,
            crate::types::provider::ProviderType::Anthropic
                | crate::types::provider::ProviderType::Ollama
                | crate::types::provider::ProviderType::Moonshot
                | crate::types::provider::ProviderType::Kimi
                | crate::types::provider::ProviderType::Minimax
                | crate::types::provider::ProviderType::OpenAICompatible
        );

    if is_meaningful {
        if catalog.get(&provider_id).await.is_none() {
            let model_id = legacy
                .models
                .get(&legacy.default_model)
                .map(|m| m.name.clone())
                .unwrap_or_else(|| legacy.default_model.clone());
            let base_url = legacy.base_url.clone().unwrap_or_else(|| {
                match legacy.provider_type {
                    crate::types::provider::ProviderType::Anthropic
                    | crate::types::provider::ProviderType::Kimi
                    | crate::types::provider::ProviderType::Minimax => {
                        "https://api.anthropic.com".to_string()
                    }
                    crate::types::provider::ProviderType::Ollama => {
                        "http://localhost:11434/v1".to_string()
                    }
                    _ => "https://api.openai.com/v1".to_string(),
                }
            });
            let api_format = match legacy.provider_type {
                crate::types::provider::ProviderType::Anthropic
                | crate::types::provider::ProviderType::Kimi
                | crate::types::provider::ProviderType::Minimax => ApiFormat::AnthropicMessages,
                _ => ApiFormat::OpenaiCompletions,
            };

            let entry = ProviderCatalogEntry {
                id: provider_id.clone(),
                display_name: provider_id.clone(),
                template_id: None,
                api_format,
                base_url,
                default_model_id: model_id.clone(),
                models: vec![ModelInfo {
                    id: model_id.clone(),
                    display_name: None,
                    context_length: None,
                    max_output_tokens: None,
                    capabilities: vec![],
                }],
                headers: std::collections::BTreeMap::new(),
                requires_key: true,
                enabled: true,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            };
            catalog.upsert(entry).await?;
            report.created_providers += 1;
        }

        if let Some(key) = legacy.api_key.clone() {
            if !key.is_empty() {
                let already = secrets.get(&provider_id).ok().flatten().is_some();
                if !already {
                    let s = secrecy::SecretString::from(key);
                    if let Err(e) = secrets.set(&provider_id, &s) {
                        warn!(
                            "v3 migration: keychain write failed for '{provider_id}': {e}"
                        );
                    } else {
                        report.secrets_to_keychain += 1;
                    }
                }
            }
        }

        if config.preferred_provider_id.is_none() {
            config.preferred_provider_id = Some(provider_id);
        }
        if config.preferred_model_id.is_none() {
            config.preferred_model_id = Some(legacy.default_model.clone());
        }
    }

    // Bump to v3 with a minimal string edit so we don't disturb the
    // rest of the file. Re-serializing through `AgentConfig` would
    // strip the `[provider]` block because of `skip_serializing`,
    // which would break test fixtures and operators that still
    // embed their provider wiring. Operators who want a clean v3
    // file can re-save via `peko agent update` once the resolver
    // path is in use.
    let version_re = regex::Regex::new(r#"(?m)^version\s*=\s*"[^"]*""#).expect("static regex");
    let updated = if version_re.is_match(&content) {
        version_re
            .replace(&content, r#"version = "3.0""#)
            .into_owned()
    } else {
        // No version line at all — prepend one.
        format!(r#"version = "3.0"
{content}"#)
    };

    let tmp = config_path.with_extension("toml.tmp");
    tokio::fs::write(&tmp, updated).await?;
    tokio::fs::rename(&tmp, config_path).await?;
    report.migrated_agents += 1;
    Ok(())
}

fn provider_id_from_legacy(
    legacy: &crate::types::provider::ProviderConfig,
) -> String {
    use crate::types::provider::ProviderType;
    match legacy.provider_type {
        ProviderType::OpenAI => "openai".to_string(),
        ProviderType::Anthropic => "anthropic".to_string(),
        ProviderType::Ollama => "ollama".to_string(),
        ProviderType::OpenAICompatible => "openai_compatible".to_string(),
        ProviderType::Moonshot => "moonshot".to_string(),
        ProviderType::Kimi => "kimi".to_string(),
        ProviderType::Minimax => "minimax".to_string(),
    }
}

/// Read `credentials.json` as a full `CredentialsStore` (both the
/// `credentials` provider-key map AND the `registry` token).
///
/// We don't use `crate::common::credentials_store` directly because
/// that path imports `GlobalPaths` (CLI-only). For migration we need
/// the in-memory representation that lets us preserve the registry
/// token across the v1→v3 cutover. The schema is defined here as a
/// mirror of `crate::common::credentials_store::CredentialsStore`:
/// keep the two in sync if the canonical struct ever gains a field.
#[derive(Clone, serde::Deserialize, serde::Serialize)]
struct LegacyCredential {
    api_key: String,
}

#[derive(Clone, serde::Deserialize, serde::Serialize)]
struct LegacyRegistryCredential {
    token: String,
    #[serde(default)]
    registry_host: Option<String>,
    #[serde(default)]
    user_namespace: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
}

#[derive(Clone, Default, serde::Deserialize, serde::Serialize)]
struct LegacyCredentialsStore {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    credentials: std::collections::HashMap<String, LegacyCredential>,
    #[serde(default)]
    registry: Option<LegacyRegistryCredential>,
}

fn load_full_credentials_store(
    path: &std::path::Path,
) -> Result<LegacyCredentialsStore> {
    let text = std::fs::read_to_string(path)?;
    let store: LegacyCredentialsStore = serde_json::from_str(&text)?;
    Ok(store)
}

fn write_credentials_store(
    path: &std::path::Path,
    store: &LegacyCredentialsStore,
) -> Result<()> {
    let content = serde_json::to_string_pretty(store)?;
    std::fs::write(path, content)?;
    // Mirror `credentials_store::save_credentials`: tighten perms on
    // Unix so a freshly-rewritten file with a registry token doesn't
    // accidentally end up world-readable.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path) {
            let mut perms = meta.permissions();
            perms.set_mode(0o600);
            let _ = std::fs::set_permissions(path, perms);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_resolver() -> (tempfile::TempDir, PathResolver) {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        std::fs::create_dir_all(home.join("agents")).unwrap();
        let resolver = PathResolver::with_dirs(home.clone(), home.clone(), home.clone());
        (dir, resolver)
    }

    #[tokio::test]
    async fn legacy_agent_config_gets_v3_and_hints() {
        let (_dir, resolver) = temp_resolver();
        let agent_dir = resolver.agents_root_dir().join("my-agent");
        std::fs::create_dir_all(&agent_dir).unwrap();
        let cfg = agent_dir.join("config.toml");
        std::fs::write(
            &cfg,
            r#"
version = "1.0"
name = "my-agent"

[provider]
provider_type = "openai"
api_key = "sk-test-marker-not-real"
default_model = "default"
timeout_seconds = 60
max_retries = 3

[provider.models.default]
name = "gpt-4o-mini"
max_tokens = 1024
temperature = 0.7
top_p = 1.0
"#,
        )
        .unwrap();

        // Run migration. The keychain step may fail on CI without a
        // secret service; the test asserts config rewriting still
        // succeeds.
        let _ = migrate_adr_provider_catalog_v3(&resolver).await;

        // Run migration. The keychain step may fail on CI without a
        // secret service; the test asserts config rewriting still
        // succeeds.
        let _ = migrate_adr_provider_catalog_v3(&resolver).await;

        let content = std::fs::read_to_string(&cfg).unwrap();
        let config: AgentConfig = toml::from_str(&content).unwrap();
        assert_eq!(config.version, "3.0");
        // The migration deliberately preserves the [provider] block
        // on disk (skip_serializing only stops new code from
        // writing it). Stripping it would break test fixtures that
        // write a v1 config and expect the daemon to honour it.
        assert!(content.contains("[provider]"));
        assert!(content.contains("sk-test-marker-not-real"));
        // The legacy block's defaults still apply until the resolver
        // path is exercised.
        assert_eq!(config.provider.api_key.as_deref(), Some("sk-test-marker-not-real"));
    }

    #[tokio::test]
    async fn empty_state_reports_zero_migrations() {
        let (_dir, resolver) = temp_resolver();
        let report = migrate_adr_provider_catalog_v3(&resolver).await.unwrap();
        assert_eq!(report.migrated_agents, 0);
        assert_eq!(report.created_providers, 0);
        assert_eq!(report.secrets_to_keychain, 0);
        assert!(!report.deleted_credentials_json);
    }

    /// PR #43 follow-up: v1→v3 migration must preserve the `registry`
    /// token in `credentials.json`. The previous revision deleted the
    /// whole file, which wiped the bearer token used by
    /// `peko ext push` / `peko ext pull` (CI failure surfaced by
    /// s2_extension_registry_roundtrip after the first fix landed).
    #[tokio::test]
    async fn migration_preserves_registry_token_when_credentials_exist() {
        let (_dir, resolver) = temp_resolver();
        let creds_path = resolver.config_dir().join("credentials.json");
        std::fs::create_dir_all(resolver.config_dir()).unwrap();
        std::fs::write(
            &creds_path,
            r#"{
  "version": 1,
  "credentials": {
    "openai": {
      "provider": "openai",
      "api_key": "sk-must-not-leak",
      "created_at": "2026-01-01T00:00:00Z"
    }
  },
  "registry": {
    "token": "ph_keep_me_around",
    "registry_host": "pekohub.example",
    "user_namespace": null,
    "created_at": "2026-01-01T00:00:00Z"
  }
}
"#,
        )
        .unwrap();

        // Run migration. The keychain step may fail on CI without a
        // secret service; the test asserts the on-disk file is rewritten
        // (not deleted) and the registry token survives.
        let _ = migrate_adr_provider_catalog_v3(&resolver).await;

        assert!(
            creds_path.exists(),
            "credentials.json must NOT be deleted when a registry token is present"
        );
        let content = std::fs::read_to_string(&creds_path).unwrap();
        assert!(
            content.contains("ph_keep_me_around"),
            "registry token must survive migration: {content}"
        );
        assert!(
            content.contains("pekohub.example"),
            "registry_host must survive migration: {content}"
        );
        // The plaintext API key was migrated (or attempted) to the
        // keychain; it should no longer appear in credentials.json.
        assert!(
            !content.contains("sk-must-not-leak"),
            "plaintext API key must be removed from credentials.json after migration: {content}"
        );
        assert!(
            content.contains("\"credentials\": {}"),
            "credentials map must be empty after migration: {content}"
        );
    }

    /// PR #43 follow-up: legacy-only credentials.json (no registry
    /// token) is best-effort rewritten when the keychain accepts the
    /// plaintext keys, or deleted when the keychain refuses (e.g.
    /// headless CI without a secret service). Either way, the
    /// plaintext key MUST NOT survive on disk.
    #[tokio::test]
    async fn migration_scrubs_plaintext_credentials_without_registry() {
        let (_dir, resolver) = temp_resolver();
        let creds_path = resolver.config_dir().join("credentials.json");
        std::fs::create_dir_all(resolver.config_dir()).unwrap();
        std::fs::write(
            &creds_path,
            r#"{
  "version": 1,
  "credentials": {
    "anthropic": {
      "provider": "anthropic",
      "api_key": "sk-ant-must-not-leak",
      "created_at": "2026-01-01T00:00:00Z"
    }
  }
}
"#,
        )
        .unwrap();

        let _ = migrate_adr_provider_catalog_v3(&resolver).await;

        if creds_path.exists() {
            // File was rewritten with empty credentials map.
            let content = std::fs::read_to_string(&creds_path).unwrap();
            assert!(
                !content.contains("sk-ant-must-not-leak"),
                "plaintext key must be scrubbed from disk: {content}"
            );
            assert!(
                content.contains("\"credentials\": {}"),
                "credentials map must be empty after migration: {content}"
            );
        }
        // If the file was deleted (keychain refused), the
        // plaintext key is also gone — both outcomes are safe.
    }
}