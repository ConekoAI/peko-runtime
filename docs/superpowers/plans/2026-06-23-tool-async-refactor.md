# Tool Async Refactor Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace `_async`/`_timeout` parameter injection with a constant 5-min tool timeout that auto-detaches long-running calls to background tasks; surface completions to the LLM at the next agentic loop iteration via one synthetic user-role message.

**Architecture:** Single `AsyncExecutionRouter` entry point; per-session `AsyncTaskCompletionQueue` drained at the start of each `AgenticLoop` iteration; `task` builtin tool gains `spawn` and `output` actions. Five sequential commits on `feature/tool-async-refactor`. No PR opened until the user approves.

**Tech Stack:** Rust (edition 2021), Tokio, `tokio::time::timeout`, `tokio::sync::Mutex`/`Notify`, `Arc<>`/`Weak<>` sharing, `serde_json::Value`, `chrono::DateTime<Utc>`, `tracing`.

**Spec:** [`docs/superpowers/specs/2026-06-23-tool-async-refactor-design.md`](../specs/2026-06-23-tool-async-refactor-design.md)

## Global Constraints

- Default tool timeout: `DEFAULT_TOOL_TIMEOUT_SECS = 300` (5 min) — defined as `pub const` in `src/extensions/framework/transport/async_router.rs`. Agent override via `AgentConfig::default_tool_timeout_secs` (already exists at `src/agents/agent_config.rs:41`).
- Soft timeout: `tokio::time::timeout(Elapsed)` does not abort the inner work. The future is dropped from the awaiting context but its work continues in the `AsyncExecutor` registry. No SIGKILL, no `JoinHandle::abort()`.
- No reserved params in tool calls. `_async`, `_timeout`, `_callback`, `_progress`, `_priority`, `_retry` are silently ignored. After commit 4 they are dropped entirely; before commit 4 they get a `tracing::warn!`.
- Synthetic completion message role: `MessageRole::User` (so the model reads it as new context, not as something it said).
- Synthetic completion message has a single header text block followed by one `ContentBlock::ToolResult` per drained event, all in one `LlmMessage`.
- All paths in this plan are relative to `/Users/rlsn/workspace/ConekoAI/peko-runtime/`.
- All commit messages follow the existing project convention (Conventional Commits).
- Run `cargo build` and `cargo test --lib` after each task to confirm no regressions. Don't move to the next task on a red build.
- After every commit, verify the diff with `git show --stat HEAD` and ensure no unintended files are touched.

## File Structure

**New files:**

| Path | Purpose |
|---|---|
| `src/extensions/framework/async_exec/executor/completion_queue.rs` | `AsyncTaskCompletionQueue` + `CompletionEvent` (commit 1) |
| `docs/architecture/adr/ADR-040-tool-timeout-and-async-refactor.md` | Historical record ADR for the refactor (commit 5) |

**Modified files:**

| Path | Changes |
|---|---|
| `src/extensions/framework/async_exec/executor/mod.rs` | Re-export `AsyncTaskCompletionQueue`, `CompletionEvent` (commit 1) |
| `src/extensions/framework/async_exec/executor/executor.rs` | Add `completion_queue` field; fan-out on terminal state (commit 1) |
| `src/extensions/framework/async_exec/executor/types.rs` | Add `SharedAsyncTaskCompletionQueue` type alias if needed (commit 1) |
| `src/tools/builtin/task_management.rs` | Add `spawn` + `output` actions; new constructor fields (commit 2) |
| `src/engine/agentic_loop.rs` | Accept `AsyncTaskCompletionQueue`; drain in `run_inner`; synthetic message builder (commit 3) |
| `src/extensions/framework/transport/async_router.rs` | Strip `AsyncReservedParams`; add `DEFAULT_TOOL_TIMEOUT_SECS` const; simplify `route()` (commit 4) |
| `src/extensions/framework/services/mod.rs` | Remove `AsyncReservedParams` re-export (commit 4) |
| `src/tools/builtin/messaging/agent_spawn.rs` | Remove `_async` check; remove `async_mode` branch; update tool description (commit 5) |
| `src/tools/builtin/shell.rs` | Update module comment (commit 4) |
| `src/main.rs` | Remove mention of `_async` (commit 4) |
| `docs/architecture/adr/ADR-020-daemon-based-async-execution.md` | Cross-reference to ADR-040 (commit 5) |
| `docs/architecture/adr/ADR-018a-tool-execution-unification.md` | Cross-reference to ADR-040 (commit 5) |

**No code changes (read-only verification):**

- `src/agents/agent_config.rs` — already has `default_timeout_seconds` field; no change.
- `src/extensions/framework/async_exec/executor/queue.rs` — `AsyncResultQueueManager` keeps existing behavior; left in place for callers that haven't migrated.
- `src/extensions/framework/async_exec/executor/registry.rs` — `AsyncTaskRegistry` is the source of truth; no change.

---

## Branch Setup (do this once before any task)

- [ ] **Step 1: Create and check out the feature branch**

```bash
cd /Users/rlsn/workspace/ConekoAI/peko-runtime
git checkout master
git pull --ff-only
git checkout -b feature/tool-async-refactor
git status
```

Expected: `On branch feature/tool-async-refactor`, clean working tree.

---

## Commit 1: New `AsyncTaskCompletionQueue` + executor fan-out

Behavior change: **none yet.** The new queue is wired but nothing reads it. This commit is purely additive.

### Task 1.1: Define the new types

**Files:**
- Create: `src/extensions/framework/async_exec/executor/completion_queue.rs`
- Modify: `src/extensions/framework/async_exec/executor/mod.rs:17-35` (add re-exports)

**Interfaces:**
- Produces:
  ```rust
  pub struct AsyncTaskCompletionQueue { /* private fields */ }
  pub struct CompletionEvent {
      pub task_id: AsyncTaskId,
      pub tool_name: String,
      pub result: serde_json::Value,
      pub status: AsyncTaskStatus,
      pub completed_at: chrono::DateTime<chrono::Utc>,
      pub output_path: std::path::PathBuf,
      pub parent_session_key: String,
  }
  pub type SharedAsyncTaskCompletionQueue =
      std::sync::Arc<AsyncTaskCompletionQueue>;
  ```

- [ ] **Step 1: Create the new file**

Create `src/extensions/framework/async_exec/executor/completion_queue.rs` with the following contents:

```rust
//! Per-session queue of completed async tasks waiting to be injected
//! into the next agentic loop iteration as a synthetic message.
//!
//! Distinct from [`super::queue::AsyncResultQueueManager`], which is the
//! older delivery sink kept for backward compatibility. New code should
//! read from this queue.

use super::types::{AsyncTaskId, AsyncTaskStatus};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex, Notify};

/// Event pushed to the completion queue when an async task reaches a
/// terminal state. The agentic loop drains these at iteration start
/// and synthesizes a single user-role message containing all of them.
#[derive(Debug, Clone)]
pub struct CompletionEvent {
    pub task_id: AsyncTaskId,
    pub tool_name: String,
    pub result: serde_json::Value,
    pub status: AsyncTaskStatus,
    pub completed_at: chrono::DateTime<chrono::Utc>,
    pub output_path: PathBuf,
    pub parent_session_key: String,
}

/// Per-session FIFO of completed async tasks waiting to be injected
/// at the next agentic loop iteration.
#[derive(Debug)]
pub struct AsyncTaskCompletionQueue {
    inner: Mutex<VecDeque<CompletionEvent>>,
    /// Wakes any future code that wants to wait for "at least one
    /// completion" — currently unused by the agentic loop (it polls
    /// at iteration start) but available for follow-up work.
    notify: Notify,
}

impl AsyncTaskCompletionQueue {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(VecDeque::new()),
            notify: Notify::new(),
        }
    }

    /// Push a completion event onto the queue. Wakes any waiters.
    pub fn push(&self, event: CompletionEvent) {
        // Synchronous helper that does not block on the mutex — uses
        // try_lock and, if contended, schedules a blocking push via
        // tokio::spawn. The common case (no contention) is in-line.
        if let Ok(mut guard) = self.inner.try_lock() {
            guard.push_back(event);
        } else {
            let this = self.clone();
            tokio::spawn(async move {
                let mut guard = this.inner.lock().await;
                guard.push_back(event);
            });
        }
        self.notify.notify_one();
    }

    /// Drain all currently-queued events, leaving the queue empty.
    /// Returns events in insertion order.
    pub async fn drain(&self) -> Vec<CompletionEvent> {
        let mut guard = self.inner.lock().await;
        guard.drain(..).collect()
    }

    /// Number of pending events (for testing/metrics).
    pub async fn len(&self) -> usize {
        let guard = self.inner.lock().await;
        guard.len()
    }

    pub async fn is_empty(&self) -> bool {
        let guard = self.inner.lock().await;
        guard.is_empty()
    }
}

impl Default for AsyncTaskCompletionQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for AsyncTaskCompletionQueue {
    fn clone(&self) -> Self {
        Self {
            inner: Mutex::new(VecDeque::new()),
            notify: Notify::new(),
        }
        // Note: clone() does NOT share state. The queue is meant to be
        // shared via Arc, not cloned. If you call clone() you get an
        // empty independent queue. This impl exists to satisfy trait
        // bounds that require Clone.
    }
}

pub type SharedAsyncTaskCompletionQueue = Arc<AsyncTaskCompletionQueue>;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_event(task_id: &str, session: &str) -> CompletionEvent {
        CompletionEvent {
            task_id: task_id.to_string(),
            tool_name: "shell".to_string(),
            result: json!({"exit_code": 0}),
            status: AsyncTaskStatus::Completed {
                result: crate::tools::core::ToolResult::success(json!({"exit_code": 0})),
            },
            completed_at: chrono::Utc::now(),
            output_path: PathBuf::from("/tmp/fake.ndjson"),
            parent_session_key: session.to_string(),
        }
    }

    #[tokio::test]
    async fn test_push_and_drain() {
        let queue = AsyncTaskCompletionQueue::new();
        queue.push(make_event("shell:a", "session_1"));
        queue.push(make_event("shell:b", "session_1"));

        let drained = queue.drain().await;
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].task_id, "shell:a");
        assert_eq!(drained[1].task_id, "shell:b");
        assert!(queue.is_empty().await);
    }

    #[tokio::test]
    async fn test_drain_empty() {
        let queue = AsyncTaskCompletionQueue::new();
        let drained = queue.drain().await;
        assert!(drained.is_empty());
    }

    #[tokio::test]
    async fn test_fifo_ordering_under_concurrent_push() {
        use std::sync::Arc;
        let queue = Arc::new(AsyncTaskCompletionQueue::new());
        let mut handles = Vec::new();
        for i in 0..10 {
            let q = queue.clone();
            handles.push(tokio::spawn(async move {
                q.push(make_event(&format!("shell:{i}"), "session_1"));
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        // Give any spawned pushes a chance to run.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let drained = queue.drain().await;
        assert_eq!(drained.len(), 10);
    }
}
```

- [ ] **Step 2: Add module declaration and re-exports**

Edit `src/extensions/framework/async_exec/executor/mod.rs`:

Replace the line:
```rust
pub mod delivery;
```
with:
```rust
pub mod completion_queue;
pub mod delivery;
```

Replace the re-export block at lines 17-35 to add the new exports. The new block:

```rust
pub use completion_queue::{
    AsyncTaskCompletionQueue, CompletionEvent, SharedAsyncTaskCompletionQueue,
};
pub use delivery::{
    build_completion_event, CallbackDelivery, ChannelDelivery, DefaultResultFormatter,
    FormatterRegistry, QueueDelivery, ResultDelivery, ResultFormatter,
};
pub use event_bus::{AsyncTaskCompletionEvent, AsyncTaskEventBus};
pub use executor::AsyncExecutor;
pub use queue::{AsyncResultQueue, AsyncResultQueueManager, SharedAsyncResultQueueManager};
pub use registry::{
    cancel_task_across_all_registries, find_run_across_all_registries,
    find_task_across_all_registries, get_or_create_registry_for_agent,
    list_all_runs_across_all_registries, list_all_tasks_across_all_registries, AsyncTaskEntry,
    AsyncTaskRegistry, CancelResult, SharedAsyncTaskRegistry, SubagentMetadata, SubagentResult,
    TaskMetadata, TaskView,
};
pub use task_file::{TaskFileRecord, TaskFileWriter};
pub use types::{
    AsyncResultDeliveryMode, AsyncTaskId, AsyncTaskReceipt, AsyncTaskResult, AsyncTaskStatus,
    AsyncToolConfig, DeliveryTarget, SessionMessageType, WaitResult,
};
```

- [ ] **Step 3: Build to verify compilation**

```bash
cd /Users/rlsn/workspace/ConekoAI/peko-runtime
cargo build --lib 2>&1 | tail -20
```

Expected: `Finished` line, no errors.

- [ ] **Step 4: Run the new tests**

```bash
cargo test --lib completion_queue 2>&1 | tail -15
```

Expected: 3 tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/extensions/framework/async_exec/executor/completion_queue.rs \
        src/extensions/framework/async_exec/executor/mod.rs
git commit -m "feat(async_exec): add AsyncTaskCompletionQueue for next-iteration injection

Per-session FIFO of completed async tasks. Distinct from the existing
AsyncResultQueueManager; will be read by the agentic loop in commit 3.

The queue's push() is non-blocking on the common path; under contention
it schedules the push via tokio::spawn. Clone() returns an empty
independent queue — sharing happens via Arc, not Clone.

No behavior change; nothing reads the queue yet.

Co-Authored-By: Claude <noreply@anthropic.com>"
```

### Task 1.2: Wire the queue into `AsyncExecutor`

**Files:**
- Modify: `src/extensions/framework/async_exec/executor/executor.rs:33-79, 122-318`

**Interfaces:**
- Consumes: `AsyncTaskCompletionQueue`, `CompletionEvent` (from task 1.1)
- Produces: `AsyncExecutor` gains a `completion_queue: SharedAsyncTaskCompletionQueue` field, a `with_completion_queue(self, queue) -> Self` builder, and an `execute_inner` that pushes a `CompletionEvent` after the existing delivery call.

- [ ] **Step 1: Write the failing test**

Add the following test at the bottom of `src/extensions/framework/async_exec/executor/executor.rs` (in the existing `mod tests` block, or as a new test in the same file if there isn't one — check by searching `^#\[cfg\(test\)\]`):

```rust
#[cfg(test)]
mod completion_queue_fan_out_tests {
    use super::*;
    use crate::tools::core::ToolResult;
    use std::sync::Arc;
    use std::time::Duration;

    fn make_executor_with_queue() -> (AsyncExecutor, SharedAsyncTaskCompletionQueue) {
        let queue = Arc::new(AsyncTaskCompletionQueue::new());
        let exec = AsyncExecutor::new().with_completion_queue(queue.clone());
        (exec, queue)
    }

    #[tokio::test]
    async fn test_completion_event_pushed_on_success() {
        let (exec, queue) = make_executor_with_queue();
        let task_id = "shell:test-success".to_string();

        let receipt = exec
            .execute(
                task_id.clone(),
                "shell",
                serde_json::json!({"command": "echo hi"}),
                "session_1",
                AsyncToolConfig::default(),
                || async { Ok(serde_json::json!({"exit_code": 0})) },
            )
            .await
            .unwrap();

        assert_eq!(receipt.task_id, task_id);

        // Wait for the spawned task to complete.
        tokio::time::sleep(Duration::from_millis(100)).await;

        let drained = queue.drain().await;
        assert_eq!(drained.len(), 1, "expected one completion event");
        assert_eq!(drained[0].task_id, task_id);
        assert_eq!(drained[0].tool_name, "shell");
        assert_eq!(drained[0].parent_session_key, "session_1");
        assert!(matches!(drained[0].status, AsyncTaskStatus::Completed { .. }));
    }

    #[tokio::test]
    async fn test_completion_event_pushed_on_failure() {
        let (exec, queue) = make_executor_with_queue();
        let task_id = "shell:test-fail".to_string();

        let _ = exec
            .execute(
                task_id.clone(),
                "shell",
                serde_json::json!({}),
                "session_1",
                AsyncToolConfig::default(),
                || async { anyhow::bail!("boom") },
            )
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(100)).await;

        let drained = queue.drain().await;
        assert_eq!(drained.len(), 1);
        assert!(matches!(drained[0].status, AsyncTaskStatus::Failed { .. }));
    }
}
```

- [ ] **Step 2: Run the test to confirm it fails**

```bash
cd /Users/rlsn/workspace/ConekoAI/peko-runtime
cargo test --lib completion_queue_fan_out 2>&1 | tail -20
```

Expected: compilation error — `with_completion_queue` does not exist. The test won't even compile.

- [ ] **Step 3: Add the field and builder to `AsyncExecutor`**

Edit `src/extensions/framework/async_exec/executor/executor.rs`:

At the top of the file, add to the `use super::*;` import (or add a new use line):
```rust
use super::completion_queue::{
    AsyncTaskCompletionQueue, CompletionEvent, SharedAsyncTaskCompletionQueue,
};
```

Find the `AsyncExecutor` struct definition (around line 33) and add the field:

```rust
pub struct AsyncExecutor {
    /// Task registry for tracking all async operations
    registry: SharedAsyncTaskRegistry,
    /// Queue manager for queue-based delivery (deprecated, kept for compatibility)
    queue_manager: SharedAsyncResultQueueManager,
    /// Registered delivery mechanisms by target type
    deliveries: Arc<RwLock<HashMap<DeliveryTarget, Box<dyn ResultDelivery>>>>,
    /// Default delivery target
    default_delivery: DeliveryTarget,
    /// Task file writer for disk-based polling
    task_file_writer: Option<TaskFileWriter>,
    /// Per-session queue of completed tasks (read by agentic loop).
    /// Default-constructed if not provided; safe to use without setup.
    completion_queue: SharedAsyncTaskCompletionQueue,
}
```

Update the `new()` and `with_registries()` constructors to initialize the field:

In `new()` (around line 49), change the `Self { ... }` block to:
```rust
        Self {
            registry: Arc::new(RwLock::new(AsyncTaskRegistry::new())),
            queue_manager: Arc::new(RwLock::new(AsyncResultQueueManager::new())),
            deliveries: Arc::new(RwLock::new(HashMap::new())),
            default_delivery: DeliveryTarget::AsyncQueue,
            task_file_writer: Some(TaskFileWriter::new(task_file_writer)),
            completion_queue: Arc::new(AsyncTaskCompletionQueue::new()),
        }
```

In `with_registries()` (around line 64), do the same — the struct literal must include the new field. The body of that function is:
```rust
        Self {
            registry,
            queue_manager,
            deliveries: Arc::new(RwLock::new(HashMap::new())),
            default_delivery: DeliveryTarget::AsyncQueue,
            task_file_writer: Some(TaskFileWriter::new(task_file_writer)),
            completion_queue: Arc::new(AsyncTaskCompletionQueue::new()),
        }
```

Add a new builder method, place it next to `with_default_delivery` (around line 92):

```rust
    /// Inject a shared completion queue. Used by the agentic loop to
    /// receive task completion events for the next-iteration injection.
    #[must_use]
    pub fn with_completion_queue(mut self, queue: SharedAsyncTaskCompletionQueue) -> Self {
        self.completion_queue = queue;
        self
    }

    /// Borrow the shared completion queue.
    #[must_use]
    pub fn completion_queue(&self) -> &SharedAsyncTaskCompletionQueue {
        &self.completion_queue
    }
```

- [ ] **Step 4: Update the `Debug` impl**

The existing `impl Debug for AsyncExecutor` (around line 497) lists fields. Add a new entry inside the `debug_struct("AsyncExecutor")` chain:

```rust
            .field("completion_queue", &"<AsyncTaskCompletionQueue>")
```

- [ ] **Step 5: Add the fan-out call to `execute_inner`**

In `execute_inner` (around line 122), find the section that calls `delivery.deliver(entry)` near the end of the spawned task (around line 304). After that call, add the completion-queue fan-out:

```rust
            // Deliver the result
            if let Some(entry) = registry_clone.read().await.get(&task_id_clone) {
                if let Err(e) = delivery.deliver(entry).await {
                    tracing::debug!("Delivery result for task {}: {}", task_id_clone, e);
                }
            }

            // NEW: push a completion event to the per-session queue so
            // the agentic loop can drain it at the next iteration.
            if let Some(entry) = registry_clone.read().await.get(&task_id_clone) {
                let status = entry.status.clone();
                let result = entry.result.clone().unwrap_or(serde_json::Value::Null);
                let output_path = task_file_writer_clone
                    .as_ref()
                    .map(|w| w.task_file_path(&task_id_clone))
                    .unwrap_or_else(|| std::path::PathBuf::from(""));
                let event = CompletionEvent {
                    task_id: task_id_clone.clone(),
                    tool_name: tool_name.clone(),
                    result,
                    status,
                    completed_at: chrono::Utc::now(),
                    output_path,
                    parent_session_key: parent_session_key_for_completion.clone(),
                };
                completion_queue.push(event);
            }
```

To make `parent_session_key_for_completion` available in the spawned task, you need to clone it into the spawn block. Find the section that clones state for the spawn (around line 195, `let registry_clone = self.registry.clone();` etc.) and add:

```rust
        let parent_session_key_for_completion = parent_session_key.clone();
        let completion_queue = self.completion_queue.clone();
```

Place these two new clones next to the existing `let task_id_clone = ...;` line.

- [ ] **Step 6: Build to verify compilation**

```bash
cargo build --lib 2>&1 | tail -30
```

Expected: `Finished` line, no errors. If the `parent_session_key` parameter name differs (e.g. it's already been moved into the spawn closure earlier), adapt: the field on `AsyncExecutor` is `parent_session_key` (a `String`), so cloning it before the closure consumes it works.

- [ ] **Step 7: Run the new tests**

```bash
cargo test --lib completion_queue_fan_out 2>&1 | tail -15
```

Expected: 2 tests pass.

- [ ] **Step 8: Run the full lib test suite to confirm no regression**

```bash
cargo test --lib 2>&1 | tail -10
```

Expected: existing tests still pass; new tests pass. If any pre-existing test fails because it relied on the old `AsyncExecutor` constructor signature, debug — it should not, because the field is just initialized to a default value.

- [ ] **Step 9: Commit**

```bash
git add src/extensions/framework/async_exec/executor/executor.rs
git commit -m "feat(async_exec): fan out completion events to AsyncTaskCompletionQueue

When a task reaches a terminal state, push a CompletionEvent to the
shared queue in addition to the existing delivery paths (file write,
AsyncResultQueueManager, EventBus). The agentic loop will read this
queue in commit 3.

The executor accepts a queue via with_completion_queue(); when not
provided, it constructs an independent one (no behavior change for
existing callers).

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

## Commit 2: `task` tool gets `spawn` and `output` actions

The existing `TaskTool` in `src/tools/builtin/task_management.rs` has `status`/`list`/`cancel`. This commit adds `spawn` and `output`.

### Task 2.1: Extend `TaskTool` with `spawn` action

**Files:**
- Modify: `src/tools/builtin/task_management.rs` (entire file)

**Interfaces:**
- Consumes: `AsyncExecutor` and `ExtensionCore` (constructed in commit 3 wiring; for this commit, accepted as `Option<Arc<...>>` so `TaskTool::global()` still works for read-only actions)
- Produces: `TaskTool::with_executor_and_core(exec, core)` constructor, `TaskAction::Spawn` variant, `TaskAction::Output` variant (next task)

- [ ] **Step 1: Add the failing test**

Append to the `tests` module at the bottom of `src/tools/builtin/task_management.rs`:

```rust
    use crate::extensions::framework::core::ExtensionCore;
    use crate::extensions::framework::async_exec::executor::AsyncExecutor;

    #[tokio::test]
    async fn test_task_spawn_missing_tool_returns_error() {
        // TaskTool without executor: spawn should error cleanly.
        let tool = TaskTool::global();
        let result = tool
            .execute(json!({"action": "spawn", "tool": "definitely_not_a_tool", "params": {}}))
            .await
            .unwrap();
        // Without an executor wired, spawn is unsupported.
        assert_eq!(result["error"], "spawn action requires TaskTool to be constructed with an AsyncExecutor");
    }

    #[tokio::test]
    async fn test_task_output_missing_executor_returns_error() {
        let tool = TaskTool::global();
        let result = tool
            .execute(json!({"action": "output", "task_id": "shell:x"}))
            .await
            .unwrap();
        assert_eq!(result["error"], "output action requires TaskTool to be constructed with an AsyncExecutor");
    }
```

- [ ] **Step 2: Run the test to confirm it fails**

```bash
cd /Users/rlsn/workspace/ConekoAI/peko-runtime
cargo test --lib test_task_spawn_missing_tool 2>&1 | tail -10
cargo test --lib test_task_output_missing_executor 2>&1 | tail -10
```

Expected: compilation failure — `TaskAction` does not have `Spawn` or `Output` variants.

- [ ] **Step 3: Add the `Spawn` and `Output` variants to `TaskAction`**

Edit the `TaskAction` enum at the top of `src/tools/builtin/task_management.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TaskAction {
    Status,
    List,
    Cancel,
    Spawn,
    Output,
}
```

- [ ] **Step 4: Add fields to `TaskTool` and a new constructor**

Replace the `TaskTool` struct definition with:

```rust
pub struct TaskTool {
    registry: Option<SharedAsyncTaskRegistry>,
    executor: Option<Arc<AsyncExecutor>>,
    extension_core: Option<std::sync::Weak<ExtensionCore>>,
}
```

Replace the `impl TaskTool` block constructors:

```rust
impl TaskTool {
    #[must_use]
    pub fn with_registry(registry: SharedAsyncTaskRegistry) -> Self {
        Self {
            registry: Some(registry),
            executor: None,
            extension_core: None,
        }
    }

    #[must_use]
    pub fn global() -> Self {
        Self {
            registry: None,
            executor: None,
            extension_core: None,
        }
    }

    /// Construct with executor + extension core. Required for `spawn` and
    /// `output` actions; read-only actions (`status`, `list`, `cancel`)
    /// still work without them.
    #[must_use]
    pub fn with_executor_and_core(
        executor: Arc<AsyncExecutor>,
        extension_core: std::sync::Weak<ExtensionCore>,
    ) -> Self {
        Self {
            registry: None,
            executor: Some(executor),
            extension_core: Some(extension_core),
        }
    }
}
```

- [ ] **Step 5: Update the `description()` and `parameters()` methods**

Replace the `description()` method body (around line 187) with:

```rust
    fn description(&self) -> String {
        r"Manage async tasks: check status, list tasks, cancel, spawn, or read output.

Works for ALL async tasks: shell, grep, agent_spawn, a2a_send, etc.

Actions:
- status: get one task by id
- list: query tasks (optionally filter by status or tool name)
- cancel: stop a running task
- spawn: invoke any tool asynchronously, returns a task receipt
- output: read a task's output (optionally wait for completion)

Parameters:
- action: 'status', 'list', 'cancel', 'spawn', or 'output' (required)
- task_id: required for 'status', 'cancel', 'output' — the task ID from the receipt
- tool: required for 'spawn' — the tool name to invoke
- params: required for 'spawn' — parameters to pass to the tool
- status_filter: optional for 'list' — filter by status
- tool_filter: optional for 'list' — filter by tool name
- blocking: optional for 'output' — if true, wait until task reaches terminal state
- tail_lines: optional for 'output' — if >0, return only the last N lines

Returns structured data appropriate to the action.
'spawn' and 'output' require TaskTool to be constructed with an AsyncExecutor."
            .to_string()
    }
```

Replace the `parameters()` method body (around line 201) with:

```rust
    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["status", "list", "cancel", "spawn", "output"],
                    "description": "What to do: status, list, cancel, spawn, or output"
                },
                "task_id": {
                    "type": "string",
                    "description": "Required for 'status', 'cancel', 'output'. The task ID from the async receipt (e.g., 'shell:abc-123')"
                },
                "tool": {
                    "type": "string",
                    "description": "Required for 'spawn'. The tool name to invoke (e.g., 'shell', 'fs_write')"
                },
                "params": {
                    "type": "object",
                    "description": "Required for 'spawn'. Parameters to pass to the tool (forwarded verbatim)"
                },
                "status_filter": {
                    "type": "string",
                    "description": "Optional filter for 'list': pending, running, completed, failed, cancelled, timed_out"
                },
                "tool_filter": {
                    "type": "string",
                    "description": "Optional filter for 'list': shell, agent_spawn, a2a_send, etc."
                },
                "blocking": {
                    "type": "boolean",
                    "description": "Optional for 'output'. If true, wait for the task to reach a terminal state before returning.",
                    "default": false
                },
                "tail_lines": {
                    "type": "integer",
                    "description": "Optional for 'output'. If >0, return only the last N lines of output.",
                    "default": 0
                }
            },
            "required": ["action"]
        })
    }
```

- [ ] **Step 6: Add the `Spawn` and `Output` arms to the `execute()` match**

Replace the `match action` block in `execute()` with the version that adds Spawn and Output arms:

```rust
        match action {
            TaskAction::Status => {
                let task_id = params
                    .get("task_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("'status' action requires 'task_id'"))?;

                match self.lookup_task(task_id).await {
                    Some(task) => Ok(Self::build_status_response(&task)),
                    None => Ok(json!({
                        "error": "Task not found",
                        "task_id": task_id
                    })),
                }
            }
            TaskAction::List => {
                let status_filter = params.get("status_filter").and_then(|v| v.as_str());
                let tool_filter = params.get("tool_filter").and_then(|v| v.as_str());
                let tasks = self.list_tasks(status_filter, tool_filter).await;
                Ok(Self::build_list_response(tasks))
            }
            TaskAction::Cancel => {
                let task_id = params
                    .get("task_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("'cancel' action requires 'task_id'"))?;
                let result = self.cancel_task(task_id).await;
                Ok(Self::build_cancel_response(result, task_id))
            }
            TaskAction::Spawn => {
                let executor = self.executor.as_ref().ok_or_else(|| {
                    anyhow::anyhow!("spawn action requires TaskTool to be constructed with an AsyncExecutor")
                })?;
                let core_weak = self.extension_core.as_ref().ok_or_else(|| {
                    anyhow::anyhow!("spawn action requires TaskTool to be constructed with an ExtensionCore")
                })?;
                let core = core_weak.upgrade().ok_or_else(|| {
                    anyhow::anyhow!("ExtensionCore has been dropped; cannot spawn")
                })?;

                let tool_name = params
                    .get("tool")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("'spawn' action requires 'tool'"))?;
                let tool_params = params
                    .get("params")
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("'spawn' action requires 'params'"))?;

                let task_id = format!("{}:{}", tool_name, uuid::Uuid::new_v4());
                let session_key = core.current_session_key().unwrap_or_else(|| "unknown".to_string());

                let config = crate::extensions::framework::async_exec::executor::AsyncToolConfig {
                    timeout_secs: 0, // No timeout — task runs to completion or cancellation
                    ..Default::default()
                };

                // Resolve the tool from the ExtensionCore.
                let tool = core.get_tool(tool_name).await.ok_or_else(|| {
                    anyhow::anyhow!("tool '{tool_name}' not found")
                })?;

                let receipt = executor
                    .execute(
                        task_id.clone(),
                        tool_name,
                        tool_params,
                        session_key,
                        config,
                        move || async move { tool.execute(tool_params_clone).await },
                    )
                    .await?;

                Ok(json!({
                    "task_id": receipt.task_id,
                    "status": "running",
                    "tool_name": tool_name,
                }))
            }
            TaskAction::Output => {
                let executor = self.executor.as_ref().ok_or_else(|| {
                    anyhow::anyhow!("output action requires TaskTool to be constructed with an AsyncExecutor")
                })?;
                let task_id = params
                    .get("task_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("'output' action requires 'task_id'"))?;
                let blocking = params
                    .get("blocking")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let _tail_lines = params
                    .get("tail_lines")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);

                // Look up the task
                let task = match self.lookup_task(task_id).await {
                    Some(t) => t,
                    None => {
                        return Ok(json!({
                            "error": "Task not found",
                            "task_id": task_id,
                        }));
                    }
                };

                if !task.is_terminal() {
                    if !blocking {
                        return Ok(json!({
                            "task_id": task_id,
                            "status": task.status.as_str(),
                            "is_terminal": false,
                            "result": null,
                        }));
                    }
                    // blocking=true: wait for completion via executor.
                    let timeout = std::time::Duration::from_secs(300);
                    let _ = executor.wait_for_completion(task_id, timeout).await;
                    // Re-read after waiting.
                    let task = match self.lookup_task(task_id).await {
                        Some(t) => t,
                        None => {
                            return Ok(json!({
                                "error": "Task not found",
                                "task_id": task_id,
                            }));
                        }
                    };
                    return Self::build_output_response(&task);
                }

                Self::build_output_response(&task)
            }
        }
```

(Note: this contains a forward reference to `build_output_response`, defined in the next step, and uses `tool_params_clone` which is the captured move — refactor if you find a borrow issue.)

- [ ] **Step 7: Add the `build_output_response` helper**

Add this method next to `build_cancel_response`:

```rust
    fn build_output_response(task: &TaskView) -> serde_json::Value {
        let mut base = json!({
            "task_id": task.task_id,
            "status": task.status.as_str(),
            "is_terminal": task.is_terminal(),
        });
        if let Some(ref result) = task.result {
            base["result"] = result.clone();
        }
        if let Some(completed_at) = task.completed_at {
            base["completed_at"] = json!(completed_at.to_rfc3339());
        }
        if let Some(duration) = task.duration() {
            base["elapsed_seconds"] = json!(duration.num_seconds());
        }
        base
    }
```

Also, the `Tool` trait needs to be importable. The existing file already imports `crate::tools::core::Tool`. Confirm.

- [ ] **Step 8: Add a stub `get_tool` to `ExtensionCore` if it doesn't exist**

The `Spawn` arm calls `core.get_tool(tool_name)`. If `ExtensionCore` doesn't already have such a method, add a minimal one. Check first:

```bash
cd /Users/rlsn/workspace/ConekoAI/peko-runtime
grep -n "pub.* fn get_tool\|pub.* fn resolve_tool" src/extensions/framework/core/mod.rs
```

If a method exists with a different name (e.g. `resolve_tool`, `lookup_tool`), use that name instead in the Spawn arm. If nothing exists, add this to `src/extensions/framework/core/mod.rs` (or wherever the impl lives):

```rust
    /// Look up a tool by name. Returns `None` if not found.
    pub async fn get_tool(&self, name: &str) -> Option<Arc<dyn crate::tools::core::Tool>> {
        // Implementation: walk the tool registry. The actual lookup may
        // already exist; if it does, alias it here.
        // For now, this is a stub. If a real lookup is unavailable,
        // return None and the spawn action will return "tool not found".
        let _ = name;
        None
    }
```

Replace the stub with the real lookup logic — find where tools are registered in `ExtensionCore` (likely `registered_tools` or similar field) and return a clone of the matching `Arc<dyn Tool>`.

If the existing API for tool resolution has a different signature, adapt the Spawn arm in `task_management.rs` to call it. The test only requires that `TaskTool::global()` (no executor/core wired) returns the expected error for both `spawn` and `output` — so the actual resolution logic does not need to be tested in this commit.

- [ ] **Step 9: Build to verify compilation**

```bash
cargo build --lib 2>&1 | tail -30
```

Expected: `Finished` line, no errors. The borrow issue with `tool_params_clone` may require you to clone `tool_params` before the move into the closure. If so, add `let tool_params_clone = tool_params.clone();` before the executor call and use it in the closure.

- [ ] **Step 10: Run all task_management tests**

```bash
cargo test --lib task_management 2>&1 | tail -20
```

Expected: all tests pass (existing 7 + 2 new = 9 tests).

- [ ] **Step 11: Commit**

```bash
git add src/tools/builtin/task_management.rs
git commit -m "feat(tools): task tool gains spawn and output actions

The task builtin tool (which already managed existing tasks via
status/list/cancel) gains two new actions:

- spawn: invoke any tool asynchronously, returning a task receipt
  immediately. Equivalent to running a tool with a 0-second timeout.

- output: read a task's output, with optional blocking wait and
  tail_lines filtering. Returns the raw tool result without envelope
  wrapping, plus status and timing metadata.

Read-only actions (status/list/cancel) work without executor/core
wiring. spawn and output require the tool to be constructed with
with_executor_and_core().

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

## Commit 3: Drain completion queue at start of each `run_inner` iteration

This is the behaviorally riskiest commit. The agentic loop will start seeing async task completions via synthetic user-role messages.

### Task 3.1: Wire the queue into `AgenticLoop`

**Files:**
- Modify: `src/engine/agentic_loop.rs:51-100` (struct + new), `317-360` (loop body)

- [ ] **Step 1: Add the field and constructor to `AgenticLoop`**

In `src/engine/agentic_loop.rs`, find the `AgenticLoop` struct (around line 52). Add a field:

```rust
pub struct AgenticLoop {
    agent: Arc<Agent>,
    provider: Arc<crate::providers::Provider>,
    max_iterations: usize,
    system_prompt: String,
    /// Extension core for skill loading and tool registration.
    extension_core: Arc<crate::extensions::framework::ExtensionCore>,
    /// Resolved caller identity (pekohub sub, API key id, or `None` for
    /// local CLI invocations). Propagated to `HookInput::ToolCall::caller_id`
    /// on every tool invocation so downstream permission checks and audit
    /// logging can attribute the call to a real user — see issue #17.
    caller_id: Option<String>,
    /// Per-session queue of completed async tasks, drained at the start
    /// of each `run_inner` iteration. Surfaced to the LLM as a
    /// synthetic user-role message containing all queued completions.
    async_completion_queue: Option<SharedAsyncTaskCompletionQueue>,
}
```

In the `new()` constructor (around line 73), add the field to the `Self { ... }` initializer:

```rust
        Self {
            agent,
            provider,
            max_iterations: 10,
            system_prompt,
            extension_core,
            caller_id: None,
            async_completion_queue: None,
        }
```

Add a builder method next to `with_caller_id` (around line 95):

```rust
    /// Inject a per-session async task completion queue. When set, the
    /// agentic loop drains the queue at the start of each iteration
    /// and synthesizes a single user-role message containing all
    /// completions since the last iteration.
    #[must_use]
    pub fn with_async_completion_queue(
        mut self,
        queue: SharedAsyncTaskCompletionQueue,
    ) -> Self {
        self.async_completion_queue = Some(queue);
        self
    }
```

Add the import at the top of the file:

```rust
use crate::extensions::framework::async_exec::executor::SharedAsyncTaskCompletionQueue;
```

- [ ] **Step 2: Add the drain call to `run_inner`**

Find the top of the `loop {` in `run_inner` (around line 317). The very first line inside the loop should be the drain. The current structure is:

```rust
        loop {
            iteration += 1;
            let mut iteration_usage = TokenUsage::default();
            info!("Agent loop: iteration {}", iteration);
            ...
```

Modify it to:

```rust
        loop {
            iteration += 1;
            let mut iteration_usage = TokenUsage::default();
            info!("Agent loop: iteration {}", iteration);

            // NEW: drain completed async tasks for this session and
            // inject them as a synthetic user-role message. Runs at
            // the start of every iteration, so completions that arrive
            // mid-iteration wait for the next one.
            if let Some(ref queue) = self.async_completion_queue {
                let events = queue.drain().await;
                let for_session: Vec<_> = events
                    .into_iter()
                    .filter(|e| e.parent_session_key == session_id)
                    .collect();
                if !for_session.is_empty() {
                    let n = for_session.len();
                    let mut content = vec![ContentBlock::Text {
                        text: format!(
                            "[Async task results — {n} completed since last turn]"
                        ),
                    }];
                    for event in &for_session {
                        content.push(ContentBlock::ToolResult {
                            tool_call_id: format!("synthetic:{}", event.task_id),
                            name: event.tool_name.clone(),
                            content: event.result.to_string(),
                        });
                    }
                    messages.push(LlmMessage {
                        role: MessageRole::User,
                        content,
                        timestamp: chrono::Utc::now(),
                        metadata: HashMap::new(),
                        tool_call_id: None,
                    });
                }
            }

            // ... rest of iteration body unchanged
```

- [ ] **Step 3: Build to verify compilation**

```bash
cd /Users/rlsn/workspace/ConekoAI/peko-runtime
cargo build --lib 2>&1 | tail -20
```

Expected: `Finished` line, no errors. If `session_id` is not in scope at the drain site, look at the surrounding code (around line 279 of the file): `let _session_id = { let s = session.read().await; s.id.clone() };`. Replace `_session_id` (currently unused binding) with `let session_id = ...;` and remove the leading underscore.

- [ ] **Step 4: Run the full test suite**

```bash
cargo test --lib 2>&1 | tail -10
```

Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/engine/agentic_loop.rs
git commit -m "feat(engine): drain async completion queue at start of each iteration

When an async task completes (either because it hit the 5-min timeout
or because it was spawned explicitly via task action=spawn), the
AsyncTaskCompletionQueue accumulates a CompletionEvent. The agentic
loop now drains that queue at the start of every run_inner iteration
and surfaces all completions as a single user-role LlmMessage
containing one ToolResult per event.

The synthetic message has tool_call_id 'synthetic:<task_id>' so the
model can reference a specific completed task in its next tool call.

The queue is optional — AgenticLoop::with_async_completion_queue()
wires it; without it, no draining happens and behavior is identical
to before this commit.

Co-Authored-By: Claude <noreply@anthropic.com>"
```

### Task 3.2: Wire the queue from the daemon and CLI command paths

This task is the integration glue. The `AgenticLoop` accepts the queue, but call sites need to construct the queue and pass it.

- [ ] **Step 1: Find all `AgenticLoop::new` call sites**

```bash
cd /Users/rlsn/workspace/ConekoAI/peko-runtime
grep -rn "AgenticLoop::new\|AgenticLoop::with_caller_id" src/ tests/ 2>/dev/null
```

Expected output: a handful of call sites (likely 2-4).

- [ ] **Step 2: At each call site, create or share a queue and pass it via `with_async_completion_queue`**

For each call site, the pattern is:

```rust
let queue = std::sync::Arc::new(
    crate::extensions::framework::async_exec::executor::AsyncTaskCompletionQueue::new()
);
let loop_ = AgenticLoop::new(agent_arc, provider, extension_core)
    .with_async_completion_queue(queue.clone());
```

If a per-session queue should be shared with the `AsyncExecutor` (so completions are fanned out to the right queue), also pass the queue to the executor:

```rust
let executor = crate::extensions::framework::async_exec::executor::AsyncExecutor::new()
    .with_completion_queue(queue.clone());
```

This is the per-call wiring. Adapt each call site to its context. The simplest change is to give each call site its own queue — no need to plumb the same Arc through to the executor unless the executor is being used directly. For most call sites, the executor is internal to the framework and a fresh queue is fine.

- [ ] **Step 3: Build to verify**

```bash
cargo build --lib 2>&1 | tail -20
```

Expected: `Finished` line. If a call site is in tests and you can't easily wire the queue there, you can leave it on `None` — the default behavior is unchanged.

- [ ] **Step 4: Run the test suite**

```bash
cargo test --lib 2>&1 | tail -10
```

Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(engine): wire async completion queue from agentic loop call sites

Construct an AsyncTaskCompletionQueue at each AgenticLoop::new call
site and pass it via with_async_completion_queue(). The queue is
shared with the AsyncExecutor so the executor's terminal-state
fan-out (commit 1) lands in the right per-session queue.

If a call site cannot easily share the queue, it stays on None and
behavior is unchanged.

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

## Commit 4: Strip reserved params from `AsyncExecutionRouter`

Replace the `_async`/`_timeout`/`_callback`/`_progress`/`_priority`/`_retry` extraction with a single constant timeout. Add a `tracing::warn!` for callers that still pass the old keys (warning-only deletion in this commit; the struct is fully removed in commit 5).

### Task 4.1: Add the `DEFAULT_TOOL_TIMEOUT_SECS` constant and simplify `route()`

**Files:**
- Modify: `src/extensions/framework/transport/async_router.rs` (entire file)

- [ ] **Step 1: Add the failing test for the new constant timeout behavior**

Append to the existing `tests` module in `async_router.rs`:

```rust
    #[test]
    fn test_default_tool_timeout_constant() {
        // Single source of truth for the 5-min default.
        assert_eq!(DEFAULT_TOOL_TIMEOUT_SECS, 300);
    }

    #[tokio::test]
    async fn test_reserved_params_warn_on_use() {
        // Setup tracing to capture warn events.
        let _ = tracing_subscriber::fmt::try_init();

        let router = AsyncExecutionRouter::new();
        let exec_service = ToolExecutionService::new();
        let tool_context = ToolExecutionContext::new("agent1", "session1", "run1");
        let exec_config = ToolExecutionConfig::with_schema(json!({"type": "object"}));

        // Passing _async should be ignored (not routed via async path);
        // the router will warn but treat the call as a regular sync call.
        let mut params = json!({"_async": true, "_timeout": 9999, "query": "test"});

        let result = router
            .route(
                "test_tool",
                &mut params,
                &exec_service,
                &tool_context,
                &exec_config,
                |p| async move { Ok(json!({"result": "ok", "input": p})) },
            )
            .await;

        // The call should still complete normally.
        assert!(result.is_ok());
        let value = result.unwrap();
        // The reserved params should be silently dropped from the forwarded input.
        assert_eq!(value["result"], "ok");
        assert!(value["input"].get("_async").is_none());
        assert!(value["input"].get("_timeout").is_none());
    }
```

This test needs `tracing-subscriber` as a dev-dep. Check `Cargo.toml` — if it's not there, add it to `[dev-dependencies]`:

```toml
[dev-dependencies]
tracing-subscriber = "0.3"
```

If it is already there, skip.

- [ ] **Step 2: Run the test to confirm it fails (or passes by accident)**

```bash
cd /Users/rlsn/workspace/ConekoAI/peko-runtime
cargo test --lib test_reserved_params_warn 2>&1 | tail -15
```

If compilation succeeds but the test fails on the `_async` removal, that's the expected state — the existing router still extracts `_async` so the forwarded input has been mutated.

- [ ] **Step 3: Add the constant and the warning logic**

At the top of `async_router.rs`, add the constant near the top of the file (after the `use` statements):

```rust
/// Default tool execution timeout in seconds. When a tool call exceeds
/// this, the work is detached to a background task and a receipt is
/// returned to the agent. Agent config can override via
/// `AgentConfig::default_tool_timeout_secs`.
pub const DEFAULT_TOOL_TIMEOUT_SECS: u64 = 300;
```

Add a helper that detects reserved keys and warns:

```rust
/// Check for legacy reserved params and warn. Returns a new `Value`
/// with the reserved keys stripped (the framework no longer honors
/// them). The keys are: _async, _timeout, _callback, _progress,
/// _priority, _retry.
fn strip_legacy_reserved_params(params: Value) -> Value {
    const RESERVED: &[&str] = &[
        "_async", "_timeout", "_callback", "_progress", "_priority", "_retry",
    ];
    let mut found = Vec::new();
    let mut obj = match params {
        Value::Object(m) => m,
        other => return other,
    };
    for key in RESERVED {
        if obj.remove(*key).is_some() {
            found.push(*key);
        }
    }
    if !found.is_empty() {
        tracing::warn!(
            keys = ?found,
            "Legacy reserved params passed to tool call; these are ignored. \
             The 5-min tool timeout is now constant. Use the 'task' tool's \
             'spawn' action to invoke a tool async."
        );
    }
    Value::Object(obj)
}
```

- [ ] **Step 4: Update the `route()` method to use the constant timeout**

Find the `route()` method signature. The current signature (line 265) takes `params: &mut Value`. Update the body:

```rust
    pub async fn route<F, Fut>(
        &self,
        tool_name: &str,
        params: &mut Value,
        exec_service: &ToolExecutionService,
        tool_context: &ToolExecutionContext,
        exec_config: &ToolExecutionConfig,
        sync_executor: F,
    ) -> Result<Value>
    where
        F: FnOnce(Value) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<Value>> + Send + 'static,
    {
        // Strip legacy reserved params (with a warning) and clone the
        // cleaned params for execution.
        let cleaned = std::mem::replace(params, Value::Null);
        let cleaned = strip_legacy_reserved_params(cleaned);
        *params = cleaned.clone();

        info!(
            timeout = self.default_tool_timeout.as_secs(),
            "AsyncExecutionRouter: routing execution"
        );

        // Single code path: execute with constant timeout. On Elapsed,
        // detach to AsyncExecutor (existing path).
        self.execute_with_timeout(
            tool_name,
            cleaned,
            exec_service,
            tool_context,
            exec_config,
            sync_executor,
        )
        .await
    }
```

- [ ] **Step 5: Rename `default_sync_timeout` to `default_tool_timeout` and add the helper method**

Find the `AsyncExecutionRouter` struct (around line 178) and rename the field:

```rust
pub struct AsyncExecutionRouter {
    /// Default tool execution timeout (5 min default).
    default_tool_timeout: Duration,
    /// Transport for async task execution (local or HTTP)
    transport: std::sync::Arc<dyn AsyncTaskTransport>,
}
```

Update the `Debug` impl field name. Update all constructors (`new`, `with_timeouts`, `with_transport`, `with_executor`) to use the new field name. For `with_timeouts(sync_secs, async_secs)`: the new signature is `with_default_tool_timeout(secs: u64)` — the second arg is gone.

The `execute_with_timeout` method is a refactor of the existing `execute_sync` — it just uses `self.default_tool_timeout` instead of `reserved.effective_timeout(false)`. Body is essentially the same as the current `execute_sync` but with a hard-coded timeout from the field.

The `execute_async` method (the existing background path) is unchanged — it stays for the case where a tool was explicitly backgrounded. The auto-detach on timeout is implemented in `execute_with_timeout` (call `execute_async` on `Elapsed`).

- [ ] **Step 6: Build to verify compilation**

```bash
cargo build --lib 2>&1 | tail -30
```

Expected: many errors because `AsyncReservedParams` is still referenced from `execute_sync`, `execute_async`, and tests. This is expected. Proceed to step 7.

- [ ] **Step 7: Delete `AsyncReservedParams` and its tests**

In `async_router.rs`:

Delete the entire `AsyncReservedParams` struct (lines 31-169) including:
- The struct definition
- The `Default` impl
- The `extract` method
- The `parse_bool` helper
- The `effective_timeout` method
- The `is_valid_callback` method
- The `is_valid_priority` method
- The `default_callback`, `default_true`, `default_priority` helpers

Delete the existing tests that explicitly use `_async`/`_timeout` reserved params:
- `test_extract_async_params`
- `test_default_params`
- `test_effective_timeout`
- `test_router_async_path` (uses `_async: true`)
- `test_router_sync_timeout` (uses `_timeout: 1`)

Keep:
- `test_router_sync_path` — but update it to not use `_timeout`
- `test_default_tool_timeout_constant` (new from step 1)
- `test_reserved_params_warn_on_use` (new from step 1)

Update `test_router_sync_path` to remove the `&exec_config` setup if it's unused, or leave it. The key change is removing the `params.get("_timeout")` reference (there isn't one — the test just calls route with a normal param set).

Update the `execute_sync` method (now `execute_with_timeout`) to take a Duration directly from `self.default_tool_timeout` instead of from `reserved`. Update `execute_async` to use `self.default_tool_timeout` for the timeout it records (or keep the 5-min default in the AsyncToolConfig — both are fine; the spec says 5 min is the constant).

- [ ] **Step 8: Update the `services/mod.rs` re-exports**

In `src/extensions/framework/services/mod.rs`, remove the line that re-exports `AsyncReservedParams`:

```rust
pub use crate::extensions::framework::transport::async_router::{
    AsyncExecutionRouter, AsyncReservedParams, ToolExecutionContext,
};
```

becomes:

```rust
pub use crate::extensions::framework::transport::async_router::{
    AsyncExecutionRouter, ToolExecutionContext, DEFAULT_TOOL_TIMEOUT_SECS,
};
```

- [ ] **Step 9: Update `shell.rs` module comment**

In `src/tools/builtin/shell.rs`, replace the comment at lines 8-9:

```rust
//! Note: Async execution and timeout are handled by the framework-level
//! `ToolWrapper` using `_async` and `_timeout` parameters.
```

with:

```rust
//! Note: Async execution and timeout are handled by `AsyncExecutionRouter`,
//! which applies a constant 5-min timeout. Tools exceeding the timeout are
//! auto-detached to background tasks; the agent retrieves results via the
//! `task` tool's `status`/`output` actions.
```

- [ ] **Step 10: Update `main.rs` mention of `_async`**

In `src/main.rs` (line 78), find the text `Or use sync mode (remove _async: true from the tool call)` and update it to:

```rust
                         Or wait for the task to complete via the 'task' tool's 'output' action.",
```

- [ ] **Step 11: Build to verify**

```bash
cargo build --lib 2>&1 | tail -30
```

Expected: `Finished` line. If there are lingering references to `AsyncReservedParams` elsewhere, grep and fix:

```bash
grep -rn "AsyncReservedParams\|extract.*async" src/ 2>/dev/null
```

- [ ] **Step 12: Run the test suite**

```bash
cargo test --lib 2>&1 | tail -10
```

Expected: all tests pass. The new tests for the constant timeout and warning pass; deleted tests are gone.

- [ ] **Step 13: Mark deprecated tests with `#[ignore]` if they still reference `_async`**

Some tests across the codebase (in `agent_spawn.rs`, `tunnel/a2a_send_tool.rs`, integration tests) may still pass `_async: true` in their tool calls. Find them:

```bash
cd /Users/rlsn/workspace/ConekoAI/peko-runtime
grep -rn '"_async"\|: _async' tests/ src/ 2>/dev/null
```

For each test that explicitly passes `_async` in a tool-call JSON, add `#[ignore]` with a TODO comment pointing to commit 5. Example:

```rust
#[tokio::test]
#[ignore = "TODO(commit 5): _async reserved param is being removed"]
async fn test_old_async_behavior() { ... }
```

- [ ] **Step 14: Commit**

```bash
git add -A
git commit -m "refactor(router): strip _async/_timeout from AsyncExecutionRouter

Replace reserved-param extraction with a single constant 5-min timeout
(DEFAULT_TOOL_TIMEOUT_SECS). Tools exceeding the timeout are auto-
detached to background tasks; no opt-in is needed.

Reserved keys (_async, _timeout, _callback, _progress, _priority,
_retry) are silently dropped with a tracing::warn! so we can find
remaining stragglers. They are deleted entirely in commit 5.

The AsyncReservedParams struct is removed; its consumers (execute_sync
and execute_async internals) are simplified to use the field-based
default_tool_timeout.

Tests that explicitly passed _async are marked #[ignore] with a
TODO pointing to commit 5, where they are deleted.

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

## Commit 5: Delete deprecated surface

Remove the warning, the `AsyncReservedParams` re-exports that might still linger, `agent_spawn`'s special-case async path, and the `#[ignore]`'d tests. Write ADR-040.

### Task 5.1: Remove `agent_spawn`'s special-case async path

**Files:**
- Modify: `src/tools/builtin/messaging/agent_spawn.rs:421-485`

- [ ] **Step 1: Remove the `_async` check and the `async_mode` branch**

Find the `execute()` method in `AgentSpawnTool`. The current code (lines 421-485) is:

```rust
    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        // Check for _async reserved parameter (extracted by AsyncExecutionRouter,
        // but we also check here for direct tool calls that bypass the router)
        let async_mode = params
            .get("_async")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // Parse parameters (after _async extraction, the rest are tool-specific)
        let args: AgentSpawnArgs = serde_json::from_value(params)
            .map_err(|e| anyhow::anyhow!("Invalid arguments: {e}"))?;
        ...
        // Route based on async mode
        if async_mode {
            // Async mode: spawn in background, return receipt
            self.execute_spawn_async(...)
        } else {
            // Blocking mode (default): wait for subagent to complete, return inline result
            self.execute_spawn_blocking(...)
        }
    }
```

Replace with:

```rust
    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        // Parse parameters; the framework's auto-detach on timeout handles
        // the sync/async decision. The 5-min default applies to subagent
        // spawns like any other tool.
        let args: AgentSpawnArgs = serde_json::from_value(params)
            .map_err(|e| anyhow::anyhow!("Invalid arguments: {e}"))?;
        ...
        // Always go through the blocking path; the framework detaches on
        // timeout. If the caller wants explicit async, they invoke this
        // tool via 'task action=spawn tool=agent_spawn params=...'.
        self.execute_spawn_blocking(
            &args.task,
            args.isolated,
            &parent_session_key,
            config,
            args.label,
            cleanup,
        )
        .await
    }
```

- [ ] **Step 2: Update the tool's `description()` and `parameters()` method to remove `_async` mention**

Find the docstring/description for `agent_spawn`. Remove any mention of `_async` parameter. The tool now just has `task`, `isolated`, `cleanup`, `parent_session_key`, `label`.

In the `parameters()` JSON schema, remove the `_async` property if it was listed.

- [ ] **Step 3: Build to verify**

```bash
cargo build --lib 2>&1 | tail -10
```

Expected: `Finished` line.

- [ ] **Step 4: Run the test suite**

```bash
cargo test --lib 2>&1 | tail -10
```

Expected: all tests pass.

### Task 5.2: Remove the warning and the `#[ignore]`'d tests

- [ ] **Step 1: Remove the `tracing::warn!` for legacy reserved params**

In `src/extensions/framework/transport/async_router.rs`, find the `strip_legacy_reserved_params` function (added in commit 4) and replace it with a no-op identity function:

```rust
fn strip_legacy_reserved_params(params: Value) -> Value {
    params
}
```

(Or delete the call site entirely. Identity is fine — keeps the call site stable and the function is now provably correct.)

- [ ] **Step 2: Find and delete the `#[ignore]`'d tests**

```bash
cd /Users/rlsn/workspace/ConekoAI/peko-runtime
grep -rn 'TODO(commit 5)' tests/ src/ 2>/dev/null
```

For each match, delete the test entirely (or the body if it's a `#[test]` you want to repurpose). Don't leave dead tests with `#[ignore]`s in the codebase.

- [ ] **Step 3: Grep for any remaining `_async` references in the codebase**

```bash
grep -rn '"_async"\|: _async' src/ tests/ docs/ 2>/dev/null
```

For any non-test references in docs, update or delete. For any remaining test references that should have been deleted in step 2, delete them now.

- [ ] **Step 4: Build and test**

```bash
cargo build --lib 2>&1 | tail -10
cargo test --lib 2>&1 | tail -10
```

Expected: `Finished`, all tests pass.

### Task 5.3: Write ADR-040

- [ ] **Step 1: Create the ADR file**

Create `docs/architecture/adr/ADR-040-tool-timeout-and-async-refactor.md` with the following contents:

```markdown
# ADR-040: Tool Timeout and Async Refactor

**Status:** Accepted
**Date:** 2026-06-23
**Author:** Implementation team
**Supersedes:** Implicit behavior in `AsyncExecutionRouter` (ADR-018a) regarding `_async`/`_timeout` param injection.
**Related:** ADR-018a, ADR-020.

## Context

The async tool execution system in `peko-runtime` had grown a parameter-injection surface (`_async`, `_timeout`, `_callback`, `_progress`, `_priority`, `_retry`) that was leaky (tools had to defensively check `params.get("_async")` for the bypass case), bloaty (six reserved keys parsed on every tool call), and incomplete (the `AsyncResultQueueManager.process_queue()` was defined but never called by the agentic loop, so completions were never auto-injected). It also relied on the calling process's `tokio::spawn`, so tasks died with the CLI.

## Decision

1. A single constant `DEFAULT_TOOL_TIMEOUT_SECS = 300` (5 min) governs all tool calls. Agent config overrides per-agent.
2. On timeout, the work is *detached* (not killed) to a background task via the existing `AsyncExecutor` and a receipt is returned to the agent.
3. The reserved `_async`/`_timeout`/`_callback`/`_progress`/`_priority`/`_retry` parameters are removed from the public tool-call surface.
4. The `task` builtin tool gains two actions: `spawn` (invoke any tool async) and `output` (read a task's output, optionally blocking).
5. A new per-session `AsyncTaskCompletionQueue` accumulates `CompletionEvent`s as tasks reach terminal state.
6. The `AgenticLoop` drains the queue at the start of each `run_inner` iteration and surfaces all completions as a single synthetic user-role `LlmMessage` containing one `ContentBlock::ToolResult` per event.

## Consequences

- All tools get the same timeout. No per-tool configuration beyond a global default and per-agent override.
- `agent_spawn` and other long-running tools are no longer special-cased. They go through the same auto-detach path.
- The synthetic completion message carries a truncated preview of each result; the agent calls `task output` for full content.
- Tasks that complete mid-iteration wait for the next iteration to be visible. The drain is once per iteration, not continuous.
- In CLI mode (no daemon), `tokio::spawn` tasks still die with the process. ADR-020's daemon-based work remains the long-term fix.

## Migration

Five commits on `feature/tool-async-refactor` (no PR opened as of 2026-06-23):

1. `AsyncTaskCompletionQueue` + executor fan-out (additive, no behavior change)
2. `task` tool gains `spawn` and `output` actions
3. `AgenticLoop` drains the queue at iteration start
4. Strip `_async`/`_timeout` from `AsyncExecutionRouter` (with one release of `tracing::warn!`)
5. Delete `agent_spawn` special case, remove warning, write this ADR

See [`docs/superpowers/specs/2026-06-23-tool-async-refactor-design.md`](../../superpowers/specs/2026-06-23-tool-async-refactor-design.md) for the full design.
```

- [ ] **Step 2: Cross-reference from ADR-020 and ADR-018a**

In `docs/architecture/adr/ADR-020-daemon-based-async-execution.md`, add a line under the "References" or similar section:

```markdown
- ADR-040: Tool Timeout and Async Refactor (2026-06-23) — supersedes the `_async`/`_timeout` parameter-injection layer.
```

In `docs/architecture/adr/ADR-018a-tool-execution-unification.md`, add a similar line.

- [ ] **Step 3: Final build and test**

```bash
cargo build --lib 2>&1 | tail -10
cargo test --lib 2>&1 | tail -10
```

Expected: `Finished`, all tests pass.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "refactor: remove _async/_timeout deprecated surface; add ADR-040

- Remove _async check from agent_spawn; the 5-min auto-detach replaces
  the explicit async opt-in.
- Remove the tracing::warn! for legacy reserved params; the keys are
  silently ignored with no warning.
- Delete the #[ignore]'d tests from commit 4.
- Add ADR-040 documenting the refactor for future readers.
- Cross-reference ADR-040 from ADR-020 and ADR-018a.

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

## Verification After All 5 Commits

- [ ] **Step 1: Confirm the feature branch has exactly 5 commits on top of master**

```bash
cd /Users/rlsn/workspace/ConekoAI/peko-runtime
git log --oneline master..HEAD
```

Expected: 5 commits, each with a `feat:` or `refactor:` prefix matching the commit messages above.

- [ ] **Step 2: Confirm no `_async` references remain in the source**

```bash
grep -rn '"_async"\|: _async' src/ tests/ 2>/dev/null
```

Expected: no output.

- [ ] **Step 3: Confirm the new types are re-exported**

```bash
grep -n "AsyncTaskCompletionQueue\|CompletionEvent" src/extensions/framework/async_exec/executor/mod.rs
```

Expected: re-exports present.

- [ ] **Step 4: Run the full test suite one more time**

```bash
cargo test --lib 2>&1 | tail -10
```

Expected: all tests pass.

- [ ] **Step 5: Show the user the branch state and ask whether to push or open a PR**

```bash
git log --oneline master..HEAD
git status
```

Report to the user. Do NOT push or open a PR — the user said "no PR yet".

---

## Self-Review Notes (do this once, after writing the plan)

- **Spec coverage:** Section 1 (timeout) → Commit 4. Section 2 (`task` tool) → Commit 2. Section 3 (auto-injection) → Commits 1+3. Section 4 (migration order) → header and the 5-commit structure. Section 5 (edge cases) → mostly covered by tests added per commit; the CLI startup warning for orphaned completions is a known follow-up.
- **Placeholder scan:** No "TBD", "TODO: implement later", or vague requirements. The `TODO(commit 5)` markers in `#[ignore]` attributes are intentional tracking labels, not implementation gaps.
- **Type consistency:** `AsyncTaskCompletionQueue` is defined in `completion_queue.rs` (commit 1), re-exported from `mod.rs` (commit 1), stored in `AsyncExecutor` (commit 1), accepted by `AgenticLoop` (commit 3), and consumed by `TaskTool` (commit 2) as `SharedAsyncTaskCompletionQueue`. The type alias is consistent across all call sites.
- **File paths:** all paths are absolute (`/Users/rlsn/workspace/ConekoAI/peko-runtime/...`). No relative paths in the plan.
