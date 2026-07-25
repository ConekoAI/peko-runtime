//! `TodoStorageRuntime` — root-side adapter for the `TodoRuntime` port.
//!
//! Phase 10d lifts `TaskCreate`/`TaskGet`/`TaskList`/`TaskUpdate` into
//! `peko_tools_builtin::tasks`. The tool surface there speaks to a
//! [`peko_tools_builtin::tasks::TodoRuntime`] port trait so the
//! built-in crate can stay free of root-only deps. This file is the
//! production adapter: it wraps the existing
//! [`peko_session::TodoStorage`] so the same JSONL sidecar format,
//! file-lock semantics, and atomic-rename write strategy continue to
//! apply.
//!
//! Phase 7 lifted the actual `TodoStorage` into `peko-session`. The
//! adapter constructs `peko_tools_builtin::tasks::Todo` from the
//! `peko_session::Todo` returned by storage field-by-field; the two
//! structs are structurally identical so this is a direct copy of
//! every field.

use std::sync::Arc;

use async_trait::async_trait;
use peko_session::TodoStorage;
use peko_tools_builtin::tasks::{Todo, TodoRuntime, TodoStatus};

/// Adapter that exposes [`TodoStorage`] through the [`TodoRuntime`]
/// port trait. Clone is cheap: the underlying [`TodoStorage`] is a
/// single `PathBuf` and the methods take `&self`.
#[derive(Clone)]
pub struct TodoStorageRuntime {
    storage: Arc<TodoStorage>,
}

impl TodoStorageRuntime {
    /// Wrap an existing `TodoStorage` in the runtime adapter.
    #[must_use]
    pub fn new(storage: Arc<TodoStorage>) -> Self {
        Self { storage }
    }
}

fn to_port_status(s: peko_session::TodoStatus) -> TodoStatus {
    match s {
        peko_session::TodoStatus::Pending => TodoStatus::Pending,
        peko_session::TodoStatus::InProgress => TodoStatus::InProgress,
        peko_session::TodoStatus::Completed => TodoStatus::Completed,
    }
}

fn to_storage_status(s: TodoStatus) -> peko_session::TodoStatus {
    match s {
        TodoStatus::Pending => peko_session::TodoStatus::Pending,
        TodoStatus::InProgress => peko_session::TodoStatus::InProgress,
        TodoStatus::Completed => peko_session::TodoStatus::Completed,
    }
}

fn to_port_todo(t: peko_session::Todo) -> Todo {
    Todo {
        task_id: t.task_id,
        subject: t.subject,
        description: t.description,
        active_form: t.active_form,
        status: to_port_status(t.status),
        owner: t.owner,
        created_at: t.created_at,
        updated_at: t.updated_at,
    }
}

#[async_trait]
impl TodoRuntime for TodoStorageRuntime {
    async fn create_todo(
        &self,
        session_key: &str,
        subject: String,
        description: Option<String>,
        active_form: Option<String>,
    ) -> anyhow::Result<Todo> {
        let todo = self
            .storage
            .create_todo(session_key, subject, description, active_form)
            .await?;
        Ok(to_port_todo(todo))
    }

    async fn get_todo(&self, session_key: &str, task_id: &str) -> anyhow::Result<Option<Todo>> {
        let todo = self.storage.get_todo(session_key, task_id).await?;
        Ok(todo.map(to_port_todo))
    }

    async fn list_todos(
        &self,
        session_key: &str,
        status_filter: Option<TodoStatus>,
    ) -> anyhow::Result<Vec<Todo>> {
        let filter = status_filter.map(to_storage_status);
        let todos = self.storage.list_todos(session_key, filter).await?;
        Ok(todos.into_iter().map(to_port_todo).collect())
    }

    async fn update_todo(
        &self,
        session_key: &str,
        task_id: &str,
        status: Option<TodoStatus>,
        owner: Option<String>,
    ) -> anyhow::Result<Option<Todo>> {
        let status = status.map(to_storage_status);
        let todo = self
            .storage
            .update_todo(session_key, task_id, status, owner)
            .await?;
        Ok(todo.map(to_port_todo))
    }
}
