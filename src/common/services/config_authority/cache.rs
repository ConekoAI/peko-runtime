//! Configuration cache wrapper
//!
//! Provides in-memory caching for agent configurations with thread-safe
//! access via `RwLock`.

use super::entry::AgentConfigEntry;
use std::collections::HashMap;
use tokio::sync::RwLock;
use tracing::debug;

/// Thread-safe configuration cache
///
/// Uses a `HashMap` keyed by agent name, wrapped in an async `RwLock`
/// to allow concurrent reads with exclusive writes.
#[derive(Debug)]
pub struct ConfigCache {
    cache: RwLock<HashMap<String, AgentConfigEntry>>,
}

impl Default for ConfigCache {
    fn default() -> Self {
        Self::new()
    }
}

impl ConfigCache {
    /// Create a new empty cache
    #[must_use]
    pub fn new() -> Self {
        Self {
            cache: RwLock::new(HashMap::new()),
        }
    }

    /// Get an entry from cache
    pub async fn get(&self, agent: &str) -> Option<AgentConfigEntry> {
        let cache = self.cache.read().await;
        cache.get(agent).cloned()
    }

    /// Insert an entry into cache
    pub async fn insert(&self, entry: &AgentConfigEntry) {
        let mut cache = self.cache.write().await;
        cache.insert(entry.name.clone(), entry.clone());
        debug!("Cached config for agent '{}'", entry.name);
    }

    /// Remove an entry from cache
    pub async fn remove(&self, agent: &str) {
        let mut cache = self.cache.write().await;
        cache.remove(agent);
        debug!("Removed cache for agent '{}'", agent);
    }

    /// Remove an entry from cache (synchronous version for sync contexts)
    pub fn remove_sync(&self, agent: &str) {
        // Use try_write to avoid blocking; if lock is contended, skip
        if let Ok(mut cache) = self.cache.try_write() {
            cache.remove(agent);
            debug!("Removed cache for agent '{}'", agent);
        } else {
            debug!("Cache lock contested, skipping invalidation for '{}'", agent);
        }
    }

    /// Clear all entries from cache
    pub async fn clear(&self) {
        let mut cache = self.cache.write().await;
        cache.clear();
        debug!("Configuration cache cleared");
    }

    /// Get all cached entries for a team
    ///
    /// Currently returns all entries (membership filtering will be added later).
    pub async fn list_by_team(&self, _team: &str) -> Vec<AgentConfigEntry> {
        let cache = self.cache.read().await;
        cache.values().cloned().collect()
    }

    /// Get all cached entries
    pub async fn list_all(&self) -> Vec<AgentConfigEntry> {
        let cache = self.cache.read().await;
        cache.values().cloned().collect()
    }

    /// Check if an entry exists in cache
    pub async fn contains(&self, agent: &str) -> bool {
        let cache = self.cache.read().await;
        cache.contains_key(agent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::agent::AgentConfig;
    use std::path::PathBuf;

    #[tokio::test]
    async fn test_cache_insert_get() {
        let cache = ConfigCache::new();
        let entry = AgentConfigEntry {
            name: "test-agent".to_string(),
            config: AgentConfig::default(),
            config_path: PathBuf::from("/path/to/config.toml"),
            source: None,
            registered_at: None,
            updated_at: None,
        };

        cache.insert(&entry).await;
        let retrieved = cache.get("test-agent").await;
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().name, "test-agent");
    }

    #[tokio::test]
    async fn test_cache_remove() {
        let cache = ConfigCache::new();
        let entry = AgentConfigEntry {
            name: "test-agent".to_string(),
            config: AgentConfig::default(),
            config_path: PathBuf::from("/path/to/config.toml"),
            source: None,
            registered_at: None,
            updated_at: None,
        };

        cache.insert(&entry).await;
        cache.remove("test-agent").await;
        let retrieved = cache.get("test-agent").await;
        assert!(retrieved.is_none());
    }

    #[tokio::test]
    async fn test_cache_clear() {
        let cache = ConfigCache::new();
        for i in 0..3 {
            let entry = AgentConfigEntry {
                name: format!("agent-{i}"),
                config: AgentConfig::default(),
                config_path: PathBuf::from("/path/to/config.toml"),
                source: None,
                registered_at: None,
                updated_at: None,
            };
            cache.insert(&entry).await;
        }

        cache.clear().await;
        let all = cache.list_all().await;
        assert!(all.is_empty());
    }
}
