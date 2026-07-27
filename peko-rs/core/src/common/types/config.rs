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
    /// Session compaction configuration
    #[serde(default)]
    pub compaction: CompactionConfig,
    /// F40b / PR #3 Phase 2B: provider-level configuration. The
    /// `[provider]` block in `config.example.toml` carries the
    /// default provider / model for the daemon, plus an optional
    /// `[provider.retry]` sub-block that overrides the
    /// factory-default retry knobs (`max_retries`, `retry_delay_ms`,
    /// `retry_jitter`).
    #[serde(default)]
    pub provider: ProviderConfig,
}

impl Default for PekoConfig {
    fn default() -> Self {
        Self {
            app_name: "peko".to_string(),
            storage: StorageConfig::default(),
            network: NetworkConfig::default(),
            logging: LogConfig::default(),
            compaction: CompactionConfig::default(),
            provider: ProviderConfig::default(),
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
}

// ============================================================================
// Compaction Configuration (ADR-022)
// ============================================================================

// Internal use of the canonical CompactionConfig. The `pub use` shim at this
// module level was deleted in the Item 2c cleanup pass — external callers that
// need `CompactionConfig` should import it directly from `peko_session::compaction`.
use peko_session::compaction::CompactionConfig;

// ============================================================================
// Provider Configuration (F40b / PR #3 Phase 2B)
// ============================================================================

/// Provider-level configuration block in `PekoConfig`. The
/// corresponding `[provider]` table in `config.example.toml` carries
/// the default provider type (`type = "anthropic"`) and model
/// (`model = "claude-3-5-haiku-latest"`) the daemon should boot with,
/// plus an optional `[provider.retry]` sub-table that overrides the
/// factory-default transport retry knobs.
///
/// Both `provider_type` and `model` are optional in the struct so
/// callers that don't want the daemon to auto-bootstrap a provider
/// (e.g. dev environments that build providers lazily from the
/// catalog) can leave them unset. The retry block is mandatory in
/// shape but each field has its own default — a missing
/// `[provider.retry]` table still parses, falling back to
/// `ProviderRetryConfig::default()`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProviderConfig {
    /// Wire-format provider id (e.g. `anthropic`, `openai`,
    /// `ollama`). Optional; when absent the daemon does not pick a
    /// default provider on boot.
    #[serde(rename = "type", skip_serializing_if = "Option::is_none", default)]
    pub provider_type: Option<String>,
    /// Default model id surfaced through `Provider::model_id()` when
    /// no explicit id is passed per-request. Optional.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub model: Option<String>,
    /// Per-call retry knobs. Defaults to the F40 factory constants
    /// (`max_retries=5`, `retry_delay_ms=1000`, `retry_jitter=0.1`,
    /// `max_attempts=8`). All fields are individually optional so a
    /// partial table is accepted — callers that only want to bump
    /// `max_retries` leave the rest at the default.
    #[serde(default)]
    pub retry: peko_provider_api::ProviderRetryConfig,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            provider_type: None,
            model: None,
            retry: peko_provider_api::ProviderRetryConfig::default(),
        }
    }
}

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

    // -------- F40b / PR #3 Phase 2B: ProviderConfig / ProviderRetryConfig --------

    #[test]
    fn test_provider_config_defaults() {
        let pc = ProviderConfig::default();
        assert!(pc.provider_type.is_none());
        assert!(pc.model.is_none());
        // Retry block is mandatory in shape; defaults mirror the F40 factory constants.
        assert_eq!(pc.retry, peko_provider_api::ProviderRetryConfig::default());
        assert_eq!(pc.retry.max_retries, 5);
        assert_eq!(pc.retry.retry_delay_ms, 1000);
        assert_eq!(pc.retry.retry_max_delay_ms, 30_000);
        assert_eq!(pc.retry.retry_jitter, Some(0.1));
        assert_eq!(pc.retry.max_attempts, 8);
    }

    /// F40b: missing `[provider]` table in a `PekoConfig` TOML
    /// falls back to `ProviderConfig::default()` rather than failing
    /// the parse — every field is `Option` or has a `#[serde(default)]`
    /// attribute.
    #[test]
    fn test_peko_config_provider_block_is_optional() {
        let cfg: PekoConfig = toml::from_str(
            r#"
                app_name = "peko"
                [storage]
                storage_type = "sqlite"
                database_path = "/tmp/peko.db"
                keys_path = "/tmp/keys"
                memory_path = "/tmp/memory.db"
                [network]
                bind_address = "127.0.0.1"
                port = 8080
                tls_enabled = false
                cors_origins = ["*"]
                request_timeout_seconds = 30
                max_body_size_mb = 10
                [logging]
                level = "info"
                format = "pretty"
                log_stdout = true
            "#,
        )
        .expect("PekoConfig without [provider] must parse");
        assert_eq!(cfg.provider, ProviderConfig::default());
        assert!(cfg.provider.provider_type.is_none());
        assert_eq!(cfg.provider.retry.max_retries, 5);
    }

    /// F40b: a partial `[provider.retry]` block (only one field set)
    /// is accepted — every field carries its own `#[serde(default)]`
    /// helper so callers can bump one knob without restating the
    /// others.
    #[test]
    fn test_provider_retry_partial_block_parses() {
        let parsed: peko_provider_api::ProviderRetryConfig =
            toml::from_str("max_retries = 8\n").expect("partial retry block must parse");
        assert_eq!(parsed.max_retries, 8);
        // Unset fields fall back to defaults.
        assert_eq!(parsed.retry_delay_ms, 1000);
        assert_eq!(parsed.retry_jitter, Some(0.1));
        assert_eq!(parsed.max_attempts, 8);
    }

    /// F40b: jitter validation rejects out-of-range values so a
    /// typo'd `[provider.retry] retry_jitter = 2.0` doesn't quietly
    /// double every backoff (a 200% jitter band would mean a 3x
    /// wait on every attempt and balloon LLM call latency).
    #[test]
    fn test_provider_retry_jitter_validation_rejects_out_of_range() {
        let mut cfg = peko_provider_api::ProviderRetryConfig::default();
        cfg.retry_jitter = Some(1.5);
        assert!(cfg.validate().is_err(), "jitter >= 1.0 must be rejected");
        cfg.retry_jitter = Some(-0.01);
        assert!(cfg.validate().is_err(), "negative jitter must be rejected");
        // 0.0 is allowed (disables jitter explicitly).
        cfg.retry_jitter = Some(0.0);
        assert!(cfg.validate().is_ok(), "jitter=0.0 is valid");
        cfg.retry_jitter = Some(0.99);
        assert!(
            cfg.validate().is_ok(),
            "jitter in [0.0, 1.0) must be accepted"
        );
    }

    /// F40b: `max_retries > max_attempts` is rejected because the
    /// transport layer would burn through the shared budget before
    /// the engine mid-stream retry site ever sees a turn.
    #[test]
    fn test_provider_retry_validation_max_retries_exceeds_max_attempts() {
        let mut cfg = peko_provider_api::ProviderRetryConfig::default();
        cfg.max_retries = 10;
        cfg.max_attempts = 4;
        let err = cfg
            .validate()
            .expect_err("max_retries > max_attempts must fail");
        assert!(
            err.to_string().contains("max_retries"),
            "error must name the failing field: {err}"
        );
    }

    /// F40b: zero `retry_delay_ms` is rejected (would tight-loop on
    /// every transient 5xx); cap smaller than seed is also rejected
    /// (the cap cannot constrain something that hasn't grown yet).
    #[test]
    fn test_provider_retry_validation_zero_delay_and_cap_too_small() {
        let mut cfg = peko_provider_api::ProviderRetryConfig::default();
        cfg.retry_delay_ms = 0;
        assert!(cfg.validate().is_err(), "zero delay must be rejected");
        cfg.retry_delay_ms = 1000;
        cfg.retry_max_delay_ms = 500;
        assert!(
            cfg.validate().is_err(),
            "cap smaller than seed must be rejected"
        );
    }

    /// F40b: round-trip through TOML preserves every field so a
    /// daemon that writes its config back on save (e.g. the
    /// `peko config edit` CLI) doesn't silently drop new knobs.
    #[test]
    fn test_provider_retry_toml_roundtrip() {
        let original = peko_provider_api::ProviderRetryConfig {
            max_retries: 7,
            retry_delay_ms: 250,
            retry_max_delay_ms: 15_000,
            retry_jitter: Some(0.25),
            max_attempts: 12,
        };
        original
            .validate()
            .expect("non-default values must validate");
        let serialized = toml::to_string(&original).unwrap();
        let parsed: peko_provider_api::ProviderRetryConfig = toml::from_str(&serialized).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    #[allow(non_snake_case)]
    fn test_PEKO_config_carries_provider_block() {
        let cfg = PekoConfig::default();
        // Smoke test: every PekoConfig exposes the new field.
        let _: &ProviderConfig = &cfg.provider;
    }
}
