//! Task Manager for synchronous tool execution
//!
//! The `TaskManager` provides a simple interface for executing tools
//! with timeout support. All execution is synchronous - the agent
//! waits for tool completion before continuing.
//!
//! For async patterns, use:
//! - Shell background: command &
//! - MCP async patterns
//! - Agent spawn

use crate::engine::execution::{ExecutionMode, TaskExecutor, TaskId};
use crate::observability::Observability;
use crate::tools::Tool;
use anyhow::Result;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Instant;
use tracing::{error, info, instrument, warn};

/// Configuration for the task manager
#[derive(Debug, Clone)]
pub struct TaskManagerConfig {
    /// Default execution timeout
    pub default_timeout: std::time::Duration,
}

impl Default for TaskManagerConfig {
    fn default() -> Self {
        Self {
            default_timeout: std::time::Duration::from_secs(120),
        }
    }
}

/// The `TaskManager` handles tool execution with timeout support
#[derive(Clone)]
pub struct TaskManager {
    /// Configuration
    config: TaskManagerConfig,
    /// Task executor
    executor: TaskExecutor,
    /// Observability for audit logging
    observability: Option<Arc<Observability>>,
}

impl std::fmt::Debug for TaskManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskManager")
            .field("config", &self.config)
            .field("executor", &self.executor)
            .field("observability", &self.observability.is_some())
            .finish()
    }
}

impl TaskManager {
    /// Create a new task manager with default configuration
    #[must_use]
    pub fn new() -> Self {
        Self::with_config(TaskManagerConfig::default())
    }

    /// Create a new task manager with custom configuration
    #[must_use]
    pub fn with_config(config: TaskManagerConfig) -> Self {
        Self {
            config,
            executor: TaskExecutor::new(),
            observability: None,
        }
    }

    /// Set observability for audit logging
    #[must_use]
    pub fn with_observability(mut self, observability: Arc<Observability>) -> Self {
        self.observability = Some(observability);
        self
    }

    /// Execute a tool synchronously with panic isolation
    ///
    /// This is the main entry point for tool execution. The agent
    /// will block until the tool completes or times out.
    ///
    /// Panics in tool execution are caught and converted to errors,
    /// preventing the agentic loop from crashing.
    #[instrument(skip(self, tool), fields(tool_name = %tool.name()))]
    pub async fn execute(
        &self,
        tool: Arc<dyn Tool>,
        params: serde_json::Value,
        mode: Option<ExecutionMode>,
    ) -> Result<serde_json::Value> {
        let timeout = mode.map_or(self.config.default_timeout, |m| m.timeout);
        let tool_name = tool.name().to_string();
        let task_id = TaskId::new();

        info!(task_id = %task_id, %tool_name, ?timeout, "Executing tool");

        // Audit log: tool call started
        if let Some(ref obs) = self.observability {
            let _ = obs
                .audit(
                    "tool.call",
                    None, // TODO: pass agent_id through context
                    serde_json::json!({
                        "task_id": task_id.to_string(),
                        "tool_name": &tool_name,
                        "params": &params,
                    }),
                )
                .await;
        }

        let start = Instant::now();

        // Execute with panic isolation
        let result = self
            .execute_with_panic_isolation(tool.clone(), params.clone(), timeout)
            .await;

        let duration = start.elapsed();

        match &result {
            Ok(output) => {
                info!(
                    task_id = %task_id,
                    %tool_name,
                    duration_ms = duration.as_millis() as u64,
                    "Tool completed successfully"
                );

                // Audit log: tool call succeeded
                if let Some(ref obs) = self.observability {
                    let _ = obs
                        .audit(
                            "tool.result",
                            None, // TODO: pass agent_id through context
                            serde_json::json!({
                                "task_id": task_id.to_string(),
                                "tool_name": &tool_name,
                                "success": true,
                                "output": output,
                                "duration_ms": duration.as_millis() as u64,
                            }),
                        )
                        .await;
                }
            }
            Err(e) => {
                warn!(
                    task_id = %task_id,
                    %tool_name,
                    duration_ms = duration.as_millis() as u64,
                    error = %e,
                    "Tool failed"
                );

                // Audit log: tool call failed
                if let Some(ref obs) = self.observability {
                    let _ = obs
                        .audit(
                            "tool.result",
                            None, // TODO: pass agent_id through context
                            serde_json::json!({
                                "task_id": task_id.to_string(),
                                "tool_name": &tool_name,
                                "success": false,
                                "error": e.to_string(),
                                "duration_ms": duration.as_millis() as u64,
                            }),
                        )
                        .await;
                }
            }
        }

        result
    }

    /// Execute a tool with panic isolation
    ///
    /// Catches panics during tool execution and converts them to errors.
    /// This ensures that a buggy tool doesn't crash the entire agentic loop.
    async fn execute_with_panic_isolation(
        &self,
        tool: Arc<dyn Tool>,
        params: serde_json::Value,
        timeout: std::time::Duration,
    ) -> Result<serde_json::Value> {
        let tool_name = tool.name().to_string();
        let tool_name_for_error = tool_name.clone();

        // Spawn the tool execution in a blocking task with panic catching
        let spawn_result = tokio::task::spawn_blocking(move || {
            // Use AssertUnwindSafe because we're catching the panic anyway
            let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
                // Create a runtime for the async tool execution
                let rt = tokio::runtime::Handle::current();
                rt.block_on(async { tool.execute(params).await })
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
                        "Tool '{}' panicked: {}",
                        tool_name,
                        panic_msg
                    ))
                }
            }
        });

        // Apply timeout to the spawned task
        let result = tokio::time::timeout(timeout, spawn_result).await;

        match result {
            Ok(Ok(tool_result)) => tool_result,
            Ok(Err(e)) => {
                if e.is_panic() {
                    error!("Task panicked during execution: {}", e);
                    Err(anyhow::anyhow!(
                        "Tool '{}' task panicked",
                        tool_name_for_error
                    ))
                } else {
                    Err(anyhow::anyhow!(
                        "Tool '{}' task cancelled",
                        tool_name_for_error
                    ))
                }
            }
            Err(_) => Err(anyhow::anyhow!(
                "Tool '{}' timed out after {:?}",
                tool_name_for_error,
                timeout
            )),
        }
    }

    /// Execute with explicit timeout
    pub async fn execute_with_timeout(
        &self,
        tool: Arc<dyn Tool>,
        params: serde_json::Value,
        timeout_secs: u64,
    ) -> Result<serde_json::Value> {
        self.execute(
            tool,
            params,
            Some(ExecutionMode::with_timeout(timeout_secs)),
        )
        .await
    }

    /// Get default timeout
    #[must_use]
    pub fn default_timeout(&self) -> std::time::Duration {
        self.config.default_timeout
    }
}

impl Default for TaskManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Statistics for the task manager
#[derive(Debug, Clone, Copy, Default)]
pub struct TaskManagerStats {
    pub total_executed: u64,
    pub total_failed: u64,
    pub total_timed_out: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::Tool;
    use async_trait::async_trait;

    // Mock tool for testing
    struct MockTool {
        name: String,
        delay_ms: u64,
        result: serde_json::Value,
    }

    #[async_trait]
    impl Tool for MockTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            "Mock tool for testing"
        }

        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {}
            })
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }

        async fn execute(&self, _params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
            if self.delay_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(self.delay_ms)).await;
            }
            Ok(self.result.clone())
        }
    }

    #[tokio::test]
    async fn test_sync_execution() {
        let manager = TaskManager::new();
        let tool = Arc::new(MockTool {
            name: "test".to_string(),
            delay_ms: 0,
            result: serde_json::json!({"success": true}),
        });

        let result = manager
            .execute(tool, serde_json::json!({}), None)
            .await
            .unwrap();

        assert_eq!(result["success"], true);
    }

    #[tokio::test]
    async fn test_execution_with_timeout() {
        let manager = TaskManager::new();
        let tool = Arc::new(MockTool {
            name: "slow".to_string(),
            delay_ms: 10000, // Very slow
            result: serde_json::json!({"done": true}),
        });

        // Should timeout
        let result = manager
            .execute_with_timeout(tool, serde_json::json!({}), 1)
            .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("timed out"));
    }

    #[tokio::test]
    async fn test_execution_success_with_delay() {
        let manager = TaskManager::new();
        let tool = Arc::new(MockTool {
            name: "fast".to_string(),
            delay_ms: 10, // Small delay
            result: serde_json::json!({"fast": true}),
        });

        let result = manager
            .execute_with_timeout(tool, serde_json::json!({}), 5)
            .await
            .unwrap();

        assert_eq!(result["fast"], true);
    }

    // Mock tool that panics
    struct PanickingTool {
        name: String,
        panic_message: String,
    }

    #[async_trait]
    impl Tool for PanickingTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            "Mock tool that panics"
        }

        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {}
            })
        }

        async fn execute(&self, _params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
            panic!("{}", self.panic_message);
        }
    }

    #[tokio::test]
    async fn test_panic_isolation() {
        let manager = TaskManager::new();
        let tool = Arc::new(PanickingTool {
            name: "panicker".to_string(),
            panic_message: "Intentional test panic".to_string(),
        });

        // Should not panic, should return an error
        let result = manager.execute(tool, serde_json::json!({}), None).await;

        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("panicked"));
        assert!(error_msg.contains("Intentional test panic"));
    }

    #[tokio::test]
    async fn test_panic_isolation_unknown_panic() {
        let manager = TaskManager::new();

        // Tool that panics with a non-string type
        struct PanicWithNumber;

        #[async_trait]
        impl Tool for PanicWithNumber {
            fn name(&self) -> &str {
                "numeric_panicker"
            }

            fn description(&self) -> &str {
                "Panics with a number"
            }

            fn parameters(&self) -> serde_json::Value {
                serde_json::json!({})
            }

            async fn execute(
                &self,
                _params: serde_json::Value,
            ) -> anyhow::Result<serde_json::Value> {
                // This panic payload is not a String or &str
                std::panic::panic_any(42i32);
            }
        }

        let tool = Arc::new(PanicWithNumber);
        let result = manager.execute(tool, serde_json::json!({}), None).await;

        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("panicked"));
        assert!(error_msg.contains("Unknown panic"));
    }
}
