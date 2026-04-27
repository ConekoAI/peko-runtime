# Issue 008: Dual Registry System for Subagents

**Severity:** CRITICAL  
**Status:** 🟡 **Open**  
**Labels:** `architecture`, `subagents`, `registry`, `async-tasks`, `refactor`  
**Reported:** 2026-04-27  

---

## Summary

Two separate registries track the same underlying subagent tasks with different data models:
- `SubagentRegistry` (`src/agent/subagent_registry.rs`) tracks `SubagentRun` structs
- `AsyncTaskRegistry` (`src/agent/async_tool_framework.rs`) tracks `AsyncTaskEntry` structs

`SubagentExecutor` must update both on every state change, creating a risk of desync and requiring duplicate logic for status queries, cancellation, and result retrieval.

---

## The Two Registries

### Registry 1: `SubagentRegistry`

**Location:** `src/agent/subagent_registry.rs`  
**Key Type:** `SubagentRun`  
**Responsibilities:**
- Track subagent runs by ID and agent name
- Store spawn parameters, status, result, and timing
- Provide queries: `get_run()`, `list_runs()`, `find_run_across_all_registries()`

```rust
pub struct SubagentRun {
    pub run_id: String,
    pub agent_name: String,
    pub task: String,
    pub status: SubagentStatus,
    pub result: Option<String>,
    pub created_at: Instant,
    pub completed_at: Option<Instant>,
}

pub enum SubagentStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
    TimedOut,
}
```

---

### Registry 2: `AsyncTaskRegistry`

**Location:** `src/agent/async_tool_framework.rs`  
**Key Type:** `AsyncTaskEntry`  
**Responsibilities:**
- Track async tasks by ID
- Store task metadata, status, result, and delivery target
- Provide queries: `get_task()`, `wait_for_completion()`, `cancel_task()`

```rust
pub struct AsyncTaskEntry {
    pub task_id: String,
    pub tool_name: String,
    pub status: AsyncTaskStatus,
    pub result: Option<AsyncTaskResult>,
    pub created_at: Instant,
    pub completed_at: Option<Instant>,
    pub delivery_target: DeliveryTarget,
}

pub enum AsyncTaskStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
    TimedOut,
}
```

---

## Evidence of Duplication

### Nearly Identical Status Enums

| Variant | `SubagentStatus` | `AsyncTaskStatus` |
|---------|------------------|-------------------|
| Pending | ✅ | ✅ |
| Running | ✅ | ✅ |
| Completed | ✅ | ✅ |
| Failed | ✅ | ✅ |
| Cancelled | ✅ | ✅ |
| TimedOut | ✅ | ✅ |

### `SubagentExecutor` Updates Both

```rust
// src/agent/subagent_executor.rs (conceptual)
async fn spawn_and_execute(...) {
    // 1. Register in SubagentRegistry
    let run_id = subagent_registry.register_run(...).await;
    
    // 2. Register in AsyncTaskRegistry
    let task_id = async_task_registry.register_task(...).await;
    
    // 3. Execute task
    let result = execute_task(...).await;
    
    // 4. Update BOTH registries
    subagent_registry.complete_run(run_id, result.clone()).await;
    async_task_registry.complete_task(task_id, result).await;
}
```

### Global Cache for `SubagentRegistry`

```rust
// src/agent/subagent_registry.rs
static GLOBAL_SUBAGENT_REGISTRIES: OnceLock<Mutex<HashMap<String, SharedSubagentRegistry>>> = ...;
```

This exists because stateless execution creates new `Agent` instances per request, so the registry must outlive any single `Agent`. The `AsyncTaskRegistry` does not have this global cache — it is owned by `AsyncExecutor`, which is owned by `SubagentExecutor`.

---

## Impact

1. **State desync risk:** If one registry update fails (e.g., due to a panic or lock timeout), the two registries diverge. A client querying `SubagentRegistry` may see "Completed" while `AsyncTaskRegistry` still shows "Running."
2. **Double memory usage:** Every subagent run is stored twice with overlapping fields.
3. **Query inconsistency:** `get_run_status()` queries `SubagentRegistry`; async delivery queries `AsyncTaskRegistry`. There is no guarantee they return the same status.
4. **Cancellation complexity:** Cancelling a subagent requires updating both registries, and the cancellation logic may race.

---

## Root Cause

- `SubagentRegistry` was built for the subagent feature (spawned agents with session isolation).
- `AsyncTaskRegistry` was built for the general async tool framework (background shell commands, etc.).
- Subagents were later implemented as a special case of async tools, but the registries were never merged.
- The global cache for `SubagentRegistry` exists because `SubagentExecutor` is recreated per request in the stateless architecture, so the registry must be global. `AsyncTaskRegistry` is owned by the executor and is lost on executor drop.

---

## Proposed Resolution

**Option A: Merge into `AsyncTaskRegistry` (Recommended)**

1. **Add subagent-specific fields to `AsyncTaskEntry`** (agent_name, session_id, parent_session_key).
2. **Make `AsyncTaskRegistry` global** (like `SubagentRegistry`) so it survives executor drops.
3. **Delete `SubagentRegistry`** and migrate all queries to `AsyncTaskRegistry`.
4. **Unify `SubagentStatus` and `AsyncTaskStatus`** into a single enum.

**Option B: Make `SubagentRegistry` the canonical registry**

1. **Add async tool fields to `SubagentRun`** (tool_name, delivery_target).
2. **Extend `SubagentRegistry` to handle non-subagent async tasks** (shell commands, etc.).
3. **Delete `AsyncTaskRegistry`**.

This is less ideal because `AsyncTaskRegistry` has richer delivery infrastructure (queue, channel, callback).

**Option C: Keep both but add a sync layer**

Create a `UnifiedTaskRegistry` facade that writes to both and reads from one. This adds complexity without solving the memory duplication.

---

## Acceptance Criteria

- [ ] There is exactly **one** registry for async tasks and subagents.
- [ ] `SubagentStatus` and `AsyncTaskStatus` are unified (or one is deleted).
- [ ] Global caching works for the unified registry (survives stateless executor drops).
- [ ] All queries (`get_run`, `list_runs`, `get_task`, `wait_for_completion`) work on the unified registry.
- [ ] Cancellation updates a single registry entry.
- [ ] All existing tests pass.

---

## Related

- Issue 006: Three Async Tool Frameworks
- Issue 007: Dual Tool Registration Paths
- `src/agent/subagent_registry.rs`
- `src/agent/subagent_executor.rs`
- `src/agent/async_tool_framework.rs`
- `src/agent/async_tool_framework.rs` (`AsyncTaskRegistry`, `AsyncResultQueueManager`)
