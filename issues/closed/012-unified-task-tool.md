# Issue 012: Consolidate `task_status` / `task_list` into a Single Unified `task` Tool

**Severity:** MEDIUM  
**Status:** 🟢 **Closed — Implemented**  
**Labels:** `architecture`, `async-tools`, `tool-design`, `refactor`, `ux`  
**Reported:** 2026-04-30  
**Related:** Issue 011 (Generalize async task status tools — closed)

---

## Summary

Issue 011 successfully replaced the subagent-specific `agent_spawn_status` / `agent_spawn_list` with universal `task_status` / `task_list` tools. While this solved the asymmetry problem, it left us with **two narrowly-scoped tools** that query the same registry, share the same projection logic (`TaskView`), and differ only in their action (get-one vs. list-many).

This issue proposes consolidating them into a **single `task` tool** with an `action` parameter. This is the natural completion of the universal task management arc: one tool, one registry, one mental model.

---

## The Problem: Tool Sprawl at the Query Layer

### Current State (Post-Issue-011)

```
┌─────────────────────────────────────────┐
│  AsyncTaskRegistry (unified storage)    │
│  ├─ shell:abc-123   [running]           │
│  ├─ agent_spawn:xyz [completed]         │
│  └─ grep:def-456    [pending]           │
└─────────────────────────────────────────┘
         ▲                    ▲
         │                    │
   ┌─────┘                    └─────┐
   │                                │
┌──┴──────────┐            ┌───────┴────┐
│task_status  │            │ task_list  │
│- task_id    │            │- status_filter
│             │            │- tool_filter
└─────────────┘            └────────────┘
   Same registry lookup. Same TaskView projection. Different wrappers.
```

### Why Two Tools Is One Too Many

1. **Token overhead in LLM context:** Every tool name + description consumes context window. Two tools for the same domain is wasteful.
2. **No SRP violation to preserve:** `task_status` and `task_list` are not independent responsibilities — they are **query variants** over the same data model. SRP applies to the *registry* (data ownership) and the *tool* (LLM interface), not to every SQL-like operation.
3. **No place for `cancel`:** Cancellation is the obvious next feature (the executor already supports it via `AsyncExecutor::cancel`). Adding `task_cancel` as a third tool would make the sprawl worse.
4. **Future actions slot naturally:** `logs`, `retry`, `kill` — all fit into `action` without new tool registrations.

---

## Design Principles

- **SRP:** The `AsyncTaskRegistry` owns data. The `task` tool owns the LLM-facing query interface. Internal helper methods (`lookup_task`, `list_tasks`, `cancel_task`) are implementation details, not separate tools.
- **DRY:** `TaskView` projection, registry lookup, and filtering logic remain in one place — reused across all actions.
- **Future-proof:** New actions require only a new enum variant + a handler method. No new tool structs, no new registration blocks, no new config flags.
- **Zero tech debt:** Delete `TaskStatusTool` and `TaskListTool` entirely. No aliases, no deprecation shims. We are at dev stage.

---

## Proposed Resolution: Unified `task` Tool

### Tool Interface

```json
{
  "name": "task",
  "description": "Manage async tasks: check status, list tasks, or cancel a running task. Works for ALL async tasks regardless of tool type.",
  "parameters": {
    "type": "object",
    "properties": {
      "action": {
        "type": "string",
        "enum": ["status", "list", "cancel"],
        "description": "What to do: 'status' (get one task), 'list' (query tasks), 'cancel' (stop a task)"
      },
      "task_id": {
        "type": "string",
        "description": "Required for 'status' and 'cancel'. The task ID from the async receipt (e.g., 'shell:abc-123')"
      },
      "status_filter": {
        "type": "string",
        "description": "Optional filter for 'list': pending, running, completed, failed, cancelled, timed_out"
      },
      "tool_filter": {
        "type": "string",
        "description": "Optional filter for 'list': shell, agent_spawn, a2a_send, etc."
      }
    },
    "required": ["action"]
  }
}
```

### Response Shapes (per action)

| Action | Shape |
|--------|-------|
| `status` | Full `TaskView` JSON — same fields as current `task_status` output |
| `list` | `{ total, active, tasks: […] }` — same as current `task_list` output |
| `cancel` | `{ success: bool, task_id, previous_status, message }` |

### Cancel Semantics

The executor already has `AsyncExecutor::cancel(task_id) -> Result<bool>`. The `task` tool will:

1. Look up the task across all registries (same as `status`).
2. If found and non-terminal → set status to `Cancelled`, set `completed_at`, notify waiters → return `success: true`.
3. If found but already terminal → return `success: false` with message `"Task already terminal: <status>"`.
4. If not found → return `success: false` with message `"Task not found"`.

**Note:** Cancellation is registry-level (status update + notification). True abort of the underlying OS process or subagent runtime requires the executor to hold an `AbortHandle` or similar — out of scope for this issue, but the registry status update unblocks waiters and prevents result delivery, which is the 90% case.

---

## Implementation Log

| Date | Milestone |
|------|-----------|
| 2026-04-30 | Issue refined with SRP-compliant `CancelResult` in registry layer, serde-driven `TaskAction`, and pure response builders |
| 2026-04-30 | Implementation complete — all 13 files updated, `cargo test --lib`: 905 passed, 0 failed, 19 ignored |

## Refined Implementation Plan

### 0. Add `cancel_task` to `AsyncTaskRegistry` and `cancel_task_across_all_registries` to `registry.rs`

The registry already has `update_status` and `get_mut`. We need a first-class `cancel` that returns structured info, plus a global helper that mirrors `find_task_across_all_registries`.

```rust
// In AsyncTaskRegistry
pub fn cancel(&mut self, task_id: &AsyncTaskId) -> CancelResult {
    match self.tasks.get_mut(task_id) {
        Some(entry) => {
            let previous = entry.status.as_str().to_string();
            if entry.status.is_terminal() {
                CancelResult::AlreadyTerminal { previous }
            } else {
                entry.status = AsyncTaskStatus::Cancelled;
                entry.completed_at = Some(chrono::Utc::now());
                entry.notify_completion();
                CancelResult::Success { previous }
            }
        }
        None => CancelResult::NotFound,
    }
}

pub enum CancelResult {
    Success { previous: String },
    AlreadyTerminal { previous: String },
    NotFound,
}

// Global helper (mirrors find_task_across_all_registries)
pub async fn cancel_task_across_all_registries(task_id: &str) -> CancelResult {
    let task_id = task_id.to_string();
    let registries: Vec<SharedAsyncTaskRegistry> = {
        let map = global_registries().lock().unwrap();
        map.values().cloned().collect()
    };
    for registry in registries {
        let mut reg = registry.write().await;
        match reg.cancel(&task_id) {
            CancelResult::NotFound => continue,
            other => return other,
        }
    }
    CancelResult::NotFound
}
```

This keeps the **registry as the single source of truth** for cancellation state and avoids the tool layer reaching into private `global_registries()` with manual iteration logic.

### 1. Create `src/tools/task_management.rs` — Replace Contents

Delete `TaskStatusTool` and `TaskListTool`. Introduce a single `TaskTool`:

```rust
//! Universal Task Management Tool
//!
//! Provides `task` — a single tool for managing ANY async task.
//! Replaces `task_status` and `task_list` (Issue 011) with one unified interface.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

use crate::tools::async_executor::{
    cancel_task_across_all_registries, find_task_across_all_registries,
    list_all_tasks_across_all_registries, AsyncTaskRegistry, CancelResult,
    SharedAsyncTaskRegistry, TaskView,
};
use crate::tools::Tool;

// ------------------------------------------------------------------------------
// TaskAction — serde-driven, extensible
// ------------------------------------------------------------------------------

/// Actions supported by the `task` tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TaskAction {
    Status,
    List,
    Cancel,
}

// ------------------------------------------------------------------------------
// TaskTool — unified interface
// ------------------------------------------------------------------------------

/// Unified task management tool.
pub struct TaskTool {
    registry: Option<SharedAsyncTaskRegistry>,
}

impl TaskTool {
    #[must_use]
    pub fn with_registry(registry: SharedAsyncTaskRegistry) -> Self {
        Self {
            registry: Some(registry),
        }
    }

    #[must_use]
    pub fn global() -> Self {
        Self { registry: None }
    }

    // ------------------------------------------------------------------
    // Internal helpers — DRY across all actions
    // ------------------------------------------------------------------

    async fn lookup_task(&self, task_id: &str) -> Option<TaskView> {
        match &self.registry {
            Some(registry) => {
                let reg = registry.read().await;
                reg.get(task_id).map(TaskView::from_entry)
            }
            None => {
                let entry = find_task_across_all_registries(task_id).await?;
                Some(TaskView::from_entry(&entry))
            }
        }
    }

    async fn list_tasks(
        &self,
        status_filter: Option<&str>,
        tool_filter: Option<&str>,
    ) -> Vec<TaskView> {
        let entries = match &self.registry {
            Some(registry) => {
                let reg = registry.read().await;
                reg.list_tasks(None)
            }
            None => list_all_tasks_across_all_registries().await,
        };

        entries
            .into_iter()
            .map(|e| TaskView::from_entry(&e))
            .filter(|t| {
                status_filter.map_or(true, |f| t.status.as_str() == f)
                    && tool_filter.map_or(true, |f| t.tool_name == f)
            })
            .collect()
    }

    async fn cancel_task(&self, task_id: &str) -> CancelResult {
        match &self.registry {
            Some(registry) => {
                let mut reg = registry.write().await;
                reg.cancel(task_id)
            }
            None => cancel_task_across_all_registries(task_id).await,
        }
    }

    // ------------------------------------------------------------------
    // Response builders — keep execute() readable, DRY field mapping
    // ------------------------------------------------------------------

    fn build_status_response(task: &TaskView) -> serde_json::Value {
        // Start with a serde-derived base, then layer computed fields
        let mut base = json!({
            "task_id": task.task_id,
            "tool_name": task.tool_name,
            "status": task.status.as_str(),
            "is_terminal": task.is_terminal(),
            "parent_session_key": task.parent_session_key,
            "metadata_type": task.metadata_type,
            "created_at": task.created_at.to_rfc3339(),
            "label": task.label,
        });

        if let Some(completed_at) = task.completed_at {
            base["completed_at"] = json!(completed_at.to_rfc3339());
        }
        if let Some(ref result) = task.result {
            base["result"] = result.clone();
        }
        if let Some(duration) = task.duration() {
            base["duration_seconds"] = json!(duration.num_seconds());
        }

        base
    }

    fn build_list_response(tasks: Vec<TaskView>) -> serde_json::Value {
        let active_count = tasks.iter().filter(|t| !t.is_terminal()).count();

        let task_jsons: Vec<_> = tasks
            .into_iter()
            .map(|t| {
                json!({
                    "task_id": t.task_id,
                    "tool_name": t.tool_name,
                    "status": t.status.as_str(),
                    "is_terminal": t.is_terminal(),
                    "metadata_type": t.metadata_type,
                    "created_at": t.created_at.to_rfc3339(),
                    "label": t.label,
                })
            })
            .collect();

        json!({
            "total": task_jsons.len(),
            "active": active_count,
            "tasks": task_jsons
        })
    }

    fn build_cancel_response(result: CancelResult, task_id: &str) -> serde_json::Value {
        match result {
            CancelResult::Success { previous } => json!({
                "success": true,
                "task_id": task_id,
                "previous_status": previous,
                "message": "Task cancelled",
            }),
            CancelResult::AlreadyTerminal { previous } => json!({
                "success": false,
                "task_id": task_id,
                "previous_status": previous,
                "message": format!("Task already terminal: {previous}"),
            }),
            CancelResult::NotFound => json!({
                "success": false,
                "task_id": task_id,
                "message": "Task not found",
            }),
        }
    }
}

#[async_trait]
impl Tool for TaskTool {
    fn name(&self) -> &'static str {
        "task"
    }

    fn description(&self) -> String {
        r"Manage async tasks: check status, list tasks, or cancel a running task.

Works for ALL async tasks: shell, grep, agent_spawn, a2a_send, etc.

Parameters:
- action: 'status', 'list', or 'cancel' (required)
- task_id: Required for 'status' and 'cancel' — the task ID from the async receipt
- status_filter: Optional for 'list' — filter by status
- tool_filter: Optional for 'list' — filter by tool name

Returns structured data appropriate to the action."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["status", "list", "cancel"],
                    "description": "What to do: status (get one task), list (query tasks), cancel (stop a task)"
                },
                "task_id": {
                    "type": "string",
                    "description": "Required for 'status' and 'cancel'. The task ID from the async receipt (e.g., 'shell:abc-123')"
                },
                "status_filter": {
                    "type": "string",
                    "description": "Optional filter for 'list': pending, running, completed, failed, cancelled, timed_out"
                },
                "tool_filter": {
                    "type": "string",
                    "description": "Optional filter for 'list': shell, agent_spawn, a2a_send, etc."
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let action: TaskAction = serde_json::from_value(
            params
                .get("action")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("Missing required 'action' parameter"))?,
        )
        .map_err(|e| anyhow::anyhow!("Invalid action: {e}"))?;

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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::async_executor::{
        AsyncTaskEntry, AsyncTaskStatus, AsyncToolConfig,
    };

    #[tokio::test]
    async fn test_task_status_not_found() {
        let tool = TaskTool::global();
        let result = tool
            .execute(json!({"action": "status", "task_id": "nonexistent:task"}))
            .await
            .unwrap();
        assert_eq!(result["error"], "Task not found");
        assert_eq!(result["task_id"], "nonexistent:task");
    }

    #[tokio::test]
    async fn test_task_list_empty() {
        let tool = TaskTool::global();
        let result = tool.execute(json!({"action": "list"})).await.unwrap();
        assert_eq!(result["total"], 0);
        assert_eq!(result["active"], 0);
    }

    #[tokio::test]
    async fn test_task_status_with_registry() {
        let registry = Arc::new(tokio::sync::RwLock::new(AsyncTaskRegistry::new()));
        {
            let mut reg = registry.write().await;
            let entry = AsyncTaskEntry::new(
                "shell:test-123".to_string(),
                "shell".to_string(),
                json!({"command": "echo hello"}),
                "session_1".to_string(),
                AsyncToolConfig::default(),
            );
            reg.register(entry);
        }

        let tool = TaskTool::with_registry(registry);
        let result = tool
            .execute(json!({"action": "status", "task_id": "shell:test-123"}))
            .await
            .unwrap();

        assert_eq!(result["task_id"], "shell:test-123");
        assert_eq!(result["tool_name"], "shell");
        assert_eq!(result["status"], "pending");
        assert_eq!(result["metadata_type"], "none");
    }

    #[tokio::test]
    async fn test_task_list_with_registry_filters() {
        let registry = Arc::new(tokio::sync::RwLock::new(AsyncTaskRegistry::new()));
        {
            let mut reg = registry.write().await;
            let mut entry1 = AsyncTaskEntry::new(
                "shell:test-1".to_string(),
                "shell".to_string(),
                json!({}),
                "session_1".to_string(),
                AsyncToolConfig::default(),
            );
            entry1.status = AsyncTaskStatus::Completed {
                result: crate::tools::traits::ToolResult::success(json!({"done": true})),
            };
            reg.register(entry1);

            let entry2 = AsyncTaskEntry::new(
                "agent_spawn:test-2".to_string(),
                "agent_spawn".to_string(),
                json!({}),
                "session_1".to_string(),
                AsyncToolConfig::default(),
            );
            reg.register(entry2);
        }

        let tool = TaskTool::with_registry(registry);

        // No filter
        let result = tool.execute(json!({"action": "list"})).await.unwrap();
        assert_eq!(result["total"], 2);
        assert_eq!(result["active"], 1);

        // Filter by tool
        let result = tool
            .execute(json!({"action": "list", "tool_filter": "shell"}))
            .await
            .unwrap();
        assert_eq!(result["total"], 1);
        assert_eq!(result["tasks"][0]["tool_name"], "shell");

        // Filter by status
        let result = tool
            .execute(json!({"action": "list", "status_filter": "completed"}))
            .await
            .unwrap();
        assert_eq!(result["total"], 1);
        assert_eq!(result["tasks"][0]["status"], "completed");
    }

    #[tokio::test]
    async fn test_task_cancel_success() {
        let registry = Arc::new(tokio::sync::RwLock::new(AsyncTaskRegistry::new()));
        {
            let mut reg = registry.write().await;
            let entry = AsyncTaskEntry::new(
                "shell:cancel-me".to_string(),
                "shell".to_string(),
                json!({}),
                "session_1".to_string(),
                AsyncToolConfig::default(),
            );
            reg.register(entry);
        }

        let tool = TaskTool::with_registry(registry);
        let result = tool
            .execute(json!({"action": "cancel", "task_id": "shell:cancel-me"}))
            .await
            .unwrap();

        assert_eq!(result["success"], true);
        assert_eq!(result["task_id"], "shell:cancel-me");
        assert_eq!(result["previous_status"], "pending");
    }

    #[tokio::test]
    async fn test_task_cancel_already_terminal() {
        let registry = Arc::new(tokio::sync::RwLock::new(AsyncTaskRegistry::new()));
        {
            let mut reg = registry.write().await;
            let mut entry = AsyncTaskEntry::new(
                "shell:done".to_string(),
                "shell".to_string(),
                json!({}),
                "session_1".to_string(),
                AsyncToolConfig::default(),
            );
            entry.status = AsyncTaskStatus::Completed {
                result: crate::tools::traits::ToolResult::success(json!({})),
            };
            reg.register(entry);
        }

        let tool = TaskTool::with_registry(registry);
        let result = tool
            .execute(json!({"action": "cancel", "task_id": "shell:done"}))
            .await
            .unwrap();

        assert_eq!(result["success"], false);
        assert!(result["message"].as_str().unwrap().contains("already terminal"));
    }

    #[tokio::test]
    async fn test_task_cancel_not_found() {
        let tool = TaskTool::global();
        let result = tool
            .execute(json!({"action": "cancel", "task_id": "shell:missing"}))
            .await
            .unwrap();

        assert_eq!(result["success"], false);
        assert_eq!(result["message"], "Task not found");
    }
}
```

**Key refinements over the original proposal:**
- `TaskAction` uses `#[derive(Deserialize)]` with `#[serde(rename_all = "snake_case")]` — no hand-rolled `from_str`, fully extensible.
- `CancelResult` enum lives in `registry.rs` (data layer), not the tool. The tool only builds JSON from it.
- `cancel_task_across_all_registries` is a public async helper in `registry.rs`, avoiding the tool layer from reaching into the private `global_registries()` static.
- `build_list_response` uses `!t.is_terminal()` instead of hardcoding status strings.
- All response builders are pure functions (`&TaskView` or owned `Vec<TaskView>`) — no `&self` coupling.

### 2. Update `src/tools/mod.rs`

Replace:
```rust
pub use task_management::{TaskListTool, TaskStatusTool};
```
With:
```rust
pub use task_management::TaskTool;
```

### 3. Update `src/tools/builtin_registry.rs`

**Imports:**
Replace `TaskListTool, TaskStatusTool` with `TaskTool`.

**Registration block:**
Replace the two separate `if !disabled_set.contains(...)` blocks with one:
```rust
if config.enable_task_management && !disabled_set.contains("task") {
    let tool = Arc::new(TaskTool::global());
    BuiltinToolAdapter::register_tool(core, tool).await?;
}
```

**`all_tool_names()`:**
Replace `"task_status", "task_list"` with `"task"`.

**`is_agent_specific_builtin()`:**
No change needed — `"task"` is global, not agent-specific.

### 4. Update `src/runtime/tool_runtime.rs`

**Imports:**
Replace `TaskListTool, TaskStatusTool` with `TaskTool`.

**Registration list:**
Replace:
```rust
Arc::new(TaskStatusTool::global()),
Arc::new(TaskListTool::global()),
```
With:
```rust
Arc::new(TaskTool::global()),
```

### 5. Update `src/types/agent.rs`

In `ExtensionConfig::default()` whitelist:
Replace `"task_status"` and `"task_list"` with `"task"`.

### 6. Update `src/agent/agent.rs`

Update the comment referencing `task_status` and `task_list` to reference `task`:
```rust
// Note: `task` tool (status/list/cancel) is registered globally by the daemon's
// ToolRuntime::register_builtins() and searches across all registries at runtime.
// We do NOT register per-agent versions here to avoid shadowing the global
// registrations and breaking visibility of router async tasks.
```

### 7. Update `src/extensions/services/async_transport.rs`

Update the comment in `create_local_transport` referencing `task_status` and `task_list`:
```rust
// Use a shared registry from the global cache so that the `task` tool can
// find async tasks created by the router.
```

### 8. Update `src/tools/agent_spawn.rs`

Update the receipt note in `AgentSpawnTool::description()`:
```
Async mode: Use `_async: true` to spawn the subagent in the background. A receipt is returned immediately. Use the `task` tool with action="status" and the runId to check progress.
```

### 9. Update `src/daemon/state.rs` tests

In `test_appstate_has_registered_tools`, replace:
```rust
assert!(tool_runtime.has_tool("task_status").await, "task_status tool not registered");
assert!(tool_runtime.has_tool("task_list").await, "task_list tool not registered");
```
With:
```rust
assert!(tool_runtime.has_tool("task").await, "task tool not registered");
```

### 10. Update E2E Tests

#### `e2e_tests/subagent/subagent_status_list.ps1`
- Replace all `task_status` → `task` with `"action": "status"`
- Replace all `task_list` → `task` with `"action": "list"`
- Parameter `task_id` stays the same (already aligned in Issue 011)
- Response keys `task_id`, `tasks`, `status`, etc. stay the same
- Add a **TEST 4** for `cancel` action:
  - Spawn an async subagent with a long sleep (e.g., 30s)
  - Immediately call `task` with `"action": "cancel"` and the `task_id`
  - Verify response shows `success: true`
  - Verify subsequent `task` status query shows `cancelled`

#### `e2e_tests/subagent/subagent_async.ps1`
- Update any prompt text referencing `task_status` to reference `task` tool with `action=status`.

---

## File Changes Summary

| File | Action | Description |
|------|--------|-------------|
| `src/tools/async_executor/registry.rs` | **Modify** | Add `CancelResult`, `AsyncTaskRegistry::cancel`, `cancel_task_across_all_registries` |
| `src/tools/async_executor/mod.rs` | **Modify** | Re-export `CancelResult`, `cancel_task_across_all_registries` |
| `src/tools/task_management.rs` | **Rewrite** | Replace `TaskStatusTool` + `TaskListTool` with unified `TaskTool` |
| `src/tools/mod.rs` | **Modify** | Update re-exports: `TaskTool` only |
| `src/tools/builtin_registry.rs` | **Modify** | Register single `task` tool; update `all_tool_names()` |
| `src/runtime/tool_runtime.rs` | **Modify** | Register `TaskTool::global()` instead of two separate tools |
| `src/types/agent.rs` | **Modify** | Update whitelist: `"task"` replaces `"task_status"` + `"task_list"` |
| `src/agent/agent.rs` | **Modify** | Update comment referencing old tool names |
| `src/extensions/services/async_transport.rs` | **Modify** | Update comment referencing old tool names |
| `src/tools/agent_spawn.rs` | **Modify** | Update receipt note to reference `task` tool |
| `src/daemon/state.rs` | **Modify** | Update test assertions for unified `task` tool |
| `e2e_tests/subagent/subagent_status_list.ps1` | **Modify** | Use unified `task` tool; add cancel test |
| `e2e_tests/subagent/subagent_async.ps1` | **Modify** | Update prompt references to `task` tool |

---

## Acceptance Criteria

- [ ] `TaskStatusTool` and `TaskListTool` structs **completely removed** from codebase
- [ ] Single `TaskTool` registered as `"task"` handles `status`, `list`, and `cancel` actions
- [ ] `task` tool can query ANY async task by `task_id` via `action=status`
- [ ] `task` tool can list ALL async tasks with optional filters via `action=list`
- [ ] `task` tool can cancel non-terminal tasks via `action=cancel`
- [ ] `BuiltinToolRegistrar` registers exactly one `task` tool (not two)
- [ ] `BuiltinToolRegistrarConfig.enable_task_management` retained (no rename needed)
- [ ] `ExtensionConfig::default()` whitelist updated
- [ ] `ToolRuntime::register_builtins` registers `TaskTool` instead of the two old tools
- [ ] `agent_spawn` receipt note references `task` tool
- [ ] E2E tests updated and passing
- [ ] All existing unit tests pass (902 passed, 0 failed, 19 ignored baseline)

---

## Why Not Keep `task_status` + `task_list`?

| Argument | Counter |
|----------|---------|
| "LLMs handle discrete tools better" | Only for *unrelated* domains. `status`/`list`/`cancel` are query variants over the same registry. An `action` enum is *easier* for an LLM than three names to remember. |
| "Parameter schema gets messy" | Not if structured cleanly. `action` drives which other params are relevant. This is standard REST/API design. |
| "Two tools is already clean enough" | It leaves no home for `cancel` without a third tool. The trajectory is toward N tools for N operations. One tool scales better. |

---

## Related

- Issue 011: Generalize `agent_spawn_status`/`agent_spawn_list` to Universal Task Management Tools (closed)
- `src/tools/task_management.rs`
- `src/tools/async_executor/registry.rs`
- `src/tools/async_executor/executor.rs`
- `src/tools/builtin_registry.rs`
- `src/runtime/tool_runtime.rs`
