# ADR-018a: Tool Execution Unification

**Status**: Proposed  
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
│  Path A: Direct Execution (Built-in tools only)                         │
│  ─────────────────────────────────────────────                          │
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
│       │                                                                 │
│       └──► Extension tool? ──Yes──► ExtensionCore::invoke_hook()        │
│                                         │                               │
│                                         ├──► MCP ──► JSON-RPC           │
│                                         ├──► Universal ──► Process      │
│                                         └──► General ──► Custom         │
│                                                                         │
│  Path B: Extension Hooks (MCP, Universal, General)                      │
│  ─────────────────────────────────────────────────                      │
│  ExtensionCore::invoke_hook(HookPoint::ToolExecute)                     │
│       │                                                                 │
│       └──► Handler executes tool directly                               │
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

### Phase 5: ToolWrapper Deprecation

`ToolWrapper` is no longer needed for `_async` handling (moved to ExtensionCore). Deprecate or repurpose:

```rust
// ToolWrapper becomes a thin adapter for backward compatibility
// or is removed entirely
#[deprecated(
    since = "0.12.0",
    note = "Async handling moved to ExtensionCore::AsyncExecutionRouter"
)]
pub struct ToolWrapper;
```

## Critical: Preserving Async Capability

### Current Gap

**ToolWrapper handles `_async` for direct execution ONLY.** When we move to ExtensionCore hooks, we lose this capability unless we explicitly migrate it.

| Path | Before ADR | After ADR (if not fixed) |
|------|------------|--------------------------|
| Built-in direct | ✅ `_async` works | N/A (no direct path) |
| Built-in via hooks | ❌ No `_async` | ✅ Must work |
| MCP via hooks | ❌ No `_async` | ✅ Must work |
| Universal via hooks | ❌ No `_async` | ✅ Must work |

### Solution: AsyncExecutionRouter

The `AsyncExecutionRouter` (shown in Phase 2) extracts `_async` parameter **at the ExtensionCore layer**, ensuring ALL tools get async capability:

```rust
// All tools now support _async
{
    "name": "shell",
    "arguments": {
        "cmd": "long-running-process",
        "_async": true,        // Works for ALL tools
        "_timeout": 300
    }
}
```

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

- [ ] `AgenticLoopV4` routes ALL tools through `ExtensionCore`
- [ ] `_async` parameter works for built-in tools via ExtensionCore
- [ ] `_async` parameter works for MCP tools via ExtensionCore
- [ ] `_async` parameter works for Universal tools via ExtensionCore
- [ ] `ToolExecutor` used only via `ToolExecutionService` (internal)
- [ ] No direct tool execution from `AgenticLoopV4`
- [ ] All 980+ tests pass with identical async behavior

## Dependencies

**ADR-018b (Unified Tool Registry)** must be completed first to provide:
- Tool lookup by name for handlers
- Consistent reserved params configuration
- Whitelist enforcement

## References

- ToolWrapper: `src/tools/wrapper.rs`
- ToolExecutor: `src/engine/tool_executor.rs`
- ExtensionCore: `src/extensions/core.rs`
- BuiltinToolAdapter: `src/extensions/adapters/builtin_tool_adapter.rs`
