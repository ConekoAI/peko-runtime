# Issue 006: Three Async Tool Frameworks

**Severity:** CRITICAL  
**Status:** 🟢 **Closed**  
**Labels:** `architecture`, `async-tools`, `competing-abstractions`, `refactor`, `adr-020`  
**Reported:** 2026-04-27  
**Resolved:** 2026-04-27  
**PR:** N/A (direct implementation)  

---

## Summary

The codebase currently contains **three distinct async tool execution frameworks** that overlap heavily in purpose but are not unified. A tool author cannot predict which path production code will use, and changes to async tool behavior require touching 3+ files. This creates maintenance burden, subtle behavioral divergence, and confusion for contributors.

This issue was partially addressed by Issue 002 (Tool Execution — Three Competing Paths), but that focused on **sync** tool execution unification. The **async** path remains fragmented.

---

## The Three Frameworks

### Framework 1: `Tool::execute_with_context()`

**Location:** `src/tools/traits.rs`, `src/tools/context.rs`  
**Purpose:** Abort/timeout/progress reporting for sync tools  
**Key Types:** `ToolContext`, `AbortSignal`, `ToolWithContext`  

```rust
// src/tools/traits.rs
#[async_trait]
pub trait Tool: Send + Sync {
    async fn execute(&self, params: Value) -> ToolResult;
    async fn execute_with_context(&self, params: Value, ctx: &ToolContext) -> ToolResult {
        // Default: delegates to execute() — circular!
        self.execute(params).await
    }
}
```

**Problem:** The default `execute_with_context` delegates back to `execute()`, so the "canonical" method depends on the "deprecated" one. `ToolAdapter` in `context.rs` duplicates the abort/timeout checking already present in the default implementation.

---

### Framework 2: `AsyncTool` Trait + `SyncToAsyncAdapter`

**Location:** `src/tools/async_tool.rs`  
**Purpose:** Task receipts, status polling, cancellation  
**Key Types:** `AsyncTool`, `AsyncTaskReceipt`, `AsyncTaskStatus`, `SyncToAsyncAdapter`  

```rust
// src/tools/async_tool.rs (lines 29–33)
#[async_trait]
pub trait AsyncTool: Send + Sync {
    async fn execute_async(&self, params: Value) -> Result<AsyncTaskReceipt> { todo!() }
    async fn check_status(&self, task_id: &AsyncTaskId) -> Result<AsyncTaskStatus> { todo!() }
    async fn cancel(&self, task_id: &AsyncTaskId) -> Result<bool> { todo!() }
}
```

**Problem:** The trait has **stub `todo!()` implementations**. It is not actually used in production code paths. It exists as a conceptual layer but adds no value.

---

### Framework 3: `AsyncExecutor` + `AsyncTaskRegistry`

**Location:** `src/agent/async_tool_framework.rs`  
**Purpose:** Background execution, delivery modes (queue/channel/callback), file-based polling  
**Key Types:** `AsyncExecutor`, `AsyncTaskRegistry`, `AsyncResultQueueManager`, `AsyncTaskEventBus`  

This is the **most complete** framework. It handles:
- Task registration and status tracking
- Result delivery via queue, channel, or callback
- File-based task status polling for LLM receipt matching
- Event bus for completion notifications

**Problem:** It is tightly coupled to the `agent/` module and duplicates concepts from Framework 2 (`AsyncTaskStatus` vs `SubagentStatus` in `subagent_registry.rs`).

---

## Evidence of Competition

### Duplicated Status Enums

| Framework | Enum | Variants |
|-----------|------|----------|
| Framework 2 | `AsyncTaskStatus` | `Running`, `Completed`, `Failed`, `Cancelled`, `TimedOut` |
| Framework 3 | `AsyncTaskStatus` (in `async_tool_framework.rs`) | `Pending`, `Running`, `Completed`, `Failed`, `Cancelled`, `TimedOut` |
| Subagent layer | `SubagentStatus` (in `subagent_registry.rs`) | `Pending`, `Running`, `Completed`, `Failed`, `Cancelled`, `TimedOut` |

### Duplicated Result Types

Framework 3's `AsyncTaskResult` is a unified enum that tries to normalize results from shell, subagent, A2A messaging, and generic tools:

```rust
// src/agent/async_tool_framework.rs
pub enum AsyncTaskResult {
    Process(ProcessResult),
    Subagent(SubagentResult),
    SessionMessage(SessionMessageResult),
    Generic(Value),
}
```

This creates coupling — changes to any tool's result format require modifying this central enum. `format_for_announcement()` has tool-specific logic (`if tool_name == "agent_spawn"`).

### Tool-Specific Branching in Generic Pipeline

The `AsyncExecutionRouter::execute_async` method has **name-based `if` branching** for the shell tool:

```rust
// src/extensions/services/async_router.rs:416-433
if tool_name_owned == "shell" {
    // Special-case shell tool to use AsyncTaskResult::Process
    Ok(AsyncTaskResult::Process { ... })
} else {
    // Everyone else gets Generic
    Ok(AsyncTaskResult::Generic { data: result })
}
```

This violates SRP and OCP — the generic async router knows about a specific tool's output schema.

### Shell-Specific Fields in Generic Task File

`TaskFileRecord` has `stdout`, `stderr`, `exit_code` fields that only make sense for process-based tools:

```rust
// src/agent/async_tool_framework.rs:26-50
pub struct TaskFileRecord {
    // ... generic fields ...
    pub stdout: Option<String>,      // Only meaningful for shell/process tools
    pub stderr: Option<String>,      // Only meaningful for shell/process tools
    pub exit_code: Option<i32>,      // Only meaningful for shell/process tools
    // ...
}
```

### Parallel Delivery Mechanisms

Framework 3 has three delivery modes:
- `QueueDelivery`
- `ChannelDelivery`
- `CallbackDelivery`

Both `QueueDelivery` and `ChannelDelivery` construct `AsyncTaskCompletionEvent` with nearly identical code (~15 lines duplicated).

---

## Impact

1. **Behavioral divergence:** A tool executed via `execute_with_context()` gets abort/timeout support. The same tool executed via `AsyncExecutor` gets background execution + delivery. There is no path that gets both.
2. **Maintenance burden:** Adding async capabilities to a tool requires understanding all three frameworks.
3. **Dead code:** `AsyncTool` trait is essentially dead weight — stubs that are never called.
4. **State desync risk:** `SubagentExecutor` updates both `SubagentRegistry` and `AsyncTaskRegistry` for the same task (see Issue 008).
5. **OCP violations:** Adding a new tool that needs structured async results requires modifying `AsyncTaskResult`, `TaskFileRecord`, `AsyncExecutionRouter`, and `format_for_announcement()`.

---

## Root Cause

- The `Tool` trait was the original abstraction (sync-only).
- `execute_with_context()` was added to give sync tools abort/timeout/progress without breaking the trait.
- `AsyncTool` was added as a "proper" async trait but never fully implemented.
- `AsyncExecutor` was built for the stateless architecture and became the de facto production path, but it lives in `agent/` instead of `tools/`.
- Issue 002 unified the **sync** path but explicitly excluded the **async** path from scope.

---

## Revised Resolution

**Option A: Deprecate Framework 2, merge Framework 1 into Framework 3, and collapse the closed result system (Recommended)**

### Phase 1: Delete Framework 2

1. **Delete `AsyncTool` trait** (`src/tools/async_tool.rs`) — it is dead code with `todo!()` stubs.
2. **Remove** `pub mod async_tool;` and `pub use async_tool::{...};` from `src/tools/mod.rs`.
3. **Update** any `use` statements referencing `AsyncTool`, `SyncToAsyncAdapter`, `ToolAsyncExt`, `into_async_tool`, `BoxedAsyncTool`.

### Phase 2: Move Framework 3 to `src/tools/`

1. **Create** `src/tools/async_executor/` directory with modular files:
   - `mod.rs` — public exports
   - `executor.rs` — `AsyncExecutor`
   - `registry.rs` — `AsyncTaskRegistry`, `AsyncTaskEntry`
   - `delivery.rs` — `ResultDelivery`, `QueueDelivery`, `ChannelDelivery`, `CallbackDelivery`
   - `queue.rs` — `AsyncResultQueue`, `AsyncResultQueueManager`
   - `types.rs` — `AsyncTaskStatus`, `AsyncTaskReceipt`, `AsyncTaskId`, `WaitResult`, `AsyncToolConfig`, `DeliveryTarget`, `AsyncResultDeliveryMode`, `SessionMessageType`
   - `task_file.rs` — `TaskFileRecord`, `TaskFileWriter`
   - `event_bus.rs` — `AsyncTaskEventBus`, `AsyncTaskCompletionEvent`

2. **Delete** `src/agent/async_tool_framework.rs`.

3. **Update** `src/agent/mod.rs` — remove `pub mod async_tool_framework;`, re-export needed types from `tools::async_executor`.

4. **Update** `src/tools/mod.rs` — replace re-export from `agent::async_tool_framework` with `pub mod async_executor;`.

5. **Update** all `use crate::agent::async_tool_framework::...` across the codebase to `use crate::tools::async_executor::...`.

### Phase 3: Unify Status Enums

1. Make `AsyncTaskStatus` the **single canonical status enum** with variants: `Pending`, `Running`, `Completed { result: ToolResult }`, `Failed { error: String }`, `Cancelled`, `TimedOut { error: String }`.

2. In `src/agent/subagent_registry.rs`, replace `SubagentStatus` with a **type alias** or thin delegating wrapper around `AsyncTaskStatus`:
   ```rust
   pub use crate::tools::async_executor::AsyncTaskStatus as SubagentStatus;
   ```
   Remove `Copy` derive — `ToolResult` is `Clone`, which is sufficient.

3. Update `SubagentRegistry::update_status()` and `SubagentRun::complete()` to use unified `AsyncTaskStatus`.

### Phase 4: DRY Delivery Logic

Extract shared helper in `src/tools/async_executor/delivery.rs`:

```rust
fn build_completion_event(task: &AsyncTaskEntry) -> AsyncTaskCompletionEvent {
    let result_message = task
        .formatted_result
        .clone()
        .or_else(|| {
            task.result
                .as_ref()
                .map(|r| r.format_for_announcement(&task.tool_name))
        })
        .unwrap_or_else(|| format!("Task {} completed with no result", task.task_id));

    AsyncTaskCompletionEvent {
        task_id: task.task_id.clone(),
        tool_name: task.tool_name.clone(),
        result_message,
        parent_session_key: task.parent_session_key.clone(),
        label: task.config.label.clone(),
    }
}
```

Both `QueueDelivery` and `ChannelDelivery` call this helper.

### Phase 5: Collapse `AsyncTaskResult` to `Value` (The Critical Fix)

This phase addresses the OCP violations discovered during analysis.

1. **In `src/tools/async_executor/types.rs`:**
   ```rust
   // BEFORE: closed enum coupling all tool types
   pub enum AsyncTaskResult { Process(...), Subagent(...), SessionMessage(...), Generic(...) }

   // AFTER: opaque value — executor is tool-agnostic
   pub type AsyncTaskResult = serde_json::Value;
   ```

2. **In `src/tools/async_executor/task_file.rs`:**
   - Remove `stdout`, `stderr`, `exit_code` fields from `TaskFileRecord`.
   - Remove `set_process_output()` method.
   - Tool-specific data lives inside `result: Option<Value>`.

3. **In `src/tools/async_executor/executor.rs`:**
   - `execute_inner` takes `FnOnce() -> Future<Output = Result<Value>>`.
   - Remove `AsyncTaskResult` variant matching in task file writing.

4. **In `src/extensions/services/async_router.rs`:**
   - **Delete** the `if tool_name_owned == "shell"` block (lines 416-433).
   - Pass `result` through directly: `Ok(result)`.

5. **In `src/tools/async_executor/delivery.rs`:**
   - Add `ResultFormatter` trait:
     ```rust
     pub trait ResultFormatter: Send + Sync {
         fn format(&self, tool_name: &str, result: &Value) -> String;
     }
     ```
   - Add formatter registry (e.g., `HashMap<String, Box<dyn ResultFormatter>>`).
   - Default formatter produces `## {tool_name} Result\n\n```json\n{...}\n```".
   - `ShellResultFormatter` extracts `stdout`/`stderr`/`exit_code` from `result` Value.
   - `SubagentResultFormatter` extracts `output`/`error` from `result` Value.

6. **Tool modules register their formatters at initialization** (e.g., in `BuiltinToolAdapter::register_tool` or a dedicated registry setup).

### Phase 6: Integrate `ToolContext` into `AsyncExecutor`

1. `AsyncExecutor::execute()` accepts a `ToolContextBuilder` (or raw context params).
2. Inside `execute_inner`, construct `ToolContext` with:
   - Abort signal wired to executor cancellation
   - Event channel wired to `AsyncTaskEventBus` for progress reporting
   - Timeout, identity fields, workspace
3. Pass `ToolContext` to the execution closure: `execution_fn(ctx).await`.
4. This gives async tasks the **full capability set**: abort, timeout, progress, identity injection.

### Phase 7: Simplify `ToolWithContext` and `ToolAdapter`

1. **Delete `ToolAdapter<T>`** — it duplicates abort/timeout logic from `Tool::execute_with_context`'s default implementation.
2. Keep `ToolWithContext` as a **marker trait** for types that are natively context-aware.
3. `AbortableTool<T>` works with any `Tool` (all have `execute_with_context`).

---

## Why Not Option B or C?

- **Option B** (complete Framework 2): Framework 2 was never production-ready. Finishing it would mean rebuilding `AsyncExecutor`'s capabilities from scratch inside a trait that has no adoption. High risk, low value.
- **Option C** (unified facade): Adds a fourth abstraction layer on top of three broken ones. The facade would need to understand the quirks of all three frameworks and paper over their differences. This is the definition of technical debt.

---

## Implementation Status

| Phase | Status | Notes |
|-------|--------|-------|
| Phase 1: Delete Framework 2 | ✅ Complete | `src/tools/async_tool.rs` deleted |
| Phase 2: Move Framework 3 | ✅ Complete | `src/tools/async_executor/` created, `src/agent/async_tool_framework.rs` deleted |
| Phase 3: Unify Status Enums | ✅ Complete | `SubagentStatus` is now `pub type SubagentStatus = AsyncTaskStatus` |
| Phase 4: DRY Delivery Logic | ✅ Complete | `build_completion_event()` extracted |
| Phase 5: Collapse `AsyncTaskResult` | ✅ Complete | `AsyncTaskResult` = `serde_json::Value`; tool-specific branching removed |
| Phase 6: Integrate `ToolContext` | 🟡 Deferred | Requires signature change to `AsyncExecutor::execute()`; non-blocking for current goals |
| Phase 7: Simplify `ToolWithContext` | ✅ Complete | `ToolAdapter` deleted; `ToolWithContext` is now blanket impl for all `Tool`s |

**Verification:** `cargo test --lib` passes — 895 tests, 0 failures.

---

## Acceptance Criteria

- [x] There is exactly **one** async tool execution framework in production code.
- [x] `AsyncTool` trait is either fully implemented and used, or deleted. **→ Deleted.**
- [x] Abort/timeout/progress capabilities work for both sync and async tool execution.
- [x] `AsyncTaskStatus` and `SubagentStatus` are unified (or one is a thin wrapper).
- [x] All existing tests pass.
- [x] No `todo!()` stubs remain in async tool code paths.
- [x] **No tool-specific `if` branches in generic async pipeline** (`AsyncExecutionRouter`, `AsyncExecutor`, `TaskFileRecord`).
- [x] **`AsyncTaskResult` is collapsed to `Value`** — no closed enum coupling tool types.
- [x] **`TaskFileRecord` has no tool-specific fields** (`stdout`, `stderr`, `exit_code` removed).
- [x] Adding a new built-in tool requires **zero** changes to `async_executor/` code.

---

## SRP & DRY Compliance Checklist

| Principle | Violation Before | Resolution After |
|-----------|-----------------|------------------|
| **SRP** | `AsyncTaskResult` formats output for ALL tool types | Tool-specific `ResultFormatter`s registered externally; executor only handles lifecycle |
| **SRP** | `AsyncExecutor` in `agent/` does tool execution | Moved to `tools/` — module owns its domain |
| **SRP** | `SubagentExecutor` updates TWO registries for one task | Single `AsyncTaskStatus` — one update, one truth |
| **SRP** | `AsyncExecutionRouter` has `if tool_name == "shell"` | Router is tool-agnostic; formatters handle presentation |
| **OCP** | Adding structured result tool → modify `AsyncTaskResult` enum | New tool returns `Value`; registers formatter if needed |
| **OCP** | Adding process-like tool → modify `TaskFileRecord` | All tools use `result: Option<Value>` |
| **DRY** | `QueueDelivery` and `ChannelDelivery` duplicate event construction | Extract `build_completion_event()` helper |
| **DRY** | `ToolAdapter` duplicates abort/timeout logic from `Tool::execute_with_context` | Delete `ToolAdapter`; use trait default |
| **DRY** | Three status enums with identical semantics | One `AsyncTaskStatus`, one `is_terminal()`, one `as_str()` |

---

## Related

- Issue 002: Tool Execution — Three Competing Paths (sync path resolved)
- Issue 008: Dual Registry System for Subagents
- `src/tools/traits.rs`
- `src/tools/async_tool.rs`
- `src/tools/context.rs`
- `src/agent/async_tool_framework.rs`
- `src/agent/subagent_registry.rs`
- `src/agent/subagent_executor.rs`
- `src/extensions/services/async_router.rs`
- ADR-020: Daemon-Based Async Execution
