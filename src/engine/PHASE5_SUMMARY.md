# Phase 5: AgenticLoop Integration and Metrics

## Overview
Integrated async tool execution into the agentic loop with automatic capability detection, progress reporting, and comprehensive metrics collection.

## Files Created/Modified

### 1. `src/engine/async_agentic_loop.rs` (NEW)
Enhanced agentic loop with async tool execution capabilities.

#### Key Structures

**AsyncAgenticConfig**
```rust
pub struct AsyncAgenticConfig {
    pub auto_async: bool,              // Enable automatic async detection
    pub async_timeout_secs: u64,       // Timeout for async operations
    pub streaming_results: bool,       // Enable streaming results
    pub progress_interval_ms: u64,     // Progress update interval
    pub force_async: bool,             // Force async for all tools (debug)
    pub collect_metrics: bool,         // Collect detailed metrics
}
```

**AsyncAgenticLoop**
```rust
pub struct AsyncAgenticLoop {
    inner: AgenticLoopV4,                    // Underlying v4 loop
    async_executor: AsyncToolExecutor,       // Async execution
    config: AsyncAgenticConfig,
    metrics: RwLock<AsyncToolMetrics>,
    capability_cache: RwLock<HashMap<String, AsyncCapability>>,
}
```

**AsyncToolMetrics**
```rust
pub struct AsyncToolMetrics {
    pub async_executions: u64,
    pub sync_executions: u64,
    pub cancellations: u64,
    pub timeouts: u64,
    pub avg_execution_time_ms: u64,
    pub async_capable_tools: Vec<String>,
}
```

#### Key Features

**Automatic Async Detection**
```rust
async fn should_use_async(&self, tool_name: &str) -> bool {
    if self.config.force_async { return true; }
    if !self.config.auto_async { return false; }
    
    // Check capability cache
    self.async_executor.supports_async(tool_name).await
}
```

**Dual Execution Modes**
- `execute_tool_sync()` - Traditional synchronous execution
- `execute_tool_async()` - Async with progress tracking

**Progress Reporting**
```rust
async fn poll_with_progress(&self, ...) -> Result<()> {
    loop {
        tokio::time::sleep(interval).await;
        let status = self.async_executor.check_status(...).await?;
        
        // Emit progress via AgenticEvent::ToolUpdate
        on_event(AgenticEvent::ToolUpdate {
            progress_percent: Some(percent),
            ...
        });
    }
}
```

**Async Tool Syntax for LLM**
```rust
pub fn get_async_tool_prompt_section(&self) -> String {    r#"## Async Tool Execution

Some tools support asynchronous execution for long-running operations:

### When to use async:
- File operations on large directories
- Network requests with uncertain timing
- Complex computations that may take minutes

### Async tool syntax:
```json
{
  "name": "tool_name",
  "arguments": { ... },
  "async": true,
  "timeout_seconds": 300
}
```
"#.to_string()
}
```

### 2. `src/observability/async_tool_metrics.rs` (NEW)
Comprehensive metrics collection for async tool execution.

#### Key Structures

**TaskExecutionMetrics**
```rust
pub struct TaskExecutionMetrics {
    pub task_id: AsyncTaskId,
    pub tool_name: String,
    pub start_time: Instant,
    pub end_time: Option<Instant>,
    pub final_status: Option<AsyncTaskStatus>,
    pub was_async: bool,
    pub progress_updates: u32,
}
```

**AsyncToolExecutionMetrics (Aggregated)**
```rust
pub struct AsyncToolExecutionMetrics {
    pub async_executions: u64,
    pub sync_executions: u64,
    pub successful_executions: u64,
    pub failed_executions: u64,
    pub cancelled_executions: u64,
    pub timeouts: u64,
    pub avg_async_duration_ms: f64,
    pub avg_sync_duration_ms: f64,
    pub tool_metrics: HashMap<String, ToolSpecificMetrics>,
}
```

**AsyncToolMetricsCollector**
```rust
pub struct AsyncToolMetricsCollector {
    active_tasks: Arc<RwLock<HashMap<AsyncTaskId, TaskExecutionMetrics>>>,
    completed_tasks: Arc<RwLock<Vec<TaskExecutionMetrics>>>,
    aggregated: Arc<RwLock<AsyncToolExecutionMetrics>>,
}
```

#### Features
- Track active and completed tasks
- Calculate running averages
- Tool-specific metrics aggregation
- Report generation

### 3. `src/engine/loop_v4.rs` (MODIFIED)
Added `system_prompt()` accessor method.

### 4. `src/engine/mod.rs` (MODIFIED)
Added module declarations and re-exports.

### 5. `src/observability/mod.rs` (MODIFIED)
Added async_tool_metrics module.

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                    AsyncAgenticLoop                              │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  ┌──────────────────────────────────────────────────────────┐   │
│  │ Tool Execution Flow                                       │   │
│  │                                                           │   │
│  │  User Request                                             │   │
│  │       │                                                   │   │
│  │       ▼                                                   │   │
│  │  AgenticLoopV4 ──▶ LLM                                   │   │
│  │       │                                                   │   │
│  │       ▼                                                   │   │
│  │  ToolCall Request                                         │   │
│  │       │                                                   │   │
│  │       ▼                                                   │   │
│  │  should_use_async()?                                      │   │
│  │       │                                                   │   │
│  │       ├─► Yes ──▶ execute_tool_async()                   │   │
│  │       │           │                                       │   │
│  │       │           ├─▶ AsyncToolExecutor                  │   │
│  │       │           │   └─▶ UnifiedAsyncExecutor           │   │
│  │       │           │       └─▶ execute_async()            │   │
│  │       │           │           └─▶ AsyncTaskReceipt       │   │
│  │       │           │                                       │   │
│  │       │           ├─▶ poll_with_progress()               │   │
│  │       │           │   └─▶ AgenticEvent::ToolUpdate       │   │
│  │       │           │       └─▶ Progress %                 │   │
│  │       │           │                                       │   │
│  │       │           └─▶ wait_for_completion()              │   │
│  │       │               └─▶ AsyncTaskStatus                │   │
│  │       │                                                   │   │
│  │       └─► No ───▶ execute_tool_sync()                    │   │
│  │                   └─▶ ToolExecutor                       │   │
│  │                       └─▶ execute_with_context()         │   │
│  │                                                           │   │
│  │  ┌─────────────────────────────────────────────────────┐  │   │
│  │  │ Metrics Collection                                  │  │   │
│  │  │                                                     │  │   │
│  │  │  start_task() ──▶ active_tasks                    │  │   │
│  │  │       │                                           │  │   │
│  │  │       └─▶ complete_task() ──▶ completed_tasks    │  │   │
│  │  │               │                                   │  │   │
│  │  │               └─▶ update_aggregated()            │  │   │
│  │  │                   └─▶ AsyncToolExecutionMetrics  │  │   │
│  │  └─────────────────────────────────────────────────────┘  │   │
│  └──────────────────────────────────────────────────────────┘   │
│                                                                  │
└──────────────────────────────────────────────────────────────────┘
```

## Usage Examples

### Creating an AsyncAgenticLoop
```rust
let loop = AsyncAgenticLoop::with_config(
    agent,
    provider,
    tools,
    extension_core,
    AsyncAgenticConfig {
        auto_async: true,
        async_timeout_secs: 300,
        streaming_results: true,
        progress_interval_ms: 1000,
        ..Default::default()
    },
);
```

### Getting Metrics
```rust
// Get current metrics
let metrics = loop.metrics().await;
println!("Async executions: {}", metrics.async_executions);
println!("Sync executions: {}", metrics.sync_executions);

// Get tool-specific metrics
let tool_metrics = loop.get_tool_metrics("my_tool").await;
```

### Enhanced System Prompt
```rust
// Build prompt with async tool section
let prompt = loop.build_system_prompt_with_async();
```

### Using Metrics Collector Directly
```rust
let collector = AsyncToolMetricsCollector::new();

// Start tracking
collector.start_task("task1".to_string(), "my_tool".to_string(), true).await;

// Complete
collector.complete_task(&"task1".to_string(), AsyncTaskStatus::Completed { ... }).await;

// Generate report
let report = collector.generate_report().await;
println!("{}", report);
```

## Test Results

All 3 new tests passing:
- `test_async_config_default` - Config defaults
- `test_metrics_default` - Metrics initialization
- `test_async_tool_prompt_section` - Prompt generation

## Integration with Existing System

### Event Flow
```rust
// Tool execution emits events:
AgenticEvent::ToolStart { ... }
AgenticEvent::ToolUpdate { progress_percent: Some(50), ... }  // Async only
AgenticEvent::ToolEnd { success: true, duration_ms: 5000, ... }
```

### Capability Caching
- Capabilities detected once per tool
- Cached in `RwLock<HashMap>`
- Updated on tool registration

### Metrics Collection
- Optional (controlled by `collect_metrics` flag)
- Non-blocking (async RwLock)
- Configurable retention (default 1000 tasks)

## Benefits

1. **Automatic Optimization**: Automatically uses async for capable tools
2. **Progress Visibility**: Real-time updates for long operations
3. **Performance Insights**: Detailed metrics on execution patterns
4. **Backward Compatible**: Sync tools work without modification
5. **Configurable**: Fine-grained control over async behavior

## Future Enhancements

### Phase 6: Advanced Features
1. Parallel async tool execution
2. Async tool result streaming
3. Intelligent timeout prediction
4. Automatic retry with backoff
5. Tool execution scheduling

## Migration Guide

### For Agent Developers

**Before**
```rust
let loop = AgenticLoopV4::new(agent, provider, tools, extension_core);
```

**After**
```rust
let loop = AsyncAgenticLoop::new(agent, provider, tools, extension_core);

// Or with custom config
let loop = AsyncAgenticLoop::with_config(
    agent, provider, tools, extension_core,
    AsyncAgenticConfig {
        auto_async: true,
        streaming_results: true,
        ..Default::default()
    }
);
```

### For Tool Developers

**No changes required for sync tools** - they automatically use fallback.

**For async-native tools**, implement `UnifiedAsyncTool`:
```rust
#[async_trait]
impl UnifiedAsyncTool for MyTool {
    fn supports_async(&self) -> bool { true }
    
    async fn execute_async(&self, params: Value, config: AsyncToolConfig) 
        -> Result<AsyncTaskReceipt> { ... }
    
    async fn check_status(&self, task_id: &AsyncTaskId) -> Result<AsyncTaskStatus> { ... }
    
    async fn cancel(&self, task_id: &AsyncTaskId) -> Result<bool> { ... }
}
```

## Summary

Phase 5 completes the async extension integration by:
1. ✅ Integrating with AgenticLoopV4
2. ✅ Adding automatic async tool selection
3. ✅ Adding async tool syntax for LLM prompts
4. ✅ Implementing streaming results
5. ✅ Adding comprehensive metrics collection

The system now seamlessly handles both sync and async tools, automatically optimizing execution based on capabilities while providing detailed observability.
