//! Execution primitives for tool execution
//!
//! Tools execute synchronously with timeout support.
//! For async patterns, use shell background, MCP async, or agent spawn.

use serde::{Deserialize, Serialize};

/// Unique identifier for tasks (used for observability)
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskId(pub String);

impl TaskId {
    /// Generate a new unique task ID
    #[must_use]
    pub fn new() -> Self {
        Self(format!(
            "task_{}",
            uuid::Uuid::new_v4().to_string().replace('-', "")
        ))
    }
}

impl Default for TaskId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for TaskId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Execution mode - sync only
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutionMode {
    /// Timeout for execution
    pub timeout: std::time::Duration,
}

impl ExecutionMode {
    /// Create a sync mode with the given timeout
    #[must_use]
    pub fn with_timeout(timeout_secs: u64) -> Self {
        Self {
            timeout: std::time::Duration::from_secs(timeout_secs),
        }
    }

    /// Create a sync mode with default 120s timeout
    #[must_use]
    pub fn default() -> Self {
        Self::with_timeout(120)
    }
}

impl Default for ExecutionMode {
    fn default() -> Self {
        Self::default()
    }
}

/// Task status for observability
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Task is currently running
    Running,
    /// Task completed successfully
    Completed,
    /// Task failed
    Failed { error: String },
    /// Task timed out
    Timeout,
}

/// Summary of a task for observability/logging
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSummary {
    pub id: TaskId,
    pub name: String,
    pub status: TaskStatus,
    pub duration_ms: u64,
}

/// Simple task executor - synchronous execution only
#[derive(Debug, Clone, Default)]
pub struct TaskExecutor;

impl TaskExecutor {
    /// Create a new task executor
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Execute a tool synchronously with timeout
    pub async fn execute<F, Fut, T>(
        &self,
        name: &str,
        f: F,
        timeout: std::time::Duration,
    ) -> anyhow::Result<T>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<T>>,
    {
        match tokio::time::timeout(timeout, f()).await {
            Ok(result) => result,
            Err(_) => Err(anyhow::anyhow!("Task '{name}' timed out after {timeout:?}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_id_generation() {
        let id1 = TaskId::new();
        let id2 = TaskId::new();
        assert_ne!(id1.0, id2.0);
        assert!(id1.0.starts_with("task_"));
    }

    #[test]
    fn test_execution_mode() {
        let mode = ExecutionMode::with_timeout(60);
        assert_eq!(mode.timeout.as_secs(), 60);
    }

    #[tokio::test]
    async fn test_executor_success() {
        let executor = TaskExecutor::new();
        let result = executor
            .execute(
                "test",
                || async { Ok(42) },
                std::time::Duration::from_secs(5),
            )
            .await;
        assert_eq!(result.unwrap(), 42);
    }

    #[tokio::test]
    async fn test_executor_timeout() {
        let executor = TaskExecutor::new();
        let result = executor
            .execute(
                "slow",
                || async {
                    tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                    Ok(())
                },
                std::time::Duration::from_millis(100),
            )
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("timed out"));
    }
}
