//! Integration between `ExtensionAsyncAdapter` and `UnifiedAsyncTool` trait
//!
//! This module provides the bridge between the extension-based async system
//! and the `UnifiedAsyncTool` trait for seamless async tool execution.

use crate::agent::async_tool_framework::{
    AsyncTaskId, AsyncTaskReceipt, AsyncTaskStatus, AsyncToolConfig,
};
use crate::extensions::core::ExtensionAsyncAdapter;
use crate::tools::{BoxedAsyncTool, UnifiedAsyncTool};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

/// Extension-based implementation of `UnifiedAsyncTool`
///
/// This adapter wraps an `ExtensionAsyncAdapter` to implement the `UnifiedAsyncTool` trait,
/// allowing extension-based tools to be used interchangeably with native async tools.
pub struct ExtensionAsyncTool {
    adapter: ExtensionAsyncAdapter,
    tool_name: String,
    description: String,
    parameters: Value,
}

impl ExtensionAsyncTool {
    /// Create a new extension-based async tool
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
impl crate::tools::Tool for ExtensionAsyncTool {
    fn name(&self) -> &str {
        &self.tool_name
    }

    fn description(&self) -> String {
        self.description.clone()
    }

    fn parameters(&self) -> Value {
        self.parameters.clone()
    }

    async fn execute(&self, _params: Value) -> Result<Value> {
        // For sync execution of extension-based tools, we would need to:
        // 1. Use ExtensionCore to invoke ToolExecute hook
        // 2. Process the result
        //
        // For now, return an error indicating this path needs implementation
        // The async path (execute_async) is the primary use case
        Err(anyhow::anyhow!(
            "Sync execution not implemented for ExtensionAsyncTool. Use execute_async instead."
        ))
    }
}

#[async_trait]
impl UnifiedAsyncTool for ExtensionAsyncTool {
    fn supports_async(&self) -> bool {
        // Check if the extension has async hooks registered
        // This is a simplified check - in practice, we'd query the extension capabilities
        true
    }

    fn supports_status_check(&self) -> bool {
        true
    }

    fn supports_cancel(&self) -> bool {
        // Extensions may or may not support cancellation
        // We'll attempt cancel but gracefully handle unsupported cases
        true
    }

    async fn execute_async(
        &self,
        params: Value,
        _config: AsyncToolConfig,
    ) -> Result<AsyncTaskReceipt> {
        self.adapter
            .execute_async(&self.tool_name, params, "default_session")
            .await
    }

    async fn check_status(&self, task_id: &AsyncTaskId) -> Result<AsyncTaskStatus> {
        self.adapter.check_status(&self.tool_name, task_id).await
    }

    async fn cancel(&self, task_id: &AsyncTaskId) -> Result<bool> {
        self.adapter.cancel(&self.tool_name, task_id).await
    }

    fn status_check_tool_name(&self) -> String {
        format!("{}_status", self.tool_name)
    }

    fn estimated_async_duration_secs(&self, _params: &Value) -> Option<u64> {
        // Could be enhanced to read from extension metadata
        None
    }
}

/// Factory for creating async tools from extensions
pub struct AsyncExtensionToolFactory {
    adapter: ExtensionAsyncAdapter,
}

impl AsyncExtensionToolFactory {
    /// Create a new factory
    #[must_use] 
    pub fn new(adapter: ExtensionAsyncAdapter) -> Self {
        Self { adapter }
    }

    /// Create an async tool for the given tool name
    ///
    /// # Arguments
    /// * `tool_name` - The name of the tool
    /// * `description` - Tool description for LLM
    /// * `parameters` - JSON schema for tool parameters
    ///
    /// # Returns
    /// A boxed `UnifiedAsyncTool` that wraps the extension
    pub fn create_tool(
        &self,
        tool_name: impl Into<String>,
        description: impl Into<String>,
        parameters: Value,
    ) -> BoxedAsyncTool {
        let tool =
            ExtensionAsyncTool::new(self.adapter.clone(), tool_name, description, parameters);
        Box::new(tool)
    }
}

/// Registry for managing async-capable tools from multiple sources
pub struct AsyncToolRegistry {
    tools: std::sync::RwLock<std::collections::HashMap<String, BoxedAsyncTool>>,
}

impl AsyncToolRegistry {
    /// Create a new empty registry
    #[must_use] 
    pub fn new() -> Self {
        Self {
            tools: std::sync::RwLock::new(std::collections::HashMap::new()),
        }
    }

    /// Register a tool
    pub fn register(&self, tool: BoxedAsyncTool) {
        let mut tools = self.tools.write().unwrap();
        tools.insert(tool.name().to_string(), tool);
    }

    /// Get a tool by name
    pub fn get(&self, name: &str) -> Option<BoxedAsyncTool> {
        let tools = self.tools.read().unwrap();
        tools.get(name).map(|_t| {
            // Clone the boxed trait object - this requires the trait to be clonable
            // For now, we'll use Arc internally in a future refactor
            todo!("Cloneable async tools or use Arc<dyn UnifiedAsyncTool>")
        })
    }

    /// List all registered tool names
    pub fn list(&self) -> Vec<String> {
        let tools = self.tools.read().unwrap();
        tools.keys().cloned().collect()
    }

    /// Check if a tool supports async execution
    pub fn supports_async(&self, name: &str) -> bool {
        let tools = self.tools.read().unwrap();
        tools.get(name).is_some_and(|t| t.supports_async())
    }
}

impl Default for AsyncToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Extension trait for `ExtensionAsyncAdapter` to add `UnifiedAsyncTool` integration
pub trait ExtensionAsyncAdapterExt {
    /// Wrap this adapter as a `UnifiedAsyncTool` for the given tool
    fn as_async_tool(
        &self,
        tool_name: impl Into<String>,
        description: impl Into<String>,
        parameters: Value,
    ) -> BoxedAsyncTool;
}

impl ExtensionAsyncAdapterExt for ExtensionAsyncAdapter {
    fn as_async_tool(
        &self,
        tool_name: impl Into<String>,
        description: impl Into<String>,
        parameters: Value,
    ) -> BoxedAsyncTool {
        let tool = ExtensionAsyncTool::new(self.clone(), tool_name, description, parameters);
        Box::new(tool)
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
        assert!(tool.supports_async());
    }

    #[test]
    fn test_async_tool_registry() {
        let registry = AsyncToolRegistry::new();

        // Initially empty
        assert!(registry.list().is_empty());

        // TODO: Test registration once we have cloneable tools
    }
}
