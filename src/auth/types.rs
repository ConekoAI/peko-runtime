//! Auth data types

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// API key entry stored on disk
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKeyEntry {
    /// Key ID (prefix: pkr_ + first 8 chars of the key)
    pub id: String,
    /// SHA-256 hash of the full key, prefixed with "sha256:"
    pub hash: String,
    /// Human-readable name
    pub name: String,
    /// When the key was created
    pub created_at: DateTime<Utc>,
    /// When the key was last used
    pub last_used_at: Option<DateTime<Utc>>,
    /// Granted scopes
    pub scopes: Vec<ApiKeyScope>,
    /// Whether the key is enabled
    pub enabled: bool,
}

/// API key permission scopes
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApiKeyScope {
    Read,
    Write,
    Admin,
}

impl std::fmt::Display for ApiKeyScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read => write!(f, "read"),
            Self::Write => write!(f, "write"),
            Self::Admin => write!(f, "admin"),
        }
    }
}

impl std::str::FromStr for ApiKeyScope {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "read" => Ok(Self::Read),
            "write" => Ok(Self::Write),
            "admin" => Ok(Self::Admin),
            _ => Err(format!("Unknown scope: {s}")),
        }
    }
}

/// On-disk format for api_keys.toml
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKeysFile {
    pub version: String,
    #[serde(rename = "key")]
    pub keys: Vec<ApiKeyEntry>,
}

impl Default for ApiKeysFile {
    fn default() -> Self {
        Self {
            version: "1".to_string(),
            keys: Vec::new(),
        }
    }
}

/// Pekohub configuration stored on disk
///
/// Defined for forward compatibility. Not used in v0.1.0 because
/// pekohub JWT validation is disabled until signature verification
/// is implemented.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PekohubConfig {
    pub version: String,
    pub runtime_did: String,
    #[serde(flatten)]
    pub credential: PekohubCredential,
    pub pekohub: PekohubEndpoints,
}

/// Pekohub credential types
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "credential_type")]
pub enum PekohubCredential {
    #[serde(rename = "shared_secret")]
    SharedSecret { secret: String },
    #[serde(rename = "mtls")]
    Mtls {
        cert_path: PathBuf,
        key_path: PathBuf,
    },
}

/// Pekohub endpoint configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PekohubEndpoints {
    pub issuer: String,
    pub jwks_url: String,
    pub token_endpoint: String,
}

/// On-disk format for auth_config.toml
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfigFile {
    pub version: String,
    /// Enable local trust (Unix socket / localhost UDP)
    pub enable_local_trust: bool,
    /// Enable pekohub JWT authentication
    pub enable_pekohub_jwt: bool,
    /// Enable API key authentication
    pub enable_api_key: bool,
    /// Trusted JWT issuers
    pub trusted_issuers: Vec<String>,
    /// Rate limit configuration
    pub rate_limit: RateLimitConfigFile,
}

impl Default for AuthConfigFile {
    fn default() -> Self {
        Self {
            version: "1".to_string(),
            enable_local_trust: true,
            enable_pekohub_jwt: false,
            enable_api_key: false,
            trusted_issuers: vec!["pekohub".to_string()],
            rate_limit: RateLimitConfigFile::default(),
        }
    }
}

/// Rate limit configuration on disk
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitConfigFile {
    pub jwt_requests_per_minute: u32,
    pub api_key_requests_per_minute: u32,
    pub burst_jwt: u32,
    pub burst_api_key: u32,
}

impl Default for RateLimitConfigFile {
    fn default() -> Self {
        Self {
            jwt_requests_per_minute: 30,
            api_key_requests_per_minute: 100,
            burst_jwt: 10,
            burst_api_key: 20,
        }
    }
}
