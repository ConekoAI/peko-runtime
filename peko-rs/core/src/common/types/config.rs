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
    // ADR-058 D6: the `bind_address` field was deleted — it was never
    // read (the daemon's HTTP/IPC bind is a loopback constant), so the
    // knob silently did nothing. Reintroduce only with mandatory
    // credential auth when remote daemon access is a designed feature.
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

// ADR-058 post-review cleanup: `DirectNetworkConfig` and the
// `network.direct` field were deleted — the direct cross-runtime
// transport was retired in B5 (all traffic flows through the tunnel
// relay), so the `[direct]` config block was parsed but never read by
// any runtime logic: a knob that silently does nothing (the same
// latent-exposure class as the D6 `bind_address` deletion). Old config
// files carrying a `[direct]` table still parse — unknown keys are
// ignored.

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

    /// ADR-058 post-review cleanup: the retired `[direct]` block is
    /// ignored when parsing an old config file (unknown keys) instead
    /// of failing the load.
    #[test]
    fn test_network_config_ignores_retired_direct_block() {
        let toml = r#"
            port = 8080
            tls_enabled = false
            cors_origins = ["*"]
            request_timeout_seconds = 30
            max_body_size_mb = 10

            [direct]
            enabled = true
            bind_address = "192.168.1.5"
            port = 11437
        "#;
        let config: NetworkConfig = toml::from_str(toml).expect("retired [direct] must be ignored");
        assert_eq!(config.port, 8080);
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
