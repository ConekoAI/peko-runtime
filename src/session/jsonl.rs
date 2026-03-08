//! OpenClaw-compatible Session JSONL Format
//!
//! Matches OpenClaw's format with proper content blocks:
//! - session entry (metadata)
//! - message entries (user/assistant/tool with content blocks)
//! - toolCall entries
//! - toolResult entries

use crate::session::lock::FileLock;
use crate::types::ContentBlock;
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tracing::{debug, info};

/// Default lock timeout for session operations (10 seconds)
pub const SESSION_LOCK_TIMEOUT_MS: u64 = 10_000;

/// Session entry type (first line in JSONL)
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

/// Message content structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageContent {
    pub role: String,
    pub content: Vec<ContentBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<i64>,
}

/// Session storage with OpenClaw-compatible JSONL format
pub struct SessionStorage {
    storage_dir: PathBuf,
}

impl SessionStorage {
    /// Create new session storage
    pub fn new(storage_dir: PathBuf) -> Self {
        Self { storage_dir }
    }

    /// Initialize a new session
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
        fs::write(&path, json + "\n").await?;

        info!("Created session: {}", session_id);
        Ok(())
    }

    /// Append a message to the session
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

        let entry_id = format!("msg_{}", uuid::Uuid::new_v4().to_string().replace("-", ""));

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

        // Append to file
        use tokio::io::AsyncWriteExt;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;
        file.write_all(line.as_bytes()).await?;
        file.flush().await?;
        drop(file);

        debug!("Appended message to session {}: {}", session_id, entry_id);
        Ok(entry_id)
    }

    /// Append a tool result to the session
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

        use tokio::io::AsyncWriteExt;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;
        file.write_all(line.as_bytes()).await?;
        file.flush().await?;

        debug!("Appended tool result to session {}", session_id);
        Ok(())
    }

    /// Append model change entry
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
            uuid::Uuid::new_v4().to_string().replace("-", "")
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

        use tokio::io::AsyncWriteExt;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;
        file.write_all(line.as_bytes()).await?;
        file.flush().await?;

        Ok(entry_id)
    }

    /// Append compaction entry
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
            uuid::Uuid::new_v4().to_string().replace("-", "")
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

        use tokio::io::AsyncWriteExt;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;
        file.write_all(line.as_bytes()).await?;
        file.flush().await?;

        debug!(
            "Appended compaction #{} to session {}",
            compaction_number, session_id
        );
        Ok(entry_id)
    }

    /// Load all entries from a session (with shared lock for consistency)
    pub async fn load_session(&self, session_id: &str) -> Result<Vec<SessionEntry>> {
        let path = self.session_path(session_id);

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

    /// Get session file path
    fn session_path(&self, session_id: &str) -> PathBuf {
        self.storage_dir.join(format!("{}.jsonl", session_id))
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
}
