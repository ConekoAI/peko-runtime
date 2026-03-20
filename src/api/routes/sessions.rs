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
use crate::common::paths::PathResolver;
use crate::session::events::SessionEvent;
use crate::session::index::{SessionEntry, SessionIndex};
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
    pub message_count: usize,
    pub total_tokens: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

impl From<SessionEntry> for SessionResponse {
    fn from(entry: SessionEntry) -> Self {
        // Convert timestamps from u64 milliseconds to RFC3339 string
        let created_at = chrono::DateTime::from_timestamp_millis(entry.created_at as i64)
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_default();
        let updated_at = chrono::DateTime::from_timestamp_millis(entry.updated_at as i64)
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_default();

        Self {
            id: entry.session_id,
            instance_id: entry.agent_name,
            created_at,
            updated_at,
            turn_count: entry.turn_count,
            message_count: entry.message_count,
            total_tokens: entry.total_tokens,
            parent_session_id: entry.parent_session_id,
            title: entry.title,
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

/// List all sessions for an agent
async fn list_sessions(
    State(state): State<AppState>,
    Path(agent_name): Path<String>,
    Query(params): Query<PaginationParams>,
) -> Result<Json<PaginatedResponse<SessionResponse>>, ApiError> {
    debug!("Listing sessions for agent: {}", agent_name);

    // Get team-aware sessions directory
    let sessions_dir = get_agent_sessions_dir(&state, &agent_name).await?;

    if !sessions_dir.exists() {
        return Ok(Json(PaginatedResponse::new(vec![], false)));
    }

    // Load session index
    let mut index = SessionIndex::open(sessions_dir);
    let entries = index
        .list_all()
        .await
        .map_err(|e| ApiError::internal(format!("Failed to list sessions: {}", e), ""))?;

    // Convert to responses and filter to only sessions for this agent
    let mut responses: Vec<SessionResponse> = entries
        .into_iter()
        .filter(|e| e.agent_name == agent_name)
        .map(|e| e.into())
        .collect();

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
    Path((agent_name, session_id)): Path<(String, String)>,
) -> Result<Json<SessionResponse>, ApiError> {
    debug!("Getting session: {} for agent: {}", session_id, agent_name);

    let sessions_dir = get_agent_sessions_dir(&state, &agent_name).await?;
    let mut index = SessionIndex::open(sessions_dir);

    let entry = index
        .get(&session_id)
        .await
        .map_err(|e| ApiError::internal(format!("Failed to load session: {}", e), ""))?
        .ok_or_else(|| ApiError::not_found("session", session_id.clone(), ""))?;

    // Verify agent ownership
    if entry.agent_name != agent_name {
        return Err(ApiError::not_found("session", session_id, ""));
    }

    Ok(Json(entry.into()))
}

/// Get session history
async fn get_session_history(
    State(state): State<AppState>,
    Path((agent_name, session_id)): Path<(String, String)>,
    Query(params): Query<HistoryParams>,
) -> Result<Json<HistoryResponse>, ApiError> {
    debug!(
        "Getting history for session: {} (agent: {})",
        session_id, agent_name
    );

    let sessions_dir = get_agent_sessions_dir(&state, &agent_name).await?;
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
    Path((agent_name, session_id)): Path<(String, String)>,
    Json(request): Json<BranchRequest>,
) -> Result<Json<BranchResponse>, ApiError> {
    info!(
        "Branching session: {} for agent: {}",
        session_id, agent_name
    );

    let sessions_dir = get_agent_sessions_dir(&state, &agent_name).await?;
    let storage = SyncSessionStorage::new(sessions_dir.clone());

    // Verify parent session exists
    if !storage.session_exists(&session_id).await {
        return Err(ApiError::not_found("session", session_id.clone(), ""));
    }

    // Generate new session ID
    let new_session_id = format!("sess_{}", uuid::Uuid::new_v4().simple());

    // Create branched session
    storage
        .create_branched_session(&new_session_id, &agent_name, &session_id, None)
        .await
        .map_err(|e| ApiError::internal(format!("Failed to branch session: {}", e), ""))?;

    // Note: Session title/label is no longer stored (sidecar removed)
    // The label from the request is ignored for now

    // Load and return new session
    let mut index = SessionIndex::open(sessions_dir);
    let entry = index
        .get(&new_session_id)
        .await
        .map_err(|e| ApiError::internal(format!("Failed to load branched session: {}", e), ""))?
        .ok_or_else(|| ApiError::internal("Branched session not found after creation", ""))?;

    Ok(Json(BranchResponse {
        session: entry.into(),
        parent_session_id: session_id,
    }))
}

/// Delete a session
async fn delete_session(
    State(state): State<AppState>,
    Path((agent_name, session_id)): Path<(String, String)>,
) -> Result<axum::http::StatusCode, ApiError> {
    info!("Deleting session: {} for agent: {}", session_id, agent_name);

    let sessions_dir = get_agent_sessions_dir(&state, &agent_name).await?;
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

/// Get agent sessions directory using team-aware path resolution
///
/// Looks up the agent in the config registry to get its team assignment,
/// then returns the appropriate sessions directory path.
async fn get_agent_sessions_dir(
    state: &AppState,
    agent_name: &str,
) -> Result<std::path::PathBuf, ApiError> {
    // Look up agent in config registry to get team
    let config_registry = state.config_registry();
    let entry = config_registry
        .get(agent_name)
        .await
        .ok_or_else(|| ApiError::not_found("agent", agent_name.to_string(), ""))?;

    // Use PathResolver for consistent path resolution
    let resolver = PathResolver::with_dirs(
        state.config_dir.clone(),
        state.data_dir.clone(),
        state.cache_dir.clone(),
    );

    // Get team-aware sessions directory
    let sessions_dir = resolver.agent_sessions_dir(agent_name, entry.team_id.as_deref());

    Ok(sessions_dir)
}

/// Get agent workspace directory using team-aware path resolution
async fn get_agent_workspace_dir(
    state: &AppState,
    agent_name: &str,
) -> Result<std::path::PathBuf, ApiError> {
    // Look up agent in config registry to get team
    let config_registry = state.config_registry();
    let entry = config_registry
        .get(agent_name)
        .await
        .ok_or_else(|| ApiError::not_found("agent", agent_name.to_string(), ""))?;

    // Use PathResolver for consistent path resolution
    let resolver = PathResolver::with_dirs(
        state.config_dir.clone(),
        state.data_dir.clone(),
        state.cache_dir.clone(),
    );

    // Get team-aware workspace directory
    let workspace = resolver.agent_workspace(agent_name, entry.team_id.as_deref());

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
    fn test_session_response_from_entry() {
        let entry = SessionEntry::new(
            "sess_123".to_string(),
            "inst_456".to_string(),
            "sess_123.jsonl".to_string(),
        );
        let response: SessionResponse = entry.into();

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
        assert_eq!(
            response.content,
            Some("Let me think about this...".to_string())
        );
    }

    #[test]
    fn test_session_response_title_optional() {
        use crate::session::index::SessionEntry;

        let entry = SessionEntry {
            session_id: "sess_123".to_string(),
            agent_name: "inst_456".to_string(),
            created_at: 1234567890000,
            updated_at: 1234567890000,
            turn_count: 0,
            message_count: 1,
            input_tokens: 0,
            output_tokens: 0,
            total_tokens: 0,
            transcript_file: "sess_123.jsonl".to_string(),
            title: None,
            parent_session_id: None,
            ended: false,
            trigger: "user".to_string(),
            provider: None,
            model: None,
            channel: None,
            recipient: None,
            cwd: None,
        };

        let response: SessionResponse = entry.into();
        assert_eq!(response.id, "sess_123");
        assert_eq!(response.title, None);
        assert_eq!(response.turn_count, 0);
    }

    #[test]
    fn test_session_response_with_parent() {
        use crate::session::index::SessionEntry;

        let entry = SessionEntry {
            session_id: "sess_child".to_string(),
            agent_name: "inst_456".to_string(),
            created_at: 1234567890000,
            updated_at: 1234567890000,
            turn_count: 5,
            message_count: 10,
            input_tokens: 500,
            output_tokens: 500,
            total_tokens: 1000,
            transcript_file: "sess_child.jsonl".to_string(),
            title: Some("Branched Session".to_string()),
            parent_session_id: Some("sess_parent".to_string()),
            ended: false,
            trigger: "branch".to_string(),
            provider: None,
            model: None,
            channel: None,
            recipient: None,
            cwd: None,
        };

        let response: SessionResponse = entry.into();
        assert_eq!(response.id, "sess_child");
        assert_eq!(response.parent_session_id, Some("sess_parent".to_string()));
        assert_eq!(response.turn_count, 5);
        assert_eq!(response.title, Some("Branched Session".to_string()));
    }
}
