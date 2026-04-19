//! Health Check Endpoint
//!
//! Provides service health information for monitoring and load balancers.

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};

use crate::api::state::AppState;
use crate::api::types::HealthResponse;

/// Health check handler
///
/// Returns 200 OK when healthy, 503 Service Unavailable when degraded.
///
/// # Response
///
/// ```json
/// {
///   "status": "ok",
///   "version": "0.1.0",
///   "uptime_seconds": 3600,
///   "instance_count": 5,
///   "team_count": 1
/// }
/// ```
pub async fn health_check(State(state): State<AppState>) -> Response {
    let uptime = state.uptime_seconds();
    let is_ready = state.is_ready().await;
    let is_degraded = state.is_degraded().await;
    let instance_count = state.instance_count().await;
    let team_count = state.team_count().await;
    tracing::info!("Health check: ready={}, degraded={}, uptime={}, instances={}, teams={}", is_ready, is_degraded, uptime, instance_count, team_count);

    // Determine status and HTTP code
    let (status_code, status_str) = if !is_ready {
        // Daemon is still starting up
        (StatusCode::SERVICE_UNAVAILABLE, "starting".to_string())
    } else if is_degraded {
        // Daemon is degraded
        (StatusCode::SERVICE_UNAVAILABLE, "degraded".to_string())
    } else {
        (StatusCode::OK, "ok".to_string())
    };

    // Query MCP server health
    let mcp_health = {
        let manager = state.runtime.mcp_manager();
        let manager = manager.read().await;
        let servers = manager.list_servers().await;
        drop(manager);

        if !servers.is_empty() {
            let healthy = servers.iter().filter(|s| s.healthy).count();
            let running = servers.iter().filter(|s| s.running).count();
            let degraded: Vec<String> = servers
                .iter()
                .filter(|s| s.running && !s.healthy)
                .map(|s| s.name.clone())
                .collect();

            Some(crate::api::types::McpServersHealth {
                total: servers.len(),
                healthy,
                running,
                degraded,
            })
        } else {
            None
        }
    };

    let response = HealthResponse {
        status: status_str,
        version: crate::VERSION.to_string(),
        uptime_seconds: uptime,
        instance_count,
        team_count,
        mcp_servers: mcp_health,
    };

    (status_code, Json(response)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::state::{AppState, DaemonConfigSnapshot};
    use axum::body::to_bytes;
    use axum::http::StatusCode;
    use tempfile::TempDir;

    async fn test_state() -> AppState {
        let temp_dir = TempDir::new().unwrap();
        let state = AppState::with_data_dir(
            temp_dir.path(),
            "127.0.0.1",
            11435,
            DaemonConfigSnapshot::default(),
            temp_dir.path().to_path_buf(),
        )
        .await
        .unwrap();
        // Mark as ready so health check returns ok (not "starting")
        state.set_ready(true).await;
        state
    }

    #[tokio::test]
    async fn test_health_check_returns_ok() {
        let state = test_state().await;
        let response = health_check(State(state)).await;

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_health_check_response_format() {
        let state = test_state().await;
        state.set_instance_count(5).await;
        state.set_team_count(2).await;

        let response = health_check(State(state)).await;
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["status"], "ok");
        assert!(json["version"].as_str().is_some());
        assert_eq!(json["instance_count"], 5);
        assert_eq!(json["team_count"], 2);
        assert!(json["uptime_seconds"].as_u64().is_some());
    }

    #[tokio::test]
    async fn test_health_check_degraded() {
        let state = test_state().await;
        state.mark_degraded().await;

        let response = health_check(State(state)).await;

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["status"], "degraded");
    }

    #[tokio::test]
    async fn test_health_check_starting() {
        let temp_dir = TempDir::new().unwrap();
        let state = AppState::with_data_dir(
            temp_dir.path(),
            "127.0.0.1",
            11435,
            DaemonConfigSnapshot::default(),
            temp_dir.path().to_path_buf(),
        )
        .await
        .unwrap();
        // NOT marking as ready — should return "starting"

        let response = health_check(State(state)).await;

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["status"], "starting");
    }
}
