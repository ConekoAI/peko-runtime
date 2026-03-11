# GAP-002: System-Managed Execution (Simplified)

**Priority:** 🔴 Critical  
**Status:** ✅ **SIMPLIFIED** (Architecture Pivot)  
**Target:** v0.5.0  
**Date:** 2026-03-10

---

## Architecture Pivot

**Original Design:** System-managed async execution with TaskHandle, OrphanPool, CleanupPolicy

**New Design:** Synchronous-only tool execution with timeout support

### Rationale for Pivot

1. **Agent Loop is Inherently Blocking**
   - Agent needs tool result to continue reasoning
   - Async execution doesn't help - still must wait for result
   - Adds unnecessary complexity with no benefit

2. **Simpler Alternatives Exist**
   - Shell background: `command &` for fire-and-forget
   - MCP async: submit → poll → retrieve pattern
   - Agent spawn: independent parallel agents

3. **Maintainability**
   - No TaskManager, OrphanPool complexity
   - No CleanupPolicy decisions
   - No async state management

---

## Final Implementation

### What Was Implemented

Simple synchronous execution with timeout:

```rust
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    async fn execute(&self, params: Value) -> Result<Value>;
    // Tools can expose timeout parameter in their schema
}
```

### What Was Removed

- ❌ `ExecutionMode::Async` 
- ❌ `TaskHandle` for async tracking
- ❌ `OrphanPool` for orphaned tasks
- ❌ `CleanupPolicy` 
- ❌ `TaskManager` complexity

### What Was Kept

- ✅ Timeout parameter in `process` tool
- ✅ `ExecutionMode::Sync { timeout }` (simplified to just timeout)
- ✅ Task status tracking for observability (optional)

---

## Usage Patterns

### Synchronous Execution (Default)

```rust
// Tool executes synchronously, agent waits
let result = tool.execute(params).await?;
```

### Long-Running Tasks

**Option 1: Shell Background**
```bash
# Start background task
{"command": "sh", "args": ["-c", "long_task.sh > /tmp/log 2>&1 &"]}

# Check later
{"command": "cat", "args": ["/tmp/log"]}
```

**Option 2: MCP Async**
```json
{"mcp": "compute", "method": "submit_job", "params": {...}}
{"mcp": "compute", "method": "get_status", "params": {"job_id": "..."}}
```

**Option 3: Agent Spawn**
```json
{"tool": "agent_spawn", "params": {"name": "Worker", "task": "..."}}
```

---

## Files Changed

### Removed
- `src/engine/orphan_pool.rs` - Deleted
- `src/engine/task_manager.rs` - Simplified

### Modified
- `src/engine/execution.rs` - Removed async types
- `src/engine/loop_v4.rs` - Removed async execution
- `src/tools/process.rs` - Added timeout parameter
- `GRAND_ARCHITECTURE.md` - Updated documentation

---

## References

- [GRAND_ARCHITECTURE.md - Tool Execution Model](../GRAND_ARCHITECTURE.md#24-tool-execution-model)
- [GRAND_ARCHITECTURE.md - Long-Running Task Patterns](../GRAND_ARCHITECTURE.md#7-long-running-task-patterns)
