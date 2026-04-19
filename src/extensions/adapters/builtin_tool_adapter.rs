//! Built-in Tool Adapter
//!
//! Registers native Tool trait implementations with `ExtensionCore`.
//!
//! Unlike `UniversalToolAdapter` which spawns external processes,
//! this adapter uses direct trait calls for minimal overhead.
//!
//! ## Usage
//! ```rust,ignore
//! let shell = Arc::new(ShellTool::new());
//! BuiltinToolAdapter::register_tool(&core, shell).await?;
//! ```

use crate::extensions::core::{ExtensionCore, HookContext, HookHandler, HookPoint};
use crate::extensions::services::ReservedParamsConfig;
use crate::extensions::types::{ExtensionId, HookOutput, ToolMetadata, ToolSource};
use crate::extensions::HookResult;
use crate::tools::Tool;
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;

/// Adapter for registering built-in tools with `ExtensionCore`
#[derive(Debug)]
pub struct BuiltinToolAdapter;

impl BuiltinToolAdapter {
    /// Register a built-in tool with the `ExtensionCore`
    ///
    /// Uses the unified tool registry (ADR-018b) for single source of truth.
    /// Registers:
    /// - Tool metadata via `register_tool()` (includes name, description, parameters, source)
    /// - Execution handler for direct trait calls
    /// - Prompt section handler for system prompt
    pub async fn register_tool(core: &ExtensionCore, tool: Arc<dyn Tool>) -> Result<()> {
        let tool_name = tool.name().to_string();
        let ext_id = ExtensionId::new(format!("builtin:tool:{tool_name}"));

        // Create tool metadata for unified registry
        let metadata = ToolMetadata {
            name: tool_name.clone(),
            description: tool.description(), // Use LLM-optimized description
            parameters: tool.parameters(),
            source: ToolSource::BuiltIn,
            reserved_params: ReservedParamsConfig::new(), // Built-in tools get default reserved params
        };

        // Create execution handler
        let exec_handler = Arc::new(BuiltinExecuteHandler::new(tool.clone()));

        // 1. Register tool in unified registry (metadata + execution handler)
        core.register_tool(metadata, exec_handler, &ext_id).await?;

        // 2. Register prompt section (still via hook for prompt injection)
        core.register_hook(
            HookPoint::PromptSystemSection {
                section: "tools".to_string(),
                priority: 100, // Built-ins get standard priority
            },
            Arc::new(BuiltinPromptHandler::new(tool)),
            &ext_id,
        )
        .await?;

        Ok(())
    }

    /// Register multiple tools
    pub async fn register_tools(core: &ExtensionCore, tools: Vec<Arc<dyn Tool>>) -> Result<()> {
        for tool in tools {
            Self::register_tool(core, tool).await?;
        }
        Ok(())
    }
}

// ============================================================================
// Hook Handlers
// ============================================================================

/// Handler for `ToolExecute` hook - DIRECT execution
pub struct BuiltinExecuteHandler {
    tool: Arc<dyn Tool>,
}

impl std::fmt::Debug for BuiltinExecuteHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BuiltinExecuteHandler")
            .field("tool_name", &self.tool.name())
            .finish()
    }
}

impl BuiltinExecuteHandler {
    /// Create a new execution handler for a tool
    pub fn new(tool: Arc<dyn Tool>) -> Self {
        Self { tool }
    }
}

#[async_trait]
impl HookHandler for BuiltinExecuteHandler {
    async fn handle(&self, ctx: HookContext) -> HookResult {
        // Extract tool call parameters
        let (tool_name, params, workspace) = match ctx.as_tool_call() {
            Some((name, params, workspace)) => (name, params, workspace),
            None => return HookResult::PassThrough,
        };

        // Verify this handler is for the right tool
        if tool_name != self.tool.name() {
            return HookResult::PassThrough;
        }

        // ADR-018a: Use AsyncExecutionRouter for unified execution
        // This provides:
        // - _async parameter extraction and routing
        // - Panic isolation via ToolExecutionService
        // - Timeout enforcement
        // - Consistent context injection

        let exec_service = ctx.services.tool_execution();
        let async_router = ctx.services.async_router();

        // Build execution context from hook input, falling back to hook state
        let tool_ctx = match ctx.as_tool_context() {
            Some(tc) => crate::extensions::services::ToolExecutionContext::new(
                tc.agent_id.clone().unwrap_or_else(|| "unknown".to_string()),
                tc.session_id
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
                tc.run_id.clone(),
            )
            .with_workspace(tc.workspace.clone().unwrap_or_else(|| ".".to_string())),
            None => {
                let ctx = crate::extensions::services::ToolExecutionContext::new(
                    "unknown", "unknown", "unknown",
                );
                // Use workspace from HookInput::ToolCall if available
                match workspace {
                    Some(ws) => ctx.with_workspace(ws),
                    None => ctx,
                }
            }
        };

        // Create execution config (built-in tools have no reserved params)
        let exec_config =
            crate::extensions::services::ToolExecutionConfig::with_schema(self.tool.parameters());

        // Clone params for mutation (AsyncExecutionRouter extracts _async, etc.)
        let mut params_mut = params.clone();

        // Inject agent workspace into tool parameters for filesystem tools.
        // Built-in tools are created once at daemon startup with a global workspace,
        // but each agent has its own workspace. We inject the agent's workspace
        // so the tool searches in the correct location.
        if let Some(ref ws) = workspace {
            if let Some(obj) = params_mut.as_object_mut() {
                match tool_name {
                    "glob" => {
                        if !obj.contains_key("directory") {
                            obj.insert("directory".to_string(), serde_json::Value::String(ws.to_string()));
                        }
                    }
                    "grep" => {
                        if !obj.contains_key("path") {
                            obj.insert("path".to_string(), serde_json::Value::String(ws.to_string()));
                        }
                    }
                    "shell" => {
                        if !obj.contains_key("cwd") {
                            obj.insert("cwd".to_string(), serde_json::Value::String(ws.to_string()));
                        }
                    }
                    _ => {}
                }
            }
        }

        // Clone tool for the closure (needed for 'static bound)
        let tool = self.tool.clone();
        let tool_name = tool.name().to_string();

        // Route execution through AsyncExecutionRouter
        let result = async_router
            .route(
                &tool_name,
                &mut params_mut,
                exec_service,
                &tool_ctx,
                &exec_config,
                move |p| {
                    let tool = tool.clone();
                    async move { tool.execute(p).await }
                },
            )
            .await;

        match result {
            Ok(value) => HookResult::Continue(HookOutput::Json(value)),
            Err(e) => HookResult::Error(e),
        }
    }

    fn hook_point(&self) -> HookPoint {
        HookPoint::ToolExecute {
            tool_name: self.tool.name().to_string(),
        }
    }

    fn priority(&self) -> i32 {
        100 // Standard priority
    }

    fn name(&self) -> String {
        format!("BuiltinExecute({})", self.tool.name())
    }
}

/// Handler for `PromptSystemSection` hook
pub struct BuiltinPromptHandler {
    tool: Arc<dyn Tool>,
}

impl std::fmt::Debug for BuiltinPromptHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BuiltinPromptHandler")
            .field("tool_name", &self.tool.name())
            .finish()
    }
}

impl BuiltinPromptHandler {
    /// Create a new prompt handler for a tool
    pub fn new(tool: Arc<dyn Tool>) -> Self {
        Self { tool }
    }
}

#[async_trait]
impl HookHandler for BuiltinPromptHandler {
    async fn handle(&self, _ctx: HookContext) -> HookResult {
        let description = self.tool.description();
        let text = format!("### {}\n\n{}", self.tool.name(), description);

        HookResult::Continue(HookOutput::Text(text))
    }

    fn hook_point(&self) -> HookPoint {
        HookPoint::PromptSystemSection {
            section: "tools".to_string(),
            priority: 100,
        }
    }

    fn priority(&self) -> i32 {
        100
    }

    fn name(&self) -> String {
        format!("BuiltinPrompt({})", self.tool.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Mock tool for testing
    struct MockTool {
        name: String,
    }

    impl std::fmt::Debug for MockTool {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("MockTool")
                .field("name", &self.name)
                .finish()
        }
    }

    #[async_trait]
    impl Tool for MockTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> String {
            "A mock tool for testing".to_string()
        }

        fn parameters(&self) -> serde_json::Value {
            json!({
                "type": "object",
                "properties": {}
            })
        }

        async fn execute(&self, _params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
            Ok(json!({"success": true}))
        }
    }

    #[test]
    fn test_builtin_tool_adapter_register() {
        // Note: This test would require async runtime and ExtensionCore
        // For now, just verify the types compile correctly
        let _tool: Arc<dyn Tool> = Arc::new(MockTool {
            name: "mock".to_string(),
        });
    }

    #[test]
    fn test_handler_names() {
        let tool: Arc<dyn Tool> = Arc::new(MockTool {
            name: "test_tool".to_string(),
        });

        let exec_handler = BuiltinExecuteHandler::new(tool.clone());
        assert_eq!(exec_handler.name(), "BuiltinExecute(test_tool)");
        assert_eq!(exec_handler.priority(), 100);

        let prompt_handler = BuiltinPromptHandler::new(tool);
        assert_eq!(prompt_handler.name(), "BuiltinPrompt(test_tool)");
    }
}
