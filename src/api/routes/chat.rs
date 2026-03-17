//! Chat API Routes
//!
//! Implements chat endpoints per API_CONTRACT.md §4:
//! - POST /agents/{id}/chat - Send message with SSE streaming
//! - WebSocket /agents/{id}/ws - Bidirectional streaming (separate module)

use crate::api::error::ApiError;
use crate::api::state::AppState;
use crate::api::streaming::{ChatSseEvent, SseStream, TokenUsage};
use crate::engine::{AgenticEvent, LifecyclePhase};
use axum::{
    extract::{Path, State},
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info};

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
    Path(instance_id): Path<String>,
    headers: axum::http::HeaderMap,
    Json(request): Json<ChatRequest>,
) -> Result<impl axum::response::IntoResponse, ApiError> {
    debug!("Chat request for instance: {}", instance_id);

    // Check Accept header for streaming preference
    let accept_header = headers
        .get("Accept")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("text/event-stream");

    let streaming = accept_header.contains("text/event-stream")
        || accept_header == "*/*"
        || accept_header.is_empty();

    if streaming {
        // Return SSE stream
        let (sse_stream, sender) = SseStream::new();

        // Spawn the chat processing
        tokio::spawn(async move {
            if let Err(e) = process_chat_stream(state, instance_id, request, sender).await {
                error!("Chat stream error: {}", e);
            }
        });

        Ok::<_, ApiError>(sse_stream.into_response())
    } else {
        // Non-streaming response
        let response = process_chat_blocking(state, instance_id, request).await?;
        Ok::<_, ApiError>(Json(response).into_response())
    }
}

/// Process chat with streaming output
async fn process_chat_stream(
    _state: AppState,
    instance_id: String,
    request: ChatRequest,
    sender: tokio::sync::mpsc::Sender<ChatSseEvent>,
) -> anyhow::Result<()> {
    let run_id = format!("run_{}", chrono::Utc::now().timestamp_millis());
    info!(
        "Starting chat stream for instance: {} (run: {})",
        instance_id, run_id
    );

    // TODO: Load instance, get provider, tools, etc.
    // For now, send a simple response

    // Send acknowledgment
    let _ = sender
        .send(ChatSseEvent::Delta {
            text: "Processing your message...".to_string(),
        })
        .await;

    // TODO: Integrate with actual agentic loop
    // This is a placeholder implementation

    // Send done event
    let _ = sender
        .send(ChatSseEvent::Done {
            message_id: format!("msg_{}", uuid::Uuid::new_v4().simple()),
            session_id: request
                .session_id
                .unwrap_or_else(|| format!("sess_{}", uuid::Uuid::new_v4().simple())),
            turn_count: 1,
            usage: TokenUsage::default(),
        })
        .await;

    Ok(())
}

/// Process chat with blocking response
async fn process_chat_blocking(
    _state: AppState,
    instance_id: String,
    request: ChatRequest,
) -> Result<ChatResponse, ApiError> {
    info!("Blocking chat request for instance: {}", instance_id);

    // TODO: Load instance, get provider, tools, run agentic loop
    // This is a placeholder implementation

    Ok(ChatResponse {
        message: AssistantMessage {
            id: format!("msg_{}", uuid::Uuid::new_v4().simple()),
            role: "assistant".to_string(),
            content: format!("Echo: {}", request.message),
            created_at: chrono::Utc::now().to_rfc3339(),
        },
        session_id: request
            .session_id
            .unwrap_or_else(|| format!("sess_{}", uuid::Uuid::new_v4().simple())),
        turn_count: 1,
        usage: TokenUsage::default(),
        tool_calls: None,
    })
}

/// Convert engine events to SSE events and send
async fn emit_events_to_sse(
    event_receiver: &mut tokio::sync::mpsc::Receiver<AgenticEvent>,
    sse_sender: &tokio::sync::mpsc::Sender<ChatSseEvent>,
    _run_id: &str,
) -> anyhow::Result<(String, u32, TokenUsage)> {
    let mut final_text = String::new();
    let mut turn_count = 0u32;
    let usage = TokenUsage::default();

    while let Some(event) = event_receiver.recv().await {
        match &event {
            AgenticEvent::Assistant { text, is_final, .. } => {
                if !text.is_empty() {
                    final_text.push_str(text);
                    let _ = sse_sender
                        .send(ChatSseEvent::Delta { text: text.clone() })
                        .await;
                }
                if *is_final {
                    turn_count += 1;
                }
            }
            AgenticEvent::Thinking { text, .. } => {
                let _ = sse_sender
                    .send(ChatSseEvent::Thinking { text: text.clone() })
                    .await;
            }
            AgenticEvent::ToolStart {
                tool_id,
                name,
                params,
                ..
            } => {
                let _ = sse_sender
                    .send(ChatSseEvent::ToolCall {
                        id: tool_id.clone(),
                        tool: name.clone(),
                        args: params.clone(),
                        async_: false,
                    })
                    .await;
            }
            AgenticEvent::ToolEnd {
                tool_id,
                result,
                success,
                ..
            } => {
                let output = result.to_string();
                let error = if *success { None } else { Some(output.clone()) };
                let _ = sse_sender
                    .send(ChatSseEvent::ToolResult {
                        tool_call_id: tool_id.clone(),
                        output,
                        error,
                    })
                    .await;
            }
            AgenticEvent::Lifecycle {
                phase: LifecyclePhase::End,
                ..
            } => {
                break;
            }
            AgenticEvent::Lifecycle {
                phase: LifecyclePhase::Error,
                error: Some(err),
                ..
            } => {
                let _ = sse_sender
                    .send(ChatSseEvent::Error {
                        code: "execution_error".to_string(),
                        message: err.clone(),
                        tool_call_id: None,
                    })
                    .await;
                return Err(anyhow::anyhow!("Execution error: {}", err));
            }
            _ => {}
        }
    }

    Ok((final_text, turn_count, usage))
}

/// Create router for chat routes
pub fn router() -> Router<AppState> {
    Router::new().route("/agents/:id/chat", post(chat_handler))
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(req.session_id.is_none());
        assert_eq!(req.role, "user");
    }
}
