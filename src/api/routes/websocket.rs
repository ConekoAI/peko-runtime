//! WebSocket Chat API Routes
//!
//! Implements WebSocket endpoint per `API_CONTRACT.md` §4.3:
//! - <ws://localhost:11435/agents/{id}/ws> - Bidirectional streaming chat

use crate::api::error::ApiError;
use crate::api::state::AppState;
use axum::{
    extract::{Path, State, WebSocketUpgrade},
    response::Response,
    routing::get,
    Router,
};
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, warn};

/// WebSocket client message types
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsClientMessage {
    /// Send a message to the agent
    Message {
        /// Client-generated request ID
        id: String,
        /// Message content
        content: String,
        /// Optional session ID to resume
        #[serde(rename = "session_id")]
        session_id: Option<String>,
    },
    /// Ping to keep connection alive
    Ping,
    /// Close the connection gracefully
    Close,
}

/// WebSocket server message types
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsServerMessage {
    /// Connection handshake
    Hello {
        /// Instance ID
        #[serde(rename = "instance_id")]
        instance_id: String,
        /// Current active session ID (if any)
        #[serde(rename = "session_id")]
        session_id: Option<String>,
    },
    /// Acknowledge receipt of client message
    Ack {
        /// Client request ID that was received
        #[serde(rename = "request_id")]
        request_id: String,
    },
    /// Text delta (streaming response)
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
    /// Turn completed
    Done {
        #[serde(rename = "message_id")]
        message_id: String,
        #[serde(rename = "session_id")]
        session_id: String,
        #[serde(rename = "turn_count")]
        turn_count: u32,
        usage: TokenUsage,
    },
    /// Pong response to ping
    Pong,
    /// Error occurred
    Error {
        code: String,
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        #[serde(rename = "request_id")]
        request_id: Option<String>,
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

/// WebSocket upgrade handler
async fn websocket_handler(
    Path(instance_id): Path<String>,
    State(_state): State<AppState>,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    info!("WebSocket upgrade request for instance: {}", instance_id);

    Ok(ws.on_upgrade(move |socket| handle_websocket(socket, instance_id)))
}

/// Handle WebSocket connection
async fn handle_websocket(mut socket: axum::extract::ws::WebSocket, instance_id: String) {
    info!(
        "WebSocket connection established for instance: {}",
        instance_id
    );

    // Send hello message
    let hello = WsServerMessage::Hello {
        instance_id: instance_id.clone(),
        session_id: None, // TODO: Get actual session ID
    };

    if let Err(e) = send_message(&mut socket, hello).await {
        error!("Failed to send hello message: {}", e);
        return;
    }

    // Handle incoming messages
    while let Some(msg) = socket.recv().await {
        match msg {
            Ok(axum::extract::ws::Message::Text(text)) => {
                debug!("Received WebSocket message: {}", text);

                match serde_json::from_str::<WsClientMessage>(&text) {
                    Ok(client_msg) => {
                        if let Err(e) =
                            handle_client_message(&mut socket, &instance_id, client_msg).await
                        {
                            error!("Error handling client message: {}", e);
                            let _ = send_message(
                                &mut socket,
                                WsServerMessage::Error {
                                    code: "handler_error".to_string(),
                                    message: e.to_string(),
                                    request_id: None,
                                },
                            )
                            .await;
                        }
                    }
                    Err(e) => {
                        warn!("Failed to parse client message: {}", e);
                        let _ = send_message(
                            &mut socket,
                            WsServerMessage::Error {
                                code: "parse_error".to_string(),
                                message: format!("Invalid message format: {e}"),
                                request_id: None,
                            },
                        )
                        .await;
                    }
                }
            }
            Ok(axum::extract::ws::Message::Close(_)) => {
                info!(
                    "WebSocket close frame received for instance: {}",
                    instance_id
                );
                break;
            }
            Ok(axum::extract::ws::Message::Ping(data)) => {
                // Axum automatically handles ping/pong, but we can log it
                debug!("Received ping");
                if let Err(e) = socket.send(axum::extract::ws::Message::Pong(data)).await {
                    error!("Failed to send pong: {}", e);
                    break;
                }
            }
            Err(e) => {
                error!("WebSocket error: {}", e);
                break;
            }
            _ => {}
        }
    }

    info!("WebSocket connection closed for instance: {}", instance_id);
}

/// Handle a client message
async fn handle_client_message(
    socket: &mut axum::extract::ws::WebSocket,
    instance_id: &str,
    msg: WsClientMessage,
) -> anyhow::Result<()> {
    match msg {
        WsClientMessage::Message {
            id,
            content,
            session_id,
        } => {
            debug!("Processing message from client: {}", id);

            // Send ack
            send_message(
                socket,
                WsServerMessage::Ack {
                    request_id: id.clone(),
                },
            )
            .await?;

            // TODO: Process the message through the agentic loop
            // For now, send a placeholder response

            // Send delta
            send_message(
                socket,
                WsServerMessage::Delta {
                    text: format!("Echo: {content}"),
                },
            )
            .await?;

            // Send done
            send_message(
                socket,
                WsServerMessage::Done {
                    message_id: format!("msg_{}", uuid::Uuid::new_v4().simple()),
                    session_id: session_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                    turn_count: 1,
                    usage: TokenUsage::default(),
                },
            )
            .await?;

            Ok(())
        }
        WsClientMessage::Ping => send_message(socket, WsServerMessage::Pong).await,
        WsClientMessage::Close => {
            info!("Client requested close for instance: {}", instance_id);
            // Return an error to signal that the connection should be closed
            Err(anyhow::anyhow!("Client requested close"))
        }
    }
}

/// Send a server message to the WebSocket
async fn send_message(
    socket: &mut axum::extract::ws::WebSocket,
    msg: WsServerMessage,
) -> anyhow::Result<()> {
    let json = serde_json::to_string(&msg)?;
    socket.send(axum::extract::ws::Message::Text(json)).await?;
    Ok(())
}

/// Create router for WebSocket routes
pub fn router() -> Router<AppState> {
    Router::new().route("/agents/:id/ws", get(websocket_handler))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ws_client_message_deserialization() {
        let json = r#"{
            "type": "message",
            "id": "req_001",
            "content": "Hello",
            "session_id": "sess_123"
        }"#;
        let msg: WsClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            WsClientMessage::Message {
                id,
                content,
                session_id,
            } => {
                assert_eq!(id, "req_001");
                assert_eq!(content, "Hello");
                assert_eq!(session_id, Some("sess_123".to_string()));
            }
            _ => panic!("Expected Message variant"),
        }
    }

    #[test]
    fn test_ws_client_message_without_session() {
        let json = r#"{
            "type": "message",
            "id": "req_002",
            "content": "Test"
        }"#;
        let msg: WsClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            WsClientMessage::Message {
                id,
                content,
                session_id,
            } => {
                assert_eq!(id, "req_002");
                assert_eq!(content, "Test");
                assert_eq!(session_id, None);
            }
            _ => panic!("Expected Message variant"),
        }
    }

    #[test]
    fn test_ws_client_ping() {
        let json = r#"{"type": "ping"}"#;
        let msg: WsClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            WsClientMessage::Ping => {}
            _ => panic!("Expected Ping variant"),
        }
    }

    #[test]
    fn test_ws_client_close() {
        let json = r#"{"type": "close"}"#;
        let msg: WsClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            WsClientMessage::Close => {}
            _ => panic!("Expected Close variant"),
        }
    }

    #[test]
    fn test_ws_server_hello_serialization() {
        let msg = WsServerMessage::Hello {
            instance_id: "inst_123".to_string(),
            session_id: Some("sess_456".to_string()),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"hello\""));
        assert!(json.contains("\"instance_id\":\"inst_123\""));
        assert!(json.contains("\"session_id\":\"sess_456\""));
    }

    #[test]
    fn test_ws_server_hello_without_session() {
        let msg = WsServerMessage::Hello {
            instance_id: "inst_789".to_string(),
            session_id: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"hello\""));
        assert!(json.contains("\"instance_id\":\"inst_789\""));
        // session_id is serialized as null when None
        assert!(json.contains("\"session_id\":null"));
    }

    #[test]
    fn test_ws_server_ack_serialization() {
        let msg = WsServerMessage::Ack {
            request_id: "req_123".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"ack\""));
        assert!(json.contains("\"request_id\":\"req_123\""));
    }

    #[test]
    fn test_ws_server_delta_serialization() {
        let msg = WsServerMessage::Delta {
            text: "Hello world".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"delta\""));
        assert!(json.contains("\"text\":\"Hello world\""));
    }

    #[test]
    fn test_ws_server_tool_call_serialization() {
        let msg = WsServerMessage::ToolCall {
            id: "tc_001".to_string(),
            tool: "web_search".to_string(),
            args: serde_json::json!({"query": "rust"}),
            async_: false,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"tool_call\""));
        assert!(json.contains("\"id\":\"tc_001\""));
        assert!(json.contains("\"tool\":\"web_search\""));
        assert!(json.contains("\"async\""));
    }

    #[test]
    fn test_ws_server_tool_result_serialization() {
        let msg = WsServerMessage::ToolResult {
            tool_call_id: "tc_001".to_string(),
            output: "Search results".to_string(),
            error: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"tool_result\""));
        assert!(json.contains("\"tool_call_id\":\"tc_001\""));
        assert!(json.contains("\"output\":\"Search results\""));
        assert!(!json.contains("error"));
    }

    #[test]
    fn test_ws_server_tool_result_with_error() {
        let msg = WsServerMessage::ToolResult {
            tool_call_id: "tc_002".to_string(),
            output: "Error output".to_string(),
            error: Some("Tool failed".to_string()),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"error\":\"Tool failed\""));
    }

    #[test]
    fn test_ws_server_thinking_serialization() {
        let msg = WsServerMessage::Thinking {
            text: "Let me analyze...".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"thinking\""));
        assert!(json.contains("\"text\":\"Let me analyze...\""));
    }

    #[test]
    fn test_ws_server_done_serialization() {
        let msg = WsServerMessage::Done {
            message_id: "msg_001".to_string(),
            session_id: "sess_001".to_string(),
            turn_count: 3,
            usage: TokenUsage {
                input_tokens: 100,
                output_tokens: 50,
                total_tokens: 150,
            },
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"done\""));
        assert!(json.contains("\"message_id\":\"msg_001\""));
        assert!(json.contains("\"turn_count\":3"));
        assert!(json.contains("\"input_tokens\":100"));
        assert!(json.contains("\"output_tokens\":50"));
        assert!(json.contains("\"total_tokens\":150"));
    }

    #[test]
    fn test_ws_server_pong_serialization() {
        let msg = WsServerMessage::Pong;
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"pong\""));
    }

    #[test]
    fn test_ws_server_error_serialization() {
        let msg = WsServerMessage::Error {
            code: "invalid_request".to_string(),
            message: "Missing required field".to_string(),
            request_id: Some("req_123".to_string()),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"error\""));
        assert!(json.contains("\"code\":\"invalid_request\""));
        assert!(json.contains("\"message\":\"Missing required field\""));
        assert!(json.contains("\"request_id\":\"req_123\""));
    }

    #[test]
    fn test_ws_server_error_without_request_id() {
        let msg = WsServerMessage::Error {
            code: "internal_error".to_string(),
            message: "Server error".to_string(),
            request_id: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(!json.contains("request_id"));
    }

    #[test]
    fn test_token_usage_default() {
        let usage = TokenUsage::default();
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.output_tokens, 0);
        assert_eq!(usage.total_tokens, 0);
    }

    #[test]
    fn test_invalid_client_message() {
        let json = r#"{"type": "unknown_type"}"#;
        let result: Result<WsClientMessage, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_client_message_missing_required_field() {
        // Message type requires "id" and "content" fields
        let json = r#"{"type": "message", "id": "req_001"}"#;
        let result: Result<WsClientMessage, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }
}
