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

use crate::agent::config_registry::ConfigRegistry;
use crate::agent::Agent;
use crate::common::paths::PathResolver;
use crate::engine::AgenticEvent;
use crate::providers::{ChatMessage, MessageRole, TokenUsage};
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
use uuid::Uuid;

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
    /// Configuration registry
    config_registry: Arc<ConfigRegistry>,
    /// Default execution timeout
    default_timeout: Duration,
    /// Execution metrics
    metrics: RwLock<ExecutionMetrics>,
    /// Path resolver for team-aware paths
    path_resolver: PathResolver,
}

/// Get team-aware session directory for an agent
async fn get_agent_session_dir(
    config_registry: &ConfigRegistry,
    path_resolver: &PathResolver,
    agent_name: &str,
) -> Result<PathBuf> {
    // Look up agent to get team
    let team_id: Option<String> = config_registry
        .get(agent_name)
        .await
        .and_then(|entry| entry.team_id.clone());

    Ok(path_resolver.agent_sessions_dir(agent_name, team_id.as_deref()))
}

impl StatelessAgentService {
    /// Create a new stateless agent service
    pub async fn new(
        config_registry: Arc<ConfigRegistry>,
        path_resolver: PathResolver,
    ) -> Result<Self> {
        let service = Self {
            config_registry,
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
            .config_registry
            .get(&request.agent_name)
            .await
            .ok_or_else(|| anyhow::anyhow!("Agent not registered: {}", request.agent_name))?;

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

        // 3. Cold-start agent (spawn)
        let agent_start = Instant::now();
        let agent = Agent::new(config_entry.config.clone())
            .await
            .with_context(|| format!("Failed to create agent: {}", request.agent_name))?;
        let cold_start_ms = agent_start.elapsed().as_millis() as u64;

        debug!(
            "Cold-started agent '{}' in {}ms",
            request.agent_name, cold_start_ms
        );

        // 4. Build full prompt with history
        let prompt = self.build_prompt(&request.message, &history)?;

        // 5. Execute agent (ignore events - use result)
        let execute_result = agent
            .execute(&prompt, |_event| {
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

        // 6. Save to session
        if success {
            if let Err(e) = self
                .save_to_session(
                    &request.agent_name,
                    &request.session_id,
                    &request.message,
                    &final_response,
                    &tool_calls,
                )
                .await
            {
                warn!("Failed to save to session: {}", e);
            }
        }

        let duration = start.elapsed();

        info!(
            "Execution complete for '{}' (success: {}, duration: {}ms, iterations: {})",
            request.agent_name,
            success,
            duration.as_millis(),
            iterations
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
            .config_registry
            .get(&request.agent_name)
            .await
            .ok_or_else(|| anyhow::anyhow!("Agent not registered: {}", request.agent_name))?;

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
            get_agent_session_dir(&self.config_registry, &self.path_resolver, agent_name).await?;
        let session_path = sessions_dir.join(session_id);
        let jsonl_path = session_path.join("messages.jsonl");

        if !jsonl_path.exists() {
            return Ok(Vec::new());
        }

        // Read messages from JSONL
        let content = tokio::fs::read_to_string(&jsonl_path).await?;
        let mut messages = Vec::new();

        for line in content.lines().filter(|l| !l.is_empty()) {
            match serde_json::from_str::<JsonlMessage>(line) {
                Ok(msg) => {
                    let role = match msg.role.as_str() {
                        "system" => MessageRole::System,
                        "user" => MessageRole::User,
                        "assistant" => MessageRole::Assistant,
                        _ => MessageRole::User,
                    };

                    messages.push(ChatMessage {
                        role,
                        content: vec![ContentBlock::Text { text: msg.content }],
                        tool_calls: None,
                        tool_call_id: None,
                    });
                }
                Err(e) => {
                    warn!("Failed to parse message line: {}", e);
                }
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

    /// Save message exchange to session
    async fn save_to_session(
        &self,
        agent_name: &str,
        session_id: &str,
        user_message: &str,
        assistant_response: &str,
        _tool_calls: &[ToolCallRecord],
    ) -> Result<()> {
        let sessions_dir =
            get_agent_session_dir(&self.config_registry, &self.path_resolver, agent_name).await?;
        let session_path = sessions_dir.join(session_id);
        tokio::fs::create_dir_all(&session_path).await?;

        let jsonl_path = session_path.join("messages.jsonl");

        // Append user message
        let user_line = serde_json::to_string(&JsonlMessage {
            id: Uuid::new_v4().to_string(),
            role: "user".to_string(),
            content: user_message.to_string(),
            timestamp: chrono::Utc::now(),
        })?;

        // Append assistant response
        let assistant_line = serde_json::to_string(&JsonlMessage {
            id: Uuid::new_v4().to_string(),
            role: "assistant".to_string(),
            content: assistant_response.to_string(),
            timestamp: chrono::Utc::now(),
        })?;

        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&jsonl_path)
            .await?;

        use tokio::io::AsyncWriteExt;
        file.write_all(user_line.as_bytes()).await?;
        file.write_all(b"\n").await?;
        file.write_all(assistant_line.as_bytes()).await?;
        file.write_all(b"\n").await?;

        Ok(())
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

/// JSONL message format for session storage
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct JsonlMessage {
    pub id: String,
    pub role: String,
    pub content: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let registry = Arc::new(
            ConfigRegistry::new(temp_dir.path().join("configs"))
                .await
                .unwrap(),
        );

        let path_resolver = PathResolver::with_dirs(
            temp_dir.path().join("config"),
            temp_dir.path().join("data"),
            temp_dir.path().join("cache"),
        );

        let service = StatelessAgentService::new(registry, path_resolver)
            .await
            .unwrap();

        let metrics = service.metrics().await;
        assert_eq!(metrics.total_executions, 0);
    }
}
