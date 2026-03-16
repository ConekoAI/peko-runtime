//! Pekobot HTTP API Server
//!
//! This module provides the HTTP API for the Pekobot daemon, serving as the
//! single control point for all runtime operations.
//!
//! ## Architecture
//!
//! The API server is built on Axum and provides:
//! - RESTful endpoints for agent/instance management
//! - Server-Sent Events (SSE) for streaming responses
//! - WebSocket endpoints for bidirectional communication
//! - Consistent error handling and response formatting
//!
//! ## Default Configuration
//!
//! - **Host:** `127.0.0.1` (localhost only by default)
//! - **Port:** `11434`
//!
//! Binding to non-loopback addresses requires explicit configuration and
//! will log a security warning.

pub mod error;
pub mod middleware;
pub mod routes;
pub mod server;
pub mod state;
pub mod types;

pub use error::ApiError;
pub use server::{ApiServer, ServerConfig};
pub use state::AppState;
pub use types::*;

/// API version string
pub const API_VERSION: &str = "1.0";

/// Default port for the HTTP API
pub const DEFAULT_PORT: u16 = 11435;

/// Default host for the HTTP API (localhost only)
pub const DEFAULT_HOST: &str = "127.0.0.1";

/// Header name for Pekobot version
pub const VERSION_HEADER: &str = "X-Pekobot-Version";

/// Header name for request ID
pub const REQUEST_ID_HEADER: &str = "X-Request-ID";

/// Pekobot version string (re-export from crate)
pub const VERSION: &str = crate::VERSION;
