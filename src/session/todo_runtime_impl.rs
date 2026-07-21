//! `TodoStorageRuntime` — root-side adapter for the `TodoRuntime` port.
//!
//! Phase 10d lifts `TaskCreate`/`TaskGet`/`TaskList`/`TaskUpdate` into
//! `peko_tools_builtin::tasks`. The tool surface there speaks to a
//! [`peko_tools_builtin::tasks::TodoRuntime`] port trait so the
//! built-in crate can stay free of root-only deps. This file is the
//! production adapter: it wraps the existing [`TodoStorage`] so the
//! same JSONL sidecar format, file-lock semantics, and atomic-rename
//! write strategy continue to apply.
//!
//! The adapter performs a thin structural conversion between
//! `peko_tools_builtin::tasks::{Todo, TodoStatus}` and the local
//! `crate::session::todos::{Todo, TodoStatus}`. The two pairs are
//! shape-identical (same field names, same serde renames); the
//! conversion is `From` impls to keep the adapter call sites terse.

use std::sync::Arc;

use async_trait::async_trait;

use crate::session::todos::{TodoStatus as RootTodoStatus, TodoStorage};
use peko_tools_builtin::tasks::{Todo, TodoRuntime, TodoStatus};

/// `peko_tools_builtin::tasks::TodoStatus` ↔ root's `TodoStatus`.
/// The two enums are structurally identical (`pending` /
/// `in_progress` / `completed`); the From impls just shuffle the
/// variant tag.
impl From<TodoStatus> for RootTodoStatus {
    fn from(s: TodoStatus) -> Self {
        match s {
            TodoStatus::Pending => RootTodoStatus::Pending,
            TodoStatus::InProgress => RootTodoStatus::InProgress,
            TodoStatus::Completed => RootTodoStatus::Completed,
        }
    }
}

impl From<RootTodoStatus> for TodoStatus {
    fn from(s: RootTodoStatus) -> Self {
        match s {
            RootTodoStatus::Pending => TodoStatus::Pending,
            RootTodoStatus::InProgress => TodoStatus::InProgress,
            RootTodoStatus::Completed => TodoStatus::Completed,
        }
    }
}

impl From<Todo> for crate::session::todos::Todo {
    fn from(t: Todo) -> Self {
        Self {
            task_id: t.task_id,
            subject: t.subject,
            description: t.description,
            active_form: t.active_form,
            status: t.status.into(),
            owner: t.owner,
            created_at: t.created_at,
            updated_at: t.updated_at,
        }
    }
}

impl From<crate::session::todos::Todo> for Todo {
    fn from(t: crate::session::todos::Todo) -> Self {
        Self {
            task_id: t.task_id,
            subject: t.subject,
            description: t.description,
            active_form: t.active_form,
            status: t.status.into(),
            owner: t.owner,
            created_at: t.created_at,
            updated_at: t.updated_at,
        }
    }
}

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
        Ok(todo.into())
    }

    async fn get_todo(&self, session_key: &str, task_id: &str) -> anyhow::Result<Option<Todo>> {
        let todo = self.storage.get_todo(session_key, task_id).await?;
        Ok(todo.map(Into::into))
    }

    async fn list_todos(
        &self,
        session_key: &str,
        status_filter: Option<TodoStatus>,
    ) -> anyhow::Result<Vec<Todo>> {
        let filter = status_filter.map(Into::into);
        let todos = self.storage.list_todos(session_key, filter).await?;
        Ok(todos.into_iter().map(Into::into).collect())
    }

    async fn update_todo(
        &self,
        session_key: &str,
        task_id: &str,
        status: Option<TodoStatus>,
        owner: Option<String>,
    ) -> anyhow::Result<Option<Todo>> {
        let status = status.map(Into::into);
        let todo = self
            .storage
            .update_todo(session_key, task_id, status, owner)
            .await?;
        Ok(todo.map(Into::into))
    }
}
