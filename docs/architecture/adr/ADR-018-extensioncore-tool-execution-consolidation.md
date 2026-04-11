# ADR-018: ExtensionCore Tool Execution Consolidation

**Status**: Proposed  
**Date**: 2026-04-11  
**Author**: Kimi Code CLI  
**Related**: ADR-017 (Extensions 2.0), ADR-019 (Dynamic Tool/Prompt Updates - future work)

## Context

The current architecture has **dual execution paths** for tools, creating inconsistencies, DRY violations, and security gaps:

1. **Direct Path**: `AgenticLoopV4` calls `ToolExecutor::execute_with_context()` for built-in tools
2. **Hook Path**: Extension-based tools (MCP, Universal, General) execute through `ExtensionCore::invoke_hook(HookPoint::ToolExecute)`

This architectural schism was created during the Extensions 2.0 migration (ADR-017), where built-in tools were partially migrated but the core loop retained direct execution for "performance" reasons.

### Current Architecture (Fragmented)

```
┌─────────────────────────────────────────────────────────────────────┐
│                         AGENTIC LOOP V4                             │
│                                                                     │
│   Built-in Tool? ──Yes──► ToolExecutor::execute_with_context()      │
│        │                                    │                       │
│        │                                    ▼                       │
│        │                           ┌─────────────────┐              │
│        │                           │  Panic Isolate  │              │
│        │                           │  Timeout        │              │
│        │                           │  Context Inject │              │
│        │                           └─────────────────┘              │
│        │                                                            │
│        No                                                           │
│        │                                                            │
│        ▼                                                            │
│   ┌─────────────────────────────────────────────────────────────┐   │
│   │              EXTENSIONCORE HOOK SYSTEM                      │   │
│   │  ┌─────────────┐  ┌─────────────┐  ┌─────────────────────┐  │   │
│   │  │BuiltinAdapter│  │  MCP        │  │  Universal          │  │   │
│   │  │  (unused)    │  │  Adapter    │  │  Adapter            │  │   │
│   │  └─────────────┘  └──────┬──────┘  └──────────┬──────────┘  │   │
│   │                          │                    │             │   │
│   │                          └────────────────────┘             │   │
│   │                                    │                        │   │
│   │                                    ▼                        │   │
│   │                          ┌─────────────────┐                │   │
│   │                          │ ToolExecution   │                │   │
│   │                          │ Service         │                │   │
│   │                          │ (params inject) │                │   │
│   │                          └─────────────────┘                │   │
│   └─────────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────────┘
```

### Problems Identified

| Issue | Description | Impact |
|-------|-------------|--------|
| **DRY Violation** | `ToolExecutor` and `TaskManager` duplicate panic isolation, timeout logic | Maintenance burden, bug risk |
| **Inconsistent Execution** | Built-in tools bypass ExtensionCore hooks; others use them | Behavior divergence |
| **Inconsistent Descriptions** | `llm_description()` vs `description()` in different paths | LLM sees wrong tool info |
| **Security Gap** | Permission checks needed in multiple places | Tools may bypass checks |
| **Test Duplication** | Two paths to test for every tool execution scenario | Testing overhead |
| **Lifecycle Mismatch** | System prompt built at construction vs tools added via hooks | Stale tool list |

## Decision

**Consolidate ALL tool execution through ExtensionCore hooks**, with `ToolExecutor` becoming the **implementation detail** called by handlers, not a public API.

### Target Architecture (Unified)

```
┌─────────────────────────────────────────────────────────────────────┐
│                         AGENTIC LOOP V4                             │
│                                                                     │
│   ALL tools ───────────────────────► ExtensionCore::invoke_hook()   │
│                                            │                        │
│                                            ▼                        │
│   ┌─────────────────────────────────────────────────────────────┐   │
│   │              EXTENSIONCORE HOOK SYSTEM                      │   │
│   │  ┌─────────────┐  ┌─────────────┐  ┌─────────────────────┐  │   │
│   │  │BuiltinAdapter│  │  MCP        │  │  Universal          │  │   │
│   │  │  (used!)     │  │  Adapter    │  │  Adapter            │  │   │
│   │  └──────┬──────┘  └──────┬──────┘  └──────────┬──────────┘  │   │
│   │         │                │                    │             │   │
│   │         └────────────────┼────────────────────┘             │   │
│   │                          │                                  │   │
│   │                          ▼                                  │   │
│   │              ┌─────────────────────┐                        │   │
│   │              │   TOOL EXECUTOR     │                        │   │
│   │              │  (implementation)   │                        │   │
│   │              │ • Panic isolation   │                        │   │
│   │              │ • Timeout           │                        │   │
│   │              │ • Context injection │                        │   │
│   │              └─────────────────────┘                        │   │
│   └─────────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────────┘
```

**Key Changes:**
1. `AgenticLoopV4` routes ALL tools through `ExtensionCore`
2. `BuiltinToolAdapter` becomes the handler for ALL built-in tools
3. `ToolExecutor` is private implementation, not publicly exposed
4. `TaskManager` tool execution deprecated and migrated

## Implementation

### Phase 1: Enable BuiltinToolAdapter for All Built-ins

**File**: `src/extensions/adapters/builtin_tool_adapter.rs`

Current state: `BuiltinToolAdapter` exists but is unused for built-in tools. Modify to handle all built-in tool executions:

```rust
// In BuiltinExecuteHandler::handle()
pub async fn handle(&self, context: &HookContext, input: ToolExecuteInput) -> HookResult {
    // All built-in tools route through here
    let tool = self.registry.get_tool(&input.tool_name)?;
    
    // Call unified ToolExecutor
    let result = self.tool_executor
        .execute_with_context(tool, input.params, &execution_context)
        .await?;
    
    HookResult::Success(ToolExecuteOutput { result })
}
```

**Changes to `AgenticLoopV4`**:

```rust
// BEFORE (direct execution)
if let Some(tool) = self.tools.iter().find(|t| t.name() == name) {
    let result = self.tool_executor.execute_with_context(tool, params, context).await?;
}

// AFTER (through ExtensionCore)
let result = self.extension_core
    .invoke_hook(
        HookPoint::ToolExecute {
            tool_name: name.to_string(),
            params,
            context: execution_context,
        }
    )
    .await?;
```

### Phase 2: Deprecate TaskManager Tool Execution

**File**: `src/engine/task_manager.rs`

Migrate all callers to use ExtensionCore instead:

| Current Caller | Migration Target |
|----------------|------------------|
| `stateless_manager.rs:269` | ExtensionCore hook |
| `agent/subagent_executor.rs:320` | ExtensionCore hook |

### Phase 3: Remove Direct ToolExecutor Access

Make `ToolExecutor` an internal implementation detail:

1. Move `ToolExecutor` to `src/extensions/services/tool_executor.rs`
2. Only callable from ExtensionCore handlers
3. Remove from `AgenticLoopV4` public API

### Phase 4: Unify Tool Description Methods

Standardize on single method for tool descriptions:

```rust
pub trait Tool: Send + Sync {
    // Remove: description() vs llm_description() confusion
    
    // Single method with clear contract
    fn description(&self) -> ToolDescription;
}

pub struct ToolDescription {
    pub name: String,
    pub description: String,
    pub parameters: ParameterSchema,
    pub returns: ReturnSchema,
}
```

### Phase 5: Migration of Async Tool Execution

**File**: `src/engine/async_tool_executor.rs`

The async executor should also route through ExtensionCore:

```rust
// AsyncToolExecutor becomes a handler for HookPoint::ToolExecuteAsync
impl HookHandler for AsyncToolExecuteHandler {
    async fn handle(&self, context: &HookContext, input: AsyncToolInput) -> HookResult {
        // Check if tool implements UnifiedAsyncTool
        // If yes, execute async path
        // If no, delegate to sync ToolExecutor
    }
}
```

## Benefits

| Benefit | Description |
|---------|-------------|
| **Single Execution Path** | All tools go through same code path |
| **Centralized Security** | Permission checks at ExtensionCore layer cover ALL tools |
| **DRY Compliance** | No duplicate panic isolation, timeout, context injection |
| **Consistent Behavior** | Reserved params, audit logging work identically for all tools |
| **Easier Testing** | One path to test, mock ExtensionCore for tests |
| **Future Extensibility** | Middleware (rate limiting, observability) applies to all |

## Drawbacks

| Drawback | Mitigation |
|----------|------------|
| **Hook Overhead** | ~1-2μs per call (negligible vs tool execution time) |
| **Breaking Change** | Internal refactor, no public API changes |
| **Migration Effort** | ~2-3 days to complete all phases |

## Acceptance Criteria

- [ ] All built-in tools execute through `BuiltinToolAdapter`
- [ ] No direct `ToolExecutor` calls from `AgenticLoopV4`
- [ ] `TaskManager` tool execution removed or delegated
- [ ] Single tool description method used everywhere
- [ ] All 980 existing tests pass
- [ ] New test: Verify ALL tools route through ExtensionCore

## Related Work

This ADR **blocks** ADR-019 (Dynamic Tool and Prompt Updates). Once unified:
- Permission checks only needed in ExtensionCore
- Tool registration only needed in one path
- Dynamic updates only need to handle one lifecycle

## References

- Current direct execution: `src/engine/loop_v4.rs:722-757`
- ExtensionCore hooks: `src/extensions/core.rs`
- BuiltinToolAdapter (unused): `src/extensions/adapters/builtin_tool_adapter.rs:145-179`
- TaskManager (legacy): `src/engine/task_manager.rs`
- ADR-017 (Extensions 2.0): `docs/architecture/adr/ADR-017-extensions-2-0.md`
