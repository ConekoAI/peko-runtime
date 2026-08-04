//! Audit Log - Security and compliance logging
//!
//! Storage model: in-memory ring buffer (fast IPC queries for events
//! emitted this session) PLUS an optional append-only JSONL file sink
//! (durable, survives daemon restarts, queryable via `peko audit tail`).
//! The JSONL sink is opt-in via [`AuditLogger::with_jsonl`] — unit tests
//! that don't need persistence use [`AuditLogger::new`] to keep the
//! fixture simple.

use anyhow::{Context, Result};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use peko_auth::Subject;

/// Audit logger
pub struct AuditLogger {
    /// In-memory buffer for fast IPC queries over events emitted this
    /// session. Newest-first iteration order; capped at `max_size`
    /// entries; oldest entries are dropped on overflow.
    buffer: VecDeque<AuditEvent>,
    /// Maximum buffer size
    max_size: usize,
    /// Optional JSONL file sink for durable persistence.
    jsonl: Option<JsonlSink>,
}

/// Audit event
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    /// When the event occurred
    pub timestamp: chrono::DateTime<chrono::Utc>,
    /// Which component logged it
    pub component: String,
    /// Type of event
    pub event_type: String,
    /// Which agent (if any)
    pub agent_did: Option<String>,
    /// Resolved caller identity as a typed `Subject` (ADR-039).
    /// Populated on every event that flows through the request path so
    /// the audit trail is attributable to a real subject — `User` /
    /// `Principal` / `Public`. `None` only on legacy events that pre-date the
    /// per-user attribution plumbing (issue #17) or on system-emitted
    /// events with no caller context (use `Subject::User("local")` —
    /// via `CallerContext::local().subject()` — or `Subject::Public`
    /// for genuinely unauthenticated events, issue #26). For
    /// security events with no caller context, prefer
    /// `Subject::Public` over `None` so per-user audit queries can
    /// still distinguish "unauthenticated security event" from "no
    /// caller recorded" (issue #26 review feedback).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller: Option<Subject>,
    /// Event details
    pub details: serde_json::Value,
    /// Severity level
    pub severity: AuditSeverity,
}

/// Audit severity levels
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AuditSeverity {
    Debug,
    Info,
    Warning,
    Error,
    Security,
}

/// Append-only JSONL file sink. One line per event. Daily rotation.
///
/// File naming: `audit-YYYY-MM-DD.jsonl` inside `dir`. Rotation is
/// lazy: each `write` checks whether the current UTC date has
/// changed; if so, the existing file handle is closed and a new one
/// opened under the new date's name. The previous file is **not**
/// deleted — historical files accumulate until the user prunes them
/// (e.g. via `peko audit prune` or a manual cron job). See ADR-046's
/// "Known v1 limitations" for the rationale.
///
/// Each line is `fsync`'d to disk after the write so a daemon crash
/// doesn't lose the event. `O_APPEND` ensures POSIX-atomic appends
/// across concurrent writers (although the `AuditLogger` write lock
/// already serializes single-writer appends).
pub struct JsonlSink {
    /// Directory holding `audit-YYYY-MM-DD.jsonl` files. Created on
    /// `open` if missing.
    dir: PathBuf,
    /// UTC date the currently-open file is named after.
    current_date: NaiveDate,
    /// Currently-open file (append-mode).
    file: File,
    /// Monotonic seq counter; survives rotation so `peko audit tail
    /// --follow` can deduplicate across the rotation boundary.
    seq: AtomicU64,
}

impl JsonlSink {
    /// Open a JSONL sink rooted at `dir`. Creates `dir` if missing.
    /// Returns an error if the directory can't be created or the
    /// initial file can't be opened.
    pub fn open(dir: impl Into<PathBuf>) -> Result<Self> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("create audit dir {}", dir.display()))?;
        let today = chrono::Utc::now().date_naive();
        let path = Self::path_for_date(&dir, today);
        let file = Self::open_append(&path)?;
        Ok(Self {
            dir,
            current_date: today,
            file,
            seq: AtomicU64::new(0),
        })
    }

    /// Write `event` as one JSONL line + trailing newline, then
    /// `fsync` to durable storage. The `seq` field is stamped onto
    /// the line as a monotonic counter.
    pub fn write(&mut self, event: &AuditEvent) -> Result<()> {
        let today = chrono::Utc::now().date_naive();
        if today != self.current_date {
            self.current_date = today;
            let path = Self::path_for_date(&self.dir, today);
            self.file = Self::open_append(&path)
                .with_context(|| format!("rotate audit file to {}", path.display()))?;
        }

        let seq = self.seq.fetch_add(1, Ordering::SeqCst);
        let mut value = serde_json::to_value(event).context("serialize audit event")?;
        if let Some(obj) = value.as_object_mut() {
            obj.insert("seq".to_string(), serde_json::Value::from(seq));
        }
        let mut line = serde_json::to_string(&value).context("re-serialize with seq")?;
        line.push('\n');

        self.file
            .write_all(line.as_bytes())
            .context("write audit line")?;
        self.file.sync_all().context("fsync audit line")?;
        Ok(())
    }

    /// Path to the JSONL file for a given date.
    fn path_for_date(dir: &Path, date: NaiveDate) -> PathBuf {
        dir.join(format!("audit-{date}.jsonl"))
    }

    /// Open `path` in append mode, creating if missing. Uses
    /// `O_APPEND` so concurrent writers don't clobber each other
    /// (irrelevant today — single-writer via `AuditLogger` write
    /// lock — but cheap insurance).
    #[cfg(unix)]
    fn open_append(path: &Path) -> Result<File> {
        use std::os::unix::fs::OpenOptionsExt;
        OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("open audit file {}", path.display()))
    }

    /// Windows variant. Mode bits aren't honored on Windows; the
    /// file inherits the parent directory ACL.
    #[cfg(not(unix))]
    fn open_append(path: &Path) -> Result<File> {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("open audit file {}", path.display()))
    }

    /// Current UTC date (the file we're writing to).
    #[must_use]
    pub fn current_date(&self) -> NaiveDate {
        self.current_date
    }

    /// Monotonic seq counter value (next event will get this seq).
    #[must_use]
    pub fn next_seq(&self) -> u64 {
        self.seq.load(Ordering::SeqCst)
    }
}

impl AuditLogger {
    /// Create new audit logger (in-memory only — no JSONL persistence).
    #[must_use]
    pub fn new() -> Self {
        Self {
            buffer: VecDeque::with_capacity(1000),
            max_size: 10000,
            jsonl: None,
        }
    }

    /// Create new audit logger with an append-only JSONL sink rooted
    /// at `dir`. The directory is created if missing. Returns an
    /// error if `dir` is unreachable.
    pub fn with_jsonl(dir: impl Into<PathBuf>) -> Result<Self> {
        let jsonl = JsonlSink::open(dir)?;
        Ok(Self {
            buffer: VecDeque::with_capacity(1000),
            max_size: 10000,
            jsonl: Some(jsonl),
        })
    }

    /// Returns `true` if this logger has a JSONL sink attached.
    #[must_use]
    pub fn has_jsonl(&self) -> bool {
        self.jsonl.is_some()
    }

    /// Log an event. Writes to the in-memory ring buffer first; if a
    /// JSONL sink is attached, also appends to the file with a
    /// monotonic seq. The file write is synchronous (blocking I/O)
    /// — audit events are infrequent and the `AuditLogger` write
    /// lock already serializes single-writer appends, so the cost
    /// is bounded.
    pub async fn log(&mut self, event: AuditEvent) -> Result<()> {
        // Persist first so a crash between ring-buffer write and
        // disk write doesn't lose the event. (Ring buffer is a
        // cache of the JSONL, not the other way around.)
        if let Some(jsonl) = self.jsonl.as_mut() {
            jsonl.write(&event)?;
        }

        if self.buffer.len() >= self.max_size {
            self.buffer.pop_front(); // Remove oldest
        }
        self.buffer.push_back(event);
        Ok(())
    }

    /// Get recent entries
    pub async fn get_entries(&self, limit: usize) -> Vec<AuditEvent> {
        self.buffer.iter().rev().take(limit).cloned().collect()
    }
}

impl Default for AuditLogger {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn mk_event(seq: u64) -> AuditEvent {
        AuditEvent {
            timestamp: chrono::Utc::now(),
            component: "test".to_string(),
            event_type: format!("event_{seq}"),
            agent_did: None,
            caller: None,
            details: serde_json::json!({"seq": seq}),
            severity: AuditSeverity::Info,
        }
    }

    #[tokio::test]
    async fn test_get_entries_limit() {
        let mut logger = AuditLogger::new();

        // Add 5 events
        for i in 0..5 {
            logger
                .log(AuditEvent {
                    timestamp: chrono::Utc::now(),
                    component: "test".to_string(),
                    event_type: format!("event_{i}"),
                    agent_did: None,
                    caller: None,
                    details: serde_json::json!({}),
                    severity: AuditSeverity::Info,
                })
                .await
                .unwrap();
        }

        // Get only 3 entries
        let entries = logger.get_entries(3).await;
        assert_eq!(entries.len(), 3);

        // Should return most recent first (LIFO order)
        assert_eq!(entries[0].event_type, "event_4");
        assert_eq!(entries[1].event_type, "event_3");
        assert_eq!(entries[2].event_type, "event_2");
    }

    /// JSONL sink writes one line per event, each parseable as a
    /// valid JSON object containing the original event fields plus a
    /// monotonic `seq`.
    #[tokio::test]
    async fn jsonl_sink_writes_one_line_per_event() {
        let dir = tempdir().unwrap();
        let mut sink = JsonlSink::open(dir.path()).unwrap();

        for i in 0..3 {
            sink.write(&mk_event(i)).unwrap();
        }

        let today = chrono::Utc::now().date_naive();
        let path = dir.path().join(format!("audit-{today}.jsonl"));
        let contents = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 3);

        // Each line is valid JSON with a `seq` field.
        for (i, line) in lines.iter().enumerate() {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(v["seq"].as_u64().unwrap(), i as u64);
            assert_eq!(v["event_type"].as_str().unwrap(), format!("event_{i}"));
            assert_eq!(v["component"].as_str().unwrap(), "test");
            assert_eq!(v["severity"].as_str().unwrap(), "info");
        }
    }

    /// `seq` is monotonic — fetch_add ensures no two events share
    /// the same seq, even across rotation boundaries.
    #[tokio::test]
    async fn jsonl_sink_seq_is_monotonic() {
        let dir = tempdir().unwrap();
        let mut sink = JsonlSink::open(dir.path()).unwrap();
        assert_eq!(sink.next_seq(), 0);
        sink.write(&mk_event(0)).unwrap();
        assert_eq!(sink.next_seq(), 1);
        sink.write(&mk_event(1)).unwrap();
        sink.write(&mk_event(2)).unwrap();
        assert_eq!(sink.next_seq(), 3);
    }

    /// `fsync` fires per line — verifiable indirectly: the file
    /// exists and is non-empty immediately after `write` returns.
    /// True fsync semantics are guaranteed by the OS at write time;
    /// here we just confirm the file is on disk and readable.
    #[tokio::test]
    async fn jsonl_sink_writes_are_durable_after_write_returns() {
        let dir = tempdir().unwrap();
        let mut sink = JsonlSink::open(dir.path()).unwrap();
        sink.write(&mk_event(0)).unwrap();
        // File should be readable immediately — sync_all ensured
        // bytes are flushed before `write` returned.
        let today = chrono::Utc::now().date_naive();
        let path = dir.path().join(format!("audit-{today}.jsonl"));
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("event_0"));
    }

    /// `AuditLogger::with_jsonl` populates both ring buffer and
    /// JSONL. `get_entries` returns from the ring buffer; the file
    /// holds the same events.
    #[tokio::test]
    async fn audit_logger_with_jsonl_writes_to_both() {
        let dir = tempdir().unwrap();
        let mut logger = AuditLogger::with_jsonl(dir.path()).unwrap();
        assert!(logger.has_jsonl());

        for i in 0..3 {
            logger.log(mk_event(i)).await.unwrap();
        }

        // Ring buffer
        let entries = logger.get_entries(10).await;
        assert_eq!(entries.len(), 3);
        // Newest first
        assert_eq!(entries[0].event_type, "event_2");

        // JSONL file
        let today = chrono::Utc::now().date_naive();
        let path = dir.path().join(format!("audit-{today}.jsonl"));
        let contents = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 3);
        for (i, line) in lines.iter().enumerate() {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(v["seq"].as_u64().unwrap(), i as u64);
        }
    }

    /// `AuditLogger::new` has no JSONL sink; `log` still works
    /// (memory-only).
    #[tokio::test]
    async fn audit_logger_new_has_no_jsonl() {
        let mut logger = AuditLogger::new();
        assert!(!logger.has_jsonl());
        logger.log(mk_event(0)).await.unwrap();
        let entries = logger.get_entries(10).await;
        assert_eq!(entries.len(), 1);
    }

    /// `JsonlSink::open` creates the directory if it doesn't exist.
    #[tokio::test]
    async fn jsonl_sink_creates_missing_directory() {
        let dir = tempdir().unwrap();
        let nested = dir.path().join("audit").join("nested");
        // Parent doesn't exist; `open` should create it.
        let _sink = JsonlSink::open(&nested).unwrap();
        assert!(nested.is_dir());
    }

    /// Issue #26: `caller: Option<Subject>` must serialize in the
    /// canonical `{kind, id}` shape that ADR-039 mandates (so per-user
    /// and per-agent audit queries can index on the tag instead of
    /// string-parsing the legacy `user:{sub}` convention) AND must be
    /// omitted (not serialized as null) when unset — keeps the wire
    /// format compact for legacy events that pre-date the per-user
    /// attribution plumbing (issue #17).
    #[test]
    fn audit_event_caller_principal_serialization() {
        // Agent caller — the canonical shape required by the issue.
        let with_agent_caller = AuditEvent {
            timestamp: chrono::Utc::now(),
            component: "tunnel".to_string(),
            event_type: "tunnel_proxied_request".to_string(),
            agent_did: Some("agent-a".to_string()),
            caller: Some(Subject::Principal("helper".into())),
            details: serde_json::json!({}),
            severity: AuditSeverity::Info,
        };
        let v: serde_json::Value = serde_json::to_value(&with_agent_caller).unwrap();
        // The Subject enum is `#[serde(tag = "kind", content = "id")]`
        // so it serializes as an inline `{kind, id}` object — not nested
        // under another key. This is the wire shape PekoHub query API
        // will key on (issue #26 acceptance criteria).
        assert_eq!(v["caller"]["kind"], "principal");
        assert_eq!(v["caller"]["id"], "helper");
        // The flat {kind, id} object is the contract — no extra nesting.
        assert!(v["caller"].is_object());
        assert_eq!(v["caller"].as_object().unwrap().len(), 2);

        // Round-trip: re-parse the value into an `AuditEvent` and check
        // the `Subject` survives — guards against accidental
        // string-conversion regressions on the audit wire format.
        let parsed: AuditEvent = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(parsed.caller, Some(Subject::Principal("helper".into())));

        // User caller — also projects cleanly.
        let with_user_caller = AuditEvent {
            timestamp: chrono::Utc::now(),
            component: "tunnel".to_string(),
            event_type: "tunnel_proxied_request".to_string(),
            agent_did: Some("agent-a".to_string()),
            caller: Some(Subject::User("user:user-42".to_string())),
            details: serde_json::json!({}),
            severity: AuditSeverity::Info,
        };
        let v: serde_json::Value = serde_json::to_value(&with_user_caller).unwrap();
        assert_eq!(v["caller"]["kind"], "user");
        assert_eq!(v["caller"]["id"], "user:user-42");

        // Public caller — for system-initiated events with no subject.
        let with_public_caller = AuditEvent {
            timestamp: chrono::Utc::now(),
            component: "cron".to_string(),
            event_type: "cron.execute".to_string(),
            agent_did: None,
            caller: Some(Subject::Public),
            details: serde_json::json!({}),
            severity: AuditSeverity::Info,
        };
        let v: serde_json::Value = serde_json::to_value(&with_public_caller).unwrap();
        // `Subject::Public` is a unit variant of an enum tagged
        // `#[serde(tag = "kind", content = "id")]` — so it serializes
        // as `{"kind": "public"}` with no `id` field (there is no id
        // to carry). This still round-trips correctly through the
        // deserializer.
        assert_eq!(v["caller"]["kind"], "public");
        assert!(
            v["caller"].get("id").is_none(),
            "Subject::Public must not serialize an id, got: {}",
            v["caller"]
        );
        let parsed: AuditEvent = serde_json::from_value(v).unwrap();
        assert_eq!(parsed.caller, Some(Subject::Public));

        // No caller — must be omitted, not serialized as null.
        let without_caller = AuditEvent {
            timestamp: chrono::Utc::now(),
            component: "tunnel".to_string(),
            event_type: "Agent".to_string(),
            agent_did: None,
            caller: None,
            details: serde_json::json!({}),
            severity: AuditSeverity::Info,
        };
        let v: serde_json::Value = serde_json::to_value(&without_caller).unwrap();
        assert!(
            v.get("caller").is_none(),
            "caller must be omitted (skip_serializing_if) when None, got: {v}"
        );
    }
}
