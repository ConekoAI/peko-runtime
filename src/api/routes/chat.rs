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
//! NOTE: This module uses the unified `EventStream` interface (ADR-015)

use crate::agent::stateless_service::MessageRequest;
use crate::api::error::ApiError;
use crate::api::state::AppState;
use crate::api::streaming::{event_stream_to_sse, TokenUsage};
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
        // Use unified EventStream API with SSE adapter (ADR-016 / ADR-021 Phase 2)
        let msg_request = MessageRequest::new(agent_name.clone(), request.message.clone())
            .with_session_opt(request.session_id.clone())
            .with_new_session(request.session_id.is_none());

        // Get cancellable EventStream from StatelessAgentService
        let (event_stream, loop_handle) = state
            .agent_service()
            .execute_message_streaming_cancellable(msg_request)
            .await
            .map_err(|e| ApiError::internal(format!("Failed to start stream: {e}"), ""))?;

        // Convert to SSE using adapter
        let (sse_stream, forwarder_handle) = event_stream_to_sse(event_stream);

        // Spawn a task that aborts the agentic loop when the client disconnects
        tokio::spawn(async move {
            // Wait for the forwarder to finish (client disconnect or loop completion)
            let _ = forwarder_handle.await;
            // If the loop is still running, abort it
            if !loop_handle.is_finished() {
                debug!("Client disconnected or forwarder ended, aborting agentic loop");
                loop_handle.abort();
            }
        });

        Ok::<_, ApiError>(sse_stream.into_response())
    } else {
        // Non-streaming response using StatelessAgentService directly
        let response = process_chat_blocking(state, agent_name, request).await?;
        Ok::<_, ApiError>(Json(response).into_response())
    }
}

/// Process chat with blocking response using `StatelessAgentService` directly
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

    // Use StatelessAgentService directly (ADR-016)
    let result = state
        .agent_service()
        .execute_message(msg_request)
        .await
        .map_err(|e| ApiError::internal(format!("Execution failed: {e}"), ""))?;

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
    
    use uuid::Uuid;

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
        // Use uuid crate directly for generating session IDs
        let id1 = Uuid::new_v4().to_string();
        let id2 = Uuid::new_v4().to_string();
        // Should be valid UUID format (36 characters with hyphens)
        assert_eq!(id1.len(), 36);
        assert_eq!(id2.len(), 36);
        // Should be unique
        assert_ne!(id1, id2);
    }

    /// Test `MessageRequest` builder from `stateless_service` (ADR-016)
    #[test]
    fn test_message_request_builder_for_api() {
        let request = MessageRequest::new("my-agent", "Hello")
            .with_team("default")
            .with_session("sess_123")
            .with_new_session(false)
            .with_timeout(60);

        assert_eq!(request.agent_name, "my-agent");
        assert_eq!(request.message, "Hello");
        assert_eq!(request.team, Some("default".to_string()));
        assert_eq!(request.session_id, Some("sess_123".to_string()));
        assert!(!request.new_session);
        assert_eq!(request.timeout_secs, Some(60));
    }

    /// Test `MessageRequest` defaults for API
    #[test]
    fn test_message_request_defaults_for_api() {
        let request = MessageRequest::new("my-agent", "Hello");

        assert_eq!(request.agent_name, "my-agent");
        assert_eq!(request.message, "Hello");
        assert_eq!(request.team, None);
        assert_eq!(request.session_id, None);
        assert!(!request.new_session);
        assert_eq!(request.timeout_secs, None);
    }

    /// Test that `ChatRequest` properly converts to `MessageRequest`
    #[test]
    fn test_chat_request_to_message_request() {
        let chat_req = ChatRequest {
            message: "Test message".to_string(),
            session_id: Some("test-session".to_string()),
            role: "user".to_string(),
        };

        // Simulate what the handler does
        let msg_request = MessageRequest::new("test-agent", chat_req.message)
            .with_session_opt(chat_req.session_id.clone())
            .with_new_session(chat_req.session_id.is_none());

        assert_eq!(msg_request.agent_name, "test-agent");
        assert_eq!(msg_request.message, "Test message");
        assert_eq!(msg_request.session_id, Some("test-session".to_string()));
        assert!(!msg_request.new_session);
    }

    /// Test `ChatResponse` structure
    #[test]
    fn test_chat_response_structure() {
        let response = ChatResponse {
            message: AssistantMessage {
                id: "msg_123".to_string(),
                role: "assistant".to_string(),
                content: "Hello!".to_string(),
                created_at: "2024-01-01T00:00:00Z".to_string(),
            },
            session_id: "sess_456".to_string(),
            turn_count: 1,
            usage: TokenUsage {
                input_tokens: 10,
                output_tokens: 5,
                total_tokens: 15,
            },
            tool_calls: None,
        };

        assert_eq!(response.session_id, "sess_456");
        assert_eq!(response.turn_count, 1);
        assert_eq!(response.usage.total_tokens, 15);
    }

    /// Test `ToolCallSummary` structure
    #[test]
    fn test_tool_call_summary() {
        let tool_call = ToolCallSummary {
            id: "tc_123".to_string(),
            tool: "read_file".to_string(),
            args: serde_json::json!({"path": "/tmp"}),
            output: "File contents".to_string(),
        };

        assert_eq!(tool_call.id, "tc_123");
        assert_eq!(tool_call.tool, "read_file");
        assert_eq!(tool_call.output, "File contents");
    }
}
