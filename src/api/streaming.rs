//! Streaming support for chat responses
//!
//! Provides Server-Sent Events (SSE) streaming for agent responses.
//! Supports delta streaming, tool calls, and tool results.

use axum::{
    body::Body,
    response::{IntoResponse, Response},
};
use futures::StreamExt;
use serde::Serialize;
use std::convert::Infallible;
use tokio::sync::mpsc;
use uuid::Uuid;

/// SSE event types for chat streaming
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ChatSseEvent {
    /// Text delta (incremental content)
    Delta { text: String },
    /// Tool call started
    ToolCall {
        id: String,
        tool: String,
        #[serde(rename = "args")]
        args: serde_json::Value,
        #[serde(rename = "async")]
        async_: bool,
    },
    /// Tool call completed
    ToolResult {
        #[serde(rename = "tool_call_id")]
        tool_call_id: String,
        output: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// Thinking/reasoning content
    Thinking { text: String },
    /// Stream completed successfully
    Done {
        #[serde(rename = "message_id")]
        message_id: String,
        #[serde(rename = "session_id")]
        session_id: String,
        #[serde(rename = "turn_count")]
        turn_count: u32,
        usage: TokenUsage,
    },
    /// Error occurred
    Error {
        code: String,
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        #[serde(rename = "tool_call_id")]
        tool_call_id: Option<String>,
    },
}

/// Token usage statistics
#[derive(Debug, Clone, Serialize, Default)]
pub struct TokenUsage {
    #[serde(rename = "input_tokens")]
    pub input_tokens: u64,
    #[serde(rename = "output_tokens")]
    pub output_tokens: u64,
    #[serde(rename = "total_tokens")]
    pub total_tokens: u64,
}

/// SSE stream response
pub struct SseStream {
    receiver: mpsc::Receiver<ChatSseEvent>,
}

impl SseStream {
    /// Create a new SSE stream with a channel
    #[must_use]
    pub fn new() -> (Self, mpsc::Sender<ChatSseEvent>) {
        let (sender, receiver) = mpsc::channel(100);
        (Self { receiver }, sender)
    }
}

impl Default for SseStream {
    fn default() -> Self {
        Self::new().0
    }
}

impl IntoResponse for SseStream {
    fn into_response(self) -> Response {
        let stream = tokio_stream::wrappers::ReceiverStream::new(self.receiver);

        let body_stream = stream.map(|event| {
            let event_type = match &event {
                ChatSseEvent::Delta { .. } => "delta",
                ChatSseEvent::ToolCall { .. } => "tool_call",
                ChatSseEvent::ToolResult { .. } => "tool_result",
                ChatSseEvent::Thinking { .. } => "thinking",
                ChatSseEvent::Done { .. } => "done",
                ChatSseEvent::Error { .. } => "error",
            };

            let data = serde_json::to_string(&event).unwrap_or_default();
            let sse_line = format!("event: {event_type}\ndata: {data}\n\n");

            Ok::<_, Infallible>(axum::body::Bytes::from(sse_line))
        });

        Response::builder()
            .header("Content-Type", "text/event-stream")
            .header("Cache-Control", "no-cache")
            .header("Connection", "keep-alive")
            .body(Body::from_stream(body_stream))
            .unwrap()
    }
}

/// Convert an engine event to an SSE event
#[must_use] 
pub fn engine_event_to_sse(
    event: &crate::engine::AgenticEvent,
    _run_id: &str,
) -> Option<ChatSseEvent> {
    match event {
        // New event type with clear semantics
        crate::engine::AgenticEvent::AssistantText { text, .. } => {
            // All AssistantText events are complete blocks, not deltas
            Some(ChatSseEvent::Delta { text: text.clone() })
        }
        // Deprecated: legacy event type (backward compatibility)
        #[allow(deprecated)]
        crate::engine::AgenticEvent::Assistant { text, is_delta, .. } => {
            if *is_delta {
                Some(ChatSseEvent::Delta { text: text.clone() })
            } else {
                // For non-delta, we might want to buffer or handle differently
                Some(ChatSseEvent::Delta { text: text.clone() })
            }
        }
        crate::engine::AgenticEvent::Thinking { text, .. } => {
            Some(ChatSseEvent::Thinking { text: text.clone() })
        }
        crate::engine::AgenticEvent::ToolStart {
            tool_id,
            name,
            params,
            ..
        } => {
            Some(ChatSseEvent::ToolCall {
                id: tool_id.clone(),
                tool: name.clone(),
                args: params.clone(),
                async_: false, // TODO: Detect async tools
            })
        }
        crate::engine::AgenticEvent::ToolEnd {
            tool_id,
            result,
            success,
            ..
        } => {
            let output = result.to_string();
            let error = if *success { None } else { Some(output.clone()) };
            Some(ChatSseEvent::ToolResult {
                tool_call_id: tool_id.clone(),
                output,
                error,
            })
        }
        _ => None, // Skip lifecycle events, etc.
    }
}

/// Convert an `EventStream` to an SSE response stream
///
/// This adapter bridges the unified `EventStream` (ADR-015) to SSE format
/// used by the HTTP API. It spawns a task to forward events and properly
/// awaits the completion signal to ensure session persistence.
#[must_use] 
pub fn event_stream_to_sse(
    event_stream: crate::channels::EventStream,
) -> (SseStream, tokio::task::JoinHandle<anyhow::Result<()>>) {
    let (sse_stream, sender) = SseStream::new();

    let handle = tokio::spawn(async move {
        let mut event_rx = event_stream.receiver;
        let completion = event_stream.completion;
        let run_id = format!("run_{}", chrono::Utc::now().timestamp_millis());
        let session_id = event_stream.session_id;
        let turn_count = 0u32;
        let mut usage = TokenUsage::default();
        let mut end_received = false;

        while let Some(event) = event_rx.recv().await {
            // Convert and send SSE events
            if let Some(sse_event) = engine_event_to_sse(&event, &run_id) {
                if sender.send(sse_event).await.is_err() {
                    break;
                }
            }

            // Track metadata from lifecycle events
            match &event {
                crate::engine::AgenticEvent::Lifecycle {
                    phase: crate::engine::LifecyclePhase::End,
                    ..
                } => {
                    end_received = true;
                    // Send completion event to client
                    let _ = sender
                        .send(ChatSseEvent::Done {
                            message_id: format!("msg_{}", Uuid::new_v4().simple()),
                            session_id: session_id.clone(),
                            turn_count,
                            usage: usage.clone(),
                        })
                        .await;
                    // Don't break yet - wait for receiver to close
                }
                crate::engine::AgenticEvent::Lifecycle {
                    phase: crate::engine::LifecyclePhase::Error,
                    error,
                    ..
                } => {
                    let _ = sender
                        .send(ChatSseEvent::Error {
                            code: "execution_error".to_string(),
                            message: error.clone().unwrap_or_default(),
                            tool_call_id: None,
                        })
                        .await;
                    end_received = true;
                }
                crate::engine::AgenticEvent::Usage {
                    prompt_tokens,
                    completion_tokens,
                    total_tokens,
                    ..
                } => {
                    usage.input_tokens = u64::from(*prompt_tokens);
                    usage.output_tokens = u64::from(*completion_tokens);
                    usage.total_tokens = u64::from(*total_tokens);
                }
                _ => {}
            }
        }

        // Receiver closed - CRITICAL: Wait for completion signal before returning
        // This ensures session persistence is complete
        if end_received {
            match tokio::time::timeout(std::time::Duration::from_secs(30), completion).await {
                Ok(Ok(Ok(()))) => Ok(()),
                Ok(Ok(Err(e))) => Err(e),
                Ok(Err(_recv_error)) => {
                    tracing::warn!("Completion sender dropped without signal");
                    Ok(())
                }
                Err(_) => {
                    tracing::error!("Completion timeout - session persistence may be incomplete");
                    Err(anyhow::anyhow!("Completion timeout"))
                }
            }
        } else {
            Ok(())
        }
    });

    (sse_stream, handle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::AgenticEvent;

    #[test]
    fn test_sse_event_serialization() {
        let event = ChatSseEvent::Delta {
            text: "Hello".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"event\":\"delta\""));
        assert!(json.contains("\"text\":\"Hello\""));
    }

    #[test]
    fn test_tool_call_serialization() {
        let event = ChatSseEvent::ToolCall {
            id: "tc_123".to_string(),
            tool: "web_search".to_string(),
            args: serde_json::json!({"query": "test"}),
            async_: false,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"event\":\"tool_call\""));
        assert!(json.contains("\"tool\":\"web_search\""));
        assert!(json.contains("\"async\""));
    }

    #[test]
    fn test_tool_result_serialization() {
        let event = ChatSseEvent::ToolResult {
            tool_call_id: "tc_123".to_string(),
            output: "Search results".to_string(),
            error: None,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"event\":\"tool_result\""));
        assert!(json.contains("\"tool_call_id\":\"tc_123\""));
        assert!(json.contains("\"output\":\"Search results\""));
        assert!(!json.contains("error")); // Should be skipped when None
    }

    #[test]
    fn test_tool_result_with_error() {
        let event = ChatSseEvent::ToolResult {
            tool_call_id: "tc_456".to_string(),
            output: "Error occurred".to_string(),
            error: Some("Tool failed".to_string()),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"error\":\"Tool failed\""));
    }

    #[test]
    fn test_thinking_serialization() {
        let event = ChatSseEvent::Thinking {
            text: "Let me think...".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"event\":\"thinking\""));
        assert!(json.contains("\"text\":\"Let me think...\""));
    }

    #[test]
    fn test_done_serialization() {
        let event = ChatSseEvent::Done {
            message_id: "msg_123".to_string(),
            session_id: "sess_456".to_string(),
            turn_count: 3,
            usage: TokenUsage {
                input_tokens: 100,
                output_tokens: 50,
                total_tokens: 150,
            },
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"event\":\"done\""));
        assert!(json.contains("\"message_id\":\"msg_123\""));
        assert!(json.contains("\"turn_count\":3"));
        assert!(json.contains("\"input_tokens\":100"));
        assert!(json.contains("\"output_tokens\":50"));
        assert!(json.contains("\"total_tokens\":150"));
    }

    #[test]
    fn test_error_serialization() {
        let event = ChatSseEvent::Error {
            code: "rate_limit".to_string(),
            message: "Too many requests".to_string(),
            tool_call_id: Some("tc_789".to_string()),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"event\":\"error\""));
        assert!(json.contains("\"code\":\"rate_limit\""));
        assert!(json.contains("\"message\":\"Too many requests\""));
        assert!(json.contains("\"tool_call_id\":\"tc_789\""));
    }

    #[test]
    fn test_error_without_tool_call_id() {
        let event = ChatSseEvent::Error {
            code: "general_error".to_string(),
            message: "Something went wrong".to_string(),
            tool_call_id: None,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(!json.contains("tool_call_id")); // Should be skipped when None
    }

    #[test]
    fn test_token_usage_default() {
        let usage = TokenUsage::default();
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.output_tokens, 0);
        assert_eq!(usage.total_tokens, 0);
    }

    #[test]
    fn test_sse_stream_creation() {
        let (stream, sender) = SseStream::new();
        // Just verify it compiles and creates properly
        drop(stream);
        drop(sender);
    }

    #[test]
    fn test_engine_event_to_sse_assistant_text() {
        // Test new AssistantText event
        let event = AgenticEvent::AssistantText {
            run_id: "run_123".to_string(),
            text: "Hello".to_string(),
            sequence: 1,
            is_interstitial: false,
        };
        let sse = engine_event_to_sse(&event, "run_123");
        match sse {
            Some(ChatSseEvent::Delta { text }) => assert_eq!(text, "Hello"),
            _ => panic!("Expected Delta event"),
        }
    }

    #[test]
    #[allow(deprecated)]
    fn test_engine_event_to_sse_assistant_delta_legacy() {
        // Test deprecated Assistant event (backward compatibility)
        let event = AgenticEvent::Assistant {
            run_id: "run_123".to_string(),
            text: "Hello".to_string(),
            is_delta: true,
            is_final: false,
        };
        let sse = engine_event_to_sse(&event, "run_123");
        match sse {
            Some(ChatSseEvent::Delta { text }) => assert_eq!(text, "Hello"),
            _ => panic!("Expected Delta event"),
        }
    }

    #[test]
    fn test_engine_event_to_sse_thinking() {
        let event = AgenticEvent::Thinking {
            run_id: "run_123".to_string(),
            text: "Thinking...".to_string(),
            is_delta: true,
            is_final: false,
            signature: None,
        };
        let sse = engine_event_to_sse(&event, "run_123");
        match sse {
            Some(ChatSseEvent::Thinking { text }) => assert_eq!(text, "Thinking..."),
            _ => panic!("Expected Thinking event"),
        }
    }

    #[test]
    fn test_engine_event_to_sse_tool_start() {
        let event = AgenticEvent::ToolStart {
            run_id: "run_123".to_string(),
            tool_id: "tc_001".to_string(),
            name: "web_search".to_string(),
            params: serde_json::json!({"query": "rust"}),
        };
        let sse = engine_event_to_sse(&event, "run_123");
        match sse {
            Some(ChatSseEvent::ToolCall {
                id,
                tool,
                args,
                async_,
            }) => {
                assert_eq!(id, "tc_001");
                assert_eq!(tool, "web_search");
                assert_eq!(args, serde_json::json!({"query": "rust"}));
                assert!(!async_);
            }
            _ => panic!("Expected ToolCall event"),
        }
    }

    #[test]
    fn test_engine_event_to_sse_tool_end_success() {
        let event = AgenticEvent::ToolEnd {
            run_id: "run_123".to_string(),
            tool_id: "tc_001".to_string(),
            result: serde_json::json!("result"),
            success: true,
            duration_ms: 100,
        };
        let sse = engine_event_to_sse(&event, "run_123");
        match sse {
            Some(ChatSseEvent::ToolResult {
                tool_call_id,
                output,
                error,
            }) => {
                assert_eq!(tool_call_id, "tc_001");
                assert!(error.is_none());
                assert!(output.contains("result"));
            }
            _ => panic!("Expected ToolResult event"),
        }
    }

    #[test]
    fn test_engine_event_to_sse_tool_end_failure() {
        let event = AgenticEvent::ToolEnd {
            run_id: "run_123".to_string(),
            tool_id: "tc_002".to_string(),
            result: serde_json::json!("Tool failed"),
            success: false,
            duration_ms: 50,
        };
        let sse = engine_event_to_sse(&event, "run_123");
        match sse {
            Some(ChatSseEvent::ToolResult {
                tool_call_id,
                error,
                ..
            }) => {
                assert_eq!(tool_call_id, "tc_002");
                assert!(error.is_some());
            }
            _ => panic!("Expected ToolResult event"),
        }
    }
}
