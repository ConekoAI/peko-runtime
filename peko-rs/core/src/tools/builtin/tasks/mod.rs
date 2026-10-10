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

// ─── DTOs ──────────────────────────────────────────────────────────

use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;

/// The todo record and status the tool exchanges are the session store's.
pub use peko_session::{Todo, TodoStatus};

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

/// The production todo storage in a private tempdir that lives as long as
/// the runtime, so Task tests exercise the real JSONL format, ids, and
/// ordering.
#[cfg(test)]
pub struct TestTodoRuntime {
    _dir: tempfile::TempDir,
    inner: crate::session::todo_runtime_impl::TodoStorageRuntime,
}

#[cfg(test)]
impl TestTodoRuntime {
    #[must_use]
    pub fn new() -> Self {
        let dir = tempfile::tempdir().expect("todo tempdir");
        let storage = Arc::new(peko_session::TodoStorage::new(dir.path().join("sessions")));
        Self {
            inner: crate::session::todo_runtime_impl::TodoStorageRuntime::new(storage),
            _dir: dir,
        }
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
        self.inner
            .create_todo(session_key, subject, description, active_form)
            .await
    }
    async fn get_todo(&self, session_key: &str, task_id: &str) -> Result<Option<Todo>> {
        self.inner.get_todo(session_key, task_id).await
    }
    async fn list_todos(
        &self,
        session_key: &str,
        status_filter: Option<TodoStatus>,
    ) -> Result<Vec<Todo>> {
        self.inner.list_todos(session_key, status_filter).await
    }
    async fn update_todo(
        &self,
        session_key: &str,
        task_id: &str,
        status: Option<TodoStatus>,
        owner: Option<String>,
    ) -> Result<Option<Todo>> {
        self.inner
            .update_todo(session_key, task_id, status, owner)
            .await
    }
}

mod tool;
pub use tool::TaskTool;
