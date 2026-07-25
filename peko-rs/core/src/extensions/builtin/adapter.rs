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
use anyhow::Result;
use async_trait::async_trait;
use peko_subject::PrincipalId;
use peko_tools_core::{Tool, ToolInterruptNotice};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
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
    /// F35 — enable the synthetic `__tool_search` stub for deferred-tool
    /// discovery. Defaults to `false` so a fresh runtime does not pay
    /// the prompt-token cost of always-on search. Per-agent override
    /// lives on [`AgentConfig::enable_tool_search`](crate::agents::AgentConfig::enable_tool_search).
    pub enable_tool_search: bool,
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
            enable_tool_search: false,
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
    pub async fn register_tool(
        core: &ExtensionCore,
        tool: Arc<dyn Tool>,
        principal_id: &PrincipalId,
    ) -> Result<()> {
        let tool_name = tool.name().to_string();
        let ext_id = ExtensionId::new(format!("builtin:tool:{tool_name}"));

        // Create tool metadata for unified registry. F34 — surface
        // `tool.exposure()` so a built-in can opt into
        // DirectModelOnly / Deferred / Hidden without subclassing.
        let metadata = ToolMetadata::new(
            tool_name.clone(),
            tool.description(),
            tool.parameters(),
            ToolSource::BuiltIn,
        )
        .with_exposure(tool.exposure());

        // Side-table: keep a clone of the Arc<dyn Tool> for direct
        // invocation paths (AsyncSpawnTool calls core.get_tool). Clone
        // BEFORE moving the original into the execute handler below.
        core.insert_tool_instance(tool_name.clone(), tool.clone())
            .await;

        // Create execution handler (consumes the original `tool` Arc).
        let exec_handler = Arc::new(BuiltinExecuteHandler::new(tool));

        // Register with unified registry (auto-generates all companion hooks)
        core.register_tool(metadata, exec_handler, &ext_id, principal_id)
            .await?;

        Ok(())
    }

    /// Register multiple tools under the same principal scope.
    pub async fn register_tools(
        core: &ExtensionCore,
        tools: Vec<Arc<dyn Tool>>,
        principal_id: &PrincipalId,
    ) -> Result<()> {
        for tool in tools {
            Self::register_tool(core, tool, principal_id).await?;
        }
        Ok(())
    }

    /// Register a single global built-in tool under
    /// [`PrincipalId::system`](peko_subject::PrincipalId::system).
    ///
    /// This is the canonical call shape for the daemon-init path: built-ins
    /// are visible to every principal and registered exactly once on the
    /// shared `ExtensionCore`.
    pub async fn register_tool_system(core: &ExtensionCore, tool: Arc<dyn Tool>) -> Result<()> {
        Self::register_tool(core, tool, PrincipalId::system()).await
    }

    /// Register multiple global built-in tools.
    pub async fn register_tools_system(
        core: &ExtensionCore,
        tools: Vec<Arc<dyn Tool>>,
    ) -> Result<()> {
        Self::register_tools(core, tools, PrincipalId::system()).await
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
            Self::register_tool_system(core, bash).await?;
        }

        // Granular filesystem tools
        if config.enable_granular_fs {
            // Read
            if !disabled_set.contains("read") {
                let tool = Arc::new(ReadTool::new().with_workspace(&workspace));
                Self::register_tool_system(core, tool).await?;
            }

            // Write
            if config.enable_granular_write && !disabled_set.contains("write") {
                let tool = Arc::new(WriteTool::new().with_workspace(&workspace));
                Self::register_tool_system(core, tool).await?;
            }

            // glob
            if !disabled_set.contains("glob") {
                let tool = Arc::new(GlobTool::new().with_workspace(&workspace));
                Self::register_tool_system(core, tool).await?;
            }

            // grep
            if !disabled_set.contains("grep") {
                let tool = Arc::new(GrepTool::new().with_workspace(&workspace));
                Self::register_tool_system(core, tool).await?;
            }

            // Edit
            if config.enable_granular_write && !disabled_set.contains("edit") {
                let tool = Arc::new(EditTool::new().with_workspace(&workspace));
                Self::register_tool_system(core, tool).await?;
            }
        }

        // Session introspection tool (unified)
        if config.enable_session_tools && !disabled_set.contains("session") {
            // Phase 10d: SessionTool takes Arc<dyn SessionRuntime>; the
            // SessionCache placeholder is provided by peko_tools_builtin.
            let registry = std::sync::Arc::new(crate::tools::SessionCache::new("main"));
            let tool = Arc::new(SessionTool::new(
                registry.as_shared() as peko_tools_builtin::session::SharedSessionRuntime
            ));
            Self::register_tool_system(core, tool).await?;
        }

        // Cron family for scheduled jobs
        let cron_disabled = disabled_set.contains("cron");
        if config.enable_cron {
            if !cron_disabled && !disabled_set.contains("croncreate") {
                Self::register_tool_system(core, Arc::new(CronCreateTool::new())).await?;
            }
            if !cron_disabled && !disabled_set.contains("crondelete") {
                Self::register_tool_system(core, Arc::new(CronDeleteTool::new())).await?;
            }
            if !cron_disabled && !disabled_set.contains("cronlist") {
                Self::register_tool_system(core, Arc::new(CronListTool::new())).await?;
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
    /// current session key). Each agent calls this once during
    /// initialization so spawned tasks land in its own completion queue.
    ///
    /// The tool is registered under the calling agent's `principal_id`
    /// rather than the system scope, so each agent gets its own
    /// (AsyncSpawn, principal_id) entry — the per-agent async family
    /// introspection scoping described above.
    pub async fn register_async_spawn_tool(
        core: &ExtensionCore,
        tool: Arc<crate::tools::builtin::AsyncSpawnTool>,
        principal_id: &PrincipalId,
    ) -> Result<()> {
        Self::register_tool(core, tool, principal_id).await
    }

    /// Register `AsyncOutput` with per-agent wiring.
    ///
    /// `AsyncOutput` requires an `AsyncExecutor` for blocking reads.
    /// Registered under the calling agent's `principal_id` (see
    /// `register_async_spawn_tool` for rationale).
    pub async fn register_async_output_tool(
        core: &ExtensionCore,
        tool: Arc<crate::tools::builtin::AsyncOutputTool>,
        principal_id: &PrincipalId,
    ) -> Result<()> {
        Self::register_tool(core, tool, principal_id).await
    }

    /// F35 — register the synthetic `__tool_search` stub for per-agent
    /// deferred-tool discovery. The tool holds a `Weak<ExtensionCore>`
    /// so it does not extend the core's lifetime past the core itself.
    ///
    /// Registered under the calling agent's `principal_id` (not the
    /// system scope) so each agent gets its own `__tool_search` instance
    /// whose execute() runs against the agent's principal scope. If a
    /// future change moves search to per-principal, only this wrapper
    /// needs to move.
    pub async fn register_tool_search_tool(
        core: &ExtensionCore,
        tool: Arc<crate::tools::builtin::ToolSearchTool>,
        principal_id: &PrincipalId,
    ) -> Result<()> {
        Self::register_tool(core, tool, principal_id).await
    }

    /// Get list of globally-registered built-in tool names.
    ///
    /// These tools are registered once at daemon startup by
    /// `BuiltinToolAdapter::register_all()` and are shared across all agents.
    #[must_use]
    pub fn global_tool_names() -> Vec<&'static str> {
        peko_principal::runtime::builtin_tools::GLOBAL_TOOL_NAMES.to_vec()
    }

    /// Get list of agent-specific built-in tool names.
    ///
    /// These tools require agent-specific runtime dependencies
    /// (e.g. `SubagentExecutor`, caller identity) and are registered
    /// per-agent in `Agent::init_builtins_async()`.
    #[must_use]
    pub fn agent_specific_tool_names() -> Vec<&'static str> {
        peko_principal::runtime::builtin_tools::AGENT_SPECIFIC_TOOL_NAMES.to_vec()
    }

    /// Get list of ALL built-in tool names (global + agent-specific).
    #[must_use]
    pub fn all_tool_names() -> Vec<&'static str> {
        peko_principal::runtime::builtin_tools::all_tool_names()
    }

    /// Check if a tool name is a built-in tool (global or agent-specific).
    #[must_use]
    pub fn is_builtin(name: &str) -> bool {
        peko_principal::runtime::builtin_tools::is_builtin_tool(name)
    }

    /// Check if a tool name is an agent-specific built-in (registered per-agent).
    #[must_use]
    pub fn is_agent_specific_builtin(name: &str) -> bool {
        peko_principal::runtime::builtin_tools::is_agent_specific_builtin_tool(name)
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
        let tool_name_for_notice = tool_name.clone();

        let exec_config = peko_extension_host::ToolExecConfig::with_schema(self.tool.parameters());

        let runtime_ctx = ctx
            .get_state::<crate::extensions::framework::types::ToolRuntimeContext>("tool_context")
            .cloned()
            .unwrap_or_default();

        // Build the ToolContext once so both the watcher task and the exec
        // closure share the same abort receiver / identity fields.
        let base_ctx =
            peko_tools_core::ToolContext::for_hook_run("hook_run", "hook", &tool_name_for_ctx)
                .with_agent_id(runtime_ctx.agent_id.clone().unwrap_or_default())
                .with_session_id(runtime_ctx.session_id.clone().unwrap_or_default())
                .with_workspace(runtime_ctx.workspace.clone().unwrap_or_default())
                .with_principal_id(runtime_ctx.principal_id.clone().unwrap_or_default())
                .with_principal_name(runtime_ctx.principal_name.clone().unwrap_or_default())
                .with_capabilities(runtime_ctx.capabilities.clone().unwrap_or_default())
                .with_active_extensions(runtime_ctx.active_extensions.clone().unwrap_or_default());
        let tool_ctx = match runtime_ctx.abort_signal.as_ref() {
            Some(rx) => base_ctx.with_abort_signal(rx.clone()),
            None => base_ctx,
        };

        // Shared state for the cancel watcher. If the framework provided an
        // abort signal, spawn a task that invokes `on_interrupt` when it fires
        // and writes the resulting notice into the slot. The framework always
        // emits a notice on cancel, even for tools that do not implement
        // `InterruptibleTool` (the blanket impl supplies a soft default).
        let cancel_fired = Arc::new(AtomicBool::new(false));
        let notice_slot: Arc<tokio::sync::Mutex<Option<ToolInterruptNotice>>> =
            Arc::new(tokio::sync::Mutex::new(None));

        if let Some(mut rx) = runtime_ctx.abort_signal.clone() {
            let cancel_fired_w = cancel_fired.clone();
            let notice_slot_w = notice_slot.clone();
            let tool_for_interrupt = tool.clone();
            let tool_ctx_w = tool_ctx.clone();
            let tool_call_id_w = String::new();
            tokio::spawn(async move {
                if *rx.borrow() {
                    // Already aborted before we started watching.
                } else if rx.changed().await.is_err() {
                    // Sender dropped without a value flip — do nothing.
                    return;
                } else if !*rx.borrow() {
                    // Flipped to false (should not happen) — ignore.
                    return;
                }
                cancel_fired_w.store(true, Ordering::SeqCst);
                let notice = tool_for_interrupt
                    .on_interrupt(&tool_call_id_w, &tool_ctx_w)
                    .await;
                *notice_slot_w.lock().await = Some(notice);
            });
        }

        let tool_ctx_for_exec = tool_ctx.clone();
        let result = ctx
            .services
            .async_router()
            .execute_from_hook(
                &ctx,
                &tool_name,
                &exec_config,
                Some(Box::new(
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
                )),
                Box::new(move |p| {
                    let tool = tool.clone();
                    let tool_ctx = tool_ctx_for_exec.clone();
                    Box::pin(async move {
                        // Use execute_with_context so tools receive session/agent
                        // context injected by the extension framework.
                        tool.execute_with_context(p, &tool_ctx).await
                    })
                        as futures::future::BoxFuture<'static, anyhow::Result<serde_json::Value>>
                }) as peko_extension_host::ExecFn,
            )
            .await;

        // If the cancel fired, always emit the interrupt notice — even if the
        // tool also completed naturally. The user wanted to stop, so cancel wins.
        if cancel_fired.load(Ordering::SeqCst) {
            let notice =
                notice_slot.lock().await.take().unwrap_or_else(|| {
                    ToolInterruptNotice::soft_default("", &tool_name_for_notice)
                });
            HookResult::Continue(HookOutput::Text(notice.to_tool_result_text()))
        } else {
            result
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
    use std::sync::Arc;

    /// Build an `ExtensionCore` whose async router is a working local
    /// transport. Phase 8a changed `peko_extension_host::ExtensionServices::new`
    /// to use a `NoopAsyncExecutionRouter` (the host crate cannot depend on
    /// root to construct a real router), so test code that exercises
    /// `BuiltinExecuteHandler::handle` must wire one in explicitly. The root
    /// `AsyncExecutionRouter` impl is what production callers use, and it
    /// handles schema validation, preprocessor, and exec-fn dispatch the
    /// same way it always has.
    fn make_test_core() -> Arc<crate::extensions::framework::core::ExtensionCore> {
        let router =
            peko_extension_host::transport::async_router::AsyncExecutionRouter::with_transport(
                peko_extension_host::transport::async_transport::create_local_transport(),
            );
        let services = crate::extensions::framework::core::ExtensionServices::with_async_router(
            Arc::new(router),
        );
        Arc::new(
            crate::extensions::framework::core::ExtensionCore::with_services(Arc::new(services)),
        )
    }

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

    #[tokio::test]
    async fn cancel_after_completion_does_not_emit_spurious_notice() {
        use crate::extensions::framework::core::{HookContext, HookPoint};
        use crate::extensions::framework::types::{HookInput, ToolRuntimeContext};
        use std::time::Duration;

        let core = make_test_core();
        let tool: Arc<dyn Tool> = Arc::new(MockTool {
            name: "Fast".to_string(),
        });
        BuiltinToolAdapter::register_tool_system(&core, tool.clone())
            .await
            .unwrap();

        let (tx, rx) = tokio::sync::watch::channel(false);
        let input = HookInput::ToolCall {
            tool_name: "Fast".to_string(),
            params: json!({}),
            workspace: None,
            agent_id: None,
            session_id: None,
            caller_id: None,
            principal_id: None,
            principal_name: None,
            capabilities: Some(vec!["tool:Fast".to_string()]),
            active_extensions: None,
            abort_signal: Some(rx),
        };
        let point = HookPoint::ToolExecute {
            tool_name: "Fast".to_string(),
        };
        let mut ctx = HookContext::new(point, input, core.services());
        let runtime_ctx = ToolRuntimeContext::default().with_abort_signal(
            // Receiver already moved into input; re-clone from the input's field
            // is awkward, so build a fresh paired receiver just for the runtime ctx.
            // In production both come from the same source; the test only needs
            // the handler to see *a* receiver on the runtime ctx.
            tx.subscribe(),
        );
        ctx.set_state("tool_context", runtime_ctx);

        let handler = BuiltinExecuteHandler::new(tool);
        let result = handler.handle(ctx).await;

        // Cancel fires *after* the tool already completed.
        tx.send(true).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        match result {
            HookResult::Continue(HookOutput::Json(v)) => {
                assert_eq!(v, json!({"success": true}));
            }
            other => panic!("expected natural JSON result, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn cancel_before_completion_emits_minimal_notice_for_soft_tool() {
        use crate::extensions::framework::core::{HookContext, HookPoint};
        use crate::extensions::framework::types::{HookInput, ToolRuntimeContext};
        use std::time::Duration;

        struct SlowMockTool;

        #[async_trait]
        impl Tool for SlowMockTool {
            fn name(&self) -> &str {
                "Slow"
            }

            fn description(&self) -> String {
                "slow mock tool".to_string()
            }

            fn parameters(&self) -> serde_json::Value {
                json!({"type": "object", "properties": {}})
            }

            async fn execute(
                &self,
                _params: serde_json::Value,
            ) -> anyhow::Result<serde_json::Value> {
                tokio::time::sleep(Duration::from_millis(100)).await;
                Ok(json!({"ok": true}))
            }
        }

        let core = make_test_core();
        let tool: Arc<dyn Tool> = Arc::new(SlowMockTool);
        BuiltinToolAdapter::register_tool_system(&core, tool.clone())
            .await
            .unwrap();

        let (tx, rx) = tokio::sync::watch::channel(false);
        let input = HookInput::ToolCall {
            tool_name: "Slow".to_string(),
            params: json!({}),
            workspace: None,
            agent_id: None,
            session_id: None,
            caller_id: None,
            principal_id: None,
            principal_name: None,
            capabilities: Some(vec!["tool:Slow".to_string()]),
            active_extensions: None,
            abort_signal: Some(rx),
        };
        let point = HookPoint::ToolExecute {
            tool_name: "Slow".to_string(),
        };
        let mut ctx = HookContext::new(point, input, core.services());
        let runtime_ctx = ToolRuntimeContext::default().with_abort_signal(tx.subscribe());
        ctx.set_state("tool_context", runtime_ctx);

        // Fire cancel while the slow tool is sleeping.
        let cancel_tx = tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            cancel_tx.send(true).unwrap();
        });

        let handler = BuiltinExecuteHandler::new(tool);
        let result = handler.handle(ctx).await;

        match result {
            HookResult::Continue(HookOutput::Text(text)) => {
                assert!(text.contains("[Slow call was CANCELLED]"), "text: {text}");
            }
            other => panic!("expected cancel notice, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn cancel_fires_custom_on_interrupt_when_tool_opts_in() {
        use crate::extensions::framework::core::{HookContext, HookPoint};
        use crate::extensions::framework::types::{HookInput, ToolRuntimeContext};
        use peko_tools_core::{ToolContext, ToolInterruptNotice};
        use std::time::Duration;

        struct EnrichingMockTool;

        #[async_trait]
        impl Tool for EnrichingMockTool {
            fn name(&self) -> &str {
                "Enriching"
            }

            fn description(&self) -> String {
                "enriches cancel notice".to_string()
            }

            fn parameters(&self) -> serde_json::Value {
                json!({"type": "object", "properties": {}})
            }

            async fn execute(
                &self,
                _params: serde_json::Value,
            ) -> anyhow::Result<serde_json::Value> {
                tokio::time::sleep(Duration::from_millis(100)).await;
                Ok(json!({"ok": true}))
            }

            async fn on_interrupt(
                &self,
                tool_call_id: &str,
                _ctx: &ToolContext,
            ) -> ToolInterruptNotice {
                ToolInterruptNotice {
                    tool_call_id: tool_call_id.to_string(),
                    tool_name: self.name().to_string(),
                    preserved: vec![],
                    rolled_back: vec!["async-tx-123".to_string()],
                    leaked: vec![],
                    resume_hint: None,
                }
            }
        }

        let core = make_test_core();
        let tool: Arc<dyn Tool> = Arc::new(EnrichingMockTool);
        BuiltinToolAdapter::register_tool_system(&core, tool.clone())
            .await
            .unwrap();

        let (tx, rx) = tokio::sync::watch::channel(false);
        let input = HookInput::ToolCall {
            tool_name: "Enriching".to_string(),
            params: json!({}),
            workspace: None,
            agent_id: None,
            session_id: None,
            caller_id: None,
            principal_id: None,
            principal_name: None,
            capabilities: Some(vec!["tool:Enriching".to_string()]),
            active_extensions: None,
            abort_signal: Some(rx),
        };
        let point = HookPoint::ToolExecute {
            tool_name: "Enriching".to_string(),
        };
        let mut ctx = HookContext::new(point, input, core.services());
        let runtime_ctx = ToolRuntimeContext::default().with_abort_signal(tx.subscribe());
        ctx.set_state("tool_context", runtime_ctx);

        let cancel_tx = tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            cancel_tx.send(true).unwrap();
        });

        let handler = BuiltinExecuteHandler::new(tool);
        let result = handler.handle(ctx).await;

        match result {
            HookResult::Continue(HookOutput::Text(text)) => {
                assert!(
                    text.contains("[Enriching call was CANCELLED]"),
                    "text: {text}"
                );
                assert!(text.contains("Rolled back: async-tx-123"), "text: {text}");
            }
            other => panic!("expected enriched cancel notice, got {:?}", other),
        }
    }

    /// Demonstrates `on_interrupt` doing real cleanup of tool-internal state,
    /// not just describing consequences. The tool accumulates partial
    /// side-effects into a shared `staged` buffer during `execute`. When
    /// cancel fires, `on_interrupt` drains that buffer (rollback) and reports
    /// the drained entries in the notice. After the call, the buffer must be
    /// empty — proving the cleanup actually ran, not just that the notice
    /// was synthesized.
    #[tokio::test]
    async fn on_interrupt_performs_actual_cleanup_of_tool_state() {
        use crate::extensions::framework::core::{HookContext, HookPoint};
        use crate::extensions::framework::types::{HookInput, ToolRuntimeContext};
        use peko_tools_core::{ToolContext, ToolInterruptNotice};
        use std::sync::Mutex as StdMutex;
        use std::time::Duration;

        struct CleanupTool {
            staged: Arc<StdMutex<Vec<String>>>,
        }

        #[async_trait]
        impl Tool for CleanupTool {
            fn name(&self) -> &str {
                "Cleanup"
            }

            fn description(&self) -> String {
                "tool that performs cleanup on cancel".to_string()
            }

            fn parameters(&self) -> serde_json::Value {
                json!({"type": "object", "properties": {}})
            }

            async fn execute(
                &self,
                _params: serde_json::Value,
            ) -> anyhow::Result<serde_json::Value> {
                let staged = self.staged.clone();
                staged
                    .lock()
                    .unwrap()
                    .extend(["row-1".to_string(), "row-2".to_string()]);
                // Hold the tool open long enough for the cancel watcher to fire
                // and for `on_interrupt` to race us.
                tokio::time::sleep(Duration::from_millis(100)).await;
                staged.lock().unwrap().push("row-3".to_string());
                Ok(json!({"ok": true}))
            }

            async fn on_interrupt(
                &self,
                tool_call_id: &str,
                _ctx: &ToolContext,
            ) -> ToolInterruptNotice {
                // Cleanup: drain whatever's been staged so far. This is the
                // "rollback" — the partial writes never reach durable storage.
                let drained = {
                    let mut guard = self.staged.lock().unwrap();
                    std::mem::take(&mut *guard)
                };
                ToolInterruptNotice {
                    tool_call_id: tool_call_id.to_string(),
                    tool_name: self.name().to_string(),
                    preserved: vec![],
                    rolled_back: drained,
                    leaked: vec![],
                    resume_hint: Some("staged write was rolled back; safe to retry".to_string()),
                }
            }
        }

        let staged = Arc::new(StdMutex::new(Vec::<String>::new()));
        let tool_struct = CleanupTool {
            staged: staged.clone(),
        };
        // Wrap in a tool that exposes `as_any` so `Arc<dyn Tool>` works.
        let tool: Arc<dyn Tool> = Arc::new(tool_struct);

        let core = make_test_core();
        BuiltinToolAdapter::register_tool_system(&core, tool.clone())
            .await
            .unwrap();

        let (tx, rx) = tokio::sync::watch::channel(false);
        let input = HookInput::ToolCall {
            tool_name: "Cleanup".to_string(),
            params: json!({}),
            workspace: None,
            agent_id: None,
            session_id: None,
            caller_id: None,
            principal_id: None,
            principal_name: None,
            capabilities: Some(vec!["tool:Cleanup".to_string()]),
            active_extensions: None,
            abort_signal: Some(rx),
        };
        let point = HookPoint::ToolExecute {
            tool_name: "Cleanup".to_string(),
        };
        let mut ctx = HookContext::new(point, input, core.services());
        let runtime_ctx = ToolRuntimeContext::default().with_abort_signal(tx.subscribe());
        ctx.set_state("tool_context", runtime_ctx);

        // Fire cancel while the tool is sleeping in `execute`.
        let cancel_tx = tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            cancel_tx.send(true).unwrap();
        });

        let handler = BuiltinExecuteHandler::new(tool);
        let result = handler.handle(ctx).await;

        // Notice carries the rolled-back entries.
        match result {
            HookResult::Continue(HookOutput::Text(text)) => {
                assert!(
                    text.contains("[Cleanup call was CANCELLED]"),
                    "text: {text}"
                );
                // The two rows staged before the cancel fired were drained
                // by `on_interrupt`. Whether the third row was drained
                // depends on whether `on_interrupt` ran before or after
                // `execute` resumed — we only assert the rows that were
                // *certainly* present at cancel time.
                assert!(
                    text.contains("Rolled back: row-1") && text.contains("row-2"),
                    "text: {text}"
                );
            }
            other => panic!("expected cancel notice, got {:?}", other),
        }

        // Cleanup actually ran: by the time the framework returned the
        // notice, `on_interrupt` had drained the staged buffer. Allow
        // a small grace window for the executor task to fully unwind
        // before we inspect the buffer.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let final_staged = staged.lock().unwrap().clone();
        // Whatever rows survived in the buffer (e.g. row-3 if `execute`
        // resumed and pushed before noticing the abort) must NOT have
        // been reported as rolled-back — the cleanup is a one-shot
        // snapshot of what was staged *at interrupt time*.
        assert!(
            final_staged.is_empty() || final_staged.iter().all(|r| r == "row-3"),
            "staged buffer after cleanup: {final_staged:?} — on_interrupt should have drained everything staged at cancel time"
        );
    }

    /// F32b — integration pin: validation short-circuits the dispatch path
    /// inside `BuiltinExecuteHandler::handle`. A tool whose declared
    /// `parameters()` schema requires `command` must NOT have `execute`
    /// invoked when the LLM omits that field; the framework must surface
    /// a `HookResult::Error` carrying the validation message instead.
    #[tokio::test]
    async fn dispatch_short_circuits_when_args_violate_declared_schema() {
        use crate::extensions::framework::core::{HookContext, HookPoint};
        use crate::extensions::framework::types::{HookInput, ToolRuntimeContext};
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct RequiredFieldTool {
            call_count: Arc<AtomicUsize>,
        }

        #[async_trait]
        impl Tool for RequiredFieldTool {
            fn name(&self) -> &str {
                "RequiredField"
            }

            fn description(&self) -> String {
                "tool that requires a `command` field".to_string()
            }

            fn parameters(&self) -> serde_json::Value {
                json!({
                    "type": "object",
                    "properties": {
                        "command": { "type": "string" }
                    },
                    "required": ["command"]
                })
            }

            async fn execute(
                &self,
                _params: serde_json::Value,
            ) -> anyhow::Result<serde_json::Value> {
                self.call_count.fetch_add(1, Ordering::SeqCst);
                Ok(json!({"executed": true}))
            }
        }

        let call_count = Arc::new(AtomicUsize::new(0));
        let tool: Arc<dyn Tool> = Arc::new(RequiredFieldTool {
            call_count: call_count.clone(),
        });

        let core = make_test_core();
        BuiltinToolAdapter::register_tool_system(&core, tool.clone())
            .await
            .unwrap();

        let (_tx, rx) = tokio::sync::watch::channel(false);
        // LLM emits args that violate the schema — `command` is missing.
        let input = HookInput::ToolCall {
            tool_name: "RequiredField".to_string(),
            params: json!({}),
            workspace: None,
            agent_id: None,
            session_id: None,
            caller_id: None,
            principal_id: None,
            principal_name: None,
            capabilities: Some(vec!["tool:RequiredField".to_string()]),
            active_extensions: None,
            abort_signal: Some(rx),
        };
        let point = HookPoint::ToolExecute {
            tool_name: "RequiredField".to_string(),
        };
        let mut ctx = HookContext::new(point, input, core.services());
        let runtime_ctx = ToolRuntimeContext::default();
        ctx.set_state("tool_context", runtime_ctx);

        let handler = BuiltinExecuteHandler::new(tool);
        let result = handler.handle(ctx).await;

        // Validation must short-circuit before the tool is invoked.
        match result {
            HookResult::Error(err) => {
                let msg = format!("{err:#}");
                assert!(
                    msg.contains("RequiredField") && msg.contains("command"),
                    "expected validation error naming the tool and the missing field, got: {msg}"
                );
            }
            other => panic!(
                "expected HookResult::Error from validation, got {:?}",
                other
            ),
        }
        assert_eq!(
            call_count.load(Ordering::SeqCst),
            0,
            "execute must not be called when args violate the schema"
        );
    }

    /// F32b — counterpart pin: a tool whose args DO satisfy its declared
    /// schema must still execute normally. Proves the validator doesn't
    /// accidentally reject legitimate calls.
    #[tokio::test]
    async fn dispatch_passes_when_args_satisfy_declared_schema() {
        use crate::extensions::framework::core::{HookContext, HookPoint};
        use crate::extensions::framework::types::{HookInput, ToolRuntimeContext};
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct RequiredFieldTool {
            call_count: Arc<AtomicUsize>,
        }

        #[async_trait]
        impl Tool for RequiredFieldTool {
            fn name(&self) -> &str {
                "RequiredFieldPass"
            }

            fn description(&self) -> String {
                "tool that requires a `command` field".to_string()
            }

            fn parameters(&self) -> serde_json::Value {
                json!({
                    "type": "object",
                    "properties": {
                        "command": { "type": "string" }
                    },
                    "required": ["command"]
                })
            }

            async fn execute(
                &self,
                params: serde_json::Value,
            ) -> anyhow::Result<serde_json::Value> {
                self.call_count.fetch_add(1, Ordering::SeqCst);
                Ok(json!({ "ran_with": params }))
            }
        }

        let call_count = Arc::new(AtomicUsize::new(0));
        let tool: Arc<dyn Tool> = Arc::new(RequiredFieldTool {
            call_count: call_count.clone(),
        });

        let core = make_test_core();
        BuiltinToolAdapter::register_tool_system(&core, tool.clone())
            .await
            .unwrap();

        let (_tx, rx) = tokio::sync::watch::channel(false);
        let input = HookInput::ToolCall {
            tool_name: "RequiredFieldPass".to_string(),
            params: json!({ "command": "ls -la" }),
            workspace: None,
            agent_id: None,
            session_id: None,
            caller_id: None,
            principal_id: None,
            principal_name: None,
            capabilities: Some(vec!["tool:RequiredFieldPass".to_string()]),
            active_extensions: None,
            abort_signal: Some(rx),
        };
        let point = HookPoint::ToolExecute {
            tool_name: "RequiredFieldPass".to_string(),
        };
        let mut ctx = HookContext::new(point, input, core.services());
        let runtime_ctx = ToolRuntimeContext::default();
        ctx.set_state("tool_context", runtime_ctx);

        let handler = BuiltinExecuteHandler::new(tool);
        let result = handler.handle(ctx).await;

        // The handler returns whatever `execute_from_hook` returned; for a
        // successful tool that is a JSON-shaped `Continue(Json(...))`.
        match result {
            HookResult::Continue(HookOutput::Json(v)) => {
                assert_eq!(v["ran_with"]["command"], "ls -la");
            }
            other => panic!("expected HookResult::Continue(Json(_)), got {:?}", other),
        }
        assert_eq!(
            call_count.load(Ordering::SeqCst),
            1,
            "execute must be called exactly once when args satisfy the schema"
        );
    }
}
