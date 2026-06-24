//! MCP Tool Proxy
//!
//! Adapts MCP tools to Peko's Tool trait, allowing MCP tools to be used
//! seamlessly by the agent system.

use crate::extensions::framework::protocols::shared::proxy_utils::{
    estimate_tool_duration, execute_with_context_handling,
};
use crate::extensions::mcp::protocol::{
    manager::McpManager,
    types::{CallToolResult, Tool as McpTool, ToolResultContent},
};
use crate::tools::{Tool, ToolContext};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, trace};

/// An MCP tool wrapped as a Peko Tool
///
/// Uses full names with mcp: prefix (e.g., "`mcp:identity:echo_identity`")
/// for consistent identification across whitelist, hooks, and execution.
pub struct McpToolProxy {
    /// Full tool name: `mcp:{server_name}:{tool_name`}
    name: String,
    /// The server this tool belongs to
    server_name: String,
    /// The tool definition from MCP
    tool: McpTool,
    /// Reference to the MCP manager for invoking the tool
    manager: Arc<RwLock<McpManager>>,
    /// Estimated duration for this tool (can be overridden per tool)
    estimated_duration_ms: u64,
}

impl McpToolProxy {
    /// Create a new tool proxy
    ///
    /// # Arguments
    /// * `server_name` - Name of the MCP server
    /// * `tool` - The MCP tool definition
    /// * `manager` - Reference to the MCP manager
    pub fn new(server_name: String, tool: McpTool, manager: Arc<RwLock<McpManager>>) -> Self {
        // Estimate duration based on tool name heuristics
        let estimated_duration = estimate_tool_duration(&tool.name);

        // Create full name with mcp: prefix for consistent identification
        let name = format!("mcp:{}:{}", server_name, tool.name);

        Self {
            name,
            server_name,
            tool,
            manager,
            estimated_duration_ms: estimated_duration,
        }
    }

    /// Create a new tool proxy with custom estimated duration
    pub fn with_duration(
        server_name: String,
        tool: McpTool,
        manager: Arc<RwLock<McpManager>>,
        estimated_duration_ms: u64,
    ) -> Self {
        let name = format!("mcp:{}:{}", server_name, tool.name);

        Self {
            name,
            server_name,
            tool,
            manager,
            estimated_duration_ms,
        }
    }

    /// Get the server name
    #[must_use]
    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    /// Get the original MCP tool definition
    #[must_use]
    pub fn mcp_tool(&self) -> &McpTool {
        &self.tool
    }

    /// Internal method to call the tool with auto-start if needed
    ///
    /// This handles the case where the server is not running by attempting
    /// to start it and retrying the call.
    async fn call_with_auto_start(
        &self,
        params: serde_json::Value,
    ) -> anyhow::Result<CallToolResult> {
        let manager = self.manager.read().await;

        // Try to call the tool, starting the server if needed
        match manager
            .call_tool(&self.server_name, &self.tool.name, params.clone())
            .await
        {
            Ok(result) => Ok(result),
            Err(crate::extensions::mcp::protocol::manager::ManagerError::ServerNotRunning(_)) => {
                // Server not running, try to start it
                drop(manager); // Drop read lock before starting server
                let manager = self.manager.write().await;
                if let Err(e) = manager.start_server(&self.server_name).await {
                    return Err(anyhow::anyhow!(
                        "MCP server '{}' failed to start: {}",
                        self.server_name,
                        e
                    ));
                }
                drop(manager); // Drop write lock before calling tool

                // Retry the tool call
                let manager = self.manager.read().await;
                manager
                    .call_tool(&self.server_name, &self.tool.name, params)
                    .await
                    .map_err(|e| anyhow::anyhow!("MCP tool error: {e}"))
            }
            Err(e) => Err(anyhow::anyhow!("MCP tool error: {e}")),
        }
    }

    /// Convert MCP tool result to a JSON value for Peko
    fn convert_result(&self, result: CallToolResult) -> serde_json::Value {
        let contents: Vec<serde_json::Value> = result
            .content
            .into_iter()
            .map(|content| match content {
                ToolResultContent::Text(text) => {
                    json!({
                        "type": "text",
                        "text": text.text
                    })
                }
                ToolResultContent::Image(image) => {
                    json!({
                        "type": "image",
                        "data": image.data,
                        "mime_type": image.mime_type
                    })
                }
                ToolResultContent::Resource(resource) => {
                    json!({
                        "type": "resource",
                        "resource": resource.resource
                    })
                }
            })
            .collect();

        json!({
            "success": !result.is_error,
            "is_error": result.is_error,
            "contents": contents
        })
    }

    /// Execute the tool and convert the result
    async fn do_execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        trace!(
            "Executing MCP tool '{}' on server '{}'",
            self.tool.name,
            self.server_name
        );

        let result = self.call_with_auto_start(params).await?;

        debug!(
            "MCP tool '{}' completed (is_error: {})",
            self.tool.name, result.is_error
        );

        Ok(self.convert_result(result))
    }
}

#[async_trait]
impl Tool for McpToolProxy {
    fn name(&self) -> &str {
        // Return full name with mcp: prefix for consistent identification
        // Format: mcp:{server_name}:{tool_name}
        &self.name
    }

    fn description(&self) -> String {
        format!(
            "{} (via MCP server: {})",
            self.tool.description, self.server_name
        )
    }

    fn parameters(&self) -> serde_json::Value {
        // Return the MCP tool's input schema directly
        self.tool.input_schema.clone()
    }

    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        self.do_execute(params).await
    }

    async fn execute_with_context(
        &self,
        params: serde_json::Value,
        ctx: &ToolContext,
    ) -> anyhow::Result<serde_json::Value> {
        // Use the shared context handling utility to eliminate duplication
        execute_with_context_handling(ctx, &self.tool.name, Some(&self.server_name), || async {
            self.do_execute(params).await
        })
        .await
    }

    fn supports_progress(&self) -> bool {
        // MCP tools don't currently support progress callbacks
        false
    }

    fn estimated_duration_ms(&self, _params: &serde_json::Value) -> u64 {
        self.estimated_duration_ms
    }
}

impl std::fmt::Debug for McpToolProxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpToolProxy")
            .field("server_name", &self.server_name)
            .field("tool_name", &self.tool.name)
            .finish()
    }
}

/// Create tool proxies for all tools from all running MCP servers
///
/// Uses `McpManager::get_all_tools()` to ensure reserved parameter injection
/// is properly configured via `InjectableMcpToolProxy` when reserved params exist.
pub async fn create_tool_proxies(manager: Arc<RwLock<McpManager>>) -> Vec<Arc<dyn Tool>> {
    let manager_guard = manager.read().await;
    // Use get_tools() instead of list_all_tools() to get InjectableMcpToolProxy
    // when reserved parameters are configured
    let tools: Vec<Arc<dyn Tool>> = manager_guard.get_tools().await;
    drop(manager_guard);

    debug!(
        "Created {} MCP tool proxies (with reserved param support)",
        tools.len()
    );
    tools
}

/// Create a single tool proxy
pub async fn create_tool_proxy(
    server_name: String,
    tool: McpTool,
    manager: Arc<RwLock<McpManager>>,
) -> Arc<dyn Tool> {
    Arc::new(McpToolProxy::new(server_name, tool, manager))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extensions::mcp::protocol::config::McpConfig;

    #[test]
    fn test_tool_proxy_creation() {
        let config = McpConfig::default();
        let manager = Arc::new(RwLock::new(McpManager::new(config)));

        let mcp_tool = McpTool {
            name: "test_tool".to_string(),
            description: "A test tool".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "arg1": {"type": "string"}
                }
            }),
        };

        let proxy = McpToolProxy::new("test_server".to_string(), mcp_tool, manager);

        assert_eq!(proxy.name(), "mcp:test_server:test_tool");
        assert_eq!(proxy.server_name(), "test_server");
        assert!(proxy.description().contains("test_server"));
    }

    #[test]
    fn test_convert_result() {
        let config = McpConfig::default();
        let manager = Arc::new(RwLock::new(McpManager::new(config)));

        let mcp_tool = McpTool {
            name: "test".to_string(),
            description: "Test".to_string(),
            input_schema: json!({}),
        };

        let proxy = McpToolProxy::new("server".to_string(), mcp_tool, manager);

        // Test successful result
        let result = CallToolResult {
            content: vec![ToolResultContent::Text(
                crate::extensions::mcp::protocol::types::TextContent {
                    text: "Hello".to_string(),
                },
            )],
            is_error: false,
        };

        let json = proxy.convert_result(result);
        assert_eq!(json["success"], true);
        assert_eq!(json["is_error"], false);
        assert_eq!(json["contents"].as_array().unwrap().len(), 1);
    }
}
