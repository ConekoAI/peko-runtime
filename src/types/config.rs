//! Pekobot global configuration

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Global Pekobot configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PekobotConfig {
    /// Application name
    pub app_name: String,
    /// Storage configuration
    pub storage: StorageConfig,
    /// Network configuration
    pub network: NetworkConfig,
    /// Logging configuration
    pub logging: LogConfig,
    /// Orchestration layer configuration
    pub orchestration: OrchestrationConfig,
}

impl Default for PekobotConfig {
    fn default() -> Self {
        Self {
            app_name: "pekobot".to_string(),
            storage: StorageConfig::default(),
            network: NetworkConfig::default(),
            logging: LogConfig::default(),
            orchestration: OrchestrationConfig::default(),
        }
    }
}

/// Storage configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    /// Storage type: sqlite, memory
    pub storage_type: String,
    /// Database file path (for sqlite)
    pub database_path: PathBuf,
    /// Key storage path
    pub keys_path: PathBuf,
    /// Memory database path
    pub memory_path: PathBuf,
}

impl Default for StorageConfig {
    fn default() -> Self {
        let data_dir = dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("pekobot");

        Self {
            storage_type: "sqlite".to_string(),
            database_path: data_dir.join("pekobot.db"),
            keys_path: data_dir.join("keys"),
            memory_path: data_dir.join("memory.db"),
        }
    }
}

/// Network configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkConfig {
    /// Bind address
    pub bind_address: String,
    /// Port for HTTP API
    pub port: u16,
    /// Enable TLS
    pub tls_enabled: bool,
    /// TLS certificate path
    pub tls_cert_path: Option<PathBuf>,
    /// TLS key path
    pub tls_key_path: Option<PathBuf>,
    /// Allowed CORS origins
    pub cors_origins: Vec<String>,
    /// Request timeout (seconds)
    pub request_timeout_seconds: u64,
    /// Maximum request body size (MB)
    pub max_body_size_mb: usize,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            bind_address: "127.0.0.1".to_string(),
            port: 8080,
            tls_enabled: false,
            tls_cert_path: None,
            tls_key_path: None,
            cors_origins: vec!["*".to_string()],
            request_timeout_seconds: 30,
            max_body_size_mb: 10,
        }
    }
}

/// Logging configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogConfig {
    /// Log level: trace, debug, info, warn, error
    pub level: String,
    /// Log format: json, pretty, compact
    pub format: String,
    /// Log to file
    pub log_file: Option<PathBuf>,
    /// Log to stdout
    pub log_stdout: bool,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
            format: "pretty".to_string(),
            log_file: None,
            log_stdout: true,
        }
    }
}

impl PekobotConfig {
    /// Load configuration from TOML file
    pub fn from_file(path: &std::path::Path) -> anyhow::Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        let config: Self = toml::from_str(&contents)?;
        Ok(config)
    }

    /// Save configuration to TOML file
    pub fn to_file(&self, path: &std::path::Path) -> anyhow::Result<()> {
        let contents = toml::to_string_pretty(self)?;
        std::fs::write(path, contents)?;
        Ok(())
    }

    /// Create default config with data directory
    #[must_use]
    pub fn with_data_dir(data_dir: PathBuf) -> Self {
        Self {
            storage: StorageConfig {
                database_path: data_dir.join("pekobot.db"),
                keys_path: data_dir.join("keys"),
                memory_path: data_dir.join("memory.db"),
                ..StorageConfig::default()
            },
            ..Self::default()
        }
    }

    /// Load orchestration configuration section
    #[must_use]
    pub fn orchestration(&self) -> &OrchestrationConfig {
        &self.orchestration
    }
}

// ============================================================================
// Orchestration Configuration (from former src/orchestration/config.rs)
// ============================================================================

/// Orchestration layer configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestrationConfig {
    /// Enable orchestration layer
    pub enabled: bool,
    /// Traditional path-based webhook server
    pub webhook: WebhookConfig,
    /// Unified external ingress (for `SaaS` integrations)
    pub external_ingress: ExternalIngressConfig,
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
            external_ingress: ExternalIngressConfig::default(),
            file_watcher: FileWatcherConfig::default(),
            router: RouterConfig::default(),
        }
    }
}

impl OrchestrationConfig {
    /// Add a webhook route
    pub fn add_webhook_route(&mut self, route: WebhookRouteConfig) {
        self.webhook.routes.push(route);
    }

    /// Add a file watch
    pub fn add_file_watch(&mut self, watch: FileWatchConfig) {
        self.file_watcher.watches.push(watch);
    }

    /// Add an external source
    pub fn add_external_source(&mut self, source: ExternalSource) {
        self.external_ingress.sources.push(source);
    }

    /// Validate configuration
    pub fn validate(&self) -> anyhow::Result<()> {
        // Basic validation - ensure ports don't conflict
        if self.webhook.enabled && self.external_ingress.enabled
            && self.webhook.port == self.external_ingress.port {
                anyhow::bail!(
                    "Webhook port ({}) conflicts with external ingress port ({})",
                    self.webhook.port,
                    self.external_ingress.port
                );
            }
        Ok(())
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
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FileWatcherConfig {
    /// Enable file watcher
    pub enabled: bool,
    /// Watch configurations
    pub watches: Vec<FileWatchConfig>,
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

/// External source detection method
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SourceDetection {
    /// Match by header name and optional value prefix
    Header {
        name: String,
        value_prefix: Option<String>,
    },
    /// Match by payload field path
    PayloadField { path: String, value: Option<String> },
    /// Match by User-Agent substring
    UserAgent { contains: String },
}

/// External ingress configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalIngressConfig {
    /// Enable external ingress
    pub enabled: bool,
    /// Port to listen on
    pub port: u16,
    /// Endpoint path
    #[serde(default = "default_endpoint")]
    pub endpoint: String,
    /// Registered external sources
    pub sources: Vec<ExternalSource>,
}

fn default_endpoint() -> String {
    "/webhook/ingress".to_string()
}

impl Default for ExternalIngressConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            port: 8080,
            endpoint: default_endpoint(),
            sources: Vec::new(),
        }
    }
}

/// External source configuration for unified ingress
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalSource {
    /// Source name (e.g., "github", "discord")
    pub name: String,
    /// Detection method
    pub detection: SourceDetection,
    /// Agent to invoke
    pub agent_id: String,
    /// Optional verification configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification: Option<serde_json::Value>,
    /// Optional transform configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transform: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = PekobotConfig::default();
        assert_eq!(config.app_name, "pekobot");
        assert_eq!(config.network.port, 8080);
        assert_eq!(config.logging.level, "info");
    }

    #[test]
    fn test_config_roundtrip() {
        let config = PekobotConfig::default();
        let toml = toml::to_string(&config).unwrap();
        let parsed: PekobotConfig = toml::from_str(&toml).unwrap();

        assert_eq!(parsed.app_name, config.app_name);
        assert_eq!(parsed.network.port, config.network.port);
    }
}
