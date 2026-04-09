# Phase 1: ExtensionAsyncAdapter Implementation

## Overview
Created `ExtensionAsyncAdapter` to bridge ExtensionCore hooks with UnifiedAsyncExecutor, enabling extensions to register async tool execution capabilities.

## Files Created/Modified

### 1. `src/extensions/core/async_adapter.rs` (NEW)
Main adapter implementation bridging ExtensionCore and UnifiedAsyncExecutor.

**Key Structures:**
- `ExtensionAsyncAdapter`: Main adapter struct
  - `core: Arc<ExtensionCore>` - Extension core for hook registration
  - `executor: UnifiedAsyncExecutor` - Unified executor for task management
  - `session_key: String` - Session identifier

- `AsyncCapability`: Extension capability detection
  - `supports_async_execution: bool`
  - `supports_status_check: bool`
  - `supports_cancel: bool`
  - `status_tool_name: Option<String>`

**Key Methods:**
- `new(core, session_key)` - Create adapter with ExtensionCore
- `with_executor(core, executor, session_key)` - Create with custom executor
- `execute_async(tool_name, params, config)` - Route async execution through extension hooks
- `fallback_async(tool_name, params, config)` - Spawn sync execution in background
- `supports_async(tool_name)` - Check if extension supports async
- `check_status(tool_name, task_id)` - Check task status
- `cancel(tool_name, task_id)` - Cancel async task

**Hook Registration Pattern:**
```rust
let hook_point = HookPoint::tool_execute_async(tool_name);
let result = self.core.execute_hook(hook_point, input).await?;
```

### 2. `src/extensions/core/mod.rs` (MODIFIED)
Added re-export:
```rust
pub mod async_adapter;
pub use async_adapter::ExtensionAsyncAdapter;
```

## Hook Point Integration

### New Hook Points (from Phase 0)
- `ToolExecuteAsync { tool_name }` - Returns AsyncReceipt
- `ToolCheckStatus { tool_name }` - Returns TaskStatus
- `ToolCancel { tool_name }` - Returns bool

### Input/Output Types
- `HookInput::TaskStatus { task_id, tool_name }`
- `HookInput::TaskCancel { task_id, tool_name }`
- `HookOutput::Receipt(AsyncReceipt)`
- `HookOutput::TaskStatus(AsyncTaskStatus)`
- `HookOutput::Bool(bool)`

## Fallback Strategy

When an extension doesn't implement async hooks:
1. Wrap synchronous `ToolExecute` execution
2. Spawn in background via UnifiedAsyncExecutor
3. Return receipt with check_status_tool pointing to unified executor

This ensures backward compatibility - existing extensions work without modification.

## Testing

All 3 tests passing:
- `test_async_adapter_creation` - Basic adapter construction
- `test_async_adapter_execution_without_handler` - Fallback behavior
- `test_async_capability_detection` - Capability query

## Next Steps (Phase 2)

Update MCP and Universal Tool adapters to implement async hooks:
1. Implement `ToolExecuteAsync` for MCP tools
2. Implement `ToolCheckStatus` for MCP tools
3. Implement `ToolCancel` for MCP tools
4. Same for Universal Tool adapter
5. Add async capability metadata to manifests

## Usage Example

```rust
// Create adapter
let adapter = ExtensionAsyncAdapter::new(extension_core, "session-123");

// Check if async supported
if adapter.supports_async("long_running_tool").await {
    // Execute async
    let receipt = adapter.execute_async(
        "long_running_tool",
        json!({ "input": "data" }),
        AsyncToolConfig::default()
    ).await?;
    
    // Check status later
    let status = adapter.check_status("long_running_tool", &receipt.task_id).await;
}
```
