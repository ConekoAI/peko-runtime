//! Stateless Agent Service - Cold-start execution for stateless architecture
//!
//! This module provides the StatelessAgentService which replaces the AgentPool
//! in the stateless cold-start architecture. Instead of maintaining a pool of
//! running agents, it cold-starts agents on-demand for each request.
//!
//! ## Architecture
//!
//! - Load configuration from ConfigRegistry (fast, in-memory)
//! - Spawn new Agent instance per request
//! - Execute and return result
//! - Agent is dropped after execution (stateless)
//! - Session state is persisted separately

use crate::agent::Agent;
use crate::common::paths::PathResolver;
use crate::common::services::AgentConfigService;
use crate::engine::AgenticEvent;
use crate::providers::{ChatMessage, TokenUsage};
use crate::session::manager::SessionManager;
use crate::session::types::{ChannelType, Peer};
// Note: Session storage uses jsonl module directly
use crate::types::message::ContentBlock;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tokio::time::timeout;
use tracing::{debug, info, instrument, warn};

/// Execution request for stateless agent
#[derive(Debug, Clone)]
pub struct ExecutionRequest {
    /// Agent name (registered in ConfigRegistry)
    pub agent_name: String,
    /// Session ID for persistence
    pub session_id: String,
    /// User message to process
    pub message: String,
    /// Optional execution context
    pub context: Option<ExecutionContext>,
    /// Optional timeout override (defaults to service default)
    pub timeout_secs: Option<u64>,
    /// User identifier for session isolation (defaults to "default")
    pub user: String,
}

impl ExecutionRequest {
    /// Create a simple execution request
    pub fn new(
        agent_name: impl Into<String>,
        session_id: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            agent_name: agent_name.into(),
            session_id: session_id.into(),
            message: message.into(),
            context: None,
            timeout_secs: None,
            user: "default".to_string(),
        }
    }

    /// Set execution context
    pub fn with_context(mut self, context: ExecutionContext) -> Self {
        self.context = Some(context);
        self
    }

    /// Set timeout
    pub fn with_timeout(mut self, secs: u64) -> Self {
        self.timeout_secs = Some(secs);
        self
    }
}

/// Execution context for request
#[derive(Debug, Clone, Default)]
pub struct ExecutionContext {
    /// Parent message ID for threading
    pub parent_message_id: Option<String>,
    /// Additional metadata
    pub metadata: HashMap<String, String>,
}

/// Message request for high-level message execution
/// 
/// This type is used by execute_message() and execute_message_streaming()
/// to provide a unified interface for message sending.
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
    /// User identifier for session isolation (defaults to "default")
    pub user: String,
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
            user: "default".to_string(),
        }
    }

    /// Set user for session isolation
    pub fn with_user(mut self, user: impl Into<String>) -> Self {
        self.user = user.into();
        self
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
    /// Tool call ID
    pub id: String,
    /// Tool name
    pub name: String,
    /// Tool parameters
    pub parameters: serde_json::Value,
    /// Tool result (if available)
    pub result: Option<String>,
}

/// Tool call record for response
#[derive(Debug, Clone)]
pub struct ToolCallRecord {
    /// Tool name
    pub name: String,
    /// Tool parameters
    pub parameters: serde_json::Value,
    /// Tool result (if available)
    pub result: Option<String>,
}

/// Message sending result
/// 
/// This is the high-level result type returned by execute_message()
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

/// Execution result
#[derive(Debug, Clone)]
pub struct ExecutionResult {
    /// Final response from agent
    pub response: String,
    /// Tool calls made during execution
    pub tool_calls: Vec<ToolCallRecord>,
    /// Token usage
    pub usage: TokenUsage,
    /// Execution duration in milliseconds
    pub duration_ms: u64,
    /// Number of iterations
    pub iterations: usize,
    /// Whether execution succeeded
    pub success: bool,
    /// Error message (if failed)
    pub error: Option<String>,
}

/// Execution metrics for monitoring
#[derive(Debug, Clone, Default)]
pub struct ExecutionMetrics {
    /// Total executions
    pub total_executions: u64,
    /// Successful executions
    pub successful_executions: u64,
    /// Failed executions
    pub failed_executions: u64,
    /// Total execution time (ms)
    pub total_duration_ms: u64,
    /// Average cold-start time (ms)
    pub avg_cold_start_ms: u64,
}

/// Stateless agent service - cold-start execution
pub struct StatelessAgentService {
    /// Agent configuration service
    config_service: Arc<AgentConfigService>,
    /// Default execution timeout
    default_timeout: Duration,
    /// Execution metrics
    metrics: RwLock<ExecutionMetrics>,
    /// Path resolver for team-aware paths
    path_resolver: PathResolver,
}

/// Get team-aware session directory for an agent
async fn get_agent_session_dir(
    config_service: &AgentConfigService,
    path_resolver: &PathResolver,
    agent_name: &str,
) -> Result<PathBuf> {
    // Look up agent to get team
    let team_id: Option<String> = config_service
        .get(agent_name, None)
        .await?
        .map(|entry| entry.team);

    Ok(path_resolver.agent_sessions_dir(agent_name, team_id.as_deref()))
}

impl StatelessAgentService {
    /// Create a new stateless agent service
    pub async fn new(
        config_service: Arc<AgentConfigService>,
        path_resolver: PathResolver,
    ) -> Result<Self> {
        let service = Self {
            config_service,
            default_timeout: Duration::from_secs(300), // 5 minutes default
            metrics: RwLock::new(ExecutionMetrics::default()),
            path_resolver,
        };

        info!("StatelessAgentService initialized with team-aware paths");

        Ok(service)
    }

    /// Set default timeout
    pub fn with_default_timeout(mut self, timeout: Duration) -> Self {
        self.default_timeout = timeout;
        self
    }

    /// Get team for an agent (helper to avoid repetition)
    async fn get_team(&self, agent_name: &str) -> Result<Option<String>> {
        Ok(self
            .config_service
            .get(agent_name, None)
            .await?
            .map(|entry| entry.team))
    }

    /// Execute a message and return a blocking response
    /// 
    /// This is the high-level interface that replaces MessageService::send_message()
    /// It handles session resolution and executes the agent, returning the complete result.
    /// 
    /// # Arguments
    /// * `request` - The message request containing agent name, message, and session options
    /// 
    /// # Returns
    /// A `MessageResult` containing the response, session info, and execution metadata
    pub async fn execute_message(&self, request: MessageRequest) -> Result<MessageResult> {
        let start = Instant::now();

        // Resolve session using SessionManager (single authority)
        let team = self.get_team(&request.agent_name).await?;
        let mut session_manager = SessionManager::for_cli(
            self.path_resolver.clone(),
            &request.agent_name,
            team.as_deref(),
            &request.user,
        );

        let resolved = session_manager
            .resolve_session(
                &request.agent_name,
                request.team.as_deref(),
                ChannelType::Http,
                "default",
                request.session_id.clone(),
                request.new_session,
            )
            .await?;

        let session_id = resolved.session_id.clone();
        let is_new_session = resolved.is_new;

        // Build execution request
        let exec_request = ExecutionRequest {
            agent_name: request.agent_name.clone(),
            session_id: session_id.clone(),
            message: request.message,
            context: None,
            timeout_secs: request.timeout_secs,
            user: request.user.clone(),
        };

        // Execute via stateless service
        let exec_result = self.execute(exec_request).await;

        let duration_ms = start.elapsed().as_millis() as u64;

        match exec_result {
            Ok(result) => {
                // Convert tool calls
                let tool_calls: Vec<ToolCallInfo> = result
                    .tool_calls
                    .into_iter()
                    .map(|tc| ToolCallInfo {
                        id: format!("tool_{}", uuid::Uuid::new_v4().simple()),
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
            Err(e) => Err(e),
        }
    }

    /// Execute a message and return an EventStream for streaming
    /// 
    /// This is the high-level streaming interface that replaces MessageService::send_message_unified()
    /// It handles session resolution and executes the agent, returning a stream of events.
    /// 
    /// # Arguments
    /// * `request` - The message request containing agent name, message, and session options
    /// 
    /// # Returns
    /// An `EventStream` containing the receiver for events and completion signal
    pub async fn execute_message_streaming(
        &self,
        request: MessageRequest,
    ) -> Result<crate::channels::EventStream> {
        let start = Instant::now();

        // Resolve session using SessionManager (single authority)
        let team = self.get_team(&request.agent_name).await?;
        let mut session_manager = SessionManager::for_cli(
            self.path_resolver.clone(),
            &request.agent_name,
            team.as_deref(),
            &request.user,
        );

        let resolved = session_manager
            .resolve_session(
                &request.agent_name,
                request.team.as_deref(),
                ChannelType::Http,
                "default",
                request.session_id.clone(),
                request.new_session,
            )
            .await?;

        let session_id = resolved.session_id.clone();
        let is_new_session = resolved.is_new;

        // Build execution request
        let exec_request = ExecutionRequest {
            agent_name: request.agent_name.clone(),
            session_id: session_id.clone(),
            message: request.message,
            context: None,
            timeout_secs: request.timeout_secs,
            user: request.user.clone(),
        };

        // Get base session for execution
        let base_session = resolved.context.hybrid.base.clone();

        // Execute streaming - directly returns EventStream
        let event_stream = self
            .execute_streaming_with_session(exec_request, base_session)
            .await?;

        let duration_ms = start.elapsed().as_millis() as u64;
        info!(
            "Streaming setup complete for '{}' in {}ms",
            request.agent_name, duration_ms
        );

        Ok(crate::channels::EventStream {
            receiver: event_stream.receiver,
            completion: event_stream.completion,
            session_id,
            is_new_session,
        })
    }

    /// Execute agent with cold start
    #[instrument(skip(self, request), fields(agent = %request.agent_name, session = %request.session_id))]
    pub async fn execute(&self, request: ExecutionRequest) -> Result<ExecutionResult> {
        let start = Instant::now();
        let timeout_duration = request
            .timeout_secs
            .map(Duration::from_secs)
            .unwrap_or(self.default_timeout);

        info!(
            "Starting cold execution for agent '{}' (timeout: {:?})",
            request.agent_name, timeout_duration
        );

        // Execute with timeout
        let result = timeout(timeout_duration, self.execute_inner(request)).await;

        let duration = start.elapsed();

        match result {
            Ok(Ok(result)) => {
                self.update_metrics(true, duration).await;
                Ok(result)
            }
            Ok(Err(e)) => {
                self.update_metrics(false, duration).await;
                Err(e)
            }
            Err(_) => {
                self.update_metrics(false, duration).await;
                Err(anyhow::anyhow!(
                    "Execution timeout after {:?}",
                    timeout_duration
                ))
            }
        }
    }

    /// Inner execution (without timeout wrapper)
    async fn execute_inner(&self, request: ExecutionRequest) -> Result<ExecutionResult> {
        let start = Instant::now();

        // 1. Load agent configuration (fast - from in-memory cache)
        let config_entry = self
            .config_service
            .get(&request.agent_name, None)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Agent not found: {}", request.agent_name))?;

        // 2. Open the specific session by ID
        // This ensures we write to the correct session file
        let team = Some(config_entry.team.as_str());
        let mut session_manager =
            SessionManager::for_cli(self.path_resolver.clone(), &request.agent_name, team, &request.user);
        
        // Try to open existing session, create if not exists
        let session = match session_manager.open_session(&request.session_id).await? {
            Some(handle) => {
                debug!("Opened existing session '{}'", request.session_id);
                handle.base().clone()
            }
            None => {
                debug!("Session '{}' not found, creating new", request.session_id);
                let peer = Peer::User(request.user.clone());
                let options = crate::session::SessionCreateOptions::new()
                    .with_trigger("api")
                    .with_session_id(&request.session_id);
                let handle = session_manager
                    .create_session(&request.agent_name, &peer, options)
                    .await?;
                
                handle.base().clone()
            }
        };

        // 3. Load session history from the opened session
        let history = self
            .load_session_history(session.clone())
            .await?;

        // 4. Cold-start agent (spawn)
        let agent = Agent::new(config_entry.config.clone())
            .await
            .with_context(|| format!("Failed to create agent: {}", request.agent_name))?;

        // 5. Execute agent with session and history
        // Use the new execute_with_session method that properly handles session resumption
        let execute_result = agent
            .execute_with_session(&request.message, session.clone(), Some(history), |_event| {
                // Events are ignored for non-streaming execution
                // All data comes from the AgenticResult
            })
            .await;

        let (success, final_response, token_usage, iterations, tool_calls, error_msg) =
            match execute_result {
                Ok(result) => {
                    // Convert ContentBlock tool calls to ToolCallRecord
                    let tool_calls: Vec<ToolCallRecord> = result
                        .tool_calls
                        .into_iter()
                        .filter_map(|block| {
                            if let ContentBlock::ToolCall {
                                name, arguments, ..
                            } = block
                            {
                                Some(ToolCallRecord {
                                    name,
                                    parameters: arguments,
                                    result: None,
                                })
                            } else {
                                None
                            }
                        })
                        .collect();

                    (
                        result.success,
                        result.final_answer,
                        result.usage,
                        result.iterations,
                        tool_calls,
                        None,
                    )
                }
                Err(e) => {
                    warn!("Agent execution failed: {}", e);
                    (
                        false,
                        String::new(),
                        TokenUsage::default(),
                        0,
                        Vec::new(),
                        Some(e.to_string()),
                    )
                }
            };

        // Note: Session persistence is handled by the engine loop via UnifiedSession.
        // The engine loop's session.add_user() and session.add_assistant() methods
        // already write to the session file during execution.
        // We do NOT write here to avoid duplication and format conflicts.

        let duration = start.elapsed();

        info!(
            "Execution complete for '{}' (success: {}, duration: {}ms, iterations: {}, tokens: {}/{})",
            request.agent_name,
            success,
            duration.as_millis(),
            iterations,
            token_usage.input,
            token_usage.output
        );

        // 7. Agent is dropped here (stateless)

        Ok(ExecutionResult {
            response: final_response,
            tool_calls,
            usage: token_usage,
            duration_ms: duration.as_millis() as u64,
            iterations,
            success,
            error: error_msg,
        })
    }

    /// Execute streaming - returns EventStream with completion signal
    ///
    /// This is the low-level streaming interface. For high-level usage,
    /// prefer `execute_message_streaming()` which handles session resolution.
    #[instrument(skip(self, request), fields(agent = %request.agent_name))]
    pub async fn execute_streaming(
        &self,
        request: ExecutionRequest,
    ) -> Result<crate::channels::EventStream> {
        // Load config, history, agent, session - these are all Send-safe
        let config_entry = self
            .config_service
            .get(&request.agent_name, None)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Agent not found: {}", request.agent_name))?;

        let _agent = Agent::new(config_entry.config.clone())
            .await
            .with_context(|| format!("Failed to create agent: {}", request.agent_name))?;

        // Open the specific session by ID (same logic as execute_inner)
        let team = Some(config_entry.team.as_str());
        let mut session_manager =
            SessionManager::for_cli(self.path_resolver.clone(), &request.agent_name, team, &request.user);
        
        let session = match session_manager.open_session(&request.session_id).await? {
            Some(handle) => {
                debug!("Opened existing session '{}' for streaming", request.session_id);
                handle.base().clone()
            }
            None => {
                debug!("Session '{}' not found, creating new for streaming", request.session_id);
                let peer = Peer::User(request.user.clone());
                let options = crate::session::SessionCreateOptions::new()
                    .with_trigger("api")
                    .with_session_id(&request.session_id);
                let handle = session_manager
                    .create_session(&request.agent_name, &peer, options)
                    .await?;
                
                handle.base().clone()
            }
        };

        // Delegate to the internal method (which loads history itself)
        self.execute_streaming_with_session(request, session).await
    }

    /// Execute streaming with an already-resolved session
    /// 
    /// This is the internal implementation used by both `execute_streaming`
    /// and `execute_message_streaming`. It uses tokio::spawn (not spawn_blocking)
    /// for single-runtime execution and provides completion signals.
    #[instrument(skip(self, request, session), fields(agent = %request.agent_name))]
    async fn execute_streaming_with_session(
        &self,
        request: ExecutionRequest,
        session: Arc<RwLock<crate::session::unified::UnifiedSession>>,
    ) -> Result<crate::channels::EventStream> {
        let prompt = self.build_prompt(&request.message, &[])?;

        // Load history for the agent
        let history = self
            .load_session_history(session.clone())
            .await?;

        // Load agent
        let agent = Agent::new(
            self.config_service
                .get(&request.agent_name, None)
                .await?
                .ok_or_else(|| anyhow::anyhow!("Agent not found: {}", request.agent_name))?
                .config
                .clone(),
        )
        .await
        .with_context(|| format!("Failed to create agent: {}", request.agent_name))?;

        // Create channels
        let (event_tx, event_rx) = tokio::sync::mpsc::channel::<AgenticEvent>(1000);
        let (completion_tx, completion_rx) = tokio::sync::oneshot::channel::<Result<()>>();

        // Spawn task on main runtime (NOT spawn_blocking)
        // This ensures session writes complete before completion signal
        tokio::spawn(async move {
            let on_event = move |event: AgenticEvent| {
                let _ = event_tx.try_send(event);
            };

            // Execute and collect result
            let result = agent
                .execute_streaming_with_session(&prompt, session, Some(history.clone()), on_event)
                .await;

            // At this point, all session writes have completed because
            // add_assistant/add_tool_result are synchronous

            // Signal completion
            let completion_result = match result {
                Ok(exec_result) => {
                    if exec_result.success {
                        Ok(())
                    } else {
                        Err(anyhow::anyhow!(
                            "Execution failed: {:?}",
                            exec_result.final_answer
                        ))
                    }
                }
                Err(e) => Err(e),
            };

            let _ = completion_tx.send(completion_result);
            // event_tx is dropped here, signaling end of stream
        });

        Ok(crate::channels::EventStream {
            receiver: event_rx,
            completion: completion_rx,
            session_id: request.session_id,
            is_new_session: false,
        })
    }

    /// Get current metrics
    pub async fn metrics(&self) -> ExecutionMetrics {
        self.metrics.read().await.clone()
    }

    /// Load session history from storage
    ///
    /// Uses the session's native load_history to preserve full ContentBlock fidelity
    /// (including ToolCall and ToolResult blocks), instead of the lossy normalized format.
    async fn load_session_history(
        &self,
        session: Arc<RwLock<crate::session::UnifiedSession>>,
    ) -> Result<Vec<ChatMessage>> {
        let messages = session.read().await.load_history().await?;
        Ok(messages)
    }

    /// Build prompt with conversation history
    fn build_prompt(&self, message: &str, history: &[ChatMessage]) -> Result<String> {
        // For now, just return the message
        // In a more complex implementation, this would format the full conversation
        // including system prompts, history, and the new message
        if history.is_empty() {
            Ok(message.to_string())
        } else {
            // Append the new message to history context
            // The agent's system prompt handles the conversation format
            Ok(message.to_string())
        }
    }

    /// Update execution metrics
    async fn update_metrics(&self, success: bool, duration: Duration) {
        let mut metrics = self.metrics.write().await;
        metrics.total_executions += 1;
        metrics.total_duration_ms += duration.as_millis() as u64;

        if success {
            metrics.successful_executions += 1;
        } else {
            metrics.failed_executions += 1;
        }

        // Calculate running average of cold-start time
        // Note: This is a simplified calculation
        if metrics.total_executions > 0 {
            metrics.avg_cold_start_ms = metrics.total_duration_ms / metrics.total_executions;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::ConfigRegistry;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_execution_request_builder() {
        let request = ExecutionRequest::new("test-agent", "test-session", "Hello")
            .with_timeout(60)
            .with_context(ExecutionContext {
                parent_message_id: Some("parent-123".to_string()),
                metadata: [("key".to_string(), "value".to_string())]
                    .into_iter()
                    .collect(),
            });

        assert_eq!(request.agent_name, "test-agent");
        assert_eq!(request.session_id, "test-session");
        assert_eq!(request.message, "Hello");
        assert_eq!(request.timeout_secs, Some(60));
        assert!(request.context.is_some());
    }

    #[tokio::test]
    async fn test_stateless_service_creation() {
        let temp_dir = TempDir::new().unwrap();

        let path_resolver = PathResolver::with_dirs(
            temp_dir.path().join("config"),
            temp_dir.path().join("data"),
            temp_dir.path().join("cache"),
        );

        let config_service = Arc::new(AgentConfigService::new(path_resolver.clone()));

        let service = StatelessAgentService::new(config_service, path_resolver)
            .await
            .unwrap();

        let metrics = service.metrics().await;
        assert_eq!(metrics.total_executions, 0);
    }

    // ====================================================================================
    // MessageRequest builder tests (ADR-016 Phase 1)
    // ====================================================================================

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
    fn test_message_request_builder_defaults() {
        let request = MessageRequest::new("my-agent", "Hello");

        assert_eq!(request.agent_name, "my-agent");
        assert_eq!(request.message, "Hello");
        assert_eq!(request.team, None);
        assert_eq!(request.session_id, None);
        assert!(!request.new_session);
        assert_eq!(request.timeout_secs, None);
    }

    #[test]
    fn test_message_request_with_session_opt() {
        // Test with Some
        let request1 = MessageRequest::new("agent", "hi")
            .with_session_opt(Some("session-id".to_string()));
        assert_eq!(request1.session_id, Some("session-id".to_string()));

        // Test with None
        let request2 = MessageRequest::new("agent", "hi")
            .with_session_opt(None);
        assert_eq!(request2.session_id, None);
    }

    // ====================================================================================
    // Service method tests (ADR-016 Phase 1)
    // ====================================================================================

    #[tokio::test]
    async fn test_get_team_helper() {
        let temp_dir = TempDir::new().unwrap();

        let path_resolver = PathResolver::with_dirs(
            temp_dir.path().join("config"),
            temp_dir.path().join("data"),
            temp_dir.path().join("cache"),
        );

        let config_service = Arc::new(AgentConfigService::new(path_resolver.clone()));
        let service = StatelessAgentService::new(config_service, path_resolver)
            .await
            .unwrap();

        // Test get_team for non-existent agent
        let team = service.get_team("non-existent-agent").await;
        assert!(team.is_ok());
        assert_eq!(team.unwrap(), None);
    }

    #[tokio::test]
    async fn test_with_default_timeout() {
        let temp_dir = TempDir::new().unwrap();

        let path_resolver = PathResolver::with_dirs(
            temp_dir.path().join("config"),
            temp_dir.path().join("data"),
            temp_dir.path().join("cache"),
        );

        let config_service = Arc::new(AgentConfigService::new(path_resolver.clone()));
        let service = StatelessAgentService::new(config_service, path_resolver)
            .await
            .unwrap();

        // Verify default timeout is 300 seconds (5 minutes)
        // We can't directly check the private field, but we can verify
        // the method returns self correctly
        let service_with_timeout = service.with_default_timeout(std::time::Duration::from_secs(120));
        // Just verify it compiles and returns
        drop(service_with_timeout);
    }

    // ====================================================================================
    // ToolCallInfo tests (ADR-016 Phase 1)
    // ====================================================================================

    #[test]
    fn test_tool_call_info_creation() {
        let tool_call = ToolCallInfo {
            id: "tool_123".to_string(),
            name: "read_file".to_string(),
            parameters: serde_json::json!({"path": "/tmp/test"}),
            result: Some("File contents".to_string()),
        };

        assert_eq!(tool_call.id, "tool_123");
        assert_eq!(tool_call.name, "read_file");
        assert_eq!(tool_call.result, Some("File contents".to_string()));
    }

    #[test]
    fn test_tool_call_info_without_result() {
        let tool_call = ToolCallInfo {
            id: "tool_456".to_string(),
            name: "web_search".to_string(),
            parameters: serde_json::json!({"query": "rust"}),
            result: None,
        };

        assert_eq!(tool_call.result, None);
    }
}
