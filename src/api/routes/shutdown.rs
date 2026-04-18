//! Shutdown Endpoint
//!
//! Triggers graceful daemon shutdown.

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};

use crate::api::state::AppState;

/// Request body for shutdown
#[derive(Debug, Default, serde::Deserialize)]
pub struct ShutdownRequest {
    /// Optional: Force immediate shutdown (skip graceful)
    #[serde(default)]
    pub force: bool,
}

/// Response from shutdown endpoint
#[derive(Debug, serde::Serialize)]
pub struct ShutdownResponse {
    pub message: String,
}

/// POST /shutdown - Triggers graceful daemon shutdown
pub async fn shutdown(
    State(state): State<AppState>,
    Json(body): Json<ShutdownRequest>,
) -> Response {
    tracing::info!("Shutdown endpoint called with force={}", body.force);

    // Signal the daemon to shutdown via the shared state
    state.request_shutdown(body.force).await;

    (StatusCode::OK, Json(ShutdownResponse {
        message: "Daemon shutdown initiated".to_string(),
    })).into_response()
}
