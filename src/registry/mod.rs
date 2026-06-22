//! Registry Client Module
//!
//! Provides push/pull functionality for agent images to remote registries.
//! Implements OCI-inspired distribution with content-addressable layers.
//!
//! ## Usage
//!
//! ```rust,ignore
//! use pekobot::registry::{RegistryClient, RegistryConfig, ProgressEvent, AgentRegistry};
//!
//! let config = RegistryConfig::default();
//! let registry = AgentRegistry::new(".peko/registry");
//! let client = RegistryClient::new(config, registry);
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

pub mod agent_registry;
pub mod client;
pub mod config;
pub mod manifest;

/// Local packaging for agent/team archives (`.agent` / `.team`).
///
/// Relocated from `src/portable/` in issue #31f as part of the
/// 9-domain reorganization. Owns the manifest format, layer model,
/// signatures, encrypted exports, and team archive builder/reconstructor.
/// The remote registry client ([`client`]) consumes the same digest /
/// media-type conventions as this module's local artifacts.
pub mod packaging;

pub use agent_registry::AgentRegistry;
pub use client::{ProgressEvent, RegistryClient, RegistryRef};
pub use config::{load_from_workspace, AuthConfig, RegistryConfig, RegistrySource, ResolvedAuth};
pub use manifest::RegistryManifest;

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
    /// Peko manifest media type (legacy)
    pub const MANIFEST_PEKO: &str = "application/vnd.peko.manifest.v1+json";
    /// OCI manifest media type (preferred for pekohub compatibility)
    pub const MANIFEST_OCI: &str = "application/vnd.oci.image.manifest.v1+json";
    /// Layer media type (gzip tar)
    pub const LAYER: &str = "application/vnd.peko.layer.v1.tar+gzip";
    /// Config media type
    pub const CONFIG: &str = "application/vnd.peko.config.v1+json";

    /// Default manifest media type to use for push operations
    pub const MANIFEST_DEFAULT: &str = MANIFEST_OCI;

    /// All accepted manifest media types (for validation)
    pub const MANIFEST_ALL: &[&str] = &[MANIFEST_PEKO, MANIFEST_OCI];
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_valid_ref() {
        assert!(is_valid_ref("pekohub.com/agent:v1.0"));
        assert!(is_valid_ref("registry.io/org/team/agent:latest"));
        assert!(!is_valid_ref(""));
        assert!(!is_valid_ref("just-host"));
    }

    #[test]
    fn test_parse_host() {
        assert_eq!(
            parse_host("pekohub.com/agent:v1.0"),
            Some("pekohub.com".to_string())
        );
        assert_eq!(
            parse_host("registry.io/org/agent:latest"),
            Some("registry.io".to_string())
        );
        assert!(parse_host("").is_none());
    }

    #[test]
    fn test_media_types() {
        assert_eq!(
            media_types::MANIFEST_PEKO,
            "application/vnd.peko.manifest.v1+json"
        );
        assert_eq!(
            media_types::MANIFEST_OCI,
            "application/vnd.oci.image.manifest.v1+json"
        );
        assert_eq!(media_types::LAYER, "application/vnd.peko.layer.v1.tar+gzip");
        assert_eq!(media_types::CONFIG, "application/vnd.peko.config.v1+json");
        assert_eq!(media_types::MANIFEST_DEFAULT, media_types::MANIFEST_OCI);
        assert_eq!(
            media_types::MANIFEST_ALL,
            &[
                "application/vnd.peko.manifest.v1+json",
                "application/vnd.oci.image.manifest.v1+json"
            ]
        );
    }

    #[test]
    fn test_registry_error_display() {
        let err = RegistryError::RegistryNotFound("test.com".to_string());
        assert!(err.to_string().contains("test.com"));

        let err = RegistryError::ImageNotFound("my-image:v1".to_string());
        assert!(err.to_string().contains("my-image:v1"));

        let err = RegistryError::AuthenticationFailed("test.com".to_string());
        assert!(err.to_string().contains("test.com"));

        let err = RegistryError::DigestMismatch {
            expected: "sha256:abc".to_string(),
            actual: "sha256:def".to_string(),
        };
        assert!(err.to_string().contains("abc"));
        assert!(err.to_string().contains("def"));

        let err = RegistryError::LayerNotFound("sha256:xyz".to_string());
        assert!(err.to_string().contains("xyz"));

        let err = RegistryError::Other("something went wrong".to_string());
        assert!(err.to_string().contains("something went wrong"));
    }
}
