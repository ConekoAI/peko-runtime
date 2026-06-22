//! Pekobot Session JSONL Format with Atomic Writes
//!
//! Implements durable JSONL sessions per `DATA_MODEL.md` §5:
//! - Atomic writes: events written to `.tmp` then renamed
//! - Automatic cleanup of partial `.tmp` files on load
//! - Support for Pekobot event format (13 event types)

use crate::session::events::SessionEvent;
use crate::session::lock::FileLock;
use crate::types::message::LlmMessage;
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

    /// Initialize a new session file with a `SessionCreated` event
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
        details: Option<&crate::session::compaction::summary_format::CompactionDetails>,
    ) -> Result<String> {
        use crate::session::events::{EventEnvelope, SystemEvent};

        let path = self.session_path(session_id);
        let _lock = FileLock::acquire(&path, SESSION_LOCK_TIMEOUT_MS).await?;

        let entry_id = format!("compact_{}", uuid::Uuid::new_v4().simple());

        let mut detail = serde_json::json!({
            "summary": summary,
            "messages_compacted": messages_compacted,
            "tokens_before": tokens_before,
            "tokens_after": tokens_after,
            "compaction_number": compaction_number,
        });

        // Include file operations details if present
        if let Some(d) = details {
            if let serde_json::Value::Object(ref mut map) = detail {
                map.insert("details".to_string(), serde_json::to_value(d)?);
            }
        }

        let event = SessionEvent::System(SystemEvent {
            envelope: EventEnvelope {
                id: entry_id.clone(),
                ts: Utc::now(),
            },
            event: "compaction".to_string(),
            detail,
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

    /// Convert Event Format to `NormalizedEntry`
    fn normalize_event(event: SessionEvent) -> Option<NormalizedEntry> {
        use crate::session::events::SessionEvent::{SessionCreated, ToolResult};
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
                    input_tokens: msg.usage().map_or(0, |u| u.input as u32),
                    output_tokens: msg.usage().map_or(0, |u| u.output as u32),
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
            crate::session::events::SessionEvent::System(sys_event) => {
                match sys_event.event.as_str() {
                    "compaction" => {
                        let detail = &sys_event.detail;
                        Some(NormalizedEntry::Compaction {
                            summary: detail
                                .get("summary")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            messages_compacted: detail
                                .get("messages_compacted")
                                .and_then(|v| v.as_u64())
                                .map(|n| n as usize)
                                .unwrap_or(0),
                            tokens_before: detail
                                .get("tokens_before")
                                .and_then(|v| v.as_u64())
                                .map(|n| n as usize)
                                .unwrap_or(0),
                            tokens_after: detail
                                .get("tokens_after")
                                .and_then(|v| v.as_u64())
                                .map(|n| n as usize)
                                .unwrap_or(0),
                            compaction_number: detail
                                .get("compaction_number")
                                .and_then(|v| v.as_u64())
                                .map(|n| n as usize)
                                .unwrap_or(0),
                            timestamp: sys_event.envelope.ts,
                        })
                    }
                    "model_change" => {
                        let detail = &sys_event.detail;
                        Some(NormalizedEntry::ModelChange {
                            provider: detail
                                .get("provider")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            model_id: detail
                                .get("model_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            timestamp: sys_event.envelope.ts,
                        })
                    }
                    _ => {
                        debug!("Unnormalized system event: {}", sys_event.event);
                        None
                    }
                }
            }
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
    #[must_use]
    pub fn index_path(&self, session_id: &str) -> PathBuf {
        self.storage_dir.join(format!("{session_id}.index.json"))
    }

    /// Get context cache file path for a session (ADR-022)
    #[must_use]
    pub fn context_cache_path(&self, session_id: &str) -> PathBuf {
        self.storage_dir.join(format!("{session_id}.context.cache"))
    }

    // ============================================================
    // Context Cache (Derived, Discardable) — ADR-022 Phase 2
    // ============================================================

    /// Write the context cache for a session.
    ///
    /// The cache is a derived file that can be rebuilt from the JSONL at any time.
    /// Format:
    /// ```text
    /// # peko-context-cache v1
    /// # checksum: <blake3 of jsonl content>
    /// # entries: <number of jsonl entries>
    /// <json array of ChatMessage>
    /// ```
    pub async fn write_context_cache(
        &self,
        session_id: &str,
        messages: &[LlmMessage],
        jsonl_checksum: &str,
        entry_count: usize,
    ) -> Result<()> {
        let cache_path = self.context_cache_path(session_id);
        let _lock = FileLock::acquire(&cache_path, SESSION_LOCK_TIMEOUT_MS).await?;

        let header = format!(
            "# peko-context-cache v1\n# checksum: {}\n# entries: {}\n",
            jsonl_checksum, entry_count
        );
        let messages_json = serde_json::to_string(messages)?;
        let content = header + &messages_json + "\n";

        let temp_path = cache_path.with_extension("cache.tmp");
        let mut file = fs::File::create(&temp_path).await?;
        file.write_all(content.as_bytes()).await?;
        file.flush().await?;
        drop(file);

        fs::rename(&temp_path, &cache_path).await?;
        debug!(
            "Wrote context cache for {} ({} messages, checksum: {})",
            session_id,
            messages.len(),
            jsonl_checksum
        );
        Ok(())
    }

    /// Load the context cache for a session if it is valid.
    ///
    /// Returns `Ok(Some(messages))` if the cache exists and its checksum/entry count
    /// matches the current JSONL. Returns `Ok(None)` if the cache is stale or missing.
    pub async fn load_context_cache(
        &self,
        session_id: &str,
        expected_checksum: &str,
        expected_entry_count: usize,
    ) -> Result<Option<Vec<LlmMessage>>> {
        let cache_path = self.context_cache_path(session_id);

        if !cache_path.exists() {
            return Ok(None);
        }

        let _lock = FileLock::acquire(&cache_path, SESSION_LOCK_TIMEOUT_MS).await?;
        let content = fs::read_to_string(&cache_path).await?;

        // Parse header lines
        let mut lines = content.lines();
        let version_line = lines.next();
        let checksum_line = lines.next();
        let entries_line = lines.next();

        // Validate version
        if version_line != Some("# peko-context-cache v1") {
            warn!("Context cache version mismatch for {}", session_id);
            return Ok(None);
        }

        // Validate checksum
        let actual_checksum = checksum_line
            .and_then(|l| l.strip_prefix("# checksum: "))
            .unwrap_or("");
        if actual_checksum != expected_checksum {
            debug!(
                "Context cache checksum mismatch for {} (expected {}, got {})",
                session_id, expected_checksum, actual_checksum
            );
            return Ok(None);
        }

        // Validate entry count
        let actual_entries = entries_line
            .and_then(|l| l.strip_prefix("# entries: "))
            .and_then(|n| n.parse::<usize>().ok())
            .unwrap_or(0);
        if actual_entries != expected_entry_count {
            debug!(
                "Context cache entry count mismatch for {} (expected {}, got {})",
                session_id, expected_entry_count, actual_entries
            );
            return Ok(None);
        }

        // Parse messages JSON (remaining content after header lines)
        let json_str: String = lines.collect::<Vec<_>>().join("\n");
        let messages: Vec<LlmMessage> = serde_json::from_str(&json_str)?;

        debug!(
            "Loaded valid context cache for {} ({} messages)",
            session_id,
            messages.len()
        );
        Ok(Some(messages))
    }

    /// Delete the context cache for a session (e.g., after external modification).
    pub async fn invalidate_context_cache(&self, session_id: &str) -> Result<()> {
        let cache_path = self.context_cache_path(session_id);
        if cache_path.exists() {
            fs::remove_file(&cache_path).await?;
            debug!("Invalidated context cache for {}", session_id);
        }
        Ok(())
    }

    /// Compute a simple checksum (blake3 hash) of the JSONL file content.
    pub async fn compute_jsonl_checksum(&self, session_id: &str) -> Result<String> {
        let path = self.session_path(session_id);
        if !path.exists() {
            return Ok("empty".to_string());
        }
        let content = fs::read_to_string(&path).await?;
        let hash = blake3::hash(content.as_bytes());
        Ok(hash.to_string())
    }

    /// Count the number of entries (non-empty lines) in the JSONL file.
    pub async fn count_jsonl_entries(&self, session_id: &str) -> Result<usize> {
        let path = self.session_path(session_id);
        if !path.exists() {
            return Ok(0);
        }
        let content = fs::read_to_string(&path).await?;
        Ok(content.lines().filter(|l| !l.trim().is_empty()).count())
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

    /// Copy a session file (for branching)
    pub async fn copy_session(&self, source_id: &str, target_id: &str) -> Result<()> {
        let source_path = self.session_path(source_id);
        let target_path = self.session_path(target_id);

        if !source_path.exists() {
            return Err(anyhow::anyhow!("Source session {source_id} does not exist"));
        }

        fs::copy(&source_path, &target_path).await?;

        info!("Copied session {} to {}", source_id, target_id);
        Ok(())
    }

    /// Delete a session file and its derived cache
    pub async fn delete_session(&self, session_id: &str) -> Result<()> {
        let path = self.session_path(session_id);
        let index_path = self.index_path(session_id);
        let cache_path = self.context_cache_path(session_id);

        if path.exists() {
            fs::remove_file(&path).await?;
        }

        if index_path.exists() {
            fs::remove_file(&index_path).await?;
        }

        if cache_path.exists() {
            fs::remove_file(&cache_path).await?;
        }

        info!("Deleted session: {}", session_id);
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

    // ============================================================
    // ADR-022: Context Cache Tests
    // ============================================================

    #[tokio::test]
    async fn test_context_cache_roundtrip() {
        let temp = TempDir::new().unwrap();
        let storage = SessionStorage::new(temp.path().to_path_buf());

        // Create a session with some events
        storage.create_session("cache_test", None).await.unwrap();

        let messages = vec![
            crate::types::message::LlmMessage::system("You are a helpful assistant."),
            crate::types::message::LlmMessage::user("Hello"),
        ];

        let checksum = storage.compute_jsonl_checksum("cache_test").await.unwrap();
        let entry_count = storage.count_jsonl_entries("cache_test").await.unwrap();

        // Write cache
        storage
            .write_context_cache("cache_test", &messages, &checksum, entry_count)
            .await
            .unwrap();

        // Load cache with matching checksum/entries
        let loaded = storage
            .load_context_cache("cache_test", &checksum, entry_count)
            .await
            .unwrap();

        assert!(loaded.is_some());
        let loaded = loaded.unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].role, crate::providers::MessageRole::System);
        assert_eq!(loaded[1].role, crate::providers::MessageRole::User);
    }

    #[tokio::test]
    async fn test_context_cache_checksum_mismatch_returns_none() {
        let temp = TempDir::new().unwrap();
        let storage = SessionStorage::new(temp.path().to_path_buf());

        storage.create_session("cache_test", None).await.unwrap();

        let messages = vec![crate::types::message::LlmMessage::user("Hello")];

        let checksum = storage.compute_jsonl_checksum("cache_test").await.unwrap();
        let entry_count = storage.count_jsonl_entries("cache_test").await.unwrap();

        storage
            .write_context_cache("cache_test", &messages, &checksum, entry_count)
            .await
            .unwrap();

        // Load with wrong checksum
        let loaded = storage
            .load_context_cache("cache_test", "wrong_checksum", entry_count)
            .await
            .unwrap();

        assert!(loaded.is_none());
    }

    #[tokio::test]
    async fn test_context_cache_entry_count_mismatch_returns_none() {
        let temp = TempDir::new().unwrap();
        let storage = SessionStorage::new(temp.path().to_path_buf());

        storage.create_session("cache_test", None).await.unwrap();

        let messages = vec![crate::types::message::LlmMessage::user("Hello")];

        let checksum = storage.compute_jsonl_checksum("cache_test").await.unwrap();
        let entry_count = storage.count_jsonl_entries("cache_test").await.unwrap();

        storage
            .write_context_cache("cache_test", &messages, &checksum, entry_count)
            .await
            .unwrap();

        // Load with wrong entry count
        let loaded = storage
            .load_context_cache("cache_test", &checksum, entry_count + 1)
            .await
            .unwrap();

        assert!(loaded.is_none());
    }

    #[tokio::test]
    async fn test_context_cache_missing_returns_none() {
        let temp = TempDir::new().unwrap();
        let storage = SessionStorage::new(temp.path().to_path_buf());

        let loaded = storage
            .load_context_cache("nonexistent", "checksum", 0)
            .await
            .unwrap();

        assert!(loaded.is_none());
    }

    #[tokio::test]
    async fn test_invalidate_context_cache() {
        let temp = TempDir::new().unwrap();
        let storage = SessionStorage::new(temp.path().to_path_buf());

        storage.create_session("cache_test", None).await.unwrap();

        let messages = vec![crate::types::message::LlmMessage::user("Hello")];

        let checksum = storage.compute_jsonl_checksum("cache_test").await.unwrap();
        let entry_count = storage.count_jsonl_entries("cache_test").await.unwrap();

        storage
            .write_context_cache("cache_test", &messages, &checksum, entry_count)
            .await
            .unwrap();

        assert!(storage.context_cache_path("cache_test").exists());

        storage
            .invalidate_context_cache("cache_test")
            .await
            .unwrap();

        assert!(!storage.context_cache_path("cache_test").exists());
    }

    #[tokio::test]
    async fn test_compute_jsonl_checksum() {
        let temp = TempDir::new().unwrap();
        let storage = SessionStorage::new(temp.path().to_path_buf());

        // Empty/nonexistent session
        let checksum1 = storage.compute_jsonl_checksum("no_session").await.unwrap();
        assert_eq!(checksum1, "empty");

        // After creating session
        storage.create_session("checksum_test", None).await.unwrap();
        let checksum2 = storage
            .compute_jsonl_checksum("checksum_test")
            .await
            .unwrap();
        assert_ne!(checksum2, "empty");

        // Checksum should be stable for same content
        let checksum3 = storage
            .compute_jsonl_checksum("checksum_test")
            .await
            .unwrap();
        assert_eq!(checksum2, checksum3);
    }

    #[tokio::test]
    async fn test_count_jsonl_entries() {
        let temp = TempDir::new().unwrap();
        let storage = SessionStorage::new(temp.path().to_path_buf());

        assert_eq!(storage.count_jsonl_entries("no_session").await.unwrap(), 0);

        storage.create_session("count_test", None).await.unwrap();
        // SessionCreated + optional cwd — create_session may write 1 or 2 lines
        let count = storage.count_jsonl_entries("count_test").await.unwrap();
        assert!(count >= 1);
    }
}
