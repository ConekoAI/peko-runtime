//! Shared helpers for the Async* family of tools.
//!
//! These helpers encapsulate registry lookup, list filtering, cancellation,
//! response building, and tail-lines truncation so each standalone tool
//! (`AsyncSpawn`, `AsyncOutput`, `AsyncStop`, `AsyncStatus`, `AsyncList`)
//! stays small and focused.

use serde_json::json;

use crate::extensions::framework::async_exec::executor::{
    cancel_task_across_all_registries, find_task_across_all_registries,
    list_all_tasks_across_all_registries, CancelResult, SharedAsyncTaskRegistry, TaskView,
};

/// Helper for registry-backed async task operations.
#[derive(Clone)]
pub struct AsyncTaskHelper {
    registry: Option<SharedAsyncTaskRegistry>,
}

impl AsyncTaskHelper {
    /// Create a helper bound to a specific registry.
    #[must_use]
    pub fn with_registry(registry: SharedAsyncTaskRegistry) -> Self {
        Self {
            registry: Some(registry),
        }
    }

    /// Create a helper that searches across all agent registries.
    #[must_use]
    pub fn global() -> Self {
        Self { registry: None }
    }

    /// Look up a task by ID.
    pub async fn lookup_task(&self, task_id: &str) -> Option<TaskView> {
        match &self.registry {
            Some(registry) => {
                let reg = registry.read().await;
                reg.get(&task_id.to_string()).map(TaskView::from_entry)
            }
            None => {
                let entry = find_task_across_all_registries(task_id).await?;
                Some(TaskView::from_entry(&entry))
            }
        }
    }

    /// List tasks with optional status/tool filters.
    pub async fn list_tasks(
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

    /// Cancel a task by ID.
    pub async fn cancel_task(&self, task_id: &str) -> CancelResult {
        match &self.registry {
            Some(registry) => {
                let mut reg = registry.write().await;
                reg.cancel(&task_id.to_string())
            }
            None => cancel_task_across_all_registries(task_id).await,
        }
    }
}

/// Build a JSON response for a task status query.
#[must_use]
pub fn build_status_response(task: &TaskView) -> serde_json::Value {
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

/// Build a JSON response for a task cancellation.
#[must_use]
pub fn build_cancel_response(result: CancelResult, task_id: &str) -> serde_json::Value {
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

/// Build a JSON response for a task output query.
#[must_use]
pub fn build_output_response(task: &TaskView, tail_lines: u64) -> serde_json::Value {
    let mut base = json!({
        "task_id": task.task_id,
        "status": task.status.as_str(),
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
