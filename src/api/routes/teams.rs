//! Team API Routes
//!
//! Implements API_CONTRACT.md §6:
//! - GET /teams - List teams
//! - POST /teams - Deploy team
//! - GET /teams/{id} - Get team details
//! - DELETE /teams/{id} - Stop and remove team
//! - POST /teams/{id}/scale - Scale agent instances
//!
//! NOTE: This module now delegates to TeamManagementService for unified handling.
//! All business logic has been moved to the service layer.

use crate::api::error::ApiError;
use crate::api::state::AppState;
use crate::api::types::{PaginatedResponse, PaginationParams};
use crate::common::types::team::{
    TeamAgentDefinition, TeamConfigSource, TeamDeployRequest, TeamRuntimeInfo, TeamScaleRequest,
};
use axum::{
    extract::{Path, Query, State},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};

/// Team response object (API_CONTRACT §2.5)
#[derive(Debug, Clone, Serialize)]
pub struct TeamResponse {
    pub id: String,
    pub name: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_path: Option<String>,
    pub created_at: String,
    pub agent_count: usize,
    pub instance_ids: Vec<String>,
}

impl From<TeamRuntimeInfo> for TeamResponse {
    fn from(info: TeamRuntimeInfo) -> Self {
        Self {
            id: info.id,
            name: info.name,
            status: info.status.to_string(),
            config_path: None, // Runtime teams don't have config paths
            created_at: info.created_at,
            agent_count: info.agent_count,
            instance_ids: info.instance_ids,
        }
    }
}

/// Deploy team request (API format)
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum DeployTeamRequestApi {
    /// Deploy from inline configuration
    Inline {
        /// Team name
        name: String,
        /// Inline configuration
        config: InlineTeamConfig,
    },
    /// Deploy from file path
    FilePath {
        /// Path to team.toml file
        config_path: String,
    },
}

/// Inline team configuration (subset of team.toml)
#[derive(Debug, Deserialize)]
pub struct InlineTeamConfig {
    /// Agent definitions
    pub agents: Vec<InlineAgentDefinition>,
    /// Shared services (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shared: Option<serde_json::Value>,
}

/// Inline agent definition
#[derive(Debug, Deserialize)]
pub struct InlineAgentDefinition {
    pub name: String,
    pub image: String,
    #[serde(default = "default_one")]
    pub instances: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<std::collections::HashMap<String, String>>,
}

fn default_one() -> u32 {
    1
}

/// Scale team request
#[derive(Debug, Deserialize)]
pub struct ScaleTeamRequestApi {
    /// Agent name to scale
    pub agent_name: String,
    /// Desired number of instances
    pub instances: u32,
}

/// Scale team response
#[derive(Debug, Serialize)]
pub struct ScaleTeamResponse {
    pub team_id: String,
    pub agent_name: String,
    pub previous_count: u32,
    pub new_count: u32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub added_instance_ids: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub removed_instance_ids: Vec<String>,
}

impl From<crate::common::types::team::TeamScaleResult> for ScaleTeamResponse {
    fn from(result: crate::common::types::team::TeamScaleResult) -> Self {
        Self {
            team_id: result.team_id,
            agent_name: result.agent_name,
            previous_count: result.previous_count,
            new_count: result.new_count,
            added_instance_ids: result.added_instance_ids,
            removed_instance_ids: result.removed_instance_ids,
        }
    }
}

/// Create the team routes router
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/teams", get(list_teams).post(deploy_team))
        .route("/teams/{id}", get(get_team).delete(stop_team))
        .route("/teams/{id}/scale", post(scale_team))
}

/// GET /teams - List all runtime teams
async fn list_teams(
    State(state): State<AppState>,
    Query(params): Query<PaginationParams>,
) -> Result<Json<PaginatedResponse<TeamResponse>>, ApiError> {
    // Delegate to unified service
    let teams = state.team_service().list_runtime_teams().await;

    // Apply pagination
    let limit = params.limit();
    let offset = params.offset();

    let total = teams.len();
    let items: Vec<TeamResponse> = teams
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(TeamResponse::from)
        .collect();

    let has_more = offset + items.len() < total;
    let next_cursor = if has_more {
        Some(base64::encode((offset + items.len()).to_string()))
    } else {
        None
    };

    Ok(Json(PaginatedResponse {
        items,
        cursor: next_cursor,
        has_more,
    }))
}

/// POST /teams - Deploy a new team
async fn deploy_team(
    State(state): State<AppState>,
    Json(request): Json<DeployTeamRequestApi>,
) -> Result<Json<TeamResponse>, ApiError> {
    // Convert API request to service request
    let service_request = match request {
        DeployTeamRequestApi::FilePath { config_path } => TeamDeployRequest {
            name: "deployed".to_string(), // Will be read from config
            config_source: TeamConfigSource::FilePath(config_path.into()),
        },
        DeployTeamRequestApi::Inline { name, config } => {
            let agents = config
                .agents
                .into_iter()
                .map(|a| TeamAgentDefinition {
                    name: a.name,
                    image: a.image,
                    instances: a.instances,
                    role: a.role,
                })
                .collect();

            TeamDeployRequest {
                name,
                config_source: TeamConfigSource::Inline { agents },
            }
        }
    };

    // Delegate to unified service - clone state for Arc
    let state_arc = std::sync::Arc::new(state.clone());
    let result = state
        .team_service()
        .deploy_runtime(service_request, state_arc)
        .await
        .map_err(|e| ApiError::internal_error(format!("Failed to deploy team: {}", e)))?;

    Ok(Json(TeamResponse {
        id: result.id,
        name: result.name,
        status: result.status,
        config_path: None,
        created_at: chrono::Utc::now().to_rfc3339(),
        agent_count: result.agent_count,
        instance_ids: result.instance_ids,
    }))
}

/// GET /teams/{id} - Get team details
async fn get_team(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<TeamResponse>, ApiError> {
    let team = state
        .team_service()
        .get_runtime_team(&id)
        .await
        .ok_or_else(|| ApiError::not_found_simple("Team", &id))?;

    Ok(Json(TeamResponse::from(team)))
}

/// DELETE /teams/{id} - Stop and remove a team
async fn stop_team(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state
        .team_service()
        .stop_runtime(&id)
        .await
        .map_err(|e| ApiError::internal_error(format!("Failed to stop team: {}", e)))?;

    Ok(Json(serde_json::json!({
        "success": true,
        "team_id": id,
        "message": "Team stopped and removed"
    })))
}

/// POST /teams/{id}/scale - Scale agent instances
async fn scale_team(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<ScaleTeamRequestApi>,
) -> Result<Json<ScaleTeamResponse>, ApiError> {
    let service_request = TeamScaleRequest {
        team_id: id,
        agent_name: request.agent_name,
        instances: request.instances,
    };

    // Clone state for Arc
    let state_arc = std::sync::Arc::new(state.clone());
    let result = state
        .team_service()
        .scale_runtime(service_request, state_arc)
        .await
        .map_err(|e| ApiError::invalid_request(format!("Failed to scale team: {}", e)))?;

    Ok(Json(ScaleTeamResponse::from(result)))
}

// Base64 encoding helper (simple implementation for pagination cursor)
mod base64 {
    pub fn encode(input: String) -> String {
        use std::io::Write;
        let mut encoder =
            ::base64::write::EncoderStringWriter::new(&::base64::engine::general_purpose::STANDARD);
        encoder.write_all(input.as_bytes()).unwrap();
        encoder.into_inner()
    }

    pub fn decode(input: &str) -> Option<Vec<u8>> {
        use ::base64::Engine;
        ::base64::engine::general_purpose::STANDARD
            .decode(input)
            .ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deploy_request_deserialization() {
        // Test file path variant
        let json = r#"{"config_path": "/path/to/team.toml"}"#;
        let req: DeployTeamRequestApi = serde_json::from_str(json).unwrap();
        match req {
            DeployTeamRequestApi::FilePath { config_path } => {
                assert_eq!(config_path, "/path/to/team.toml");
            }
            _ => panic!("Expected FilePath variant"),
        }

        // Test inline variant
        let json = r#"{
            "name": "research-team",
            "config": {
                "agents": [
                    {
                        "name": "coordinator",
                        "image": "./agents/coordinator",
                        "instances": 1,
                        "role": "coordinator"
                    }
                ]
            }
        }"#;
        let req: DeployTeamRequestApi = serde_json::from_str(json).unwrap();
        match req {
            DeployTeamRequestApi::Inline { name, config } => {
                assert_eq!(name, "research-team");
                assert_eq!(config.agents.len(), 1);
            }
            _ => panic!("Expected Inline variant"),
        }
    }

    #[test]
    fn test_team_response_serialization() {
        let response = TeamResponse {
            id: "team_123".to_string(),
            name: "test-team".to_string(),
            status: "running".to_string(),
            config_path: Some(".pekobot/teams/test-team/config.toml".to_string()),
            created_at: "2026-03-17T10:00:00.000Z".to_string(),
            agent_count: 2,
            instance_ids: vec!["inst_1".to_string(), "inst_2".to_string()],
        };

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"id\":\"team_123\""));
        assert!(json.contains("\"status\":\"running\""));
    }
}
