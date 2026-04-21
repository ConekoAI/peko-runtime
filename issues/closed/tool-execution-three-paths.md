# Issue Tool Execution — Three Competing Paths

**Severity:** CRITICAL  
**Status:** Closed ✅  
**Labels:** `architecture`, `tool-execution`, `adr-018a`, `adr-018b`, `refactor`  
**Reported:** 2026-04-21  
**Closed:** 2026-04-21  

---

## Summary

Tools could be executed through three different paths, creating a diamond dependency where the same logical tool existed in multiple registries. The `Tool` trait was the original abstraction. The `ExtensionCore` hook system wrapped tools as `HookHandler`s via `BuiltinToolAdapter`. MCP tools implemented the `Tool` trait and also registered with `ExtensionCore`. This redundancy created subtle bugs, inconsistent behavior, and maintenance burden.

This issue has been resolved. All phases are complete.

---

## Execution Paths (Before)

| Path | Entry Point | Used By | Mechanism |
|------|-------------|---------|-----------|
| **Path 1: Direct trait** | `tool.execute(params)` | Legacy code, tests | Direct `async_trait` call |
| **Path 2: Agentic loop via ExtensionCore** | `ExtensionCore::invoke_hook(HookPoint::ToolExecute, ...)` | `AgenticLoop::run_inner()` | `BuiltinToolAdapter` wraps `Tool` as `HookHandler` |
| **Path 3: Standalone runtime via ExtensionCore** | `ToolRuntime::execute_tool(name, params)` | Daemon, standalone contexts | Same `ExtensionCore::invoke_hook`, different caller |

**After:** All production code uses `execute_tool_via_core()` (Path 2/3 unified). The `Tool` trait `execute`/`execute_with_context` methods are documented as test-only.

---

## Changes Made

### Phase 1: Extract Shared `tool_result_from_hook` ✅

**File: `src/extensions/types.rs`**

Added `tool_result_from_hook()` — the single place where `HookResult`→tool output semantics are defined. Used by both `AgenticLoop` and `ToolRuntime`.

### Phase 2: Add Canonical Free Function `execute_tool_via_core` ✅

**File: `src/runtime/tool_runtime.rs`**

Added `execute_tool_via_core(core, tool_name, params, workspace)` — the canonical production execution path. `ToolRuntime::execute_tool_with_workspace` now delegates to this function.

### Phase 3: Simplify `AgenticLoop::run_inner` ✅

**File: `src/engine/agentic_loop.rs`**

Replaced the ~90-line duplicated `HookResult` match block with a call to `execute_tool_via_core()`. `AgenticLoop` and `ToolRuntime` now behave identically.

### Phase 4: Document `Tool::execute` as Test-Only ✅

**File: `src/tools/traits.rs`**

Added doc comments to `execute` and `execute_with_context` warning that they are test-only in production contexts. Production code must route through `ExtensionCore::invoke_hook`.

### Phase 5: Eliminate Redundant `register_builtins` Call ✅

**File: `src/agent/agent.rs`**

Removed the unconditional `ToolRuntime::register_builtins()` call from `init_builtins_async()`. Added a defensive check that logs an error if common built-ins are not pre-registered by the daemon startup path.

- **`AppState::new()`** (daemon startup): Creates `ToolRuntime`, registers common built-ins.
- **`Agent::init_builtins_async()`** (per-request): Registers only agent-specific tools (`SessionStatusTool`, `AgentSpawnTool`, etc.).

### Phase 6: Update `BuiltinToolAdapter` to Use `execute_with_context` ✅

**File: `src/extensions/adapters/builtin_tool_adapter.rs`**

Changed the execution closure from `tool.execute(p)` to `tool.execute_with_context(p, &ctx)`, ensuring abort checking, timeout handling, progress reporting, and metrics collection are active even for tools invoked through the hook system.

### Phase 7: Add `ToolContext::default_for_tool` ✅

**File: `src/tools/context.rs`**

Added `ToolContext::default_for_tool()` constructor so `BuiltinToolAdapter` can create a minimal context when no abort/progress signals are available from the hook system.

---

## Bug Fix: E2E Test Failure

After completing Phase 5, the built-in tool e2e tests failed with "No tools available". The root cause was a `OnceLock` race in the daemon startup sequence:

1. `main.rs::init_extension_core()` created an empty `ExtensionCore` and called `init_global_core()` — **set succeeded**
2. `AppState::build()` created a tool-filled `ExtensionCore` and called `init_global_core()` — **set failed silently** (OnceLock already set)
3. `Agent::new()` → `global_core()` returned the **empty** core from step 1

**Fix:** `AppState::build()` now checks `global_core()` first. If it already exists (set by `main.rs`), it reuses that instance and registers tools on it. Otherwise it creates a new one.

**File: `src/daemon/state.rs`**

---

## Acceptance Criteria

- [x] `tool_result_from_hook` helper exists and is used by both `AgenticLoop` and `ToolRuntime`.
- [x] `execute_tool_via_core` free function is the canonical production execution path.
- [x] `AgenticLoop::run_inner` no longer contains duplicated `HookResult` match logic.
- [x] The `Tool` trait `execute`/`execute_with_context` methods are documented as "test-only in production contexts".
- [x] `Agent::init_builtins_async` no longer unconditionally re-registers common built-ins; it only registers agent-specific tools.
- [x] `BuiltinToolAdapter` calls `execute_with_context` instead of plain `execute`.
- [x] E2E built-in tool tests (grep, shell, read_file, write_file, glob, str_replace_file, session_status) all pass.
- [x] All 836 unit tests pass.

---

## Related

- `src/tools/traits.rs`
- `src/extensions/adapters/builtin_tool_adapter.rs`
- `src/runtime/tool_runtime.rs`
- `src/engine/agentic_loop.rs`
- `src/extensions/adapters/mcp_adapter.rs`
- `src/daemon/state.rs`
- `src/tools/context.rs`
- `src/extensions/types.rs`
- ADR-018a: Tool Execution Unification
- ADR-018b: Unified Tool Registry
- ADR-020: Daemon-Based Async Execution
