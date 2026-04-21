# Issue 002: Tool Execution — Three Competing Paths

**Severity:** CRITICAL  
**Status:** Open  
**Labels:** `architecture`, `tool-execution`, `adr-018a`, `adr-018b`, `refactor`  
**Reported:** 2026-04-21  

---

## Summary

Tools can be executed through three different paths, creating a diamond dependency where the same logical tool exists in multiple registries. The `Tool` trait is the original abstraction. The `ExtensionCore` hook system wraps tools as `HookHandler`s via `BuiltinToolAdapter`. MCP tools implement the `Tool` trait and also register with `ExtensionCore`. This redundancy creates subtle bugs, inconsistent behavior, and maintenance burden.

---

## Execution Paths

| Path | Entry Point | Used By | Mechanism |
|------|-------------|---------|-----------|
| **Path 1: Direct trait** | `tool.execute(params)` | Legacy code, tests | Direct `async_trait` call |
| **Path 2: Agentic loop via ExtensionCore** | `ExtensionCore::invoke_hook(HookPoint::ToolExecute, ...)` | `AgenticLoop::run_inner()` | `BuiltinToolAdapter` wraps `Tool` as `HookHandler` |
| **Path 3: Standalone runtime via ExtensionCore** | `ToolRuntime::execute_tool(name, params)` | Daemon, standalone contexts | Same `ExtensionCore::invoke_hook`, different caller |

---

## Evidence

### Path 1: Direct `Tool::execute`

**`src/tools/traits.rs` (lines 74–179):**
```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> String;
    fn parameters(&self) -> serde_json::Value;
    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value>;
    async fn execute_with_context(&self, params: serde_json::Value, ctx: &ToolContext) -> anyhow::Result<serde_json::Value>;
}
```

### Path 2: Agentic loop → ExtensionCore

**`src/engine/agentic_loop.rs` (lines 700–799):**
```rust
let tool_in_extension = self.extension_core.get_tool_metadata(name).await.is_some();

let (tool_result_str, tool_result_json, success) = if tool_in_extension {
    let hook_point = HookPointBuilder::tool_execute(name);
    let hook_input = HookInput::ToolCall { tool_name: name.clone(), params: arguments.clone(), workspace: ... };
    match self.extension_core.invoke_hook(hook_point, hook_input).await {
        HookResult::Continue(HookOutput::Json(result)) => ...
        HookResult::Continue(HookOutput::Text(result)) => ...
        // ... 10+ match arms for different HookResult variants
    }
} else {
    let s = format!("Tool '{name}' not found");
    (s.clone(), serde_json::Value::String(s), false)
};
```

### Path 3: `ToolRuntime` → ExtensionCore

**`src/runtime/tool_runtime.rs` (lines 184–230):**
```rust
pub async fn execute_tool(&self, tool_name: &str, params: serde_json::Value) -> Result<serde_json::Value> {
    let point = HookPoint::ToolExecute { tool_name: tool_name.to_string() };
    let input = HookInput::ToolCall { tool_name: tool_name.to_string(), params, workspace: Some(...) };
    let result = self.extension_core.invoke_hook(point, input).await;
    // ... match on HookResult
}
```

---

## Diamond Dependency

The same tools are registered multiple times into different contexts:

### `Agent::init_builtins_async()`

**`src/agent/agent.rs` (lines 43–162, specifically line 157):**
```rust
BuiltinToolAdapter::register_tool(&self.extension_core, tool.clone()).await
```

### `ToolRuntime::register_builtins()`

**`src/runtime/tool_runtime.rs` (lines 111–162):**
```rust
pub async fn register_builtins(extension_core: &ExtensionCore, path_resolver: &PathResolver) -> Result<()> {
    let tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(ShellTool::new().with_workspace(workspace.clone())),
        Arc::new(ReadFileTool::new().with_workspace(workspace.clone())),
        // ... same tools as Agent
    ];
    for tool in &tools {
        BuiltinToolAdapter::register_tool(extension_core, tool.clone()).await?;
    }
}
```

### MCP tools

**`src/extensions/adapters/mcp_adapter.rs` (lines 528–541):**
```rust
// MCP tools implement Tool trait AND register with ExtensionCore
HookPoint::ToolExecute { tool_name: format!("{}:{}:*", MCP_TOOL_PREFIX, manifest.name) }
```

---

## Impact

1. **Inconsistent behavior:** A tool may behave differently depending on which path executes it (e.g., `ToolContext` availability, workspace injection, timeout handling).
2. **Double registration:** The same `ShellTool` instance may be registered in the agent's `ExtensionCore` and the daemon's `ExtensionCore`, leading to divergent state.
3. **Maintenance burden:** Fixing a tool execution bug requires checking three code paths.
4. **Test fragmentation:** Tests may pass on the direct trait path but fail via the hook path (or vice versa) due to `HookInput` serialization/deserialization, workspace injection, or `ToolContext` differences.
5. **ADR-018a incomplete:** The "Tool Execution Unification" ADR declares a single entry point (`ExtensionCore::invoke_hook`) but the `Tool::execute` direct path still exists and is used.

---

## Root Cause

- The `Tool` trait predates the extension framework.
- ADR-018a and ADR-018b introduced `ExtensionCore` as the unified registry but did not fully deprecate the direct trait path.
- `ToolRuntime` was extracted from `Agent::init_builtins_async()` for daemon use (ADR-020) but duplicates the registration logic rather than sharing a single registry instance.
- MCP tools were retrofitted to implement `Tool` for backward compatibility while also registering via `McpAdapter` for the new system.

---

## Proposed Resolution

### Phase 1: Make `ExtensionCore` the sole execution path (P0)

1. **Deprecate direct `Tool::execute` usage in production code.** Retain the trait method for testing, but route all production execution through `ExtensionCore::invoke_hook`.
2. **Remove `ToolRuntime::execute_tool` as a separate concept.** `ToolRuntime` should be a thin wrapper that holds an `Arc<ExtensionCore>` and delegates directly. The `execute_tool` method can remain as a convenience but must not add new semantics.
3. **Unify registration:** Ensure `Agent` and `ToolRuntime` share the same `ExtensionCore` instance (or a clearly parent-child relationship) so tools are registered exactly once.

### Phase 2: Reconcile `Tool` trait with `HookHandler` (P1)

1. **Evaluate whether `Tool` should become a `HookHandler` factory.** Every `Tool` implementation could automatically generate its `BuiltinToolAdapter` wrapper, eliminating the manual registration step.
2. **Or, make `Tool` a narrower trait** that only defines metadata (`name`, `description`, `parameters`) while execution is always mediated by `ExtensionCore`.

### Phase 3: Audit and remove dead paths (P2)

1. Search for all direct `tool.execute(...)` calls outside of tests.
2. Migrate to `ExtensionCore::invoke_hook` or `ToolRuntime::execute_tool`.
3. Delete `BuiltinToolAdapter` if tools self-register.

---

## Acceptance Criteria

- [ ] All production tool execution flows through `ExtensionCore::invoke_hook` (or a single thin wrapper).
- [ ] The `Tool` trait direct path is documented as "test-only" or removed.
- [ ] `Agent` and `ToolRuntime` do not independently register the same tools.
- [ ] A single tool instance cannot be registered twice in conflicting ways.
- [ ] ADR-018a is updated to reflect the completed unification.

---

## Related

- `src/tools/traits.rs`
- `src/extensions/adapters/builtin_tool_adapter.rs`
- `src/runtime/tool_runtime.rs`
- `src/engine/agentic_loop.rs`
- `src/extensions/adapters/mcp_adapter.rs`
- ADR-018a: Tool Execution Unification
- ADR-018b: Unified Tool Registry
- ADR-020: Daemon-Based Async Execution
