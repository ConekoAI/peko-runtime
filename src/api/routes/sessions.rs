//! Session Management API Routes
//!
//! Implements session endpoints per API_CONTRACT.md §5:
//! - GET /agents/{id}/sessions - List sessions
//! - GET /agents/{id}/sessions/{session_id} - Get session metadata
//! - GET /agents/{id}/sessions/{session_id}/history - Get session history
//! - POST /agents/{id}/sessions/{session_id}/branch - Branch session
//! - DELETE /agents/{id}/sessions/{session_id} - Delete session

use crate::api::error::ApiError;
use crate::api::state::AppState;
use crate::api::types::{PaginatedResponse, PaginationParams};
use crate::session::events::SessionEvent;
use crate::session::sidecar::{SessionSidecarIndex, SidecarManager};
use crate::session::sync::SyncSessionStorage;
use axum::{
    extract::{Path, Query, State},
    routing::{delete, get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

/// Session response object (API_CONTRACT §2.3)
#[derive(Debug, Clone, Serialize)]
pub struct SessionResponse {
    pub id: String,
    pub instance_id: String,
    pub created_at: String,
    pub updated_at: String,
    pub turn_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

impl From<SessionSidecarIndex> for SessionResponse {
    fn from(index: SessionSidecarIndex) -> Self {
        Self {
            id: index.session_id,
            instance_id: index.instance_id,
            created_at: index.created_at.to_rfc3339(),
            updated_at: index.updated_at.to_rfc3339(),
            turn_count: index.turn_count,
            parent_session_id: index.parent_session_id,
            title: index.title,
        }
    }
}

/// History event response (API_CONTRACT §5.3)
#[derive(Debug, Clone, Serialize)]
pub struct HistoryEventResponse {
    pub id: String,
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub created_at: String,
}

impl From<&SessionEvent> for HistoryEventResponse {
    fn from(event: &SessionEvent) -> Self {
        let envelope = event.envelope();
        let mut response = Self {
            id: envelope.id.clone(),
            event_type: event.event_type().to_string(),
            role: None,
            content: None,
            tool: None,
            args: None,
            tool_call_id: None,
            output: None,
            error: None,
            created_at: envelope.ts.to_rfc3339(),
        };

        match event {
            SessionEvent::UserMessage(e) => {
                response.role = Some("user".to_string());
                response.content = Some(e.content.clone());
            }
            SessionEvent::AssistantMessage(e) => {
                response.role = Some("assistant".to_string());
                response.content = Some(e.content.clone());
            }
            SessionEvent::Thinking(e) => {
                response.content = Some(e.content.clone());
            }
            SessionEvent::ToolCall(e) => {
                response.tool = Some(e.tool.clone());
                response.args = Some(e.args.clone());
                response.tool_call_id = Some(e.tool_call_id.clone());
            }
            SessionEvent::ToolResult(e) => {
                response.tool_call_id = Some(e.tool_call_id.clone());
                response.output = e.output.clone();
                response.error = e.error.clone();
            }
            SessionEvent::HookTrigger(e) => {
                if let Some(ref payload) = e.payload {
                    response.content = Some(payload.to_string());
                }
            }
            SessionEvent::System(e) => {
                response.content = Some(e.detail.to_string());
            }
            _ => {}
        }

        response
    }
}

/// History response
#[derive(Debug, Clone, Serialize)]
pub struct HistoryResponse {
    pub session_id: String,
    pub items: Vec<HistoryEventResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    pub has_more: bool,
}

/// History query parameters
#[derive(Debug, Deserialize)]
pub struct HistoryParams {
    #[serde(default = "default_true")]
    pub include_tool_calls: bool,
    #[serde(default)]
    pub include_thinking: bool,
    #[serde(default = "default_limit_100")]
    pub limit: usize,
    pub cursor: Option<String>,
}

fn default_true() -> bool {
    true
}

fn default_limit_100() -> usize {
    100
}

/// Branch session request
#[derive(Debug, Deserialize)]
pub struct BranchRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Branch session response
#[derive(Debug, Serialize)]
pub struct BranchResponse {
    #[serde(flatten)]
    pub session: SessionResponse,
    pub parent_session_id: String,
}

/// List all sessions for an instance
async fn list_sessions(
    State(state): State<AppState>,
    Path(instance_id): Path<String>,
    Query(params): Query<PaginationParams>,
) -> Result<Json<PaginatedResponse<SessionResponse>>, ApiError> {
    debug!("Listing sessions for instance: {}", instance_id);

    // Get instance workspace path
    let workspace = get_instance_workspace(&state, &instance_id).await?;
    let sessions_dir = workspace.join("sessions");

    if !sessions_dir.exists() {
        return Ok(Json(PaginatedResponse::new(vec![], false)));
    }

    // Load sidecar indices
    let sidecar_manager = SidecarManager::new(sessions_dir);
    let indices = sidecar_manager
        .list_indices()
        .await
        .map_err(|e| ApiError::internal(format!("Failed to list sessions: {}", e), ""))?;

    // Convert to responses
    let mut responses: Vec<SessionResponse> =
        indices.into_iter().map(|(_, index)| index.into()).collect();

    // Filter to only sessions for this instance
    responses.retain(|r| r.instance_id == instance_id);

    // Apply pagination
    let total = responses.len();
    let offset = params.offset();
    let limit = params.limit();

    let items: Vec<SessionResponse> = responses.into_iter().skip(offset).take(limit).collect();

    let has_more = offset + items.len() < total;

    Ok(Json(PaginatedResponse::new(items, has_more)))
}

/// Get session by ID
async fn get_session(
    State(state): State<AppState>,
    Path((instance_id, session_id)): Path<(String, String)>,
) -> Result<Json<SessionResponse>, ApiError> {
    debug!(
        "Getting session: {} for instance: {}",
        session_id, instance_id
    );

    let workspace = get_instance_workspace(&state, &instance_id).await?;
    let sessions_dir = workspace.join("sessions");
    let sidecar_manager = SidecarManager::new(sessions_dir);

    let index = sidecar_manager
        .load(&session_id)
        .await
        .map_err(|e| ApiError::internal(format!("Failed to load session: {}", e), ""))?
        .ok_or_else(|| ApiError::not_found("session", session_id.clone(), ""))?;

    // Verify instance ownership
    if index.instance_id != instance_id {
        return Err(ApiError::not_found("session", session_id, ""));
    }

    Ok(Json(index.into()))
}

/// Get session history
async fn get_session_history(
    State(state): State<AppState>,
    Path((instance_id, session_id)): Path<(String, String)>,
    Query(params): Query<HistoryParams>,
) -> Result<Json<HistoryResponse>, ApiError> {
    debug!(
        "Getting history for session: {} (instance: {})",
        session_id, instance_id
    );

    let workspace = get_instance_workspace(&state, &instance_id).await?;
    let sessions_dir = workspace.join("sessions");
    let storage = SyncSessionStorage::new(sessions_dir);

    // Verify session exists
    if !storage.session_exists(&session_id).await {
        return Err(ApiError::not_found("session", session_id, ""));
    }

    // Load events
    let events = storage
        .load_events(&session_id)
        .await
        .map_err(|e| ApiError::internal(format!("Failed to load session history: {}", e), ""))?;

    // Convert and filter events
    let mut items: Vec<HistoryEventResponse> = events
        .iter()
        .filter_map(|event| {
            let event_type = event.event_type();

            // Filter based on include params
            if !params.include_tool_calls {
                if event_type == "tool.call" || event_type == "tool.result" {
                    return None;
                }
            }

            if !params.include_thinking && event_type == "thinking" {
                return None;
            }

            Some(event.into())
        })
        .collect();

    // Apply pagination (newest first by default)
    items.reverse();
    let total = items.len();
    let limit = params.limit.min(100);
    let offset = params
        .cursor
        .as_ref()
        .and_then(|c| c.parse::<usize>().ok())
        .unwrap_or(0);

    let items: Vec<HistoryEventResponse> = items.into_iter().skip(offset).take(limit).collect();

    let has_more = offset + items.len() < total;
    let next_cursor = if has_more {
        Some((offset + items.len()).to_string())
    } else {
        None
    };

    Ok(Json(HistoryResponse {
        session_id,
        items,
        cursor: next_cursor,
        has_more,
    }))
}

/// Branch a session
async fn branch_session(
    State(state): State<AppState>,
    Path((instance_id, session_id)): Path<(String, String)>,
    Json(request): Json<BranchRequest>,
) -> Result<Json<BranchResponse>, ApiError> {
    info!(
        "Branching session: {} for instance: {}",
        session_id, instance_id
    );

    let workspace = get_instance_workspace(&state, &instance_id).await?;
    let sessions_dir = workspace.join("sessions");
    let storage = SyncSessionStorage::new(sessions_dir.clone());

    // Verify parent session exists
    if !storage.session_exists(&session_id).await {
        return Err(ApiError::not_found("session", session_id.clone(), ""));
    }

    // Generate new session ID
    let new_session_id = format!("sess_{}", uuid::Uuid::new_v4().simple());

    // Create branched session
    storage
        .create_branched_session(&new_session_id, &instance_id, &session_id, None)
        .await
        .map_err(|e| ApiError::internal(format!("Failed to branch session: {}", e), ""))?;

    // Set label if provided
    if let Some(label) = request.label {
        if let Err(e) = storage.set_title(&new_session_id, label).await {
            warn!("Failed to set label for branched session: {}", e);
        }
    }

    // Load and return new session
    let sidecar_manager = SidecarManager::new(sessions_dir);
    let index = sidecar_manager
        .load(&new_session_id)
        .await
        .map_err(|e| ApiError::internal(format!("Failed to load branched session: {}", e), ""))?
        .ok_or_else(|| ApiError::internal("Branched session not found after creation", ""))?;

    Ok(Json(BranchResponse {
        session: index.into(),
        parent_session_id: session_id,
    }))
}

/// Delete a session
async fn delete_session(
    State(state): State<AppState>,
    Path((instance_id, session_id)): Path<(String, String)>,
) -> Result<axum::http::StatusCode, ApiError> {
    info!(
        "Deleting session: {} for instance: {}",
        session_id, instance_id
    );

    let workspace = get_instance_workspace(&state, &instance_id).await?;
    let sessions_dir = workspace.join("sessions");
    let storage = SyncSessionStorage::new(sessions_dir);

    // Verify session exists
    if !storage.session_exists(&session_id).await {
        return Err(ApiError::not_found("session", session_id, ""));
    }

    // Check if this is the active session of a running instance
    // (Would need to check instance state - simplified here)

    // Delete session
    storage
        .delete_session(&session_id)
        .await
        .map_err(|e| ApiError::internal(format!("Failed to delete session: {}", e), ""))?;

    Ok(axum::http::StatusCode::NO_CONTENT)
}

/// Get instance workspace path
async fn get_instance_workspace(
    state: &AppState,
    instance_id: &str,
) -> Result<std::path::PathBuf, ApiError> {
    // In a real implementation, this would look up the instance in the state
    // and return its workspace path. For now, we construct a path.
    let workspace = state.workspace_path.join("agents").join(instance_id);

    if !workspace.exists() {
        return Err(ApiError::not_found("instance", instance_id.to_string(), ""));
    }

    Ok(workspace)
}

/// Create router for session routes
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/agents/:id/sessions", get(list_sessions))
        .route(
            "/agents/:id/sessions/:session_id",
            get(get_session).delete(delete_session),
        )
        .route(
            "/agents/:id/sessions/:session_id/history",
            get(get_session_history),
        )
        .route(
            "/agents/:id/sessions/:session_id/branch",
            post(branch_session),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::events::{
        AssistantMessageEvent, EventEnvelope, TokenUsage, UserMessageEvent,
    };
    use chrono::Utc;

    #[test]
    fn test_session_response_from_index() {
        let index = SessionSidecarIndex::new(
            "sess_123",
            "inst_456",
            crate::session::events::SessionTrigger::User,
        );
        let response: SessionResponse = index.into();

        assert_eq!(response.id, "sess_123");
        assert_eq!(response.instance_id, "inst_456");
        assert_eq!(response.turn_count, 0);
    }

    #[test]
    fn test_history_response_from_event() {
        let event = SessionEvent::UserMessage(UserMessageEvent {
            envelope: EventEnvelope {
                id: "evt_001".to_string(),
                session_id: "sess_123".to_string(),
                ts: Utc::now(),
                seq: 1,
            },
            message_id: "msg_001".to_string(),
            content: "Hello".to_string(),
            source: crate::session::events::MessageSource::User,
        });

        let response: HistoryEventResponse = (&event).into();
        assert_eq!(response.id, "evt_001");
        assert_eq!(response.event_type, "user.message");
        assert_eq!(response.role, Some("user".to_string()));
        assert_eq!(response.content, Some("Hello".to_string()));
    }

    #[test]
    fn test_history_response_from_tool_call() {
        use crate::session::events::ToolCallEvent;
        use serde_json::json;

        let event = SessionEvent::ToolCall(ToolCallEvent {
            envelope: EventEnvelope {
                id: "evt_002".to_string(),
                session_id: "sess_123".to_string(),
                ts: Utc::now(),
                seq: 2,
            },
            tool_call_id: "tc_001".to_string(),
            tool: "web_search".to_string(),
            args: json!({"query": "test"}),
            async_: false,
            timeout_seconds: Some(30),
        });

        let response: HistoryEventResponse = (&event).into();
        assert_eq!(response.event_type, "tool.call");
        assert_eq!(response.tool, Some("web_search".to_string()));
        assert_eq!(response.tool_call_id, Some("tc_001".to_string()));
    }

    #[test]
    fn test_history_response_from_assistant_message() {
        use crate::session::events::{AssistantMessageEvent, TokenUsage};

        let event = SessionEvent::AssistantMessage(AssistantMessageEvent {
            envelope: EventEnvelope {
                id: "evt_003".to_string(),
                session_id: "sess_123".to_string(),
                ts: Utc::now(),
                seq: 3,
            },
            message_id: "msg_002".to_string(),
            content: "The answer is 42.".to_string(),
            usage: TokenUsage {
                input_tokens: 100,
                output_tokens: 50,
                total_tokens: 150,
            },
        });

        let response: HistoryEventResponse = (&event).into();
        assert_eq!(response.event_type, "assistant.message");
        assert_eq!(response.role, Some("assistant".to_string()));
        assert_eq!(response.content, Some("The answer is 42.".to_string()));
    }

    #[test]
    fn test_history_response_from_thinking() {
        use crate::session::events::ThinkingEvent;

        let event = SessionEvent::Thinking(ThinkingEvent {
            envelope: EventEnvelope {
                id: "evt_004".to_string(),
                session_id: "sess_123".to_string(),
                ts: Utc::now(),
                seq: 4,
            },
            content: "Let me think about this...".to_string(),
        });

        let response: HistoryEventResponse = (&event).into();
        assert_eq!(response.event_type, "thinking");
        assert_eq!(response.content, Some("Let me think about this...".to_string()));
    }

    #[test]
    fn test_session_response_title_optional() {
        use crate::session::sidecar::SessionSidecarIndex;
        use crate::session::events::SessionTrigger;
        use chrono::Utc;

        let index = SessionSidecarIndex {
            session_id: "sess_123".to_string(),
            instance_id: "inst_456".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            turn_count: 0,
            event_count: 1,
            total_tokens: 0,
            parent_session_id: None,
            trigger: SessionTrigger::User,
            ended: false,
            title: None,
        };

        let response: SessionResponse = index.into();
        assert_eq!(response.id, "sess_123");
        assert_eq!(response.title, None);
        assert_eq!(response.turn_count, 0);
    }

    #[test]
    fn test_session_response_with_parent() {
        use crate::session::sidecar::SessionSidecarIndex;
        use crate::session::events::SessionTrigger;
        use chrono::Utc;

        let index = SessionSidecarIndex {
            session_id: "sess_child".to_string(),
            instance_id: "inst_456".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            turn_count: 5,
            event_count: 10,
            total_tokens: 1000,
            parent_session_id: Some("sess_parent".to_string()),
            trigger: SessionTrigger::Branch,
            ended: false,
            title: Some("Branched Session".to_string()),
        };

        let response: SessionResponse = index.into();
        assert_eq!(response.id, "sess_child");
        assert_eq!(response.parent_session_id, Some("sess_parent".to_string()));
        assert_eq!(response.turn_count, 5);
        assert_eq!(response.title, Some("Branched Session".to_string()));
    }
}
