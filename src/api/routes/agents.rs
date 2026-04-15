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
//!
//! NOTE: This module now delegates to AgentService for unified handling.
//! All business logic has been moved to the service layer.

use crate::api::error::ApiError;
use crate::api::state::AppState;
use crate::api::types::{PaginatedResponse, PaginationParams};
use crate::common::services::ConfigAuthority;
use crate::common::types::agent::{AgentCreateRequest, AgentDeleteOptions, AgentUpdateRequest};
use crate::observability::performance::PerformanceGuard;
use axum::{
    extract::{Path, Query, State},
    routing::{get, post},
    Json, Router,
};
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

impl From<crate::common::services::AgentConfigEntry> for AgentConfigResponse {
    fn from(entry: crate::common::services::AgentConfigEntry) -> Self {
        let capabilities = entry
            .config
            .capabilities
            .iter()
            .map(|c| c.name.clone())
            .collect();

        // Get image_ref from config if available, otherwise use a placeholder
        let image_ref = entry.config.provider.default_model.clone();

        Self {
            name: entry.name,
            image_ref,
            image_digest: "sha256:unknown".to_string(), // Not directly available in new structure
            team_id: Some(entry.team),
            capabilities,
            registered_at: entry
                .registered_at
                .map(|d| d.to_rfc3339())
                .unwrap_or_else(|| chrono::Utc::now().to_rfc3339()),
            updated_at: entry.updated_at.map(|d| d.to_rfc3339()),
        }
    }
}

impl From<crate::common::types::agent::AgentSummary> for AgentConfigResponse {
    fn from(summary: crate::common::types::agent::AgentSummary) -> Self {
        let capabilities = summary
            .config
            .capabilities
            .iter()
            .map(|c| c.name.clone())
            .collect();

        Self {
            name: summary.name,
            image_ref: summary.config.provider.default_model.clone(),
            image_digest: "sha256:unknown".to_string(),
            team_id: Some(summary.team),
            capabilities,
            registered_at: chrono::Utc::now().to_rfc3339(),
            updated_at: None,
        }
    }
}

impl From<crate::common::types::agent::AgentInfo> for AgentConfigResponse {
    fn from(info: crate::common::types::agent::AgentInfo) -> Self {
        let capabilities = info
            .config
            .capabilities
            .iter()
            .map(|c| c.name.clone())
            .collect();

        Self {
            name: info.name,
            image_ref: info.config.provider.default_model.clone(),
            image_digest: "sha256:unknown".to_string(),
            team_id: Some(info.team),
            capabilities,
            registered_at: chrono::Utc::now().to_rfc3339(),
            updated_at: None,
        }
    }
}

/// Register agent request
#[derive(Debug, Deserialize)]
pub struct RegisterAgentRequest {
    /// Image reference, digest, or path (optional if provider is specified)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// Provider to use (e.g., "kimi", "openai") - alternative to image
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Model name (optional, used with provider)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Agent name (optional, derived from image or generated)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Team ID to assign
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
    /// Environment variables
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,
    /// Auto-create team if it doesn't exist (default: true)
    #[serde(default = "default_true")]
    pub auto_create_team: bool,
}

fn default_true() -> bool {
    true
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
    // Delegate to unified service
    let agents = state
        .agent_mgmt_service()
        .list_agents(None)
        .await
        .map_err(|e| ApiError::internal(format!("Failed to list agents: {}", e), ""))?;

    let items: Vec<AgentConfigResponse> = agents
        .into_iter()
        .map(AgentConfigResponse::from)
        .skip(params.offset())
        .take(params.limit())
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

    // Determine agent name
    let name = request
        .name
        .unwrap_or_else(|| format!("agent-{}", generate_short_id()));

    // Determine provider (from image or explicit provider)
    let provider = request.provider.unwrap_or_else(|| "openai".to_string());

    // Build service request
    let service_request = AgentCreateRequest::new(&name, &provider)
        .with_team(request.team_id.as_deref().unwrap_or("default"))
        .with_model_opt(request.model)
        .with_auto_create_team(request.auto_create_team);

    // Delegate to unified service
    let result = state
        .agent_mgmt_service()
        .create_agent(service_request)
        .await
        .map_err(|e| ApiError::internal(format!("Failed to create agent: {}", e), ""))?;

    // Get the registered entry for response
    let entry = state
        .config_service()
        .get(&result.name, None)
        .await
        .map_err(|e| ApiError::internal(format!("Failed to get agent: {}", e), ""))?
        .ok_or_else(|| {
            ApiError::internal(
                "Agent creation succeeded but entry not found".to_string(),
                "",
            )
        })?;

    info!(
        "Registered agent '{}' in team '{}' (provider: {})",
        result.name, result.team, result.provider
    );

    Ok(Json(AgentConfigResponse::from(entry)))
}

/// Get agent configuration by name
async fn get_agent(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<AgentConfigResponse>, ApiError> {
    let agent = state
        .agent_mgmt_service()
        .get_agent(&name, None)
        .await
        .map_err(|e| ApiError::internal(format!("Failed to get agent: {}", e), ""))?
        .ok_or_else(|| ApiError::not_found("agent", name.clone(), ""))?;

    Ok(Json(AgentConfigResponse::from(agent)))
}

/// Unregister agent
async fn unregister_agent(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<axum::http::StatusCode, ApiError> {
    // Check if agent is currently executing
    if state.lifecycle().is_executing(&name).await {
        return Err(ApiError::conflict(
            format!("Agent '{}' is currently executing", name),
            "Wait for execution to complete or cancel it",
        ));
    }

    // Delegate to unified service
    let _result = state
        .agent_mgmt_service()
        .delete_agent(&name, None, AgentDeleteOptions::default())
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

    // Check if agent exists
    let exists = state
        .config_service()
        .exists(&name, None)
        .await
        .map_err(|e| ApiError::internal(format!("Failed to check agent existence: {}", e), ""))?;
    if !exists {
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
        user: "default".to_string(),
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
    let exists = state
        .config_service()
        .exists(&name, None)
        .await
        .map_err(|e| ApiError::internal(format!("Failed to check agent existence: {}", e), ""))?;
    if !exists {
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
    // Check if agent is currently executing
    if state.lifecycle().is_executing(&name).await {
        return Err(ApiError::conflict(
            format!("Agent '{}' is currently executing", name),
            "Wait for execution to complete",
        ));
    }

    // Build update request
    let update = AgentUpdateRequest {
        image: request.image,
        team_id: request.team_id,
    };

    // Delegate to unified service
    let agent = state
        .agent_mgmt_service()
        .update_agent(&name, None, update)
        .await
        .map_err(|e| ApiError::internal(format!("Failed to update agent: {}", e), ""))?;

    info!("Updated agent '{}' configuration", name);

    Ok(Json(AgentConfigResponse::from(agent)))
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
    use chrono::Utc;

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

// Helper trait for optional values
trait AgentCreateRequestExt {
    fn with_model_opt(self, model: Option<String>) -> Self;
}

impl AgentCreateRequestExt for AgentCreateRequest {
    fn with_model_opt(mut self, model: Option<String>) -> Self {
        self.model = model;
        self
    }
}
