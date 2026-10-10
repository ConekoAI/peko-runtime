//! `TodoStorageRuntime` — root-side adapter for the `TodoRuntime` port.
//!
//! Phase 10d lifts `Task action create`/`Task action get`/`Task action list`/`Task action update` into
//! `peko_tools_builtin::tasks`. The tool surface there speaks to a
//! [`crate::tools::builtin::tasks::TodoRuntime`] port trait so the
//! built-in crate can stay free of root-only deps. This file is the
//! production adapter: it wraps the existing
//! [`peko_session::TodoStorage`] so the same JSONL sidecar format,
//! file-lock semantics, and atomic-rename write strategy continue to
//! apply.
//!
//! The tool's `Todo` / `TodoStatus` are `peko_session`'s, so the adapter
//! passes storage records through unchanged.

use std::sync::Arc;

use crate::tools::builtin::tasks::{Todo, TodoRuntime, TodoStatus};
use async_trait::async_trait;
use peko_session::TodoStorage;

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
        self.storage
            .create_todo(session_key, subject, description, active_form)
            .await
    }

    async fn get_todo(&self, session_key: &str, task_id: &str) -> anyhow::Result<Option<Todo>> {
        self.storage.get_todo(session_key, task_id).await
    }

    async fn list_todos(
        &self,
        session_key: &str,
        status_filter: Option<TodoStatus>,
    ) -> anyhow::Result<Vec<Todo>> {
        self.storage.list_todos(session_key, status_filter).await
    }

    async fn update_todo(
        &self,
        session_key: &str,
        task_id: &str,
        status: Option<TodoStatus>,
        owner: Option<String>,
    ) -> anyhow::Result<Option<Todo>> {
        self.storage
            .update_todo(session_key, task_id, status, owner)
            .await
    }
}
