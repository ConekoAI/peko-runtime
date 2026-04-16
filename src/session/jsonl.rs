//! Pekobot Session JSONL Format with Atomic Writes
//!
//! Implements durable JSONL sessions per DATA_MODEL.md §5:
//! - Atomic writes: events written to `.tmp` then renamed
//! - Automatic cleanup of partial `.tmp` files on load
//! - Support for Pekobot event format (13 event types)

use crate::session::events::SessionEvent;
use crate::session::lock::FileLock;
use anyhow::Result;
use chrono::{DateTime, Utc};
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tracing::{debug, info, warn};

/// Default lock timeout for session operations (10 seconds)
pub const SESSION_LOCK_TIMEOUT_MS: u64 = 10_000;

/// Normalized session entry for unified access
///
/// Provides a simplified view over session events for common use cases.
#[derive(Debug, Clone)]
pub enum NormalizedEntry {
    /// Session header/metadata
    Session {
        id: String,
        version: i32,
        timestamp: DateTime<Utc>,
        cwd: Option<String>,
    },
    /// User message
    UserMessage {
        id: String,
        content: String,
        timestamp: DateTime<Utc>,
        source: crate::session::events::MessageSource,
    },
    /// Assistant message
    AssistantMessage {
        id: String,
        content: String,
        timestamp: DateTime<Utc>,
        input_tokens: u32,
        output_tokens: u32,
    },
    /// System message
    SystemMessage {
        content: String,
        timestamp: DateTime<Utc>,
    },
    /// Tool result
    ToolResult {
        tool_call_id: String,
        tool_name: String,
        content: String,
        is_error: bool,
    },
    /// Compaction record
    Compaction {
        summary: String,
        messages_compacted: usize,
        tokens_before: usize,
        tokens_after: usize,
        compaction_number: usize,
        timestamp: DateTime<Utc>,
    },
    /// Model change
    ModelChange {
        provider: String,
        model_id: String,
        timestamp: DateTime<Utc>,
    },
    /// Custom/unknown entry
    Custom {
        custom_type: String,
        data: serde_json::Value,
    },
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

    /// Append a Pekobot event to the session atomically
    pub async fn append_event(&self, session_id: &str, event: &SessionEvent) -> Result<()> {
        let path = self.session_path(session_id);
        let _lock = FileLock::acquire(&path, SESSION_LOCK_TIMEOUT_MS).await?;

        let json = serde_json::to_string(event)?;
        let line = json + "\n";

        // Atomic append
        self.atomic_append(&path, &line).await?;

        Ok(())
    }

    /// Initialize a new session file with a SessionCreated event
    pub async fn create_session(&self, session_id: &str, cwd: Option<String>) -> Result<()> {
        use crate::session::events::{EventEnvelope, SessionCreatedEvent, SessionTrigger};

        // Ensure directory exists
        fs::create_dir_all(&self.storage_dir).await?;

        let path = self.session_path(session_id);

        // Create session created event
        let event = SessionEvent::SessionCreated(SessionCreatedEvent {
            envelope: EventEnvelope {
                id: format!("evt_{}", uuid::Uuid::new_v4().simple()),
                ts: Utc::now(),
            },
            instance_id: session_id.to_string(),
            image_digest: String::new(),
            parent_session_id: None,
            trigger: SessionTrigger::User,
        });

        let json = serde_json::to_string(&event)?;
        self.atomic_write(&path, json + "\n", false).await?;

        // Write cwd as a separate system event if provided
        if let Some(cwd_path) = cwd {
            use crate::session::events::SystemEvent;
            let cwd_event = SessionEvent::System(SystemEvent {
                envelope: EventEnvelope {
                    id: format!("evt_{}", uuid::Uuid::new_v4().simple()),
                    ts: Utc::now(),
                },
                event: "cwd".to_string(),
                detail: serde_json::json!({ "path": cwd_path }),
            });
            let json = serde_json::to_string(&cwd_event)?;
            self.atomic_append(&path, &(json + "\n")).await?;
        }

        info!("Created session: {}", session_id);
        Ok(())
    }

    /// Append a model change entry atomically
    pub async fn append_model_change(
        &self,
        session_id: &str,
        _parent_id: Option<String>,
        provider: &str,
        model_id: &str,
    ) -> Result<String> {
        use crate::session::events::{EventEnvelope, SystemEvent};

        let path = self.session_path(session_id);
        let _lock = FileLock::acquire(&path, SESSION_LOCK_TIMEOUT_MS).await?;

        let entry_id = format!("model_{}", uuid::Uuid::new_v4().simple());

        let event = SessionEvent::System(SystemEvent {
            envelope: EventEnvelope {
                id: entry_id.clone(),
                ts: Utc::now(),
            },
            event: "model_change".to_string(),
            detail: serde_json::json!({
                "provider": provider,
                "model_id": model_id,
            }),
        });

        let json = serde_json::to_string(&event)?;
        let line = json + "\n";

        // Atomic append
        self.atomic_append(&path, &line).await?;

        Ok(entry_id)
    }

    /// Append compaction entry atomically
    pub async fn append_compaction(
        &self,
        session_id: &str,
        _parent_id: Option<String>,
        summary: &str,
        messages_compacted: usize,
        tokens_before: usize,
        tokens_after: usize,
        compaction_number: usize,
    ) -> Result<String> {
        use crate::session::events::{EventEnvelope, SystemEvent};

        let path = self.session_path(session_id);
        let _lock = FileLock::acquire(&path, SESSION_LOCK_TIMEOUT_MS).await?;

        let entry_id = format!("compact_{}", uuid::Uuid::new_v4().simple());

        let event = SessionEvent::System(SystemEvent {
            envelope: EventEnvelope {
                id: entry_id.clone(),
                ts: Utc::now(),
            },
            event: "compaction".to_string(),
            detail: serde_json::json!({
                "summary": summary,
                "messages_compacted": messages_compacted,
                "tokens_before": tokens_before,
                "tokens_after": tokens_after,
                "compaction_number": compaction_number,
            }),
        });

        let json = serde_json::to_string(&event)?;
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
            // Parse as Pekobot event
            match serde_json::from_str::<SessionEvent>(line) {
                Ok(event) => {
                    events.push(event);
                }
                Err(e) => {
                    debug!("Failed to parse session event: {}", e);
                }
            }
        }

        Ok(events)
    }

    /// Load session normalizing Event Format entries
    ///
    /// This method provides a unified view over session data.
    pub async fn load_normalized(&self, session_id: &str) -> Result<Vec<NormalizedEntry>> {
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

            // Parse Event Format
            if let Ok(event) = serde_json::from_str::<SessionEvent>(line) {
                if let Some(entry) = Self::normalize_event(event) {
                    entries.push(entry);
                }
                continue;
            }

            // Unknown format - log warning
            warn!("Failed to parse session line: {}", line);
        }

        Ok(entries)
    }

    /// Convert Event Format to NormalizedEntry
    fn normalize_event(event: SessionEvent) -> Option<NormalizedEntry> {
        use crate::session::events::SessionEvent::*;
        use crate::types::message::MessageRole;

        // Try unified message conversion first
        if let Some(msg) = event.as_message() {
            let text = msg.text_content();
            let message_id = msg.message_id.clone();
            let timestamp = msg.envelope.ts;
            return match msg.role() {
                MessageRole::User => Some(NormalizedEntry::UserMessage {
                    id: message_id,
                    content: text,
                    timestamp,
                    source: msg
                        .source()
                        .unwrap_or(crate::session::events::MessageSource::User),
                }),
                MessageRole::Assistant => Some(NormalizedEntry::AssistantMessage {
                    id: message_id,
                    content: text,
                    timestamp,
                    input_tokens: msg.usage().map(|u| u.input_tokens).unwrap_or(0),
                    output_tokens: msg.usage().map(|u| u.output_tokens).unwrap_or(0),
                }),
                MessageRole::System => Some(NormalizedEntry::SystemMessage {
                    content: text,
                    timestamp,
                }),
                MessageRole::Tool => {
                    let tool_name = msg
                        .message
                        .content
                        .iter()
                        .find_map(|block| {
                            if let crate::types::message::ContentBlock::ToolResult {
                                name, ..
                            } = block
                            {
                                Some(name.clone())
                            } else {
                                None
                            }
                        })
                        .unwrap_or_default();
                    Some(NormalizedEntry::ToolResult {
                        tool_call_id: msg.tool_call_id().unwrap_or_default().to_string(),
                        tool_name,
                        content: text,
                        is_error: false,
                    })
                }
            };
        }

        // Handle non-message events
        match event {
            SessionCreated(e) => Some(NormalizedEntry::Session {
                id: e.envelope.id,
                version: 3,
                timestamp: e.envelope.ts,
                cwd: None,
            }),
            ToolResult(e) => Some(NormalizedEntry::ToolResult {
                tool_call_id: e.tool_call_id,
                tool_name: String::new(),
                content: e.output.unwrap_or_default(),
                is_error: e.error.is_some(),
            }),
            _ => {
                // Other event types can be added as needed
                debug!("Unnormalized event type: {}", event.event_type());
                None
            }
        }
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
    use crate::session::events::{EventEnvelope, SessionCreatedEvent, SessionTrigger};
    use chrono::Utc;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_load_events() {
        let temp = TempDir::new().unwrap();
        let storage = SessionStorage::new(temp.path().to_path_buf());

        // Create a session file with a SessionCreated event
        let event = SessionEvent::SessionCreated(SessionCreatedEvent {
            envelope: EventEnvelope {
                id: "test-1".to_string(),
                ts: Utc::now(),
            },
            instance_id: "instance-1".to_string(),
            image_digest: "sha256:abc".to_string(),
            parent_session_id: None,
            trigger: SessionTrigger::User,
        });

        // Write event directly to file
        let path = temp.path().join("test_session.jsonl");
        let json = serde_json::to_string(&event).unwrap();
        fs::write(&path, json + "\n").await.unwrap();

        // Load events
        let events = storage.load_events("test_session").await.unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], SessionEvent::SessionCreated(_)));
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

        // Create source session file
        let event = SessionEvent::SessionCreated(SessionCreatedEvent {
            envelope: EventEnvelope {
                id: "test-1".to_string(),
                ts: Utc::now(),
            },
            instance_id: "instance-1".to_string(),
            image_digest: "sha256:abc".to_string(),
            parent_session_id: None,
            trigger: SessionTrigger::User,
        });
        let path = temp.path().join("source.jsonl");
        let json = serde_json::to_string(&event).unwrap();
        fs::write(&path, json + "\n").await.unwrap();

        // Copy session
        storage.copy_session("source", "target").await.unwrap();

        // Verify copy
        let source_events = storage.load_events("source").await.unwrap();
        let target_events = storage.load_events("target").await.unwrap();
        assert_eq!(source_events.len(), target_events.len());
    }

    #[tokio::test]
    async fn test_delete_session() {
        let temp = TempDir::new().unwrap();
        let storage = SessionStorage::new(temp.path().to_path_buf());

        // Create session file
        let path = temp.path().join("to_delete.jsonl");
        fs::write(&path, "{}").await.unwrap();
        assert!(storage.session_exists("to_delete").await);

        storage.delete_session("to_delete").await.unwrap();
        assert!(!storage.session_exists("to_delete").await);
    }
}
