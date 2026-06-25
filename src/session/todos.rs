//! Planning todo sidecar storage.
//!
//! Todos live in a per-session JSONL sidecar (`{session_key}.todos.jsonl`)
//! alongside the main session JSONL. Writes are atomic (tmp + rename) and
//! use the same durability strategy as `SessionStorage`.

use crate::session::lock::FileLock;
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::io::AsyncWriteExt;

/// Default lock timeout for todo operations (10 seconds)
pub const TODO_LOCK_TIMEOUT_MS: u64 = 10_000;

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

impl std::str::FromStr for TodoStatus {
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

/// Storage backend for session-scoped planning todos.
#[derive(Debug, Clone)]
pub struct TodoStorage {
    storage_dir: PathBuf,
}

impl TodoStorage {
    /// Create new todo storage rooted in the given session directory.
    #[must_use]
    pub fn new(storage_dir: PathBuf) -> Self {
        Self { storage_dir }
    }

    /// Get the storage directory.
    #[must_use]
    pub fn storage_dir(&self) -> &Path {
        &self.storage_dir
    }

    /// Path to the sidecar file for a session.
    fn sidecar_path(&self, session_key: &str) -> PathBuf {
        self.storage_dir.join(format!("{session_key}.todos.jsonl"))
    }

    /// Load all todos for a session.
    pub async fn load_todos(&self, session_key: &str) -> Result<Vec<Todo>> {
        let path = self.sidecar_path(session_key);
        if !path.exists() {
            return Ok(vec![]);
        }

        let _lock = FileLock::acquire(&path, TODO_LOCK_TIMEOUT_MS).await?;
        let content = fs::read_to_string(&path).await?;

        let mut todos = Vec::new();
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Todo>(line) {
                Ok(todo) => todos.push(todo),
                Err(e) => tracing::debug!("Failed to parse todo line: {e}"),
            }
        }
        Ok(todos)
    }

    /// Replace all todos for a session atomically.
    async fn save_todos(&self, session_key: &str, todos: &[Todo]) -> Result<()> {
        fs::create_dir_all(&self.storage_dir).await?;
        let path = self.sidecar_path(session_key);
        let _lock = FileLock::acquire(&path, TODO_LOCK_TIMEOUT_MS).await?;

        let mut lines = String::new();
        for todo in todos {
            lines.push_str(&serde_json::to_string(todo)?);
            lines.push('\n');
        }

        self.atomic_write(&path, lines).await
    }

    /// Write content atomically (tmp file + rename).
    async fn atomic_write(&self, path: &Path, content: String) -> Result<()> {
        let temp_path = path.with_extension("todos.tmp");
        let mut file = fs::File::create(&temp_path).await?;
        file.write_all(content.as_bytes()).await?;
        file.flush().await?;
        drop(file);
        fs::rename(&temp_path, path).await?;
        Ok(())
    }

    /// Create a new todo and persist it.
    pub async fn create_todo(
        &self,
        session_key: &str,
        subject: String,
        description: Option<String>,
        active_form: Option<String>,
    ) -> Result<Todo> {
        let mut todos = self.load_todos(session_key).await?;
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
        todos.push(todo.clone());
        self.save_todos(session_key, &todos).await?;
        Ok(todo)
    }

    /// Get a single todo by id.
    pub async fn get_todo(&self, session_key: &str, task_id: &str) -> Result<Option<Todo>> {
        let todos = self.load_todos(session_key).await?;
        Ok(todos.into_iter().find(|t| t.task_id == task_id))
    }

    /// List todos, optionally filtered by status.
    pub async fn list_todos(
        &self,
        session_key: &str,
        status_filter: Option<TodoStatus>,
    ) -> Result<Vec<Todo>> {
        let todos = self.load_todos(session_key).await?;
        match status_filter {
            Some(status) => Ok(todos.into_iter().filter(|t| t.status == status).collect()),
            None => Ok(todos),
        }
    }

    /// Update a todo's status and/or owner.
    pub async fn update_todo(
        &self,
        session_key: &str,
        task_id: &str,
        status: Option<TodoStatus>,
        owner: Option<String>,
    ) -> Result<Option<Todo>> {
        let mut todos = self.load_todos(session_key).await?;
        let mut updated = None;
        for todo in &mut todos {
            if todo.task_id == task_id {
                if let Some(s) = status {
                    todo.status = s;
                }
                if owner.is_some() {
                    todo.owner = owner.clone();
                }
                todo.updated_at = Utc::now();
                updated = Some(todo.clone());
                break;
            }
        }
        if updated.is_some() {
            self.save_todos(session_key, &todos).await?;
        }
        Ok(updated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_create_and_get_todo() {
        let temp = TempDir::new().unwrap();
        let storage = TodoStorage::new(temp.path().to_path_buf());

        let created = storage
            .create_todo(
                "agent:test:cli:default",
                "Fix the thing".to_string(),
                Some("Detailed description".to_string()),
                Some("Fixing the thing".to_string()),
            )
            .await
            .unwrap();

        assert!(created.task_id.starts_with("todo:"));
        assert_eq!(created.subject, "Fix the thing");
        assert_eq!(created.status, TodoStatus::Pending);

        let fetched = storage
            .get_todo("agent:test:cli:default", &created.task_id)
            .await
            .unwrap();
        assert!(fetched.is_some());
        let fetched = fetched.unwrap();
        assert_eq!(fetched.subject, "Fix the thing");
        assert_eq!(fetched.active_form, Some("Fixing the thing".to_string()));
    }

    #[tokio::test]
    async fn test_list_and_filter() {
        let temp = TempDir::new().unwrap();
        let storage = TodoStorage::new(temp.path().to_path_buf());
        let session = "agent:test:cli:default";

        let a = storage
            .create_todo(session, "A".to_string(), None, None)
            .await
            .unwrap();
        let b = storage
            .create_todo(session, "B".to_string(), None, None)
            .await
            .unwrap();
        storage
            .update_todo(session, &b.task_id, Some(TodoStatus::InProgress), None)
            .await
            .unwrap();

        let all = storage.list_todos(session, None).await.unwrap();
        assert_eq!(all.len(), 2);

        let pending = storage
            .list_todos(session, Some(TodoStatus::Pending))
            .await
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].task_id, a.task_id);
    }

    #[tokio::test]
    async fn test_update_todo() {
        let temp = TempDir::new().unwrap();
        let storage = TodoStorage::new(temp.path().to_path_buf());
        let session = "agent:test:cli:default";

        let todo = storage
            .create_todo(session, "Task".to_string(), None, None)
            .await
            .unwrap();
        let updated = storage
            .update_todo(
                session,
                &todo.task_id,
                Some(TodoStatus::Completed),
                Some("claude".to_string()),
            )
            .await
            .unwrap()
            .unwrap();

        assert_eq!(updated.status, TodoStatus::Completed);
        assert_eq!(updated.owner, Some("claude".to_string()));
        assert!(updated.updated_at >= updated.created_at);

        let missing = storage
            .update_todo(session, "todo:nope", Some(TodoStatus::InProgress), None)
            .await
            .unwrap();
        assert!(missing.is_none());
    }

    #[test]
    fn test_todo_status_from_str() {
        assert_eq!(
            TodoStatus::from_str("pending").unwrap(),
            TodoStatus::Pending
        );
        assert_eq!(
            TodoStatus::from_str("in_progress").unwrap(),
            TodoStatus::InProgress
        );
        assert_eq!(
            TodoStatus::from_str("completed").unwrap(),
            TodoStatus::Completed
        );
        assert!(TodoStatus::from_str("unknown").is_err());
    }
}
