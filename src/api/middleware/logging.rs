//! Request Logging Middleware
//!
//! Logs all HTTP requests with timing information.

use axum::{
    body::Body,
    extract::ConnectInfo,
    http::{Method, Request, Response, Uri},
    middleware::Next,
};
use std::net::SocketAddr;
use std::time::Instant;
use tracing::{info, warn};

use crate::api::middleware::request_id::get_request_id;

/// Log level for requests
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LogLevel {
    /// Log all requests at info level
    Info,
    /// Log errors at warn level, successes at debug
    Debug,
    /// Log errors at warn level, don't log successes
    Error,
}

impl Default for LogLevel {
    fn default() -> Self {
        LogLevel::Info
    }
}

/// Middleware that logs HTTP requests
pub async fn logging_middleware(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    request: Request<Body>,
    next: Next,
) -> Response<Body> {
    let start = Instant::now();
    let method = request.method().clone();
    let uri = request.uri().clone();
    let request_id = get_request_id(&request)
        .map(|r| r.0.clone())
        .unwrap_or_default();

    // Log request start
    tracing::debug!(
        method = %method,
        uri = %uri,
        request_id = %request_id,
        client = %addr,
        "Request started"
    );

    // Process request
    let response = next.run(request).await;

    // Calculate duration
    let duration = start.elapsed();
    let status = response.status();

    // Log response
    let duration_ms = duration.as_secs_f64() * 1000.0;

    if status.is_server_error() {
        warn!(
            method = %method,
            uri = %uri,
            status = status.as_u16(),
            request_id = %request_id,
            duration_ms = %format!("{:.2}", duration_ms),
            "Request failed"
        );
    } else {
        info!(
            method = %method,
            uri = %uri,
            status = status.as_u16(),
            request_id = %request_id,
            duration_ms = %format!("{:.2}", duration_ms),
            "Request completed"
        );
    }

    response
}

/// Alternative logging middleware without ConnectInfo
///
/// Use this when you don't have the ConnectInfo layer
pub async fn logging_middleware_no_remote_addr(
    request: Request<Body>,
    next: Next,
) -> Response<Body> {
    let start = Instant::now();
    let method = request.method().clone();
    let uri = request.uri().clone();
    let request_id = get_request_id(&request)
        .map(|r| r.0.clone())
        .unwrap_or_default();

    // Process request
    let response = next.run(request).await;

    // Calculate duration
    let duration = start.elapsed();
    let status = response.status();

    // Log response
    let duration_ms = duration.as_secs_f64() * 1000.0;

    if status.is_server_error() {
        warn!(
            method = %method,
            uri = %uri,
            status = status.as_u16(),
            request_id = %request_id,
            duration_ms = %format!("{:.2}", duration_ms),
            "Request failed"
        );
    } else {
        info!(
            method = %method,
            uri = %uri,
            status = status.as_u16(),
            request_id = %request_id,
            duration_ms = %format!("{:.2}", duration_ms),
            "Request completed"
        );
    }

    response
}
