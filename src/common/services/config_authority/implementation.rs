//! `ConfigAuthority` implementation
//!
//! The main implementation of the `ConfigAuthority` trait.

use super::authority_trait::{ConfigAuthority, ConfigError, ConfigResult};
use super::cache::ConfigCache;
use super::entry::{AgentConfigEntry, ConfigSource};
use super::io::ConfigIo;
use crate::common::paths::PathResolver;
use crate::types::agent::AgentConfig;
use async_trait::async_trait;
use chrono::Utc;
use std::path::PathBuf;
use tracing::{debug, info, warn};

/// Main implementation of `ConfigAuthority`
///
/// This is the single canonical implementation for agent configuration management.
/// It coordinates between:
/// - `ConfigCache`: In-memory caching
/// - `ConfigIo`: TOML file operations
#[derive(Debug)]
pub struct ConfigAuthorityImpl {
    path_resolver: PathResolver,
    cache: ConfigCache,
    io: ConfigIo,
}

impl std::fmt::Display for ConfigAuthorityImpl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ConfigAuthorityImpl")
    }
}

impl ConfigAuthorityImpl {
    /// Create a new `ConfigAuthorityImpl`
    #[must_use]
    pub fn new(path_resolver: PathResolver) -> Self {
        Self {
            path_resolver,
            cache: ConfigCache::new(),
            io: ConfigIo::new(),
        }
    }

    /// Create from existing components (for testing)
    #[allow(dead_code)]
    fn with_components(
        path_resolver: PathResolver,
        cache: ConfigCache,
        io: ConfigIo,
    ) -> Self {
        Self {
            path_resolver,
            cache,
            io,
        }
    }

    /// Get the canonical config path for an agent
    pub fn config_path(&self, agent_name: &str) -> PathBuf {
        self.path_resolver.agent_config(agent_name)
    }
}

#[async_trait]
impl ConfigAuthority for ConfigAuthorityImpl {
    async fn get(&self, agent_name: &str) -> ConfigResult<Option<AgentConfigEntry>> {
        // Check cache first
        if let Some(entry) = self.cache.get(agent_name).await {
            debug!("Cache hit for agent '{}'", agent_name);
            return Ok(Some(entry));
        }

        // Load from disk
        let config_path = self.config_path(agent_name);
        if !config_path.exists() {
            return Ok(None);
        }

        let config = self.io.load_toml(&config_path).await.map_err(|e| {
            ConfigError::Other(format!(
                "Failed to load config from {}: {}",
                config_path.display(),
                e
            ))
        })?;

        // v3: API keys live in the OS keychain (catalog + SecretStore),
        // not on the agent config. The config has no `provider`
        // block to backfill.

        let entry = AgentConfigEntry {
            name: agent_name.to_string(),
            config,
            config_path,
            source: Some(ConfigSource::Direct {
                reason: "file".to_string(),
            }),
            registered_at: Some(Utc::now()),
            updated_at: Some(Utc::now()),
        };

        // Cache the result
        self.cache.insert(&entry).await;

        Ok(Some(entry))
    }

    async fn save(&self, agent_name: &str, config: &AgentConfig) -> ConfigResult<PathBuf> {
        let config_path = self.config_path(agent_name);

        // Save to TOML
        self.io
            .save_toml(&config_path, config)
            .await
            .map_err(|e| ConfigError::Other(format!("Failed to save config: {e}")))?;

        info!(
            "Saved agent '{}' config to {}",
            agent_name,
            config_path.display()
        );

        // Create entry and cache it
        let entry = AgentConfigEntry {
            name: agent_name.to_string(),
            config: config.clone(),
            config_path: config_path.clone(),
            source: Some(ConfigSource::Direct {
                reason: "saved".to_string(),
            }),
            registered_at: Some(Utc::now()),
            updated_at: Some(Utc::now()),
        };

        self.cache.insert(&entry).await;

        Ok(config_path)
    }

    async fn exists(&self, agent_name: &str) -> ConfigResult<bool> {
        match self.get(agent_name).await {
            Ok(Some(_)) => Ok(true),
            Ok(None) => {
                // Double-check by looking at file system
                let config_path = self.config_path(agent_name);
                Ok(config_path.exists())
            }
            Err(e) => Err(e),
        }
    }

    async fn list_in_team(&self, _team: &str) -> ConfigResult<Vec<AgentConfigEntry>> {
        // In the new layout, agents are top-level. For now, list all agents.
        // Membership filtering will be added later.
        self.list_all().await
    }

    async fn list_all(&self) -> ConfigResult<Vec<AgentConfigEntry>> {
        let mut all_agents = Vec::new();

        // In the new layout, list all agents from the top-level agents directory
        let agents_dir = self.path_resolver.agents_root_dir();
        if !agents_dir.exists() {
            return Ok(all_agents);
        }

        let mut entries = match tokio::fs::read_dir(&agents_dir).await {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(all_agents),
            Err(e) => return Err(ConfigError::Io(e)),
        };

        while let Some(entry) = entries.next_entry().await.map_err(ConfigError::Io)? {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let agent_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let config_path = self.config_path(agent_name);

            if !config_path.exists() {
                continue;
            }

            match self.io.load_toml(&config_path).await {
                Ok(config) => {
                    all_agents.push(AgentConfigEntry {
                        name: agent_name.to_string(),
                        config,
                        config_path: config_path.clone(),
                        source: Some(ConfigSource::Direct {
                            reason: "file".to_string(),
                        }),
                        registered_at: Some(Utc::now()),
                        updated_at: Some(Utc::now()),
                    });
                }
                Err(e) => {
                    warn!("Failed to load config for agent '{}': {}", agent_name, e);
                }
            }
        }

        Ok(all_agents)
    }

    async fn delete(&self, agent_name: &str) -> ConfigResult<bool> {
        let config_path = self.config_path(agent_name);

        if !config_path.exists() {
            return Ok(false);
        }

        // Remove from cache
        self.cache.remove(agent_name).await;

        // Delete file
        self.io
            .delete(&config_path)
            .await
            .map_err(|e| ConfigError::Other(format!("Failed to delete config: {e}")))?;

        info!("Deleted agent '{}' config", agent_name);
        Ok(true)
    }

    async fn clear_cache(&self) {
        self.cache.clear().await;
        debug!("Agent configuration cache cleared");
    }

    async fn invalidate_cache(&self, agent_name: &str) {
        self.cache.remove(agent_name).await;
        debug!("Cache invalidated for agent '{}'", agent_name);
    }

    fn path_resolver(&self) -> &PathResolver {
        &self.path_resolver
    }
}

impl ConfigAuthorityImpl {
    /// Enable an extension in an agent's config whitelist (synchronous).
    ///
    /// **Read-modify-write via `toml::Value` (PR #43 follow-up).** The
    /// previous implementation parsed into `AgentConfig` and re-serialized
    /// with `toml::to_string_pretty(&config)`. In v3, `AgentConfig::provider`
    /// is `skip_serializing`, so any code path that round-trips through
    /// `Serialize` silently drops the legacy `[provider]` block on disk.
    /// That breaks `init_provider_legacy` (which reads the in-config
    /// `api_key` / `base_url` / `provider_type` when the catalog resolver
    /// is not in use), producing empty stdout from `peko send`. Using a
    /// targeted `toml::Value` edit preserves the legacy block during the
    /// v1→v3 migration window (see `runtime::migration_v3`).
    pub fn enable_tool_sync(&self, agent_name: &str, tool_name: &str) -> anyhow::Result<()> {
        let config_path = self.config_path(agent_name);
        if !config_path.exists() {
            anyhow::bail!("Agent '{agent_name}' not found");
        }

        let content = std::fs::read_to_string(&config_path)?;
        update_extensions_enabled(&content, |existing| {
            if existing.iter().any(|e| e.eq_ignore_ascii_case(tool_name)) {
                None
            } else {
                let mut next = existing.to_vec();
                next.push(tool_name.to_string());
                Some(next)
            }
        })
        .and_then(|updated| std::fs::write(&config_path, updated).map_err(Into::into))?;

        // Invalidate cache so subsequent reads pick up the change
        self.cache.remove_sync(agent_name);

        Ok(())
    }

    /// Disable an extension in an agent's config whitelist (synchronous).
    ///
    /// See [`enable_tool_sync`] for why this uses a `toml::Value`
    /// targeted edit instead of a `Serialize` round-trip.
    pub fn disable_tool_sync(&self, agent_name: &str, tool_name: &str) -> anyhow::Result<()> {
        let config_path = self.config_path(agent_name);
        if !config_path.exists() {
            anyhow::bail!("Agent '{agent_name}' not found");
        }

        let content = std::fs::read_to_string(&config_path)?;
        update_extensions_enabled(&content, |existing| {
            let filtered: Vec<String> = existing
                .iter()
                .filter(|e| !e.eq_ignore_ascii_case(tool_name))
                .cloned()
                .collect();
            if filtered.len() == existing.len() {
                None
            } else {
                Some(filtered)
            }
        })
        .and_then(|updated| std::fs::write(&config_path, updated).map_err(Into::into))?;

        // Invalidate cache so subsequent reads pick up the change
        self.cache.remove_sync(agent_name);

        Ok(())
    }

    /// Enable a tool for all agents
    pub fn enable_tool_for_team(&self, _team: &str, tool_name: &str) -> anyhow::Result<usize> {
        let agents_dir = self.path_resolver.agents_root_dir();
        if !agents_dir.exists() {
            anyhow::bail!("No agents directory found");
        }

        let mut updated_count = 0;
        for entry in std::fs::read_dir(&agents_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }

            let agent_name = entry.file_name().to_string_lossy().to_string();
            let config_path = self.config_path(&agent_name);
            if config_path.exists() {
                self.enable_tool_sync(&agent_name, tool_name)?;
                updated_count += 1;
            }
        }

        Ok(updated_count)
    }

    /// Disable a tool for all agents
    pub fn disable_tool_for_team(&self, _team: &str, tool_name: &str) -> anyhow::Result<usize> {
        let agents_dir = self.path_resolver.agents_root_dir();
        if !agents_dir.exists() {
            anyhow::bail!("No agents directory found");
        }

        let mut updated_count = 0;
        for entry in std::fs::read_dir(&agents_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }

            let agent_name = entry.file_name().to_string_lossy().to_string();
            let config_path = self.config_path(&agent_name);
            if config_path.exists() {
                self.disable_tool_sync(&agent_name, tool_name)?;
                updated_count += 1;
            }
        }

        Ok(updated_count)
    }
}

/// Mutate `extensions.enabled` on an agent's on-disk TOML config
/// without disturbing other fields (notably the legacy `[provider]`
/// block, which v3's `AgentConfig::Serialize` drops because of
/// `skip_serializing`).
///
/// `mutate` receives the current `extensions.enabled` list and returns
/// the new list, or `None` to indicate "no change" (in which case the
/// file is left untouched).
fn update_extensions_enabled(
    content: &str,
    mutate: impl FnOnce(&[String]) -> Option<Vec<String>>,
) -> anyhow::Result<String> {
    let mut root: toml::Value = toml::from_str(content)
        .map_err(|e| anyhow::anyhow!("failed to parse agent config as TOML: {e}"))?;

    let table = root
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("agent config root is not a TOML table"))?;

    // Locate or create the `[extensions]` table.
    let extensions_entry = table
        .entry("extensions".to_string())
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
    let extensions_table = extensions_entry
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("`extensions` is not a TOML table"))?;

    // Read existing enabled list (treat absent as empty).
    let existing: Vec<String> = extensions_table
        .get("enabled")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    let Some(next) = mutate(&existing) else {
        return Ok(content.to_string());
    };

    let mut arr = toml::value::Array::with_capacity(next.len());
    for s in next {
        arr.push(toml::Value::String(s));
    }
    extensions_table.insert("enabled".to_string(), toml::Value::Array(arr));

    Ok(toml::to_string_pretty(&root)?)
}

impl Clone for ConfigAuthorityImpl {
    fn clone(&self) -> Self {
        Self {
            path_resolver: self.path_resolver.clone(),
            cache: ConfigCache::new(), // Fresh cache for cloned instance
            io: self.io.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_resolver(temp_dir: &TempDir) -> PathResolver {
        PathResolver::with_dirs(
            temp_dir.path().to_path_buf(),
            temp_dir.path().join("data"),
            temp_dir.path().join("cache"),
        )
    }

    #[tokio::test]
    async fn test_save_and_get() {
        let temp_dir = TempDir::new().unwrap();
        let resolver = create_test_resolver(&temp_dir);
        let authority = ConfigAuthorityImpl::new(resolver);

        let config = AgentConfig::default();

        // Save
        let path = authority.save("test-agent", &config).await.unwrap();
        assert!(path.exists());

        // Retrieve
        let entry = authority.get("test-agent").await.unwrap();
        assert!(entry.is_some());

        let entry = entry.unwrap();
        assert_eq!(entry.name, "test-agent");
    }

    #[tokio::test]
    async fn test_exists() {
        let temp_dir = TempDir::new().unwrap();
        let resolver = create_test_resolver(&temp_dir);
        let authority = ConfigAuthorityImpl::new(resolver);

        // Non-existent
        assert!(!authority.exists("nonexistent").await.unwrap());

        // Create and check
        let config = AgentConfig::default();
        authority.save("existing", &config).await.unwrap();
        assert!(authority.exists("existing").await.unwrap());
    }

    #[tokio::test]
    async fn test_list_in_team() {
        let temp_dir = TempDir::new().unwrap();
        let resolver = create_test_resolver(&temp_dir);
        let authority = ConfigAuthorityImpl::new(resolver);

        // Initially empty
        let agents = authority.list_in_team("default").await.unwrap();
        assert!(agents.is_empty());

        // Add some agents
        for i in 0..3 {
            let config = AgentConfig::default();
            authority
                .save(&format!("agent-{i}"), &config)
                .await
                .unwrap();
        }

        let agents = authority.list_in_team("default").await.unwrap();
        assert_eq!(agents.len(), 3);
    }

    #[tokio::test]
    async fn test_delete() {
        let temp_dir = TempDir::new().unwrap();
        let resolver = create_test_resolver(&temp_dir);
        let authority = ConfigAuthorityImpl::new(resolver);

        // Create an agent
        let config = AgentConfig::default();
        authority.save("to-delete", &config).await.unwrap();

        // Verify exists
        assert!(authority.exists("to-delete").await.unwrap());

        // Delete
        let deleted = authority.delete("to-delete").await.unwrap();
        assert!(deleted);

        // Verify gone
        assert!(!authority.exists("to-delete").await.unwrap());
    }

    #[tokio::test]
    async fn test_cache_isolation_between_clones() {
        let temp_dir = TempDir::new().unwrap();
        let resolver = create_test_resolver(&temp_dir);
        let authority1 = ConfigAuthorityImpl::new(resolver.clone());
        let authority2 = authority1.clone();

        let config = AgentConfig::default();
        authority1.save("shared-agent", &config).await.unwrap();

        // Both should be able to read
        let entry1 = authority1.get("shared-agent").await;
        let entry2 = authority2.get("shared-agent").await;

        assert!(entry1.is_ok());
        assert!(entry2.is_ok());
    }

    /// PR #43 follow-up: any code path that round-trips `AgentConfig`
    /// through `Serialize` drops the legacy `[provider]` block (v3
    /// marks it `skip_serializing`). `update_extensions_enabled` is the
    /// targeted-edit replacement; it must preserve the block verbatim.
    #[test]
    fn update_extensions_enabled_preserves_legacy_provider_block() {
        let v1_with_provider = r#"version = "1.0"
name = "legacy-agent"

[provider]
provider_type = "openai_compatible"
api_key = "mock-key-must-not-vanish"
base_url = "http://127.0.0.1:18080"
default_model = "default"

[extensions]
enabled = []

[channels]
cli = true
"#;

        let updated = update_extensions_enabled(v1_with_provider, |existing| {
            assert!(existing.is_empty());
            let mut next = Vec::new();
            next.push("calculator-skill".to_string());
            Some(next)
        })
        .expect("update succeeds");

        // The targeted edit must NOT touch the [provider] block.
        assert!(
            updated.contains("[provider]"),
            "[provider] block must survive enable_tool_sync: {updated}"
        );
        assert!(
            updated.contains("api_key = \"mock-key-must-not-vanish\""),
            "literal api_key must survive enable_tool_sync: {updated}"
        );
        assert!(
            updated.contains("base_url = \"http://127.0.0.1:18080\""),
            "base_url must survive enable_tool_sync: {updated}"
        );
        // The new extension must appear in the whitelist.
        assert!(
            updated.contains("\"calculator-skill\""),
            "extension id must be in updated whitelist: {updated}"
        );
        // The legacy empty `enabled = []` is replaced (targeted edit,
        // not whole-config rewrite).
        assert!(
            !updated.contains("enabled = []"),
            "empty enabled list must be replaced, not duplicated: {updated}"
        );
    }

    /// Targeted edit must skip writing when the whitelist is unchanged
    /// (matches the original `enable_tool_sync` / `disable_tool_sync`
    /// short-circuit semantics).
    #[test]
    fn update_extensions_enabled_no_op_when_mutate_returns_none() {
        let content = r#"version = "1.0"
name = "x"

[provider]
api_key = "still-here"

[extensions]
enabled = ["calculator-skill"]
"#;
        let updated = update_extensions_enabled(content, |_existing| None).expect("no-op ok");
        assert_eq!(
            updated, content,
            "no-op mutate should return the original content verbatim"
        );
    }
}
