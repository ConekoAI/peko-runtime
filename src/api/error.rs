//! API Error Types
//!
//! This module defines error types for the HTTP API and implements
//! conversions to HTTP responses following the API contract.

use axum::{
    extract::rejection::JsonRejection,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};

use crate::api::types::{ErrorDetail, ErrorResponse};

/// API Error types
///
/// These errors map to specific HTTP status codes and error codes
/// as defined in the API contract.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// Internal server error
    #[error("Internal server error: {message}")]
    Internal {
        /// Error message
        message: String,
        /// Request ID for tracing
        request_id: String,
    },

    /// Resource not found
    #[error("Not found: {resource_type} {resource_id}")]
    NotFound {
        /// Type of resource
        resource_type: String,
        /// Resource identifier
        resource_id: String,
        /// Request ID for tracing
        request_id: String,
    },

    /// Bad request (malformed or invalid)
    #[error("Bad request: {message}")]
    BadRequest {
        /// Error message
        message: String,
        /// Request ID for tracing
        request_id: String,
        /// Optional field-level validation errors
        details: Option<serde_json::Value>,
    },

    /// Service unavailable (daemon starting up or degraded)
    #[error("Service unavailable")]
    ServiceUnavailable {
        /// Request ID for tracing
        request_id: String,
    },

    /// Method not allowed
    #[error("Method not allowed: {method}")]
    MethodNotAllowed {
        /// HTTP method that was attempted
        method: String,
        /// Request ID for tracing
        request_id: String,
    },

    /// Conflict (resource state prevents operation)
    #[error("Conflict: {message}")]
    Conflict {
        /// Error message
        message: String,
        /// Request ID for tracing
        request_id: String,
    },
}

impl ApiError {
    /// Get the error code for this error (machine-readable)
    #[must_use] 
    pub fn code(&self) -> &'static str {
        match self {
            ApiError::Internal { .. } => "internal_error",
            ApiError::NotFound { .. } => "not_found",
            ApiError::BadRequest { .. } => "bad_request",
            ApiError::ServiceUnavailable { .. } => "service_unavailable",
            ApiError::MethodNotAllowed { .. } => "method_not_allowed",
            ApiError::Conflict { .. } => "conflict",
        }
    }

    /// Get the HTTP status code for this error
    #[must_use] 
    pub fn status_code(&self) -> StatusCode {
        match self {
            ApiError::Internal { .. } => StatusCode::INTERNAL_SERVER_ERROR,
            ApiError::NotFound { .. } => StatusCode::NOT_FOUND,
            ApiError::BadRequest { .. } => StatusCode::BAD_REQUEST,
            ApiError::ServiceUnavailable { .. } => StatusCode::SERVICE_UNAVAILABLE,
            ApiError::MethodNotAllowed { .. } => StatusCode::METHOD_NOT_ALLOWED,
            ApiError::Conflict { .. } => StatusCode::CONFLICT,
        }
    }

    /// Get the request ID for this error
    #[must_use] 
    pub fn request_id(&self) -> &str {
        match self {
            ApiError::Internal { request_id, .. } => request_id,
            ApiError::NotFound { request_id, .. } => request_id,
            ApiError::BadRequest { request_id, .. } => request_id,
            ApiError::ServiceUnavailable { request_id } => request_id,
            ApiError::MethodNotAllowed { request_id, .. } => request_id,
            ApiError::Conflict { request_id, .. } => request_id,
        }
    }

    /// Create an internal error (with request ID)
    pub fn internal(message: impl Into<String>, request_id: impl Into<String>) -> Self {
        Self::Internal {
            message: message.into(),
            request_id: request_id.into(),
        }
    }

    /// Create an internal error (without request ID - will be set by middleware)
    pub fn internal_error(message: impl Into<String>) -> Self {
        Self::Internal {
            message: message.into(),
            request_id: "pending".to_string(),
        }
    }

    /// Create a not found error (with request ID)
    pub fn not_found(
        resource_type: impl Into<String>,
        resource_id: impl Into<String>,
        request_id: impl Into<String>,
    ) -> Self {
        Self::NotFound {
            resource_type: resource_type.into(),
            resource_id: resource_id.into(),
            request_id: request_id.into(),
        }
    }

    /// Create a not found error (without request ID - will be set by middleware)
    pub fn not_found_simple(
        resource_type: impl Into<String>,
        resource_id: impl Into<String>,
    ) -> Self {
        Self::NotFound {
            resource_type: resource_type.into(),
            resource_id: resource_id.into(),
            request_id: "pending".to_string(),
        }
    }

    /// Create a bad request error (with request ID)
    pub fn bad_request(message: impl Into<String>, request_id: impl Into<String>) -> Self {
        Self::BadRequest {
            message: message.into(),
            request_id: request_id.into(),
            details: None,
        }
    }

    /// Create a bad request error (without request ID - will be set by middleware)
    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::BadRequest {
            message: message.into(),
            request_id: "pending".to_string(),
            details: None,
        }
    }

    /// Create a service unavailable error (with request ID)
    pub fn service_unavailable(request_id: impl Into<String>) -> Self {
        Self::ServiceUnavailable {
            request_id: request_id.into(),
        }
    }

    /// Create a conflict error (with request ID)
    pub fn conflict(message: impl Into<String>, request_id: impl Into<String>) -> Self {
        Self::Conflict {
            message: message.into(),
            request_id: request_id.into(),
        }
    }

    /// Set the request ID for this error
    pub fn with_request_id(self, request_id: impl Into<String>) -> Self {
        let request_id = request_id.into();
        match self {
            Self::Internal { message, .. } => Self::Internal {
                message,
                request_id,
            },
            Self::NotFound {
                resource_type,
                resource_id,
                ..
            } => Self::NotFound {
                resource_type,
                resource_id,
                request_id,
            },
            Self::BadRequest {
                message, details, ..
            } => Self::BadRequest {
                message,
                request_id,
                details,
            },
            Self::ServiceUnavailable { .. } => Self::ServiceUnavailable { request_id },
            Self::MethodNotAllowed { method, .. } => Self::MethodNotAllowed { method, request_id },
            Self::Conflict { message, .. } => Self::Conflict {
                message,
                request_id,
            },
        }
    }

    /// Add details to a bad request error
    #[must_use] 
    pub fn with_details(self, details: serde_json::Value) -> Self {
        match self {
            Self::BadRequest {
                message,
                request_id,
                ..
            } => Self::BadRequest {
                message,
                request_id,
                details: Some(details),
            },
            _ => self,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        let code = self.code().to_string();
        let request_id = self.request_id().to_string();

        let message = match &self {
            ApiError::Internal { message, .. } => message.clone(),
            ApiError::NotFound {
                resource_type,
                resource_id,
                ..
            } => format!("{resource_type} not found: {resource_id}"),
            ApiError::BadRequest { message, .. } => message.clone(),
            ApiError::ServiceUnavailable { .. } => "Service unavailable".to_string(),
            ApiError::MethodNotAllowed { method, .. } => {
                format!("HTTP method {method} not allowed")
            }
            ApiError::Conflict { message, .. } => message.clone(),
        };

        let details = match self {
            ApiError::BadRequest { details, .. } => details,
            _ => None,
        };

        let error_response = ErrorResponse {
            error: ErrorDetail {
                code,
                message,
                request_id,
                details,
            },
        };

        (status, Json(error_response)).into_response()
    }
}

/// Extension trait for Results to add request ID context
pub trait ResultExt<T> {
    /// Convert any error to an internal API error with request ID
    fn map_internal_error(self, request_id: impl Into<String>) -> Result<T, ApiError>;
}

impl<T, E: std::fmt::Display> ResultExt<T> for Result<T, E> {
    fn map_internal_error(self, request_id: impl Into<String>) -> Result<T, ApiError> {
        self.map_err(|e| ApiError::internal(e.to_string(), request_id))
    }
}

/// Convert JSON extraction errors to API errors
impl From<(JsonRejection, String)> for ApiError {
    fn from((err, request_id): (JsonRejection, String)) -> Self {
        match err {
            JsonRejection::JsonSyntaxError(_) => {
                ApiError::bad_request("Invalid JSON syntax", request_id)
            }
            JsonRejection::JsonDataError(_) => ApiError::bad_request(
                "Invalid JSON data (type mismatch or validation failed)",
                request_id,
            ),
            JsonRejection::MissingJsonContentType(_) => {
                ApiError::bad_request("Content-Type must be application/json", request_id)
            }
            _ => ApiError::bad_request("Failed to parse JSON body", request_id),
        }
    }
}
