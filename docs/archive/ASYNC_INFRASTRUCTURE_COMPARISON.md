# Native vs Framework-Level Async Infrastructure Comparison

## Executive Summary

**YES, they use the SAME infrastructure** (`UnifiedAsyncExecutor`), but with critical differences in how they're configured and invoked.

## Infrastructure Components

Both native and framework-level async use:
- `UnifiedAsyncExecutor` - The core async task manager
- `AsyncTaskRegistry` - Task tracking
- `AsyncResultQueueManager` - Result delivery
- `AsyncToolConfig` - Configuration (timeout, delivery mode, etc.)

## Key Differences

| Aspect | Shell Native Async | Framework-Level Async (ToolWrapper) |
|--------|-------------------|-------------------------------------|
| **Entry point** | `ShellTool::execute()` with `async: true` | `ToolWrapper::execute()` with `_async: true` |
| **Executor source** | Must be injected via `ShellTool::with_async()` | From `WrapperConfig.async_executor` |
| **Configuration** | Hardcoded in shell tool | Configurable via `WrapperConfig` |
| **Return format** | Shell-specific receipt JSON | Standard receipt via executor |
| **Works today?** | **NO** - executor never injected | Yes - when properly configured |

## Critical Finding: Native Async is Broken

### Evidence

1. **Shell tool creation** (`src/tools/factory.rs:397`):
```rust
let shell = Arc::new(ShellTool::new().with_workspace(&workspace));
// NOTE: .with_async(executor) is NEVER called!
```

2. **Shell tool execution** (`src/tools/shell.rs:226-229`):
```rust
async fn execute_async(...) -> Result<...> {
    let executor = self
        .executor
        .clone()
        .ok_or_else(|| anyhow::anyhow!("Async mode not configured for shell tool"))?;
    // ...
}
```

3. **When LLM uses native async**:
```json
{"command": "sleep 10", "async": true}
```

**Result**: `Error: Async mode not configured for shell tool`

### Why It's Broken

The shell tool was designed to receive a `UnifiedAsyncExecutor` via `with_async()`, but:
- `BuiltinToolAdapter::register_tool()` never calls it
- `ToolFactory` never provides the executor
- Only tests call `with_async()`

## Execution Flow Comparison

### Path 1: Shell Native Async (Currently Broken)

```
LLM -> {"command": "ls", "async": true}
  ↓
ExtensionCore::invoke_hook(ToolExecute)
  ↓
BuiltinExecuteHandler::handle()
  ↓
ShellTool::execute({"command": "ls", "async": true})
  ↓
ShellTool::execute_async()  // tries to use self.executor
  ↓
ERROR: "Async mode not configured for shell tool"
```

### Path 2: Framework-Level Async (Works)

```
LLM -> {"command": "ls", "_async": true}
  ↓
ToolWrapper::execute()
  ↓
ReservedParams::extract() -> removes "_async", "_timeout"
  ↓
ToolWrapper::execute_async()
  ↓
AsyncToolExecutor::execute_async(ShellTool, {"command": "ls"})
  ↓
UnifiedAsyncExecutor::execute() // framework's executor
  ↓
ShellTool::execute({"command": "ls"}) // in background task
  ↓
SUCCESS: Returns receipt
```

## Infrastructure Deep Dive

### UnifiedAsyncExecutor

Both paths ultimately call `UnifiedAsyncExecutor::execute()`:

```rust
// src/agent/async_tool_framework.rs
pub async fn execute<F, Fut>(
    &self,
    task_id: String,
    tool_name: &str,
    params: Value,
    session_key: String,
    config: AsyncToolConfig,
    operation: F,
) -> Result<AsyncTaskReceipt>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = Result<AsyncTaskResult>> + Send,
```

### Difference in Invocation

|  | Shell Native | Framework-Level |
|--|-------------|-----------------|
| **Direct call** | `executor.execute(..., shell_closure)` | `executor.execute(..., tool_closure)` |
| **Closure content** | Spawns shell process directly | Calls `tool.execute(params)` |
| **Task type** | `AsyncTaskResult::Process` | `AsyncTaskResult::Generic` or `::Process` |

## The Double Async Problem (If Native Were Fixed)

If shell's native async were configured, using both would cause:

```json
{
    "command": "./long-task.sh",
    "async": true,        // Shell native
    "_async": true        // Framework-level
}
```

**Execution flow**:
```
ToolWrapper::execute()  // sees _async: true
  ↓
AsyncToolExecutor::execute_async(ShellTool, params)
  ↓
UnifiedAsyncExecutor::execute(shell_task)
  ↓  [Background task starts]
ShellTool::execute({"command": "./long-task.sh", "async": true})
  ↓
ShellTool::execute_async()  // sees async: true
  ↓
UnifiedAsyncExecutor::execute(shell_closure)  // AGAIN!
  ↓  [Nested background task]
Actual shell execution
```

**Result**: 
- Task wrapped in task
- Two receipts generated
- Unpredictable behavior

## Current State in Different Execution Paths

| Path | Shell Native Async | Framework Async |
|------|-------------------|-----------------|
| **Extension Framework** | ❌ BROKEN | ❌ Not used |
| **ToolFactory (legacy)** | ❌ BROKEN | ❌ Not used |
| **Agentic Loop + ToolWrapper** | ⚠️ Works sync only | ✅ Available |

## Recommendation

### Immediate Fix

Remove shell's native `async` parameter since:
1. It's currently broken (no executor injected)
2. Framework-level async works and is more flexible
3. Prevents double-async confusion

### Code Change

```rust
// In ShellArgs - REMOVE:
pub r#async: Option<bool>,  // DELETE

// In ShellTool::execute() - SIMPLIFY:
async fn execute(&self, params: Value) -> Result<Value> {
    let args: ShellArgs = serde_json::from_value(params)?;
    let timeout_ms = args.timeout_ms.min(MAX_TIMEOUT_MS);
    
    // Always execute synchronously - framework handles async
    self.execute_command(&args.command, timeout_ms, args.cwd.as_deref(), args.stdin.as_deref()).await
}
```

### Long-Term

Migrate shell tool to use `UnifiedAsyncTool` trait properly, allowing framework to manage all async execution.

## Summary

| Question | Answer |
|----------|--------|
| Same infrastructure? | **YES** - both use `UnifiedAsyncExecutor` |
| Same entry point? | **NO** - native via tool, framework via wrapper |
| Currently working? | **NO** - native is broken, framework works |
| Should both exist? | **NO** - causes confusion and double-async risk |
