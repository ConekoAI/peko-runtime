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
use crate::providers::{ChatMessage, MessageRole, TokenUsage};
use crate::session::events::SessionEvent;

use crate::session::jsonl::SessionStorage;
use crate::session::manager::SessionManager;
use crate::session::types::Peer;
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

        debug!(
            "Loaded config for '{}' in {:?}",
            request.agent_name,
            start.elapsed()
        );

        // 2. Load session history
        let history = self
            .load_session_history(&request.agent_name, &request.session_id)
            .await?;
        debug!("Loaded {} messages from session history", history.len());

        // 3. Get or create session via SessionManager
        let sessions_dir = get_agent_session_dir(
            &self.config_service,
            &self.path_resolver,
            &request.agent_name,
        )
        .await?;
        let mut session_manager = SessionManager::new().with_directory(sessions_dir.clone());
        let peer = Peer::User("default".to_string());
        let session = session_manager
            .get_or_create_base(&request.agent_name, &peer)
            .await?;

        // CRITICAL: Update the session ID to match the requested session
        // This ensures we're writing to the correct session file
        {
            let mut base = session.write().await;
            if base.id != request.session_id {
                debug!("Session ID mismatch: base has '{}', request is for '{}'. Using request session ID.", 
                       base.id, request.session_id);
                // Note: The session file is keyed by ID, so we need to ensure
                // we're writing to the right file. For now, we update the base id.
                base.id = request.session_id.clone();
            }
        }

        // 4. Cold-start agent (spawn)
        let agent_start = Instant::now();
        let agent = Agent::new(config_entry.config.clone())
            .await
            .with_context(|| format!("Failed to create agent: {}", request.agent_name))?;
        let cold_start_ms = agent_start.elapsed().as_millis() as u64;

        debug!(
            "Cold-started agent '{}' in {}ms",
            request.agent_name, cold_start_ms
        );

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

    /// Execute streaming - returns event receiver
    #[instrument(skip(self, request), fields(agent = %request.agent_name))]
    pub async fn execute_streaming(
        &self,
        request: ExecutionRequest,
    ) -> Result<tokio::sync::mpsc::Receiver<AgenticEvent>> {
        // Load config
        let config_entry = self
            .config_service
            .get(&request.agent_name, None)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Agent not found: {}", request.agent_name))?;

        // Load history
        let history = self
            .load_session_history(&request.agent_name, &request.session_id)
            .await?;

        // Cold-start agent
        let agent = Agent::new(config_entry.config.clone())
            .await
            .with_context(|| format!("Failed to create agent: {}", request.agent_name))?;

        // Create session via SessionManager
        let mut session_manager = SessionManager::new()
            .with_registry(&request.agent_name)
            .await?;
        let peer = Peer::User("default".to_string());
        let session = session_manager
            .get_or_create_base(&request.agent_name, &peer)
            .await?;

        // Start streaming execution
        let prompt = self.build_prompt(&request.message, &history)?;
        let event_rx = agent
            .execute_streaming_with_session(&prompt, session, Some(history))
            .await?;

        Ok(event_rx)
    }

    /// Get current metrics
    pub async fn metrics(&self) -> ExecutionMetrics {
        self.metrics.read().await.clone()
    }

    /// Load session history from storage
    async fn load_session_history(
        &self,
        agent_name: &str,
        session_id: &str,
    ) -> Result<Vec<ChatMessage>> {
        let sessions_dir =
            get_agent_session_dir(&self.config_service, &self.path_resolver, agent_name).await?;

        // Use standard SessionStorage (reads {session_id}.jsonl)
        let storage = SessionStorage::new(sessions_dir);

        // Load events from storage
        let events = storage.load_events(session_id).await?;
        let mut messages = Vec::new();

        for event in events {
            match event {
                SessionEvent::UserMessage(msg) => {
                    messages.push(ChatMessage {
                        role: MessageRole::User,
                        content: vec![ContentBlock::Text { text: msg.content }],
                        tool_calls: None,
                        tool_call_id: None,
                    });
                }
                SessionEvent::AssistantMessage(msg) => {
                    messages.push(ChatMessage {
                        role: MessageRole::Assistant,
                        content: vec![ContentBlock::Text { text: msg.content }],
                        tool_calls: None,
                        tool_call_id: None,
                    });
                }
                // Skip other event types for history loading
                _ => {}
            }
        }

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
}
