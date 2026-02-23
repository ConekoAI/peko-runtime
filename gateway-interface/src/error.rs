//! Gateway error types

use thiserror::Error;

/// Gateway errors
#[derive(Error, Debug)]
pub enum GatewayError {
    #[error("Plugin not found: {0}")]
    PluginNotFound(String),

    #[error("Version mismatch: expected {expected}, got {actual}")]
    VersionMismatch { expected: String, actual: String },

    #[error("Failed to load plugin '{name}': {source}")]
    LoadFailed {
        name: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error("Initialization failed: {message}")]
    InitializationFailed { message: String },

    #[error("Connection failed: {message}")]
    ConnectionFailed { message: String },

    #[error("Connection lost: {message}")]
    ConnectionLost { message: String },

    #[error("Send failed: {message}")]
    SendFailed { message: String },

    #[error("Receive failed: {message}")]
    ReceiveFailed { message: String },

    #[error("Entity not found: {entity}")]
    EntityNotFound { entity: String },

    #[error("Not supported: {operation}")]
    NotSupported { operation: String },

    #[error("Rate limited: retry after {retry_after}s")]
    RateLimited { retry_after: u64 },

    #[error("Authentication failed: {message}")]
    AuthenticationFailed { message: String },

    #[error("Configuration error: {message}")]
    ConfigurationError { message: String },

    #[error("Plugin error: {0}")]
    Plugin(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("Timeout after {duration}ms")]
    Timeout { duration: u64 },

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(String),

    #[error("Internal error: {message}")]
    Internal { message: String },
}

/// Result type
pub type GatewayResult<T> = Result<T, GatewayError>;
