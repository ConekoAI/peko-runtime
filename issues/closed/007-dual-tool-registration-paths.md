# Issue 007: Dual Tool Registration Paths — Incomplete ADR-018b Migration

**Severity:** CRITICAL  
**Status:** 🔒 **CLOSED**  
**Closed:** 2026-04-27  
**Commits:** See git log for Issue-007  
**Labels:** `architecture`, `tool-registry`, `adr-018b`, `refactor`, `extensions`  
**Reported:** 2026-04-27  

---

## Summary

ADR-018b introduced a **unified tool registry** (`ExtensionCore::register_tool()`) to replace manual per-hook registration. However, the migration is **incomplete**: `universal_tool_adapter.rs` and `mcp_adapter.rs` still manually register 6 separate hooks per tool via `register_tools_with_core()`, while `builtin_registry.rs` uses the new unified path. This creates two parallel registration systems, confusion about which is canonical, and duplicated boilerplate.

---

## The Two Paths

### Path 1: Unified Registry (`register_tool()`)

**Location:** `src/extensions/core/registry.rs`  
**Used by:** `builtin_registry.rs`, partial adapter usage  
**Mechanism:** Single call registers a tool with metadata, schema, and execution handler:

```rust
// src/extensions/core/registry.rs
pub async fn register_tool(
    &self,
    metadata: ToolMetadata,
    handler: Arc<dyn HookHandler>,
    extension_id: &ExtensionId,
) -> HookResult<()> {
    // Registers with HookRegistry AND ToolRegistry in one operation
}
```

**Benefits:** One call per tool, consistent metadata, centralized enable/disable policy.

---

### Path 2: Manual Hook Registration (`register_tools_with_core()`)

**Location:** `src/extensions/adapters/universal_tool_adapter.rs` (lines 807–945)  
**Location:** `src/extensions/adapters/mcp_adapter.rs` (similar pattern)  
**Mechanism:** 6 separate hook registrations per tool:

```rust
// Universal tool adapter — repeated for EVERY tool
for tool in &tools {
    // 1. Tool registration hook
    core.register_hook(HookPoint::ToolRegister { ... }, ...).await?;
    // 2. Prompt injection hook
    core.register_hook(HookPoint::PromptSystemSection { ... }, ...).await?;
    // 3. Sync execution hook
    core.register_hook(HookPoint::ToolExecute { tool_name: ... }, ...).await?;
    // 4. Async execution hook
    core.register_hook(HookPoint::ToolExecuteAsync { tool_name: ... }, ...).await?;
    // 5. Check status hook
    core.register_hook(HookPoint::ToolCheckStatus { tool_name: ... }, ...).await?;
    // 6. Cancel hook
    core.register_hook(HookPoint::ToolCancel { tool_name: ... }, ...).await?;
}
```

**Problems:**
- ~60 lines of boilerplate per toolset
- Risk of partial registration (if one hook fails, tool is half-registered)
- No centralized metadata — schema, description, and enable/disable are scattered across hooks
- Duplicates logic already present in `register_tool()`

---

## Evidence

### `UniversalToolAdapter::register_tool()` (uses unified registry)

```rust
// src/extensions/adapters/universal_tool_adapter.rs (lines 196–263)
pub async fn register_tool(
    &self,
    core: &ExtensionCore,
    manifest: &ExtensionManifest,
) -> Result<()> {
    let metadata = ToolMetadata { ... };
    core.register_tool(metadata, Arc::new(handler), &manifest.id).await?;
    Ok(())
}
```

This method **exists** and uses the unified registry. But it is **not** called by `register_tools_with_core()`.

### `register_tools_with_core()` (uses manual hooks)

```rust
// src/extensions/adapters/universal_tool_adapter.rs (lines 807–945)
pub async fn register_tools_with_core(
    &self,
    core: &ExtensionCore,
    manifest: &ExtensionManifest,
    tools: &[UniversalToolManifest],
) -> Result<()> {
    // Manually constructs and registers 6 handlers per tool
    // ~140 lines of repetitive hook registration
}
```

### MCP Adapter has the same duality

```rust
// src/extensions/adapters/mcp_adapter.rs
// register_server_tools() — uses unified registry (lines ~280–310)
// register_servers_with_core() — uses manual hooks (lines ~650–800)
```

---

## Impact

1. **Inconsistent behavior:** Tools registered via unified registry get centralized metadata and enable/disable policy. Tools registered manually do not.
2. **Registration bugs:** If a tool is registered via both paths (or partially via one), the `ExtensionCore` may have duplicate or conflicting hooks.
3. **Maintenance burden:** Adding a new hook point (e.g., `ToolPreExecute`) requires updating both `register_tool()` AND every adapter's manual registration.
4. **Developer confusion:** Contributors cannot tell which path is canonical. The unified registry is documented as the new way, but most tools are still registered manually.

---

## Root Cause

- ADR-018b was implemented incrementally. The `register_tool()` method was added to `ExtensionCore`, but the adapters were not fully migrated.
- `register_tools_with_core()` was likely written before `register_tool()` existed, and never updated.
- The MCP adapter has a third path (`create_tool_instances()` returning `Arc<dyn Tool>`) that predates both.

---

## Resolution

**Option A implemented:** Complete ADR-018b migration.

### Changes Made

1. **`src/extensions/core/tool_registration.rs` (NEW)** — `ToolRegistration` composite + auto-generated handlers (`AutoPromptHandler`, `AutoAsyncHandler`, `AutoStatusHandler`, `AutoCancelHandler`) that provide sensible defaults for all companion hooks.

2. **`src/extensions/core/registry.rs`** — `register_tool()` enhanced to perform atomic composite registration: execution hook (adapter-provided) + 4 auto-generated companion hooks. `unregister_tool()` now cleans up all companion hooks atomically via `companion_hook_ids` stored in `ToolMetadata`.

3. **`src/extensions/types.rs`** — Added `companion_hook_ids: Option<Vec<HookId>>` to `ToolMetadata` for atomic cleanup tracking.

4. **`src/extensions/adapters/mod.rs`** — Added `register_tools()` to `ExtensionTypeAdapter` trait (default no-op).

5. **`src/extensions/manager/mod.rs`** — Replaced ~70 lines of special-case dual registration with a single generic `adapter.register_tools(&core, &manifest).await` call.

6. **`src/extensions/adapters/universal_tool_adapter.rs`** — Deleted `register_tools_with_core()` and all 5 factory/handler pairs (~400 lines). `resolve_hooks()` now returns `vec![]` for tool hooks. `register_tools()` delegates to `register_tool()`.

7. **`src/extensions/adapters/mcp_adapter.rs`** — Deleted `register_servers_with_core()` and all tool-specific factory/handler pairs (~350 lines). `resolve_hooks()` now returns only lifecycle hooks (`AgentInit`, `AgentShutdown`). `register_tools()` delegates to `register_server_tools()`.

8. **`src/extensions/adapters/builtin_tool_adapter.rs`** — Removed redundant manual `PromptSystemSection` hook registration.

9. **Re-exports updated** — Removed deleted functions from `mod.rs` and `tools/universal/mod.rs`.

### Net Result

- **~750 lines deleted**, ~200 lines added = **~550 lines net reduction**.
- All 900 tests pass.
- Zero special-case logic in `ExtensionManager`.
- Single canonical path: `ExtensionCore::register_tool()`.

---

## Acceptance Criteria

- [x] All tool adapters (`BuiltinToolAdapter`, `UniversalToolAdapter`, `McpAdapter`) use the **same** registration path.
- [x] `register_tools_with_core()` deleted from Universal adapter.
- [x] `register_servers_with_core()` deleted from MCP adapter.
- [x] No tool is registered via both paths simultaneously.
- [x] Tool metadata (schema, description, enable/disable) is centralized in `ToolRegistry` for all tool types.
- [x] All existing tests pass (900 passed, 19 ignored).

---

## Related

- Issue 002: Tool Execution — Three Competing Paths
- Issue 006: Three Async Tool Frameworks
- ADR-018b: Unified Tool Registry
- `src/extensions/core/registry.rs`
- `src/extensions/adapters/universal_tool_adapter.rs`
- `src/extensions/adapters/mcp_adapter.rs`
- `src/extensions/adapters/builtin_tool_adapter.rs`
- `src/tools/builtin_registry.rs`
