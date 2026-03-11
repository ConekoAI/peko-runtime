//! MCP Tool Proxy
//!
//! Adapts MCP tools to Pekobot's Tool trait, allowing MCP tools to be used
//! seamlessly by the agent system.

use crate::mcp::{
    manager::McpManager,
    types::{CallToolResult, Tool as McpTool, ToolResultContent},
};
use crate::tools::{Tool, ToolContext, ToolError};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, error, trace, warn};

/// An MCP tool wrapped as a Pekobot Tool
pub struct McpToolProxy {
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

        Self {
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
        Self {
            server_name,
            tool,
            manager,
            estimated_duration_ms,
        }
    }

    /// Get the server name
    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    /// Get the original MCP tool definition
    pub fn mcp_tool(&self) -> &McpTool {
        &self.tool
    }

    /// Convert MCP tool result to a JSON value for Pekobot
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
}

#[async_trait]
impl Tool for McpToolProxy {
    fn name(&self) -> &str {
        &self.tool.name
    }

    fn description(&self) -> &str {
        &self.tool.description
    }

    fn llm_description(&self) -> String {
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
        trace!(
            "Executing MCP tool '{}' on server '{}'",
            self.tool.name,
            self.server_name
        );

        // Get the manager
        let manager = self.manager.read().await;

        // Try to call the tool, starting the server if needed
        let result = match manager
            .call_tool(&self.server_name, &self.tool.name, params.clone())
            .await
        {
            Ok(result) => result,
            Err(crate::mcp::manager::ManagerError::ServerNotRunning(_)) => {
                // Server not running, try to start it
                drop(manager); // Drop read lock before starting server
                let mut manager = self.manager.write().await;
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
                    .map_err(|e| anyhow::anyhow!("MCP tool error: {}", e))?
            }
            Err(e) => {
                return Err(anyhow::anyhow!("MCP tool error: {}", e));
            }
        };

        debug!(
            "MCP tool '{}' completed (is_error: {})",
            self.tool.name, result.is_error
        );

        Ok(self.convert_result(result))
    }

    async fn execute_with_context(
        &self,
        params: serde_json::Value,
        ctx: &ToolContext,
    ) -> anyhow::Result<serde_json::Value> {
        use std::time::Instant;

        // Check abort before starting
        if ctx.is_aborted() {
            return Err(ToolError::Aborted.into());
        }

        // Check timeout before starting
        let start_time = Instant::now();
        ctx.check_timeout(start_time)?;

        // Report start status
        ctx.report_status(format!(
            "Starting {} (via {})",
            self.tool.name, self.server_name
        ))
        .await;

        // Execute the tool
        let result = self.execute(params).await;

        // Check abort after completion
        if ctx.is_aborted() {
            return Err(ToolError::Aborted.into());
        }

        // Check timeout after completion
        ctx.check_timeout(start_time)?;

        // Report completion status
        match &result {
            Ok(_) => {
                ctx.report_status(format!(
                    "Completed {} (via {})",
                    self.tool.name, self.server_name
                ))
                .await;
            }
            Err(e) => {
                ctx.report_status(format!(
                    "Failed {} (via {}): {}",
                    self.tool.name, self.server_name, e
                ))
                .await;
            }
        }

        result
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

/// Estimate tool duration based on name heuristics
fn estimate_tool_duration(name: &str) -> u64 {
    let name_lower = name.to_lowercase();

    // Fast operations (milliseconds)
    if name_lower.contains("read")
        || name_lower.contains("get")
        || name_lower.contains("list")
        || name_lower.contains("search")
        || name_lower.contains("find")
    {
        return 500; // 500ms
    }

    // Medium operations (seconds)
    if name_lower.contains("write")
        || name_lower.contains("create")
        || name_lower.contains("update")
        || name_lower.contains("delete")
        || name_lower.contains("copy")
        || name_lower.contains("move")
    {
        return 2000; // 2s
    }

    // Slow operations (network/external calls)
    if name_lower.contains("fetch")
        || name_lower.contains("download")
        || name_lower.contains("upload")
        || name_lower.contains("browser")
        || name_lower.contains("http")
        || name_lower.contains("request")
    {
        return 5000; // 5s
    }

    // Very slow operations (builds, long processes)
    if name_lower.contains("build")
        || name_lower.contains("compile")
        || name_lower.contains("test")
        || name_lower.contains("run")
        || name_lower.contains("exec")
        || name_lower.contains("shell")
    {
        return 30000; // 30s
    }

    // Default
    1000 // 1s
}

/// Create tool proxies for all tools from all running MCP servers
pub async fn create_tool_proxies(manager: Arc<RwLock<McpManager>>) -> Vec<Arc<dyn Tool>> {
    let manager_guard = manager.read().await;
    let tools = manager_guard.list_all_tools().await;
    drop(manager_guard);

    let mut proxies: Vec<Arc<dyn Tool>> = Vec::new();

    for (server_name, tool) in tools {
        trace!(
            "Creating tool proxy for '{}' from server '{}'",
            tool.name,
            server_name
        );
        let proxy = McpToolProxy::new(server_name, tool, manager.clone());
        proxies.push(Arc::new(proxy));
    }

    debug!("Created {} MCP tool proxies", proxies.len());
    proxies
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
    use crate::mcp::config::{McpConfig, McpServerConfig};

    #[test]
    fn test_estimate_tool_duration() {
        assert_eq!(estimate_tool_duration("read_file"), 500);
        assert_eq!(estimate_tool_duration("search_code"), 500);
        assert_eq!(estimate_tool_duration("write_file"), 2000);
        assert_eq!(estimate_tool_duration("fetch_url"), 5000);
        assert_eq!(estimate_tool_duration("build_project"), 30000);
        assert_eq!(estimate_tool_duration("unknown"), 1000);
    }

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

        assert_eq!(proxy.name(), "test_tool");
        assert_eq!(proxy.server_name(), "test_server");
        assert!(proxy.llm_description().contains("test_server"));
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
            content: vec![ToolResultContent::Text(crate::mcp::types::TextContent {
                text: "Hello".to_string(),
            })],
            is_error: false,
        };

        let json = proxy.convert_result(result);
        assert_eq!(json["success"], true);
        assert_eq!(json["is_error"], false);
        assert_eq!(json["contents"].as_array().unwrap().len(), 1);
    }
}
