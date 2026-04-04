//! Configuration cache wrapper
//!
//! Provides in-memory caching for agent configurations with thread-safe
//! access via RwLock.

use super::entry::AgentConfigEntry;
use std::collections::HashMap;
use tokio::sync::RwLock;
use tracing::debug;

/// Thread-safe configuration cache
///
/// Uses a HashMap keyed by `"{team}/{agent}"` format, wrapped in an async RwLock
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
    pub fn new() -> Self {
        Self {
            cache: RwLock::new(HashMap::new()),
        }
    }

    /// Generate cache key from team and agent name
    pub fn cache_key(team: &str, agent: &str) -> String {
        format!("{}/{}", team, agent)
    }

    /// Get an entry from cache
    pub async fn get(&self, team: &str, agent: &str) -> Option<AgentConfigEntry> {
        let key = Self::cache_key(team, agent);
        let cache = self.cache.read().await;
        cache.get(&key).cloned()
    }

    /// Insert an entry into cache
    pub async fn insert(&self, entry: &AgentConfigEntry) {
        let key = Self::cache_key(&entry.team, &entry.name);
        let mut cache = self.cache.write().await;
        cache.insert(key, entry.clone());
        debug!("Cached config for agent '{}' in team '{}'", entry.name, entry.team);
    }

    /// Remove an entry from cache
    pub async fn remove(&self, team: &str, agent: &str) {
        let key = Self::cache_key(team, agent);
        let mut cache = self.cache.write().await;
        cache.remove(&key);
        debug!("Removed cache for agent '{}' in team '{}'", agent, team);
    }

    /// Clear all entries from cache
    pub async fn clear(&self) {
        let mut cache = self.cache.write().await;
        cache.clear();
        debug!("Configuration cache cleared");
    }

    /// Get all cached entries for a team
    pub async fn list_by_team(&self, team: &str) -> Vec<AgentConfigEntry> {
        let prefix = format!("{}/", team);
        let cache = self.cache.read().await;
        cache
            .values()
            .filter(|e| e.team == team)
            .cloned()
            .collect()
    }

    /// Get all cached entries
    pub async fn list_all(&self) -> Vec<AgentConfigEntry> {
        let cache = self.cache.read().await;
        cache.values().cloned().collect()
    }

    /// Check if an entry exists in cache
    pub async fn contains(&self, team: &str, agent: &str) -> bool {
        let key = Self::cache_key(team, agent);
        let cache = self.cache.read().await;
        cache.contains_key(&key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::agent::AgentConfig;
    use std::path::PathBuf;

    #[tokio::test]
    async fn test_cache_key() {
        assert_eq!(ConfigCache::cache_key("team1", "agent1"), "team1/agent1");
        assert_eq!(ConfigCache::cache_key("default", "myagent"), "default/myagent");
    }

    #[tokio::test]
    async fn test_cache_insert_get() {
        let cache = ConfigCache::new();
        let entry = AgentConfigEntry {
            name: "test-agent".to_string(),
            team: "default".to_string(),
            config: AgentConfig::default(),
            config_path: PathBuf::from("/path/to/config.toml"),
            source: None,
            registered_at: None,
            updated_at: None,
        };

        cache.insert(&entry).await;
        let retrieved = cache.get("default", "test-agent").await;
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().name, "test-agent");
    }

    #[tokio::test]
    async fn test_cache_remove() {
        let cache = ConfigCache::new();
        let entry = AgentConfigEntry {
            name: "test-agent".to_string(),
            team: "default".to_string(),
            config: AgentConfig::default(),
            config_path: PathBuf::from("/path/to/config.toml"),
            source: None,
            registered_at: None,
            updated_at: None,
        };

        cache.insert(&entry).await;
        cache.remove("default", "test-agent").await;
        let retrieved = cache.get("default", "test-agent").await;
        assert!(retrieved.is_none());
    }

    #[tokio::test]
    async fn test_cache_clear() {
        let cache = ConfigCache::new();
        for i in 0..3 {
            let entry = AgentConfigEntry {
                name: format!("agent-{}", i),
                team: "default".to_string(),
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
