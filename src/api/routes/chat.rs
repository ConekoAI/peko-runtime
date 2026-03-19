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

use crate::api::error::ApiError;
use crate::api::state::AppState;
use crate::api::streaming::{ChatSseEvent, SseStream, TokenUsage};
use crate::engine::{AgenticEvent, LifecyclePhase};
use crate::observability::performance::GLOBAL_METRICS;
use axum::{
    extract::{Path, State},
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info};
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

/// Generate a session ID if not provided
fn generate_session_id() -> String {
    format!("sess_{}", Uuid::new_v4().simple())
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
        // Return SSE stream
        let (sse_stream, sender) = SseStream::new();

        // Spawn the chat processing (stateless cold-start)
        tokio::spawn(async move {
            if let Err(e) = process_chat_stream(state, agent_name, request, sender).await {
                error!("Chat stream error: {}", e);
            }
        });

        Ok::<_, ApiError>(sse_stream.into_response())
    } else {
        // Non-streaming response (stateless cold-start)
        let response = process_chat_blocking(state, agent_name, request).await?;
        Ok::<_, ApiError>(Json(response).into_response())
    }
}

/// Process chat with streaming output (stateless execution)
async fn process_chat_stream(
    state: AppState,
    agent_name: String,
    request: ChatRequest,
    sender: tokio::sync::mpsc::Sender<ChatSseEvent>,
) -> anyhow::Result<()> {
    let run_id = format!("run_{}", chrono::Utc::now().timestamp_millis());
    info!(
        "Starting stateless chat stream for agent: {} (run: {})",
        agent_name, run_id
    );

    let session_id = request
        .session_id
        .clone()
        .unwrap_or_else(generate_session_id);

    // ADR-013: Cold-start sequence
    // 1. Check agent is registered
    if !state.config_registry().exists(&agent_name).await {
        let _ = sender
            .send(ChatSseEvent::Error {
                code: "agent_not_found".to_string(),
                message: format!("Agent '{}' not found", agent_name),
                tool_call_id: None,
            })
            .await;
        return Err(anyhow::anyhow!("Agent not found: {}", agent_name));
    }

    // 2. Send acknowledgment (cold-start beginning)
    let _ = sender
        .send(ChatSseEvent::Delta {
            text: "Processing your message...".to_string(),
        })
        .await;

    // 3. Execute statelessly
    let exec_request = crate::agent::stateless_service::ExecutionRequest {
        agent_name: agent_name.clone(),
        session_id: session_id.clone(),
        message: request.message.clone(),
        context: None,
        timeout_secs: None,
    };

    let start_time = std::time::Instant::now();

    match state.agent_service().execute(exec_request).await {
        Ok(result) => {
            let duration_ms = start_time.elapsed().as_millis() as u64;
            info!(
                "Stateless execution completed for {} in {}ms (success: {})",
                agent_name, duration_ms, result.success
            );

            // Send the response content
            if !result.response.is_empty() {
                let _ = sender
                    .send(ChatSseEvent::Delta {
                        text: result.response,
                    })
                    .await;
            }

            // Convert tool calls to SSE events
            for tc in &result.tool_calls {
                let tool_id = format!("tool_{}", Uuid::new_v4().simple());
                let _ = sender
                    .send(ChatSseEvent::ToolCall {
                        id: tool_id.clone(),
                        tool: tc.name.clone(),
                        args: tc.parameters.clone(),
                        async_: false,
                    })
                    .await;

                // Send tool result if available
                if let Some(ref output) = tc.result {
                    let _ = sender
                        .send(ChatSseEvent::ToolResult {
                            tool_call_id: tool_id,
                            output: output.clone(),
                            error: None,
                        })
                        .await;
                }
            }

            // Record first token latency (end-to-end time as proxy)
            GLOBAL_METRICS.record_first_token(start_time.elapsed());

            // Send done event
            let _ = sender
                .send(ChatSseEvent::Done {
                    message_id: format!("msg_{}", Uuid::new_v4().simple()),
                    session_id,
                    turn_count: result.iterations as u32,
                    usage: TokenUsage {
                        input_tokens: result.usage.input,
                        output_tokens: result.usage.output,
                        total_tokens: result.usage.total,
                    },
                })
                .await;
        }
        Err(e) => {
            error!("Stateless execution failed: {}", e);
            let _ = sender
                .send(ChatSseEvent::Error {
                    code: "execution_error".to_string(),
                    message: format!("Execution failed: {}", e),
                    tool_call_id: None,
                })
                .await;
            return Err(e);
        }
    }

    Ok(())
}

/// Process chat with blocking response (stateless execution)
async fn process_chat_blocking(
    state: AppState,
    agent_name: String,
    request: ChatRequest,
) -> Result<ChatResponse, ApiError> {
    info!("Stateless blocking chat request for agent: {}", agent_name);

    let session_id = request
        .session_id
        .clone()
        .unwrap_or_else(generate_session_id);

    // Check agent exists
    if !state.config_registry().exists(&agent_name).await {
        return Err(ApiError::not_found("agent", agent_name.clone(), ""));
    }

    // Execute statelessly
    let exec_request = crate::agent::stateless_service::ExecutionRequest {
        agent_name: agent_name.clone(),
        session_id: session_id.clone(),
        message: request.message.clone(),
        context: None,
        timeout_secs: None,
    };

    let start_time = std::time::Instant::now();

    match state.agent_service().execute(exec_request).await {
        Ok(result) => {
            let duration_ms = start_time.elapsed().as_millis();
            info!(
                "Stateless execution completed for {} in {}ms",
                agent_name, duration_ms
            );

            // Convert tool calls to summaries
            let tool_calls = if result.tool_calls.is_empty() {
                None
            } else {
                Some(
                    result
                        .tool_calls
                        .into_iter()
                        .map(|tc| ToolCallSummary {
                            id: format!("tool_{}", Uuid::new_v4().simple()),
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
                    content: result.response,
                    created_at: chrono::Utc::now().to_rfc3339(),
                },
                session_id,
                turn_count: result.iterations as u32,
                usage: TokenUsage {
                    input_tokens: result.usage.input,
                    output_tokens: result.usage.output,
                    total_tokens: result.usage.total,
                },
                tool_calls,
            })
        }
        Err(e) => {
            error!("Stateless execution failed: {}", e);
            Err(ApiError::internal(format!("Execution failed: {}", e), ""))
        }
    }
}

/// Create router for chat routes
pub fn router() -> Router<AppState> {
    // ADR-013: Use agent_name (not instance_id) in path
    Router::new().route("/agents/:name/chat", post(chat_handler))
}

#[cfg(test)]
mod tests {
    use super::*;
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
        assert!(id1.starts_with("sess_"));
        assert!(id2.starts_with("sess_"));
        assert_ne!(id1, id2);
    }
}
