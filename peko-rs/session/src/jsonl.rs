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

use crate::events::SessionEvent;
use crate::key::safe_filename_component;
use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use peko_fs_persistence::{append_bytes_durable, FileLock};
use peko_message::LlmMessage;
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tracing::{debug, info, warn};

/// Default lock timeout for session operations (10 seconds)
pub const SESSION_LOCK_TIMEOUT_MS: u64 = 10_000;

/// Reason a rotation was triggered. Stored on `RotationOutcome` so
/// tests / observability can distinguish future causes (cron pressure,
/// explicit user request, etc.) from the initial size-driven path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RotationReason {
    /// The session's JSONL crossed `test_config::rotate_bytes()`
    /// during an append.
    SizeExceeded,
}

/// Outcome of `SessionStorage::append_event_with_rotation`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RotationOutcome {
    /// No rotation happened — the event was appended to the current
    /// page as in `append_event`.
    NoRotation,
    /// Rotation happened; the previous content of
    /// `<session_id>.jsonl` was paged to `<session_id>.<page_n>.jsonl`
    /// and the event was appended to a fresh `<session_id>.jsonl`.
    /// The session id is stable across paging.
    Paged { session_id: String, page_n: u32 },
}

/// WS2 (implicit session management): callback invoked from
/// `SessionStorage::append_event_with_rotation` when the live
/// session's JSONL crosses the size threshold and the current page
/// needs to be renamed aside to a `<id>.<n>.jsonl` page. The default
/// impl is a panic — `append_event_with_rotation` requires an explicit
/// sink so production wiring can't accidentally bypass rotation.
#[async_trait]
pub trait RotationSink: Send + Sync {
    /// Page the current transcript in place (rename `<S>.jsonl` →
    /// `<S>.<n>.jsonl`, leaving `<S>.jsonl` free for the next append)
    /// and return the page number used.
    async fn request_rotation(&self, session_id: &str, reason: RotationReason) -> Result<u32>;
}

/// Scan `dir` for page files of `session_id` (`<S>.<n>.jsonl`) and
/// return the page numbers sorted ascending (1 = oldest). Non-numeric
/// suffixes — including legacy `<S>#<UTC-ts>.jsonl` chapter files —
/// are ignored.
///
/// Note: a literal session id ending in `.<digits>` would be
/// indistinguishable from a page file; real ids (session keys, uuids)
/// never take that shape.
#[must_use]
pub fn page_numbers(dir: &Path, session_id: &str) -> Vec<u32> {
    let prefix = format!("{}.", safe_filename_component(session_id));
    let mut numbers = vec![];
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some(rest) = name
                .strip_prefix(&prefix)
                .and_then(|r| r.strip_suffix(".jsonl"))
            else {
                continue;
            };
            if let Ok(n) = rest.parse::<u32>() {
                numbers.push(n);
            }
        }
    }
    numbers.sort_unstable();
    numbers
}

/// Path of page `n` for `session_id` in `dir` (`<S>.<n>.jsonl`).
#[must_use]
pub fn page_path(dir: &Path, session_id: &str, n: u32) -> PathBuf {
    dir.join(format!(
        "{}.{}.jsonl",
        safe_filename_component(session_id),
        n
    ))
}

/// Whether a `.jsonl` file name is a numbered page (`<id>.<n>.jsonl`)
/// rather than a live transcript. Legacy `<id>#<ts>.jsonl` chapter
/// files are not pages (non-numeric suffix).
fn is_page_file_name(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".jsonl") else {
        return false;
    };
    let Some((_, suffix)) = stem.rsplit_once('.') else {
        return false;
    };
    !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit())
}

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
        source: crate::events::MessageSource,
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

/// A single match from [`SessionStorage::search_transcripts`].
#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptSearchHit {
    /// Session the match was found in
    pub session_id: String,
    /// Role of the matching message ("user" or "assistant")
    pub role: String,
    /// Timestamp of the matching message
    pub timestamp: DateTime<Utc>,
    /// ~160 chars of message text centered on the match
    pub snippet: String,
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

    /// Append a Peko event to the session, paging the current JSONL
    /// aside to `<id>.<n>.jsonl` first if the live file would exceed
    /// `test_config::rotate_bytes()` after the append.
    ///
    /// WS2 (implicit session management): the engine keeps the live
    /// session finite by auto-paging. The session id is stable — the
    /// flow:
    ///
    /// 1. Acquire the `FileLock` for the live file (matches
    ///    `append_event`'s cross-process serialisation).
    /// 2. `fs::metadata(&path).len()` under the lock is
    ///    authoritative — concurrent writers can't race past us.
    /// 3. If `size + serialized_len > rotate_bytes()`, call
    ///    `sink.request_rotation(session_id, SizeExceeded)`. The
    ///    sink is responsible for the locked rename of `<id>.jsonl`
    ///    to the next `<id>.<n>.jsonl` page plus context-cache
    ///    invalidation (see `manager::SessionManagerRotationSink`).
    /// 4. Drop the lock, re-acquire on the (now free) live path,
    ///    and append — this recreates a fresh `<id>.jsonl`.
    /// 5. Return `RotationOutcome::Paged { session_id, page_n }` so
    ///    callers can invalidate any in-memory caches keyed by
    ///    `session_id`.
    ///
    /// Returns `RotationOutcome::NoRotation` when the append fits in
    /// the existing file.
    pub async fn append_event_with_rotation(
        &self,
        session_id: &str,
        event: &SessionEvent,
        sink: &dyn RotationSink,
    ) -> Result<RotationOutcome> {
        let path = self.session_path(session_id);
        let json = serde_json::to_string(event)?;
        let line = json + "\n";
        let threshold = crate::test_config::rotate_bytes();

        // Lock the source file once for the size check + append.
        let _lock = FileLock::acquire(&path, SESSION_LOCK_TIMEOUT_MS).await?;
        let current_size = match fs::metadata(&path).await {
            Ok(meta) => meta.len() as usize,
            // File may not exist yet — treat as zero. The append
            // below will create it.
            Err(_) => 0,
        };

        if current_size + line.len() <= threshold {
            Self::append_bytes(&path, line.as_bytes()).await?;
            // Lock released on drop.
            return Ok(RotationOutcome::NoRotation);
        }

        // Over threshold — drop the live-file lock so the sink's page
        // rename (which acquires a fresh FileLock on the same path)
        // doesn't see us as holding the lock during its rename. The
        // sink renames `<id>.jsonl` aside to `<id>.<n>.jsonl`; once it
        // returns we re-acquire on the live path and recreate it.
        drop(_lock);

        let page_n = sink
            .request_rotation(session_id, RotationReason::SizeExceeded)
            .await?;

        let _new_lock = FileLock::acquire(&path, SESSION_LOCK_TIMEOUT_MS).await?;
        Self::append_bytes(&path, line.as_bytes()).await?;

        Ok(RotationOutcome::Paged {
            session_id: session_id.to_string(),
            page_n,
        })
    }

    /// Initialize a new session file with a `SessionCreated` event
    pub async fn create_session(&self, session_id: &str, cwd: Option<String>) -> Result<()> {
        self.create_session_with_header(session_id, cwd, crate::events::SessionTrigger::User, None)
            .await
    }

    /// Initialize a new session file with an explicit `SessionCreated`
    /// header (trigger + parent linkage). The metadata/index carry these
    /// values already; the JSONL header must agree — a spawn session
    /// whose header says `trigger: "user"` contradicts the index's
    /// `"spawn"` (2026-08-07 field test, F5).
    pub async fn create_session_with_header(
        &self,
        session_id: &str,
        cwd: Option<String>,
        trigger: crate::events::SessionTrigger,
        parent_session_id: Option<String>,
    ) -> Result<()> {
        use crate::events::{EventEnvelope, SessionCreatedEvent};

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
            parent_session_id,
            trigger,
        });

        let json = serde_json::to_string(&event)?;
        // F30: single-shot create-and-write for the first line; no
        // tmp+rename dance needed since the file does not exist yet.
        Self::write_and_sync(&path, (json + "\n").as_bytes()).await?;

        // Write cwd as a separate system event if provided
        if let Some(cwd_path) = cwd {
            use crate::events::SystemEvent;
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
        use crate::events::{EventEnvelope, SystemEvent};

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
        details: Option<&crate::compaction::summary_format::CompactionDetails>,
    ) -> Result<String> {
        use crate::events::{EventEnvelope, SystemEvent};

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

    /// Ordered transcript paths for `session_id`: pages `1..=N`
    /// (oldest first) followed by the current page. Missing files are
    /// skipped.
    fn transcript_paths(&self, session_id: &str) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = page_numbers(&self.storage_dir, session_id)
            .into_iter()
            .map(|n| page_path(&self.storage_dir, session_id, n))
            .filter(|p| p.exists())
            .collect();
        let current = self.session_path(session_id);
        if current.exists() {
            paths.push(current);
        }
        paths
    }

    /// Load all Peko events from a session
    ///
    /// Reads the paged transcript (`<id>.1.jsonl` … `<id>.N.jsonl`,
    /// oldest first) stitched with the current page (`<id>.jsonl`), so
    /// callers see the full history in chronological order regardless
    /// of how many rotations happened.
    ///
    /// Also cleans up any partial .tmp files that may exist from crashes.
    pub async fn load_events(&self, session_id: &str) -> Result<Vec<SessionEvent>> {
        // Clean up any partial tmp files from previous crashes
        self.cleanup_temp_files(session_id).await?;

        let paths = self.transcript_paths(session_id);
        if paths.is_empty() {
            return Ok(vec![]);
        }

        // Acquire lock on the current page to ensure a consistent
        // read. Pages are immutable once created, so they need no
        // lock of their own.
        let _lock =
            FileLock::acquire(self.session_path(session_id), SESSION_LOCK_TIMEOUT_MS).await?;

        let mut events = vec![];

        for path in paths {
            let content = fs::read_to_string(&path).await?;
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
        }

        Ok(events)
    }

    /// Load session normalizing Event Format entries
    ///
    /// This method provides a unified view over session data. Like
    /// `load_events`, it stitches paged transcripts (`<id>.N.jsonl`
    /// pages then the current page) into one chronological view.
    pub async fn load_normalized(&self, session_id: &str) -> Result<Vec<NormalizedEntry>> {
        // Clean up any partial tmp files from previous crashes
        self.cleanup_temp_files(session_id).await?;

        let paths = self.transcript_paths(session_id);
        if paths.is_empty() {
            return Ok(vec![]);
        }

        // Acquire lock on the current page (pages are immutable).
        let _lock =
            FileLock::acquire(self.session_path(session_id), SESSION_LOCK_TIMEOUT_MS).await?;

        let mut entries = vec![];

        for path in paths {
            let content = fs::read_to_string(&path).await?;
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
        }

        Ok(entries)
    }

    /// Convert Event Format to `NormalizedEntry`
    fn normalize_event(event: SessionEvent) -> Option<NormalizedEntry> {
        use crate::events::SessionEvent::{SessionCreated, ToolResult};
        use peko_message::MessageRole;

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
                    source: msg.source().unwrap_or(crate::events::MessageSource::User),
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
                            if let peko_message::ContentBlock::ToolResult { name, .. } = block {
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
            crate::events::SessionEvent::System(sys_event) => match sys_event.event.as_str() {
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
            },
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

    /// Check if session exists (current page or any numbered page)
    pub async fn session_exists(&self, session_id: &str) -> bool {
        self.session_path(session_id).exists()
            || !page_numbers(&self.storage_dir, session_id).is_empty()
    }

    /// List all sessions
    ///
    /// Numbered page files (`<id>.<n>.jsonl`) are storage-internal and
    /// excluded — the session shows up once, under its stable id.
    pub async fn list_sessions(&self) -> Result<Vec<String>> {
        let mut sessions = vec![];

        if self.storage_dir.exists() {
            let mut entries = fs::read_dir(&self.storage_dir).await?;
            while let Some(entry) = entries.next_entry().await? {
                if let Some(name) = entry.file_name().to_str() {
                    if name.ends_with(".jsonl") && !is_page_file_name(name) {
                        sessions.push(name.trim_end_matches(".jsonl").to_string());
                    }
                }
            }
        }

        sessions.sort_by(|a, b| b.cmp(a)); // Newest first
        Ok(sessions)
    }

    /// Copy a session file (for branching)
    ///
    /// Copies the current page plus every numbered page, preserving
    /// relative names under the target id.
    pub async fn copy_session(&self, source_id: &str, target_id: &str) -> Result<()> {
        let source_path = self.session_path(source_id);
        let target_path = self.session_path(target_id);

        if !source_path.exists() && page_numbers(&self.storage_dir, source_id).is_empty() {
            return Err(anyhow::anyhow!("Source session {source_id} does not exist"));
        }

        // Hold the source's append lock for the duration of the copy so
        // the branch gets a consistent snapshot — a concurrent
        // `append_event` blocks instead of landing mid-copy.
        let _lock = FileLock::acquire(&source_path, SESSION_LOCK_TIMEOUT_MS).await?;

        for n in page_numbers(&self.storage_dir, source_id) {
            let page = page_path(&self.storage_dir, source_id, n);
            fs::copy(&page, page_path(&self.storage_dir, target_id, n)).await?;
        }
        if source_path.exists() {
            fs::copy(&source_path, &target_path).await?;
        }

        info!("Copied session {} to {}", source_id, target_id);
        Ok(())
    }

    /// Delete a session file, all its numbered pages, and its derived cache
    pub async fn delete_session(&self, session_id: &str) -> Result<()> {
        let path = self.session_path(session_id);
        let index_path = self.index_path(session_id);
        let cache_path = self.context_cache_path(session_id);

        // Hold the same cross-process lock `append_event` uses so a
        // concurrent append (`O_CREAT | O_APPEND`) cannot recreate an
        // orphan JSONL in the middle of the delete.
        let _lock = FileLock::acquire(&path, SESSION_LOCK_TIMEOUT_MS).await?;

        if path.exists() {
            fs::remove_file(&path).await?;
        }

        for n in page_numbers(&self.storage_dir, session_id) {
            let page = page_path(&self.storage_dir, session_id, n);
            if page.exists() {
                fs::remove_file(&page).await?;
            }
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

    /// Case-insensitive substring scan over session transcripts.
    ///
    /// Scans the text content of user/assistant messages in each listed
    /// session's JSONL (tool-call JSON noise is skipped — only
    /// `ContentBlock::Text` content is matched). The caller supplies the
    /// (ownership-scoped) session id list. Sessions that fail to load
    /// are skipped with a warning rather than failing the whole search.
    /// Scanning stops after `max_hits` hits.
    pub async fn search_transcripts(
        &self,
        ids: &[String],
        needle: &str,
        max_hits: usize,
    ) -> Result<Vec<TranscriptSearchHit>> {
        let mut hits = Vec::new();
        if needle.is_empty() || max_hits == 0 {
            return Ok(hits);
        }
        let needle_lower = needle.to_lowercase();

        'sessions: for session_id in ids {
            let events = match self.load_events(session_id).await {
                Ok(events) => events,
                Err(e) => {
                    warn!(
                        "search_transcripts: skipping session {} that failed to load: {}",
                        session_id, e
                    );
                    continue;
                }
            };

            for event in &events {
                let Some(msg) = event.as_message() else {
                    continue;
                };
                let role = match msg.role() {
                    peko_message::MessageRole::User => "user",
                    peko_message::MessageRole::Assistant => "assistant",
                    _ => continue,
                };

                let text = msg.text_content();
                let Some(match_start) = text.to_lowercase().find(&needle_lower) else {
                    continue;
                };

                hits.push(TranscriptSearchHit {
                    session_id: session_id.clone(),
                    role: role.to_string(),
                    timestamp: msg.envelope.ts,
                    snippet: Self::match_snippet(&text, match_start, needle_lower.len()),
                });

                if hits.len() >= max_hits {
                    break 'sessions;
                }
            }
        }

        Ok(hits)
    }

    /// Extract a ~160-char snippet centered on the match at
    /// `[match_start, match_start + match_len)` (byte offsets snapped
    /// to char boundaries), marking truncated ends with `…`.
    fn match_snippet(text: &str, match_start: usize, match_len: usize) -> String {
        const RADIUS: usize = 80;

        let floor_boundary = |mut i: usize| {
            while i > 0 && !text.is_char_boundary(i) {
                i -= 1;
            }
            i
        };
        let ceil_boundary = |mut i: usize| {
            while i < text.len() && !text.is_char_boundary(i) {
                i += 1;
            }
            i
        };

        let start = floor_boundary(match_start.saturating_sub(RADIUS));
        let end = ceil_boundary((match_start + match_len + RADIUS).min(text.len()));

        let mut snippet = String::new();
        if start > 0 {
            snippet.push('…');
        }
        snippet.push_str(&text[start..end]);
        if end < text.len() {
            snippet.push('…');
        }
        snippet
    }
}

#[cfg(test)]
mod tests {
    use crate::events::{EventEnvelope, SessionCreatedEvent, SessionTrigger};
    use crate::jsonl::RotationOutcome;
    use crate::*;
    use chrono::Utc;
    use std::path::PathBuf;
    use tempfile::TempDir;
    use tokio::fs;

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

    /// RAII guard that enables `PEKO_TEST_MODE` for the duration of a
    /// test and restores the previous state on drop. Used by the
    /// WS2 rotation tests so the `test_config` default-value tests
    /// don't see leaked env state.
    ///
    /// Holds `crate::test_config::TEST_MODE_LOCK` so two guard-holding
    /// tests can't run concurrently (the env var is process-global).
    struct PekoTestModeGuard {
        prev: Option<String>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }
    impl PekoTestModeGuard {
        fn new() -> Self {
            let lock = crate::test_config::TEST_MODE_LOCK
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let prev = std::env::var("PEKO_TEST_MODE").ok();
            std::env::set_var("PEKO_TEST_MODE", "1");
            Self { prev, _lock: lock }
        }
    }
    impl Drop for PekoTestModeGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var("PEKO_TEST_MODE", v),
                None => std::env::remove_var("PEKO_TEST_MODE"),
            }
        }
    }

    /// WS2 (implicit session management): when the file fits under the
    /// rotation threshold the sink is never consulted and the event
    /// lands in the original file.
    #[tokio::test]
    async fn test_append_event_with_rotation_under_threshold_returns_no_rotation() {
        let _guard = PekoTestModeGuard::new();
        let temp = TempDir::new().unwrap();
        let storage = SessionStorage::new(temp.path().to_path_buf());

        // Build a real `SessionCreated` event so we exercise the same
        // serialisation path production uses.
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

        // Sink that would panic if invoked — the test verifies it
        // stays cold under threshold.
        struct PanicSink;
        #[async_trait::async_trait]
        impl RotationSink for PanicSink {
            async fn request_rotation(
                &self,
                _session_id: &str,
                _reason: RotationReason,
            ) -> anyhow::Result<u32> {
                panic!("sink must not fire under threshold")
            }
        }

        let outcome = storage
            .append_event_with_rotation("live", &event, &PanicSink)
            .await
            .unwrap();
        assert_eq!(outcome, RotationOutcome::NoRotation);
        assert!(temp.path().join("live.jsonl").exists());
    }

    /// WS2: when an append would push the JSONL past `rotate_bytes()`
    /// the sink pages the full file aside and the event lands in a
    /// fresh `<id>.jsonl` under the SAME session id.
    #[tokio::test]
    async fn test_append_event_with_rotation_over_threshold_calls_sink() {
        let _guard = PekoTestModeGuard::new();
        let temp = TempDir::new().unwrap();
        let storage = SessionStorage::new(temp.path().to_path_buf());

        // Pre-fill the live file past threshold so the next append
        // crosses it deterministically.
        let oversize = "x".repeat(crate::test_config::rotate_bytes() + 1);
        tokio::fs::write(temp.path().join("live.jsonl"), oversize)
            .await
            .unwrap();

        // Sink that pages in place, mirroring the production
        // `SessionManagerRotationSink`.
        struct CapturingSink {
            dir: PathBuf,
            captured: std::sync::Arc<std::sync::Mutex<Option<String>>>,
        }
        #[async_trait::async_trait]
        impl RotationSink for CapturingSink {
            async fn request_rotation(
                &self,
                session_id: &str,
                reason: RotationReason,
            ) -> anyhow::Result<u32> {
                self.captured
                    .lock()
                    .unwrap()
                    .replace(session_id.to_string());
                assert_eq!(reason, RotationReason::SizeExceeded);
                let n = crate::jsonl::page_numbers(&self.dir, session_id)
                    .into_iter()
                    .max()
                    .unwrap_or(0)
                    + 1;
                tokio::fs::rename(
                    self.dir.join(format!("{session_id}.jsonl")),
                    crate::jsonl::page_path(&self.dir, session_id, n),
                )
                .await?;
                Ok(n)
            }
        }
        let captured = std::sync::Arc::new(std::sync::Mutex::new(None));
        let sink = CapturingSink {
            dir: temp.path().to_path_buf(),
            captured: std::sync::Arc::clone(&captured),
        };

        let event = SessionEvent::SessionCreated(SessionCreatedEvent {
            envelope: EventEnvelope {
                id: "test-2".to_string(),
                ts: Utc::now(),
            },
            instance_id: "instance-2".to_string(),
            image_digest: "sha256:def".to_string(),
            parent_session_id: None,
            trigger: SessionTrigger::User,
        });

        let outcome = storage
            .append_event_with_rotation("live", &event, &sink)
            .await
            .unwrap();
        assert_eq!(
            outcome,
            RotationOutcome::Paged {
                session_id: "live".to_string(),
                page_n: 1,
            }
        );
        assert_eq!(*captured.lock().unwrap(), Some("live".to_string()));
        assert!(
            temp.path().join("live.1.jsonl").exists(),
            "previous content should be paged aside"
        );
        assert!(
            temp.path().join("live.jsonl").exists(),
            "event should land in a fresh current page under the same id"
        );
    }

    // ============================================================
    // Paging (round 7): discovery, stitching, delete, copy
    // ============================================================

    /// A test sink that pages in place exactly like the production
    /// `SessionManagerRotationSink` (rename live file to the next
    /// numbered page, return the page number).
    struct PagingSink {
        dir: PathBuf,
    }
    #[async_trait::async_trait]
    impl RotationSink for PagingSink {
        async fn request_rotation(
            &self,
            session_id: &str,
            _reason: RotationReason,
        ) -> anyhow::Result<u32> {
            let n = crate::jsonl::page_numbers(&self.dir, session_id)
                .into_iter()
                .max()
                .unwrap_or(0)
                + 1;
            tokio::fs::rename(
                self.dir
                    .join(format!("{}.jsonl", safe_filename_component(session_id))),
                crate::jsonl::page_path(&self.dir, session_id, n),
            )
            .await?;
            Ok(n)
        }
    }

    fn user_event(text: &str) -> SessionEvent {
        SessionEvent::MessageV2(crate::message::SessionMessage::user(
            text,
            crate::message::MessageSource::User,
        ))
    }

    #[test]
    fn test_page_numbers_discovers_numeric_pages_only() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path();
        for name in [
            "s.3.jsonl",
            "s.1.jsonl",
            "s.2.jsonl",
            // Not pages: live file, legacy chapter, non-numeric suffix,
            // a page belonging to a different session.
            "s.jsonl",
            "s#20260101-000000.jsonl",
            "s.old.jsonl",
            "other.7.jsonl",
        ] {
            std::fs::write(dir.join(name), "{}\n").unwrap();
        }

        assert_eq!(crate::jsonl::page_numbers(dir, "s"), vec![1, 2, 3]);
        assert_eq!(crate::jsonl::page_numbers(dir, "other"), vec![7]);
        assert_eq!(
            crate::jsonl::page_numbers(dir, "missing"),
            Vec::<u32>::new()
        );
    }

    /// Multiple rotations number pages monotonically and `load_events`
    /// stitches pages 1..=N + the current page in chronological order.
    #[tokio::test]
    async fn test_paging_stitches_full_history_in_order() {
        let _guard = PekoTestModeGuard::new();
        let temp = TempDir::new().unwrap();
        let storage = SessionStorage::new(temp.path().to_path_buf());
        let sink = PagingSink {
            dir: temp.path().to_path_buf(),
        };

        // Each event is sized so two fit under the threshold but three
        // do not — every third append pages, giving pages [1, 2].
        let threshold = crate::test_config::rotate_bytes();
        let overhead = serde_json::to_string(&user_event("")).unwrap().len() + 1;
        let text_len = threshold / 2 - overhead - 32;
        let big = "m".repeat(text_len);
        let line_len = serde_json::to_string(&user_event(&big)).unwrap().len() + 1;
        assert!(2 * line_len <= threshold, "two events must fit in a page");
        assert!(3 * line_len > threshold, "three events must force a page");

        let mut expected = vec![];
        let mut page_numbers_seen = vec![];
        for i in 0..5 {
            let text = format!("{big}-{i}");
            let outcome = storage
                .append_event_with_rotation("live", &user_event(&text), &sink)
                .await
                .unwrap();
            expected.push(text);
            if let RotationOutcome::Paged { session_id, page_n } = outcome {
                assert_eq!(session_id, "live");
                page_numbers_seen.push(page_n);
            }
        }

        // Two pages plus the live file; pages numbered monotonically.
        assert_eq!(page_numbers_seen, vec![1, 2]);
        assert_eq!(crate::jsonl::page_numbers(temp.path(), "live"), vec![1, 2]);
        assert!(temp.path().join("live.jsonl").exists());

        // Stitched read returns every event in append order.
        let events = storage.load_events("live").await.unwrap();
        let texts: Vec<String> = events
            .iter()
            .filter_map(|e| e.as_message().map(|m| m.text_content()))
            .collect();
        assert_eq!(texts, expected);

        // `load_normalized` sees the same stitched history.
        let entries = storage.load_normalized("live").await.unwrap();
        assert_eq!(entries.len(), expected.len());

        // Page files are storage-internal: `list_sessions` shows the
        // session once under its stable id.
        assert_eq!(
            storage.list_sessions().await.unwrap(),
            vec!["live".to_string()]
        );
    }

    #[tokio::test]
    async fn test_delete_session_removes_pages() {
        let temp = TempDir::new().unwrap();
        let storage = SessionStorage::new(temp.path().to_path_buf());

        storage.create_session("paged", None).await.unwrap();
        std::fs::write(temp.path().join("paged.1.jsonl"), "{}\n").unwrap();
        std::fs::write(temp.path().join("paged.2.jsonl"), "{}\n").unwrap();
        assert!(storage.session_exists("paged").await);

        storage.delete_session("paged").await.unwrap();

        assert!(!storage.session_exists("paged").await);
        assert!(!temp.path().join("paged.1.jsonl").exists());
        assert!(!temp.path().join("paged.2.jsonl").exists());
        assert!(!temp.path().join("paged.jsonl").exists());
    }

    /// A paged session whose current page was never recreated (freshly
    /// rotated, no append yet) still exists and loads its history.
    #[tokio::test]
    async fn test_session_exists_and_loads_with_only_pages() {
        let temp = TempDir::new().unwrap();
        let storage = SessionStorage::new(temp.path().to_path_buf());

        storage.create_session("only_pages", None).await.unwrap();
        let event = storage.load_events("only_pages").await.unwrap();
        // Move the live file aside to a page, leaving no current page.
        tokio::fs::rename(
            temp.path().join("only_pages.jsonl"),
            temp.path().join("only_pages.1.jsonl"),
        )
        .await
        .unwrap();

        assert!(storage.session_exists("only_pages").await);
        let events = storage.load_events("only_pages").await.unwrap();
        assert_eq!(events.len(), event.len());
    }

    #[tokio::test]
    async fn test_copy_session_copies_pages() {
        let temp = TempDir::new().unwrap();
        let storage = SessionStorage::new(temp.path().to_path_buf());

        storage.create_session("branch_src", None).await.unwrap();
        storage
            .append_event("branch_src", &user_event("latest"))
            .await
            .unwrap();
        std::fs::write(temp.path().join("branch_src.1.jsonl"), "{}\n").unwrap();
        std::fs::write(temp.path().join("branch_src.2.jsonl"), "{}\n").unwrap();

        storage
            .copy_session("branch_src", "branch_dst")
            .await
            .unwrap();

        assert_eq!(
            crate::jsonl::page_numbers(temp.path(), "branch_dst"),
            vec![1, 2]
        );
        assert!(temp.path().join("branch_dst.jsonl").exists());
        // The branch's stitched history matches the source's.
        let src_events = storage.load_events("branch_src").await.unwrap();
        let dst_events = storage.load_events("branch_dst").await.unwrap();
        assert_eq!(src_events.len(), dst_events.len());
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
        let msg =
            crate::message::SessionMessage::user("hello", crate::message::MessageSource::User);
        storage
            .append_event("f30_test", &crate::events::SessionEvent::MessageV2(msg))
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
            if let crate::events::SessionEvent::MessageV2(m) = e {
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
            peko_message::LlmMessage::system("You are a helpful assistant."),
            peko_message::LlmMessage::user("Hello"),
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

        let messages = vec![peko_message::LlmMessage::user("Hello")];

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

        let messages = vec![peko_message::LlmMessage::user("Hello")];

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

        let messages = vec![peko_message::LlmMessage::user("Hello")];

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

    // ============================================================
    // search_transcripts Tests
    // ============================================================

    #[tokio::test]
    async fn test_search_transcripts_finds_needle_case_insensitive() {
        let temp = TempDir::new().unwrap();
        let storage = SessionStorage::new(temp.path().to_path_buf());

        storage.create_session("sess_one", None).await.unwrap();
        storage.create_session("sess_two", None).await.unwrap();

        let msg = crate::message::SessionMessage::user(
            "nothing interesting here",
            crate::message::MessageSource::User,
        );
        storage
            .append_event("sess_one", &SessionEvent::MessageV2(msg))
            .await
            .unwrap();
        let msg = crate::message::SessionMessage::assistant_text(
            "the Needle is hidden here",
            "test",
            "test-model",
        );
        storage
            .append_event("sess_two", &SessionEvent::MessageV2(msg))
            .await
            .unwrap();

        let ids = vec!["sess_one".to_string(), "sess_two".to_string()];
        // Different casing than the stored text: still matches.
        let hits = storage
            .search_transcripts(&ids, "NEEDLE", 10)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].session_id, "sess_two");
        assert_eq!(hits[0].role, "assistant");
        assert!(hits[0].snippet.contains("Needle"));

        // Unknown sessions are skipped, not fatal.
        let ids = vec!["sess_missing".to_string(), "sess_one".to_string()];
        let hits = storage
            .search_transcripts(&ids, "interesting", 10)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].session_id, "sess_one");
        assert_eq!(hits[0].role, "user");

        // Empty needle / zero budget short-circuit to no hits.
        assert!(storage
            .search_transcripts(&ids, "", 10)
            .await
            .unwrap()
            .is_empty());
        assert!(storage
            .search_transcripts(&ids, "interesting", 0)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn test_search_transcripts_respects_max_hits() {
        let temp = TempDir::new().unwrap();
        let storage = SessionStorage::new(temp.path().to_path_buf());

        storage.create_session("sess_hits", None).await.unwrap();
        for i in 0..3 {
            let msg = crate::message::SessionMessage::user(
                format!("match number {i}"),
                crate::message::MessageSource::User,
            );
            storage
                .append_event("sess_hits", &SessionEvent::MessageV2(msg))
                .await
                .unwrap();
        }

        let ids = vec!["sess_hits".to_string()];
        let hits = storage.search_transcripts(&ids, "match", 2).await.unwrap();
        assert_eq!(hits.len(), 2, "scanning stops after max_hits");

        let hits = storage.search_transcripts(&ids, "match", 10).await.unwrap();
        assert_eq!(hits.len(), 3);
    }

    #[tokio::test]
    async fn test_search_transcripts_snippet_centered_on_match() {
        let temp = TempDir::new().unwrap();
        let storage = SessionStorage::new(temp.path().to_path_buf());

        storage.create_session("sess_long", None).await.unwrap();
        let text = format!("{}needle{}", "a".repeat(200), "b".repeat(200));
        let msg = crate::message::SessionMessage::user(text, crate::message::MessageSource::User);
        storage
            .append_event("sess_long", &SessionEvent::MessageV2(msg))
            .await
            .unwrap();

        let ids = vec!["sess_long".to_string()];
        let hits = storage
            .search_transcripts(&ids, "needle", 10)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);

        let snippet = &hits[0].snippet;
        assert!(snippet.contains("needle"));
        assert!(
            snippet.starts_with('…'),
            "truncated prefix marked: {snippet}"
        );
        assert!(snippet.ends_with('…'), "truncated suffix marked: {snippet}");
        // ~80 chars either side of the 6-char match plus two ellipsis marks.
        assert!(snippet.chars().count() <= 80 + 6 + 80 + 2);
    }
}
