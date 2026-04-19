//! MCP Server Management API Routes
//!
//! Provides HTTP endpoints for monitoring and managing MCP servers:
//! - GET /mcp/servers — List all MCP servers and their status
//! - POST /mcp/servers/{name}/restart — Restart a specific MCP server

use crate::api::error::ApiError;
use crate::api::state::AppState;
use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

/// Response for listing MCP servers
#[derive(Debug, Serialize, Deserialize)]
pub struct ListMcpServersResponse {
    pub servers: Vec<McpServerInfo>,
    pub total: usize,
    pub healthy_count: usize,
    pub running_count: usize,
}

/// Individual MCP server status
#[derive(Debug, Serialize, Deserialize)]
pub struct McpServerInfo {
    pub name: String,
    pub running: bool,
    pub healthy: bool,
    pub restart_count: u32,
    pub consecutive_failures: u32,
    pub last_error: Option<String>,
    pub server_info: Option<String>,
    pub tool_count: usize,
}

/// Response from a restart request
#[derive(Debug, Serialize, Deserialize)]
pub struct RestartMcpServerResponse {
    pub name: String,
    pub success: bool,
    pub message: String,
}

/// Create the MCP server management router
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/mcp/servers", get(list_mcp_servers))
        .route("/mcp/servers/{name}/restart", post(restart_mcp_server))
}

/// List all MCP servers and their status
async fn list_mcp_servers(
    State(state): State<AppState>,
) -> Result<Json<ListMcpServersResponse>, ApiError> {
    let manager = state.runtime.mcp_manager();
    let manager = manager.read().await;
    let server_states = manager.list_servers().await;
    drop(manager);

    let mut servers = Vec::new();
    let mut healthy_count = 0;
    let mut running_count = 0;

    for server_state in server_states {
        if server_state.healthy {
            healthy_count += 1;
        }
        if server_state.running {
            running_count += 1;
        }

        servers.push(McpServerInfo {
            name: server_state.name.clone(),
            running: server_state.running,
            healthy: server_state.healthy,
            restart_count: server_state.restart_count,
            consecutive_failures: server_state.consecutive_failures,
            last_error: server_state.last_error.clone(),
            server_info: server_state.server_info.clone(),
            tool_count: server_state.tools.len(),
        });
    }

    Ok(Json(ListMcpServersResponse {
        total: servers.len(),
        healthy_count,
        running_count,
        servers,
    }))
}

/// Restart a specific MCP server
async fn restart_mcp_server(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<RestartMcpServerResponse>, ApiError> {
    info!("API request to restart MCP server: {}", name);

    let manager = state.runtime.mcp_manager();
    let mut manager = manager.write().await;

    match manager.restart_server(&name).await {
        Ok(()) => {
            info!("MCP server '{}' restarted successfully via API", name);
            Ok(Json(RestartMcpServerResponse {
                name,
                success: true,
                message: "Server restarted successfully".to_string(),
            }))
        }
        Err(e) => {
            warn!("Failed to restart MCP server '{}': {}", name, e);
            Err(ApiError::internal_error(format!(
                "Failed to restart MCP server '{}': {}",
                name, e
            )))
        }
    }
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
    async fn test_list_mcp_servers_returns_empty() {
        let state = test_state().await;
        let response = list_mcp_servers(State(state)).await.unwrap();

        assert_eq!(response.0.total, 0);
        assert_eq!(response.0.healthy_count, 0);
        assert_eq!(response.0.running_count, 0);
        assert!(response.0.servers.is_empty());
    }

    #[tokio::test]
    async fn test_mcp_router_has_routes() {
        let state = test_state().await;
        let app = router().with_state(state);

        // Test GET /mcp/servers
        let response = app
            .clone()
            .oneshot(Request::get("/mcp/servers").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
    }
}
