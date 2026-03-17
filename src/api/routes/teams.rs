//! Team API Routes
//!
//! Implements API_CONTRACT.md §6:
//! - GET /teams - List teams
//! - POST /teams - Deploy team
//! - GET /teams/{id} - Get team details
//! - DELETE /teams/{id} - Stop and remove team
//! - POST /teams/{id}/scale - Scale agent instances

use crate::api::error::ApiError;
use crate::api::state::AppState;
use crate::api::types::{PaginatedResponse, PaginationParams};
use crate::team::config::TeamConfig;
use crate::team::Team;
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

/// Deploy team request
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum DeployTeamRequest {
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
pub struct ScaleTeamRequest {
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

impl From<crate::team::ScaleResult> for ScaleTeamResponse {
    fn from(result: crate::team::ScaleResult) -> Self {
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

impl From<Team> for TeamResponse {
    fn from(team: Team) -> Self {
        Self {
            id: team.id.clone(),
            name: team.name.clone(),
            status: team.status.to_string(),
            config_path: Some(format!(".pekobot/teams/{}/config.toml", team.name)),
            created_at: team.created_at.to_rfc3339(),
            agent_count: team.agent_instances.len(),
            instance_ids: team.all_instance_ids(),
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

/// GET /teams - List all teams
async fn list_teams(
    State(state): State<AppState>,
    Query(params): Query<PaginationParams>,
) -> Result<Json<PaginatedResponse<TeamResponse>>, ApiError> {
    let team_manager = &state.team_manager;
    let teams = team_manager.list_teams().await;

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
    Json(request): Json<DeployTeamRequest>,
) -> Result<Json<TeamResponse>, ApiError> {
    let config = match request {
        DeployTeamRequest::FilePath { config_path } => {
            // Load from file
            TeamConfig::from_file(&config_path).map_err(|e| {
                ApiError::invalid_request(format!("Failed to load team config: {}", e))
            })?
        }
        DeployTeamRequest::Inline { name, config } => {
            // Convert inline config to TeamConfig
            let agents = config
                .agents
                .into_iter()
                .map(|a| crate::team::config::AgentDefinition {
                    name: a.name,
                    image: a.image,
                    instances: a.instances,
                    role: a.role.and_then(|r| match r.as_str() {
                        "coordinator" => Some(crate::team::config::AgentRole::Coordinator),
                        "worker" => Some(crate::team::config::AgentRole::Worker),
                        _ => None,
                    }),
                    env: a.env,
                })
                .collect();

            TeamConfig {
                identity: crate::team::config::TeamIdentity {
                    name,
                    description: None,
                },
                agents,
                shared: None, // TODO: Parse shared from JSON
            }
        }
    };

    // Deploy the team (use the Arc from team_manager - it holds the same Arc)
    let team_manager = state.team_manager.clone();
    let state_arc = std::sync::Arc::new(state);
    let team = team_manager
        .deploy(config, state_arc)
        .await
        .map_err(|e| ApiError::internal_error(format!("Failed to deploy team: {}", e)))?;

    Ok(Json(TeamResponse::from(team)))
}

/// GET /teams/{id} - Get team details
async fn get_team(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<TeamResponse>, ApiError> {
    let team = state
        .team_manager
        .get_team(&id)
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
        .team_manager
        .remove_team(&id)
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
    Json(request): Json<ScaleTeamRequest>,
) -> Result<Json<ScaleTeamResponse>, ApiError> {
    // Clone the team_manager reference before moving state into Arc
    let team_manager = state.team_manager.clone();
    let state_arc = std::sync::Arc::new(state);

    let result = team_manager
        .scale_agent(&id, &request.agent_name, request.instances, state_arc)
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
        let req: DeployTeamRequest = serde_json::from_str(json).unwrap();
        match req {
            DeployTeamRequest::FilePath { config_path } => {
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
        let req: DeployTeamRequest = serde_json::from_str(json).unwrap();
        match req {
            DeployTeamRequest::Inline { name, config } => {
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
