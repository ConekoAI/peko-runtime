//! Gateway error types
//!
//! This module re-exports error types from gateway-interface and adds
//! registry-specific error types.

pub use gateway_interface::{GatewayError, GatewayResult};

use thiserror::Error;

/// Errors from the registry
#[derive(Error, Debug)]
pub enum RegistryError {
    /// Plugin not found
    #[error("Plugin '{0}' not found in registry")]
    NotFound(String),

    /// Plugin already loaded
    #[error("Plugin '{0}' is already loaded")]
    AlreadyLoaded(String),

    /// Download failed
    #[error("Failed to download plugin '{name}': {source}")]
    DownloadFailed {
        name: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// Cache error
    #[error("Cache error for plugin '{name}': {message}")]
    CacheError { name: String, message: String },

    /// Invalid plugin manifest
    #[error("Invalid plugin manifest for '{name}': {message}")]
    InvalidManifest { name: String, message: String },

    /// Platform not supported by plugin
    #[error("Plugin '{name}' does not support platform '{platform}'")]
    UnsupportedPlatform { name: String, platform: String },

    /// Dependency missing
    #[error("Plugin '{name}' requires '{dependency}' which is not installed")]
    MissingDependency { name: String, dependency: String },

    /// Pekohub communication error
    #[error("Pekohub error: {0}")]
    Pekohub(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Result type for registry operations
pub type RegistryResult<T> = Result<T, RegistryError>;
