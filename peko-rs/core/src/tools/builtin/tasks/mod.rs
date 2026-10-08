//! Task domain actions and runtime contracts.

pub mod common;
mod create;
mod get;
mod list;
mod update;

pub use common::{missing_session_error, parse_status_param, require_session_id};
pub(crate) use create::TaskCreateAction;
pub(crate) use get::TaskGetAction;
pub(crate) use list::TaskListAction;
pub(crate) use update::TaskUpdateAction;

// ─── DTOs (canonical home; root re-exports these) ─────────────────

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use std::sync::Arc;

/// Status of a planning todo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    /// Todo has not been started.
    Pending,
    /// Todo is actively being worked on.
    InProgress,
    /// Todo is finished.
    Completed,
}

impl TodoStatus {
    /// Return the canonical string representation.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            TodoStatus::Pending => "pending",
            TodoStatus::InProgress => "in_progress",
            TodoStatus::Completed => "completed",
        }
    }
}

impl std::fmt::Display for TodoStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for TodoStatus {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "pending" => Ok(TodoStatus::Pending),
            "in_progress" => Ok(TodoStatus::InProgress),
            "completed" => Ok(TodoStatus::Completed),
            _ => Err(anyhow::anyhow!("Unknown todo status: {s}")),
        }
    }
}

/// A planning todo record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Todo {
    /// Unique todo identifier (e.g., `todo:abc123`).
    #[serde(rename = "taskId")]
    pub task_id: String,
    /// Short imperative title.
    pub subject: String,
    /// Optional longer description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Optional present-continuous form shown in UI spinners.
    #[serde(rename = "activeForm", skip_serializing_if = "Option::is_none")]
    pub active_form: Option<String>,
    /// Current status.
    pub status: TodoStatus,
    /// Optional owner/agent name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// Creation timestamp.
    pub created_at: DateTime<Utc>,
    /// Last mutation timestamp.
    pub updated_at: DateTime<Utc>,
}

// ─── TodoRuntime port trait ────────────────────────────────────────

/// Runtime port the Task\* tools use to talk to session-scoped todo
/// storage.
///
/// The production wiring implements this with `TodoStorageRuntime`
/// (root's `src/session/todo_runtime_impl.rs`) which wraps
/// `Arc<TodoStorage>`. Tests construct a `TestTodoRuntime` fixture
/// (in this module under `#[cfg(test)]`) that mimics the storage
/// semantics with an in-memory map.
///
/// The trait is per-process: each agent/daemon constructs one runtime
/// backed by its session directory and shares it across the Task actions.
#[async_trait]
pub trait TodoRuntime: Send + Sync {
    /// Create a new todo in `session_key`. Returns the created record
    /// (with assigned `task_id`).
    async fn create_todo(
        &self,
        session_key: &str,
        subject: String,
        description: Option<String>,
        active_form: Option<String>,
    ) -> Result<Todo>;

    /// Fetch a single todo by id. Returns `None` when no todo with that
    /// id exists in `session_key`.
    async fn get_todo(&self, session_key: &str, task_id: &str) -> Result<Option<Todo>>;

    /// List todos in `session_key`, optionally filtered by status.
    async fn list_todos(
        &self,
        session_key: &str,
        status_filter: Option<TodoStatus>,
    ) -> Result<Vec<Todo>>;

    /// Update a todo's status and/or owner. Returns the updated record
    /// (with refreshed `updated_at`), or `None` when no todo with that
    /// id exists.
    async fn update_todo(
        &self,
        session_key: &str,
        task_id: &str,
        status: Option<TodoStatus>,
        owner: Option<String>,
    ) -> Result<Option<Todo>>;
}

/// Type alias for the shared runtime handle threaded through every
/// `Task*Tool` constructor.
pub type SharedTodoRuntime = Arc<dyn TodoRuntime>;

// ─── Test fixture ──────────────────────────────────────────────────

/// In-memory [`TodoRuntime`] for tests. Mirrors the production
/// `TodoStorage` semantics (auto-assigned `task_id`, monotonic
/// `updated_at`) but does not touch disk.
///
/// Used by the shim test modules in `src/tools/builtin/{task_create,
/// task_get, task_list, task_update}.rs` and exposed as a `pub use`
/// for those tests to consume. The fixture is gated under
/// `#[cfg(test)]` so production builds never link it.
#[cfg(test)]
pub struct TestTodoRuntime {
    /// Session key → list of todos. Synchronous interior mutability so
    /// the trait methods stay `&self`.
    sessions: std::sync::Mutex<std::collections::HashMap<String, Vec<Todo>>>,
}

#[cfg(test)]
impl TestTodoRuntime {
    /// Build an empty in-memory todo runtime.
    #[must_use]
    pub fn new() -> Self {
        Self {
            sessions: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }
}

#[cfg(test)]
impl Default for TestTodoRuntime {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[async_trait]
impl TodoRuntime for TestTodoRuntime {
    async fn create_todo(
        &self,
        session_key: &str,
        subject: String,
        description: Option<String>,
        active_form: Option<String>,
    ) -> Result<Todo> {
        let now = Utc::now();
        let todo = Todo {
            task_id: format!("todo:{}", uuid::Uuid::new_v4().simple()),
            subject,
            description,
            active_form,
            status: TodoStatus::Pending,
            owner: None,
            created_at: now,
            updated_at: now,
        };
        let mut sessions = self
            .sessions
            .lock()
            .expect("TestTodoRuntime mutex poisoned");
        sessions
            .entry(session_key.to_string())
            .or_default()
            .push(todo.clone());
        Ok(todo)
    }

    async fn get_todo(&self, session_key: &str, task_id: &str) -> Result<Option<Todo>> {
        let sessions = self
            .sessions
            .lock()
            .expect("TestTodoRuntime mutex poisoned");
        Ok(sessions
            .get(session_key)
            .and_then(|todos| todos.iter().find(|t| t.task_id == task_id).cloned()))
    }

    async fn list_todos(
        &self,
        session_key: &str,
        status_filter: Option<TodoStatus>,
    ) -> Result<Vec<Todo>> {
        let sessions = self
            .sessions
            .lock()
            .expect("TestTodoRuntime mutex poisoned");
        let todos = sessions.get(session_key).cloned().unwrap_or_default();
        Ok(match status_filter {
            Some(status) => todos.into_iter().filter(|t| t.status == status).collect(),
            None => todos,
        })
    }

    async fn update_todo(
        &self,
        session_key: &str,
        task_id: &str,
        status: Option<TodoStatus>,
        owner: Option<String>,
    ) -> Result<Option<Todo>> {
        let mut sessions = self
            .sessions
            .lock()
            .expect("TestTodoRuntime mutex poisoned");
        let Some(todos) = sessions.get_mut(session_key) else {
            return Ok(None);
        };
        for todo in todos.iter_mut() {
            if todo.task_id == task_id {
                if let Some(s) = status {
                    todo.status = s;
                }
                if owner.is_some() {
                    todo.owner = owner.clone();
                }
                todo.updated_at = Utc::now();
                return Ok(Some(todo.clone()));
            }
        }
        Ok(None)
    }
}

// ─── JSON-roundtrip pin ────────────────────────────────────────────

#[cfg(test)]
mod tests {
    //! Pin the JSON wire shape against the root-side mirror.
    //!
    //! Root's `src/session/todos.rs` re-exports `Todo` and `TodoStatus`
    //! from this module, so deserializing a value through both paths
    //! and asserting equality proves the wire shapes still match.
    use super::*;

    #[test]
    fn todo_status_roundtrip() {
        let cases = vec![
            TodoStatus::Pending,
            TodoStatus::InProgress,
            TodoStatus::Completed,
        ];
        for s in cases {
            let json = serde_json::to_string(&s).unwrap();
            let back: TodoStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(s, back);
        }
    }

    #[test]
    fn todo_roundtrip() {
        let todo = Todo {
            task_id: "todo:abc123".into(),
            subject: "Fix bug".into(),
            description: Some("Long description".into()),
            active_form: Some("Fixing bug".into()),
            status: TodoStatus::InProgress,
            owner: Some("claude".into()),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let json = serde_json::to_value(&todo).unwrap();
        assert_eq!(json["taskId"], "todo:abc123");
        assert_eq!(json["activeForm"], "Fixing bug");
        assert_eq!(json["status"], "in_progress");
        let back: Todo = serde_json::from_value(json).unwrap();
        assert_eq!(back.task_id, todo.task_id);
        assert_eq!(back.status, todo.status);
        assert_eq!(back.active_form, todo.active_form);
    }

    #[test]
    fn todo_serialisation_skips_none_fields() {
        let todo = Todo {
            task_id: "todo:abc".into(),
            subject: "x".into(),
            description: None,
            active_form: None,
            status: TodoStatus::Pending,
            owner: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let json = serde_json::to_value(&todo).unwrap();
        let obj = json.as_object().unwrap();
        assert!(!obj.contains_key("description"));
        assert!(!obj.contains_key("activeForm"));
        assert!(!obj.contains_key("owner"));
    }
}

mod tool;
pub use tool::TaskTool;
