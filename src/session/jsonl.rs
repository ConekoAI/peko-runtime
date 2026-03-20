//! Pekobot Session JSONL Format with Atomic Writes
//!
//! Implements durable JSONL sessions per DATA_MODEL.md §5:
//! - Atomic writes: events written to `.tmp` then renamed
//! - Automatic cleanup of partial `.tmp` files on load
//! - Support for Pekobot event format (13 event types)
//! - Backward compatibility with OpenClaw format

use crate::session::events::SessionEvent;
use crate::session::lock::FileLock;
use crate::types::ContentBlock;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tracing::{debug, info, warn};

/// Default lock timeout for session operations (10 seconds)
pub const SESSION_LOCK_TIMEOUT_MS: u64 = 10_000;

/// Legacy session entry type (OpenClaw compatible)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SessionEntry {
    #[serde(rename = "session")]
    Session {
        version: i32,
        id: String,
        timestamp: DateTime<Utc>,
        #[serde(skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
    },

    #[serde(rename = "model_change")]
    ModelChange {
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        parent_id: Option<String>,
        timestamp: DateTime<Utc>,
        provider: String,
        #[serde(rename = "modelId")]
        model_id: String,
    },

    #[serde(rename = "message")]
    Message {
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        parent_id: Option<String>,
        timestamp: DateTime<Utc>,
        message: MessageContent,
    },

    #[serde(rename = "toolResult")]
    ToolResult {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(rename = "toolName")]
        tool_name: String,
        content: Vec<ContentBlock>,
        #[serde(skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },

    /// Compaction entry - records a context compaction event
    #[serde(rename = "compaction")]
    Compaction {
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        parent_id: Option<String>,
        timestamp: DateTime<Utc>,
        /// Summary text (structured format)
        summary: String,
        /// Number of messages compacted
        messages_compacted: usize,
        /// Tokens before compaction
        tokens_before: usize,
        /// Tokens after compaction
        tokens_after: usize,
        /// Compaction number (1st, 2nd, etc.)
        compaction_number: usize,
    },

    #[serde(rename = "custom")]
    Custom {
        #[serde(rename = "customType")]
        custom_type: String,
        data: serde_json::Value,
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        parent_id: Option<String>,
        timestamp: DateTime<Utc>,
    },
}

/// Message content structure (legacy)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageContent {
    pub role: String,
    pub content: Vec<ContentBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<i64>,
}

/// Session storage with atomic writes
#[derive(Debug, Clone)]
pub struct SessionStorage {
    storage_dir: PathBuf,
}

impl SessionStorage {
    /// Create new session storage
    #[must_use]
    pub fn new(storage_dir: PathBuf) -> Self {
        Self { storage_dir }
    }

    /// Get the storage directory
    #[must_use]
    pub fn storage_dir(&self) -> &Path {
        &self.storage_dir
    }

    /// Initialize a new session with atomic write
    pub async fn create_session(&self, session_id: &str, cwd: Option<String>) -> Result<()> {
        // Ensure directory exists
        fs::create_dir_all(&self.storage_dir).await?;

        let path = self.session_path(session_id);

        // Create session entry
        let session_entry = SessionEntry::Session {
            version: 3,
            id: session_id.to_string(),
            timestamp: Utc::now(),
            cwd,
        };

        let json = serde_json::to_string(&session_entry)?;
        self.atomic_write(&path, json + "\n", false).await?;

        info!("Created session: {}", session_id);
        Ok(())
    }

    /// Append a message to the session atomically
    pub async fn append_message(
        &self,
        session_id: &str,
        parent_id: Option<String>,
        role: &str,
        content: Vec<ContentBlock>,
    ) -> Result<String> {
        let path = self.session_path(session_id);

        // Acquire lock for concurrent access protection
        let _lock = FileLock::acquire(&path, SESSION_LOCK_TIMEOUT_MS).await?;

        let entry_id = format!("msg_{}", uuid::Uuid::new_v4().to_string().replace('-', ""));

        let entry = SessionEntry::Message {
            id: entry_id.clone(),
            parent_id,
            timestamp: Utc::now(),
            message: MessageContent {
                role: role.to_string(),
                content,
                timestamp: Some(Utc::now().timestamp_millis()),
            },
        };

        let json = serde_json::to_string(&entry)?;
        let line = json + "\n";

        // Atomic append
        self.atomic_append(&path, &line).await?;

        debug!("Appended message to session {}: {}", session_id, entry_id);
        Ok(entry_id)
    }

    /// Append a Pekobot event to the session atomically
    pub async fn append_event(&self, session_id: &str, event: &SessionEvent) -> Result<()> {
        let path = self.session_path(session_id);

        // Acquire lock for concurrent access protection
        let _lock = FileLock::acquire(&path, SESSION_LOCK_TIMEOUT_MS).await?;

        let json = serde_json::to_string(event)?;
        let line = json + "\n";

        // Atomic append
        self.atomic_append(&path, &line).await?;

        debug!(
            "Appended event to session {}: {}",
            session_id,
            event.event_type()
        );
        Ok(())
    }

    /// Append a tool result to the session atomically
    pub async fn append_tool_result(
        &self,
        session_id: &str,
        tool_call_id: &str,
        tool_name: &str,
        result: String,
        is_error: bool,
    ) -> Result<()> {
        let path = self.session_path(session_id);

        // Acquire lock for concurrent access protection
        let _lock = FileLock::acquire(&path, SESSION_LOCK_TIMEOUT_MS).await?;

        let entry = SessionEntry::ToolResult {
            tool_call_id: tool_call_id.to_string(),
            tool_name: tool_name.to_string(),
            content: vec![ContentBlock::Text { text: result }],
            is_error: Some(is_error),
        };

        let json = serde_json::to_string(&entry)?;
        let line = json + "\n";

        // Atomic append
        self.atomic_append(&path, &line).await?;

        debug!("Appended tool result to session {}", session_id);
        Ok(())
    }

    /// Append model change entry atomically
    pub async fn append_model_change(
        &self,
        session_id: &str,
        parent_id: Option<String>,
        provider: &str,
        model_id: &str,
    ) -> Result<String> {
        let path = self.session_path(session_id);

        // Acquire lock for concurrent access protection
        let _lock = FileLock::acquire(&path, SESSION_LOCK_TIMEOUT_MS).await?;

        let entry_id = format!(
            "model_{}",
            uuid::Uuid::new_v4().to_string().replace('-', "")
        );

        let entry = SessionEntry::ModelChange {
            id: entry_id.clone(),
            parent_id,
            timestamp: Utc::now(),
            provider: provider.to_string(),
            model_id: model_id.to_string(),
        };

        let json = serde_json::to_string(&entry)?;
        let line = json + "\n";

        // Atomic append
        self.atomic_append(&path, &line).await?;

        Ok(entry_id)
    }

    /// Append compaction entry atomically
    pub async fn append_compaction(
        &self,
        session_id: &str,
        parent_id: Option<String>,
        summary: &str,
        messages_compacted: usize,
        tokens_before: usize,
        tokens_after: usize,
        compaction_number: usize,
    ) -> Result<String> {
        let path = self.session_path(session_id);

        // Acquire lock for concurrent access protection
        let _lock = FileLock::acquire(&path, SESSION_LOCK_TIMEOUT_MS).await?;

        let entry_id = format!(
            "compact_{}",
            uuid::Uuid::new_v4().to_string().replace('-', "")
        );

        let entry = SessionEntry::Compaction {
            id: entry_id.clone(),
            parent_id,
            timestamp: Utc::now(),
            summary: summary.to_string(),
            messages_compacted,
            tokens_before,
            tokens_after,
            compaction_number,
        };

        let json = serde_json::to_string(&entry)?;
        let line = json + "\n";

        // Atomic append
        self.atomic_append(&path, &line).await?;

        debug!(
            "Appended compaction #{} to session {}",
            compaction_number, session_id
        );
        Ok(entry_id)
    }

    /// Write content atomically (tmp file + rename)
    ///
    /// If `append` is true, the content will be appended to the existing file.
    /// If `append` is false, the file will be overwritten.
    async fn atomic_write(&self, path: &Path, content: String, append: bool) -> Result<()> {
        let temp_path = path.with_extension("tmp");

        if append && path.exists() {
            // For append, we need to copy existing content to temp first
            let existing = fs::read_to_string(path).await?;
            let combined = existing + &content;

            // Write combined content to temp
            let mut file = fs::File::create(&temp_path).await?;
            file.write_all(combined.as_bytes()).await?;
            file.flush().await?;
            drop(file);
        } else {
            // For new file, just write content
            let mut file = fs::File::create(&temp_path).await?;
            file.write_all(content.as_bytes()).await?;
            file.flush().await?;
            drop(file);
        }

        // Atomic rename
        fs::rename(&temp_path, path).await?;

        Ok(())
    }

    /// Append a line atomically
    async fn atomic_append(&self, path: &Path, line: &str) -> Result<()> {
        self.atomic_write(path, line.to_string(), true).await
    }

    /// Load all entries from a session (legacy format)
    ///
    /// Also cleans up any partial .tmp files that may exist from crashes.
    pub async fn load_session(&self, session_id: &str) -> Result<Vec<SessionEntry>> {
        let path = self.session_path(session_id);
        // Clean up any partial tmp files from previous crashes
        self.cleanup_temp_files(session_id).await?;

        if !path.exists() {
            return Ok(vec![]);
        }

        // Acquire lock to ensure consistent read
        let _lock = FileLock::acquire(&path, SESSION_LOCK_TIMEOUT_MS).await?;

        let content = fs::read_to_string(&path).await?;
        let mut entries = vec![];

        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<SessionEntry>(line) {
                Ok(entry) => entries.push(entry),
                Err(e) => {
                    debug!("Failed to parse session entry: {}", e);
                }
            }
        }

        Ok(entries)
    }

    /// Load all Pekobot events from a session
    ///
    /// Also cleans up any partial .tmp files that may exist from crashes.
    pub async fn load_events(&self, session_id: &str) -> Result<Vec<SessionEvent>> {
        let path = self.session_path(session_id);

        // Clean up any partial tmp files from previous crashes
        self.cleanup_temp_files(session_id).await?;

        if !path.exists() {
            return Ok(vec![]);
        }

        // Acquire lock to ensure consistent read
        let _lock = FileLock::acquire(&path, SESSION_LOCK_TIMEOUT_MS).await?;

        let content = fs::read_to_string(&path).await?;
        let mut events = vec![];

        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            // Try to parse as Pekobot event first
            match serde_json::from_str::<SessionEvent>(line) {
                Ok(event) => {
                    events.push(event);
                }
                Err(e) => {
                    debug!("Failed to parse as Pekobot event: {}", e);
                    // Could be legacy format - skip for now
                    // In a full implementation, we might convert legacy events
                }
            }
        }

        Ok(events)
    }

    /// Clean up partial .tmp files from a previous crash
    pub async fn cleanup_temp_files(&self, session_id: &str) -> Result<()> {
        let tmp_path = self.session_tmp_path(session_id);

        if tmp_path.exists() {
            warn!(
                "Found partial tmp file from previous crash: {}. Removing.",
                tmp_path.display()
            );
            fs::remove_file(&tmp_path).await?;
        }

        Ok(())
    }

    /// Get session file path
    fn session_path(&self, session_id: &str) -> PathBuf {
        self.storage_dir.join(format!("{session_id}.jsonl"))
    }

    /// Get session tmp file path
    fn session_tmp_path(&self, session_id: &str) -> PathBuf {
        self.storage_dir.join(format!("{session_id}.tmp"))
    }

    /// Get index file path for a session
    pub fn index_path(&self, session_id: &str) -> PathBuf {
        self.storage_dir.join(format!("{session_id}.index.json"))
    }

    /// Check if session exists
    pub async fn session_exists(&self, session_id: &str) -> bool {
        self.session_path(session_id).exists()
    }

    /// List all sessions
    pub async fn list_sessions(&self) -> Result<Vec<String>> {
        let mut sessions = vec![];

        if self.storage_dir.exists() {
            let mut entries = fs::read_dir(&self.storage_dir).await?;
            while let Some(entry) = entries.next_entry().await? {
                if let Some(name) = entry.file_name().to_str() {
                    if name.ends_with(".jsonl") {
                        sessions.push(name.trim_end_matches(".jsonl").to_string());
                    }
                }
            }
        }

        sessions.sort_by(|a, b| b.cmp(a)); // Newest first
        Ok(sessions)
    }

    /// Delete a session file
    pub async fn delete_session(&self, session_id: &str) -> Result<()> {
        let path = self.session_path(session_id);
        let index_path = self.index_path(session_id);

        if path.exists() {
            fs::remove_file(&path).await?;
        }

        if index_path.exists() {
            fs::remove_file(&index_path).await?;
        }

        info!("Deleted session: {}", session_id);
        Ok(())
    }

    /// Copy a session file (for branching)
    pub async fn copy_session(&self, source_id: &str, target_id: &str) -> Result<()> {
        let source_path = self.session_path(source_id);
        let target_path = self.session_path(target_id);

        if !source_path.exists() {
            return Err(anyhow::anyhow!(
                "Source session {} does not exist",
                source_id
            ));
        }

        fs::copy(&source_path, &target_path).await?;

        info!("Copied session {} to {}", source_id, target_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_session_creation() {
        let temp = TempDir::new().unwrap();
        let storage = SessionStorage::new(temp.path().to_path_buf());

        storage
            .create_session("test_session", Some("/home/test".to_string()))
            .await
            .unwrap();

        let sessions = storage.list_sessions().await.unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0], "test_session");
    }

    #[tokio::test]
    async fn test_append_and_load() {
        let temp = TempDir::new().unwrap();
        let storage = SessionStorage::new(temp.path().to_path_buf());

        storage.create_session("test", None).await.unwrap();

        let msg_id = storage
            .append_message(
                "test",
                None,
                "user",
                vec![ContentBlock::Text {
                    text: "Hello".to_string(),
                }],
            )
            .await
            .unwrap();

        assert!(!msg_id.is_empty());

        let entries = storage.load_session("test").await.unwrap();
        assert_eq!(entries.len(), 2); // session + message
    }

    #[tokio::test]
    async fn test_atomic_append() {
        let temp = TempDir::new().unwrap();
        let storage = SessionStorage::new(temp.path().to_path_buf());

        // Create session
        storage.create_session("atomic_test", None).await.unwrap();

        // Append multiple messages
        for i in 0..10 {
            storage
                .append_message(
                    "atomic_test",
                    None,
                    "user",
                    vec![ContentBlock::Text {
                        text: format!("Message {}", i),
                    }],
                )
                .await
                .unwrap();
        }

        // Verify all entries
        let entries = storage.load_session("atomic_test").await.unwrap();
        assert_eq!(entries.len(), 11); // session + 10 messages
    }

    #[tokio::test]
    async fn test_cleanup_temp_files() {
        let temp = TempDir::new().unwrap();
        let storage = SessionStorage::new(temp.path().to_path_buf());

        // Create a fake tmp file
        let tmp_path = temp.path().join("test_session.tmp");
        fs::write(&tmp_path, "partial content").await.unwrap();

        assert!(tmp_path.exists());

        // Loading session should clean up tmp file
        storage.cleanup_temp_files("test_session").await.unwrap();

        assert!(!tmp_path.exists());
    }

    #[tokio::test]
    async fn test_copy_session() {
        let temp = TempDir::new().unwrap();
        let storage = SessionStorage::new(temp.path().to_path_buf());

        // Create and populate session
        storage.create_session("source", None).await.unwrap();
        storage
            .append_message(
                "source",
                None,
                "user",
                vec![ContentBlock::Text {
                    text: "Hello".to_string(),
                }],
            )
            .await
            .unwrap();

        // Copy session
        storage.copy_session("source", "target").await.unwrap();

        // Verify copy
        let source_entries = storage.load_session("source").await.unwrap();
        let target_entries = storage.load_session("target").await.unwrap();
        assert_eq!(source_entries.len(), target_entries.len());
    }

    #[tokio::test]
    async fn test_delete_session() {
        let temp = TempDir::new().unwrap();
        let storage = SessionStorage::new(temp.path().to_path_buf());

        storage.create_session("to_delete", None).await.unwrap();
        assert!(storage.session_exists("to_delete").await);

        storage.delete_session("to_delete").await.unwrap();
        assert!(!storage.session_exists("to_delete").await);
    }
}
