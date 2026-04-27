//! Integration between `ExtensionAsyncAdapter` and the async executor framework
//!
//! This module provides helpers for using `ExtensionAsyncAdapter` with the
//! unified async executor. The `AsyncTool` trait (Framework 2) has been
//! deleted per Issue 006.

use crate::extensions::core::ExtensionAsyncAdapter;
use crate::tools::Tool;
use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

/// Extension-based tool wrapper that implements `Tool`.
///
/// Sync execution delegates to the extension's `ToolExecute` hook via
/// `ExtensionAsyncAdapter::execute_async` fallback path.
pub struct ExtensionAsyncTool {
    adapter: ExtensionAsyncAdapter,
    tool_name: String,
    description: String,
    parameters: Value,
}

impl ExtensionAsyncTool {
    /// Create a new extension-based tool wrapper
    pub fn new(
        adapter: ExtensionAsyncAdapter,
        tool_name: impl Into<String>,
        description: impl Into<String>,
        parameters: Value,
    ) -> Self {
        Self {
            adapter,
            tool_name: tool_name.into(),
            description: description.into(),
            parameters,
        }
    }

    /// Get the underlying adapter
    #[must_use]
    pub fn adapter(&self) -> &ExtensionAsyncAdapter {
        &self.adapter
    }
}

impl std::fmt::Debug for ExtensionAsyncTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtensionAsyncTool")
            .field("tool_name", &self.tool_name)
            .field("description", &self.description)
            .finish()
    }
}

#[async_trait]
impl Tool for ExtensionAsyncTool {
    fn name(&self) -> &str {
        &self.tool_name
    }

    fn description(&self) -> String {
        self.description.clone()
    }

    fn parameters(&self) -> Value {
        self.parameters.clone()
    }

    async fn execute(&self, params: Value) -> Result<Value> {
        // Fallback: use the adapter's async path and wait for completion
        let receipt = self
            .adapter
            .execute_async(&self.tool_name, params, "default_session")
            .await?;

        // Wait for the task to complete
        let result = self
            .adapter
            .wait_for_completion(&receipt.task_id, std::time::Duration::from_secs(300))
            .await?;

        match result {
            crate::tools::async_executor::WaitResult::Completed { result } => Ok(result.to_json()),
            crate::tools::async_executor::WaitResult::Failed { error } => {
                Err(anyhow::anyhow!("Async execution failed: {error}"))
            }
            crate::tools::async_executor::WaitResult::Cancelled => {
                Err(anyhow::anyhow!("Async execution was cancelled"))
            }
            crate::tools::async_executor::WaitResult::Timeout => {
                Err(anyhow::anyhow!("Async execution timed out"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extensions::ExtensionCore;
    use std::sync::Arc;

    #[test]
    fn test_extension_async_tool_creation() {
        let core = Arc::new(ExtensionCore::new());
        let adapter = ExtensionAsyncAdapter::new(core);

        let tool = ExtensionAsyncTool::new(
            adapter,
            "test_tool",
            "A test tool",
            serde_json::json!({
                "type": "object",
                "properties": {}
            }),
        );

        assert_eq!(tool.tool_name, "test_tool");
    }
}
