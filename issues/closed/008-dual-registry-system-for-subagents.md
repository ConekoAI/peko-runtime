# Issue 008: Dual Registry System for Subagents

**Severity:** CRITICAL  
**Status:** 🔒 **Closed**  
**Labels:** `architecture`, `subagents`, `registry`, `async-tasks`, `refactor`  
**Reported:** 2026-04-27  
**Resolved:** 2026-04-28  
**Closed:** 2026-04-28  

---

## Summary

Two separate registries track the same underlying subagent tasks with different data models:
- `SubagentRegistry` (`src/agent/subagent_registry.rs`) tracks `SubagentRun` structs
- `AsyncTaskRegistry` (`src/tools/async_executor/registry.rs`) tracks `AsyncTaskEntry` structs

`SubagentExecutor` must update both on every state change, creating a risk of desync and requiring duplicate logic for status queries, cancellation, and result retrieval.

> **Note:** `SubagentStatus` is already a type alias to `AsyncTaskStatus` (since a prior partial cleanup), so the status enums are technically unified. The remaining problem is the dual *registry* system and the manual synchronization burden in `SubagentExecutor`.

---

## The Two Registries

### Registry 1: `SubagentRegistry`

**Location:** `src/agent/subagent_registry.rs`  
**Key Type:** `SubagentRun`  
**Responsibilities:**
- Track subagent runs by ID and agent name
- Store spawn parameters, status, result, and timing
- Provide queries: `get_run()`, `list_runs()`, `find_run_across_all_registries()`
- Track parent/child session relationships and spawn depth
- Global per-agent caching via `GLOBAL_SUBAGENT_REGISTRIES`

```rust
pub struct SubagentRun {
    pub run_id: String,
    pub child_session_key: String,
    pub parent_session_key: String,
    pub task: String,
    pub status: SubagentStatus,        // = AsyncTaskStatus (already unified)
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub cleanup: SpawnCleanupPolicy,
    pub label: Option<String>,
    pub result: Option<SubagentResult>,
    pub depth: u32,
    pub announce_completion: bool,
}
```

---

### Registry 2: `AsyncTaskRegistry`

**Location:** `src/tools/async_executor/registry.rs`  
**Key Type:** `AsyncTaskEntry`  
**Responsibilities:**
- Track async tasks by ID
- Store task metadata, status, result, and delivery target
- Provide queries: `get_task()`, `wait_for_completion()`, `cancel_task()`
- Manage result delivery (queue, channel, callback)
- Write task files for disk-based polling
- Completion notification channels for sync waiting

```rust
pub struct AsyncTaskEntry {
    pub task_id: AsyncTaskId,
    pub tool_name: String,
    pub params: Value,
    pub status: AsyncTaskStatus,
    pub result: Option<Value>,
    pub parent_session_key: String,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub config: AsyncToolConfig,
    pub formatted_result: Option<String>,
    completion_tx: Option<mpsc::Sender<AsyncTaskStatus>>,
}
```

---

## Evidence of Duplication

### `SubagentExecutor` Updates Both

```rust
// src/agent/subagent_executor.rs (actual)
async fn spawn_and_execute(...) {
    // 1. Register in SubagentRegistry
    let mut registry = self.registry.write().await;
    registry.register(run);
    // ...
    
    // 2. Execute via AsyncExecutor (registers in AsyncTaskRegistry internally)
    self.unified_executor.execute(run_id.clone(), ...).await?;
    
    // 3. Inside the execution closure, manually update SubagentRegistry AGAIN
    //    after the AsyncExecutor has already updated AsyncTaskRegistry
    let mut registry = registry_clone.write().await;
    registry.complete(&run_id_clone, subagent_result);
}

// Cancellation also updates both:
pub async fn cancel(&self, run_id: &str) -> Result<()> {
    self.unified_executor.cancel(&run_id.to_string()).await.ok();  // AsyncTaskRegistry
    let mut registry = self.registry.write().await;
    registry.update_status(run_id, SubagentStatus::Cancelled);      // SubagentRegistry
    Ok(())
}
```

### Double Memory Usage

Every subagent run stores:
- `SubagentRun` in `SubagentRegistry` (~200 bytes + strings)
- `AsyncTaskEntry` in `AsyncTaskRegistry` (~150 bytes + strings)
- Overlapping fields: `run_id`/`task_id`, `status`, `result`, `parent_session_key`, timestamps

### Query Inconsistency Risk

`agent_spawn_status` queries `SubagentRegistry`. The async framework's delivery mechanism queries `AsyncTaskRegistry`. If one update fails (panic, lock timeout), they diverge.

---

## Root Cause

- `SubagentRegistry` was built for the subagent feature (spawned agents with session isolation).
- `AsyncTaskRegistry` was built for the general async tool framework (background shell commands, etc.).
- Subagents were later implemented as a special case of async tools, but the registries were never merged.
- The global cache for `SubagentRegistry` exists because `SubagentExecutor` is recreated per request in the stateless architecture. `AsyncTaskRegistry` is owned by the executor and is lost on executor drop.

---

## ✅ Resolution: Unified Registry with Domain Extensions (Implemented)

**Chosen approach: Evolved Option A** — Merged into `AsyncTaskRegistry` with a clean extension model.

### Design Principles

| Principle | How it's satisfied |
|-----------|-------------------|
| **Single Source of Truth** | One `AsyncTaskRegistry` holds all async task state |
| **SRP** | Registry owns lifecycle; `SubagentMetadata` is pure data; `SubagentExecutor` orchestrates |
| **DRY** | One registry, one status enum (already done), one set of query/cancel/wait methods |
| **Open/Closed** | New task types add metadata variants without touching registry logic |
| **Future-proof** | Global cache, delivery infrastructure, and task files are framework-level concerns |

---

### Phase 1: Extend `AsyncTaskEntry` with Optional Domain Metadata

Add an `extensions` field to `AsyncTaskEntry` that carries domain-specific data without polluting the generic task model:

```rust
// In src/tools/async_executor/registry.rs

/// Domain-specific metadata extensions for async task entries.
/// 
/// This enum keeps the generic `AsyncTaskEntry` clean while allowing
/// domain modules (subagents, shell commands, etc.) to attach their
/// own structured data. The registry ignores this field — it is
/// owned by the domain module that creates the task.
#[derive(Debug, Clone)]
pub enum TaskMetadata {
    /// No additional metadata (generic async tool)
    None,
    /// Subagent-specific metadata
    Subagent(SubagentMetadata),
    // Future variants: ShellCommand, FileWatcher, etc.
}

impl Default for TaskMetadata {
    fn default() -> Self { Self::None }
}

/// Subagent-specific metadata attached to an `AsyncTaskEntry`.
/// 
/// This replaces the entire `SubagentRun` struct. All fields that
/// were previously in `SubagentRun` but not in `AsyncTaskEntry`
/// live here.
#[derive(Debug, Clone)]
pub struct SubagentMetadata {
    pub child_session_key: String,
    pub cleanup: crate::session::types::SpawnCleanupPolicy,
    pub depth: u32,
    pub announce_completion: bool,
    /// The subagent result (output, error, token_usage) —
    /// distinct from the generic `AsyncTaskEntry.result` which is
    /// the raw JSON returned by the execution closure.
    pub subagent_result: Option<SubagentResult>,
}
```

Update `AsyncTaskEntry`:

```rust
pub struct AsyncTaskEntry {
    pub task_id: AsyncTaskId,
    pub tool_name: String,
    pub params: Value,
    pub status: AsyncTaskStatus,
    pub result: Option<Value>,
    pub parent_session_key: String,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub config: AsyncToolConfig,
    pub formatted_result: Option<String>,
    pub metadata: TaskMetadata,        // <-- NEW
    completion_tx: Option<mpsc::Sender<AsyncTaskStatus>>,
}
```

---

### Phase 2: Make `AsyncTaskRegistry` Global

Replace the per-executor `AsyncTaskRegistry` with a global cache, identical to how `SubagentRegistry` works today:

```rust
// In src/tools/async_executor/registry.rs (or a new global.rs)

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::RwLock;

static GLOBAL_ASYNC_TASK_REGISTRIES: std::sync::OnceLock<
    Mutex<HashMap<String, SharedAsyncTaskRegistry>>
> = std::sync::OnceLock::new();

fn global_registries() -> &'static Mutex<HashMap<String, SharedAsyncTaskRegistry>> {
    GLOBAL_ASYNC_TASK_REGISTRIES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Get or create a shared async task registry for a given agent name.
/// 
/// This ensures that all `Agent` instances for the same agent name share
/// the same registry, making status queries and result delivery work
/// across stateless requests.
pub fn get_or_create_registry_for_agent(agent_name: &str) -> SharedAsyncTaskRegistry {
    let mut map = global_registries().lock().unwrap();
    map.entry(agent_name.to_string())
        .or_insert_with(|| Arc::new(RwLock::new(AsyncTaskRegistry::new())))
        .clone()
}

/// Look up a task by ID across all agent registries.
pub async fn find_task_across_all_registries(task_id: &str) -> Option<AsyncTaskEntry> {
    let registries: Vec<SharedAsyncTaskRegistry> = {
        let map = global_registries().lock().unwrap();
        map.values().cloned().collect()
    };
    for registry in registries {
        let reg = registry.read().await;
        if let Some(entry) = reg.get(task_id) {
            return Some(entry.clone());
        }
    }
    None
}

/// List all tasks across all agent registries.
pub async fn list_all_tasks_across_all_registries() -> Vec<AsyncTaskEntry> {
    let registries: Vec<SharedAsyncTaskRegistry> = {
        let map = global_registries().lock().unwrap();
        map.values().cloned().collect()
    };
    let mut all = Vec::new();
    for registry in registries {
        let reg = registry.read().await;
        for entry in reg.list_tasks(None) {
            all.push(entry);
        }
    }
    all
}
```

---

### Phase 3: Add Subagent-Specific Query Methods to `AsyncTaskRegistry`

The `AsyncTaskRegistry` gains subagent-aware query methods, but they are thin filters over the unified data:

```rust
impl AsyncTaskRegistry {
    // --- Existing methods remain unchanged ---
    // register, get, update_status, wait_for_completion, check_status, ...

    // --- New subagent-specific queries ---

    /// Get all tasks with `TaskMetadata::Subagent` for a parent session.
    pub fn list_subagents_for_parent(&self, parent_session_key: &str) -> Vec<&AsyncTaskEntry> {
        self.tasks.values()
            .filter(|e| e.parent_session_key == parent_session_key)
            .filter(|e| matches!(e.metadata, TaskMetadata::Subagent(_)))
            .collect()
    }

    /// Count active (non-terminal) subagents for a parent session.
    pub fn count_active_subagents_for_parent(&self, parent_session_key: &str) -> usize {
        self.tasks.values()
            .filter(|e| e.parent_session_key == parent_session_key)
            .filter(|e| matches!(e.metadata, TaskMetadata::Subagent(_)))
            .filter(|e| !e.status.is_terminal())
            .count()
    }

    /// Get the spawn depth of a session by looking up where it was a child.
    pub fn get_subagent_depth_for_session(&self, session_key: &str) -> u32 {
        self.tasks.values()
            .filter_map(|e| match &e.metadata {
                TaskMetadata::Subagent(m) if m.child_session_key == session_key => Some(m.depth),
                _ => None,
            })
            .next()
            .unwrap_or(0)
    }

    /// Get subagent-specific result data (if any).
    pub fn get_subagent_result(&self, task_id: &AsyncTaskId) -> Option<SubagentResult> {
        self.tasks.get(task_id).and_then(|e| match &e.metadata {
            TaskMetadata::Subagent(m) => m.subagent_result.clone(),
            _ => None,
        })
    }
}
```

---

### Phase 4: Rewrite `SubagentExecutor` to Use Only the Unified Registry

`SubagentExecutor` drops its `registry: SharedSubagentRegistry` field entirely. All state operations go through `self.unified_executor.registry()` (the global `SharedAsyncTaskRegistry`).

```rust
#[derive(Clone)]
pub struct SubagentExecutor {
    // REMOVED: registry: SharedSubagentRegistry
    unified_executor: AsyncExecutor,
    agent_name: String,
    max_concurrent: usize,
    announcement_tx: Option<AnnouncementSender>,
    provider: Option<Arc<crate::providers::Provider>>,
    agent_config: Option<AgentConfig>,
    session_manager: Arc<RwLock<SessionManager>>,
}

impl SubagentExecutor {
    pub fn new(
        session_manager: Arc<RwLock<SessionManager>>,
        agent_name: impl Into<String>,
        max_concurrent: usize,
    ) -> Self {
        let agent_name = agent_name.into();
        // Use the GLOBAL async task registry
        let async_registry = crate::tools::async_executor::get_or_create_registry_for_agent(&agent_name);
        let async_queue_manager = Arc::new(RwLock::new(AsyncResultQueueManager::new()));
        let unified_executor = AsyncExecutor::with_registries(async_registry, async_queue_manager);

        Self {
            unified_executor,
            agent_name,
            max_concurrent,
            announcement_tx: None,
            provider: None,
            agent_config: None,
            session_manager,
        }
    }

    pub async fn spawn_and_execute(&self, ...) -> Result<String> {
        // ... depth checks, concurrent checks, session creation ...

        // Build the metadata extension
        let metadata = TaskMetadata::Subagent(SubagentMetadata {
            child_session_key: child_session_key.clone(),
            cleanup: config.cleanup,
            depth: child_depth,
            announce_completion: config.announce_completion,
            subagent_result: None,
        });

        // Execute via unified executor — this is the ONLY registration point
        self.unified_executor
            .execute_with_metadata(
                run_id.clone(),
                "agent_spawn",
                params,
                parent_session_key.to_string(),
                async_config,
                metadata,                    // <-- attached once, never synced
                execution_closure,
            )
            .await?;

        Ok(run_id)
    }

    pub async fn cancel(&self, run_id: &str) -> Result<()> {
        // Single registry update — no dual sync
        self.unified_executor.cancel(&run_id.to_string()).await
    }

    pub async fn get_run(&self, run_id: &str) -> Option<SubagentRunView> {
        // Read from unified registry, project into a view type for backward compat
        let registry = self.unified_executor.registry().read().await;
        registry.get(run_id).map(SubagentRunView::from_entry)
    }

    pub async fn get_run_status(&self, run_id: &str) -> Option<AsyncTaskStatus> {
        let registry = self.unified_executor.registry().read().await;
        registry.check_status(run_id)
    }

    pub async fn count_active_runs(&self) -> usize {
        let registry = self.unified_executor.registry().read().await;
        registry.count_active_subagents_for_parent(&self.agent_name)
            // or list all and filter by tool_name == "agent_spawn"
    }

    pub async fn shutdown(&self) {
        let mut registry = self.unified_executor.registry().write().await;
        let active: Vec<String> = registry.list_tasks(None)
            .into_iter()
            .filter(|e| e.tool_name == "agent_spawn" && !e.status.is_terminal())
            .map(|e| e.task_id.clone())
            .collect();
        for run_id in active {
            registry.update_status(&run_id, AsyncTaskStatus::Cancelled);
        }
    }
}
```

**Key change:** `SubagentExecutor` no longer writes to two registries. It creates the `SubagentMetadata`, attaches it to the task, and lets `AsyncExecutor` handle all lifecycle state. When the closure completes, it updates the metadata's `subagent_result` field inside the same registry write that updates status.

---

### Phase 5: Introduce `SubagentRunView` (Backward Compatibility)

The `agent_spawn_status` and `agent_spawn_list` tools expect `SubagentRun`-shaped data. Instead of keeping `SubagentRun` as a stored type, make it a **read-only view** projected from `AsyncTaskEntry`:

```rust
/// A read-only view of an async task entry, projected into the
/// subagent domain model. This replaces `SubagentRun` as the
/// public-facing type for subagent queries.
/// 
/// This is NOT stored anywhere — it is constructed on demand from
/// the unified registry's `AsyncTaskEntry`.
#[derive(Debug, Clone)]
pub struct SubagentRunView {
    pub run_id: String,
    pub child_session_key: String,
    pub parent_session_key: String,
    pub task: String,
    pub status: AsyncTaskStatus,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub cleanup: SpawnCleanupPolicy,
    pub label: Option<String>,
    pub result: Option<SubagentResult>,
    pub depth: u32,
    pub announce_completion: bool,
}

impl SubagentRunView {
    pub fn from_entry(entry: &AsyncTaskEntry) -> Option<Self> {
        let meta = match &entry.metadata {
            TaskMetadata::Subagent(m) => m,
            _ => return None,
        };
        Some(Self {
            run_id: entry.task_id.clone(),
            child_session_key: meta.child_session_key.clone(),
            parent_session_key: entry.parent_session_key.clone(),
            task: entry.params.get("task")?.as_str()?.to_string(),
            status: entry.status.clone(),
            started_at: entry.created_at,
            completed_at: entry.completed_at,
            cleanup: meta.cleanup,
            label: entry.config.label.clone(),
            result: meta.subagent_result.clone(),
            depth: meta.depth,
            announce_completion: meta.announce_completion,
        })
    }
}
```

Update `AgentSpawnStatusTool` and `AgentSpawnListTool` to query the unified registry and project views:

```rust
impl AgentSpawnStatusTool {
    async fn lookup_run(&self, run_id: &str) -> Option<SubagentRunView> {
        match &self.registry {
            Some(registry) => {
                let reg = registry.read().await;
                reg.get(run_id).and_then(SubagentRunView::from_entry)
            }
            None => {
                let entry = crate::tools::async_executor::find_task_across_all_registries(run_id).await?;
                SubagentRunView::from_entry(&entry)
            }
        }
    }
}
```

---

### Phase 6: Delete `SubagentRegistry`

After all callers are migrated:

1. **Delete** `src/agent/subagent_registry.rs`
2. **Remove** `pub mod subagent_registry;` from `src/agent/mod.rs`
3. **Update** all `use crate::agent::subagent_registry::{...}` imports to use the new types from `crate::tools::async_executor`
4. **Move** `SubagentResult` and `SubagentRunView` to `src/agent/subagent_types.rs` (or keep inline in `subagent_executor.rs` if small)

---

### Phase 7: Update `AsyncExecutor.execute` to Accept Metadata

Add an `execute_with_metadata` method (or extend `execute` with an optional parameter):

```rust
impl AsyncExecutor {
    pub async fn execute_with_metadata<F, Fut>(
        &self,
        task_id: AsyncTaskId,
        tool_name: impl Into<String>,
        params: Value,
        parent_session_key: impl Into<String>,
        config: AsyncToolConfig,
        metadata: TaskMetadata,
        execution_fn: F,
    ) -> Result<AsyncTaskReceipt>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<(Value, TaskMetadata)>> + Send + 'static,
    {
        // Same as execute(), but:
        // 1. Attach metadata to the AsyncTaskEntry on registration
        // 2. After execution, write the returned metadata back into the entry
        //    (so the closure can populate SubagentMetadata.subagent_result)
    }
}
```

Alternatively, keep `execute()` unchanged and have the closure mutate the registry directly via a provided `SharedAsyncTaskRegistry` handle. The metadata approach is cleaner because it avoids exposing the registry to the closure.

---

## File Changes Summary

| File | Action | Description |
|------|--------|-------------|
| `src/tools/async_executor/registry.rs` | Modify | Add `TaskMetadata`, `SubagentMetadata`, global cache, subagent query methods |
| `src/tools/async_executor/executor.rs` | Modify | Add `execute_with_metadata` or equivalent |
| `src/tools/async_executor/mod.rs` | Modify | Re-export new types |
| `src/agent/subagent_executor.rs` | Modify | Remove `SharedSubagentRegistry`, use unified registry exclusively |
| `src/agent/subagent_announce.rs` | Modify | Use `SubagentRunView` instead of `SubagentRun` |
| `src/tools/agent_spawn.rs` | Modify | Use unified registry queries, `SubagentRunView` |
| `src/agent/subagent_registry.rs` | **Delete** | Entire file — functionality absorbed into unified registry |
| `src/agent/subagent_types.rs` | **Create** | `SubagentResult`, `SubagentRunView`, `SubagentMetadata` |
| `src/agent/mod.rs` | Modify | Remove `subagent_registry` module, add `subagent_types` |

---

## Acceptance Criteria

- [ ] `SubagentRegistry` is deleted; no file at `src/agent/subagent_registry.rs`.
- [ ] `AsyncTaskRegistry` is the **only** registry tracking async tasks and subagents.
- [ ] `AsyncTaskRegistry` uses a global per-agent cache (survives stateless executor drops).
- [ ] `SubagentStatus` type alias remains (already = `AsyncTaskStatus`) — no code changes needed.
- [ ] `SubagentExecutor` updates **one** registry on spawn, completion, and cancellation.
- [ ] `agent_spawn_status` and `agent_spawn_list` tools work correctly in both bound and global modes.
- [ ] Announcement service (`subagent_announce.rs`) continues to format and deliver results.
- [ ] All existing tests pass.
- [ ] No manual synchronization loops between two registries.

---

## Implementation Summary

| Phase | Status | Description |
|-------|--------|-------------|
| Phase 1 | ✅ Done | Added `TaskMetadata` enum and `SubagentMetadata` to `AsyncTaskEntry` |
| Phase 2 | ✅ Done | Added global per-agent cache to `AsyncTaskRegistry` |
| Phase 3 | ✅ Done | Added subagent-specific query methods to `AsyncTaskRegistry` |
| Phase 4 | ✅ Done | Created `SubagentRunView` read-only projection in `subagent_types.rs` |
| Phase 5 | ✅ Done | Rewrote `SubagentExecutor` to use unified registry exclusively |
| Phase 6 | ✅ Done | Updated `agent_spawn.rs` tools to query unified registry |
| Phase 7 | ✅ Done | Updated `subagent_announce.rs` to use `SubagentRunView` |
| Phase 8 | ✅ Done | Deleted `subagent_registry.rs`, updated `agent/mod.rs` |
| Phase 9 | ✅ Done | All tests pass (60+ unit tests) |

### Key Changes

- **Deleted:** `src/agent/subagent_registry.rs` (445 lines)
- **Created:** `src/agent/subagent_types.rs` (75 lines) — `SubagentRunView`, re-exports `SubagentResult`/`SubagentStatus`
- **Modified:** `src/tools/async_executor/registry.rs` — added `TaskMetadata`, `SubagentMetadata`, global cache, subagent queries
- **Modified:** `src/tools/async_executor/executor.rs` — added `execute_with_metadata()` method
- **Modified:** `src/tools/async_executor/mod.rs` — re-exported new types
- **Modified:** `src/agent/subagent_executor.rs` — removed dual-registry sync, uses unified registry
- **Modified:** `src/agent/subagent_announce.rs` — uses `SubagentRunView`
- **Modified:** `src/tools/agent_spawn.rs` — queries unified registry
- **Modified:** `src/agent/mod.rs` — removed `subagent_registry` module, added `subagent_types`
- **Modified:** `src/agent/tests/subagent_integration_tests.rs` — migrated to unified registry

---

## Acceptance Criteria

- [x] `SubagentRegistry` is deleted; no file at `src/agent/subagent_registry.rs`.
- [x] `AsyncTaskRegistry` is the **only** registry tracking async tasks and subagents.
- [x] `AsyncTaskRegistry` uses a global per-agent cache (survives stateless executor drops).
- [x] `SubagentStatus` type alias remains (re-export of `AsyncTaskStatus`).
- [x] `SubagentExecutor` updates **one** registry on spawn, completion, and cancellation.
- [x] `agent_spawn_status` and `agent_spawn_list` tools work correctly in both bound and global modes.
- [x] Announcement service (`subagent_announce.rs`) continues to format and deliver results.
- [x] All existing tests pass.
- [x] No manual synchronization loops between two registries.

---

## Related

- Issue 006: Three Async Tool Frameworks (closed — consolidated into `AsyncExecutor`)
- Issue 007: Dual Tool Registration Paths
- `src/tools/async_executor/registry.rs`
- `src/tools/async_executor/executor.rs`
- `src/agent/subagent_executor.rs`
- `src/tools/agent_spawn.rs`
