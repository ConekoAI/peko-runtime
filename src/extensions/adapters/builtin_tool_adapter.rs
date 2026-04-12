//! Built-in Tool Adapter
//!
//! Registers native Tool trait implementations with ExtensionCore.
//!
//! Unlike UniversalToolAdapter which spawns external processes,
//! this adapter uses direct trait calls for minimal overhead.
//!
//! ## Usage
//! ```rust,ignore
//! let shell = Arc::new(ShellTool::new());
//! BuiltinToolAdapter::register_tool(&core, shell).await?;
//! ```

use crate::extensions::core::{ExtensionCore, HookContext, HookHandler, HookPoint};
use crate::extensions::types::{ExtensionId, HookOutput, ToolMetadata, ToolSource};
use crate::extensions::services::ReservedParamsConfig;
use crate::extensions::HookResult;
use crate::tools::Tool;
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;

/// Adapter for registering built-in tools with ExtensionCore
#[derive(Debug)]
pub struct BuiltinToolAdapter;

impl BuiltinToolAdapter {
    /// Register a built-in tool with the ExtensionCore
    ///
    /// Uses the unified tool registry (ADR-018b) for single source of truth.
    /// Registers:
    /// - Tool metadata via `register_tool()` (includes name, description, parameters, source)
    /// - Execution handler for direct trait calls
    /// - Prompt section handler for system prompt
    pub async fn register_tool(core: &ExtensionCore, tool: Arc<dyn Tool>) -> Result<()> {
        let tool_name = tool.name().to_string();
        let ext_id = ExtensionId::new(&format!("builtin:{}", tool_name));

        // Create tool metadata for unified registry
        let metadata = ToolMetadata {
            name: tool_name.clone(),
            description: tool.llm_description(), // Use LLM-optimized description
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

/// Handler for ToolRegister hook
pub struct BuiltinRegisterHandler {
    tool: Arc<dyn Tool>,
}

impl std::fmt::Debug for BuiltinRegisterHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BuiltinRegisterHandler")
            .field("tool_name", &self.tool.name())
            .finish()
    }
}

impl BuiltinRegisterHandler {
    /// Create a new registration handler for a tool
    pub fn new(tool: Arc<dyn Tool>) -> Self {
        Self { tool }
    }
}

#[async_trait]
impl HookHandler for BuiltinRegisterHandler {
    async fn handle(&self, _ctx: HookContext) -> HookResult {
        use crate::providers::ToolDefinition;

        let tool_def = ToolDefinition {
            name: self.tool.name().to_string(),
            description: self.tool.llm_description(), // Use LLM-optimized description
            parameters: self.tool.parameters(),
        };

        HookResult::Continue(HookOutput::Tool(tool_def))
    }

    fn hook_point(&self) -> HookPoint {
        HookPoint::ToolRegister
    }

    fn name(&self) -> String {
        format!("BuiltinRegister({})", self.tool.name())
    }
}

/// Handler for ToolExecute hook - DIRECT execution
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
        let (tool_name, params) = match ctx.as_tool_call() {
            Some((name, params)) => (name, params),
            None => return HookResult::PassThrough,
        };

        // Verify this handler is for the right tool
        if tool_name != self.tool.name() {
            return HookResult::PassThrough;
        }

        // DIRECT execution via trait call
        // No process spawn, no network call, no serialization overhead
        match self.tool.execute(params.clone()).await {
            Ok(result) => HookResult::Continue(HookOutput::Json(result)),
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

/// Handler for PromptSystemSection hook
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
        let description = self.tool.llm_description();
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
            f.debug_struct("MockTool").field("name", &self.name).finish()
        }
    }

    #[async_trait]
    impl Tool for MockTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            "A mock tool for testing"
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

        let reg_handler = BuiltinRegisterHandler::new(tool.clone());
        assert_eq!(reg_handler.name(), "BuiltinRegister(test_tool)");

        let exec_handler = BuiltinExecuteHandler::new(tool.clone());
        assert_eq!(exec_handler.name(), "BuiltinExecute(test_tool)");
        assert_eq!(exec_handler.priority(), 100);

        let prompt_handler = BuiltinPromptHandler::new(tool);
        assert_eq!(prompt_handler.name(), "BuiltinPrompt(test_tool)");
    }
}
