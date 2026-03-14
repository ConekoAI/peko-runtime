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
use crate::tools::Tool;
use anyhow::Result;
use std::sync::Arc;
use std::time::Instant;
use tracing::{info, instrument, warn};

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
#[derive(Debug, Clone)]
pub struct TaskManager {
    /// Configuration
    config: TaskManagerConfig,
    /// Task executor
    executor: TaskExecutor,
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
        }
    }

    /// Execute a tool synchronously
    ///
    /// This is the main entry point for tool execution. The agent
    /// will block until the tool completes or times out.
    #[instrument(skip(self, tool), fields(tool_name = %tool.name()))]
    pub async fn execute(
        &self,
        tool: Arc<dyn Tool>,
        params: serde_json::Value,
        mode: Option<ExecutionMode>,
    ) -> Result<serde_json::Value> {
        let timeout = mode
            .map_or(self.config.default_timeout, |m| m.timeout);
        let tool_name = tool.name().to_string();
        let task_id = TaskId::new();

        info!(task_id = %task_id, %tool_name, ?timeout, "Executing tool");

        let start = Instant::now();

        let result = self
            .executor
            .execute(&tool_name, || tool.execute(params), timeout)
            .await;

        let duration = start.elapsed();

        match &result {
            Ok(_) => {
                info!(
                    task_id = %task_id,
                    %tool_name,
                    duration_ms = duration.as_millis() as u64,
                    "Tool completed successfully"
                );
            }
            Err(e) => {
                warn!(
                    task_id = %task_id,
                    %tool_name,
                    duration_ms = duration.as_millis() as u64,
                    error = %e,
                    "Tool failed"
                );
            }
        }

        result
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
}
