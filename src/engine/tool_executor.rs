//! Unified Tool Executor
//!
//! Provides a single interface for executing all tool types with proper context injection.
//! This ensures Universal Tools, MCP Tools, and built-in tools all receive the same
//! runtime context (`agent_id`, `session_id`, etc.) for reserved parameter injection.

use crate::tools::{Tool, ToolContext};
use anyhow::Result;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, instrument};

/// Context passed to tool execution
///
/// This contains runtime identity information that gets injected into tools
/// via the `ToolContext` for reserved parameter support.
#[derive(Debug, Clone)]
pub struct ToolExecutionContext {
    /// Agent identifier
    pub agent_id: String,
    /// Session identifier
    pub session_id: String,
    /// Run identifier
    pub run_id: String,
    /// Peer identifier (for distributed contexts)
    pub peer_id: Option<String>,
    /// Workspace path
    pub workspace: String,
}

impl ToolExecutionContext {
    /// Create a new execution context
    pub fn new(
        agent_id: impl Into<String>,
        session_id: impl Into<String>,
        run_id: impl Into<String>,
    ) -> Self {
        Self {
            agent_id: agent_id.into(),
            session_id: session_id.into(),
            run_id: run_id.into(),
            peer_id: None,
            workspace: ".".to_string(),
        }
    }

    /// Set `peer_id`
    #[must_use]
    pub fn with_peer_id(mut self, peer_id: impl Into<String>) -> Self {
        self.peer_id = Some(peer_id.into());
        self
    }

    /// Set workspace
    #[must_use]
    pub fn with_workspace(mut self, workspace: impl Into<String>) -> Self {
        self.workspace = workspace.into();
        self
    }

    /// Convert to `ToolContext` for tool execution
    ///
    /// Creates a `ToolContext` with identity fields set for reserved parameter injection.
    pub fn to_tool_context(&self) -> ToolContext {
        tracing::debug!(
            "ToolExecutionContext::to_tool_context - agent_id={}, session_id={}, workspace={}",
            self.agent_id,
            self.session_id,
            self.workspace
        );
        // Create abort signal for this tool execution
        let abort_signal = crate::tools::AbortSignal::new();

        abort_signal
            .create_context(&self.run_id, "tool_exec", "tool")
            .with_agent_id(&self.agent_id)
            .with_session_id(&self.session_id)
            .with_workspace(&self.workspace)
    }
}

/// Unified tool executor
///
/// Replaces `TaskManager` and provides consistent context injection for all tools.
#[derive(Debug, Clone)]
pub struct ToolExecutor {
    /// Default timeout for tool execution
    default_timeout: Duration,
}

impl ToolExecutor {
    /// Create a new tool executor with default settings
    #[must_use] 
    pub fn new() -> Self {
        Self {
            default_timeout: Duration::from_secs(120),
        }
    }

    /// Create with custom default timeout
    #[must_use] 
    pub fn with_timeout(default_timeout: Duration) -> Self {
        Self { default_timeout }
    }

    /// Execute a tool with context injection
    ///
    /// This is the primary method for tool execution. It ensures all tools
    /// receive proper runtime context for reserved parameter injection.
    ///
    /// # Arguments
    /// * `tool` - The tool to execute
    /// * `params` - Parameters from the LLM
    /// * `context` - Runtime context with agent/session identity
    ///
    /// # Returns
    /// Tool result or error
    #[instrument(skip(self, tool, params, context), fields(tool_name = %tool.name()))]
    pub async fn execute_with_context(
        &self,
        tool: Arc<dyn Tool>,
        params: serde_json::Value,
        context: &ToolExecutionContext,
    ) -> Result<serde_json::Value> {
        let tool_name = tool.name().to_string();
        let start_time = std::time::Instant::now();

        info!(
            tool = %tool_name,
            agent_id = %context.agent_id,
            session_id = %context.session_id,
            "Starting tool execution with context"
        );

        // Create ToolContext with identity for reserved param injection
        let tool_context = context.to_tool_context();
        tracing::info!(
            "ToolExecutor - Created ToolContext: agent_id={:?}, session_id={:?}, workspace={:?}",
            tool_context.agent_id,
            tool_context.session_id,
            tool_context.workspace
        );

        // Execute with panic isolation
        let result = self
            .execute_with_panic_isolation(tool, params, &tool_context)
            .await;

        let duration = start_time.elapsed();

        match &result {
            Ok(_) => {
                info!(
                    tool = %tool_name,
                    duration_ms = duration.as_millis() as u64,
                    "Tool executed successfully"
                );
            }
            Err(e) => {
                info!(
                    tool = %tool_name,
                    error = %e,
                    duration_ms = duration.as_millis() as u64,
                    "Tool execution failed"
                );
            }
        }

        result
    }

    /// Execute with panic isolation
    ///
    /// Catches panics during tool execution and converts them to errors.
    /// This ensures that a buggy tool doesn't crash the entire agent.
    async fn execute_with_panic_isolation(
        &self,
        tool: Arc<dyn Tool>,
        params: serde_json::Value,
        tool_context: &ToolContext,
    ) -> Result<serde_json::Value> {
        let tool_name = tool.name().to_string();

        // Spawn the tool execution in a blocking task with panic catching
        let spawn_result = tokio::task::spawn_blocking({
            let tool = Arc::clone(&tool);
            let params = params.clone();
            let tool_context = tool_context.clone();

            move || {
                // Use AssertUnwindSafe because we're catching the panic anyway
                let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
                    // Create a runtime for the async tool execution
                    let rt = tokio::runtime::Handle::current();
                    rt.block_on(async {
                        // Call execute_with_context to enable reserved param injection
                        tool.execute_with_context(params, &tool_context).await
                    })
                }));

                match result {
                    Ok(tool_result) => tool_result,
                    Err(panic_info) => {
                        let panic_msg = if let Some(s) = panic_info.downcast_ref::<String>() {
                            s.clone()
                        } else if let Some(s) = panic_info.downcast_ref::<&str>() {
                            s.to_string()
                        } else {
                            "Unknown panic".to_string()
                        };

                        Err(anyhow::anyhow!(
                            "Tool '{tool_name}' panicked: {panic_msg}"
                        ))
                    }
                }
            }
        });

        // Apply timeout to the spawned task
        let result = tokio::time::timeout(self.default_timeout, spawn_result).await;

        match result {
            Ok(Ok(tool_result)) => tool_result,
            Ok(Err(e)) => {
                if e.is_panic() {
                    error!("Task panicked during execution: {}", e);
                    Err(anyhow::anyhow!("Tool '{}' task panicked", tool.name()))
                } else {
                    Err(anyhow::anyhow!("Tool '{}' task cancelled", tool.name()))
                }
            }
            Err(_) => Err(anyhow::anyhow!(
                "Tool '{}' timed out after {:?}",
                tool.name(),
                self.default_timeout
            )),
        }
    }

    /// Legacy execute method (without context)
    ///
    /// For backward compatibility. Tools will receive default/empty context.
    /// Prefer `execute_with_context` for new code.
    pub async fn execute(
        &self,
        tool: Arc<dyn Tool>,
        params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let context = ToolExecutionContext::new(
            "unknown", // agent_id
            "unknown", // session_id
            "unknown", // run_id
        );
        self.execute_with_context(tool, params, &context).await
    }
}

impl Default for ToolExecutor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{Tool, ToolContext};
    use async_trait::async_trait;
    use serde_json::json;

    /// Simple mock tool for testing
    struct MockTool {
        name: String,
    }

    #[async_trait]
    impl Tool for MockTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> String {
            "Mock tool".to_string()
        }

        fn parameters(&self) -> serde_json::Value {
            json!({"type": "object", "properties": {}})
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }

        async fn execute(&self, _params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
            Ok(json!({"result": "ok"}))
        }

        async fn execute_with_context(
            &self,
            params: serde_json::Value,
            ctx: &ToolContext,
        ) -> anyhow::Result<serde_json::Value> {
            // Return context info for verification
            Ok(json!({
                "params": params,
                "agent_id": ctx.agent_id,
                "session_id": ctx.session_id,
                "run_id": ctx.run_id,
            }))
        }
    }

    #[tokio::test]
    async fn test_executor_with_context() {
        let executor = ToolExecutor::new();
        let tool = Arc::new(MockTool {
            name: "test".to_string(),
        });

        let context = ToolExecutionContext::new("agent_123", "session_456", "run_789");

        let params = json!({"input": "test"});
        let result = executor
            .execute_with_context(tool, params, &context)
            .await
            .unwrap();

        assert_eq!(result["agent_id"], "agent_123");
        assert_eq!(result["session_id"], "session_456");
        assert_eq!(result["run_id"], "run_789");
    }

    #[tokio::test]
    async fn test_executor_legacy_execute() {
        let executor = ToolExecutor::new();
        let tool = Arc::new(MockTool {
            name: "legacy".to_string(),
        });

        let params = json!({"input": "test"});
        let result = executor.execute(tool, params).await.unwrap();

        // Legacy execute uses "unknown" for context
        assert_eq!(result["agent_id"], "unknown");
        assert_eq!(result["session_id"], "unknown");
    }
}
