//! Async Task API Routes
//!
//! Implements daemon-side endpoints for async tool execution (ADR-020):
//! - POST /async/tasks — Spawn a new async task
//! - GET /async/tasks/{id} — Get task status and result
//! - DELETE /async/tasks/{id} — Cancel a task
//! - GET /async/tasks — List tasks (optionally filter by session_key)

use crate::agent::async_tool_framework::{
    AsyncToolConfig, AsyncTaskId, AsyncTaskReceipt, AsyncTaskResult, AsyncTaskStatus,
};
use crate::api::error::ApiError;
use crate::api::state::AppState;
use axum::{
    extract::{Path, Query, State},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{debug, info};

/// Request to spawn an async task
#[derive(Debug, Serialize, Deserialize)]
pub struct SpawnAsyncTaskRequest {
    pub task_id: AsyncTaskId,
    pub tool_name: String,
    pub params: serde_json::Value,
    pub session_key: String,
    #[serde(default)]
    pub workspace: std::path::PathBuf,
    pub config: AsyncToolConfig,
}

/// Response for task status query
#[derive(Debug, Serialize, Deserialize)]
pub struct AsyncTaskStatusResponse {
    pub task_id: AsyncTaskId,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Query parameters for listing tasks
#[derive(Debug, Deserialize, Default)]
pub struct ListAsyncTasksQuery {
    pub session_key: Option<String>,
}

/// Create the async tasks router
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/async/tasks", post(spawn_async_task).get(list_async_tasks))
        .route("/async/tasks/:id", get(get_async_task).delete(cancel_async_task))
}

/// Spawn a new async task
async fn spawn_async_task(
    State(state): State<AppState>,
    Json(req): Json<SpawnAsyncTaskRequest>,
) -> Result<Json<AsyncTaskReceipt>, ApiError> {
    info!(
        task_id = %req.task_id,
        tool_name = %req.tool_name,
        "Spawning async task via daemon API"
    );

    let receipt = state
        .runtime
        .execute_tool_async(
            req.task_id,
            req.tool_name,
            req.params,
            req.session_key,
            req.workspace,
            req.config,
        )
        .await
        .map_err(|e| ApiError::internal_error(format!("Failed to spawn async task: {e}")))?;

    Ok(Json(receipt))
}

/// Get the status of an async task
async fn get_async_task(
    State(state): State<AppState>,
    Path(id): Path<AsyncTaskId>,
) -> Result<Json<AsyncTaskStatusResponse>, ApiError> {
    debug!(task_id = %id, "Getting async task status");

    let status = state.runtime.async_task_executor().check_status(&id).await;

    let response = match status {
        Some(AsyncTaskStatus::Completed { result }) => AsyncTaskStatusResponse {
            task_id: id.clone(),
            status: "completed".to_string(),
            result: Some(serde_json::json!({ "result": result })),
            error: None,
        },
        Some(AsyncTaskStatus::Failed { error }) => AsyncTaskStatusResponse {
            task_id: id.clone(),
            status: "failed".to_string(),
            result: None,
            error: Some(error),
        },
        Some(AsyncTaskStatus::TimedOut { error }) => AsyncTaskStatusResponse {
            task_id: id.clone(),
            status: "timed_out".to_string(),
            result: None,
            error: Some(error),
        },
        Some(AsyncTaskStatus::Cancelled) => AsyncTaskStatusResponse {
            task_id: id.clone(),
            status: "cancelled".to_string(),
            result: None,
            error: None,
        },
        Some(AsyncTaskStatus::Running) => AsyncTaskStatusResponse {
            task_id: id.clone(),
            status: "running".to_string(),
            result: None,
            error: None,
        },
        Some(AsyncTaskStatus::Pending) | None => AsyncTaskStatusResponse {
            task_id: id.clone(),
            status: "pending".to_string(),
            result: None,
            error: None,
        },
    };

    Ok(Json(response))
}

/// Cancel an async task
async fn cancel_async_task(
    State(state): State<AppState>,
    Path(id): Path<AsyncTaskId>,
) -> Result<Json<serde_json::Value>, ApiError> {
    info!(task_id = %id, "Cancelling async task");

    let cancelled = state
        .runtime
        .async_task_executor()
        .cancel(&id)
        .await
        .map_err(|e| ApiError::internal_error(format!("Failed to cancel async task: {e}")))?;

    Ok(Json(serde_json::json!({
        "task_id": id,
        "cancelled": cancelled,
    })))
}

/// List async tasks
async fn list_async_tasks(
    State(state): State<AppState>,
    Query(query): Query<ListAsyncTasksQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    debug!("Listing async tasks");

    let tasks = state
        .runtime
        .async_task_executor()
        .list_tasks(query.session_key.as_deref())
        .await;

    let task_summaries: Vec<_> = tasks
        .into_iter()
        .map(|entry| {
            serde_json::json!({
                "task_id": entry.task_id,
                "tool_name": entry.tool_name,
                "status": entry.status,
                "created_at": entry.created_at,
                "completed_at": entry.completed_at,
                "session_key": entry.parent_session_key,
            })
        })
        .collect();

    Ok(Json(serde_json::json!({
        "tasks": task_summaries,
        "total": task_summaries.len(),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::state::{AppState, DaemonConfigSnapshot};
    use axum::body::Body;
    use axum::http::Request;
    use tower::util::ServiceExt;

    async fn test_state() -> AppState {
        let temp_dir = tempfile::TempDir::new().unwrap();
        AppState::with_data_dir(
            temp_dir.path(),
            "127.0.0.1",
            11435,
            DaemonConfigSnapshot::default(),
            temp_dir.path().to_path_buf(),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn test_router_has_spawn_endpoint() {
        let app = router().with_state(test_state().await);

        let request = Request::builder()
            .uri("/async/tasks")
            .method("POST")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "task_id": "test_task_1",
                    "tool_name": "shell",
                    "params": {"command": "echo hello"},
                    "session_key": "agent1_session1",
                    "config": {
                        "delivery_mode": "QueueWhenBusy",
                        "timeout_secs": 60,
                        "cleanup_after_delivery": true
                    }
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        // Should return 200 with a receipt, or 500 if something goes wrong during execution
        // We accept either because this is a routing test
        assert!(
            response.status().is_success() || response.status().is_server_error(),
            "Expected success or server error, got {:?}",
            response.status()
        );
    }

    #[tokio::test]
    async fn test_router_has_get_endpoint() {
        let app = router().with_state(test_state().await);

        let request = Request::builder()
            .uri("/async/tasks/nonexistent")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), 200);
    }

    #[tokio::test]
    async fn test_router_has_cancel_endpoint() {
        let app = router().with_state(test_state().await);

        let request = Request::builder()
            .uri("/async/tasks/nonexistent")
            .method("DELETE")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), 200);
    }

    #[tokio::test]
    async fn test_router_has_list_endpoint() {
        let app = router().with_state(test_state().await);

        let request = Request::builder()
            .uri("/async/tasks")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), 200);
    }
}
