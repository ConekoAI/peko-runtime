# Phase 4: ToolExecutor Integration and Capability Detection

## Overview
Created the `AsyncToolExecutor` that integrates the `UnifiedAsyncTool` trait with the engine's tool execution system. Added capability detection, progress reporting, and seamless sync-to-async fallback.

## Files Created/Modified

### 1. `src/engine/async_tool_executor.rs` (NEW)
Enhanced executor with async tool support.

#### Key Structures

**AsyncToolExecutor**
```rust
pub struct AsyncToolExecutor {
    sync_executor: ToolExecutor,          // Base sync execution
    async_executor: UnifiedAsyncExecutor,  // Async task management
    capability_cache: Arc<RwLock<HashMap<String, AsyncCapability>>>,
    progress_callbacks: Arc<RwLock<HashMap<String, ProgressCallback>>>,
    default_config: AsyncToolConfig,
}
```

**AsyncCapability**
```rust
pub struct AsyncCapability {
    pub supports_async: bool,
    pub supports_status_check: bool,
    pub supports_cancel: bool,
    pub supports_progress: bool,
    pub estimated_duration_secs: Option<u64>,
}
```

**ToolProgress**
```rust
pub struct ToolProgress {
    pub task_id: String,
    pub tool_name: String,
    pub percent: u8,
    pub message: String,
    pub metadata: Option<Value>,
}
```

#### Key Methods

**Execution**
- `execute_async()` - Execute tool asynchronously
- `execute_with_progress()` - Execute with progress callbacks
- `execute_sync_fallback()` - Wrap sync execution for async interface

**Status & Control**
- `check_status()` - Query task status
- `cancel()` - Cancel running task
- `wait_for_completion()` - Block until task completes

**Capability Detection**
- `supports_async()` - Check if tool supports async
- `detect_capabilities()` - Full capability scan

#### Factory Pattern

**AsyncToolExecutorFactory**
- Creates executors with shared state
- Enables consistent configuration across components

```rust
let factory = AsyncToolExecutorFactory::with_timeout(Duration::from_secs(60));
let executor = factory.create_executor();
```

### 2. `src/engine/mod.rs` (MODIFIED)
Added module declaration and re-exports:
```rust
pub mod async_tool_executor;
pub use async_tool_executor::{AsyncCapability, AsyncToolExecutor, ToolProgress};
```

### 3. `src/tools/traits.rs` (MODIFIED)
Added `as_any()` method to Tool trait for downcasting:
```rust
fn as_any(&self) -> &dyn std::any::Any;
```

### 4. Test Files Updated
- `src/tools/async_tool.rs` - Added `as_any()` to MockTool
- `src/engine/tool_executor.rs` - Added `as_any()` to MockTool
- `src/engine/task_manager.rs` - Added `as_any()` to MockTool

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                    AsyncToolExecutor                             │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  ┌──────────────────────────────────────────────────────────┐   │
│  │ Tool Registration & Discovery                             │   │
│  │                                                           │   │
│  │  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐       │   │
│  │  │ Native Tool │  │ MCP Tool    │  │ Universal   │       │   │
│  │  │             │  │             │  │ Tool        │       │   │
│  │  └──────┬──────┘  └──────┬──────┘  └──────┬──────┘       │   │
│  │         │                 │                 │              │   │
│  │         └─────────────────┼─────────────────┘              │   │
│  │                           ▼                                │   │
│  │                  ┌─────────────────┐                       │   │
│  │                  │ UnifiedAsyncTool│                       │   │
│  │                  │ Trait Object    │                       │   │
│  │                  └────────┬────────┘                       │   │
│  └───────────────────────────┼────────────────────────────────┘   │
│                              │                                     │
│  ┌───────────────────────────▼────────────────────────────────┐   │
│  │                    Execution Flow                           │   │
│  │                                                             │   │
│  │  execute_async()                                            │   │
│  │       │                                                     │   │
│  │       ├─► check capability cache                            │   │
│  │       │                                                     │   │
│  │       ├─► supports_async? ──yes─► tool.execute_async()      │   │
│  │       │                            │                        │   │
│  │       │                            ▼                        │   │
│  │       │                     UnifiedAsyncExecutor            │   │
│  │       │                            │                        │   │
│  │       │                            ▼                        │   │
│  │       │                     AsyncTaskReceipt                 │   │
│  │       │                                                     │   │
│  │       └─► no ─► SyncToAsyncAdapter                         │   │
│  │                      │                                      │   │
│  │                      ▼                                      │   │
│  │               spawn in background                          │   │
│  │                      │                                      │   │
│  │                      ▼                                      │   │
│  │               AsyncTaskReceipt                             │   │
│  │                                                             │   │
│  └─────────────────────────────────────────────────────────────┘   │
│                                                                    │
│  ┌─────────────────────────────────────────────────────────────┐   │
│  │                    Status & Progress                        │   │
│  │                                                             │   │
│  │  check_status(task_id) ◄──── AsyncTaskRegistry              │   │
│  │  cancel(task_id)       ◄────                                │   │
│  │  on_progress(callback) ◄──── Progress Callbacks             │   │
│  │                                                             │   │
│  └─────────────────────────────────────────────────────────────┘   │
│                                                                    │
└────────────────────────────────────────────────────────────────────┘
```

## Usage Examples

### Basic Async Execution
```rust
let executor = AsyncToolExecutor::new();

// Execute async
let receipt = executor.execute_async(
    Arc::new(my_tool),
    params,
    &context
).await?;

// Check status
loop {
    let status = executor.check_status("my_tool", &receipt.task_id).await?;
    if status.is_terminal() {
        break;
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
}
```

### With Progress Reporting
```rust
let executor = AsyncToolExecutor::new();

let receipt = executor.execute_with_progress(
    Arc::new(my_tool),
    params,
    &context,
    |progress| {
        println!("{}%: {}", progress.percent, progress.message);
    }
).await?;
```

### Capability Detection
```rust
let executor = AsyncToolExecutor::new();
let tool = Arc::new(my_tool);

// Check if supports async
if executor.supports_async(tool.name()).await {
    let cap = executor.detect_capabilities(&tool).await;
    println!("Supports status check: {}", cap.supports_status_check);
}
```

### Factory Pattern
```rust
// Create factory with shared state
let factory = AsyncToolExecutorFactory::with_timeout(
    Duration::from_secs(60)
);

// Create multiple executors sharing the same async executor
let executor1 = factory.create_executor();
let executor2 = factory.create_executor();
```

## Testing

All 3 new tests passing:
- `test_async_executor_creation` - Basic construction
- `test_async_executor_with_timeout` - Custom timeout config
- `test_capability_detection` - Capability scanning structure

Total: 1072 tests passed, 0 failed

## Integration Points

### With Extension System
```rust
use crate::extensions::async_integration::ExtensionAsyncTool;

let ext_tool = ExtensionAsyncTool::new(adapter, name, desc, params);
let receipt = ext_tool.execute_async(params, config).await?;
```

### With Tool Registry
```rust
// Register async-capable tool
registry.register(Box::new(my_async_tool));

// Later, retrieve and execute
if let Some(tool) = registry.get("my_tool") {
    let receipt = executor.execute_async(tool, params, &context).await?;
}
```

### With Engine
```rust
// In Engine::run()
let async_executor = AsyncToolExecutor::with_timeout(
    Duration::from_secs(config.tool_timeout_secs)
);

// Use for tool execution in agentic loop
let receipt = async_executor.execute_async(tool, params, &context).await?;
```

## Future Enhancements

### Phase 5: Full Integration
1. Integrate with `AgenticLoopV4` for automatic async tool selection
2. Add async tool syntax for LLM (e.g., `tool.execute_async()`)
3. Implement streaming results for long-running tasks
4. Add metrics collection for async operations

### Phase 6: Advanced Features
1. Parallel async tool execution
2. Async tool composition (chaining)
3. Result caching for async operations
4. Automatic retry with exponential backoff

## Migration Guide

### For Tool Authors

**Sync Tool (Automatic)**
```rust
// No changes needed - works via fallback
```

**Native Async Tool**
```rust
#[async_trait]
impl UnifiedAsyncTool for MyTool {
    fn supports_async(&self) -> bool { true }
    
    async fn execute_async(&self, params: Value, config: AsyncToolConfig) 
        -> Result<AsyncTaskReceipt> {
        // Native implementation
    }
    
    // ... other methods
}
```

### For Engine Users

**Before**
```rust
let executor = ToolExecutor::new();
let result = executor.execute(tool, params).await?;
```

**After**
```rust
let executor = AsyncToolExecutor::new();

if executor.supports_async(tool.name()).await {
    let receipt = executor.execute_async(tool, params, &context).await?;
    // Handle async
} else {
    let result = executor.sync_executor().execute(tool, params).await?;
}
```

## Benefits

1. **Unified Interface**: Same API for sync and async tools
2. **Capability Detection**: Automatic feature discovery
3. **Progress Visibility**: Real-time feedback for long operations
4. **Seamless Fallback**: Sync tools work without modification
5. **Resource Sharing**: Factory pattern enables efficient executor reuse
6. **Type Safety**: Compile-time guarantees for async operations
