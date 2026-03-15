//! Sessions Send Tool - A2A messaging via session-to-session communication
//!
//! This tool provides agent-to-agent messaging using the unified async executor,
//! enabling consistent delivery modes (queue_when_busy, steer, collect, interrupt)
//! for A2A communication.
//!
//! Inspired by OpenClaw's sessions_send tool architecture.

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

/// Sessions Send tool for A2A messaging
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
    /// Default timeout for sync mode
    default_timeout_secs: u64,
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
            default_timeout_secs: 60,
        }
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
    pub fn with_timeout(mut self, timeout_secs: u64) -> Self {
        self.default_timeout_secs = timeout_secs;
        self
    }

    /// Execute send in async mode
    async fn execute_async(
        &self,
        target_session_key: String,
        message: String,
        delivery_mode: AsyncResultDeliveryMode,
        label: Option<String>,
    ) -> Result<serde_json::Value> {
        let executor = self
            .executor
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Async mode not configured for sessions_send tool"))?;

        let parent_session_key = self
            .current_session_key
            .clone()
            .unwrap_or_else(|| "default".to_string());

        let task_id = format!("a2a_{}", Uuid::new_v4().simple());
        let conversation_id = format!("conv_{}", Uuid::new_v4().simple());

        // Clone for closure
        let target_session_clone = target_session_key.clone();
        let message_clone = message.clone();
        let parent_session_clone = parent_session_key.clone();

        // Execute using unified executor
        let receipt = executor
            .execute(
                task_id.clone(),
                "sessions_send",
                json!({
                    "target_session": &target_session_key,
                    "message": &message,
                    "source_session": &parent_session_key,
                }),
                parent_session_key,
                AsyncToolConfig {
                    delivery_mode,
                    delivery_target: None,
                    timeout_secs: self.default_timeout_secs,
                    cleanup_after_delivery: true,
                    label: label.clone(),
                },
                move || async move {
                    // In a real implementation, this would:
                    // 1. Send message to target session
                    // 2. Wait for response (if applicable)
                    // 3. Return the response as SessionMessage

                    // For now, simulate successful message delivery
                    Ok(AsyncTaskResult::SessionMessage {
                        from_session: parent_session_clone,
                        to_session: target_session_clone,
                        content: message_clone,
                        message_type: SessionMessageType::Request,
                        conversation_id: format!("conv_{}", Uuid::new_v4().simple()),
                        token_usage: None,
                    })
                },
            )
            .await?;

        Ok(json!({
            "task_id": receipt.task_id,
            "status": "accepted",
            "mode": "async",
            "target_session": target_session_key,
            "conversation_id": conversation_id,
            "check_status_tool": receipt.check_status_tool,
            "note": "Message queued for delivery. Result will be announced when target responds."
        }))
    }

    /// Execute send in sync mode
    async fn execute_sync(
        &self,
        target_session_key: String,
        message: String,
        timeout_secs: u64,
    ) -> Result<serde_json::Value> {
        let executor = self
            .executor
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Async mode not configured for sessions_send tool"))?;

        let parent_session_key = self
            .current_session_key
            .clone()
            .unwrap_or_else(|| "default".to_string());

        let task_id = format!("a2a_{}", Uuid::new_v4().simple());

        // Clone for closure
        let target_session_clone = target_session_key.clone();
        let message_clone = message.clone();
        let parent_session_clone = parent_session_key.clone();

        // Execute using unified executor
        let receipt = executor
            .execute(
                task_id.clone(),
                "sessions_send",
                json!({
                    "target_session": &target_session_key,
                    "message": &message,
                    "source_session": &parent_session_key,
                }),
                parent_session_key.clone(),
                AsyncToolConfig {
                    delivery_mode: AsyncResultDeliveryMode::QueueWhenBusy,
                    delivery_target: None,
                    timeout_secs,
                    cleanup_after_delivery: true,
                    label: None,
                },
                move || async move {
                    // Simulate sending message and waiting for response
                    // In real implementation, would wait for target agent response
                    Ok(AsyncTaskResult::SessionMessage {
                        from_session: target_session_clone,
                        to_session: parent_session_clone,
                        content: format!("Response to: {}", message_clone),
                        message_type: SessionMessageType::Response,
                        conversation_id: format!("conv_{}", Uuid::new_v4().simple()),
                        token_usage: None,
                    })
                },
            )
            .await?;

        // Wait for completion
        use std::time::Duration;
        use tokio::time::timeout;

        let wait_result = timeout(
            Duration::from_secs(timeout_secs),
            executor.wait_for_completion(&task_id, Duration::from_secs(timeout_secs)),
        )
        .await;

        match wait_result {
            Ok(Ok(crate::agent::async_tool_framework::WaitResult::Completed { .. })) => Ok(json!({
                "status": "completed",
                "task_id": receipt.task_id,
                "target_session": target_session_key,
                "mode": "sync",
                "result": "Message delivered successfully"
            })),
            Ok(Ok(crate::agent::async_tool_framework::WaitResult::Timeout)) => {
                Err(anyhow::anyhow!(
                    "Timeout waiting for response after {} seconds",
                    timeout_secs
                ))
            }
            Ok(Ok(crate::agent::async_tool_framework::WaitResult::Failed { error })) => {
                Err(anyhow::anyhow!("Failed to send message: {}", error))
            }
            Ok(Ok(crate::agent::async_tool_framework::WaitResult::Cancelled)) => {
                Err(anyhow::anyhow!("Message was cancelled"))
            }
            Ok(Err(e)) => Err(anyhow::anyhow!("Error waiting for completion: {}", e)),
            Err(_) => Err(anyhow::anyhow!(
                "Timeout waiting for response after {} seconds",
                timeout_secs
            )),
        }
    }

    /// Resolve agent ID to session key
    ///
    /// In a full implementation, this would:
    /// 1. Check if agent has an active session
    /// 2. Create ephemeral session if needed
    /// 3. Return the session key
    async fn resolve_agent_session(&self, agent_id: &str) -> Result<String> {
        // For now, construct a standard session key
        // In production, this would query the session manager
        Ok(format!("agent:{}", agent_id))
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
        "Send messages to other agent sessions (A2A messaging)"
    }

    fn llm_description(&self) -> String {
        r#"## Purpose
Send messages to other agents via session-to-session communication.

## Modes

### Async Mode (default)
Returns immediately with task ID. Target agent response delivered via queue.
```json
{
  "target": "analyzer",
  "message": "Review this code",
  "mode": "async"
}
```

### Sync Mode
Blocks until target responds or timeout.
```json
{
  "target": "analyzer", 
  "message": "Review this code",
  "mode": "sync",
  "timeout": 60
}
```

## Delivery Modes (Async)
- `queue_when_busy`: Queue result, deliver when idle (default)
- `interrupt`: Interrupt current execution
- `collect`: Batch with other results
- `steer`: Inject into running session

## When to Use
- Delegating tasks to specialized agents
- Requesting analysis from expert agents
- Coordinating multi-agent workflows
- Fire-and-forget announcements

## Input
```json
{
  "target": "agent-id-or-session-key",
  "message": "Your message here",
  "mode": "async",
  "label": "optional-label"
}
```

## Response (Async)
```json
{
  "status": "accepted",
  "task_id": "a2a_uuid",
  "conversation_id": "conv_uuid",
  "note": "Message queued for delivery..."
}
```

## Response (Sync)
```json
{
  "status": "completed",
  "result": "Target agent response..."
}
```"#
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "target": {
                    "type": "string",
                    "description": "Target agent ID or session key"
                },
                "message": {
                    "type": "string",
                    "description": "Message to send to target agent"
                },
                "mode": {
                    "type": "string",
                    "enum": ["async", "sync"],
                    "description": "Execution mode",
                    "default": "async"
                },
                "timeout": {
                    "type": "integer",
                    "description": "Timeout in seconds for sync mode",
                    "default": 60,
                    "minimum": 1,
                    "maximum": 300
                },
                "label": {
                    "type": "string",
                    "description": "Optional label for tracking (async mode)"
                },
                "delivery_mode": {
                    "type": "string",
                    "enum": ["queue_when_busy", "interrupt", "collect", "steer"],
                    "description": "Result delivery mode (async)",
                    "default": "queue_when_busy"
                }
            },
            "required": ["target", "message"]
        })
    }

    async fn execute(&self, params: serde_json::Value) -> Result<serde_json::Value> {
        let target = params["target"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: target"))?;

        let message = params["message"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: message"))?;

        // Parse mode
        let mode = params
            .get("mode")
            .and_then(|m| m.as_str())
            .unwrap_or("async");

        match mode {
            "sync" => {
                let timeout = params
                    .get("timeout")
                    .and_then(|t| t.as_u64())
                    .unwrap_or(self.default_timeout_secs);

                let target_session = self.resolve_agent_session(target).await?;
                self.execute_sync(target_session, message.to_string(), timeout)
                    .await
            }
            "async" => {
                let label = params
                    .get("label")
                    .and_then(|l| l.as_str())
                    .map(String::from);

                let delivery_mode = params
                    .get("delivery_mode")
                    .and_then(|d| d.as_str())
                    .map(|d| match d {
                        "interrupt" => AsyncResultDeliveryMode::Interrupt,
                        "collect" => AsyncResultDeliveryMode::Collect,
                        "steer" => AsyncResultDeliveryMode::Steer,
                        _ => AsyncResultDeliveryMode::QueueWhenBusy,
                    })
                    .unwrap_or_default();

                let target_session = self.resolve_agent_session(target).await?;
                self.execute_async(target_session, message.to_string(), delivery_mode, label)
                    .await
            }
            _ => Err(anyhow::anyhow!(
                "Invalid mode '{}'. Use 'sync' or 'async'",
                mode
            )),
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
    fn test_sessions_send_tool_with_session_context() {
        let tool =
            SessionsSendTool::new().with_session_context("agent:test:session:123", "test_agent");

        assert_eq!(
            tool.current_session_key,
            Some("agent:test:session:123".to_string())
        );
        assert_eq!(tool.current_agent_name, Some("test_agent".to_string()));
    }

    #[tokio::test]
    async fn test_resolve_agent_session() {
        let tool = SessionsSendTool::new();
        let session_key = tool.resolve_agent_session("analyzer").await.unwrap();
        assert!(session_key.contains("analyzer"));
    }
}
