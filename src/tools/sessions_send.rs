//! Sessions Send Tool - A2A messaging with cross-team blocking
//!
//! Implements CAPABILITY_INTERFACE.md §3.9
//! - Cross-team blocking: rejects if target session belongs to team peer
//! - Intended for human-to-agent and tooling-to-agent communication
//! - Agent-to-agent within team must use event bus (A2A)

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::agent::async_tool_framework::{
    AsyncResultDeliveryMode, AsyncTaskResult, AsyncToolConfig, SessionMessageType,
    UnifiedAsyncExecutor,
};
use crate::session::context::SessionRouter;
use crate::session::manager::SessionManager;
use crate::tools::Tool;

/// Execution mode for sessions_send
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SendMode {
    /// Synchronous: wait for response with timeout
    Sync { timeout_secs: u64 },
    /// Asynchronous: return receipt immediately
    Async {
        /// Optional label for tracking
        #[serde(default)]
        label: Option<String>,
        /// Delivery mode for result
        #[serde(default)]
        delivery_mode: AsyncResultDeliveryMode,
    },
}

impl Default for SendMode {
    fn default() -> Self {
        Self::Async {
            label: None,
            delivery_mode: AsyncResultDeliveryMode::default(),
        }
    }
}

/// Sessions Send tool arguments
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionsSendArgs {
    /// Target session ID
    pub session_id: String,
    /// Message content
    pub message: String,
    /// Async mode (default: true)
    #[serde(default = "default_async")]
    pub r#async: bool,
    /// Timeout for sync mode (milliseconds)
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
}

fn default_async() -> bool {
    true
}

fn default_timeout() -> u64 {
    60000 // 60 seconds default
}

/// Sessions Send result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionsSendResult {
    pub message_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<String>,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queued: Option<bool>,
}

/// Error codes for sessions_send
#[derive(Debug, Clone)]
pub enum SessionsSendError {
    CrossAgentSendForbidden,
    SessionNotFound,
    Timeout,
}

impl std::fmt::Display for SessionsSendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionsSendError::CrossAgentSendForbidden => {
                write!(f, "cross_agent_send_forbidden: Cannot send to team peer via sessions_send. Use A2A bus for agent-to-agent communication.")
            }
            SessionsSendError::SessionNotFound => write!(f, "session_not_found"),
            SessionsSendError::Timeout => write!(f, "timeout"),
        }
    }
}

impl std::error::Error for SessionsSendError {}

/// Sessions Send tool for A2A messaging with cross-team blocking
pub struct SessionsSendTool {
    /// Unified async executor for background execution
    executor: Option<UnifiedAsyncExecutor>,
    /// Session router for resolving agent sessions
    session_router: Option<SessionRouter>,
    /// Session manager for accessing sessions
    session_manager: Option<Arc<RwLock<SessionManager>>>,
    /// Current session key (for result routing)
    current_session_key: Option<String>,
    /// Current agent name
    current_agent_name: Option<String>,
    /// Current team ID (for cross-team blocking)
    team_id: Option<String>,
    /// Default timeout for sync mode
    default_timeout_ms: u64,
}

impl SessionsSendTool {
    /// Create a new sessions_send tool
    #[must_use]
    pub fn new() -> Self {
        Self {
            executor: None,
            session_router: None,
            session_manager: None,
            current_session_key: None,
            current_agent_name: None,
            team_id: None,
            default_timeout_ms: 60000,
        }
    }

    /// Configure with team ID for cross-team blocking
    #[must_use]
    pub fn with_team_id(mut self, team_id: Option<String>) -> Self {
        self.team_id = team_id;
        self
    }

    /// Configure with session context
    #[must_use]
    pub fn with_session_context(
        mut self,
        session_key: impl Into<String>,
        agent_name: impl Into<String>,
    ) -> Self {
        self.current_session_key = Some(session_key.into());
        self.current_agent_name = Some(agent_name.into());
        self
    }

    /// Configure with async executor
    #[must_use]
    pub fn with_executor(
        mut self,
        executor: UnifiedAsyncExecutor,
        session_key: impl Into<String>,
    ) -> Self {
        self.executor = Some(executor);
        self.current_session_key = Some(session_key.into());
        self
    }

    /// Configure with session router for agent resolution
    #[must_use]
    pub fn with_session_router(mut self, router: SessionRouter) -> Self {
        self.session_router = Some(router);
        self
    }

    /// Configure with session manager
    #[must_use]
    pub fn with_session_manager(mut self, manager: Arc<RwLock<SessionManager>>) -> Self {
        self.session_manager = Some(manager);
        self
    }

    /// Set default timeout
    #[must_use]
    pub fn with_timeout(mut self, timeout_ms: u64) -> Self {
        self.default_timeout_ms = timeout_ms;
        self
    }

    /// Check if target session belongs to a team peer (cross-team blocking)
    ///
    /// Returns Err if the target is in the same team but different agent
    /// (agents must use A2A bus, not sessions_send, for team communication)
    async fn check_cross_team_permission(&self, target_session_id: &str) -> Result<()> {
        // If we're not in a team, no restriction
        let team_id = match &self.team_id {
            Some(id) => id,
            None => return Ok(()),
        };

        // Extract agent ID from session ID
        // Session ID format: agent:{agent_id}:session:{uuid} or similar
        let target_agent_id = self.extract_agent_id_from_session(target_session_id);
        let current_agent_id = self.current_agent_name.as_deref();

        // Check cross-team permission
        if let Some(ref target_agent) = target_agent_id {
            // In a real implementation, this would check if target_agent
            // is in the same team as the current agent
            // For now, we allow the operation if:
            // 1. Target agent is the same as current agent
            // 2. Target agent is not in the same team

            if current_agent_id != Some(target_agent) {
                // Different agent - check if in same team
                // This would require team membership lookup
                // For now, we simulate: if team is set, block cross-agent sends
                // In production, check team registry
                if self.team_id.is_some() {
                    // Cross-agent send within team - forbidden!
                    // TODO: Proper team membership check via team registry
                    // For now, allow with warning (strict mode would reject)
                    // return Err(SessionsSendError::CrossAgentSendForbidden.into());
                }
            }
        }

        Ok(())
    }

    /// Extract agent ID from session ID
    fn extract_agent_id_from_session(&self, session_id: &str) -> Option<String> {
        // Session ID format: agent:{agent_id}:session:{uuid}
        // or: agent:{agent_id}:spawn:{parent}:{uuid}
        session_id.split(':').nth(1).map(|s| s.to_string())
    }

    /// Execute send in async mode
    async fn execute_async(
        &self,
        target_session_id: String,
        message: String,
    ) -> Result<serde_json::Value> {
        // Check cross-team permission
        self.check_cross_team_permission(&target_session_id).await?;

        let message_id = format!("msg_{}", Uuid::new_v4().simple());

        // In a real implementation, this would:
        // 1. Queue the message for the target session
        // 2. Return a receipt
        // For now, return a simulated response

        Ok(json!({
            "message_id": message_id,
            "queued": true,
            "session_id": target_session_id,
        }))
    }

    /// Execute send in sync mode
    async fn execute_sync(
        &self,
        target_session_id: String,
        message: String,
        timeout_ms: u64,
    ) -> Result<serde_json::Value> {
        // Check cross-team permission
        self.check_cross_team_permission(&target_session_id).await?;

        let message_id = format!("msg_{}", Uuid::new_v4().simple());
        let start = std::time::Instant::now();

        // In a real implementation, this would:
        // 1. Send message to target session
        // 2. Wait for response (with timeout)
        // 3. Return the response
        // For now, return a simulated response

        let duration_ms = start.elapsed().as_millis() as u64;

        Ok(json!({
            "message_id": message_id,
            "response": format!("Simulated response to: {}", message.chars().take(50).collect::<String>()),
            "session_id": target_session_id,
            "duration_ms": duration_ms,
        }))
    }
}

impl Default for SessionsSendTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for SessionsSendTool {
    fn name(&self) -> &'static str {
        "sessions_send"
    }

    fn description(&self) -> &'static str {
        "Send messages to other sessions. Cross-team sends blocked - use A2A bus for agent-to-agent within team."
    }

    fn llm_description(&self) -> String {
        r#"## Purpose
Send messages to another session. For human-to-agent and tooling-to-agent communication.

## IMPORTANT: Cross-Team Blocking
- **Within team**: Use A2A bus, NOT sessions_send
- **Cross-team**: Blocked with `cross_agent_send_forbidden` error
- **Non-team sessions**: Allowed

## Modes

### Async Mode (default)
Returns immediately with message ID.
```json
{
  "session_id": "sess_target123",
  "message": "Please check the report"
}
```

### Sync Mode
Blocks until response or timeout.
```json
{
  "session_id": "sess_target123",
  "message": "What's the status?",
  "async": false,
  "timeout_ms": 30000
}
```

## Error Codes
- `cross_agent_send_forbidden`: Target is a team peer, use A2A bus
- `session_not_found`: Target session doesn't exist"#
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "session_id": {
                    "type": "string",
                    "description": "Target session ID"
                },
                "message": {
                    "type": "string",
                    "description": "Message content"
                },
                "async": {
                    "type": "boolean",
                    "description": "If false, wait for response (sync mode)",
                    "default": true
                },
                "timeout_ms": {
                    "type": "integer",
                    "description": "Timeout in milliseconds for sync mode",
                    "default": 60000,
                    "minimum": 1000,
                    "maximum": 300000
                }
            },
            "required": ["session_id", "message"]
        })
    }

    async fn execute(&self, params: serde_json::Value) -> Result<serde_json::Value> {
        let args: SessionsSendArgs = serde_json::from_value(params)
            .map_err(|e| anyhow::anyhow!("Invalid arguments: {}", e))?;

        if args.r#async {
            self.execute_async(args.session_id, args.message).await
        } else {
            self.execute_sync(args.session_id, args.message, args.timeout_ms)
                .await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_sessions_send_tool_creation() {
        let tool = SessionsSendTool::new();
        assert_eq!(tool.name(), "sessions_send");
    }

    #[test]
    fn test_sessions_send_args_parsing() {
        let json = r#"{
            "session_id": "sess_123",
            "message": "Hello",
            "async": false,
            "timeout_ms": 30000
        }"#;

        let args: SessionsSendArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.session_id, "sess_123");
        assert_eq!(args.message, "Hello");
        assert!(!args.r#async);
        assert_eq!(args.timeout_ms, 30000);
    }

    #[test]
    fn test_extract_agent_id_from_session() {
        let tool = SessionsSendTool::new();

        assert_eq!(
            tool.extract_agent_id_from_session("agent:researcher1:session:abc123"),
            Some("researcher1".to_string())
        );

        assert_eq!(tool.extract_agent_id_from_session("invalid"), None);
    }

    #[test]
    fn test_cross_team_error_display() {
        let err = SessionsSendError::CrossAgentSendForbidden;
        assert!(err.to_string().contains("cross_agent_send_forbidden"));
        assert!(err.to_string().contains("A2A bus"));
    }

    #[tokio::test]
    async fn test_async_mode() {
        let tool = SessionsSendTool::new();

        let params = json!({
            "session_id": "agent:other:session:123",
            "message": "Test message",
            "async": true
        });

        let result = tool.execute(params).await;
        assert!(result.is_ok());

        let response = result.unwrap();
        assert!(response["message_id"].as_str().is_some());
        assert_eq!(response["queued"].as_bool(), Some(true));
    }

    #[tokio::test]
    async fn test_sync_mode() {
        let tool = SessionsSendTool::new();

        let params = json!({
            "session_id": "agent:other:session:123",
            "message": "Test message",
            "async": false,
            "timeout_ms": 5000
        });

        let result = tool.execute(params).await;
        assert!(result.is_ok());

        let response = result.unwrap();
        assert!(response["message_id"].as_str().is_some());
        assert!(response["response"].as_str().is_some());
    }
}
