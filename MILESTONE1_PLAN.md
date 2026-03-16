# Milestone 1: HTTP API Server Foundation — Implementation Plan

**Version:** 1.0  
**Date:** 2026-03-16  
**Status:** Ready for Implementation  

This document provides the detailed implementation plan for Milestone 1, including file-by-file specifications, type definitions, and acceptance criteria.

---

## Overview

Milestone 1 establishes the HTTP API server as the single control point for all Pekobot runtime operations. The server will be built using Axum and will handle all API requests defined in `API_CONTRACT.md`.

### Deliverables

1. HTTP server listening on configurable address (default: `127.0.0.1:11435`)
2. `GET /health` endpoint returning daemon status
3. `GET /info` endpoint returning version and capabilities
4. Standard headers (`X-Pekobot-Version`, `X-Request-ID`) on all responses
5. Consistent error envelope format
6. Warning logged when binding to non-loopback address
7. Graceful shutdown handling

---

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                         Daemon                               │
│  ┌─────────────────────────────────────────────────────┐    │
│  │                 HTTP Server (Axum)                   │    │
│  │  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐   │    │
│  │  │  Routes     │  │ Middleware  │  │  State      │   │    │
│  │  │  (api/      │  │  (version,  │  │  (shared    │   │    │
│  │  │   routes)   │  │   req_id)   │  │   state)    │   │    │
│  │  └─────────────┘  └─────────────┘  └─────────────┘   │    │
│  └─────────────────────────────────────────────────────┘    │
│                         │                                    │
│                         ▼                                    │
│  ┌─────────────────────────────────────────────────────┐    │
│  │              Existing Daemon Components              │    │
│  │         (scheduler, session manager, etc.)           │    │
│  └─────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────┘
```

---

## File Structure

```
src/api/
├── mod.rs              # Module exports, server initialization
├── server.rs           # Axum server setup, graceful shutdown
├── state.rs            # Shared application state
├── error.rs            # Error types and response conversion
├── types.rs            # Request/response types
├── middleware/
│   ├── mod.rs          # Middleware exports
│   ├── version.rs      # X-Pekobot-Version header
│   ├── request_id.rs   # X-Request-ID header
│   └── logging.rs      # Request logging
└── routes/
    ├── mod.rs          # Route aggregation
    ├── health.rs       # GET /health
    └── info.rs         # GET /info
```

---

## Implementation Tasks

### Task 1.1: Create Module Structure

**Files to create:**
- `src/api/mod.rs`
- `src/api/server.rs`
- `src/api/state.rs`
- `src/api/error.rs`
- `src/api/types.rs`
- `src/api/middleware/mod.rs`
- `src/api/middleware/version.rs`
- `src/api/middleware/request_id.rs`
- `src/api/middleware/logging.rs`
- `src/api/routes/mod.rs`
- `src/api/routes/health.rs`
- `src/api/routes/info.rs`

**Files to modify:**
- `src/lib.rs` - Add `pub mod api;`
- `src/daemon/mod.rs` - Integrate HTTP server

### Task 1.2: Define Types and Error Handling

**In `src/api/types.rs`:**

```rust
// Request/response types for API endpoints
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Health check response
#[derive(Debug, Clone, Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
    pub uptime_seconds: u64,
    pub instance_count: u64,
    pub team_count: u64,
}

/// Daemon info response
#[derive(Debug, Clone, Serialize)]
pub struct InfoResponse {
    pub version: String,
    pub api_version: String,
    pub workspace: String,
    pub port: u16,
    pub pid: u32,
    pub platform: String,
    pub capabilities: CapabilitiesInfo,
}

#[derive(Debug, Clone, Serialize)]
pub struct CapabilitiesInfo {
    pub streaming: bool,
    pub websocket: bool,
    pub teams: bool,
}

/// Standard error response envelope
#[derive(Debug, Clone, Serialize)]
pub struct ErrorResponse {
    pub error: ErrorDetail,
}

#[derive(Debug, Clone, Serialize)]
pub struct ErrorDetail {
    pub code: String,
    pub message: String,
    pub request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}
```

**In `src/api/error.rs`:**

```rust
// Error types and conversion to HTTP responses
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("Internal server error: {0}")]
    Internal(String),
    
    #[error("Not found: {0}")]
    NotFound(String),
    
    #[error("Bad request: {0}")]
    BadRequest(String),
    
    #[error("Service unavailable")]
    ServiceUnavailable,
}

impl ApiError {
    pub fn code(&self) -> &'static str {
        match self {
            ApiError::Internal(_) => "internal_error",
            ApiError::NotFound(_) => "not_found",
            ApiError::BadRequest(_) => "bad_request",
            ApiError::ServiceUnavailable => "service_unavailable",
        }
    }
    
    pub fn status_code(&self) -> StatusCode {
        match self {
            ApiError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            ApiError::NotFound(_) => StatusCode::NOT_FOUND,
            ApiError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ApiError::ServiceUnavailable => StatusCode::SERVICE_UNAVAILABLE,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        // Build error envelope
        let body = Json(json!({
            "error": {
                "code": self.code(),
                "message": self.to_string(),
                "request_id": "TODO", // Extract from request context
            }
        }));
        
        (self.status_code(), body).into_response()
    }
}
```

### Task 1.3: Implement Middleware

**In `src/api/middleware/version.rs`:**

Add middleware that injects `X-Pekobot-Version` header on all responses.

**In `src/api/middleware/request_id.rs`:**

Add middleware that:
- Reads `X-Request-ID` from incoming requests
- Generates a new UUID if not present
- Echoes it back in the response header
- Makes it available to route handlers via request extensions

**In `src/api/middleware/logging.rs`:**

Add request logging middleware with:
- Request method and path
- Request ID
- Response status
- Duration

### Task 1.4: Implement Routes

**In `src/api/routes/health.rs`:**

```rust
use axum::{
    extract::State,
    http::StatusCode,
    response::Json,
};
use crate::api::types::HealthResponse;
use crate::api::state::AppState;
use std::time::SystemTime;

pub async fn health_check(
    State(state): State<AppState>,
) -> Result<Json<HealthResponse>, StatusCode> {
    let uptime = SystemTime::now()
        .duration_since(state.started_at)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    
    // TODO: Get actual counts from daemon
    let instance_count = 0;
    let team_count = 0;
    
    let response = HealthResponse {
        status: "ok".to_string(),
        version: crate::VERSION.to_string(),
        uptime_seconds: uptime,
        instance_count,
        team_count,
    };
    
    Ok(Json(response))
}
```

**In `src/api/routes/info.rs`:**

```rust
use axum::{
    extract::State,
    response::Json,
};
use crate::api::types::{CapabilitiesInfo, InfoResponse};
use crate::api::state::AppState;

pub async fn daemon_info(
    State(state): State<AppState>,
) -> Json<InfoResponse> {
    Json(InfoResponse {
        version: crate::VERSION.to_string(),
        api_version: "1.0".to_string(),
        workspace: state.workspace_path.display().to_string(),
        port: state.port,
        pid: std::process::id(),
        platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        capabilities: CapabilitiesInfo {
            streaming: true,
            websocket: true,
            teams: true,
        },
    })
}
```

### Task 1.5: Implement Server

**In `src/api/server.rs`:**

```rust
use axum::{
    routing::get,
    Router,
};
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tracing::{info, warn};

pub struct ApiServer {
    config: ServerConfig,
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub workspace_path: std::path::PathBuf,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 11435,
            workspace_path: std::path::PathBuf::from(".pekobot"),
        }
    }
}

impl ApiServer {
    pub fn new(config: ServerConfig) -> Self {
        // Log warning if binding to non-loopback
        if config.host != "127.0.0.1" && config.host != "localhost" {
            warn!(
                "WARNING: Binding to non-loopback address '{}'. \
                 This may expose the daemon to network access. \
                 Use only in trusted network environments.",
                config.host
            );
        }
        
        Self { config }
    }
    
    pub async fn run(self, shutdown_rx: tokio::sync::oneshot::Receiver<()>) -> anyhow::Result<()> {
        let addr: SocketAddr = format!("{}:{}", self.config.host, self.config.port)
            .parse()?;
        
        let app = self.create_router();
        
        let listener = TcpListener::bind(&addr).await?;
        info!("🌐 HTTP API server listening on http://{}", addr);
        
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
                info!("🛑 HTTP API server shutting down...");
            })
            .await?;
        
        info!("👋 HTTP API server stopped");
        Ok(())
    }
    
    fn create_router(&self) -> Router {
        // Build routes
        Router::new()
            .route("/health", get(routes::health::health_check))
            .route("/info", get(routes::info::daemon_info))
            // Add state and middleware
            .layer(...)
    }
}
```

### Task 1.6: Integrate with Daemon

**In `src/daemon/mod.rs`:**

Add HTTP server startup to the daemon's run loop:

```rust
pub async fn run(mut self) -> Result<()> {
    // ... existing setup ...
    
    // Start HTTP API server
    let api_config = api::ServerConfig {
        host: self.config.host.clone(),
        port: self.config.port,
        workspace_path: self.config.data_dir.clone(),
    };
    
    let api_server = api::ApiServer::new(api_config);
    let (api_shutdown_tx, api_shutdown_rx) = tokio::sync::oneshot::channel();
    
    let api_handle = tokio::spawn(async move {
        if let Err(e) = api_server.run(api_shutdown_rx).await {
            error!("API server error: {}", e);
        }
    });
    
    // Main daemon loop
    loop {
        tokio::select! {
            // ... existing handlers ...
            
            Some(cmd) = self.command_rx.recv() => {
                match cmd {
                    DaemonCommand::Shutdown => {
                        // Signal API server to shutdown
                        let _ = api_shutdown_tx.send(());
                        break;
                    }
                    // ... other commands ...
                }
            }
        }
    }
    
    // Wait for API server to finish
    let _ = api_handle.await;
    
    Ok(())
}
```

### Task 1.7: Update Configuration

**In `src/daemon/mod.rs`:**

Add host and port to `DaemonConfig`:

```rust
pub struct DaemonConfig {
    // ... existing fields ...
    pub host: String,
    pub port: u16,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            // ... existing defaults ...
            host: "127.0.0.1".to_string(),
            port: 11435,
        }
    }
}
```

---

## Testing Plan

### Unit Tests

**In `src/api/tests.rs` (or inline):**

1. Test health endpoint returns 200 with correct structure
2. Test info endpoint returns correct version
3. Test X-Pekobot-Version header present on all responses
4. Test X-Request-ID echo behavior
5. Test error envelope format
6. Test graceful shutdown

### Integration Tests

**In `tests/api_integration_tests.rs`:**

```rust
#[tokio::test]
async fn test_health_endpoint() {
    // Start daemon
    let daemon = start_test_daemon().await;
    
    // Make request
    let response = reqwest::get("http://localhost:11435/health")
        .await
        .unwrap();
    
    assert_eq!(response.status(), 200);
    
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["status"], "ok");
    assert!(body.get("version").is_some());
    assert!(body.get("uptime_seconds").is_some());
}

#[tokio::test]
async fn test_request_id_echo() {
    let client = reqwest::Client::new();
    
    let response = client
        .get("http://localhost:11435/health")
        .header("X-Request-ID", "test-123")
        .send()
        .await
        .unwrap();
    
    assert_eq!(
        response.headers().get("X-Request-ID").unwrap(),
        "test-123"
    );
}

#[tokio::test]
async fn test_non_loopback_warning() {
    // Verify warning is logged when binding to 0.0.0.0
}
```

### Manual Testing

```bash
# Start daemon
pekobot daemon start

# Test health endpoint
curl http://localhost:11435/health

# Test info endpoint
curl http://localhost:11435/info

# Verify headers
curl -I http://localhost:11435/health | grep -i "X-Pekobot-Version"
curl -I http://localhost:11435/health -H "X-Request-ID: abc123" | grep -i "X-Request-ID"

# Test with non-loopback (should see warning)
# Edit runtime.toml to set host = "0.0.0.0"
pekobot daemon start 2>&1 | grep "WARNING"
```

---

## Acceptance Criteria

- [ ] HTTP server starts on `127.0.0.1:11435` by default
- [ ] `GET /health` returns `200 OK` with `{"status":"ok",...}`
- [ ] `GET /info` returns version, workspace, port, pid, platform, capabilities
- [ ] All responses include `X-Pekobot-Version` header
- [ ] Requests with `X-Request-ID` have it echoed in response
- [ ] Requests without `X-Request-ID` get a generated UUID in response
- [ ] Error responses use standard envelope format
- [ ] Binding to non-loopback address logs a warning
- [ ] Server shuts down gracefully on daemon stop
- [ ] All tests pass

---

## Dependencies

**Cargo.toml additions:**

```toml
[dependencies]
# Already present: axum, tokio, serde, serde_json, tracing
# May need to add:
tower = "0.4"          # For middleware layering
tower-http = "0.5"     # For CORS, logging middleware (optional)
```

**Existing dependencies to use:**
- `axum` (already in Cargo.toml)
- `tokio` (already in Cargo.toml)
- `serde`/`serde_json` (already in Cargo.toml)
- `tracing` (already in Cargo.toml)
- `uuid` (already in Cargo.toml)

---

## Notes

1. **Keep it minimal:** This milestone only implements `/health` and `/info`. Other endpoints come in later milestones.

2. **State management:** For now, `AppState` can be minimal. It will grow as we add more endpoints that need access to daemon internals.

3. **Error handling:** Use `thiserror` for error types (already a dependency). Convert to Axum responses using `IntoResponse`.

4. **Middleware:** Axum's middleware system uses `tower::Layer`. Keep middleware simple and composable.

5. **Graceful shutdown:** Use `tokio::sync::oneshot` for clean shutdown signaling between daemon and API server.

---

*Version: 1.0 · Last Updated: 2026-03-16 · Status: Ready for Implementation*
