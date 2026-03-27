//! Session Management API Routes
//!
//! Implements session endpoints per API_CONTRACT.md §5:
//! - GET /agents/{id}/sessions - List sessions
//! - GET /agents/{id}/sessions/{session_id} - Get session metadata
//! - GET /agents/{id}/sessions/{session_id}/history - Get session history
//! - POST /agents/{id}/sessions/{session_id}/branch - Branch session
//! - DELETE /agents/{id}/sessions/{session_id} - Delete session
//!
//! NOTE: This module now delegates to SessionService for unified handling

use crate::api::error::ApiError;
use crate::api::state::AppState;
use crate::api::types::{PaginatedResponse, PaginationParams};
use crate::common::services::{HistoryEvent, HistoryQuery, SessionInfo};
use crate::session::events::SessionEvent;

use axum::{
    extract::{Path, Query, State},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

/// Session response object (API_CONTRACT §2.3)
#[derive(Debug, Clone, Serialize)]
pub struct SessionResponse {
    pub id: String,
    pub instance_id: String,
    pub created_at: String,
    pub updated_at: String,
    pub turn_count: u32,
    pub message_count: usize,
    /// Current context window size (total_tokens from last assistant message)
    pub context_window: usize,
    /// Cumulative input tokens across all assistant messages
    pub total_input_tokens: usize,
    /// Cumulative output tokens across all assistant messages
    pub total_output_tokens: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

impl From<SessionInfo> for SessionResponse {
    fn from(info: SessionInfo) -> Self {
        Self {
            id: info.id,
            instance_id: info.agent_name,
            created_at: format_timestamp(info.created_at),
            updated_at: format_timestamp(info.updated_at),
            turn_count: info.turn_count,
            message_count: info.message_count,
            context_window: info.context_window,
            total_input_tokens: info.total_input_tokens,
            total_output_tokens: info.total_output_tokens,
            parent_session_id: info.parent_session_id,
            title: info.title,
        }
    }
}

/// Format a millisecond timestamp to RFC3339 string
fn format_timestamp(ms: u64) -> String {
    chrono::DateTime::from_timestamp_millis(ms as i64)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_default()
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
            SessionEvent::MessageV2(msg) => {
                response.role = Some(match msg.role() {
                    crate::types::message::MessageRole::User => "user",
                    crate::types::message::MessageRole::Assistant => "assistant",
                    crate::types::message::MessageRole::System => "system",
                    crate::types::message::MessageRole::Tool => "tool",
                }.to_string());
                response.content = Some(msg.text_content());
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

impl From<HistoryEvent> for HistoryEventResponse {
    fn from(event: HistoryEvent) -> Self {
        match event {
            HistoryEvent::Session { timestamp } => Self {
                id: String::new(),
                event_type: "session".to_string(),
                role: None,
                content: None,
                tool: None,
                args: None,
                tool_call_id: None,
                output: None,
                error: None,
                created_at: timestamp,
            },
            HistoryEvent::Message {
                role,
                content,
                timestamp,
            } => Self {
                id: String::new(),
                event_type: "message".to_string(),
                role: Some(role),
                content: Some(content),
                tool: None,
                args: None,
                tool_call_id: None,
                output: None,
                error: None,
                created_at: timestamp,
            },
            HistoryEvent::ToolCall {
                tool_name,
                args,
                tool_call_id,
            } => Self {
                id: String::new(),
                event_type: "tool.call".to_string(),
                role: None,
                content: None,
                tool: Some(tool_name),
                args: Some(args),
                tool_call_id: Some(tool_call_id),
                output: None,
                error: None,
                created_at: String::new(),
            },
            HistoryEvent::ToolResult {
                tool_call_id,
                output,
                error,
            } => Self {
                id: String::new(),
                event_type: "tool.result".to_string(),
                role: None,
                content: None,
                tool: None,
                args: None,
                tool_call_id: Some(tool_call_id),
                output,
                error,
                created_at: String::new(),
            },
            HistoryEvent::Thinking { content } => Self {
                id: String::new(),
                event_type: "thinking".to_string(),
                role: None,
                content: Some(content),
                tool: None,
                args: None,
                tool_call_id: None,
                output: None,
                error: None,
                created_at: String::new(),
            },
            HistoryEvent::ModelChange { provider, model_id } => Self {
                id: String::new(),
                event_type: "model.change".to_string(),
                role: None,
                content: Some(format!("{} / {}", provider, model_id)),
                tool: None,
                args: None,
                tool_call_id: None,
                output: None,
                error: None,
                created_at: String::new(),
            },
            HistoryEvent::Compaction { summary } => Self {
                id: String::new(),
                event_type: "compaction".to_string(),
                role: None,
                content: Some(summary),
                tool: None,
                args: None,
                tool_call_id: None,
                output: None,
                error: None,
                created_at: String::new(),
            },
            HistoryEvent::Custom { custom_type } => Self {
                id: String::new(),
                event_type: custom_type,
                role: None,
                content: None,
                tool: None,
                args: None,
                tool_call_id: None,
                output: None,
                error: None,
                created_at: String::new(),
            },
        }
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

    // Use SessionService
    let sessions = state
        .session_service()
        .list_sessions(&agent_name, None) // TODO: Extract team from agent_name or query param
        .await
        .map_err(|e| ApiError::internal(format!("Failed to list sessions: {}", e), ""))?;

    // Convert to responses
    let responses: Vec<SessionResponse> = sessions.into_iter().map(Into::into).collect();

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

    // Use SessionService
    let session = state
        .session_service()
        .get_session(&agent_name, None, &session_id)
        .await
        .map_err(|e| ApiError::internal(format!("Failed to load session: {}", e), ""))?
        .ok_or_else(|| ApiError::not_found("session", session_id.clone(), ""))?;

    Ok(Json(session.into()))
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

    // Build query parameters
    let query = HistoryQuery {
        include_tool_calls: params.include_tool_calls,
        include_thinking: params.include_thinking,
        limit: params.limit.min(100),
        cursor: params.cursor.clone(),
    };

    // Delegate everything to SessionService (filtering, pagination, conversion)
    let result = state
        .session_service()
        .get_history(&agent_name, None, &session_id, query)
        .await
        .map_err(|e| ApiError::internal(format!("Failed to load session history: {}", e), ""))?;

    // Convert HistoryEvent to HistoryEventResponse
    let items: Vec<HistoryEventResponse> = result.events.into_iter().map(Into::into).collect();

    Ok(Json(HistoryResponse {
        session_id,
        items,
        cursor: result.cursor,
        has_more: result.has_more,
    }))
}

/// Branch a session
async fn branch_session(
    State(state): State<AppState>,
    Path((agent_name, session_id)): Path<(String, String)>,
    _request: Json<BranchRequest>,
) -> Result<Json<BranchResponse>, ApiError> {
    info!(
        "Branching session: {} for agent: {}",
        session_id, agent_name
    );

    // Use SessionService to create branch
    let branch_result = state
        .session_service()
        .branch_session(&agent_name, None, &session_id, None)
        .await
        .map_err(|e| ApiError::internal(format!("Failed to branch session: {}", e), ""))?;

    // Get the branched session info
    let new_session = state
        .session_service()
        .get_session(&agent_name, None, &branch_result.new_session_id)
        .await
        .map_err(|e| ApiError::internal(format!("Failed to get branched session: {}", e), ""))?
        .ok_or_else(|| ApiError::internal("Branched session not found after creation", ""))?;

    Ok(Json(BranchResponse {
        session: new_session.into(),
        parent_session_id: session_id,
    }))
}

/// Delete a session
async fn delete_session(
    State(state): State<AppState>,
    Path((agent_name, session_id)): Path<(String, String)>,
) -> Result<axum::http::StatusCode, ApiError> {
    info!("Deleting session: {} for agent: {}", session_id, agent_name);

    // Use SessionService
    state
        .session_service()
        .delete_session(&agent_name, None, &session_id)
        .await
        .map_err(|e| ApiError::internal(format!("Failed to delete session: {}", e), ""))?;

    Ok(axum::http::StatusCode::NO_CONTENT)
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
    use crate::session::SessionMessage;

    #[test]
    fn test_session_response_from_session_info() {
        let info = SessionInfo {
            id: "sess_123".to_string(),
            agent_name: "myagent".to_string(),
            created_at: 1234567890000,
            updated_at: 1234567890000,
            turn_count: 5,
            message_count: 10,
            context_window: 1000,
            total_input_tokens: 500,
            total_output_tokens: 500,
            parent_session_id: None,
            title: Some("Test Session".to_string()),
            ended: false,
        };
        let response: SessionResponse = info.into();

        assert_eq!(response.id, "sess_123");
        assert_eq!(response.instance_id, "myagent");
        assert_eq!(response.turn_count, 5);
        assert_eq!(response.title, Some("Test Session".to_string()));
    }

    #[test]
    fn test_history_response_from_user_message() {
        let event = SessionEvent::MessageV2(SessionMessage::user(
            "Hello",
            crate::session::events::MessageSource::User,
        ));

        let response: HistoryEventResponse = (&event).into();
        assert_eq!(response.event_type, "message.v2");
        assert_eq!(response.role, Some("user".to_string()));
        assert_eq!(response.content, Some("Hello".to_string()));
    }

    #[test]
    fn test_history_response_from_tool_call() {
        use crate::session::events::{EventEnvelope, ToolCallEvent};
        use chrono::Utc;
        use serde_json::json;

        let event = SessionEvent::ToolCall(ToolCallEvent {
            envelope: EventEnvelope {
                id: "evt_002".to_string(),
                ts: Utc::now(),
                session_id: None,
                seq: None,
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
        let event = SessionEvent::MessageV2(SessionMessage::assistant_text(
            "The answer is 42.",
            "openai",
            "gpt-4",
        ));

        let response: HistoryEventResponse = (&event).into();
        assert_eq!(response.event_type, "message.v2");
        assert_eq!(response.role, Some("assistant".to_string()));
        assert_eq!(response.content, Some("The answer is 42.".to_string()));
    }

    #[test]
    fn test_history_response_from_thinking() {
        use crate::session::events::{EventEnvelope, ThinkingEvent};
        use chrono::Utc;

        let event = SessionEvent::Thinking(ThinkingEvent {
            envelope: EventEnvelope {
                id: "evt_004".to_string(),
                ts: Utc::now(),
                session_id: None,
                seq: None,
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
        let info = SessionInfo {
            id: "sess_123".to_string(),
            agent_name: "myagent".to_string(),
            created_at: 1234567890000,
            updated_at: 1234567890000,
            turn_count: 0,
            message_count: 1,
            context_window: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            parent_session_id: None,
            title: None,
            ended: false,
        };

        let response: SessionResponse = info.into();
        assert_eq!(response.id, "sess_123");
        assert_eq!(response.title, None);
        assert_eq!(response.turn_count, 0);
    }

    #[test]
    fn test_session_response_with_parent() {
        let info = SessionInfo {
            id: "sess_child".to_string(),
            agent_name: "myagent".to_string(),
            created_at: 1234567890000,
            updated_at: 1234567890000,
            turn_count: 5,
            message_count: 10,
            context_window: 1000,
            total_input_tokens: 500,
            total_output_tokens: 500,
            parent_session_id: Some("sess_parent".to_string()),
            title: Some("Branched Session".to_string()),
            ended: false,
        };

        let response: SessionResponse = info.into();
        assert_eq!(response.id, "sess_child");
        assert_eq!(response.parent_session_id, Some("sess_parent".to_string()));
        assert_eq!(response.turn_count, 5);
        assert_eq!(response.title, Some("Branched Session".to_string()));
    }
}
