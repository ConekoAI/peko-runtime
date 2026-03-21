//! Message Service
//!
//! Provides unified message sending for both CLI and HTTP API.
//! Handles session management, agent execution, and response formatting.

use crate::agent::stateless_service::{ExecutionRequest, StatelessAgentService};
use crate::providers::TokenUsage;
use anyhow::Result;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;
use tracing::{error, info, instrument};
use uuid::Uuid;

/// Message sending request
#[derive(Debug, Clone)]
pub struct MessageRequest {
    /// Agent name
    pub agent_name: String,
    /// Team (optional)
    pub team: Option<String>,
    /// Message content
    pub message: String,
    /// Session ID (optional - creates new if not provided)
    pub session_id: Option<String>,
    /// Force new session
    pub new_session: bool,
    /// Timeout in seconds (optional)
    pub timeout_secs: Option<u64>,
}

impl MessageRequest {
    /// Create a new message request
    pub fn new(agent_name: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            agent_name: agent_name.into(),
            team: None,
            message: message.into(),
            session_id: None,
            new_session: false,
            timeout_secs: None,
        }
    }

    /// Set team
    pub fn with_team(mut self, team: impl Into<String>) -> Self {
        self.team = Some(team.into());
        self
    }

    /// Set session ID
    pub fn with_session(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    /// Set new session flag
    pub fn with_new_session(mut self, new: bool) -> Self {
        self.new_session = new;
        self
    }

    /// Set timeout
    pub fn with_timeout(mut self, secs: u64) -> Self {
        self.timeout_secs = Some(secs);
        self
    }
}

/// Tool call information in response
#[derive(Debug, Clone)]
pub struct ToolCallInfo {
    pub id: String,
    pub name: String,
    pub parameters: serde_json::Value,
    pub result: Option<String>,
}

/// Message sending result
#[derive(Debug, Clone)]
pub struct MessageResult {
    /// Response content
    pub content: String,
    /// Session ID used
    pub session_id: String,
    /// Whether this was a new session
    pub is_new_session: bool,
    /// Token usage
    pub usage: TokenUsage,
    /// Tool calls made
    pub tool_calls: Vec<ToolCallInfo>,
    /// Execution duration in milliseconds
    pub duration_ms: u64,
    /// Number of iterations
    pub iterations: usize,
    /// Whether execution succeeded
    pub success: bool,
    /// Error message (if failed)
    pub error: Option<String>,
}

/// Chat event for streaming responses
#[derive(Debug, Clone)]
pub enum ChatEvent {
    /// Content delta (streaming text)
    Delta { text: String },
    /// Tool call started
    ToolCall {
        id: String,
        name: String,
        args: serde_json::Value,
    },
    /// Tool call completed
    ToolResult {
        tool_call_id: String,
        output: String,
        error: Option<String>,
    },
    /// Execution completed
    Done {
        message_id: String,
        session_id: String,
        turn_count: u32,
        usage: TokenUsage,
    },
    /// Error occurred
    Error { code: String, message: String },
}

/// Unified message service
pub struct MessageService {
    agent_service: Arc<StatelessAgentService>,
}

impl MessageService {
    /// Create a new message service
    pub fn new(agent_service: Arc<StatelessAgentService>) -> Self {
        Self { agent_service }
    }

    /// Send a message and get a blocking response
    ///
    /// This is used by CLI and non-streaming API requests
    #[instrument(skip(self, request), fields(agent = %request.agent_name))]
    pub async fn send_message(&self, request: MessageRequest) -> Result<MessageResult> {
        let start = Instant::now();

        // Generate or use provided session ID
        let (session_id, is_new_session) = if request.new_session || request.session_id.is_none() {
            (generate_session_id(), true)
        } else {
            (request.session_id.unwrap(), false)
        };

        info!(
            "Sending message to agent '{}' (session: {}, new: {})",
            request.agent_name, session_id, is_new_session
        );

        // Build execution request
        let exec_request = ExecutionRequest {
            agent_name: request.agent_name.clone(),
            session_id: session_id.clone(),
            message: request.message.clone(),
            context: None,
            timeout_secs: request.timeout_secs,
        };

        // Execute via stateless service
        let exec_result = self.agent_service.execute(exec_request).await;

        let duration_ms = start.elapsed().as_millis() as u64;

        match exec_result {
            Ok(result) => {
                // Convert tool calls
                let tool_calls = result
                    .tool_calls
                    .into_iter()
                    .map(|tc| ToolCallInfo {
                        id: format!("tool_{}", Uuid::new_v4().simple()),
                        name: tc.name,
                        parameters: tc.parameters,
                        result: tc.result,
                    })
                    .collect();

                Ok(MessageResult {
                    content: result.response,
                    session_id,
                    is_new_session,
                    usage: result.usage,
                    tool_calls,
                    duration_ms,
                    iterations: result.iterations,
                    success: result.success,
                    error: result.error,
                })
            }
            Err(e) => {
                error!("Message sending failed: {}", e);
                Err(e)
            }
        }
    }

    /// Send a message with streaming response
    ///
    /// Returns a channel that receives events as they occur
    #[instrument(skip(self, request), fields(agent = %request.agent_name))]
    pub async fn send_message_streaming(
        &self,
        request: MessageRequest,
    ) -> Result<mpsc::Receiver<ChatEvent>> {
        // Generate or use provided session ID
        let (session_id, _is_new_session) = if request.new_session || request.session_id.is_none() {
            (generate_session_id(), true)
        } else {
            (request.session_id.unwrap(), false)
        };

        info!(
            "Starting streaming message to agent '{}' (session: {})",
            request.agent_name, session_id
        );

        // Create channel for events
        let (tx, rx) = mpsc::channel::<ChatEvent>(100);

        // Clone what we need for the spawned task
        let agent_service = self.agent_service.clone();
        let agent_name = request.agent_name.clone();
        let message = request.message.clone();
        let timeout_secs = request.timeout_secs;
        let session_id_clone = session_id.clone();

        // Spawn execution in background
        tokio::spawn(async move {
            // Send initial acknowledgment
            let _ = tx
                .send(ChatEvent::Delta {
                    text: "Processing your message...".to_string(),
                })
                .await;

            // Build execution request
            let exec_request = ExecutionRequest {
                agent_name: agent_name.clone(),
                session_id: session_id_clone.clone(),
                message: message.clone(),
                context: None,
                timeout_secs,
            };

            // Execute
            match agent_service.execute(exec_request).await {
                Ok(result) => {
                    // Send response content
                    if !result.response.is_empty() {
                        let _ = tx
                            .send(ChatEvent::Delta {
                                text: result.response,
                            })
                            .await;
                    }

                    // Send tool call events
                    for tc in &result.tool_calls {
                        let tool_id = format!("tool_{}", Uuid::new_v4().simple());

                        let _ = tx
                            .send(ChatEvent::ToolCall {
                                id: tool_id.clone(),
                                name: tc.name.clone(),
                                args: tc.parameters.clone(),
                            })
                            .await;

                        if let Some(ref output) = tc.result {
                            let _ = tx
                                .send(ChatEvent::ToolResult {
                                    tool_call_id: tool_id,
                                    output: output.clone(),
                                    error: None,
                                })
                                .await;
                        }
                    }

                    // Send completion event
                    let _ = tx
                        .send(ChatEvent::Done {
                            message_id: format!("msg_{}", Uuid::new_v4().simple()),
                            session_id: session_id_clone,
                            turn_count: result.iterations as u32,
                            usage: result.usage,
                        })
                        .await;
                }
                Err(e) => {
                    error!("Streaming execution failed: {}", e);
                    let _ = tx
                        .send(ChatEvent::Error {
                            code: "execution_error".to_string(),
                            message: format!("Execution failed: {}", e),
                        })
                        .await;
                }
            }
        });

        Ok(rx)
    }
}

/// Generate a unique session ID
pub fn generate_session_id() -> String {
    format!("sess_{}", Uuid::new_v4().simple())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_request_builder() {
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

    #[test]
    fn test_generate_session_id() {
        let id1 = generate_session_id();
        let id2 = generate_session_id();

        assert!(id1.starts_with("sess_"));
        assert!(id2.starts_with("sess_"));
        assert_ne!(id1, id2);
    }
}
