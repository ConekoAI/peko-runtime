# Tool Monitoring, Abortion, and Progress Updates

This document describes the implementation of tool monitoring, abortion, and progress updates for Pekobot, bringing it to parity with OpenClaw's capabilities.

## Overview

Pekobot now supports full tool lifecycle management similar to OpenClaw:

1. **Tool Abortion** - Cancel long-running tools via `AbortSignal`
2. **Progress Updates** - Real-time progress reporting via `ToolUpdate` events (with throttling)
3. **Timeout Handling** - Per-tool execution timeouts
4. **Tool Monitoring** - Full visibility into tool execution (start/update/end)

## Architecture

### Core Components

#### 1. `ToolContext` (`src/tools/core/context.rs`)

The execution context passed to tools, providing:

- **Abort Signal**: `is_aborted()` method for checking cancellation
- **Progress Reporting**: `report_progress()` and `report_status()` methods with throttling
- **Timeout Handling**: `check_timeout()` and `timeout()` methods
- **Event Channel**: Automatic emission of `ToolUpdate` events

```rust
pub struct ToolContext {
    pub run_id: String,
    pub tool_id: String,
    pub tool_name: String,
    event_tx: Option<mpsc::Sender<AgenticEvent>>,
    abort_rx: watch::Receiver<bool>,
    pub progress_throttle_ms: u64,  // Configurable throttling
    last_progress_update: Arc<Mutex<Option<Instant>>>,
    pub timeout: Option<Duration>,  // Optional timeout
}
```

#### 2. `AbortSignal` (`src/tools/core/context.rs`)

Similar to OpenClaw's `wrapToolWithAbortSignal`, provides:

- Signal sender/receiver pair
- `abort()` method to trigger cancellation
- `create_context()` to build ToolContext

```rust
let signal = AbortSignal::new();
let ctx = signal.create_context("run-1", "tool-1", "my-tool");

// Later, from another task:
signal.abort();
```

#### 3. `ToolError` (`src/tools/core/context.rs`)

Strongly typed errors for tool execution:

```rust
pub enum ToolError {
    Aborted,
    Timeout(Duration),
    Other(String),
}
```

Used for clean error handling instead of string matching:

```rust
match result {
    Err(e) => match e.downcast_ref::<ToolError>() {
        Some(ToolError::Aborted) => "Tool was aborted",
        Some(ToolError::Timeout(d)) => format!("Timed out after {:?}", d),
        _ => format!("Error: {}", e),
    }
}
```

#### 4. Enhanced `Tool` Trait (`src/tools/core/traits.rs`)

Extended with context-aware execution:

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    
    // Basic execution (backward compatible)
    async fn execute(&self, params: Value) -> Result<Value>;
    
    // Context-aware execution with abort + progress + timeout
    async fn execute_with_context(
        &self,
        params: Value,
        ctx: &ToolContext,
    ) -> Result<Value> {
        // Default implementation delegates to execute()
        // but adds abort checks, timeout checks, and status events
    }
    
    // Does this tool support progress updates?
    fn supports_progress(&self) -> bool { false }
    
    // Estimated execution duration
    fn estimated_duration_ms(&self, params: &Value) -> u64 { 1000 }
}
```

### Event Flow

```
Tool Execution Flow:

1. AgenticLoop detects tool call
2. Emit ToolStart { run_id, tool_id, name, params }
3. Create AbortSignal + ToolContext
4. Execute tool with context
   └─> Tool periodically checks ctx.is_aborted()
   └─> Tool calls ctx.check_timeout(start_time)
   └─> Tool calls ctx.report_progress(current, total, message)
   └─> ToolContext emits ToolUpdate events (throttled)
5. Tool completes, times out, or aborts
6. Emit ToolEnd { run_id, tool_id, result, success, duration_ms }
```

## Usage

### For Tool Authors

Create tools that support progress, abort, and timeout:

```rust
use crate::tools::context::{ToolContext, ToolError};
use crate::tools::Tool;
use std::time::Instant;

#[async_trait]
impl Tool for MyLongRunningTool {
    fn name(&self) -> &str { "my_tool" }
    fn description(&self) -> &str { "Does long work" }
    
    async fn execute(&self, params: Value) -> Result<Value> {
        // Basic implementation
    }
    
    // Override for progress support
    async fn execute_with_context(
        &self,
        params: Value,
        ctx: &ToolContext,
    ) -> Result<Value> {
        let items = parse_items(&params);
        let total = items.len();
        let start_time = Instant::now();
        
        for (i, item) in items.iter().enumerate() {
            // Check abort frequently
            if ctx.is_aborted() {
                return Err(ToolError::Aborted.into());
            }
            
            // Check timeout frequently
            ctx.check_timeout(start_time)?;
            
            // Do work
            process(item).await;
            
            // Report progress (automatically throttled)
            ctx.report_progress(
                i + 1,
                total,
                Some(format!("Processed item {}", item.name))
            ).await;
        }
        
        Ok(json!({ "processed": total }))
    }
    
    fn supports_progress(&self) -> bool { true }
}
```

### Throttling Configuration

Control progress update frequency:

```rust
let ctx = signal
    .create_context_with_events("run-1", "tool-1", "my-tool", event_tx)
    .with_throttle(1000); // Max 1 update per second

// Or disable throttling
let ctx = signal
    .create_context_with_events("run-1", "tool-1", "my-tool", event_tx)
    .with_throttle(0); // No throttling
```

### Timeout Configuration

Set per-tool timeouts:

```rust
use std::time::Duration;

let ctx = signal
    .create_context_with_events("run-1", "tool-1", "my-tool", event_tx)
    .with_timeout(Duration::from_secs(30));

// In the tool, check timeout:
let start = Instant::now();
ctx.check_timeout(start)?; // Returns Err(ToolError::Timeout) if exceeded
```

### For Channel/CLI Developers

Subscribe to tool events for display:

```rust
let (event_tx, mut event_rx) = mpsc::channel(100);

// Run agent with streaming
let result = agent.execute_streaming(prompt).await?;

// Process events
while let Some(event) = event_rx.recv().await {
    match event {
        AgenticEvent::ToolStart { name, .. } => {
            println!("Starting tool: {}", name);
        }
        AgenticEvent::ToolUpdate { 
            output, 
            progress_percent, 
            .. 
        } => {
            if let Some(percent) = progress_percent {
                println!("Progress: {}% - {}", percent, output);
            } else {
                println!("Status: {}", output);
            }
        }
        AgenticEvent::ToolEnd { 
            success, 
            duration_ms, 
            .. 
        } => {
            println!("Tool {} in {}ms", 
                if success { "succeeded" } else { "failed" },
                duration_ms
            );
        }
        _ => {}
    }
}
```

### Aborting Tool Execution

```rust
// In streaming mode, the loop can be aborted:
let abort_signal = AbortSignal::new();

// Run with abort support
let loop_ = AgenticLoop::new(agent, provider, tools)
    .with_abort_signal(abort_signal.clone());

// Later, abort from another task:
tokio::spawn(async move {
    tokio::time::sleep(Duration::from_secs(5)).await;
    abort_signal.abort();
});
```

## Example: ProgressDemoTool

See `src/tools/framework/shared/proxy_utils.rs` for examples demonstrating:
- Progress reporting with batch processing
- Abort signal handling
- Timeout checking
- Throttled updates

Run the example:

```bash
cargo run --example tool_monitoring
```

This shows:
1. Normal execution with progress bars
2. Mid-execution abortion
3. Timeout handling
4. Throttled progress updates

## Testing

```bash
# Run the integration tests
cargo test tool_monitoring -- --nocapture

# Run the example
cargo run --example tool_monitoring
```

Tests cover:
- Abort signal propagation
- Progress event emission with throttling
- Timeout handling
- Multiple concurrent tools with independent abort signals
- Error type checking

## Comparison with OpenClaw

| Feature | OpenClaw | Pekobot (New) |
|---------|----------|---------------|
| Abort Signal | `wrapToolWithAbortSignal()` | `AbortSignal` + `ToolContext` |
| Progress Events | `tool_execution_update` | `ToolUpdate` event |
| Tool Events | `tool_execution_start/end` | `ToolStart` / `ToolEnd` |
| Context | Passed to tools | `ToolContext` parameter |
| Default Behavior | Falls back to blocking | `execute_with_context` defaults to `execute` |
| **Progress Throttling** | N/A | ✅ Configurable ms delay |
| **Timeout Support** | N/A | ✅ Per-tool timeout |
| **Strong Error Types** | N/A | ✅ `ToolError` enum |

## Migration Guide

### Existing Tools (No Changes Required)

Tools using only `execute()` continue to work unchanged. The agent loop will:
- Wrap them with `ToolAdapter`
- Check abort before/after execution
- Emit basic start/end events
- Support timeout checking

### Upgrading Tools for Progress

1. Implement `execute_with_context()`:
```rust
async fn execute_with_context(
    &self,
    params: Value,
    ctx: &ToolContext,
) -> Result<Value> {
    // Your implementation
}
```

2. Set `supports_progress() -> bool`:
```rust
fn supports_progress(&self) -> bool { true }
```

3. Add abort and timeout checks:
```rust
let start = Instant::now();
for item in items {
    if ctx.is_aborted() {
        return Err(ToolError::Aborted.into());
    }
    ctx.check_timeout(start)?;
    // Process item...
}
```

4. Report progress (automatically throttled):
```rust
ctx.report_progress(current, total, Some("Processing...")).await;
```

5. Configure throttling/timeout when creating context:
```rust
let ctx = signal
    .create_context_with_events(run_id, tool_id, tool_name, event_tx)
    .with_throttle(500)  // 500ms between updates
    .with_timeout(Duration::from_secs(30));  // 30 second timeout
```

## Future Enhancements

- [x] ✅ Progress throttling
- [x] ✅ Timeout support
- [x] ✅ Strong error types
- [ ] Parallel tool execution with individual abort
- [ ] Tool retry logic with backoff
- [ ] Tool execution history/persistence
- [ ] Tool result size limits
