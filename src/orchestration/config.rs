//! Orchestration layer configuration
//!
//! Configuration for event routing, file watching, and webhook server.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Orchestration layer configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestrationConfig {
    /// Enable orchestration layer
    pub enabled: bool,
    /// Webhook server configuration
    pub webhook: WebhookConfig,
    /// File watcher configuration
    pub file_watcher: FileWatcherConfig,
    /// Event router configuration
    pub router: RouterConfig,
}

impl Default for OrchestrationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            webhook: WebhookConfig::default(),
            file_watcher: FileWatcherConfig::default(),
            router: RouterConfig::default(),
        }
    }
}

/// Webhook server configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookConfig {
    /// Enable webhook server
    pub enabled: bool,
    /// Port to listen on
    pub port: u16,
    /// Bind address
    pub bind_address: String,
    /// Registered webhook routes
    pub routes: Vec<WebhookRouteConfig>,
}

impl Default for WebhookConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            port: 8080,
            bind_address: "0.0.0.0".to_string(),
            routes: Vec::new(),
        }
    }
}

/// Individual webhook route configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookRouteConfig {
    /// Route path (e.g., "/github")
    pub path: String,
    /// Agent to invoke
    pub agent_id: String,
    /// Source identifier
    pub source: String,
    /// Optional secret for HMAC verification
    pub secret: Option<String>,
}

/// File watcher configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileWatcherConfig {
    /// Enable file watcher
    pub enabled: bool,
    /// Watch configurations
    pub watches: Vec<FileWatchConfig>,
}

impl Default for FileWatcherConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            watches: Vec::new(),
        }
    }
}

/// Individual file watch configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileWatchConfig {
    /// Path to watch
    pub path: PathBuf,
    /// Agent to invoke on changes
    pub agent_id: String,
    /// File pattern filter (glob)
    pub filter: Option<String>,
    /// Watch recursively
    pub recursive: bool,
    /// Debounce duration in milliseconds
    pub debounce_ms: u64,
}

impl Default for FileWatchConfig {
    fn default() -> Self {
        Self {
            path: PathBuf::from("."),
            agent_id: "default".to_string(),
            filter: None,
            recursive: true,
            debounce_ms: 1000,
        }
    }
}

/// Event router configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouterConfig {
    /// Maximum event history size
    pub max_history: usize,
    /// Enable event logging
    pub log_events: bool,
}

impl Default for RouterConfig {
    fn default() -> Self {
        Self {
            max_history: 1000,
            log_events: true,
        }
    }
}

impl OrchestrationConfig {
    /// Load from TOML string
    pub fn from_toml(toml_str: &str) -> anyhow::Result<Self> {
        let config: Self = toml::from_str(toml_str)?;
        Ok(config)
    }

    /// Convert to TOML string
    pub fn to_toml(&self) -> anyhow::Result<String> {
        let toml = toml::to_string_pretty(self)?;
        Ok(toml)
    }

    /// Add a webhook route
    pub fn add_webhook_route(&mut self, route: WebhookRouteConfig) {
        self.webhook.routes.push(route);
    }

    /// Add a file watch
    pub fn add_file_watch(&mut self, watch: FileWatchConfig) {
        self.file_watcher.watches.push(watch);
    }

    /// Validate the configuration
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.webhook.enabled {
            if self.webhook.port == 0 {
                return Err(anyhow::anyhow!("Webhook port cannot be 0"));
            }
            for route in &self.webhook.routes {
                if route.path.is_empty() {
                    return Err(anyhow::anyhow!("Webhook route path cannot be empty"));
                }
                if route.agent_id.is_empty() {
                    return Err(anyhow::anyhow!("Webhook route agent_id cannot be empty"));
                }
            }
        }

        if self.file_watcher.enabled {
            for watch in &self.file_watcher.watches {
                if watch.path.as_os_str().is_empty() {
                    return Err(anyhow::anyhow!("File watch path cannot be empty"));
                }
                if watch.agent_id.is_empty() {
                    return Err(anyhow::anyhow!("File watch agent_id cannot be empty"));
                }
            }
        }

        Ok(())
    }
}

/// Builder for OrchestrationConfig
pub struct OrchestrationConfigBuilder {
    config: OrchestrationConfig,
}

impl OrchestrationConfigBuilder {
    /// Create a new builder with defaults
    pub fn new() -> Self {
        Self {
            config: OrchestrationConfig::default(),
        }
    }

    /// Enable/disable orchestration
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.config.enabled = enabled;
        self
    }

    /// Enable webhook server
    pub fn with_webhook(mut self, port: u16) -> Self {
        self.config.webhook.enabled = true;
        self.config.webhook.port = port;
        self
    }

    /// Add webhook route
    pub fn add_webhook_route(
        mut self,
        path: impl Into<String>,
        agent_id: impl Into<String>,
    ) -> Self {
        self.config.webhook.routes.push(WebhookRouteConfig {
            path: path.into(),
            agent_id: agent_id.into(),
            source: "webhook".to_string(),
            secret: None,
        });
        self
    }

    /// Enable file watcher
    pub fn with_file_watcher(mut self) -> Self {
        self.config.file_watcher.enabled = true;
        self
    }

    /// Add file watch
    pub fn add_file_watch(
        mut self,
        path: impl Into<PathBuf>,
        agent_id: impl Into<String>,
    ) -> Self {
        self.config.file_watcher.watches.push(FileWatchConfig {
            path: path.into(),
            agent_id: agent_id.into(),
            filter: None,
            recursive: true,
            debounce_ms: 1000,
        });
        self
    }

    /// Set router config
    pub fn with_router_config(mut self, config: RouterConfig) -> Self {
        self.config.router = config;
        self
    }

    /// Build the configuration
    pub fn build(self) -> OrchestrationConfig {
        self.config
    }
}

impl Default for OrchestrationConfigBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = OrchestrationConfig::default();
        assert!(config.enabled);
        assert!(!config.webhook.enabled);
        assert!(!config.file_watcher.enabled);
        assert_eq!(config.router.max_history, 1000);
    }

    #[test]
    fn test_config_builder() {
        let config = OrchestrationConfigBuilder::new()
            .with_webhook(3000)
            .add_webhook_route("/github", "github-agent")
            .with_file_watcher()
            .add_file_watch("/tmp", "file-agent")
            .build();

        assert!(config.enabled);
        assert!(config.webhook.enabled);
        assert_eq!(config.webhook.port, 3000);
        assert_eq!(config.webhook.routes.len(), 1);
        assert_eq!(config.webhook.routes[0].path, "/github");
        assert!(config.file_watcher.enabled);
        assert_eq!(config.file_watcher.watches.len(), 1);
    }

    #[test]
    fn test_config_toml_roundtrip() {
        let config = OrchestrationConfigBuilder::new()
            .enabled(true)
            .with_webhook(8080)
            .add_webhook_route("/slack", "slack-agent")
            .build();

        let toml_str = config.to_toml().unwrap();
        let parsed = OrchestrationConfig::from_toml(&toml_str).unwrap();

        assert_eq!(parsed.enabled, config.enabled);
        assert_eq!(parsed.webhook.port, config.webhook.port);
        assert_eq!(parsed.webhook.routes.len(), config.webhook.routes.len());
    }

    #[test]
    fn test_validation_webhook_empty_path() {
        let mut config = OrchestrationConfig::default();
        config.webhook.enabled = true;
        config.webhook.routes.push(WebhookRouteConfig {
            path: "".to_string(),
            agent_id: "test".to_string(),
            source: "test".to_string(),
            secret: None,
        });

        assert!(config.validate().is_err());
    }

    #[test]
    fn test_validation_webhook_empty_agent() {
        let mut config = OrchestrationConfig::default();
        config.webhook.enabled = true;
        config.webhook.routes.push(WebhookRouteConfig {
            path: "/test".to_string(),
            agent_id: "".to_string(),
            source: "test".to_string(),
            secret: None,
        });

        assert!(config.validate().is_err());
    }

    #[test]
    fn test_validation_valid_config() {
        let config = OrchestrationConfigBuilder::new()
            .with_webhook(8080)
            .add_webhook_route("/test", "test-agent")
            .with_file_watcher()
            .add_file_watch("/tmp", "file-agent")
            .build();

        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_webhook_route_config_with_secret() {
        let route = WebhookRouteConfig {
            path: "/github".to_string(),
            agent_id: "github-agent".to_string(),
            source: "github".to_string(),
            secret: Some("my-secret".to_string()),
        };

        assert_eq!(route.path, "/github");
        assert_eq!(route.secret, Some("my-secret".to_string()));
    }

    #[test]
    fn test_file_watch_config_defaults() {
        let watch = FileWatchConfig::default();
        assert_eq!(watch.path, PathBuf::from("."));
        assert_eq!(watch.agent_id, "default");
        assert!(watch.recursive);
        assert_eq!(watch.debounce_ms, 1000);
    }

    #[test]
    fn test_router_config() {
        let config = RouterConfig {
            max_history: 500,
            log_events: false,
        };

        assert_eq!(config.max_history, 500);
        assert!(!config.log_events);
    }
}
