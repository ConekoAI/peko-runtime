//! Message Service
//!
//! Provides unified message sending for both CLI and HTTP API.
//! Handles session management, agent execution, and response formatting.
//!
//! This service now uses SessionResolver for consistent session resolution
//! across CLI and HTTP API interfaces.

use crate::agent::stateless_service::{ExecutionRequest, StatelessAgentService};
use crate::common::paths::PathResolver;
use crate::common::services::SessionResolver;
use crate::providers::TokenUsage;
use crate::session::types::ChannelType;
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

    /// Set session ID from Option (preserves None)
    pub fn with_session_opt(mut self, session_id: Option<String>) -> Self {
        self.session_id = session_id;
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
/// 
/// **DEPRECATED:** Use AgenticEvent from crate::engine instead.
/// This type is kept for backward compatibility but will be removed in a future version.
#[deprecated(since = "0.2.0", note = "Use AgenticEvent from crate::engine instead")]
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
///
/// Uses SessionResolver for consistent session resolution between CLI and API.
pub struct MessageService {
    agent_service: Arc<StatelessAgentService>,
    session_resolver: SessionResolver,
}

impl MessageService {
    /// Create a new message service
    pub fn new(agent_service: Arc<StatelessAgentService>, path_resolver: PathResolver) -> Self {
        let session_resolver = SessionResolver::new(path_resolver);
        Self {
            agent_service,
            session_resolver,
        }
    }

    /// Send a message and get a blocking response
    ///
    /// This is used by CLI and non-streaming API requests.
    /// Uses SessionResolver for consistent session resolution.
    #[instrument(skip(self, request), fields(agent = %request.agent_name))]
    pub async fn send_message(&self, request: MessageRequest) -> Result<MessageResult> {
        let start = Instant::now();

        // Use SessionResolver for consistent session resolution
        let team = request.team.as_deref();
        let channel = ChannelType::Http; // HTTP API uses Http channel
        let channel_id = "default";

        let (session_ctx, is_new_session) = self
            .session_resolver
            .resolve_session(
                &request.agent_name,
                team,
                channel,
                channel_id,
                request.session_id.clone(),
                request.new_session,
            )
            .await?;

        let session_id = {
            let base = session_ctx.hybrid.base.read().await;
            base.id.clone()
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

    /// Send a message with streaming response (legacy API)
    ///
    /// **DEPRECATED:** Use `send_message_unified` instead. 
    /// Returns a channel that receives events as they occur.
    #[deprecated(since = "0.2.0", note = "Use send_message_unified instead")]
    #[instrument(skip(self, request), fields(agent = %request.agent_name))]
    #[allow(deprecated)]
    pub async fn send_message_streaming(
        &self,
        request: MessageRequest,
    ) -> Result<mpsc::Receiver<ChatEvent>> {
        // Use SessionResolver for consistent session resolution
        let team = request.team.as_deref();
        let channel = ChannelType::Http; // HTTP API uses Http channel
        let channel_id = "default";

        let (session_ctx, _is_new_session) = self
            .session_resolver
            .resolve_session(
                &request.agent_name,
                team,
                channel,
                channel_id,
                request.session_id.clone(),
                request.new_session,
            )
            .await?;

        let session_id = {
            let base = session_ctx.hybrid.base.read().await;
            base.id.clone()
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

    /// Send a message and return an EventStream (ADR-015 unified architecture)
    ///
    /// This is the unified interface that always returns a stream of events.
    /// Channels consume this stream to produce appropriate output.
    ///
    /// # Example
    /// ```rust,ignore
    /// let event_stream = message_service.send_message_unified(request).await?;
    /// let output = channel.process_stream(event_stream).await?;
    /// ```
    #[instrument(skip(self, request), fields(agent = %request.agent_name))]
    pub async fn send_message_unified(
        &self,
        request: MessageRequest,
    ) -> Result<crate::channels::EventStream> {
        let start = Instant::now();

        // Use SessionResolver for consistent session resolution
        let team = request.team.as_deref();
        let channel_type = ChannelType::Http;
        let channel_id = "default";

        let (session_ctx, is_new_session) = self
            .session_resolver
            .resolve_session(
                &request.agent_name,
                team,
                channel_type,
                channel_id,
                request.session_id.clone(),
                request.new_session,
            )
            .await?;

        let session_id = {
            let base = session_ctx.hybrid.base.read().await;
            base.id.clone()
        };

        info!(
            "Sending unified message to agent '{}' (session: {}, new: {})",
            request.agent_name, session_id, is_new_session
        );

        // Create channel for AgenticEvents (not ChatEvent)
        let (tx, rx) = mpsc::channel::<crate::engine::AgenticEvent>(100);

        // Clone what we need for the spawned task
        let agent_service = self.agent_service.clone();
        let agent_name = request.agent_name.clone();
        let message = request.message.clone();
        let timeout_secs = request.timeout_secs;
        let session_id_clone = session_id.clone();

        // Spawn execution in background
        tokio::spawn(async move {
            // Build execution request
            let exec_request = ExecutionRequest {
                agent_name: agent_name.clone(),
                session_id: session_id_clone.clone(),
                message: message.clone(),
                context: None,
                timeout_secs,
            };

            // Use streaming execution to get real-time events
            match agent_service.execute_streaming(exec_request).await {
                Ok(mut event_rx) => {
                    // Forward all events from the streaming execution
                    while let Some(event) = event_rx.recv().await {
                        let is_end = matches!(
                            event,
                            crate::engine::AgenticEvent::Lifecycle {
                                phase: crate::engine::LifecyclePhase::End,
                                ..
                            } | crate::engine::AgenticEvent::Lifecycle {
                                phase: crate::engine::LifecyclePhase::Error,
                                ..
                            }
                        );
                        
                        if tx.send(event).await.is_err() {
                            break;
                        }
                        
                        if is_end {
                            break;
                        }
                    }
                }
                Err(e) => {
                    error!("Streaming execution failed: {}", e);
                    let _ = tx
                        .send(crate::engine::AgenticEvent::Lifecycle {
                            run_id: format!("run_{}", Uuid::new_v4()),
                            phase: crate::engine::LifecyclePhase::Error,
                            error: Some(format!("Execution failed: {}", e)),
                        })
                        .await;
                }
            }
        });

        let duration_ms = start.elapsed().as_millis() as u64;
        info!(
            "Unified message setup complete for '{}' in {}ms",
            request.agent_name, duration_ms
        );

        Ok(crate::channels::EventStream {
            receiver: rx,
            session_id,
            is_new_session,
        })
    }
}

/// Generate a unique session ID
///
/// Uses standard UUID format to match CLI session generation.
/// This ensures consistency between HTTP API and CLI interfaces.
pub fn generate_session_id() -> String {
    Uuid::new_v4().to_string()
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

        // Should be valid UUID format (36 characters with hyphens)
        assert_eq!(id1.len(), 36);
        assert_eq!(id2.len(), 36);
        // Should contain hyphens at expected positions
        assert_eq!(id1.chars().nth(8), Some('-'));
        assert_eq!(id1.chars().nth(13), Some('-'));
        assert_eq!(id1.chars().nth(18), Some('-'));
        assert_eq!(id1.chars().nth(23), Some('-'));
        // Should be unique
        assert_ne!(id1, id2);
    }
}
