# Async Tool Framework Implementation

## Overview

The Async Tool Framework provides unified infrastructure for asynchronous tool execution. It replaces manual registry/queue management with a centralized executor pattern.

## Architecture

### Core Components

```
┌─────────────────────────────────────────────────────────────┐
│                 UnifiedAsyncExecutor                        │
├─────────────────────────────────────────────────────────────┤
│  ┌─────────────────┐  ┌─────────────────────────────────┐  │
│  │ AsyncTaskRegistry│  │    ResultDelivery (pluggable)   │  │
│  │                 │  ├─────────────────────────────────┤  │
│  │ - Track tasks   │  │  • QueueDelivery (default)      │  │
│  │ - Store status  │  │  • ChannelDelivery              │  │
│  │ - Cache results │  │  • CallbackDelivery             │  │
│  └─────────────────┘  └─────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
                    ┌───────────────────┐
                    │ AsyncTaskResult   │
                    │ (unified enum)    │
                    ├───────────────────┤
                    │ • Process         │
                    │ • Subagent        │
                    │ • Invocation      │
                    │ • Generic         │
                    └───────────────────┘
```

## Key Types

### AsyncTaskResult

Unified result type for all async operations:

```rust
pub enum AsyncTaskResult {
    Process { stdout, stderr, exit_code },
    Subagent { result, task },
    Invocation { result, invocation_id },
    Generic { data, result_type },
}
```

Provides standardized formatting:
- `format_for_announcement()` - Format for parent agent delivery
- `summary()` - Short summary for logging

### ResultDelivery Trait

```rust
#[async_trait]
pub trait ResultDelivery: Send + Sync {
    async fn deliver(&self, entry: &AsyncTaskEntry) -> Result<()>;
    fn clone_box(&self) -> Box<dyn ResultDelivery>;
}
```

**Built-in implementations:**
- `QueueDelivery` - Queue for later delivery (default)
- `ChannelDelivery` - Send via mpsc channel
- `CallbackDelivery` - Invoke async callback

### UnifiedAsyncExecutor

Central executor for all async operations:

```rust
impl UnifiedAsyncExecutor {
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

    pub async fn wait_for_completion(
        &self,
        task_id: &AsyncTaskId,
        timeout: Duration,
    ) -> Result<WaitResult>;

    pub async fn check_status(&self, task_id: &AsyncTaskId) -> Option<AsyncTaskStatus>;
    pub async fn cancel(&self, task_id: &AsyncTaskId) -> Result<bool>;
}
```

## Tool Integration

### ProcessTool Example

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
                    let result = Self::execute_command(...).await?;
                    Ok(AsyncTaskResult::Process { ... })
                },
            )
            .await?;

        Ok(json!({ "task_id": receipt.task_id, "status": "accepted" }))
    }
}
```

## Benefits Over Manual Pattern

### Code Reduction

| Aspect | Manual Pattern | Unified Executor |
|--------|---------------|------------------|
| Registry access | 4 lock operations | 0 (handled internally) |
| Queue management | Manual enqueue | Via delivery trait |
| Status updates | 2+ updates | Automatic |
| Error handling | Per-tool | Standardized |
| Lines of code | ~80 | ~40 |

### Consistency

All async tools now share:
- Same registry and queue management
- Same result formatting
- Same delivery mechanisms
- Same status tracking

### Testability

```rust
// Mock executor for testing
let mock_executor = UnifiedAsyncExecutor::with_registries(
    Arc::new(RwLock::new(AsyncTaskRegistry::new())),
    Arc::new(RwLock::new(AsyncResultQueueManager::new())),
);

let tool = ProcessTool::new().with_async(mock_executor, "test_session");
```

## Migration Status

| Tool | Status | Notes |
|------|--------|-------|
| ProcessTool | ✅ Migrated | Using UnifiedAsyncExecutor |
| AgentSpawnTool | ⏳ Pending | Uses SubagentExecutor (higher-level) |
| agent_invoke | ✅ Separate | Uses InvocationRegistry (A2A messaging) |

## Files

- `src/agent/async_tool_framework.rs` - Core framework
- `src/tools/process.rs` - ProcessTool implementation
- `docs/async-tool-framework.md` - User documentation

## Test Coverage

All 500 tests pass including:
- `test_async_task_result_process` - Result formatting
- `test_process_async_mode` - Async execution
- `test_process_async_with_timeout` - Timeout handling
- `test_async_task_registry` - Registry operations
- `test_queue_delivery` - Delivery mechanisms
