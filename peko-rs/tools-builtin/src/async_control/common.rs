//! Shared helpers for the Async* family of tools.
//!
//! These helpers encapsulate response building and tail-lines
//! truncation so each standalone tool (`AsyncSpawn`, `AsyncOutput`,
//! `AsyncStop`, `AsyncStatus`, `AsyncList`) stays small and focused.
//!
//! `AsyncTaskHelper` was previously imported from the framework host
//! (`AsyncTaskRegistry`). The framework host now adapts any
//! `AsyncRuntime` to a simple `runtime()` projection, so the helper
//! here just wraps a shared `Arc<dyn AsyncRuntime>` and exposes the
//! same shape. Legacy registry-bound helpers moved out — the helper
//! is purely runtime-facing now.

use serde_json::json;

use crate::async_control::{CancelResult, SharedAsyncRuntime, TaskView};

/// Helper for async task operations that speak to an `AsyncRuntime` port.
///
/// Where the pre-Phase-10c helper held an `Option<SharedAsyncTaskRegistry>`
/// to support both registry-bound and cross-registry-global modes, the
/// helper now just holds an `AsyncRuntime` — `list_tasks`, `lookup_task`,
/// and `cancel_task` are 1:1 with the trait. The "global" vs "with
/// registry" constructor distinction collapses to "always per-runtime".
#[derive(Clone)]
pub struct AsyncTaskHelper {
    runtime: SharedAsyncRuntime,
}

impl AsyncTaskHelper {
    /// Create a helper bound to a specific async runtime.
    #[must_use]
    pub fn new(runtime: SharedAsyncRuntime) -> Self {
        Self { runtime }
    }

    /// Look up a task by ID.
    pub async fn lookup_task(&self, task_id: &str) -> Option<TaskView> {
        self.runtime.lookup(task_id).await
    }

    /// List tasks with optional status/tool filters.
    pub async fn list_tasks(
        &self,
        status_filter: Option<&str>,
        tool_filter: Option<&str>,
    ) -> Vec<TaskView> {
        self.runtime.list(status_filter, tool_filter).await
    }

    /// Cancel a task by ID.
    pub async fn cancel_task(&self, task_id: &str) -> CancelResult {
        self.runtime.cancel(task_id).await
    }

    /// Borrow the underlying runtime handle.
    ///
    /// `AsyncOutput` calls this for blocking reads. Most callers don't
    /// need to escape the helper — prefer `lookup_task` / `list_tasks` /
    /// `cancel_task` when applicable.
    #[must_use]
    pub fn runtime_handle(&self) -> &SharedAsyncRuntime {
        &self.runtime
    }
}

/// Build a JSON response for a task status query.
#[must_use]
pub fn build_status_response(task: &TaskView) -> serde_json::Value {
    let mut base = json!({
        "task_id": task.task_id,
        "tool_name": task.tool_name,
        "status": task.status,
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

/// Build a JSON response for a task list query.
#[must_use]
pub fn build_list_response(tasks: Vec<TaskView>) -> serde_json::Value {
    let active_count = tasks.iter().filter(|t| !t.is_terminal()).count();

    let task_jsons: Vec<_> = tasks
        .into_iter()
        .map(|t| {
            json!({
                "task_id": t.task_id,
                "tool_name": t.tool_name,
                "status": t.status,
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

/// Build a JSON response for a task cancellation.
///
/// `AsyncStop` uses a three-way result shape:
/// - `Success` — the task was running and is now cancelled
/// - `AlreadyTerminal` — the task was already in a terminal state; the
///   call is *successful* (the task can't be cancelled, but it didn't
///   need to be), and `already_terminal: true` tells the LLM it was a
///   no-op rather than a real cancellation
/// - `NotFound` — no task with that id exists in any registry
///
/// Keeping `success: true` for the already-terminal case is the
/// Claude-Code `TaskStop` shape and avoids the trap where a model
/// sees `success: false` and concludes the cancellation failed
/// when in fact the task was already done.
#[must_use]
pub fn build_cancel_response(result: CancelResult, task_id: &str) -> serde_json::Value {
    match result {
        CancelResult::Success { previous } => json!({
            "success": true,
            "task_id": task_id,
            "previous_status": previous,
            "already_terminal": false,
            "message": "Task cancelled",
        }),
        CancelResult::AlreadyTerminal { previous } => json!({
            "success": true,
            "task_id": task_id,
            "previous_status": previous,
            "already_terminal": true,
            "message": format!("Task already terminal: {previous}"),
        }),
        CancelResult::NotFound => json!({
            "success": false,
            "task_id": task_id,
            "already_terminal": false,
            "message": "Task not found",
        }),
    }
}

/// Build a JSON response for a task output query.
#[must_use]
pub fn build_output_response(task: &TaskView, tail_lines: u64) -> serde_json::Value {
    let mut base = json!({
        "task_id": task.task_id,
        "status": task.status,
        "is_terminal": task.is_terminal(),
    });
    if let Some(ref result) = task.result {
        base["result"] = apply_tail_lines(result, tail_lines);
    }
    if let Some(completed_at) = task.completed_at {
        base["completed_at"] = json!(completed_at.to_rfc3339());
    }
    if let Some(duration) = task.duration() {
        base["elapsed_seconds"] = json!(duration.num_seconds());
    }
    base
}

/// Apply `tail_lines` filtering to a tool result value.
///
/// Recognizes two shapes: a JSON string (truncate lines directly) and a JSON
/// object with a string `stdout` field (truncate that field, leave the rest).
/// Other shapes pass through unchanged.
#[must_use]
pub fn apply_tail_lines(result: &serde_json::Value, tail_lines: u64) -> serde_json::Value {
    if tail_lines == 0 {
        return result.clone();
    }
    let last_n = |s: &str| -> String {
        let mut lines: Vec<&str> = s.lines().collect();
        if lines.len() > tail_lines as usize {
            lines = lines.split_off(lines.len() - tail_lines as usize);
        }
        lines.join("\n")
    };
    if let Some(s) = result.as_str() {
        return serde_json::Value::String(last_n(s));
    }
    if let Some(obj) = result.as_object() {
        if let Some(stdout) = obj.get("stdout").and_then(|v| v.as_str()) {
            let mut new_obj = obj.clone();
            new_obj.insert(
                "stdout".to_string(),
                serde_json::Value::String(last_n(stdout)),
            );
            return serde_json::Value::Object(new_obj);
        }
    }
    result.clone()
}
