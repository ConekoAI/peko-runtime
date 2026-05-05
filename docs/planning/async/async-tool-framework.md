# Async Tool Framework

## Overview

The Async Tool Framework provides unified infrastructure for tools that execute asynchronously. It enables any tool to:

1. Return a receipt immediately (non-blocking)
2. Execute work in the background
3. Deliver results via pluggable mechanisms (queue, channel, callback)

This design generalizes the subagent spawn pattern for any async tool operation.

## Core Components

### AsyncTaskResult

Unified result type for all async operations:

```rust
pub enum AsyncTaskResult {
    /// Shell command result (from process tool)
    Process { stdout: String, stderr: String, exit_code: i32 },
    /// Subagent execution result
    Subagent { result: String, task: String },
    /// Tool invocation result (from agent_invoke)
    Invocation { result: Value, invocation_id: String },
    /// Generic result for other tools
    Generic { data: Value, result_type: String },
}

impl AsyncTaskResult {
    /// Format for announcement to parent agent
    pub fn format_for_announcement(&self, tool_name: &str, label: &str) -> String;
    /// Get a short summary for logging
    pub fn summary(&self) -> String;
}
```

### ResultDelivery Trait

Pluggable delivery mechanism:

```rust
#[async_trait]
pub trait ResultDelivery: Send + Sync {
    async fn deliver(&self, entry: &AsyncTaskEntry) -> Result<()>;
    fn clone_box(&self) -> Box<dyn ResultDelivery>;
}
```

**Implementations:**
- `QueueDelivery` - Queue result for later delivery (default)
- `ChannelDelivery` - Send via async channel
- `CallbackDelivery` - Invoke custom callback function

### UnifiedAsyncExecutor

Central executor for all async operations:

```rust
pub struct UnifiedAsyncExecutor {
    registry: SharedAsyncTaskRegistry,
    queue_manager: SharedAsyncResultQueueManager,
    deliveries: HashMap<DeliveryTarget, Box<dyn ResultDelivery>>,
    default_delivery: DeliveryTarget,
}

impl UnifiedAsyncExecutor {
    /// Execute an async task with the given closure
    pub async fn execute<F, Fut>(
        &self,
        task_id: AsyncTaskId,
        tool_name: &str,
        params: Value,
        parent_session_key: &str,
        config: AsyncToolConfig,
        execution_fn: F,
    ) -> Result<AsyncTaskReceipt>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<AsyncTaskResult>> + Send;

    /// Wait for task completion with timeout
    pub async fn wait_for_completion(
        &self,
        task_id: &AsyncTaskId,
        timeout: Duration,
    ) -> Result<WaitResult>;

    /// Check current task status
    pub async fn check_status(&self, task_id: &AsyncTaskId) -> Option<AsyncTaskStatus>;

    /// Cancel a running task
    pub async fn cancel(&self, task_id: &AsyncTaskId) -> Result<bool>;
}
```

## Integration Example: Process Tool

```rust
pub struct ProcessTool {
    executor: Option<UnifiedAsyncExecutor>,
    session_key: Option<String>,
}

impl ProcessTool {
    pub fn with_async(
        mut self,
        executor: UnifiedAsyncExecutor,
        session_key: impl Into<String>,
    ) -> Self {
        self.executor = Some(executor);
        self.session_key = Some(session_key.into());
        self
    }

    async fn execute_async(&self, ...) -> Result<Value> {
        let executor = self.executor.clone()
            .ok_or_else(|| anyhow!("Async mode not configured"))?;

        let receipt = executor
            .execute(
                task_id,
                "process",
                params,
                session_key,
                AsyncToolConfig { ... },
                move || async move {
                    // Execute the actual work
                    let result = Self::execute_command(...).await?;
                    Ok(AsyncTaskResult::Process { ... })
                },
            )
            .await?;

        Ok(json!({
            "task_id": receipt.task_id,
            "status": "accepted",
            ...
        }))
    }
}
```

## Delivery Modes

| Mode | Behavior |
|------|----------|
| `QueueWhenBusy` | Queue result and deliver when agent is idle (default) |
| `Interrupt` | Interrupt current agent execution with result |
| `Collect` | Batch multiple results together |
| `Steer` | Try to inject into running session (advanced) |

## Result Delivery Flow

### Queue Delivery (Default)

```
Tool executes async
        ↓
Result stored in AsyncTaskRegistry
        ↓
Delivery mechanism invoked (QueueDelivery)
        ↓
Added to session's AsyncResultQueue
        ↓
Agent loop checks queue when idle
        ↓
Results delivered as system messages
```

### Channel Delivery

```rust
let (tx, mut rx) = mpsc::channel(100);
let delivery = ChannelDelivery::new(tx);
executor.register_delivery(DeliveryTarget::Channel, delivery);

// Receive results in real-time
while let Some(event) = rx.recv().await {
    println!("Task completed: {}", event.task_id);
}
```

## Architecture Benefits

1. **Unified async pattern**: Any tool can be async using the same infrastructure
2. **Pluggable delivery**: Choose delivery mechanism per task or globally
3. **Simplified tool code**: ~50% reduction in async boilerplate
4. **Consistent error handling**: Standardized result types and status tracking
5. **Race condition safety**: No corruption from concurrent session writes

## Migration Guide

### From Manual Registry/Queue Management

**Before:**
```rust
// Manual registry/queue management
let registry = self.async_registry.clone().unwrap();
let queue_manager = self.queue_manager.clone().unwrap();

// Register task
let mut reg = registry.write().await;
reg.register(entry);
drop(reg);

// Spawn background task with manual status updates
tokio::spawn(async move {
    // Update status
    let mut reg = registry.write().await;
    reg.update_status(&task_id, AsyncTaskStatus::Running);
    
    // Do work...
    
    // Update status again
    reg.update_status(&task_id, AsyncTaskStatus::Completed { ... });
    
    // Queue result
    let mut manager = queue_manager.write().await;
    manager.enqueue(event);
});
```

**After:**
```rust
// Use unified executor
let executor = self.executor.clone().unwrap();

let receipt = executor
    .execute(
        task_id,
        tool_name,
        params,
        session_key,
        config,
        move || async move {
            // Do work...
            Ok(AsyncTaskResult::Process { ... })
        },
    )
    .await?;
```

## Comparison to Original Design

| Aspect | Original | Current (Unified) |
|--------|----------|-------------------|
| Registry access | Manual lock/unlock | Handled by executor |
| Queue management | Manual enqueue | Via ResultDelivery trait |
| Status updates | Manual | Automatic |
| Result formatting | Per-tool | Standardized via AsyncTaskResult |
| Tool code size | ~80 lines | ~40 lines |
| Testability | Requires registries | Can mock executor |

## Future Extensions

### Other Async Tools

The framework enables async execution for:
- **Cron jobs** - Schedule future execution
- **Long processes** - Spawn process, notify on completion  
- **External webhooks** - Register callback, deliver when received
- **Batch operations** - Multiple operations, results collected

### Custom Delivery Mechanisms

Implement `ResultDelivery` for custom behaviors:

```rust
#[async_trait]
impl ResultDelivery for WebhookDelivery {
    async fn deliver(&self, entry: &AsyncTaskEntry) -> Result<()> {
        // POST result to webhook URL
        let client = reqwest::Client::new();
        client.post(&self.url)
            .json(&entry.result)
            .send()
            .await?;
        Ok(())
    }
    
    fn clone_box(&self) -> Box<dyn ResultDelivery> {
        Box::new(self.clone())
    }
}
```
