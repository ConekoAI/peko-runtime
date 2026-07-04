//! Built-in Tool Adapter
//!
//! Registers native Tool trait implementations with `ExtensionCore`.
//!
//! Unlike `UniversalToolAdapter` which spawns external processes,
//! this adapter uses direct trait calls for minimal overhead.
//!
//! ## Usage
//! ```rust,ignore
//! let bash = Arc::new(BashTool::new());
//! BuiltinToolAdapter::register_tool(&core, bash).await?;
//! ```

use crate::extensions::framework::core::{ExtensionCore, HookContext, HookHandler, HookPoint};
use crate::extensions::framework::types::{ExtensionId, HookOutput, ToolMetadata, ToolSource};
use crate::extensions::framework::HookResult;
use crate::tools::Tool;
use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for built-in tool registration
#[derive(Debug, Clone)]
pub struct BuiltinToolRegistrarConfig {
    /// Workspace directory for tools
    pub workspace_dir: PathBuf,
    /// Enable granular filesystem tools (`Read`, `Write`, `Glob`, `Grep`, `Edit`)
    pub enable_granular_fs: bool,
    /// Enable write tools (`Write`, `Edit`)
    pub enable_granular_write: bool,
    /// Enable shell tool
    pub enable_shell: bool,
    /// Enable session introspection tools
    pub enable_session_tools: bool,
    /// Enable cron tool
    pub enable_cron: bool,
    /// Enable async execution control tools (AsyncSpawn, AsyncOutput, AsyncStop,
    /// AsyncStatus, AsyncList)
    pub enable_async_tools: bool,
    /// Enable planning todo tools (TaskCreate, TaskGet, TaskList, TaskUpdate)
    pub enable_task_tools: bool,
    /// Path to cron database
    pub cron_db_path: Option<PathBuf>,
    /// Instance ID for cron persistence
    pub instance_id: Option<String>,
    /// List of disabled tool names
    pub disabled_tools: Vec<String>,
}

impl Default for BuiltinToolRegistrarConfig {
    fn default() -> Self {
        Self {
            workspace_dir: PathBuf::from("."),
            enable_granular_fs: true,
            enable_granular_write: true,
            enable_shell: true,
            enable_session_tools: true,
            enable_cron: true,
            enable_async_tools: true,
            enable_task_tools: true,
            cron_db_path: None,
            instance_id: None,
            disabled_tools: Vec::new(),
        }
    }
}

// ============================================================================
// Adapter
// ============================================================================

/// Adapter for registering built-in tools with `ExtensionCore`
#[derive(Debug)]
pub struct BuiltinToolAdapter;

impl BuiltinToolAdapter {
    /// Register a built-in tool with the `ExtensionCore`
    ///
    /// Uses the unified tool registry (ADR-018b) for single source of truth.
    /// `ExtensionCore::register_tool()` auto-generates all companion hooks
    /// (prompt, async, status, cancel) from the metadata; this method only
    /// supplies the execution handler.
    ///
    /// In addition to the hook-based registration, this method also
    /// populates the `ExtensionCore` side-table of `Arc<dyn Tool>`
    /// instances keyed by tool name. The side-table is what
    /// `ExtensionCore::get_tool` reads from so direct callers (e.g.,
    /// `AsyncSpawnTool`) can obtain an `Arc<dyn Tool>` without going
    /// through the hook layer.
    pub async fn register_tool(core: &ExtensionCore, tool: Arc<dyn Tool>) -> Result<()> {
        let tool_name = tool.name().to_string();
        let ext_id = ExtensionId::new(format!("builtin:tool:{tool_name}"));

        // Create tool metadata for unified registry
        let metadata = ToolMetadata::new(
            tool_name.clone(),
            tool.description(),
            tool.parameters(),
            ToolSource::BuiltIn,
        );

        // Side-table: keep a clone of the Arc<dyn Tool> for direct
        // invocation paths (AsyncSpawnTool calls core.get_tool). Clone
        // BEFORE moving the original into the execute handler below.
        core.insert_tool_instance(tool_name.clone(), tool.clone())
            .await;

        // Create execution handler (consumes the original `tool` Arc).
        let exec_handler = Arc::new(BuiltinExecuteHandler::new(tool));

        // Register with unified registry (auto-generates all companion hooks)
        core.register_tool(metadata, exec_handler, &ext_id).await?;

        Ok(())
    }

    /// Register multiple tools
    pub async fn register_tools(core: &ExtensionCore, tools: Vec<Arc<dyn Tool>>) -> Result<()> {
        for tool in tools {
            Self::register_tool(core, tool).await?;
        }
        Ok(())
    }

    /// Register all enabled built-in tools with `ExtensionCore`
    ///
    /// This is the single entry point for registering built-in tools.
    /// All tools are registered as hooks in `ExtensionCore`, making them
    /// discoverable via `ToolRegister` hook and executable via `ToolExecute` hook.
    ///
    /// `AsyncSpawn` and `AsyncOutput` are **NOT** registered here. They
    /// depend on a per-agent `AsyncExecutor` and `ExtensionCore` reference,
    /// so they are registered per-agent by `register_async_spawn_tool` and
    /// `register_async_output_tool` once the agent has constructed its
    /// executor and queue.
    pub async fn register_all(
        core: &ExtensionCore,
        config: &BuiltinToolRegistrarConfig,
    ) -> Result<()> {
        Self::register_globals(core, config).await
    }

    /// Register global built-in tools.
    ///
    /// `AsyncSpawn` and `AsyncOutput` are excluded because they require
    /// per-agent wiring (an `AsyncExecutor` and an `ExtensionCore`
    /// reference). Callers that need those tools must use
    /// `register_async_spawn_tool` / `register_async_output_tool` per-agent.
    pub async fn register_globals(
        core: &ExtensionCore,
        config: &BuiltinToolRegistrarConfig,
    ) -> Result<()> {
        use crate::tools::builtin::{
            BashTool, CronCreateTool, CronDeleteTool, CronListTool, EditTool, GlobTool, GrepTool,
            ReadTool, SessionTool, WriteTool,
        };

        let disabled_set: HashSet<String> = config
            .disabled_tools
            .iter()
            .map(|s| s.to_lowercase())
            .collect();

        let workspace = config.workspace_dir.clone();

        // Shell tool (Bash)
        let bash_enabled = config.enable_shell;
        let bash_disabled = disabled_set.contains("bash");
        if bash_enabled && !bash_disabled {
            let bash = Arc::new(BashTool::new().with_workspace(&workspace));
            Self::register_tool(core, bash).await?;
        }

        // Granular filesystem tools
        if config.enable_granular_fs {
            // Read
            if !disabled_set.contains("read") {
                let tool = Arc::new(ReadTool::new().with_workspace(&workspace));
                Self::register_tool(core, tool).await?;
            }

            // Write
            if config.enable_granular_write && !disabled_set.contains("write") {
                let tool = Arc::new(WriteTool::new().with_workspace(&workspace));
                Self::register_tool(core, tool).await?;
            }

            // glob
            if !disabled_set.contains("glob") {
                let tool = Arc::new(GlobTool::new().with_workspace(&workspace));
                Self::register_tool(core, tool).await?;
            }

            // grep
            if !disabled_set.contains("grep") {
                let tool = Arc::new(GrepTool::new().with_workspace(&workspace));
                Self::register_tool(core, tool).await?;
            }

            // Edit
            if config.enable_granular_write && !disabled_set.contains("edit") {
                let tool = Arc::new(EditTool::new().with_workspace(&workspace));
                Self::register_tool(core, tool).await?;
            }
        }

        // Session introspection tool (unified)
        if config.enable_session_tools && !disabled_set.contains("session") {
            let registry = crate::tools::SessionCache::new("main");
            let tool = Arc::new(SessionTool::new(Box::new(registry)));
            Self::register_tool(core, tool).await?;
        }

        // Cron family for scheduled jobs
        let cron_disabled = disabled_set.contains("cron");
        if config.enable_cron {
            if !cron_disabled && !disabled_set.contains("croncreate") {
                Self::register_tool(core, Arc::new(CronCreateTool::new())).await?;
            }
            if !cron_disabled && !disabled_set.contains("crondelete") {
                Self::register_tool(core, Arc::new(CronDeleteTool::new())).await?;
            }
            if !cron_disabled && !disabled_set.contains("cronlist") {
                Self::register_tool(core, Arc::new(CronListTool::new())).await?;
            }
        }

        // Async task control family (global members)
        //
        // AsyncStatus, AsyncList, and AsyncStop are intentionally NOT registered
        // globally. The previous global implementation enumerated tasks across
        // every agent's registry, which broke session isolation (issue from
        // parity audit). Each agent now registers its own copy bound to its
        // AsyncExecutor's registry inside `Agent::rebuild_async_tools`, so
        // introspection is scoped to the calling agent's own tasks.
        //
        // AsyncSpawn and AsyncOutput are also per-agent for the same reason:
        // they depend on per-agent state (AsyncExecutor + ExtensionCore for
        // spawn-side lookups).

        Ok(())
    }

    /// Register `AsyncSpawn` with per-agent wiring.
    ///
    /// `AsyncSpawn` requires an `AsyncExecutor` and an `ExtensionCore`
    /// reference (so it can look up the target tool by name and read the
    /// current session key). Each agent calls this once during initialization
    /// so spawned tasks land in its own completion queue.
    pub async fn register_async_spawn_tool(
        core: &ExtensionCore,
        tool: Arc<crate::tools::builtin::AsyncSpawnTool>,
    ) -> Result<()> {
        Self::register_tool(core, tool).await
    }

    /// Register `AsyncOutput` with per-agent wiring.
    ///
    /// `AsyncOutput` requires an `AsyncExecutor` for blocking reads.
    pub async fn register_async_output_tool(
        core: &ExtensionCore,
        tool: Arc<crate::tools::builtin::AsyncOutputTool>,
    ) -> Result<()> {
        Self::register_tool(core, tool).await
    }

    /// Get list of globally-registered built-in tool names.
    ///
    /// These tools are registered once at daemon startup by
    /// `BuiltinToolAdapter::register_all()` and are shared across all agents.
    #[must_use]
    pub fn global_tool_names() -> Vec<&'static str> {
        crate::extensions::framework::adapters::builtin_tools::GLOBAL_TOOL_NAMES.to_vec()
    }

    /// Get list of agent-specific built-in tool names.
    ///
    /// These tools require agent-specific runtime dependencies
    /// (e.g. `SubagentExecutor`, caller identity) and are registered
    /// per-agent in `Agent::init_builtins_async()`.
    #[must_use]
    pub fn agent_specific_tool_names() -> Vec<&'static str> {
        crate::extensions::framework::adapters::builtin_tools::AGENT_SPECIFIC_TOOL_NAMES.to_vec()
    }

    /// Get list of ALL built-in tool names (global + agent-specific).
    #[must_use]
    pub fn all_tool_names() -> Vec<&'static str> {
        crate::extensions::framework::adapters::builtin_tools::all_tool_names()
    }

    /// Check if a tool name is a built-in tool (global or agent-specific).
    #[must_use]
    pub fn is_builtin(name: &str) -> bool {
        crate::extensions::framework::adapters::builtin_tools::is_builtin_tool(name)
    }

    /// Check if a tool name is an agent-specific built-in (registered per-agent).
    #[must_use]
    pub fn is_agent_specific_builtin(name: &str) -> bool {
        crate::extensions::framework::adapters::builtin_tools::is_agent_specific_builtin_tool(name)
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
        let tool = self.tool.clone();
        let tool_name = tool.name().to_string();
        let tool_name_for_preproc = tool_name.clone();
        let tool_name_for_ctx = tool_name.clone();

        let exec_config = crate::extensions::framework::services::ToolExecutionConfig::with_schema(
            self.tool.parameters(),
        );

        let runtime_ctx = ctx
            .get_state::<crate::extensions::framework::types::ToolRuntimeContext>("tool_context")
            .cloned()
            .unwrap_or_default();

        ctx.services
            .async_router()
            .execute_from_hook(
                &ctx,
                &tool_name,
                &exec_config,
                Some(
                    move |params: &mut serde_json::Value, workspace: Option<&str>| {
                        if let Some(obj) = params.as_object_mut() {
                            // Subagent spawn inherently takes longer than simple tools because
                            // the subagent runs a full agentic loop with its own LLM calls.
                            // Inject a longer default timeout for blocking Agent if none
                            // is provided by the caller.
                            if tool_name_for_preproc == "Agent" && !obj.contains_key("_timeout") {
                                obj.insert(
                                    "_timeout".to_string(),
                                    serde_json::Value::Number(300.into()),
                                );
                            }

                            // Inject agent workspace into tool parameters for filesystem tools.
                            if let Some(ws) = workspace {
                                match tool_name_for_preproc.as_str() {
                                    "Glob" => {
                                        if !obj.contains_key("directory") {
                                            obj.insert(
                                                "directory".to_string(),
                                                serde_json::Value::String(ws.to_string()),
                                            );
                                        }
                                    }
                                    "Grep" => {
                                        if !obj.contains_key("path") {
                                            obj.insert(
                                                "path".to_string(),
                                                serde_json::Value::String(ws.to_string()),
                                            );
                                        }
                                    }
                                    "Bash" => {
                                        if !obj.contains_key("cwd") {
                                            obj.insert(
                                                "cwd".to_string(),
                                                serde_json::Value::String(ws.to_string()),
                                            );
                                        }
                                    }
                                    "Write" | "Edit" => {
                                        if let Some(path_val) = obj.get("file_path") {
                                            if let Some(path_str) = path_val.as_str() {
                                                let path_buf = std::path::PathBuf::from(path_str);
                                                if !path_buf.is_absolute() {
                                                    let resolved =
                                                        std::path::PathBuf::from(ws).join(path_str);
                                                    obj.insert(
                                                        "file_path".to_string(),
                                                        serde_json::Value::String(
                                                            resolved.to_string_lossy().to_string(),
                                                        ),
                                                    );
                                                }
                                            }
                                        }
                                    }
                                    "Read" => {
                                        if let Some(path_val) = obj.get("file_path") {
                                            if let Some(path_str) = path_val.as_str() {
                                                let path_buf = std::path::PathBuf::from(path_str);
                                                if !path_buf.is_absolute() {
                                                    let resolved =
                                                        std::path::PathBuf::from(ws).join(path_str);
                                                    obj.insert(
                                                        "file_path".to_string(),
                                                        serde_json::Value::String(
                                                            resolved.to_string_lossy().to_string(),
                                                        ),
                                                    );
                                                }
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                    },
                ),
                move |p| {
                    let tool = tool.clone();
                    let tool_ctx = crate::tools::ToolContext::for_hook_run(
                        "hook_run",
                        "hook",
                        &tool_name_for_ctx,
                    )
                    .with_agent_id(runtime_ctx.agent_id.clone().unwrap_or_default())
                    .with_session_id(runtime_ctx.session_id.clone().unwrap_or_default())
                    .with_workspace(runtime_ctx.workspace.clone().unwrap_or_default())
                    .with_principal_id(runtime_ctx.principal_id.clone().unwrap_or_default())
                    .with_principal_name(runtime_ctx.principal_name.clone().unwrap_or_default());
                    async move {
                        // Use execute_with_context so tools receive session/agent
                        // context injected by the extension framework.
                        tool.execute_with_context(p, &tool_ctx).await
                    }
                },
            )
            .await
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

    #[test]
    fn test_is_builtin() {
        // Global tools
        assert!(BuiltinToolAdapter::is_builtin("Bash"));
        assert!(BuiltinToolAdapter::is_builtin("Read"));
        assert!(BuiltinToolAdapter::is_builtin("BASH")); // case insensitive
        assert!(BuiltinToolAdapter::is_builtin("AsyncStatus"));
        assert!(BuiltinToolAdapter::is_builtin("AsyncList"));
        assert!(BuiltinToolAdapter::is_builtin("AsyncStop"));
        // Agent-specific tools
        assert!(BuiltinToolAdapter::is_builtin("Agent"));
        assert!(BuiltinToolAdapter::is_builtin("principal_send"));
        assert!(BuiltinToolAdapter::is_builtin("AsyncSpawn"));
        assert!(BuiltinToolAdapter::is_builtin("AsyncOutput"));
        assert!(BuiltinToolAdapter::is_builtin("PRINCIPAL_SEND")); // case insensitive
                                                                   // Unknown
        assert!(!BuiltinToolAdapter::is_builtin("unknown_tool"));
    }

    #[test]
    fn test_all_tool_names() {
        let names = BuiltinToolAdapter::all_tool_names();
        assert!(names.contains(&"Bash"));
        assert!(names.contains(&"Read"));
        assert!(names.contains(&"Agent"));
        assert!(names.contains(&"principal_send"));
        assert!(names.contains(&"AsyncSpawn"));
        assert!(names.contains(&"AsyncOutput"));
        assert!(names.contains(&"AsyncStatus"));
        assert!(names.contains(&"AsyncList"));
        assert!(names.contains(&"AsyncStop"));
    }

    #[test]
    fn test_global_tool_names() {
        let names = BuiltinToolAdapter::global_tool_names();
        assert!(names.contains(&"Bash"));
        assert!(names.contains(&"AsyncStatus"));
        assert!(names.contains(&"AsyncList"));
        assert!(names.contains(&"AsyncStop"));
        assert!(!names.contains(&"AsyncSpawn")); // agent-specific, not global
        assert!(!names.contains(&"AsyncOutput")); // agent-specific, not global
        assert!(!names.contains(&"Agent")); // agent-specific, not global
        assert!(!names.contains(&"principal_send")); // agent-specific, not global
    }

    #[test]
    fn test_agent_specific_tool_names() {
        let names = BuiltinToolAdapter::agent_specific_tool_names();
        assert!(names.contains(&"Agent"));
        assert!(names.contains(&"principal_send"));
        assert!(names.contains(&"AsyncSpawn"));
        assert!(names.contains(&"AsyncOutput"));
        assert!(!names.contains(&"Bash")); // global, not agent-specific
    }

    #[test]
    fn test_is_agent_specific_builtin() {
        assert!(BuiltinToolAdapter::is_agent_specific_builtin("Agent"));
        assert!(BuiltinToolAdapter::is_agent_specific_builtin(
            "principal_send"
        ));
        assert!(BuiltinToolAdapter::is_agent_specific_builtin("AGENT")); // case insensitive
        assert!(!BuiltinToolAdapter::is_agent_specific_builtin("Bash"));
        assert!(!BuiltinToolAdapter::is_agent_specific_builtin("session"));
        assert!(!BuiltinToolAdapter::is_agent_specific_builtin("unknown"));
    }
}
