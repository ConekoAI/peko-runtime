# Issue 011: Generalize `agent_spawn_status`/`agent_spawn_list` to Universal Task Management Tools

**Severity:** MEDIUM  
**Status:** 🟢 **Closed — Implemented**  
**Labels:** `architecture`, `async-tools`, `tool-design`, `refactor`, `ux`  
**Reported:** 2026-04-29  
**Related:** Issue 006 (Three Async Tool Frameworks — closed), Issue 008 (Dual Registry System — closed)

---

## Summary

The `_async` reserved parameter is **universal** — it works for ALL tools (`shell`, `grep`, `a2a_send`, `agent_spawn`, etc.) via the `AsyncExecutionRouter`. However, the status/query tools (`agent_spawn_status`, `agent_spawn_list`) are **narrowly scoped to subagent runs only**. This creates an asymmetry: agents can spawn any tool asynchronously, but can only check status on subagent tasks.

This issue proposes replacing the subagent-specific status tools with universal `task_status`/`task_list` tools that work across **all async task types**.

---

## The Asymmetry

### Universal Async Spawn (Works for ALL tools)

```json
// Any tool can be async
{"command": "./long-build.sh", "_async": true, "_timeout": 300}     // shell
{"pattern": "src/**/*.rs", "_async": true}                           // glob
{"target_agent": "analyzer", "message": "...", "_async": true}       // a2a_send
{"task": "Review code", "_async": true}                              // agent_spawn
```

The `AsyncExecutionRouter` (Issue 006) returns a **generic receipt** for all of these:

```json
{
  "_async_status": "queued",
  "task_id": "shell:abc-123",
  "task_file": "/path/to/async_tasks/shell_abc-123.json"
}
```

### Narrow Status Query (Subagent-only)

But the only status tool is `agent_spawn_status`:

```json
// This works
{"run_id": "agent_spawn:xyz-789"}

// These have NO equivalent tool
{"run_id": "shell:abc-123"}      // ❌ No shell_status tool
{"run_id": "grep:def-456"}       // ❌ No grep_status tool
{"run_id": "a2a_send:ghi-012"}   // ❌ No a2a_send_status tool
```

---

## Evidence from E2E Tests

| Test File | Uses `agent_spawn_status` | Uses `agent_spawn_list` | Actual Monitoring Mechanism |
|-----------|---------------------------|------------------------|----------------------------|
| `subagent_blocking.ps1` | ❌ No | ❌ No | Inline result (blocking) |
| `subagent_async.ps1` | ❌ No | ❌ No | **`read_file` on `task_file`** |
| `subagent_status_list.ps1` | ✅ Yes | ✅ Yes | Dedicated test for these tools |

**Key finding:** The `subagent_async.ps1` e2e test instructs the agent to poll the **`task_file` directly via `read_file`** — not via `agent_spawn_status`. The `task_file` is the **de facto** universal monitoring mechanism.

From `subagent_async.ps1` (line 123):
> *"Check the task_file path from the async receipt... Read the task_file using read_file or shell to see if the subagent task is complete."*

---

## Current `agent_spawn_status` Implementation

The tool searches `SubagentMetadata`-only registries:

```rust
// src/tools/agent_spawn.rs:511-523
async fn lookup_run(&self, run_id: &str) -> Option<SubagentRunView> {
    match &self.registry {
        Some(registry) => {
            let reg = registry.read().await;
            reg.get(&run_id).and_then(SubagentRunView::from_entry)  // Only SubagentMetadata
        }
        None => {
            let entry = find_run_across_all_registries(&run_id).await?;  // Only subagent runs
            SubagentRunView::from_entry(&entry)
        }
    }
}
```

`SubagentRunView::from_entry()` returns `None` for any task whose `metadata` is not `TaskMetadata::Subagent(...)`. So a `shell` async task is invisible to this tool.

---

## Problem Statement

1. **Framework inconsistency:** The async framework is universal, but status tools are subagent-specific.
2. **Redundancy with `task_file`:** The `task_file` on disk already provides generic status polling for ALL async tasks. `agent_spawn_status` is a narrower, duplicate mechanism.
3. **LLM confusion:** The LLM sees `agent_spawn_status` in its tool list and may try to use it for non-subagent async tasks, which will fail silently ("Run not found").
4. **Tool sprawl:** If we followed the current pattern, we'd need `shell_status`, `grep_status`, `a2a_send_status`, etc. — one per tool.

---

## Resolution: Option A — Generalize to `task_status` / `task_list`

Replace `agent_spawn_status` and `agent_spawn_list` with universal task management tools. No backward compatibility layer — we are at dev stage.

### Design Principles

- **SRP:** The registry owns data + generic projections. Tools own LLM-facing execution. Subagent types own subagent-specific domain logic.
- **DRY:** Reuse the existing `AsyncTaskRegistry` which already stores all task types. No new storage layer.
- **Future-proof:** `TaskMetadata` remains an opaque extension enum. New metadata variants (e.g., `ShellCommand`, `FileWatcher`) require zero changes to `task_status`/`task_list`.
- **Zero tech debt:** Delete the old tools entirely rather than maintaining aliases or deprecation shims.

---

## Implementation Plan

### 1. Add `TaskView` to `src/tools/async_executor/registry.rs`

A generic, serializable projection from `AsyncTaskEntry` that works for ALL task types. Lives alongside the data model for cohesion.

```rust
/// A generic, serializable view of any async task entry.
///
/// This is NOT stored — it is constructed on demand from the unified
/// registry's `AsyncTaskEntry`. It works for ALL task types regardless
/// of `TaskMetadata` variant.
#[derive(Debug, Clone, Serialize)]
pub struct TaskView {
    pub task_id: String,
    pub tool_name: String,
    pub status: AsyncTaskStatus,
    pub parent_session_key: String,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub result: Option<Value>,
    pub label: Option<String>,
    pub metadata_type: String, // "none", "subagent", etc.
}

impl TaskView {
    /// Project an `AsyncTaskEntry` into a universal `TaskView`.
    #[must_use]
    pub fn from_entry(entry: &AsyncTaskEntry) -> Self {
        let metadata_type = match &entry.metadata {
            TaskMetadata::None => "none",
            TaskMetadata::Subagent(_) => "subagent",
        };

        Self {
            task_id: entry.task_id.clone(),
            tool_name: entry.tool_name.clone(),
            status: entry.status.clone(),
            parent_session_key: entry.parent_session_key.clone(),
            created_at: entry.created_at,
            completed_at: entry.completed_at,
            result: entry.result.clone(),
            label: entry.config.label.clone(),
            metadata_type: metadata_type.to_string(),
        }
    }

    /// Get duration of the task
    #[must_use]
    pub fn duration(&self) -> Option<chrono::Duration> {
        let end = self.completed_at.unwrap_or_else(Utc::now);
        Some(end.signed_duration_since(self.created_at))
    }

    /// Check if status is terminal
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        self.status.is_terminal()
    }
}
```

### 2. Create `src/tools/task_management.rs`

New module containing `TaskStatusTool` and `TaskListTool`.

```rust
//! Universal Task Management Tools
//!
//! Provides `task_status` and `task_list` — generic tools for querying
//! ANY async task regardless of tool type. These replace the subagent-specific
//! `agent_spawn_status` and `agent_spawn_list` tools.

use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

use crate::tools::async_executor::{
    find_task_across_all_registries, list_all_tasks_across_all_registries,
    SharedAsyncTaskRegistry, TaskView,
};
use crate::tools::Tool;

/// Tool for checking the status of any async task by task_id.
pub struct TaskStatusTool {
    registry: Option<SharedAsyncTaskRegistry>,
}

impl TaskStatusTool {
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
}

#[async_trait]
impl Tool for TaskStatusTool {
    fn name(&self) -> &'static str {
        "task_status"
    }

    fn description(&self) -> String {
        r"Check the status of any async task by its task_id.

Works for ALL async tasks: shell, grep, agent_spawn, a2a_send, etc.

Parameters:
- task_id: The task ID from the async receipt (required)

Returns the current status, result (if complete), timing, and tool name."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "The task ID from the async receipt (e.g., 'shell:abc-123', 'agent_spawn:xyz-789')"
                }
            },
            "required": ["task_id"]
        })
    }

    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let task_id = params
            .get("task_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required 'task_id' parameter"))?;

        match self.lookup_task(task_id).await {
            Some(task) => {
                let mut response = json!({
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
                    response["completed_at"] = json!(completed_at.to_rfc3339());
                }

                if let Some(result) = task.result {
                    response["result"] = result;
                }

                if let Some(duration) = task.duration() {
                    response["duration_seconds"] = json!(duration.num_seconds());
                }

                Ok(response)
            }
            None => Ok(json!({
                "error": "Task not found",
                "task_id": task_id
            })),
        }
    }
}

/// Tool for listing async tasks.
pub struct TaskListTool {
    registry: Option<SharedAsyncTaskRegistry>,
}

impl TaskListTool {
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
}

#[async_trait]
impl Tool for TaskListTool {
    fn name(&self) -> &'static str {
        "task_list"
    }

    fn description(&self) -> String {
        r"List async tasks for the current session or across all sessions.

Parameters:
- status_filter: Filter by status — 'pending', 'running', 'completed', 'failed', 'cancelled', 'timed_out' (optional)
- tool_filter: Filter by tool name — 'shell', 'agent_spawn', etc. (optional)

Returns a list of tasks with their status, tool name, and timing.".to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "status_filter": {
                    "type": "string",
                    "description": "Filter by status: pending, running, completed, failed, cancelled, timed_out"
                },
                "tool_filter": {
                    "type": "string",
                    "description": "Filter by tool name: shell, agent_spawn, a2a_send, etc."
                }
            }
        })
    }

    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let status_filter = params.get("status_filter").and_then(|v| v.as_str());
        let tool_filter = params.get("tool_filter").and_then(|v| v.as_str());

        let tasks = self.list_tasks(status_filter, tool_filter).await;

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

        let active_count = task_jsons
            .iter()
            .filter(|t| {
                let s = t["status"].as_str().unwrap_or("");
                s != "completed" && s != "failed" && s != "cancelled" && s != "timed_out"
            })
            .count();

        Ok(json!({
            "total": task_jsons.len(),
            "active": active_count,
            "tasks": task_jsons
        }))
    }
}
```

### 3. Modify `src/tools/agent_spawn.rs`

**Remove:**
- `AgentSpawnStatusTool` struct and its `Tool` impl
- `AgentSpawnListTool` struct and its `Tool` impl
- Imports: `find_run_across_all_registries`, `list_all_runs_across_all_registries`, `SharedAsyncTaskRegistry`, `TaskMetadata` (keep only what's needed for spawn logic)

**Keep:**
- `AgentSpawnTool` and all spawn-related logic
- `SessionKeyProvider`, `StaticSessionKeyProvider`, `DynamicSessionKeyProvider`
- `AgentSpawnArgs`
- All spawn tests

**Update `AgentSpawnTool::execute_spawn_async`:**
Change the note in the receipt response from:
```
"note": "Subagent is running in the background. Use agent_spawn_status with the runId to check progress."
```
to:
```
"note": "Subagent is running in the background. Use task_status with the runId to check progress."
```

### 4. Modify `src/tools/mod.rs`

Add:
```rust
pub mod task_management;
pub use task_management::{TaskListTool, TaskStatusTool};
```

Remove from re-exports:
```rust
pub use agent_spawn::{AgentSpawnListTool, AgentSpawnStatusTool, AgentSpawnTool};
```
→ keep only:
```rust
pub use agent_spawn::AgentSpawnTool;
```

### 5. Modify `src/tools/builtin_registry.rs`

**Replace** the subagent-specific registration block with the generic task tools:

```rust
// Before:
use crate::tools::{
    AgentSpawnListTool, AgentSpawnStatusTool, CronTool, ...
};

// After:
use crate::tools::{
    CronTool, TaskListTool, TaskStatusTool, ...
};
```

**Replace** `enable_subagent_tools` config field:
```rust
// Before:
pub enable_subagent_tools: bool,

// After:
pub enable_task_management: bool,
```

Update `Default` impl:
```rust
enable_task_management: true,  // was: enable_subagent_tools: true
```

**Replace** registration block:
```rust
// Before:
if config.enable_subagent_tools {
    if !disabled_set.contains("agent_spawn_status") {
        let tool = Arc::new(AgentSpawnStatusTool::global());
        BuiltinToolAdapter::register_tool(core, tool).await?;
    }
    if !disabled_set.contains("agent_spawn_list") {
        let tool = Arc::new(AgentSpawnListTool::global());
        BuiltinToolAdapter::register_tool(core, tool).await?;
    }
}

// After:
if config.enable_task_management {
    if !disabled_set.contains("task_status") {
        let tool = Arc::new(TaskStatusTool::global());
        BuiltinToolAdapter::register_tool(core, tool).await?;
    }
    if !disabled_set.contains("task_list") {
        let tool = Arc::new(TaskListTool::global());
        BuiltinToolAdapter::register_tool(core, tool).await?;
    }
}
```

**Update** `all_tool_names()`:
```rust
// Before:
vec![..., "agent_spawn_status", "agent_spawn_list", "a2a_send"]

// After:
vec![..., "task_status", "task_list", "a2a_send"]
```

**Update** `is_agent_specific_builtin()`:
```rust
// Before:
matches!(name, "a2a_send" | "sessions_send" | "agent_spawn" | "agent_spawn_status" | "agent_spawn_list" | "session_status")

// After:
matches!(name, "a2a_send" | "sessions_send" | "agent_spawn" | "task_status" | "task_list" | "session_status")
```

### 6. Modify `src/types/agent.rs`

Update `ExtensionConfig::default()` whitelist:
```rust
// Before:
enabled: vec![
    ...,
    "agent_spawn".to_string(),
    "agent_spawn_status".to_string(),
    "agent_spawn_list".to_string(),
],

// After:
enabled: vec![
    ...,
    "agent_spawn".to_string(),
    "task_status".to_string(),
    "task_list".to_string(),
],
```

Update tests that reference `agent_spawn_status`/`agent_spawn_list` in the whitelist.

### 7. Modify E2E Tests

#### `e2e_tests/subagent/subagent_status_list.ps1`
Replace all references to `agent_spawn_status` → `task_status`, `agent_spawn_list` → `task_list`.

Update parameter names:
- `agent_spawn_status` tool: `run_id` → `task_id`
- `agent_spawn_list` tool: no params change (still no required params)

Update response key references in prompts:
- `run_id` in responses → `task_id`
- `runs` in responses → `tasks`

Update test assertion strings as needed.

#### `e2e_tests/subagent/subagent_async.ps1`
Optionally update prompts to mention `task_status` as an alternative to `read_file task_file` polling. Keep `task_file` polling as the primary mechanism since it tests the underlying contract.

#### `e2e_tests/extensions/tools/tool_async.ps1`
No changes required — this test uses `task_file` polling and doesn't reference subagent-specific tools.

### 8. File Changes Summary

| File | Action | Description |
|------|--------|-------------|
| `src/tools/async_executor/registry.rs` | **Modify** | Add `TaskView` struct with `from_entry`, `duration`, `is_terminal` |
| `src/tools/task_management.rs` | **Create** | New `TaskStatusTool` and `TaskListTool` |
| `src/tools/agent_spawn.rs` | **Modify** | Remove `AgentSpawnStatusTool`/`AgentSpawnListTool`; update receipt note |
| `src/tools/mod.rs` | **Modify** | Add `task_management` module; update re-exports |
| `src/tools/builtin_registry.rs` | **Modify** | Register `task_status`/`task_list`; replace `enable_subagent_tools` with `enable_task_management` |
| `src/types/agent.rs` | **Modify** | Update `ExtensionConfig::default()` whitelist |
| `e2e_tests/subagent/subagent_status_list.ps1` | **Modify** | Use `task_status`/`task_list` with `task_id` param |
| `e2e_tests/subagent/subagent_async.ps1` | **Modify** | Optionally mention `task_status` in prompts |

---

## Acceptance Criteria

- [x] Design finalized — `task_status`/`task_list` replace `agent_spawn_status`/`agent_spawn_list`
- [x] `TaskView` generic projection added to `AsyncTaskRegistry`
- [x] `task_status` tool can query ANY async task by `task_id`
- [x] `task_list` tool can list ALL async tasks with optional filters
- [x] Both tools return universal format with `tool_name`, `status`, `result`, timing
- [x] `agent_spawn_status` and `agent_spawn_list` code **removed** (not deprecated)
- [x] `BuiltinToolRegistrar` registers `task_status`/`task_list`
- [x] `BuiltinToolRegistrarConfig.enable_subagent_tools` renamed to `enable_task_management`
- [x] `ExtensionConfig::default()` updated
- [x] E2E tests updated
- [x] All existing tests pass (902 passed, 0 failed, 19 ignored)
- [x] `agent_spawn` receipt note updated to reference `task_status`

## Implementation Notes

- **Closed:** 2026-04-29
- **Commit:** All changes committed in a single changeset
- **Verification:** `cargo check` clean, `cargo test --lib` 902/902 passed

---

## Why Not Option B or C?

- **Option B** (deprecate in favor of `task_file` + `read_file`): The `task_file` is the source of truth and works universally, but raw file parsing is poor LLM UX. Structured tool responses are more ergonomic and allow filtering/listing.
- **Option C** (hybrid): Adds surface area without benefit. The registry already mirrors task state; `task_status` is just a structured query over it. `task_file` remains the cross-process/daemon fallback automatically.

---

## Related

- Issue 006: Three Async Tool Frameworks (closed — consolidated into `AsyncExecutor`)
- Issue 008: Dual Registry System for Subagents (closed — unified into `AsyncTaskRegistry`)
- `src/tools/agent_spawn.rs`
- `src/tools/async_executor/registry.rs`
- `src/tools/async_executor/types.rs`
- `src/extensions/services/async_router.rs`
- `e2e_tests/subagent/`
