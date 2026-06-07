//! Auth configuration loading and validation

use super::types::{AuthConfigFile, RateLimitConfigFile};
use crate::common::paths::PathResolver;
use std::net::SocketAddr;
use std::path::PathBuf;

/// Runtime auth configuration
#[derive(Clone, Debug)]
pub struct AuthConfig {
    enable_local_trust: bool,
    enable_pekohub_jwt: bool,
    enable_api_key: bool,
    trusted_issuers: Vec<String>,
    rate_limit: RateLimitConfig,
    /// Path to the auth config file
    config_path: PathBuf,
    /// Path to the API keys file
    api_keys_path: PathBuf,
    /// Path to the pekohub config file
    pekohub_path: PathBuf,
}

/// Rate limit configuration
#[derive(Clone, Debug)]
pub struct RateLimitConfig {
    pub jwt_requests_per_minute: u32,
    pub api_key_requests_per_minute: u32,
    pub burst_jwt: u32,
    pub burst_api_key: u32,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            jwt_requests_per_minute: 30,
            api_key_requests_per_minute: 100,
            burst_jwt: 10,
            burst_api_key: 20,
        }
    }
}

impl AuthConfig {
    /// Load auth configuration from disk, or create defaults
    pub fn load(resolver: &PathResolver) -> anyhow::Result<Self> {
        let config_path = resolver.runtime_dir().join("auth_config.toml");
        let api_keys_path = resolver.runtime_dir().join("api_keys.toml");
        let pekohub_path = resolver.runtime_dir().join("pekohub.toml");

        let file = if config_path.exists() {
            let content = std::fs::read_to_string(&config_path)?;
            match toml::from_str::<AuthConfigFile>(&content) {
                Ok(f) => f,
                Err(e) => {
                    tracing::warn!("Failed to parse auth_config.toml: {}. Using defaults.", e);
                    AuthConfigFile::default()
                }
            }
        } else {
            AuthConfigFile::default()
        };

        Ok(Self {
            enable_local_trust: file.enable_local_trust,
            enable_pekohub_jwt: file.enable_pekohub_jwt,
            enable_api_key: file.enable_api_key,
            trusted_issuers: file.trusted_issuers,
            rate_limit: RateLimitConfig {
                jwt_requests_per_minute: file.rate_limit.jwt_requests_per_minute,
                api_key_requests_per_minute: file.rate_limit.api_key_requests_per_minute,
                burst_jwt: file.rate_limit.burst_jwt,
                burst_api_key: file.rate_limit.burst_api_key,
            },
            config_path,
            api_keys_path,
            pekohub_path,
        })
    }

    /// Save the current configuration to disk
    pub fn save(&self) -> anyhow::Result<()> {
        let file = AuthConfigFile {
            version: "1".to_string(),
            enable_local_trust: self.enable_local_trust,
            enable_pekohub_jwt: self.enable_pekohub_jwt,
            enable_api_key: self.enable_api_key,
            trusted_issuers: self.trusted_issuers.clone(),
            rate_limit: RateLimitConfigFile {
                jwt_requests_per_minute: self.rate_limit.jwt_requests_per_minute,
                api_key_requests_per_minute: self.rate_limit.api_key_requests_per_minute,
                burst_jwt: self.rate_limit.burst_jwt,
                burst_api_key: self.rate_limit.burst_api_key,
            },
        };

        if let Some(parent) = self.config_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let toml = toml::to_string_pretty(&file)?;
        std::fs::write(&self.config_path, toml)?;
        Ok(())
    }

    /// Check if local trust is enabled
    #[must_use]
    pub fn enable_local_trust(&self) -> bool {
        self.enable_local_trust
    }

    /// Check if pekohub JWT is enabled
    #[must_use]
    pub fn enable_pekohub_jwt(&self) -> bool {
        self.enable_pekohub_jwt
    }

    /// Check if API key auth is enabled
    #[must_use]
    pub fn enable_api_key(&self) -> bool {
        self.enable_api_key
    }

    /// Get trusted issuers
    #[must_use]
    pub fn trusted_issuers(&self) -> &[String] {
        &self.trusted_issuers
    }

    /// Get rate limit configuration
    #[must_use]
    pub fn rate_limit(&self) -> &RateLimitConfig {
        &self.rate_limit
    }

    /// Check if any remote authentication method is configured
    #[must_use]
    pub fn has_any_remote_auth_method(&self) -> bool {
        self.enable_pekohub_jwt || self.enable_api_key
    }

    /// Get the path to the API keys file
    #[must_use]
    pub fn api_keys_path(&self) -> &PathBuf {
        &self.api_keys_path
    }

    /// Get the path to the pekohub config file
    #[must_use]
    pub fn pekohub_path(&self) -> &PathBuf {
        &self.pekohub_path
    }

    /// Get the path to the auth config file
    #[must_use]
    pub fn config_path(&self) -> &PathBuf {
        &self.config_path
    }
}

/// Check if a socket address is loopback
#[must_use]
pub fn is_loopback(addr: &SocketAddr) -> bool {
    addr.ip().is_loopback()
}

/// Enforce that public binds require authentication.
///
/// Returns an error if the daemon is bound to a non-loopback address
/// but no remote auth method is configured.
pub fn enforce_auth_for_public_bind(
    bind_addr: &SocketAddr,
    auth_config: &AuthConfig,
) -> anyhow::Result<()> {
    if !is_loopback(bind_addr) {
        if !auth_config.has_any_remote_auth_method() {
            anyhow::bail!(
                "Daemon is bound to {bind_addr}, but no remote authentication method is configured. \
                 Configure pekohub JWT or API keys, or bind to 127.0.0.1."
            );
        }
        tracing::info!(
            "Daemon is bound to {bind_addr}. Remote access is enabled. Ensure your firewall rules are correct."
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_loopback() {
        let loopback: SocketAddr = "127.0.0.1:11435".parse().unwrap();
        assert!(is_loopback(&loopback));

        let public: SocketAddr = "0.0.0.0:11435".parse().unwrap();
        assert!(!is_loopback(&public));
    }

    #[test]
    fn test_enforce_auth_loopback_ok() {
        let addr: SocketAddr = "127.0.0.1:11435".parse().unwrap();
        let config = AuthConfig {
            enable_local_trust: true,
            enable_pekohub_jwt: false,
            enable_api_key: false,
            trusted_issuers: vec![],
            rate_limit: RateLimitConfig::default(),
            config_path: PathBuf::from("/tmp/auth_config.toml"),
            api_keys_path: PathBuf::from("/tmp/api_keys.toml"),
            pekohub_path: PathBuf::from("/tmp/pekohub.toml"),
        };
        assert!(enforce_auth_for_public_bind(&addr, &config).is_ok());
    }

    #[test]
    fn test_enforce_auth_public_without_auth_fails() {
        let addr: SocketAddr = "0.0.0.0:11435".parse().unwrap();
        let config = AuthConfig {
            enable_local_trust: true,
            enable_pekohub_jwt: false,
            enable_api_key: false,
            trusted_issuers: vec![],
            rate_limit: RateLimitConfig::default(),
            config_path: PathBuf::from("/tmp/auth_config.toml"),
            api_keys_path: PathBuf::from("/tmp/api_keys.toml"),
            pekohub_path: PathBuf::from("/tmp/pekohub.toml"),
        };
        assert!(enforce_auth_for_public_bind(&addr, &config).is_err());
    }

    #[test]
    fn test_enforce_auth_public_with_jwt_ok() {
        let addr: SocketAddr = "0.0.0.0:11435".parse().unwrap();
        let config = AuthConfig {
            enable_local_trust: true,
            enable_pekohub_jwt: true,
            enable_api_key: false,
            trusted_issuers: vec!["pekohub".to_string()],
            rate_limit: RateLimitConfig::default(),
            config_path: PathBuf::from("/tmp/auth_config.toml"),
            api_keys_path: PathBuf::from("/tmp/api_keys.toml"),
            pekohub_path: PathBuf::from("/tmp/pekohub.toml"),
        };
        assert!(enforce_auth_for_public_bind(&addr, &config).is_ok());
    }

    #[test]
    fn test_enforce_auth_public_with_api_key_ok() {
        let addr: SocketAddr = "0.0.0.0:11435".parse().unwrap();
        let config = AuthConfig {
            enable_local_trust: true,
            enable_pekohub_jwt: false,
            enable_api_key: true,
            trusted_issuers: vec![],
            rate_limit: RateLimitConfig::default(),
            config_path: PathBuf::from("/tmp/auth_config.toml"),
            api_keys_path: PathBuf::from("/tmp/api_keys.toml"),
            pekohub_path: PathBuf::from("/tmp/pekohub.toml"),
        };
        assert!(enforce_auth_for_public_bind(&addr, &config).is_ok());
    }
}
