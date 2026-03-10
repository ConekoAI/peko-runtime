# GAP-002: System-Managed Async Execution

**Priority:** 🔴 Critical  
**Status:** Open  
**Target:** v0.5.0  
**Est. Effort:** 1 week  

---

## Problem Statement

The Grand Architecture specifies that the **system** manages sync/async execution, not tools themselves. Tools should be simple synchronous functions.

Currently:
- Tools execute synchronously via `await`
- No concept of fire-and-forget
- No `TaskHandle` for async operations
- No orphan pool for tasks that outlive their parent

---

## Current State

```rust
// src/engine/loop_v4.rs:486
let tool_result = if let Some(tool) = self.tools.iter().find(|t| t.name() == name) {
    match tool.execute(arguments.clone()).await {  // Always sync/blocking
        Ok(result) => result.to_string(),
        Err(e) => format!("Error: {}", e),
    }
}
```

No `ExecutionMode`, no `TaskHandle`, no async management.

---

## Target State

Per [GRAND_ARCHITECTURE.md section 2.4](../GRAND_ARCHITECTURE.md#24-system-managed-async-model):

```rust
// System manages sync vs async
let handle = execution_engine.execute(
    "agent_spawn",
    json!({"task": "Research asyncio"}),
    ExecutionMode::Async  // Fire and forget, get handle
).await?;

// Continue conversation...

// Check result later
if let TaskStatus::Completed { result } = handle.status().await {
    // Process result
}
```

---

## Scope

### In Scope
- `ExecutionMode` enum (`Sync`, `Async`)
- `TaskHandle` for async operation tracking
- `TaskStatus` enum for state tracking
- System-level task spawning (not tool-level)
- Orphan pool for tasks that outlive parent
- Cleanup policies (`CancelOnParentExit`, `Orphan`, `Transfer`)

### Out of Scope (Future)
- Distributed task execution
- Task persistence across restarts
- Complex retry/circuit breaker logic

---

## Goals

1. **Execution Engine**: Add sync/async modes to tool execution
2. **Task Handles**: Return handles for async operations
3. **Task Lifecycle**: Track pending, running, completed, failed states
4. **Orphan Management**: Handle tasks when parent agent/session terminates

---

## Proposed Implementation

### Core Types
```rust
// src/engine/execution.rs
pub enum ExecutionMode {
    /// Block until result
    Sync { timeout: Duration },
    /// Return immediately with handle
    Async,
}

pub enum ExecutionResult {
    /// Sync result
    Value(Value),
    /// Async handle
    Handle(TaskHandle),
}

pub struct TaskHandle {
    pub id: String,
    pub status: Arc<RwLock<TaskStatus>>,
}

pub enum TaskStatus {
    Pending,
    Running,
    Completed { result: Value },
    Failed { error: String },
    Cancelled,
}

pub enum CleanupPolicy {
    CancelOnParentExit,
    Orphan,
    TransferToOrphanPool,
}
```

### Execution Engine Extension
```rust
impl AgenticLoopV4 {
    pub async fn execute(
        &self,
        tool: &str,
        args: Value,
        mode: ExecutionMode,
        cleanup_policy: CleanupPolicy,
    ) -> Result<ExecutionResult> {
        match mode {
            ExecutionMode::Sync { timeout } => {
                // Current behavior
                let result = tokio::time::timeout(
                    timeout,
                    self.execute_tool(tool, args)
                ).await??;
                Ok(ExecutionResult::Value(result))
            }
            ExecutionMode::Async => {
                // Spawn and return handle
                let handle = self.spawn_tool_task(tool, args, cleanup_policy).await?;
                Ok(ExecutionResult::Handle(handle))
            }
        }
    }
}
```

### Orphan Pool
```rust
pub struct OrphanPool {
    tasks: HashMap<TaskId, OrphanedTask>,
}

impl OrphanPool {
    pub fn adopt(&mut self, handle: TaskHandle) -> Result<()>;
    pub fn get(&self, id: &str) -> Option<&OrphanedTask>;
    pub fn cleanup_completed(&mut self);
}
```

---

## Dependencies

- **Required by:** GAP-003 (Spawn overlays need async execution)
- **Related to:** GAP-005 (Agent messaging needs async delivery)

---

## Success Criteria

- [ ] Can execute tool in async mode and get `TaskHandle`
- [ ] Can check task status via handle
- [ ] Can wait for async task completion
- [ ] Tasks are cancelled when parent exits (default policy)
- [ ] Tasks can be orphaned to survive parent
- [ ] Orphan pool is queryable via CLI

---

## References

- [GRAND_ARCHITECTURE.md - System Async Model](../GRAND_ARCHITECTURE.md#24-system-managed-async-model)
- [GRAND_ARCHITECTURE.md - Async Flow Examples](../GRAND_ARCHITECTURE.md#8-async-flow-examples)
- Current loop: `src/engine/loop_v4.rs`
