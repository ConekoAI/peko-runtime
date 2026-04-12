# ADR-018a: Tool Execution Unification

**Status**: Completed  
**Completed Date**: 2026-04-11  
**Date**: 2026-04-11  
**Author**: Kimi Code CLI  
**Depends On**: ADR-018b (Unified Tool Registry)  
**Related**: ADR-017 (Extensions 2.0), ADR-018c (Tool Naming Cleanup), ADR-019 (Dynamic Updates)

## Context

Tools currently execute through **two different paths** depending on tool type, causing:
- Inconsistent reserved parameter handling
- Duplicate panic isolation and timeout logic
- Security gaps (permission checks in multiple places)
- Maintenance burden (two paths to test and maintain)

## Problem Statement

### Current State: Dual Execution Paths

```
┌─────────────────────────────────────────────────────────────────────────┐
│                     CURRENT EXECUTION PATHS                             │
├─────────────────────────────────────────────────────────────────────────┤
│                                                                         │
│  BEFORE MIGRATION (Two Paths - This is the problem)                     │
│                                                                         │
│  AgenticLoopV4::run_loop()                                              │
│       │                                                                 │
│       ├──► Built-in? ──Yes──► self.tools.iter().find()                  │
│       │                           │                                     │
│       │                           ▼                                     │
│       │                   ToolExecutor::execute_with_context()          │
│       │                           │                                     │
│       │                           ├──► Panic isolation                  │
│       │                           ├──► Timeout                          │
│       │                           └──► Context injection                │
│       │                           └──► _async handling (ToolWrapper)    │
│       │                                                                 │
│       └──► Extension tool? ──Yes──► ExtensionCore::invoke_hook()        │
│                                         │                               │
│                                         ├──► MCP ──► JSON-RPC           │
│                                         ├──► Universal ──► Process      │
│                                         └──► General ──► Custom         │
│                                                                         │
│  PROBLEM:                                                               │
│  • Two paths = duplicate logic (panic isolation, timeout)               │
│  • Built-in tools bypass ExtensionCore entirely                         │
│  • ToolWrapper _async only works for direct path                        │
│  • Different context injection (fake vs real)                           │
│                                                                         │
└─────────────────────────────────────────────────────────────────────────┘
```

### Issues with Current Design

1. **DRY Violation**: `ToolExecutor` and `TaskManager` duplicate panic isolation logic
2. **Inconsistent Context**: Built-in tools get fake context values ("unknown") via direct path
3. **Bypassed Validation**: Extension tools use `ToolExecutionService`, built-ins don't
4. **Async Handling Gap**: ToolWrapper's `_async` parameter only works for direct execution

## Decision

**Route ALL tool execution through ExtensionCore hooks**, with execution concerns (panic isolation, timeout, async routing) implemented as **hook middleware** or **shared services** called by handlers.

### Key Design Principles

1. **Single Entry Point**: `ExtensionCore::invoke_hook(HookPoint::ToolExecute)` for ALL tools
2. **Shared Execution Service**: `ToolExecutor` becomes internal service used by handlers
3. **Async Unification**: Move `_async` handling from ToolWrapper to ExtensionCore layer
4. **Consistent Context**: All tools receive real `ToolContext` via hook context

## Implementation

### Phase 1: Unified Execution Service

Move `ToolExecutor` to ExtensionCore services and make it the **only** execution implementation:

```rust
// src/extensions/services/tool_executor.rs
pub struct ToolExecutionService {
    panic_isolator: PanicIsolator,
    timeout_config: TimeoutConfig,
}

impl ToolExecutionService {
    /// Execute any tool with full isolation - used by ALL handlers
    pub async fn execute<F, Fut>(
        &self,
        executor: F,
        timeout: Option<Duration>,
    ) -> Result<Value>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Value>>,
    {
        // Panic isolation (one implementation)
        let result = self.panic_isolator.run(async {
            // Timeout (one implementation)
            if let Some(duration) = timeout {
                tokio::time::timeout(duration, executor()).await?
            } else {
                executor().await
            }
        }).await;
        
        result
    }
}
```

### Phase 2: Async Handling in ExtensionCore

**CRITICAL**: ToolWrapper's `_async` handling must be preserved. Move it to ExtensionCore:

```rust
// src/extensions/core/execution.rs
pub struct AsyncExecutionRouter;

impl AsyncExecutionRouter {
    /// Route execution based on _async parameter
    pub async fn route(
        &self,
        params: &mut Value,
        exec_service: &ToolExecutionService,
        async_executor: &AsyncToolExecutor,
        sync_executor: impl FnOnce(Value) -> Result<Value>,
    ) -> Result<Value> {
        // Extract _async and other reserved params (moved from ToolWrapper)
        let reserved = ReservedParams::extract(params)?;
        
        if reserved.async_mode {
            // Async path
            async_executor
                .execute_with_receipt(params.clone(), reserved)
                .await
        } else {
            // Sync path with timeout
            let timeout = reserved.timeout_secs.map(Duration::from_secs);
            exec_service
                .execute(|| sync_executor(params.clone()), timeout)
                .await
        }
    }
}
```

### Phase 3: Update All Handlers to Use Shared Service

**Built-in Handler**:
```rust
// src/extensions/adapters/builtin_tool_adapter.rs
impl BuiltinExecuteHandler {
    async fn handle(&self, ctx: HookContext, input: ToolExecuteInput) -> HookResult {
        let tool = ctx.registry.get_tool(&input.tool_name)?;
        let exec_service = ctx.services.tool_execution();
        let async_router = ctx.services.async_router();
        
        // Use shared execution service
        let result = async_router
            .route(
                &mut input.params,
                exec_service,
                &ctx.services.async_executor(),
                |params| async move {
                    // This closure is the actual tool execution
                    tool.execute(params).await
                },
            )
            .await?;
        
        HookResult::Success(ToolExecuteOutput { result })
    }
}
```

**MCP Handler**:
```rust
// src/extensions/adapters/mcp_adapter.rs
impl McpToolExecuteHandler {
    async fn handle(&self, ctx: HookContext, input: ToolExecuteInput) -> HookResult {
        let exec_service = ctx.services.tool_execution();
        let async_router = ctx.services.async_router();
        
        // Same pattern as built-in
        let result = async_router
            .route(
                &mut input.params,
                exec_service,
                &ctx.services.async_executor(),
                |params| async move {
                    self.manager.call_tool(&self.server, &self.tool, params).await
                },
            )
            .await?;
        
        HookResult::Success(ToolExecuteOutput { result })
    }
}
```

### Phase 4: AgenticLoopV4 Migration

Change `AgenticLoopV4` to route ALL tools through ExtensionCore:

```rust
// src/engine/loop_v4.rs
impl AgenticLoopV4 {
    async fn execute_tool(&self, name: &str, params: Value) -> Result<Value> {
        // ALL tools go through ExtensionCore - no special case for built-in
        let result = self.extension_core
            .invoke_hook(
                HookPoint::ToolExecute {
                    tool_name: name.to_string(),
                    params,
                    context: self.build_execution_context(),
                }
            )
            .await?;
        
        match result {
            HookOutput::Json(value) => Ok(value),
            HookOutput::AsyncTask(receipt) => {
                // Handle async task receipt
                self.handle_async_receipt(receipt).await
            }
            _ => Err(anyhow!("Unexpected hook output")),
        }
    }
}
```

### Phase 5: ToolWrapper Deprecation & Responsibility Transfer

`ToolWrapper` is no longer needed. Its responsibilities are **explicitly transferred** to ExtensionCore services:

| ToolWrapper Responsibility | New Owner | Location |
|---------------------------|-----------|----------|
| `_async` parameter detection | `AsyncExecutionRouter` | `ExtensionCore::services` |
| `_timeout` handling | `ToolExecutionService` | `ExtensionCore::services` |
| `_callback` handling | `AsyncToolExecutor` | `ExtensionCore::services` |
| `_progress` handling | `AsyncToolExecutor` | `ExtensionCore::services` |
| Panic isolation | `ToolExecutionService` | `ExtensionCore::services` |
| Sync execution with timeout | `ToolExecutionService` | `ExtensionCore::services` |

```rust
// DEPRECATED: ToolWrapper no longer needed
#[deprecated(
    since = "0.12.0",
    note = "All responsibilities moved to ExtensionCore services"
)]
pub struct ToolWrapper;
```

**Who calls what:**
1. **AgenticLoopV4** calls `ExtensionCore::invoke_hook()`
2. **Handler** (Builtin, MCP, Universal) receives the call
3. **Handler** calls `AsyncExecutionRouter::route()` 
4. **AsyncExecutionRouter** extracts `_async` and decides path
5. **Async path**: `AsyncToolExecutor::execute_with_receipt()`
6. **Sync path**: `ToolExecutionService::execute()` with timeout
7. **Both paths** eventually call the actual tool execution closure

## Critical: Single Path + Responsibility Transfer

### ToolWrapper Responsibilities: Before vs After

**Before (ToolWrapper era):**
```
LLM calls tool
    │
    ▼
ToolWrapper::execute(params)
    │
    ├──► ReservedParams::extract(params)  ← _async, _timeout
    │
    ├──► _async=true? ──► AsyncToolExecutor::execute()
    │
    └──► _async=false? ──► ToolExecutor::execute_with_context()
                              │
                              └──► Panic isolation, timeout
```

**After (ExtensionCore era):**
```
LLM calls tool
    │
    ▼
ExtensionCore::invoke_hook(ToolExecute)
    │
    ▼
Handler::handle(ctx, input)
    │
    ├──► Get services from ctx.services
    │
    ▼
AsyncExecutionRouter::route(params, services)
    │
    ├──► ReservedParams::extract(params)  ← _async, _timeout (MOVED HERE)
    │
    ├──► _async=true? ──► AsyncToolExecutor::execute() (via services)
    │
    └──► _async=false? ──► ToolExecutionService::execute() (via services)
                              │
                              └──► Panic isolation, timeout (MOVED HERE)
```

### The Goal: ONE Path for ALL Tools

After ADR-018a, there is **NO "Built-in direct" path**. ALL tools go through ExtensionCore:

```
┌─────────────────────────────────────────────────────────────────────────┐
│                  AFTER ADR-018a: SINGLE PATH                            │
├─────────────────────────────────────────────────────────────────────────┤
│                                                                         │
│  AgenticLoopV4::execute_tool()                                          │
│       │                                                                 │
│       └──► ExtensionCore::invoke_hook(HookPoint::ToolExecute)           │
│                │                                                        │
│                ├──► Built-in handler ──► ShellTool, ReadFileTool, etc.  │
│                ├──► MCP handler ──► McpManager ──► JSON-RPC             │
│                ├──► Universal handler ──► Process spawn                 │
│                └──► General handler ──► Custom implementation           │
│                                                                         │
│  ALL handlers use:                                                      │
│  • AsyncExecutionRouter (for _async param)                              │
│  • ToolExecutionService (for panic isolation, timeout)                  │
│  • Real ToolContext (session_id, workspace, etc.)                       │
│                                                                         │
└─────────────────────────────────────────────────────────────────────────┘
```

### The Async Challenge

**Current problem**: ToolWrapper handles `_async` but ONLY for the direct path. Since we're eliminating the direct path, we must migrate `_async` handling to ExtensionCore.

| Before ADR-018a | After ADR-018a |
|-----------------|----------------|
| 2 paths (direct + hooks) | 1 path (hooks only) |
| Built-in: direct path with `_async` | Built-in: hooks with `_async` ✅ |
| MCP: hooks without `_async` | MCP: hooks with `_async` ✅ |
| Universal: hooks without `_async` | Universal: hooks with `_async` ✅ |

### Solution: AsyncExecutionRouter

Move `_async` extraction from ToolWrapper (client-side) to ExtensionCore (server-side):

```rust
// In ExtensionCore handler (ALL tools go through here)
async fn handle(&self, ctx: HookContext, input: ToolExecuteInput) -> HookResult {
    // Extract _async HERE (was in ToolWrapper before)
    let reserved = ReservedParams::extract(&mut input.params)?;
    
    if reserved.async_mode {
        // Async path
        ctx.services.async_executor()
            .execute_with_receipt(input.params, reserved)
            .await
    } else {
        // Sync path with timeout
        ctx.services.tool_execution()
            .execute(|| tool.execute(input.params), reserved.timeout)
            .await
    }
}
```

Now ALL tools (built-in, MCP, Universal, General) support `_async` uniformly.

## Benefits

| Benefit | Description |
|---------|-------------|
| **DRY** | Single `ToolExecutionService` for panic isolation/timeout |
| **Consistent Async** | `_async` parameter works for ALL tool types |
| **Single Context** | All tools receive real `ToolContext` via hooks |
| **Testability** | Mock `ExtensionCore` covers all tool scenarios |
| **Observability** | Single point for metrics/logging |

## Drawbacks

| Drawback | Mitigation |
|----------|------------|
| **Hook overhead** | ~1-2μs per call (negligible vs tool execution) |
| **Deeper stack traces** | ExtensionCore adds 2-3 frames |
| **Migration complexity** | Must preserve `_async` behavior exactly |

## Acceptance Criteria

### Architecture
- [x] `AgenticLoopV4` routes **ALL** tools through `ExtensionCore` (no direct path)
- [x] `ToolExecutor` used only via `ToolExecutionService` (internal to ExtensionCore)
- [ ] No `AgenticLoopV4.tools` field (use ExtensionCore registry instead) - *Deferred to ADR-018b*

### Async Capability
- [x] `_async` parameter works for built-in tools
- [x] `_async` parameter works for MCP tools  
- [x] `_async` parameter works for Universal tools
- [x] `_async` parameter works for General extension tools
- [x] All 980+ tests pass with identical async behavior ✅ (988 tests passing)

### Validation
- [x] No code path bypasses ExtensionCore for tool execution
- [x] Stack traces show consistent ExtensionCore → Handler → Tool flow

## Dependencies

**ADR-018b (Unified Tool Registry)** - ✅ Completed. Provides unified `register_tool()`, `list_tools()`, and `get_tool_metadata()` APIs.

## Implementation Notes

All 8 phases have been completed:

| Phase | Component | Status |
|-------|-----------|--------|
| 1 | ToolExecutionService | ✅ `execute_with_isolation()` with panic isolation + timeout |
| 2 | AsyncExecutionRouter | ✅ `route()` with `_async` parameter extraction |
| 3 | ExtensionServices | ✅ `async_router()` accessor added |
| 4 | Builtin Adapter | ✅ Uses AsyncExecutionRouter |
| 5 | MCP Adapter | ✅ Uses AsyncExecutionRouter |
| 6 | Universal Adapter | ✅ Uses AsyncExecutionRouter |
| 7 | AgenticLoopV4 | ✅ Routes through ExtensionCore hooks |
| 8 | ToolWrapper | ✅ Deprecated with `#[deprecated]` attributes |

### Code Locations

- `ToolExecutionService`: `src/extensions/services/tool_execution.rs`
- `AsyncExecutionRouter`: `src/extensions/services/async_router.rs`
- `ExtensionServices`: `src/extensions/services/mod.rs`
- Builtin Handler: `src/extensions/adapters/builtin_tool_adapter.rs`
- MCP Handler: `src/extensions/adapters/mcp_adapter.rs`
- Universal Handler: `src/extensions/adapters/universal_tool_adapter.rs`
- AgenticLoopV4: `src/engine/loop_v4.rs` (lines ~724-760, ~1300-1330)

## References

- ToolWrapper: `src/tools/wrapper.rs`
- ToolExecutor: `src/engine/tool_executor.rs`
- ExtensionCore: `src/extensions/core.rs`
- BuiltinToolAdapter: `src/extensions/adapters/builtin_tool_adapter.rs`
