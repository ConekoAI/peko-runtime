//! Peko Session JSONL Format with Atomic Writes
//!
//! Implements durable JSONL sessions per `DATA_MODEL.md` §5:
//! - O(1) appends: each event is opened with `O_APPEND`, written in
//!   a single `write_all`, then `fsync` + per-process directory sync
//!   (mirrors the kimi-code `FileSystemAgentRecordPersistence` shape
//!   at `packages/agent-core/src/agent/records/persistence.ts:219-248`).
//!   Replaces the previous read-modify-rename pattern that was O(n)
//!   per append (`audit section 7 — Atomic write is O(n) per append`).
//! - Crash tolerance: a torn last line is filtered out by `load_events` /
//!   `load_normalized` (matches pi-mono's `parseSessionEntryLine`
//!   skip-unparseable approach). No `.tmp` files exist any more —
//!   `cleanup_temp_files` is kept as a no-op for backward compat and
//!   drops any leftover `.tmp` from a pre-F30 install.
//! - Support for Peko event format (13 event types)

use crate::common::types::message::LlmMessage;
use crate::session::events::SessionEvent;
use crate::session::safe_filename_component;
use anyhow::Result;
use chrono::{DateTime, Utc};
use peko_fs_persistence::{append_bytes_durable, FileLock};
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

    /// Append a Peko event to the session atomically
    pub async fn append_event(&self, session_id: &str, event: &SessionEvent) -> Result<()> {
        let path = self.session_path(session_id);
        let _lock = FileLock::acquire(&path, SESSION_LOCK_TIMEOUT_MS).await?;

        let json = serde_json::to_string(event)?;
        let line = json + "\n";

        // F30: O_APPEND + fsync + sync_dir — O(1) per append.
        Self::append_bytes(&path, line.as_bytes()).await?;

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
        // F30: single-shot create-and-write for the first line; no
        // tmp+rename dance needed since the file does not exist yet.
        Self::write_and_sync(&path, (json + "\n").as_bytes()).await?;

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
            Self::append_bytes(&path, (json + "\n").as_bytes()).await?;
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

        // F30: O_APPEND + fsync + sync_dir
        Self::append_bytes(&path, line.as_bytes()).await?;

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

        // F30: O_APPEND + fsync + sync_dir
        Self::append_bytes(&path, line.as_bytes()).await?;

        debug!(
            "Appended compaction #{} to session {}",
            compaction_number, session_id
        );
        Ok(entry_id)
    }

    /// Open a file in `O_APPEND` mode, creating it if missing, and
    /// return the fd ready for a single `write_all`. Mirrors
    /// kimi-code's `open(filePath, shouldClear ? "w" : "a")` shape
    /// (`packages/agent-core/src/agent/records/persistence.ts:219-248`).
    ///
    /// `O_APPEND` is atomic on POSIX under `PIPE_BUF` (4 KiB on
    /// Open a file in `O_WRONLY | O_CREAT | O_TRUNC` for one-shot
    /// create-and-write. Used by `create_session` for the first
    /// `SessionCreated` line; subsequent events use `append_line`.
    ///
    /// F30: replaces the previous `tmp + rename` "new file" branch
    /// of `atomic_write`. The single `writeFile` + `fsync` shape is
    /// durable on its own (no rename dance needed for a file that
    /// doesn't exist yet).
    async fn open_for_write(path: &Path) -> Result<fs::File> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }
        let file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)
            .await?;
        Ok(file)
    }

    /// Write `bytes` to `path` in a single `write_all` + `fsync`.
    /// `sync_dir` ensures the directory entry for `path` is durable
    /// across crashes (mirrors kimi-code's `syncDir(directory)` call).
    async fn write_and_sync(path: &Path, bytes: &[u8]) -> Result<()> {
        let mut file = Self::open_for_write(path).await?;
        file.write_all(bytes).await?;
        file.sync_all().await?;
        drop(file);
        if let Some(parent) = path.parent() {
            Self::sync_dir(parent).await?;
        }
        Ok(())
    }

    /// Append `bytes` to `path` in a single `write_all` + `fsync`.
    /// Caller is expected to hold `FileLock` for cross-process
    /// safety; in-process callers serialize via `Mutex` if needed.
    /// Delegates to `common::persistence::append_bytes_durable`, which
    /// owns the `O_APPEND` + `fsync` + directory-sync semantics shared
    /// with the chat-log shard writes.
    async fn append_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
        append_bytes_durable(path, bytes)
            .await
            .map_err(anyhow::Error::from)
    }

    /// Fsync a directory. On Linux this means opening the directory
    /// and calling `sync_all`; on macOS the equivalent is opening
    /// `..` from a child fd (the fd-based `fsync` on a directory fd
    /// is unreliable). On Windows `File::sync_all` on a directory
    /// returns `ERROR_INVALID_FUNCTION`; we swallow the error to
    /// preserve best-effort durability on Windows.
    async fn sync_dir(dir: &Path) -> Result<()> {
        match fs::File::open(dir).await {
            Ok(f) => {
                // Best-effort: some platforms return errors here.
                if let Err(e) = f.sync_all().await {
                    debug!(
                        "sync_dir({:?}) best-effort sync failed (non-fatal on this platform): {}",
                        dir, e
                    );
                }
            }
            Err(e) => {
                debug!("sync_dir({:?}) could not open dir (non-fatal): {}", dir, e);
            }
        }
        Ok(())
    }

    /// Load all Peko events from a session
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
            // Parse as Peko event
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
        use crate::common::types::message::MessageRole;
        use crate::session::events::SessionEvent::{SessionCreated, ToolResult};

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
                            if let crate::common::types::message::ContentBlock::ToolResult {
                                name,
                                ..
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

    /// Clean up partial `.tmp` files left over from a pre-F30 install.
    ///
    /// F30 switched from the `tmp + rename` write pattern to buffered
    /// `O_APPEND + fsync`, so this method no longer creates `.tmp`
    /// files in normal operation. It's kept as a one-shot sweep for
    /// upgrading installs and is a no-op when no `.tmp` files exist.
    /// Torn last lines are filtered out at read time by `load_events`
    /// / `load_normalized` (mirrors pi-mono's `parseSessionEntryLine`
    /// skip-unparseable approach).
    pub async fn cleanup_temp_files(&self, session_id: &str) -> Result<()> {
        let tmp_path = self.session_tmp_path(session_id);

        if tmp_path.exists() {
            warn!(
                "Found leftover tmp file from a pre-F30 install: {}. Removing.",
                tmp_path.display()
            );
            fs::remove_file(&tmp_path).await?;
        }

        Ok(())
    }

    /// Get session file path
    fn session_path(&self, session_id: &str) -> PathBuf {
        self.storage_dir
            .join(format!("{}.jsonl", safe_filename_component(session_id)))
    }

    /// Get session tmp file path (pre-F30 only — F30 doesn't create one).
    ///
    /// Still computed for the `cleanup_temp_files` sweep; once any
    /// pre-F30 install has been upgraded past a single startup, no
    /// `.tmp` files will ever exist again.
    fn session_tmp_path(&self, session_id: &str) -> PathBuf {
        self.storage_dir
            .join(format!("{}.tmp", safe_filename_component(session_id)))
    }

    /// Get index file path for a session
    #[must_use]
    pub fn index_path(&self, session_id: &str) -> PathBuf {
        self.storage_dir.join(format!(
            "{}.index.json",
            safe_filename_component(session_id)
        ))
    }

    /// Get context cache file path for a session (ADR-022)
    #[must_use]
    pub fn context_cache_path(&self, session_id: &str) -> PathBuf {
        self.storage_dir.join(format!(
            "{}.context.cache",
            safe_filename_component(session_id)
        ))
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
    async fn test_cleanup_temp_files_sweeps_pre_f30_install() {
        let temp = TempDir::new().unwrap();
        let storage = SessionStorage::new(temp.path().to_path_buf());

        // Simulate a leftover `.tmp` file from a pre-F30 install.
        let tmp_path = temp.path().join("test_session.tmp");
        fs::write(&tmp_path, "partial content").await.unwrap();
        assert!(tmp_path.exists());

        // The cleanup sweep should drop the leftover `.tmp` so it
        // doesn't shadow the live JSONL going forward.
        storage.cleanup_temp_files("test_session").await.unwrap();
        assert!(!tmp_path.exists());

        // Idempotent: a second call is a no-op.
        storage.cleanup_temp_files("test_session").await.unwrap();
        assert!(!tmp_path.exists());
    }

    /// F30 writes never leave a `.tmp` behind. Verify by writing a
    /// handful of events through the public API and checking the
    /// storage dir contains only the JSONL.
    #[tokio::test]
    async fn test_f30_writes_no_tmp() {
        let temp = TempDir::new().unwrap();
        let storage = SessionStorage::new(temp.path().to_path_buf());

        storage.create_session("f30_test", None).await.unwrap();

        // A single user message through `append_event`.
        let msg = crate::session::message::SessionMessage::user(
            "hello",
            crate::session::message::MessageSource::User,
        );
        storage
            .append_event(
                "f30_test",
                &crate::session::events::SessionEvent::MessageV2(msg),
            )
            .await
            .unwrap();

        let mut entries = tokio::fs::read_dir(temp.path()).await.unwrap();
        let mut names: Vec<String> = vec![];
        while let Some(entry) = entries.next_entry().await.unwrap() {
            names.push(entry.file_name().to_string_lossy().to_string());
        }
        names.sort();

        // Only the JSONL (no `.tmp` left over).
        assert_eq!(names, vec!["f30_test.jsonl".to_string()]);
    }

    /// F30's torn-line tolerance: a half-written last line must be
    /// silently filtered out by `load_events` (mirrors pi-mono's
    /// skip-unparseable approach).
    #[tokio::test]
    async fn test_f30_torn_last_line_filtered() {
        let temp = TempDir::new().unwrap();
        let storage = SessionStorage::new(temp.path().to_path_buf());

        storage.create_session("torn_test", None).await.unwrap();

        // Append a well-formed event, then simulate a crash mid-line
        // by writing a partial JSON blob to disk directly.
        let path = temp.path().join("torn_test.jsonl");
        let mut content = fs::read_to_string(&path).await.unwrap();
        content.push_str("{\"envelope\":{\"id\":\"half\",\"ts\":\"2026-07-20T");
        fs::write(&path, content).await.unwrap();

        // `load_events` must return exactly the events that were
        // fully written before the torn line.
        let events = storage.load_events("torn_test").await.unwrap();
        assert!(
            !events.is_empty(),
            "expected at least the SessionCreated event to survive the torn last line"
        );
        // No half-written event should appear in the returned list:
        // the torn envelope id "half" must not leak through.
        for e in &events {
            if let crate::session::events::SessionEvent::MessageV2(m) = e {
                assert_ne!(m.envelope.id, "half", "torn-line event leaked");
            }
        }
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
            crate::common::types::message::LlmMessage::system("You are a helpful assistant."),
            crate::common::types::message::LlmMessage::user("Hello"),
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
        assert_eq!(loaded[0].role, peko_providers::MessageRole::System);
        assert_eq!(loaded[1].role, peko_providers::MessageRole::User);
    }

    #[tokio::test]
    async fn test_context_cache_checksum_mismatch_returns_none() {
        let temp = TempDir::new().unwrap();
        let storage = SessionStorage::new(temp.path().to_path_buf());

        storage.create_session("cache_test", None).await.unwrap();

        let messages = vec![crate::common::types::message::LlmMessage::user("Hello")];

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

        let messages = vec![crate::common::types::message::LlmMessage::user("Hello")];

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

        let messages = vec![crate::common::types::message::LlmMessage::user("Hello")];

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
