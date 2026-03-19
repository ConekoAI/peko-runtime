//! Agent Configuration API Routes (Stateless Architecture)
//!
//! Implements agent configuration management endpoints for the stateless
//! cold-start architecture:
//! - GET /agents - List registered agent configurations
//! - POST /agents - Register new agent from image
//! - GET /agents/{name} - Get agent configuration
//! - DELETE /agents/{name} - Unregister agent
//! - POST /agents/{name}/execute - Execute agent (stateless)
//!
//! Note: No instance lifecycle endpoints (start/stop/status) in stateless model.
//! Agents are cold-started per request.

use crate::api::error::ApiError;
use crate::api::state::AppState;
use crate::api::types::{PaginatedResponse, PaginationParams};
use crate::image::registry::{ImageRegistry, RegistryConfig};
use crate::image::ImageRef;
use crate::observability::performance::PerformanceGuard;
use axum::{
    extract::{Path, Query, State},
    routing::{get, post},
    Json, Router,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::info;

/// Agent configuration response (stateless model)
#[derive(Debug, Clone, Serialize)]
pub struct AgentConfigResponse {
    /// Agent name (unique identifier)
    pub name: String,
    /// Source image reference
    pub image_ref: String,
    /// Pinned image digest
    pub image_digest: String,
    /// Team ID (if assigned)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
    /// Capabilities
    pub capabilities: Vec<String>,
    /// Registration timestamp
    pub registered_at: String,
    /// Last updated timestamp
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

/// Register agent request
#[derive(Debug, Deserialize)]
pub struct RegisterAgentRequest {
    /// Image reference, digest, or path
    pub image: String,
    /// Agent name (optional, derived from image if not provided)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Team ID to assign
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
    /// Environment variables
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,
}

/// Update agent request
#[derive(Debug, Deserialize)]
pub struct UpdateAgentRequest {
    /// New image reference
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// Team ID to assign
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
}

/// Execute agent request
#[derive(Debug, Deserialize)]
pub struct ExecuteAgentRequest {
    /// Session ID for persistence
    pub session_id: String,
    /// Message to send
    pub message: String,
    /// Optional execution context
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<serde_json::Value>,
    /// Optional timeout override (seconds)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
}

/// Execute agent response
#[derive(Debug, Serialize)]
pub struct ExecuteAgentResponse {
    /// Execution ID
    pub execution_id: String,
    /// Agent response
    pub response: String,
    /// Tool calls made
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCallResponse>,
    /// Token usage
    pub usage: TokenUsageResponse,
    /// Execution duration in milliseconds
    pub duration_ms: u64,
    /// Whether execution succeeded
    pub success: bool,
    /// Error message (if failed)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Tool call response
#[derive(Debug, Serialize)]
pub struct ToolCallResponse {
    /// Tool name
    pub name: String,
    /// Tool parameters
    pub parameters: serde_json::Value,
    /// Tool result (if available)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
}

/// Token usage response
#[derive(Debug, Serialize, Default)]
pub struct TokenUsageResponse {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

/// List all registered agents
async fn list_agents(
    State(state): State<AppState>,
    Query(params): Query<PaginationParams>,
) -> Result<Json<PaginatedResponse<AgentConfigResponse>>, ApiError> {
    let registry = state.config_registry();
    let configs = registry.list().await;

    let items: Vec<AgentConfigResponse> = configs
        .into_iter()
        .skip(params.offset())
        .take(params.limit())
        .map(|entry| AgentConfigResponse {
            name: entry.name.clone(),
            image_ref: entry.image_ref.clone(),
            image_digest: entry.image_digest.clone(),
            team_id: entry.team_id.clone(),
            capabilities: entry.capabilities(),
            registered_at: entry.registered_at.to_rfc3339(),
            updated_at: Some(entry.updated_at.to_rfc3339()),
        })
        .collect();

    Ok(Json(PaginatedResponse::new(items, false)))
}

/// Register new agent from image
async fn register_agent(
    State(state): State<AppState>,
    Json(request): Json<RegisterAgentRequest>,
) -> Result<Json<AgentConfigResponse>, ApiError> {
    // Start timing
    let _guard = PerformanceGuard::new("register_agent");

    // Parse image reference
    let image_ref = ImageRef::parse(&request.image)
        .map_err(|e| ApiError::bad_request(format!("Invalid image reference: {}", e), ""))?;

    // Determine agent name
    let name = request.name.unwrap_or_else(|| {
        // Derive name from image reference
        match &image_ref {
            ImageRef::LocalTag { name, .. } => name.clone(),
            ImageRef::RegistryRef { path, .. } => {
                path.split('/').last().unwrap_or("agent").to_string()
            }
            _ => format!("agent-{}", generate_short_id()),
        }
    });

    // Resolve image in registry
    let registry_path = state.workspace_path.join("registry");
    let config = RegistryConfig::new(&registry_path);
    let registry = ImageRegistry::new(config);

    let manifest = registry
        .resolve(&image_ref)
        .await
        .map_err(|e| ApiError::internal(format!("Failed to resolve image: {}", e), ""))?
        .ok_or_else(|| ApiError::not_found("image", request.image.clone(), ""))?;

    // Register in config registry
    let entry = state
        .config_registry()
        .register(&name, &image_ref, &registry, request.team_id.clone())
        .await
        .map_err(|e| ApiError::internal(format!("Failed to register agent: {}", e), ""))?;

    info!(
        "Registered agent '{}' from image {} (digest: {})",
        name, request.image, entry.image_digest
    );

    Ok(Json(AgentConfigResponse {
        name: entry.name.clone(),
        image_ref: entry.image_ref.clone(),
        image_digest: entry.image_digest.clone(),
        team_id: entry.team_id.clone(),
        capabilities: entry.capabilities(),
        registered_at: entry.registered_at.to_rfc3339(),
        updated_at: Some(entry.updated_at.to_rfc3339()),
    }))
}

/// Get agent configuration by name
async fn get_agent(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<AgentConfigResponse>, ApiError> {
    let entry = state
        .config_registry()
        .get(&name)
        .await
        .ok_or_else(|| ApiError::not_found("agent", name.clone(), ""))?;

    Ok(Json(AgentConfigResponse {
        name: entry.name.clone(),
        image_ref: entry.image_ref.clone(),
        image_digest: entry.image_digest.clone(),
        team_id: entry.team_id.clone(),
        capabilities: entry.capabilities(),
        registered_at: entry.registered_at.to_rfc3339(),
        updated_at: Some(entry.updated_at.to_rfc3339()),
    }))
}

/// Unregister agent
async fn unregister_agent(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<axum::http::StatusCode, ApiError> {
    // Check if agent exists
    if !state.config_registry().exists(&name).await {
        return Err(ApiError::not_found("agent", name, ""));
    }

    // Check if agent is currently executing
    if state.lifecycle().is_executing(&name).await {
        return Err(ApiError::conflict(
            format!("Agent '{}' is currently executing", name),
            "Wait for execution to complete or cancel it",
        ));
    }

    // Unregister
    state
        .config_registry()
        .unregister(&name)
        .await
        .map_err(|e| ApiError::internal(format!("Failed to unregister agent: {}", e), ""))?;

    info!("Unregistered agent '{}'", name);

    Ok(axum::http::StatusCode::NO_CONTENT)
}

/// Execute agent (stateless cold-start)
async fn execute_agent(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(request): Json<ExecuteAgentRequest>,
) -> Result<Json<ExecuteAgentResponse>, ApiError> {
    // Start timing
    let _guard = PerformanceGuard::new("execute_agent");

    // Check if agent is registered
    if !state.config_registry().exists(&name).await {
        return Err(ApiError::not_found("agent", name.clone(), ""));
    }

    // Build execution request
    let exec_request = crate::agent::stateless_service::ExecutionRequest {
        agent_name: name.clone(),
        session_id: request.session_id.clone(),
        message: request.message.clone(),
        context: request
            .context
            .map(|ctx| crate::agent::stateless_service::ExecutionContext {
                parent_message_id: ctx
                    .get("parent_message_id")
                    .and_then(|v| v.as_str().map(String::from)),
                metadata: std::collections::HashMap::new(),
            }),
        timeout_secs: request.timeout_secs,
    };

    // Execute
    let result = state
        .agent_service()
        .execute(exec_request)
        .await
        .map_err(|e| ApiError::internal(format!("Execution failed: {}", e), ""))?;

    // Convert tool calls to response format
    let tool_calls: Vec<ToolCallResponse> = result
        .tool_calls
        .into_iter()
        .map(|tc| ToolCallResponse {
            name: tc.name,
            parameters: tc.parameters,
            result: tc.result,
        })
        .collect();

    Ok(Json(ExecuteAgentResponse {
        execution_id: format!("exec_{}", generate_short_id()),
        response: result.response,
        tool_calls,
        usage: TokenUsageResponse {
            prompt_tokens: result.usage.input as u32,
            completion_tokens: result.usage.output as u32,
            total_tokens: result.usage.total as u32,
        },
        duration_ms: result.duration_ms,
        success: result.success,
        error: result.error,
    }))
}

/// Get agent metrics
async fn get_agent_metrics(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // Check if agent exists
    if !state.config_registry().exists(&name).await {
        return Err(ApiError::not_found("agent", name.clone(), ""));
    }

    // Get service metrics
    let metrics = state.agent_service().metrics().await;

    // Get agent-specific execution count (approximate from active)
    let is_executing = state.lifecycle().is_executing(&name).await;

    Ok(Json(serde_json::json!({
        "agent_name": name,
        "is_executing": is_executing,
        "service_metrics": {
            "total_executions": metrics.total_executions,
            "successful_executions": metrics.successful_executions,
            "failed_executions": metrics.failed_executions,
            "avg_cold_start_ms": metrics.avg_cold_start_ms,
        }
    })))
}

/// Update agent configuration
async fn update_agent(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(request): Json<UpdateAgentRequest>,
) -> Result<Json<AgentConfigResponse>, ApiError> {
    // Check if agent exists
    if !state.config_registry().exists(&name).await {
        return Err(ApiError::not_found("agent", name.clone(), ""));
    }

    // Check if agent is currently executing
    if state.lifecycle().is_executing(&name).await {
        return Err(ApiError::conflict(
            format!("Agent '{}' is currently executing", name),
            "Wait for execution to complete",
        ));
    }

    // Parse new image if provided
    let (image_ref, image_registry) = if let Some(img) = request.image {
        let image_ref = ImageRef::parse(&img)
            .map_err(|e| ApiError::bad_request(format!("Invalid image reference: {}", e), ""))?;

        let registry_path = state.workspace_path.join("registry");
        let config = RegistryConfig::new(&registry_path);
        let registry = ImageRegistry::new(config);

        // Verify image exists
        let _manifest = registry
            .resolve(&image_ref)
            .await
            .map_err(|e| ApiError::internal(format!("Failed to resolve image: {}", e), ""))?
            .ok_or_else(|| ApiError::not_found("image", img.clone(), ""))?;

        (Some(image_ref), Some(registry))
    } else {
        (None, None)
    };

    // Update configuration
    let entry = state
        .config_registry()
        .update(
            &name,
            image_ref.as_ref(),
            image_registry.as_ref(),
            request.team_id,
        )
        .await
        .map_err(|e| ApiError::internal(format!("Failed to update agent: {}", e), ""))?;

    info!("Updated agent '{}' configuration", name);

    Ok(Json(AgentConfigResponse {
        name: entry.name.clone(),
        image_ref: entry.image_ref.clone(),
        image_digest: entry.image_digest.clone(),
        team_id: entry.team_id.clone(),
        capabilities: entry.capabilities(),
        registered_at: entry.registered_at.to_rfc3339(),
        updated_at: Some(entry.updated_at.to_rfc3339()),
    }))
}

/// Generate a short random ID
fn generate_short_id() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let chars: String = (0..8)
        .map(|_| rng.sample(rand::distributions::Alphanumeric) as char)
        .collect();
    chars.to_lowercase()
}

/// Create router for agent routes
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/agents", get(list_agents).post(register_agent))
        .route(
            "/agents/:name",
            get(get_agent).delete(unregister_agent).patch(update_agent),
        )
        .route("/agents/:name/execute", post(execute_agent))
        .route("/agents/:name/metrics", get(get_agent_metrics))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_short_id() {
        let id1 = generate_short_id();
        let id2 = generate_short_id();
        assert_eq!(id1.len(), 8);
        assert_eq!(id2.len(), 8);
        assert_ne!(id1, id2); // Very unlikely to collide
    }

    #[test]
    fn test_agent_config_response_serialization() {
        let response = AgentConfigResponse {
            name: "test-agent".to_string(),
            image_ref: "test:latest".to_string(),
            image_digest: "sha256:abc123".to_string(),
            team_id: Some("team-1".to_string()),
            capabilities: vec!["chat".to_string(), "search".to_string()],
            registered_at: Utc::now().to_rfc3339(),
            updated_at: None,
        };

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("test-agent"));
        assert!(json.contains("sha256:abc123"));
    }
}
