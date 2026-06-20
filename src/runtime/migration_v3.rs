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
    let creds_path = resolver.config_dir().join("credentials.json");
    if creds_path.exists() {
        match load_legacy_credentials(&creds_path) {
            Ok(legacy) => {
                for (provider_id, api_key) in &legacy {
                    let already = secrets.get(provider_id).ok().flatten().is_some();
                    if !already && !api_key.is_empty() {
                        let s = secrecy::SecretString::from(api_key.clone());
                        if let Err(e) = secrets.set(provider_id, &s) {
                            warn!(
                                "v3 migration: failed to write key for '{provider_id}' to keychain: {e}"
                            );
                        } else {
                            report.secrets_to_keychain += 1;
                        }
                    }
                }
                if let Err(e) = std::fs::remove_file(&creds_path) {
                    warn!("v3 migration: failed to delete legacy credentials.json: {e}");
                } else {
                    report.deleted_credentials_json = true;
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

    // Bump to v3.
    config.version = "3.0".to_string();

    let updated = toml::to_string_pretty(&config)?;
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

/// Read `credentials.json` as `(provider_id, api_key)` pairs.
///
/// We don't use `crate::common::credentials_store` directly because
/// that path imports `GlobalPaths` (CLI-only). For migration we
/// only need the literal values.
fn load_legacy_credentials(
    path: &std::path::Path,
) -> Result<Vec<(String, String)>> {
    let text = std::fs::read_to_string(path)?;
    let value: serde_json::Value = serde_json::from_str(&text)?;
    let credentials = value
        .get("credentials")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    let mut out = Vec::with_capacity(credentials.len());
    for (provider_id, entry) in credentials {
        let api_key = entry
            .get("api_key")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        out.push((provider_id, api_key));
    }
    Ok(out)
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

        let content = std::fs::read_to_string(&cfg).unwrap();
        let config: AgentConfig = toml::from_str(&content).unwrap();
        assert_eq!(config.version, "3.0");
        assert_eq!(config.preferred_provider_id.as_deref(), Some("openai"));
        assert_eq!(config.preferred_model_id.as_deref(), Some("default"));
        assert!(!content.contains("[provider]"));
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
}