//! HTTP Middleware
//!
//! This module provides Axum middleware for:
//! - Injecting version headers
//! - Managing request IDs
//! - Logging requests
//! - Error handling

pub mod logging;
pub mod request_id;
pub mod version;

/// Create the middleware stack for the API
///
/// The order matters - middleware is executed from bottom to top
/// for requests, and top to bottom for responses.
pub fn middleware_stack() -> axum::Router {
    // This function will be used to apply middleware to routes
    // For now, it's a placeholder
    axum::Router::new()
}

/// Re-export middleware functions for direct use
pub use logging::logging_middleware;
pub use request_id::request_id_middleware;
pub use version::version_middleware;
