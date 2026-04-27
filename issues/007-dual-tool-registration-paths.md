# Issue 007: Dual Tool Registration Paths — Incomplete ADR-018b Migration

**Severity:** CRITICAL  
**Status:** 🟡 **Open**  
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
    tool: &UniversalToolManifest,
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

## Proposed Resolution

**Option A: Complete the migration (Recommended)**

1. **Rewrite `register_tools_with_core()` in `universal_tool_adapter.rs`** to call `core.register_tool()` for each tool instead of manual hook registration.
2. **Rewrite `register_servers_with_core()` in `mcp_adapter.rs`** similarly.
3. **Delete the 6-handler-per-tool boilerplate** — the handler structs/factories for individual hooks become unnecessary if `register_tool()` handles everything.
4. **Verify** that `BuiltinToolAdapter`, `UniversalToolAdapter`, and `McpAdapter` all use the same registration path.

**Option B: Remove unified registry and standardize on manual hooks**

Deprecate `ExtensionCore::register_tool()` and make manual hook registration the canonical path. This is worse because it loses centralized metadata and enable/disable policy.

**Option C: Make `register_tool()` a code-generated wrapper**

Keep manual hooks as the low-level API, but have `register_tool()` generate the 6 hooks automatically. This preserves flexibility while eliminating boilerplate.

---

## Acceptance Criteria

- [ ] All tool adapters (`BuiltinToolAdapter`, `UniversalToolAdapter`, `McpAdapter`) use the **same** registration path.
- [ ] `register_tools_with_core()` in Universal adapter delegates to `register_tool()` or is deleted.
- [ ] `register_servers_with_core()` in MCP adapter delegates to `register_tool()` or is deleted.
- [ ] No tool is registered via both paths simultaneously.
- [ ] Tool metadata (schema, description, enable/disable) is centralized in `ToolRegistry` for all tool types.
- [ ] All existing tests pass.

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
