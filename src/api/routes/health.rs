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
    let is_degraded = state.is_degraded().await;
    let instance_count = state.instance_count().await;
    let team_count = state.team_count().await;

    let response = if is_degraded {
        HealthResponse {
            status: "degraded".to_string(),
            version: crate::VERSION.to_string(),
            uptime_seconds: uptime,
            instance_count,
            team_count,
        }
    } else {
        HealthResponse {
            status: "ok".to_string(),
            version: crate::VERSION.to_string(),
            uptime_seconds: uptime,
            instance_count,
            team_count,
        }
    };

    let status = if is_degraded {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    };

    (status, Json(response)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::state::{AppState, DaemonConfigSnapshot};
    use axum::body::to_bytes;
    use axum::http::StatusCode;

    fn test_state() -> AppState {
        AppState::new(
            "/tmp/test",
            "127.0.0.1",
            11435,
            DaemonConfigSnapshot::default(),
        )
    }

    #[tokio::test]
    async fn test_health_check_returns_ok() {
        let state = test_state();
        let response = health_check(State(state)).await;

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_health_check_response_format() {
        let state = test_state();
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
        let state = test_state();
        state.mark_degraded().await;

        let response = health_check(State(state)).await;

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["status"], "degraded");
    }
}
