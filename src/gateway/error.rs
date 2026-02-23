//! Gateway error types

use thiserror::Error;

/// Errors that can occur when working with gateways
#[derive(Error, Debug)]
pub enum GatewayError {
    /// Plugin not found in registry or cache
    #[error("Gateway plugin not found: {0}")]
    PluginNotFound(String),

    /// Plugin API version mismatch
    #[error("Gateway plugin API version mismatch: expected {expected}, got {actual}")]
    VersionMismatch { expected: String, actual: String },

    /// Failed to load plugin (dynamic library error)
    #[error("Failed to load gateway plugin '{name}': {source}")]
    LoadFailed {
        name: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// Plugin factory not found in library
    #[error("Gateway factory not found in plugin '{0}'")]
    FactoryNotFound(String),

    /// Initialization failed
    #[error("Gateway '{name}' initialization failed: {message}")]
    InitializationFailed { name: String, message: String },

    /// Connection failed
    #[error("Gateway '{name}' connection failed: {message}")]
    ConnectionFailed { name: String, message: String },

    /// Connection lost
    #[error("Gateway '{name}' connection lost: {message}")]
    ConnectionLost { name: String, message: String },

    /// Send failed
    #[error("Failed to send message via '{gateway}': {message}")]
    SendFailed { gateway: String, message: String },

    /// Receive failed
    #[error("Failed to receive message from '{gateway}': {message}")]
    ReceiveFailed { gateway: String, message: String },

    /// Entity not found
    #[error("Entity not found: {entity}")]
    EntityNotFound { entity: String },

    /// Operation not supported by this gateway
    #[error("Operation not supported by gateway '{gateway}': {operation}")]
    NotSupported { gateway: String, operation: String },

    /// Rate limited
    #[error("Rate limited by gateway '{gateway}': retry after {retry_after}s")]
    RateLimited { gateway: String, retry_after: u64 },

    /// Authentication/authorization error
    #[error("Authentication failed for gateway '{gateway}': {message}")]
    AuthenticationFailed { gateway: String, message: String },

    /// Configuration error
    #[error("Configuration error for gateway '{gateway}': {message}")]
    ConfigurationError { gateway: String, message: String },

    /// Plugin reported an error
    #[error("Gateway plugin error: {0}")]
    Plugin(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// Timeout
    #[error("Gateway operation timed out after {duration}ms")]
    Timeout { duration: u64 },

    /// IO error
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Serialization error
    #[error("Serialization error: {0}")]
    Serialization(String),

    /// Internal error
    #[error("Internal gateway error: {message}")]
    Internal { message: String },
}

impl GatewayError {
    /// Check if this error is recoverable (can retry)
    pub fn is_recoverable(&self) -> bool {
        matches!(
            self,
            GatewayError::ConnectionLost { .. }
                | GatewayError::RateLimited { .. }
                | GatewayError::Timeout { .. }
        )
    }

    /// Check if this is an authentication error
    pub fn is_auth_error(&self) -> bool {
        matches!(self, GatewayError::AuthenticationFailed { .. })
    }

    /// Get the gateway name if available
    pub fn gateway_name(&self) -> Option<&str> {
        match self {
            GatewayError::InitializationFailed { name, .. }
            | GatewayError::ConnectionFailed { name, .. }
            | GatewayError::ConnectionLost { name, .. }
            | GatewayError::SendFailed { gateway: name, .. }
            | GatewayError::ReceiveFailed { gateway: name, .. }
            | GatewayError::NotSupported { gateway: name, .. }
            | GatewayError::RateLimited { gateway: name, .. }
            | GatewayError::AuthenticationFailed { gateway: name, .. }
            | GatewayError::ConfigurationError { gateway: name, .. } => Some(name),
            _ => None,
        }
    }
}

/// Result type for gateway operations
pub type GatewayResult<T> = Result<T, GatewayError>;

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
