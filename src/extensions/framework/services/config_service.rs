//! Extension Configuration Service
//!
//! Provides persistent key-value configuration for extensions at global,
//! team, and agent scopes. Stores data as TOML files under the data directory.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Scope for extension configuration
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigScope {
    /// Global scope (applies to all agents/teams)
    Global,
    /// Team scope (applies to all agents in a team)
    Team(String),
    /// Agent scope (applies to a specific agent)
    Agent(String, String),
}

/// Persistent extension configuration storage
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct ExtensionConfigData {
    /// Global settings (apply to all agents/teams)
    #[serde(default)]
    global: HashMap<String, serde_json::Value>,

    /// Per-team settings
    #[serde(default)]
    teams: HashMap<String, HashMap<String, serde_json::Value>>,

    /// Per-agent settings (format: "team/agent")
    #[serde(default)]
    agents: HashMap<String, HashMap<String, serde_json::Value>>,
}

impl ExtensionConfigData {
    fn config_path(data_dir: &Path, extension_id: &str) -> PathBuf {
        data_dir
            .join("extensions")
            .join(extension_id)
            .join("config.toml")
    }

    fn load(data_dir: &Path, extension_id: &str) -> anyhow::Result<Self> {
        let path = Self::config_path(data_dir, extension_id);
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(&path)?;
        let config: Self = toml::from_str(&content)?;
        Ok(config)
    }

    fn save(&self, data_dir: &Path, extension_id: &str) -> anyhow::Result<()> {
        let path = Self::config_path(data_dir, extension_id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = toml::to_string_pretty(self)?;
        std::fs::write(&path, content)?;
        Ok(())
    }

    fn set(
        &mut self,
        team: Option<&str>,
        agent: Option<&str>,
        key: String,
        value: serde_json::Value,
    ) {
        let target = match (team, agent) {
            (Some(_), Some(_)) => {
                let agent_id = agent.unwrap().to_string();
                self.agents.entry(agent_id).or_default()
            }
            (Some(team_id), None) => self.teams.entry(team_id.to_string()).or_default(),
            _ => &mut self.global,
        };
        target.insert(key, value);
    }

    fn unset(&mut self, team: Option<&str>, agent: Option<&str>, key: &str) -> bool {
        match (team, agent) {
            (Some(_), Some(_)) => {
                if let Some(agent_config) = self.agents.get_mut(agent.unwrap()) {
                    agent_config.remove(key).is_some()
                } else {
                    false
                }
            }
            (Some(team_id), None) => {
                if let Some(team_config) = self.teams.get_mut(team_id) {
                    team_config.remove(key).is_some()
                } else {
                    false
                }
            }
            _ => self.global.remove(key).is_some(),
        }
    }

    fn get_scope(
        &self,
        team: Option<&str>,
        agent: Option<&str>,
    ) -> &HashMap<String, serde_json::Value> {
        match (team, agent) {
            (Some(_), Some(a)) => self.agents.get(a).unwrap_or(&self.global),
            (Some(t), None) => self.teams.get(t).unwrap_or(&self.global),
            _ => &self.global,
        }
    }
}

/// Service for managing extension configuration persistence
#[derive(Debug, Clone)]
pub struct ExtensionConfigService {
    data_dir: PathBuf,
}

impl ExtensionConfigService {
    /// Create a new config service with the given data directory
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
        }
    }

    /// Load configuration for an extension
    pub fn load(&self, extension_id: &str) -> anyhow::Result<HashMap<String, serde_json::Value>> {
        let data = ExtensionConfigData::load(&self.data_dir, extension_id)?;
        let mut result = data.global.clone();
        result.extend(data.teams.into_iter().flat_map(|(k, v)| {
            v.into_iter()
                .map(move |(vk, vv)| (format!("team.{k}.{vk}"), vv))
        }));
        result.extend(data.agents.into_iter().flat_map(|(k, v)| {
            v.into_iter()
                .map(move |(vk, vv)| (format!("agent.{k}.{vk}"), vv))
        }));
        Ok(result)
    }

    /// Save configuration for an extension
    pub fn save(&self, extension_id: &str, config: &ExtensionConfigData) -> anyhow::Result<()> {
        config.save(&self.data_dir, extension_id)
    }

    /// Set a configuration value at a specific scope
    pub fn set(
        &self,
        extension_id: &str,
        scope: ConfigScope,
        key: &str,
        value: serde_json::Value,
    ) -> anyhow::Result<()> {
        let mut data = ExtensionConfigData::load(&self.data_dir, extension_id)?;
        let (team, agent) = match &scope {
            ConfigScope::Global => (None, None),
            ConfigScope::Team(t) => (Some(t.as_str()), None),
            ConfigScope::Agent(t, a) => (Some(t.as_str()), Some(format!("{t}/{a}"))),
        };
        data.set(team, agent.as_deref(), key.to_string(), value);
        data.save(&self.data_dir, extension_id)
    }

    /// Unset a configuration key at a specific scope
    ///
    /// Returns `true` if the key was found and removed.
    pub fn unset(&self, extension_id: &str, scope: ConfigScope, key: &str) -> anyhow::Result<bool> {
        let mut data = ExtensionConfigData::load(&self.data_dir, extension_id)?;
        let (team, agent) = match &scope {
            ConfigScope::Global => (None, None),
            ConfigScope::Team(t) => (Some(t.as_str()), None),
            ConfigScope::Agent(t, a) => (Some(t.as_str()), Some(format!("{t}/{a}"))),
        };
        let removed = data.unset(team, agent.as_deref(), key);
        data.save(&self.data_dir, extension_id)?;
        Ok(removed)
    }

    /// Show configuration values at a specific scope
    pub fn show(
        &self,
        extension_id: &str,
        scope: ConfigScope,
    ) -> anyhow::Result<HashMap<String, serde_json::Value>> {
        let data = ExtensionConfigData::load(&self.data_dir, extension_id)?;
        let agent_key = match &scope {
            ConfigScope::Agent(t, a) => Some(format!("{t}/{a}")),
            _ => None,
        };
        let (team, agent) = match &scope {
            ConfigScope::Global => (None, None),
            ConfigScope::Team(t) => (Some(t.as_str()), None),
            ConfigScope::Agent(t, _a) => (Some(t.as_str()), agent_key.as_deref()),
        };
        Ok(data.get_scope(team, agent).clone())
    }

    /// Get inherited global configuration
    pub fn global(&self, extension_id: &str) -> anyhow::Result<HashMap<String, serde_json::Value>> {
        let data = ExtensionConfigData::load(&self.data_dir, extension_id)?;
        Ok(data.global.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_service() -> (TempDir, ExtensionConfigService) {
        let temp = TempDir::new().unwrap();
        let service = ExtensionConfigService::new(temp.path());
        (temp, service)
    }

    #[test]
    fn test_load_empty() {
        let (_temp, service) = create_service();
        let config = service.load("test-ext").unwrap();
        assert!(config.is_empty());
    }

    #[test]
    fn test_set_and_show_global() {
        let (_temp, service) = create_service();
        service
            .set(
                "test-ext",
                ConfigScope::Global,
                "api_key",
                serde_json::json!("secret"),
            )
            .unwrap();

        let config = service.show("test-ext", ConfigScope::Global).unwrap();
        assert_eq!(config.get("api_key").unwrap(), "secret");
    }

    #[test]
    fn test_set_and_show_team() {
        let (_temp, service) = create_service();
        service
            .set(
                "test-ext",
                ConfigScope::Team("myteam".to_string()),
                "endpoint",
                serde_json::json!("https://example.com"),
            )
            .unwrap();

        let config = service
            .show("test-ext", ConfigScope::Team("myteam".to_string()))
            .unwrap();
        assert_eq!(config.get("endpoint").unwrap(), "https://example.com");

        // Global should be empty
        let global = service.show("test-ext", ConfigScope::Global).unwrap();
        assert!(global.is_empty());
    }

    #[test]
    fn test_set_and_show_agent() {
        let (_temp, service) = create_service();
        service
            .set(
                "test-ext",
                ConfigScope::Agent("myteam".to_string(), "myagent".to_string()),
                "timeout",
                serde_json::json!(30),
            )
            .unwrap();

        let config = service
            .show(
                "test-ext",
                ConfigScope::Agent("myteam".to_string(), "myagent".to_string()),
            )
            .unwrap();
        assert_eq!(config.get("timeout").unwrap(), &serde_json::json!(30));
    }

    #[test]
    fn test_unset_existing() {
        let (_temp, service) = create_service();
        service
            .set(
                "test-ext",
                ConfigScope::Global,
                "key",
                serde_json::json!("value"),
            )
            .unwrap();

        let removed = service
            .unset("test-ext", ConfigScope::Global, "key")
            .unwrap();
        assert!(removed);

        let config = service.show("test-ext", ConfigScope::Global).unwrap();
        assert!(config.is_empty());
    }

    #[test]
    fn test_unset_missing() {
        let (_temp, service) = create_service();
        let removed = service
            .unset("test-ext", ConfigScope::Global, "missing")
            .unwrap();
        assert!(!removed);
    }

    #[test]
    fn test_global_inheritance() {
        let (_temp, service) = create_service();
        service
            .set(
                "test-ext",
                ConfigScope::Global,
                "shared",
                serde_json::json!("global_value"),
            )
            .unwrap();
        service
            .set(
                "test-ext",
                ConfigScope::Team("myteam".to_string()),
                "local",
                serde_json::json!("team_value"),
            )
            .unwrap();

        let global = service.global("test-ext").unwrap();
        assert_eq!(global.get("shared").unwrap(), "global_value");
    }
}
