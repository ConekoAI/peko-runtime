//! Peko global configuration

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Global peko configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PekoConfig {
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
    /// Session compaction configuration
    #[serde(default)]
    pub compaction: CompactionConfig,
}

impl Default for PekoConfig {
    fn default() -> Self {
        Self {
            app_name: "peko".to_string(),
            storage: StorageConfig::default(),
            network: NetworkConfig::default(),
            logging: LogConfig::default(),
            orchestration: OrchestrationConfig::default(),
            compaction: CompactionConfig::default(),
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
            .join("peko");

        Self {
            storage_type: "sqlite".to_string(),
            database_path: data_dir.join("peko.db"),
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
    /// Direct cross-runtime connection configuration (advanced users)
    #[serde(default)]
    pub direct: DirectNetworkConfig,
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
            direct: DirectNetworkConfig::default(),
        }
    }
}

/// Direct cross-runtime connection configuration.
///
/// Allows runtimes to accept inbound direct connections from other
/// authorized runtimes without routing through the PekoHub tunnel.
/// Disabled by default; intended for advanced users who control their
/// own network topology.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectNetworkConfig {
    /// Master enable for inbound direct connections.
    #[serde(default = "default_direct_enabled")]
    pub enabled: bool,
    /// Address the inbound direct server binds to.
    #[serde(default = "default_direct_bind_address")]
    pub bind_address: String,
    /// Port the inbound direct server listens on.
    #[serde(default = "default_direct_port")]
    pub port: u16,
    /// Require TLS for inbound direct connections.
    #[serde(default = "default_direct_tls_required")]
    pub tls_required: bool,
    /// Server certificate chain (PEM) for inbound TLS.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tls_cert_path: Option<PathBuf>,
    /// Server private key (PEM) for inbound TLS.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tls_key_path: Option<PathBuf>,
    /// Optional CA to require for inbound mTLS client auth.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tls_client_ca_path: Option<PathBuf>,
    /// Explicit URL this runtime advertises to peers for inbound direct
    /// connections (e.g. `wss://203.0.113.4:11436`). When absent, the
    /// runtime does not publish a direct endpoint to the hub.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub advertise_endpoint: Option<String>,
}

impl Default for DirectNetworkConfig {
    fn default() -> Self {
        Self {
            enabled: default_direct_enabled(),
            bind_address: default_direct_bind_address(),
            port: default_direct_port(),
            tls_required: default_direct_tls_required(),
            tls_cert_path: None,
            tls_key_path: None,
            tls_client_ca_path: None,
            advertise_endpoint: None,
        }
    }
}

fn default_direct_enabled() -> bool {
    false
}

fn default_direct_bind_address() -> String {
    "0.0.0.0".to_string()
}

fn default_direct_port() -> u16 {
    11436
}

fn default_direct_tls_required() -> bool {
    true
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

impl PekoConfig {
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
                database_path: data_dir.join("peko.db"),
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
        if self.webhook.enabled
            && self.external_ingress.enabled
            && self.webhook.port == self.external_ingress.port
        {
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

// ============================================================================
// Compaction Configuration (ADR-022)
// ============================================================================

/// Session compaction configuration (re-exported from the canonical
/// definition in `crate::session::compaction`).
///
/// `PekoConfig.compaction` keeps this name so existing TOML configs
/// continue to round-trip. The canonical type lives next to the engine
/// that consumes it (`engine/compaction_orchestrator`, `session/compaction/*`).
#[allow(unused_imports)]
pub use crate::session::compaction::CompactionConfig;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = PekoConfig::default();
        assert_eq!(config.app_name, "peko");
        assert_eq!(config.network.port, 8080);
        assert_eq!(config.logging.level, "info");
    }

    #[test]
    fn test_direct_network_config_defaults() {
        let direct = DirectNetworkConfig::default();
        assert!(!direct.enabled);
        assert_eq!(direct.bind_address, "0.0.0.0");
        assert_eq!(direct.port, 11436);
        assert!(direct.tls_required);
        assert!(direct.tls_cert_path.is_none());
        assert!(direct.tls_key_path.is_none());
        assert!(direct.tls_client_ca_path.is_none());
        assert!(direct.advertise_endpoint.is_none());
    }

    #[test]
    fn test_direct_network_config_toml_roundtrip() {
        let direct = DirectNetworkConfig::default();
        let toml = toml::to_string(&direct).unwrap();
        let parsed: DirectNetworkConfig = toml::from_str(&toml).unwrap();
        assert_eq!(parsed.enabled, direct.enabled);
        assert_eq!(parsed.bind_address, direct.bind_address);
        assert_eq!(parsed.port, direct.port);
        assert_eq!(parsed.tls_required, direct.tls_required);
        assert!(parsed.advertise_endpoint.is_none());
    }

    #[test]
    fn test_network_config_direct_default() {
        let config = NetworkConfig::default();
        assert!(!config.direct.enabled);
        assert_eq!(config.direct.port, 11436);
    }

    #[test]
    fn test_network_config_direct_toml_parsing() {
        let toml = r#"
            bind_address = "127.0.0.1"
            port = 8080
            tls_enabled = false
            cors_origins = ["*"]
            request_timeout_seconds = 30
            max_body_size_mb = 10

            [direct]
            enabled = true
            bind_address = "192.168.1.5"
            port = 11437
            tls_required = true
            tls_cert_path = "/etc/peko/direct.crt"
            tls_key_path = "/etc/peko/direct.key"
            advertise_endpoint = "wss://203.0.113.4:11436"
        "#;
        let config: NetworkConfig = toml::from_str(toml).unwrap();
        assert!(config.direct.enabled);
        assert_eq!(config.direct.bind_address, "192.168.1.5");
        assert_eq!(config.direct.port, 11437);
        assert!(config.direct.tls_required);
        assert_eq!(
            config.direct.tls_cert_path,
            Some(PathBuf::from("/etc/peko/direct.crt"))
        );
        assert_eq!(
            config.direct.tls_key_path,
            Some(PathBuf::from("/etc/peko/direct.key"))
        );
        assert_eq!(
            config.direct.advertise_endpoint,
            Some("wss://203.0.113.4:11436".to_string())
        );
    }

    #[test]
    fn test_compaction_config_defaults() {
        let config = CompactionConfig::default();
        assert!(config.enabled);
        assert_eq!(config.auto_threshold_percent, 85);
        assert_eq!(config.reserve_tokens, 16_384);
        assert_eq!(config.keep_recent_tokens, 20_000);
        assert_eq!(config.max_compactions_per_session, 100);
        assert_eq!(config.cooldown_seconds, 60);
        assert!(config.model_limits.is_empty());
    }

    #[test]
    fn test_compaction_config_toml_roundtrip() {
        let config = CompactionConfig::default();
        let toml = toml::to_string(&config).unwrap();
        let parsed: CompactionConfig = toml::from_str(&toml).unwrap();
        assert_eq!(parsed.enabled, config.enabled);
        assert_eq!(parsed.auto_threshold_percent, config.auto_threshold_percent);
        assert_eq!(parsed.reserve_tokens, config.reserve_tokens);
    }

    #[test]
    #[allow(non_snake_case)]
    fn test_PEKO_config_with_compaction() {
        let config = PekoConfig::default();
        assert!(config.compaction.enabled);
        assert_eq!(config.compaction.auto_threshold_percent, 85);
    }
}
