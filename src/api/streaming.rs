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
            let sse_line = format!("event: {}\ndata: {}\n\n", event_type, data);

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
pub fn engine_event_to_sse(
    event: &crate::engine::AgenticEvent,
    _run_id: &str,
) -> Option<ChatSseEvent> {
    match event {
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
