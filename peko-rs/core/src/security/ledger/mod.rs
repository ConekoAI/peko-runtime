//! The per-principal tool ledger (ADR-079).
//!
//! Every tool call a principal makes is written here by the dispatcher:
//! a `start` record before the call runs, and an `end` record when it
//! finishes, is refused, or detaches into the background (then a second
//! `end` when the background body finishes). Records carry the call's
//! identity, its parent, the acting-for stamp, redacted params, and a
//! redacted result excerpt. Files are hash-chained
//! ([`peko_observability::chain`]) so tampering is detectable.
//!
//! - [`redact`] — redaction and size caps.
//! - [`read`] — grouping records into calls, compartment scoping, and
//!   the verification inputs drawn from the global audit log.

pub mod read;
pub mod redact;

#[cfg(test)]
mod dispatch_tests;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use peko_observability::chain::{as_record, Appended, ChainedJsonl};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use redact::{head, tail, Redactor, EXCERPT_CAP};

/// File prefix: `ledger-YYYY-MM-DD.jsonl`.
pub const PREFIX: &str = "ledger";
/// Record schema version.
pub const VERSION: u64 = 1;
/// How often a ledger's head is checkpointed into the global audit log.
pub const CHECKPOINT_EVERY: Duration = Duration::from_hours(1);
/// Default retention when `principal.toml` sets none.
pub const DEFAULT_RETENTION_DAYS: u32 = 90;

/// Global audit event types the ledger writes (ADR-079 D5, D7).
pub const EVENT_CHECKPOINT: &str = "ledger.checkpoint";
pub const EVENT_PRUNED: &str = "ledger.pruned";
pub const EVENT_WRITE_FAILED: &str = "ledger.write_failed";

/// A principal's ledger directory, from its sessions directory: both sit
/// in the principal's Local tier (`LocalLayout::ledger_dir`).
#[must_use]
pub fn dir_for_sessions(sessions_dir: &Path) -> PathBuf {
    sessions_dir
        .parent()
        .map_or_else(|| sessions_dir.join(PREFIX), |local| local.join(PREFIX))
}

/// How a call ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Ok,
    Error,
    Cancelled,
    /// Refused by the ADR-078 floor.
    FloorDenied,
    /// Refused by the owner's policy.
    PolicyDenied,
    /// Filed for the owner's approval; nothing ran.
    AwaitingOwner,
    /// A non-owner's protected write, filed as a proposal.
    Proposed,
    /// Still running in the background; a later `end` follows.
    Detached,
}

impl Outcome {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Error => "error",
            Self::Cancelled => "cancelled",
            Self::FloorDenied => "floor_denied",
            Self::PolicyDenied => "policy_denied",
            Self::AwaitingOwner => "awaiting_owner",
            Self::Proposed => "proposed",
            Self::Detached => "detached",
        }
    }

    /// Every outcome, for argument validation.
    pub const ALL: [Self; 8] = [
        Self::Ok,
        Self::Error,
        Self::Cancelled,
        Self::FloorDenied,
        Self::PolicyDenied,
        Self::AwaitingOwner,
        Self::Proposed,
        Self::Detached,
    ];
}

/// One principal's ledger.
pub struct Ledger {
    chain: Arc<ChainedJsonl>,
    principal_id: String,
    checkpoint: Mutex<CheckpointState>,
}

struct CheckpointState {
    at: Instant,
    seq: Option<u64>,
}

impl std::fmt::Debug for Ledger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ledger")
            .field("dir", &self.chain.dir())
            .field("principal_id", &self.principal_id)
            .finish_non_exhaustive()
    }
}

impl Ledger {
    /// Open `principal_id`'s ledger in `dir`. A new ledger starts with a
    /// `genesis` record.
    pub fn open(dir: &Path, principal_id: &str) -> Result<Self> {
        let chain = ChainedJsonl::open(dir, PREFIX)
            .with_context(|| format!("open tool ledger in {}", dir.display()))?;
        if chain.head().0 == 0 {
            chain.append(as_record(json!({
                "v": VERSION,
                "kind": "genesis",
                "ts": now(),
                "principal_id": principal_id,
            }))?)?;
        }
        Ok(Self {
            chain: Arc::new(chain),
            principal_id: principal_id.to_string(),
            checkpoint: Mutex::new(CheckpointState {
                at: Instant::now(),
                seq: None,
            }),
        })
    }

    #[must_use]
    pub fn dir(&self) -> &Path {
        self.chain.dir()
    }

    #[must_use]
    pub fn principal_id(&self) -> &str {
        &self.principal_id
    }

    /// Append `record` durably (fsync'd before this returns).
    pub async fn append(&self, record: Map<String, Value>) -> Result<Appended> {
        let chain = Arc::clone(&self.chain);
        tokio::task::spawn_blocking(move || chain.append(record))
            .await
            .context("ledger writer task")?
    }

    /// `(seq, hash)` of the last record when a checkpoint is due: an
    /// hour since the last one (or `force`) and the chain has advanced.
    pub fn take_checkpoint(&self, force: bool) -> Option<(u64, String)> {
        let (next, head) = self.chain.head();
        let last = next.checked_sub(1)?;
        let mut state = self.checkpoint.lock().expect("ledger checkpoint poisoned");
        if state.seq == Some(last) || (!force && state.at.elapsed() < CHECKPOINT_EVERY) {
            return None;
        }
        state.at = Instant::now();
        state.seq = Some(last);
        Some((last, head))
    }
}

/// RFC 3339 UTC, millisecond precision.
#[must_use]
pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Facts the `start` record carries.
pub struct Start<'a> {
    pub call_id: &'a str,
    pub spec: &'a peko_engine::ToolCallSpec,
    pub action: Option<&'a str>,
    pub source: Option<&'a str>,
    pub params_digest: &'a str,
}

/// The `start` record for a call, with params redacted.
#[must_use]
pub fn start_record(start: &Start<'_>, redactor: &Redactor) -> Map<String, Value> {
    let spec = start.spec;
    let mut record = Map::new();
    record.insert("v".into(), json!(VERSION));
    record.insert("kind".into(), json!("start"));
    record.insert("ts".into(), json!(now()));
    record.insert("call_id".into(), json!(start.call_id));
    insert_some(&mut record, "run_id", spec.run_id.as_deref());
    insert_some(&mut record, "tool_call_id", spec.tool_call_id.as_deref());
    if let Some(parent) = &spec.parent {
        record.insert("parent".into(), json!(parent));
    }
    insert_some(&mut record, "principal_id", spec.principal_id.as_deref());
    insert_some(
        &mut record,
        "principal_name",
        spec.principal_name.as_deref(),
    );
    insert_some(&mut record, "agent_id", spec.agent_id.as_deref());
    insert_some(&mut record, "session_id", spec.session_id.as_deref());
    record.insert(
        "acting_for".into(),
        json!(spec
            .acting_for
            .clone()
            .unwrap_or_else(peko_tools_core::ActingFor::unattributed)),
    );
    record.insert("tool".into(), json!(spec.tool_name));
    insert_some(&mut record, "action", start.action);
    insert_some(&mut record, "source", start.source);
    record.insert("params".into(), redactor.params(&spec.params));
    record.insert("params_digest".into(), json!(start.params_digest));
    record
}

/// Facts an `end` record carries.
#[derive(Debug, Clone)]
pub struct End {
    pub outcome: Outcome,
    pub duration_ms: Option<u64>,
    pub exit_code: Option<i64>,
    pub task_id: Option<String>,
    pub result: Option<Excerpt>,
    /// The late `end` of a detached call.
    pub late: bool,
}

/// The `end` record for `call_id`.
#[must_use]
pub fn end_record(call_id: &str, end: &End) -> Map<String, Value> {
    let mut record = Map::new();
    record.insert("v".into(), json!(VERSION));
    record.insert("kind".into(), json!("end"));
    record.insert("ts".into(), json!(now()));
    record.insert("call_id".into(), json!(call_id));
    record.insert("outcome".into(), json!(end.outcome.as_str()));
    if let Some(ms) = end.duration_ms {
        record.insert("duration_ms".into(), json!(ms));
    }
    if let Some(code) = end.exit_code {
        record.insert("exit_code".into(), json!(code));
    }
    insert_some(&mut record, "task_id", end.task_id.as_deref());
    if end.late {
        record.insert("late".into(), json!(true));
    }
    if let Some(result) = &end.result {
        record.insert("result".into(), json!(result));
    }
    record
}

fn insert_some(record: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    if let Some(value) = value.filter(|v| !v.is_empty()) {
        record.insert(key.into(), json!(value));
    }
}

/// A redacted, capped view of a call's result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Excerpt {
    pub excerpt: String,
    /// Size of the full result text.
    pub bytes: usize,
    pub truncated: bool,
}

/// The excerpt for a result. Process output (`stdout` / `stderr`, as
/// from `Bash` and `Workflow`) keeps each stream's tail, matching their
/// own truncation; anything else keeps the head of its text.
#[must_use]
pub fn excerpt(text: &str, json: &Value, redactor: &Redactor) -> Excerpt {
    let stream = |key: &str| json.get(key).and_then(Value::as_str);
    if let (Some(stdout), Some(stderr)) = (stream("stdout"), stream("stderr")) {
        let half = EXCERPT_CAP / 2;
        let (stdout, stderr) = (redactor.text(stdout), redactor.text(stderr));
        let mut excerpt = tail(&stdout, half);
        if !stderr.is_empty() {
            excerpt.push_str("\n[stderr]\n");
            excerpt.push_str(&tail(&stderr, half));
        }
        return Excerpt {
            excerpt,
            bytes: stdout.len() + stderr.len(),
            truncated: stdout.len() > half || stderr.len() > half,
        };
    }
    let redacted = redactor.text(text);
    Excerpt {
        excerpt: head(&redacted, EXCERPT_CAP),
        bytes: text.len(),
        truncated: redacted.len() > EXCERPT_CAP,
    }
}

/// The exit code a process tool reported.
#[must_use]
pub fn exit_code(json: &Value) -> Option<i64> {
    json.get("exit_code").and_then(Value::as_i64)
}

/// The task id of a detached call's receipt (the router's
/// timeout-detach shape), if `json` is one.
#[must_use]
pub fn detached_task_id(json: &Value) -> Option<String> {
    (json["_async_status"] == "queued" && json["reason"] == "timeout")
        .then(|| json["task_id"].as_str().map(str::to_string))
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_new_ledger_starts_with_genesis_and_checkpoints_once_per_head() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::open(dir.path(), "p1").unwrap();
        let reopened = Ledger::open(dir.path(), "p1").unwrap();
        let lines = peko_observability::chain::read_all(dir.path(), PREFIX, |_| true).unwrap();
        assert_eq!(lines.len(), 1, "reopening does not write a second genesis");
        assert_eq!(lines[0].record.as_ref().unwrap()["kind"], "genesis");
        drop(reopened);

        assert!(ledger.take_checkpoint(false).is_none(), "not due yet");
        let (seq, _) = ledger.take_checkpoint(true).unwrap();
        assert_eq!(seq, 0);
        assert!(ledger.take_checkpoint(true).is_none(), "head unchanged");
        ledger
            .append(end_record(
                "c",
                &End {
                    outcome: Outcome::Ok,
                    duration_ms: None,
                    exit_code: None,
                    task_id: None,
                    result: None,
                    late: false,
                },
            ))
            .await
            .unwrap();
        assert_eq!(ledger.take_checkpoint(true).unwrap().0, 1);
    }

    #[test]
    fn process_output_keeps_stream_tails() {
        let redactor = Redactor::default();
        let json = json!({ "stdout": format!("{}END", "a".repeat(5000)), "stderr": "warn" });
        let e = excerpt("ignored", &json, &redactor);
        assert!(e.excerpt.contains("END"));
        assert!(e.excerpt.ends_with("[stderr]\nwarn"));
        assert!(e.truncated);
        assert_eq!(e.bytes, 5007);

        let plain = excerpt("short", &Value::Null, &redactor);
        assert_eq!(plain.excerpt, "short");
        assert!(!plain.truncated);
    }

    #[test]
    fn detach_receipts_are_recognized() {
        let receipt =
            json!({ "_async_status": "queued", "reason": "timeout", "task_id": "Bash:1" });
        assert_eq!(detached_task_id(&receipt).as_deref(), Some("Bash:1"));
        let spawned = json!({ "_async_status": "queued", "task_id": "Bash:1" });
        assert_eq!(detached_task_id(&spawned), None);
    }
}
