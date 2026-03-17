//! Registry Client Module
//!
//! Provides push/pull functionality for agent images to remote registries.
//! Implements OCI-inspired distribution with content-addressable layers.
//!
//! ## Usage
//!
//! ```rust,ignore
//! use pekobot::registry::{RegistryClient, RegistryConfig, ProgressEvent};
//!
//! let config = RegistryConfig::default();
//! let client = RegistryClient::new(config, ".pekobot/registry");
//!
//! // Pull an image
//! client.pull("pekohub.com/agents/base:v1", |event| {
//!     println!("{:?}", event);
//! }).await?;
//!
//! // Push an image
//! client.push(&digest, "pekohub.com/user/my-agent:v1", |event| {
//!     println!("{:?}", event);
//! }).await?;
//! ```

pub mod client;
pub mod config;

pub use client::{ProgressEvent, RegistryClient, RegistryRef};
pub use config::{load_from_workspace, AuthConfig, RegistryConfig, RegistrySource, ResolvedAuth};

/// Registry errors
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("Registry not found: {0}")]
    RegistryNotFound(String),

    #[error("Image not found: {0}")]
    ImageNotFound(String),

    #[error("Authentication failed for {0}")]
    AuthenticationFailed(String),

    #[error("Layer digest mismatch: expected {expected}, got {actual}")]
    DigestMismatch { expected: String, actual: String },

    #[error("Layer not found locally: {0}")]
    LayerNotFound(String),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Other: {0}")]
    Other(String),
}

/// Registry API version
pub const REGISTRY_API_VERSION: &str = "v2";

/// Media types for registry operations
pub mod media_types {
    /// Manifest media type
    pub const MANIFEST: &str = "application/vnd.pekobot.manifest.v1+json";

    /// Layer media type (gzip tar)
    pub const LAYER: &str = "application/vnd.pekobot.layer.v1.tar+gzip";

    /// Config media type
    pub const CONFIG: &str = "application/vnd.pekobot.config.v1+json";
}

/// Check if a registry reference is valid
#[must_use]
pub fn is_valid_ref(r#ref: &str) -> bool {
    RegistryRef::parse(r#ref).is_ok()
}

/// Parse registry host from reference
#[must_use]
pub fn parse_host(r#ref: &str) -> Option<String> {
    RegistryRef::parse(r#ref).ok().map(|r| r.host)
}
