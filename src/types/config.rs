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
}

impl Default for PekobotConfig {
    fn default() -> Self {
        Self {
            app_name: "pekobot".to_string(),
            storage: StorageConfig::default(),
            network: NetworkConfig::default(),
            logging: LogConfig::default(),
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
