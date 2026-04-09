# Phase 3: UnifiedAsyncTool Trait Implementation

## Overview
Created the `UnifiedAsyncTool` trait and supporting infrastructure to provide seamless async tool execution across both native tools and extension-based tools.

## Files Created/Modified

### 1. `src/tools/async_tool.rs` (NEW)
Core trait and adapter implementations for async tool support.

#### UnifiedAsyncTool Trait
```rust
#[async_trait]
pub trait UnifiedAsyncTool: Tool {
    fn supports_async(&self) -> bool;
    fn supports_status_check(&self) -> bool;
    fn supports_cancel(&self) -> bool;
    
    async fn execute_async(&self, params: Value, config: AsyncToolConfig) 
        -> Result<AsyncTaskReceipt>;
    async fn check_status(&self, task_id: &AsyncTaskId) -> Result<AsyncTaskStatus>;
    async fn cancel(&self, task_id: &AsyncTaskId) -> Result<bool>;
    
    fn status_check_tool_name(&self) -> String;
    fn estimated_async_duration_secs(&self, params: &Value) -> Option<u64>;
}
```

#### SyncToAsyncAdapter
Wraps synchronous `Tool` implementations to provide async capabilities:
- Automatically enables async for any sync tool
- Uses `UnifiedAsyncExecutor` for background execution
- Returns `AsyncTaskReceipt` for tracking
- Implements status checking via executor
- Cancellation support via executor

#### ToolAsyncExt Extension Trait
```rust
pub trait ToolAsyncExt: Tool {
    fn into_async(self) -> SyncToAsyncAdapter<Self>;
    fn into_async_with_executor(self, executor) -> SyncToAsyncAdapter<Self>;
}
```

Provides convenient `.into_async()` method on all Tools.

### 2. `src/tools/mod.rs` (MODIFIED)
Added module declaration and re-exports:
```rust
pub mod async_tool;
pub use async_tool::{
    into_async_tool, BoxedAsyncTool, SyncToAsyncAdapter, ToolAsyncExt, UnifiedAsyncTool,
};
```

### 3. `src/extensions/async_integration.rs` (NEW)
Integration between ExtensionAsyncAdapter and UnifiedAsyncTool.

#### ExtensionAsyncTool
Wraps `ExtensionAsyncAdapter` to implement `UnifiedAsyncTool`:
- Provides async execution via extension hooks
- Status checking through adapter
- Cancellation support

#### AsyncToolRegistry
Registry for managing async-capable tools:
- Store and retrieve tools by name
- Check async capability support
- Designed for multi-source tool management

#### ExtensionAsyncAdapterExt
Extension trait for `ExtensionAsyncAdapter`:
```rust
pub trait ExtensionAsyncAdapterExt {
    fn as_async_tool(&self, name, description, params) -> BoxedAsyncTool;
}
```

### 4. `src/extensions/mod.rs` (MODIFIED)
Added async_integration module and re-exported `AsyncReceipt`.

### 5. `src/extensions/core/async_adapter.rs` (MODIFIED)
Added accessor methods:
- `core()` - Access to underlying ExtensionCore
- `executor()` - Access to UnifiedAsyncExecutor

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                        Tool Ecosystem                            │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  ┌──────────────────┐        ┌──────────────────────────┐       │
│  │  Native Tools    │        │  Extension-Based Tools   │       │
│  │                  │        │                          │       │
│  │ ┌──────────────┐ │        │ ┌──────────────────────┐ │       │
│  │ │Sync Tool     │ │        │ │ MCP Tool             │ │       │
│  │ └──────┬───────┘ │        │ └──────────┬───────────┘ │       │
│  │        │         │        │            │             │       │
│  │ ┌──────▼───────┐ │        │ ┌──────────▼───────────┐ │       │
│  │ │into_async()  │ │        │ │ ExtensionAsyncAdapter│ │       │
│  │ └──────┬───────┘ │        │ └──────────┬───────────┘ │       │
│  │        │         │        │            │             │       │
│  └────────┼─────────┘        └────────────┼─────────────┘       │
│           │                               │                     │
│           └───────────────┬───────────────┘                     │
│                           │                                     │
│                    ┌──────▼───────┐                             │
│                    │ UnifiedAsync │                             │
│                    │    Tool      │                             │
│                    └──────┬───────┘                             │
│                           │                                     │
│              ┌────────────┼────────────┐                        │
│              │            │            │                        │
│         execute_async  check_status  cancel                     │
│              │            │            │                        │
│              └────────────┼────────────┘                        │
│                           │                                     │
│                    ┌──────▼───────┐                             │
│                    │UnifiedAsync  │                             │
│                    │  Executor    │                             │
│                    └──────────────┘                             │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

## Usage Examples

### Converting a Sync Tool to Async
```rust
use pekobot::tools::{Tool, ToolAsyncExt};

let my_tool = MySyncTool::new();
let async_tool = my_tool.into_async();

// Now supports async execution
let receipt = async_tool.execute_async(params, config).await?;
```

### Using Extension-Based Async Tools
```rust
use pekobot::extensions::{ExtensionAsyncAdapter, ExtensionAsyncAdapterExt};

let adapter = ExtensionAsyncAdapter::new(core);
let tool = adapter.as_async_tool("my_tool", "Description", params_schema);

// Execute async
let receipt = tool.execute_async(params, config).await?;
let status = tool.check_status(&receipt.task_id).await?;
```

### Creating a Custom Async Tool
```rust
use pekobot::tools::UnifiedAsyncTool;

#[async_trait]
impl UnifiedAsyncTool for MyNativeAsyncTool {
    fn supports_async(&self) -> bool { true }
    
    async fn execute_async(&self, params: Value, config: AsyncToolConfig) 
        -> Result<AsyncTaskReceipt> {
        // Native async implementation
    }
    
    async fn check_status(&self, task_id: &AsyncTaskId) -> Result<AsyncTaskStatus> {
        // Native status checking
    }
    
    async fn cancel(&self, task_id: &AsyncTaskId) -> Result<bool> {
        // Native cancellation
    }
}
```

## Testing

All tests passing:
- `tools::async_tool::tests`: 3 tests passed
- `extensions::async_integration::tests`: 2 tests passed
- `extensions::core::async_adapter::tests`: 3 tests passed

## Key Design Decisions

### 1. Trait Extension Pattern
`UnifiedAsyncTool` extends `Tool` trait rather than replacing it, allowing:
- Gradual adoption of async capabilities
- Backward compatibility with existing tools
- Mix of sync and async tools in the same registry

### 2. Adapter Pattern for Sync Tools
`SyncToAsyncAdapter` wraps sync tools to provide async semantics:
- Uses UnifiedAsyncExecutor for background execution
- Returns receipts for tracking
- Minimal overhead for tools that don't need async

### 3. Extension Integration
`ExtensionAsyncTool` bridges the extension system with the trait:
- Allows extension-based tools to implement UnifiedAsyncTool
- Uses existing ExtensionAsyncAdapter infrastructure
- Seamless integration with hook-based async execution

## Benefits

1. **Unified Interface**: All async tools use the same trait regardless of source
2. **Backward Compatibility**: Sync tools automatically work via adapter
3. **Extensibility**: New async tool sources can implement the trait
4. **Type Safety**: Compile-time guarantees for async capability support
5. **Testability**: Easy to mock and test async tool behavior

## Next Steps (Phase 4)

1. Integrate with ToolExecutor to use UnifiedAsyncTool for async execution
2. Update tool registration to auto-detect and wrap async-capable tools
3. Add async capability metadata to tool manifests
4. Implement progress reporting for long-running async tasks
5. Add metrics and monitoring for async tool execution

## Migration Path

For existing tools wanting native async support:

1. **Option 1 - Use Adapter (Automatic)**
   ```rust
   let async_tool = existing_tool.into_async();
   ```

2. **Option 2 - Implement UnifiedAsyncTool (Native)**
   ```rust
   #[async_trait]
   impl UnifiedAsyncTool for MyTool {
       // Custom async implementation
   }
   ```

3. **Option 3 - Extension-Based**
   - Register async hooks in extension
   - Use ExtensionAsyncAdapter
   - Automatically implements UnifiedAsyncTool
