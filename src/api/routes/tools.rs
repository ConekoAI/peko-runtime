//! Tool Execution API Routes
//!
//! Implements daemon-side endpoints for synchronous tool execution (ADR-021 Phase 1):
//! - POST /tools/execute — Execute a tool synchronously
//! - GET /tools — List all available tools

use crate::api::error::ApiError;
use crate::api::state::AppState;
use crate::extensions::types::ToolMetadata;
use axum::{
    extract::State,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

/// Request to execute a tool synchronously
#[derive(Debug, Serialize, Deserialize)]
pub struct ExecuteToolRequest {
    pub tool_name: String,
    pub params: serde_json::Value,
    #[serde(default)]
    pub workspace: std::path::PathBuf,
}

/// Response from a synchronous tool execution
#[derive(Debug, Serialize, Deserialize)]
pub struct ExecuteToolResponse {
    pub tool_name: String,
    pub result: serde_json::Value,
}

/// Response for listing tools
#[derive(Debug, Serialize, Deserialize)]
pub struct ListToolsResponse {
    pub tools: Vec<ToolMetadata>,
    pub total: usize,
}

/// Create the tools router
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/tools/execute", post(execute_tool))
        .route("/tools", get(list_tools))
}

/// Execute a tool synchronously
async fn execute_tool(
    State(state): State<AppState>,
    Json(req): Json<ExecuteToolRequest>,
) -> Result<Json<ExecuteToolResponse>, ApiError> {
    info!(
        tool_name = %req.tool_name,
        "Executing tool synchronously via daemon API"
    );

    // Validate workspace path if provided
    if !req.workspace.as_os_str().is_empty() && !req.workspace.exists() {
        return Err(ApiError::invalid_request(format!(
            "Workspace path does not exist: {}",
            req.workspace.display()
        )));
    }

    let result = if req.workspace.as_os_str().is_empty() {
        state
            .runtime
            .execute_tool(&req.tool_name, req.params)
            .await
    } else {
        state
            .runtime
            .execute_tool_with_workspace(&req.tool_name, req.params, &req.workspace)
            .await
    }
    .map_err(|e| ApiError::internal_error(format!("Tool execution failed: {e}")))?;

    Ok(Json(ExecuteToolResponse {
        tool_name: req.tool_name,
        result,
    }))
}

/// List all available tools
async fn list_tools(
    State(state): State<AppState>,
) -> Result<Json<ListToolsResponse>, ApiError> {
    debug!("Listing available tools");

    let tools = state.runtime.list_tools().await;
    let total = tools.len();

    Ok(Json(ListToolsResponse { tools, total }))
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
    async fn test_router_has_execute_endpoint() {
        let app = router().with_state(test_state().await);

        let request = Request::builder()
            .uri("/tools/execute")
            .method("POST")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "tool_name": "shell",
                    "params": {"command": "echo hello"}
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        // Should return 200 with result, or 500 if something goes wrong during execution
        assert!(
            response.status().is_success() || response.status().is_server_error(),
            "Expected success or server error, got {:?}",
            response.status()
        );
    }

    #[tokio::test]
    async fn test_router_has_list_endpoint() {
        let app = router().with_state(test_state().await);

        let request = Request::builder()
            .uri("/tools")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), 200);
    }
}
