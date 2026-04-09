# Phase 2: MCP and Universal Tool Async Hook Support

## Overview
Updated both MCP and Universal Tool adapters to implement async hook handlers for the ExtensionAsyncAdapter integration.

## Changes

### MCP Adapter (`mcp_adapter.rs`)

#### New Imports
```rust
use crate::extensions::types::{AsyncReceipt, HookInput};
use crate::agent::async_tool_framework::AsyncTaskStatus;
use uuid::Uuid;
```

#### New Hook Bindings (resolve_hooks)
- `ToolExecuteAsync` - For async tool execution registration
- `ToolCheckStatus` - For polling task status
- `ToolCancel` - For cancelling async tasks

#### New Handler Types
1. **McpToolExecuteAsyncHandler**
   - Generates unique task IDs: `mcp:{server}:{tool}:{uuid}`
   - Returns `AsyncReceipt` with metadata
   - Wraps synchronous MCP execution for async compatibility

2. **McpToolCheckStatusHandler**
   - Returns `AsyncTaskStatus::Pending` (MCP doesn't have native async tracking)
   - Placeholder for future MCP async protocol support

3. **McpToolCancelHandler**
   - Returns `false` (MCP standard doesn't support cancellation)
   - Placeholder for future enhancement

#### Updated Functions
- `register_servers_with_core()` - Now registers 6 hooks per server (was 3)
- `load_and_register_servers()` - Updated hook count divisor to 6

### Universal Tool Adapter (`universal_tool_adapter.rs`)

#### New Imports
```rust
use crate::extensions::types::{AsyncReceipt, HookInput};
use crate::agent::async_tool_framework::AsyncTaskStatus;
use uuid::Uuid;
```

#### New Hook Bindings (resolve_hooks)
- `ToolExecuteAsync` - For async tool execution
- `ToolCheckStatus` - For polling task status
- `ToolCancel` - For cancelling async tasks

#### New Handler Types
1. **UniversalToolExecuteAsyncHandler**
   - Generates unique task IDs: `universal:{tool}:{uuid}`
   - Returns `AsyncReceipt` with tool metadata

2. **UniversalToolCheckStatusHandler**
   - Returns `AsyncTaskStatus::Pending`
   - Placeholder for native async tracking

3. **UniversalToolCancelHandler**
   - Returns `false`
   - Placeholder for future cancellation support

#### Updated Functions
- `register_tools_with_core()` - Now registers 6 hooks per tool (was 3)
- `load_and_register_tools()` - Updated hook count divisor to 6
- Test updated to expect 12 hooks for 2 tools (was 6)

## Hook Registration Pattern

Each tool/server now registers 6 hooks:
1. `ToolRegister` - Tool discovery/registration
2. `PromptSystemSection` - Prompt injection
3. `ToolExecute` - Synchronous execution
4. `ToolExecuteAsync` - Asynchronous execution
5. `ToolCheckStatus` - Task status polling
6. `ToolCancel` - Task cancellation

## Task ID Format

### MCP Tools
```
mcp:{server_name}:{tool_name}:{uuid}
```
Example: `mcp:filesystem:read_file:550e8400-e29b-41d4-a716-446655440000`

### Universal Tools
```
universal:{tool_name}:{uuid}
```
Example: `universal:calculator:550e8400-e29b-41d4-a716-446655440000`

## AsyncReceipt Structure

```rust
AsyncReceipt {
    task_id: String,              // Unique task identifier
    estimated_duration_secs: Option<u64>,  // Estimated time (none for now)
    check_status_tool: String,    // Tool name for status checks
    metadata: Option<Value>,      // Additional context
}
```

## Testing

All tests passing:
- MCP adapter: 4 tests passed
- Universal Tool adapter: 6 tests passed

## Next Steps (Phase 3)

1. Create `UnifiedAsyncTool` trait for seamless integration
2. Implement trait for all async-capable tools
3. Add capability detection for native async support
4. Add async-specific metadata to manifests

## Notes

- Both adapters currently return `Pending` for status checks - this is a placeholder
- Cancellation returns `false` as neither MCP nor Universal Tools have native cancel support
- The async execution wraps synchronous execution for backward compatibility
- Native async support can be added incrementally without breaking changes
