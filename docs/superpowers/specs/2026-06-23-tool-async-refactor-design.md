# Tool Async Refactor: Auto-Backgrounding on Timeout

**Date:** 2026-06-23
**Status:** Approved design — ready for implementation planning
**Author:** Design discussion between user and Claude
**Depends on:** `AsyncExecutionRouter` (ADR-018a), daemon-based async (ADR-020)

## Problem

The current async tool-calling system relies on `_async` and `_timeout` parameter injection through `AsyncExecutionRouter`. The design has accumulated several known problems:

1. **Leaky abstraction** — every tool's `execute()` defensively checks `params.get("_async")` for the bypass case (e.g. `agent_spawn.rs:425`).
2. **Reserved param bloat** — six reserved keys (`_async`, `_timeout`, `_callback`, `_progress`, `_priority`, `_retry`) extracted on every tool call, with non-trivial parsing.
3. **No auto-injection at the next agentic loop** — `AsyncResultQueueManager.process_queue()` is defined but never called by `AgenticLoop`. Today, the LLM must explicitly call `task status` to learn task completions.
4. **Process-bound tasks** — CLI mode spawns `tokio::task` in the calling process; tasks die when the CLI exits. ADR-020 documents the daemon-based fix.
5. **The `task` tool exists** but only manages existing tasks — it cannot spawn new ones or read output.

## Goals

- Every tool call gets a constant timeout (5 min default).
- When a tool exceeds the timeout, the work is *detached* (not killed) and surfaces to the agent as a background task with a receipt.
- The agent can read task output, list tasks, cancel tasks, and (explicitly) spawn any tool async — all through one unified `task` tool.
- When a task completes, its result is automatically injected into the next agentic loop iteration as a synthetic user-role message.
- Reserved `_async`/`_timeout` parameters are removed from the public surface.
- All existing async-aware code paths (`agent_spawn`, `shell`, etc.) route through the new framework with no special cases.

## Non-Goals

- Daemon-side completion delivery (CLI mode queue stays in-process). ADR-020's daemon-as-central-runtime work continues in parallel.
- Auto-recovery of in-flight tasks lost to daemon restart.
- Per-tool timeout configuration beyond a global default and an agent-level override.

## Design

### 1. Timeout enforcement

A single constant `DEFAULT_TOOL_TIMEOUT_SECS = 300` (5 minutes). The default is overridable per-agent via `AgentConfig::default_tool_timeout_secs`.

Every tool call goes through `AsyncExecutionRouter::route()`. The router wraps the tool's execution in `tokio::time::timeout(DEFAULT_TOOL_TIMEOUT, tool.run())`. The behavior is:

| Outcome | Surface to agent |
|---|---|
| `Ok(value)` before timeout | Tool result returned inline as today |
| `Err(Elapsed)` | Task detached to background; receipt returned |
| `Err(other)` | Error returned inline as today |

The detach is **soft**: the underlying `JoinHandle` is moved into the `AsyncExecutor`'s registry and continues to run to completion (or to a `taskstop` cancellation). The tool's work is not aborted. There is no grace window — at t=5min exactly, the agent sees a receipt.

There is no `_async` opt-in. The implicit behavior is: try sync; if it doesn't finish in time, detach.

### 2. The `task` tool

One tool, five actions. Extends the existing `TaskTool` in `src/tools/builtin/task_management.rs`.

```jsonc
// status (unchanged)
{ "action": "status", "task_id": "shell:abc-123" }

// list (unchanged)
{ "action": "list", "status_filter": "running", "tool_filter": "shell" }

// cancel (unchanged)
{ "action": "cancel", "task_id": "shell:abc-123" }

// spawn (new) — invoke any tool async, return receipt immediately
{ "action": "spawn", "tool": "shell", "params": { "command": "..." } }

// output (new) — read task output
{
  "action": "output",
  "task_id": "shell:abc-123",
  "blocking": false,        // optional, default false
  "tail_lines": 0           // optional, default 0 = full output
}
```

**Receipt format** (returned by both auto-detach and explicit `task spawn`):
```json
{
  "task_id": "shell:abc-123",
  "tool_name": "shell",
  "status": "running",
  "output_path": "<data_dir>/async_tasks/shell:abc-123.ndjson",
  "started_at": "2026-06-23T...",
  "preview": "<first ~2KB of streamed output, may be empty>"
}
```

**`task output` response format**:
```json
{
  "task_id": "shell:abc-123",
  "status": "completed" | "running" | "failed" | "cancelled" | "timed_out",
  "is_terminal": true,
  "result": <raw tool result>,
  "output_path": "<data_dir>/async_tasks/shell:abc-123.ndjson",
  "elapsed_seconds": 312.4,
  "completed_at": "2026-06-23T..." | null
}
```

The `result` field is the raw tool result — the value the tool would have returned synchronously. No envelope wrapping.

The `task` tool needs access to the `AsyncExecutor` (to dispatch `spawn`) and `ExtensionCore` (to resolve `tool: "shell"` to a `Box<dyn Tool>`). `TaskTool` gains two new optional fields: `executor: Option<Arc<AsyncExecutor>>` and `extension_core: Option<Weak<ExtensionCore>>`. Read-only actions (`status`, `list`) still work without them; `spawn` and `output` require both.

Param validation for `task spawn` is lazy: the inner `AsyncExecutor` runs the target tool, and the target tool's own JSON-schema validation catches bad params. Validation failures surface as a completion event with `status: "failed"` and `result: { error: "..." }`.

### 3. Auto-injection at the next agentic loop

A new struct `AsyncTaskCompletionQueue` in `src/extensions/framework/async_exec/executor/completion_queue.rs`:

```rust
pub struct AsyncTaskCompletionQueue {
    inner: Arc<Mutex<VecDeque<CompletionEvent>>>,
    notify: Arc<Notify>,
}

pub struct CompletionEvent {
    pub task_id: AsyncTaskId,
    pub tool_name: String,
    pub result: serde_json::Value,
    pub status: AsyncTaskStatus,
    pub completed_at: chrono::DateTime<Utc>,
    pub output_path: std::path::PathBuf,
    pub parent_session_key: String,
}

impl AsyncTaskCompletionQueue {
    pub fn push(&self, event: CompletionEvent);
    pub fn drain(&self) -> Vec<CompletionEvent>;
}
```

`AsyncExecutor` is extended: after a task reaches a terminal state, in addition to the existing delivery paths (file write, `AsyncResultQueueManager.enqueue`, `EventBus` events), the executor pushes a `CompletionEvent` to any registered `AsyncTaskCompletionQueue`. The new queue is one more sink alongside the existing ones.

`AgenticLoop::run_inner` drains the queue at the start of each iteration:

```rust
loop {
    iteration += 1;

    // NEW: drain completed async tasks for this session
    if let Some(queue) = &self.async_completion_queue {
        let events = queue.drain();
        let for_session: Vec<_> = events.into_iter()
            .filter(|e| e.parent_session_key == session_id)
            .collect();
        if !for_session.is_empty() {
            messages.push(synthetic_completion_message(&for_session));
        }
    }

    // ... rest of iteration unchanged
}
```

**Synthetic message format** (one message per drain, not one per event):

```rust
LlmMessage {
    role: MessageRole::User,  // user-role: model reads it as new context
    content: vec![
        ContentBlock::Text {
            text: format!("[Async task results — {} completed since last turn]", n),
        },
        ContentBlock::ToolResult {
            tool_call_id: format!("synthetic:{}", e.task_id),
            name: e.tool_name,
            content: e.result.to_string(),  // truncated preview; full content in output_path
        },
        // ...one ToolResult per event...
    ],
}
```

The drain happens once per iteration at the start. Tasks that complete mid-iteration wait until the next iteration to be surfaced. The synthetic message carries a *truncated* preview of each result; the agent calls `task output` for full content.

The `tool_call_id` is `synthetic:<task_id>` so the model can reference a specific completed task in its next tool call.

### 4. Migration — 5 commits on a single feature branch

A new branch `feature/tool-async-refactor` from `master`. Five commits, each independently revertible. No PR is opened until the user decides the branch is ready.

**Commit 1: New `AsyncTaskCompletionQueue` + executor fan-out.**

- New file `src/extensions/framework/async_exec/executor/completion_queue.rs`.
- `AsyncExecutor` gains an `Arc<AsyncTaskCompletionQueue>` field. The constructor wires a fresh queue; an `with_completion_queue` setter is also provided.
- After the existing `delivery.deliver(entry)` call in `execute_inner`, push a `CompletionEvent` to the queue.
- No behavior change for existing callers — the queue is a new sink that nobody reads yet.

**Commit 2: `task` tool gets `spawn` and `output` actions.**

- Extend `TaskTool` per Section 2.
- Add `executor` and `extension_core` fields and constructors.
- Update tool description and `parameters()` schema.
- Existing `status`/`list`/`cancel` tests still pass; add new tests for `spawn` and `output`.

**Commit 3: Drain at the start of `run_inner`.**

- `AgenticLoop::new` accepts an `Arc<AsyncTaskCompletionQueue>`.
- `run_inner` drains at the start of each iteration (per Section 3).
- Wire the queue from the daemon / `AgenticLoop` construction site.
- Synthetic message builder function lives in `agentic_loop.rs` (private).
- This is the behaviorally riskiest commit. Old code paths still work; old `AsyncResultQueueManager` still exists for code that hasn't migrated.

**Commit 4: Strip reserved params from `AsyncExecutionRouter`.**

- Delete `AsyncReservedParams` and its `parse_bool` / `effective_timeout` / `is_valid_callback` / `is_valid_priority` / `extract` methods and tests.
- Simplify `route()` — single code path with `tokio::time::timeout`.
- Rename `default_sync_timeout` to `default_tool_timeout`; remove `default_async_timeout`.
- Add `tracing::warn!` for callers that still pass `_async` or `_timeout` (one release of warnings before deletion).
- Update `shell.rs` module comment.
- Update `main.rs` and any docs that reference `_async` / `_timeout`.
- Add `#[ignore]` to any test that explicitly passes `_async`, with a TODO pointing to commit 5.

**Commit 5: Remove deprecated surface.**

- Delete the `tracing::warn!` from commit 4.
- Delete `agent_spawn.rs`'s special-case async branch (`_async` check, `async_mode` branch in `execute()`).
- Update `agent_spawn.rs` tool description and `parameters()` schema.
- Delete the `#[ignore]`'d tests from commit 4.
- Update `ADR-020` and `ADR-018a` to cross-reference the new ADR.
- Write `docs/architecture/adr/ADR-040-tool-timeout-and-async-refactor.md` summarizing the refactor and pointing to this spec.

### 5. Edge cases, error handling, testing

**Edge cases:**

1. Task completes during `stream_with_tools` mid-iteration — sits in the queue, surfaced at the next iteration's drain.
2. `task output blocking=true` exceeds 5 min — the `output` call itself auto-backgrounds, returning a receipt for *the read* (not the original task). Documented in the `task` tool description.
3. Daemon restart loses in-flight tasks — out of scope. Their `AsyncTaskEntry` rows stay `running` until the existing janitor purges them.
4. CLI mode `tokio::spawn` tasks die when CLI exits — same as today's `LocalAsyncTransport`. At CLI startup, if pending completions exist for the active session, surface a one-time warning.
5. Multiple LLM calls in flight for the same session — not currently supported. Queue is single-consumer.
6. `task spawn` for a tool that doesn't exist — spawned task immediately fails, completion event has `{ error: "tool not found" }`.
7. `task output` for a task in a different session — allowed (no session check on explicit lookup), consistent with existing `TaskTool::lookup_task` behavior. Documented.
8. `task` tool itself times out — auto-detached like any other tool. Degenerate but acceptable.

**Error handling matrix:**

| Condition | Result |
|---|---|
| Tool returns Ok before 5 min | Tool result returned synchronously |
| Tool exceeds 5 min | Receipt returned, task continues in background |
| Tool errors before 5 min | Error returned synchronously, no task created |
| `task spawn` with invalid tool name | Spawned task immediately fails, completion event has `{ error: "tool not found" }` |
| `task status` for unknown task_id | `{ error: "Task not found", task_id: "..." }` (existing behavior) |
| `task cancel` for already-terminal task | `{ success: false, message: "Task already terminal: ..." }` (existing behavior) |
| `task output blocking=true` exceeds 5 min | The `output` call itself auto-backgrounds |
| `CompletionEvent` for a session with no active loop | Stays in queue; consumed at next loop start, or purged by janitor |

**Testing strategy (TDD):**

*Unit tests:*
- `AsyncTaskCompletionQueue`: push/drain ordering, `Notify` semantics, multi-consumer safety.
- `AsyncExecutionRouter` (after refactor): the three cases — fast return, timeout-detach, tool error.
- `TaskTool` (after extension): each of the five actions. New actions get the most attention.
- Synthetic message builder: well-formed `LlmMessage` with correct role, header, and `tool_call_id` pattern.

*Integration tests (in `tests/`, using mock provider):*
- Shell command with sleep > timeout → receipt, task continues, completion visible in next iteration.
- `task spawn` with a fast command → receipt immediately, completion visible in next iteration.
- `task output blocking=true` on a fast task → returns the result inline.
- `task output blocking=false` on a slow task → returns running status.
- Multiple `task spawn` calls in one turn, all complete during next iteration → single batched synthetic message.

*E2E tests:*
- New `tests/cli_a2a/tool_async_v2` scenario: shell with timeout = 1s, command sleeps 3s → receipt, then later completion visible.
- Update `e2e_tests/extensions/tools/tool_async.ps1` to use natural long-running tool calls instead of `_async` opt-in.

*Per-commit verification:*
- Commits 1, 2: no behavior change. Existing tests pass; new unit tests pass.
- Commit 3: highest-risk. Full e2e suite must pass. Manual verification of synthetic-message format with a real provider.
- Commit 4: existing tests updated to remove `_async` usage; new tests confirm the warnings appear.
- Commit 5: full test suite passes; warnings are gone; no `_async` references remain in `src/`.

## Open Questions

None — all design decisions resolved during brainstorming. See the conversation log for the decision tree (timeout scope, tool shape, output mechanism, auto-injection timing, batched delivery, receipt content, output format, subagent unification, output streaming, timeout enforcement, configurability).
