//! Session Transcript Storage - JSONL persistence for conversation history
//!
//! Matches `OpenClaw`'s format:
//! ~/.pekobot/agents/<agentId>/sessions/
//!   ├── sessions.json          # Session metadata
//!   ├── <sessionId>.jsonl      # Full conversation transcript
//!   └── <sessionId>-topic-<threadId>.jsonl  # Thread sessions

use crate::types::provider::{ChatMessage, ToolCall};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::fs;
use tracing::{debug, info, warn};

/// A single turn in the conversation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptEntry {
    /// Entry ID (UUID)
    pub id: String,
    /// Timestamp
    pub timestamp: DateTime<Utc>,
    /// Message role
    pub role: String,
    /// Message content
    pub content: String,
    /// Tool calls (for assistant messages)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// Tool call ID (for tool messages)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Tool name (for tool messages)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    /// Metadata
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, serde_json::Value>>,
}

impl TranscriptEntry {
    /// Create from a `ChatMessage`
    #[must_use]
    pub fn from_message(msg: &ChatMessage) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            role: msg.role.clone(),
            content: msg.content.clone(),
            tool_calls: msg.tool_calls.clone(),
            tool_call_id: msg.tool_call_id.clone(),
            tool_name: msg.name.clone(),
            metadata: None,
        }
    }

    /// Create a system entry
    #[must_use]
    pub fn system(content: &str) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            role: "system".to_string(),
            content: content.to_string(),
            tool_calls: None,
            tool_call_id: None,
            tool_name: None,
            metadata: None,
        }
    }

    /// Create a user entry
    #[must_use]
    pub fn user(content: &str) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            role: "user".to_string(),
            content: content.to_string(),
            tool_calls: None,
            tool_call_id: None,
            tool_name: None,
            metadata: None,
        }
    }

    /// Create an assistant entry
    #[must_use]
    pub fn assistant(content: &str) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            role: "assistant".to_string(),
            content: content.to_string(),
            tool_calls: None,
            tool_call_id: None,
            tool_name: None,
            metadata: None,
        }
    }
}

/// Transcript storage configuration
#[derive(Debug, Clone)]
pub struct TranscriptConfig {
    /// Base storage directory
    pub storage_dir: PathBuf,
    /// Auto-create directories
    pub auto_create: bool,
}

impl Default for TranscriptConfig {
    fn default() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        Self {
            storage_dir: home
                .join(".pekobot")
                .join("agents")
                .join("main")
                .join("sessions"),
            auto_create: true,
        }
    }
}

/// Manages JSONL transcript storage
pub struct TranscriptStorage {
    config: TranscriptConfig,
}

impl TranscriptStorage {
    /// Create new transcript storage
    #[must_use]
    pub fn new(config: TranscriptConfig) -> Self {
        Self { config }
    }

    /// Create with default config
    #[must_use]
    pub fn default_config() -> Self {
        Self::new(TranscriptConfig::default())
    }

    /// Get path for a session transcript
    #[must_use]
    pub fn transcript_path(&self, session_id: &str) -> PathBuf {
        self.config.storage_dir.join(format!("{session_id}.jsonl"))
    }

    /// Get path for thread transcript
    #[must_use]
    pub fn thread_transcript_path(&self, session_id: &str, thread_id: &str) -> PathBuf {
        self.config
            .storage_dir
            .join(format!("{session_id}-thread-{thread_id}.jsonl"))
    }

    /// Ensure storage directory exists
    async fn ensure_dir(&self) -> Result<()> {
        if !self.config.auto_create {
            return Ok(());
        }

        if !self.config.storage_dir.exists() {
            fs::create_dir_all(&self.config.storage_dir)
                .await
                .context("Failed to create transcript directory")?;
            debug!(
                "Created transcript directory: {}",
                self.config.storage_dir.display()
            );
        }

        Ok(())
    }

    /// Append entry to transcript
    pub async fn append(&self, session_id: &str, entry: &TranscriptEntry) -> Result<()> {
        self.ensure_dir().await?;

        let path = self.transcript_path(session_id);
        let line = serde_json::to_string(entry).context("Failed to serialize transcript entry")?;

        // Append with newline
        let mut file_content = if path.exists() {
            fs::read_to_string(&path).await.unwrap_or_default()
        } else {
            String::new()
        };

        file_content.push_str(&line);
        file_content.push('\n');

        fs::write(&path, file_content)
            .await
            .context("Failed to write transcript")?;

        debug!("Appended entry to transcript: {}", path.display());
        Ok(())
    }

    /// Append multiple entries
    pub async fn append_batch(&self, session_id: &str, entries: &[TranscriptEntry]) -> Result<()> {
        for entry in entries {
            self.append(session_id, entry).await?;
        }
        Ok(())
    }

    /// Read transcript for a session
    pub async fn read(&self, session_id: &str) -> Result<Vec<TranscriptEntry>> {
        let path = self.transcript_path(session_id);

        if !path.exists() {
            return Ok(vec![]);
        }

        let content = fs::read_to_string(&path)
            .await
            .context("Failed to read transcript")?;

        let mut entries = vec![];
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }

            match serde_json::from_str::<TranscriptEntry>(line) {
                Ok(entry) => entries.push(entry),
                Err(e) => warn!("Failed to parse transcript line: {}", e),
            }
        }

        Ok(entries)
    }

    /// Read transcript as raw JSONL string
    pub async fn read_raw(&self, session_id: &str) -> Result<Option<String>> {
        let path = self.transcript_path(session_id);

        if !path.exists() {
            return Ok(None);
        }

        let content = fs::read_to_string(&path)
            .await
            .context("Failed to read transcript")?;

        Ok(Some(content))
    }

    /// List all transcript files
    pub async fn list_transcripts(&self) -> Result<Vec<(PathBuf, String)>> {
        if !self.config.storage_dir.exists() {
            return Ok(vec![]);
        }

        let mut files = vec![];
        let mut entries = fs::read_dir(&self.config.storage_dir)
            .await
            .context("Failed to read transcript directory")?;

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "jsonl") {
                let session_id = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("unknown")
                    .to_string();
                files.push((path, session_id));
            }
        }

        Ok(files)
    }

    /// Delete transcript
    pub async fn delete(&self, session_id: &str) -> Result<bool> {
        let path = self.transcript_path(session_id);

        if path.exists() {
            fs::remove_file(&path)
                .await
                .context("Failed to delete transcript")?;
            info!("Deleted transcript: {}", path.display());
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Get transcript stats
    pub async fn stats(&self, session_id: &str) -> Result<TranscriptStats> {
        let entries = self.read(session_id).await?;

        let user_count = entries.iter().filter(|e| e.role == "user").count();
        let assistant_count = entries.iter().filter(|e| e.role == "assistant").count();
        let tool_count = entries.iter().filter(|e| e.role == "tool").count();

        let total_chars: usize = entries.iter().map(|e| e.content.len()).sum();

        let first_timestamp = entries.first().map(|e| e.timestamp);
        let last_timestamp = entries.last().map(|e| e.timestamp);

        Ok(TranscriptStats {
            entry_count: entries.len(),
            user_messages: user_count,
            assistant_messages: assistant_count,
            tool_results: tool_count,
            total_chars,
            first_timestamp,
            last_timestamp,
        })
    }

    /// Search within transcript
    pub async fn search(&self, session_id: &str, query: &str) -> Result<Vec<TranscriptEntry>> {
        let entries = self.read(session_id).await?;
        let query_lower = query.to_lowercase();

        let matches: Vec<TranscriptEntry> = entries
            .into_iter()
            .filter(|e| {
                e.content.to_lowercase().contains(&query_lower)
                    || e.role.to_lowercase().contains(&query_lower)
            })
            .collect();

        Ok(matches)
    }

    /// Get status
    pub async fn status(&self) -> Result<String> {
        let transcripts = self.list_transcripts().await?;

        let total_entries: usize =
            futures::future::try_join_all(transcripts.iter().map(|(_, id)| self.stats(id)))
                .await
                .map(|stats| stats.iter().map(|s| s.entry_count).sum())?;

        Ok(format!(
            "📝 Transcripts: {} files, {} total entries",
            transcripts.len(),
            total_entries
        ))
    }
}

impl Default for TranscriptStorage {
    fn default() -> Self {
        Self::default_config()
    }
}

/// Transcript statistics
#[derive(Debug, Clone)]
pub struct TranscriptStats {
    pub entry_count: usize,
    pub user_messages: usize,
    pub assistant_messages: usize,
    pub tool_results: usize,
    pub total_chars: usize,
    pub first_timestamp: Option<DateTime<Utc>>,
    pub last_timestamp: Option<DateTime<Utc>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    async fn create_test_storage() -> (TranscriptStorage, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        let config = TranscriptConfig {
            storage_dir: temp_dir.path().join("sessions"),
            auto_create: true,
        };
        let storage = TranscriptStorage::new(config);
        (storage, temp_dir)
    }

    #[tokio::test]
    async fn test_append_and_read() {
        let (storage, _temp) = create_test_storage().await;

        let entry = TranscriptEntry::user("Hello");
        storage.append("session-1", &entry).await.unwrap();

        let entries = storage.read("session-1").await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].role, "user");
        assert_eq!(entries[0].content, "Hello");
    }

    #[tokio::test]
    async fn test_multiple_entries() {
        let (storage, _temp) = create_test_storage().await;

        storage
            .append("session-2", &TranscriptEntry::system("You are helpful"))
            .await
            .unwrap();
        storage
            .append("session-2", &TranscriptEntry::user("Hi"))
            .await
            .unwrap();
        storage
            .append("session-2", &TranscriptEntry::assistant("Hello!"))
            .await
            .unwrap();

        let entries = storage.read("session-2").await.unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].role, "system");
        assert_eq!(entries[1].role, "user");
        assert_eq!(entries[2].role, "assistant");
    }

    #[tokio::test]
    async fn test_stats() {
        let (storage, _temp) = create_test_storage().await;

        storage
            .append("session-3", &TranscriptEntry::user("Test"))
            .await
            .unwrap();
        storage
            .append("session-3", &TranscriptEntry::assistant("Response"))
            .await
            .unwrap();

        let stats = storage.stats("session-3").await.unwrap();
        assert_eq!(stats.entry_count, 2);
        assert_eq!(stats.user_messages, 1);
        assert_eq!(stats.assistant_messages, 1);
    }

    #[tokio::test]
    async fn test_delete() {
        let (storage, _temp) = create_test_storage().await;

        storage
            .append("session-4", &TranscriptEntry::user("Test"))
            .await
            .unwrap();
        assert!(storage.transcript_path("session-4").exists());

        let deleted = storage.delete("session-4").await.unwrap();
        assert!(deleted);
        assert!(!storage.transcript_path("session-4").exists());
    }
}
