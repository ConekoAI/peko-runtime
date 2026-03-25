//! Chat API Routes (Stateless Architecture)
//!
//! Implements chat endpoints per ADR-013 stateless cold-start model:
//! - POST /agents/{name}/chat - Send message with SSE streaming
//! - Stateless execution: agent cold-starts per request
//!
//! ADR-013 Compliance:
//! - No persistent instance state
//! - Agent cold-starts on every request
//! - Loads config from disk, executes, exits
//!
//! NOTE: This module uses the unified EventStream interface (ADR-015)

use crate::api::error::ApiError;
use crate::api::state::AppState;
use crate::api::streaming::{event_stream_to_sse, ChatSseEvent, SseStream, TokenUsage};
use crate::common::services::MessageRequest;
use axum::{
    extract::{Path, State},
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use tracing::{debug, info};
use uuid::Uuid;

/// Chat request body
#[derive(Debug, Deserialize)]
pub struct ChatRequest {
    /// User message content
    pub message: String,
    /// Session ID to resume (optional)
    #[serde(rename = "session_id")]
    pub session_id: Option<String>,
    /// Message role (default: "user")
    #[serde(default = "default_user_role")]
    pub role: String,
}

fn default_user_role() -> String {
    "user".to_string()
}

/// Non-streaming chat response
#[derive(Debug, Serialize)]
pub struct ChatResponse {
    pub message: AssistantMessage,
    #[serde(rename = "session_id")]
    pub session_id: String,
    #[serde(rename = "turn_count")]
    pub turn_count: u32,
    pub usage: TokenUsage,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "tool_calls")]
    pub tool_calls: Option<Vec<ToolCallSummary>>,
}

/// Assistant message in response
#[derive(Debug, Serialize)]
pub struct AssistantMessage {
    pub id: String,
    pub role: String,
    pub content: String,
    #[serde(rename = "created_at")]
    pub created_at: String,
}

/// Tool call summary for non-streaming response
#[derive(Debug, Serialize)]
pub struct ToolCallSummary {
    pub id: String,
    pub tool: String,
    #[serde(rename = "args")]
    pub args: serde_json::Value,
    pub output: String,
}

/// Chat handler - routes to streaming or non-streaming based on Accept header
async fn chat_handler(
    State(state): State<AppState>,
    Path(agent_name): Path<String>,
    headers: axum::http::HeaderMap,
    Json(request): Json<ChatRequest>,
) -> Result<impl axum::response::IntoResponse, ApiError> {
    debug!("Chat request for agent: {}", agent_name);

    // Check Accept header for streaming preference
    let accept_header = headers
        .get("Accept")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("text/event-stream");

    let streaming = accept_header.contains("text/event-stream")
        || accept_header == "*/*"
        || accept_header.is_empty();

    if streaming {
        // Use unified EventStream API with SSE adapter (ADR-015)
        let msg_request = MessageRequest::new(agent_name.clone(), request.message.clone())
            .with_session_opt(request.session_id.clone())
            .with_new_session(request.session_id.is_none());

        // Get EventStream from unified API
        let event_stream = state
            .message_service()
            .send_message_unified(msg_request)
            .await
            .map_err(|e| ApiError::internal(format!("Failed to start stream: {}", e), ""))?;

        // Convert to SSE using adapter
        let (sse_stream, _handle) = event_stream_to_sse(event_stream);

        Ok::<_, ApiError>(sse_stream.into_response())
    } else {
        // Non-streaming response using MessageService
        let response = process_chat_blocking(state, agent_name, request).await?;
        Ok::<_, ApiError>(Json(response).into_response())
    }
}

/// Process chat with blocking response using MessageService
async fn process_chat_blocking(
    state: AppState,
    agent_name: String,
    request: ChatRequest,
) -> Result<ChatResponse, ApiError> {
    info!("Blocking chat request for agent: {}", agent_name);

    // Build message request
    let msg_request = MessageRequest::new(agent_name.clone(), request.message)
        .with_session_opt(request.session_id.clone())
        .with_new_session(request.session_id.is_none());

    // Use MessageService
    let result = state
        .message_service()
        .send_message(msg_request)
        .await
        .map_err(|e| ApiError::internal(format!("Execution failed: {}", e), ""))?;

    // Convert tool calls to summaries
    let tool_calls = if result.tool_calls.is_empty() {
        None
    } else {
        Some(
            result
                .tool_calls
                .into_iter()
                .map(|tc| ToolCallSummary {
                    id: tc.id,
                    tool: tc.name,
                    args: tc.parameters,
                    output: tc.result.unwrap_or_default(),
                })
                .collect(),
        )
    };

    Ok(ChatResponse {
        message: AssistantMessage {
            id: format!("msg_{}", Uuid::new_v4().simple()),
            role: "assistant".to_string(),
            content: result.content,
            created_at: chrono::Utc::now().to_rfc3339(),
        },
        session_id: result.session_id,
        turn_count: result.iterations as u32,
        usage: TokenUsage {
            input_tokens: result.usage.input,
            output_tokens: result.usage.output,
            total_tokens: result.usage.total,
        },
        tool_calls,
    })
}

/// Create router for chat routes
pub fn router() -> Router<AppState> {
    // ADR-013: Use agent_name (not instance_id) in path
    Router::new().route("/agents/:name/chat", post(chat_handler))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::services::message_service::generate_session_id;
    use tokio::sync::mpsc;

    #[test]
    fn test_chat_request_deserialization() {
        let json = r#"{
            "message": "Hello",
            "session_id": "sess_123",
            "role": "user"
        }"#;
        let req: ChatRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.message, "Hello");
        assert_eq!(req.session_id, Some("sess_123".to_string()));
        assert_eq!(req.role, "user");
    }

    #[test]
    fn test_chat_request_defaults() {
        let json = r#"{"message": "Hi"}"#;
        let req: ChatRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.message, "Hi");
        assert_eq!(req.session_id, None);
        assert_eq!(req.role, "user"); // default
    }

    #[test]
    fn test_generate_session_id() {
        let id1 = generate_session_id();
        let id2 = generate_session_id();
        // Should be valid UUID format (36 characters with hyphens)
        assert_eq!(id1.len(), 36);
        assert_eq!(id2.len(), 36);
        // Should be unique
        assert_ne!(id1, id2);
    }
}
