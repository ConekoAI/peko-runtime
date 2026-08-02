//! `ApprovalQueue` — durable in-memory queue of pending self-modification
//! requests (ADR-045 PR #3).
//!
//! Mirrors [`crate::ipc::auth::AuthTable`] structurally (`Arc<Mutex<HashMap>>`)
//! but adds a durability requirement: every `insert` and `decide`
//! is mirrored to disk under `<runtime>/pending-requests/<uuid>.json`,
//! mode 0600, written atomically via temp+rename.
//!
//! The on-disk artifacts let the user discover pending requests from
//! their terminal via `peko pending list` (PR #4) and survive daemon
//! restart via `rehydrate` (PR #4). In PR #3 only the writer side
//! exists; readers come in PR #4.
//!
//! ## Concurrency
//!
//! `std::sync::Mutex` is appropriate here — write paths (`insert`,
//! `decide`, `rehydrate`) are cold (user-triggered), read paths
//! (`get`, `list_pending`) are cold too. The hot path is
//! `daemon_api.request_self_modify` calling `insert` on agent-driven
//! `peko_self` calls, which are also rare.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use peko_subject::PrincipalId;
use peko_subject::Subject;

use crate::daemon::api::{RequestId, SelfModifyError, SelfModifyOp};

/// Default cap on the in-memory queue. Sized generously so a noisy
/// agent can't easily OOM the daemon, but small enough to surface
/// "user is ignoring the inbox" before the queue grows unbounded.
pub const DEFAULT_MAX_PENDING: usize = 1024;

/// One pending self-modification request.
///
/// Persisted to disk as JSON. The schema is stable across PR #3 and
/// PR #4 (no fields will be removed without a migration); new fields
/// are appended at the end with `#[serde(default)]` for forward
/// compatibility.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub id: RequestId,
    /// Snapshot of the requesting principal at submission time.
    pub principal_id: PrincipalId,
    /// Unix-epoch seconds when the request was queued.
    pub requested_at_secs: u64,
    /// Free-text reason the agent supplied. Echoed in the inbox UI
    /// (PR #4) and the CLI listing (`peko pending list`).
    pub reason: String,
    /// What the agent wants done. See [`SelfModifyOp`](super::api::SelfModifyOp).
    pub op: super::api::SelfModifyOp,
    /// Current status. See [`ApprovalStatus`].
    pub status: ApprovalStatus,
}

impl ApprovalRequest {
    /// Construct a fresh `Pending` request from an op + caller.
    pub fn from_op(op: super::api::SelfModifyOp, principal_id: PrincipalId) -> Self {
        let id = Uuid::new_v4();
        let reason = op.reason().to_string();
        let requested_at_secs = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self {
            id,
            principal_id,
            requested_at_secs,
            reason,
            op,
            status: ApprovalStatus::Pending,
        }
    }
}

/// One reason an op carries. Echoed into the on-disk artifact so
/// the user can see why the agent is asking without having to inspect
/// the in-memory struct.
impl super::api::SelfModifyOp {
    fn reason(&self) -> &str {
        match self {
            Self::GrantCapability { reason, .. }
            | Self::InstallExtension { reason, .. }
            | Self::EditAgentConfig { reason, .. }
            | Self::EditCronSchedule { reason, .. } => reason,
        }
    }
}

/// The decision lifecycle.
///
/// `Pending → Approved` and `Pending → Denied` are the only valid
/// transitions. Once a request has been decided, its on-disk file is
/// rewritten with the new status (so the file's status is the
/// authoritative source of truth, not the in-memory map alone).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ApprovalStatus {
    Pending,
    Approved {
        decided_at_secs: u64,
        by: Subject,
    },
    Denied {
        decided_at_secs: u64,
        by: Subject,
        /// Free-text reason the user gave (often blank).
        reason: String,
    },
}

impl ApprovalStatus {
    /// `true` if the request is still waiting for a decision.
    #[must_use]
    pub fn is_pending(&self) -> bool {
        matches!(self, Self::Pending)
    }
}

/// Errors specific to the decision side (`decide`, `rehydrate`).
///
/// Distinct from `SelfModifyError` because decision-time failures
/// (not found, malformed disk file) are a different operational
/// concern than submission-time failures (queue full, invalid cap).
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum DecisionError {
    #[error("no pending request with id {0}")]
    NotFound(RequestId),
    #[error("request {0} already decided (status = {1:?}); decisions are terminal")]
    AlreadyDecided(RequestId, ApprovalStatus),
    #[error("on-disk artifact for {0} is malformed: {1}")]
    CorruptedArtifact(RequestId, String),
}

/// User-driven decision input.
///
/// Distinct from [`ApprovalStatus`] (the on-disk/in-memory wire shape)
/// because the daemon fills in the `Subject` (from caller context) and
/// the timestamp — the CLI only says "grant" or "deny with reason".
///
/// PR #4 step 1: the IPC handler calls
/// `ApprovalQueue::decide_with(id, decision, subject)` to apply this.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Grant,
    Deny { reason: String },
}

impl Decision {
    /// `true` if this is a grant.
    #[must_use]
    pub fn is_grant(&self) -> bool {
        matches!(self, Self::Grant)
    }
}

/// Concurrent map of `RequestId → ApprovalRequest`, mirrored to disk.
///
/// The in-memory map is the authoritative source for the running
/// daemon. On-disk files are written on every transition and read
/// at startup by `rehydrate` (PR #4).
#[derive(Debug)]
pub struct ApprovalQueue {
    inner: Mutex<HashMap<RequestId, ApprovalRequest>>,
    /// Per-request durable-on-disk root (`<runtime>/pending-requests/`).
    persist_root: PathBuf,
    /// Hard cap on the in-memory map; inserts beyond this fail with
    /// `SelfModifyError::QueueFull`.
    max_pending: usize,
}

impl ApprovalQueue {
    /// Construct a new queue. Caller must ensure `persist_root` exists.
    pub fn new(persist_root: PathBuf, max_pending: usize) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(HashMap::new()),
            persist_root,
            max_pending,
        })
    }

    /// Access the configured persistence root. Used by tests and the
    /// `decide` path that resolves `<uuid>.json` paths.
    #[must_use]
    pub fn persist_root(&self) -> &Path {
        &self.persist_root
    }

    /// Queue a new request. The caller (agent) is unblocked as soon
    /// as the in-memory map is updated and the on-disk file is
    /// committed; the user decides asynchronously.
    ///
    /// Errors:
    /// - `QueueFull` if the in-memory map is at capacity.
    /// - `PersistenceFailed` if the on-disk write fails. The in-memory
    ///   map is NOT updated in this case (queue stays consistent
    ///   with disk).
    pub fn insert(&self, request: ApprovalRequest) -> Result<RequestId, SelfModifyError> {
        let mut g = self.inner.lock().expect("approval queue poisoned");
        if g.len() >= self.max_pending {
            return Err(SelfModifyError::QueueFull(g.len()));
        }
        // Persist before committing to the in-memory map. If the
        // disk write fails, the map is unchanged.
        self.write_to_disk(&request)
            .map_err(|e| SelfModifyError::PersistenceFailed(e.to_string()))?;
        let id = request.id;
        g.insert(id, request);
        Ok(id)
    }

    /// Look up a single request by id.
    #[must_use]
    pub fn get(&self, id: RequestId) -> Option<ApprovalRequest> {
        self.inner.lock().expect("approval queue poisoned").get(&id).cloned()
    }

    /// All currently-pending requests (status == Pending), in
    /// insertion order (HashMap iteration order is unspecified but
    /// stable for the lifetime of a process; the user's UI can sort).
    #[must_use]
    pub fn list_pending(&self) -> Vec<ApprovalRequest> {
        let g = self.inner.lock().expect("approval queue poisoned");
        g.values().filter(|r| r.status.is_pending()).cloned().collect()
    }

    /// All requests in the queue, regardless of status. Used by the
    /// daemon-side inbox (push delivery path) and by tests that need
    /// to audit decided requests.
    #[must_use]
    pub fn list_all(&self) -> Vec<ApprovalRequest> {
        let g = self.inner.lock().expect("approval queue poisoned");
        g.values().cloned().collect()
    }

    /// User-driven decision from the CLI / inbox UI.
    ///
    /// Distinct from [`ApprovalStatus`] (the in-memory wire shape) because
    /// the daemon fills in the `Subject` (from caller context) and the
    /// timestamp — the CLI only says "grant" or "deny with reason".
    pub fn decide_with(
        &self,
        id: RequestId,
        decision: Decision,
        by: Subject,
    ) -> Result<ApprovalRequest, DecisionError> {
        let now_secs = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let new_status = match decision {
            Decision::Grant => ApprovalStatus::Approved {
                decided_at_secs: now_secs,
                by,
            },
            Decision::Deny { reason } => ApprovalStatus::Denied {
                decided_at_secs: now_secs,
                by,
                reason,
            },
        };
        self.decide(id, new_status)?;
        // Re-fetch the updated entry. The decide() above succeeded so
        // it's guaranteed to be present.
        Ok(self.get(id).expect("decide succeeded but entry vanished"))
    }

    /// Apply a decision to a pending request.
    ///
    /// PR #3 ships only the persistence plumbing; the calling CLI
    /// command (`peko pending decide`) is PR #4 work. Tests can drive
    /// `decide` directly.
    pub fn decide(
        &self,
        id: RequestId,
        decision: ApprovalStatus,
    ) -> Result<(), DecisionError> {
        let mut g = self.inner.lock().expect("approval queue poisoned");
        let entry = g.get_mut(&id).ok_or(DecisionError::NotFound(id))?;
        if !entry.status.is_pending() {
            return Err(DecisionError::AlreadyDecided(id, entry.status.clone()));
        }
        // Auto-stamp `decided_at_secs` if the caller passed 0 (the
        // common path: CLI caller constructs Approved/Denied without
        // knowing the wall-clock time). Pre-stamped statuses (e.g.
        // from a replayed audit log) keep their original timestamp.
        let now_secs = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let stamped = match decision {
            ApprovalStatus::Pending => decision, // shouldn't happen but tolerate
            ApprovalStatus::Approved { decided_at_secs, by } => {
                let secs = if decided_at_secs == 0 { now_secs } else { decided_at_secs };
                ApprovalStatus::Approved { decided_at_secs: secs, by }
            }
            ApprovalStatus::Denied { decided_at_secs, by, reason } => {
                let secs = if decided_at_secs == 0 { now_secs } else { decided_at_secs };
                ApprovalStatus::Denied { decided_at_secs: secs, by, reason }
            }
        };
        entry.status = stamped.clone();
        // Mirror to disk. We hold the lock across the write so two
        // concurrent decisions can't race; on failure we revert the
        // status. The disk write is the slowest path; in PR #4 the
        // caller will already be on a cold path (user clicked
        // approve/deny in the inbox).
        self.write_to_disk(entry).map_err(|e| {
            // Revert in-memory state on disk failure (best-effort).
            entry.status = ApprovalStatus::Pending;
            DecisionError::CorruptedArtifact(id, e.to_string())
        })?;
        Ok(())
    }

    /// Rehydrate the in-memory map from disk.
    ///
    /// Reads every `<uuid>.json` under `persist_root` and inserts
    /// into the map. Returns the number of requests loaded.
    ///
    /// PR #3 implements the file reader but does NOT call this from
    /// daemon startup; PR #4 wires the call.
    pub fn rehydrate(&self) -> std::io::Result<usize> {
        let mut loaded = 0usize;
        let entries = std::fs::read_dir(&self.persist_root)?;
        let mut g = self.inner.lock().expect("approval queue poisoned");
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let raw = std::fs::read(&path)?;
            match serde_json::from_slice::<ApprovalRequest>(&raw) {
                Ok(req) => {
                    g.insert(req.id, req);
                    loaded += 1;
                }
                Err(_) => {
                    // Skip malformed files but log to stderr. Don't
                    // poison startup because one bad file is present.
                    eprintln!(
                        "approval_queue: skipping malformed artifact {}",
                        path.display()
                    );
                }
            }
        }
        Ok(loaded)
    }

    /// Compute the on-disk file path for a given request id.
    #[must_use]
    pub fn artifact_path(&self, id: RequestId) -> PathBuf {
        self.persist_root.join(format!("{id}.json"))
    }

    /// Atomically write the request to `<persist_root>/<id>.json`,
    /// mode 0600. Uses `temp + rename` so a crash mid-write leaves
    /// either the old file or the new file, never a partial file.
    fn write_to_disk(&self, request: &ApprovalRequest) -> std::io::Result<()> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;

        std::fs::create_dir_all(&self.persist_root)?;
        let final_path = self.artifact_path(request.id);
        // The temp file lives next to the final one so rename is atomic
        // on the same filesystem.
        let tmp_path = self.persist_root.join(format!(".{}.json.tmp", request.id));

        let bytes = serde_json::to_vec_pretty(request)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        {
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp_path)?;
            f.write_all(&bytes)?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp_path, &final_path)?;
        Ok(())
    }
}

// =============================================================================
// `ApprovalQueueApi` — thin `DaemonApi` adapter over `Arc<ApprovalQueue>`.
//
// ADR-045 PR #3: the daemon populates the process-global `DaemonApi`
// slot BEFORE `ToolRuntime::with_workspace_and_core` runs
// `register_builtins` (so `peko_self` registration can find the API).
// At that point `AppState` doesn't exist yet — `let state = Self { ... }`
// runs later. We need a value type that can be constructed from just
// `Arc<ApprovalQueue>` and that implements `DaemonApi`.
//
// `AppState` later implements `DaemonApi` by delegating to this same
// adapter, so the meta-capability check + queue insertion logic lives
// in exactly one place.
// =============================================================================

/// Thin adapter that exposes an `Arc<ApprovalQueue>` as the
/// `DaemonApi` trait. See module-level docs for the construction
/// ordering constraint this addresses.
#[derive(Clone, Debug)]
pub struct ApprovalQueueApi {
    queue: Arc<ApprovalQueue>,
}

impl ApprovalQueueApi {
    /// Wrap an `ApprovalQueue` as a `DaemonApi`.
    #[must_use]
    pub fn new(queue: Arc<ApprovalQueue>) -> Self {
        Self { queue }
    }
}

#[async_trait]
impl crate::daemon::api::DaemonApi for ApprovalQueueApi {
    async fn request_self_modify(
        &self,
        op: SelfModifyOp,
        ctx: crate::daemon::api::SelfModifyContext,
    ) -> Result<RequestId, SelfModifyError> {
        // Meta-capability immutability. Reject before reaching the
        // queue so the user's inbox never sees these ops.
        if let SelfModifyOp::GrantCapability { capability, .. } = &op {
            // Parse `<domain>:<action>`. We accept any non-empty
            // string containing a colon to avoid lockstep with the
            // catalog; an unknown capability just won't grant
            // anything when `peko pending decide --grant` runs
            // (PR #4 `ApprovalEngine::execute`).
            let Some((domain, action)) = capability.split_once(':') else {
                return Err(SelfModifyError::InvalidCapabilityFormat(capability.clone()));
            };
            if domain.is_empty() || action.is_empty() {
                return Err(SelfModifyError::InvalidCapabilityFormat(capability.clone()));
            }
            if domain == "principal" || domain == "runtime" {
                return Err(SelfModifyError::MetaCapabilityForbidden(capability.clone()));
            }
            if domain == "tool" {
                return Err(SelfModifyError::ToolCapabilityNotSelfGrantable(
                    capability.clone(),
                ));
            }
        }

        // Internal callers (cron, daemon-originated) may pass
        // `PrincipalId::system()`; agent-driven callers (peko_self
        // from a running agent) pass the active principal's id.
        // Both are accepted at the queue level — the meta-capability
        // gate above is the only structural protection.
        let principal_id = ctx.principal_id;
        let request = ApprovalRequest::from_op(op, principal_id);
        self.queue.insert(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::api::{RequestId, SelfModifyOp};
    use peko_subject::PrincipalId;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    fn mk_queue() -> (TempDir, Arc<ApprovalQueue>) {
        let dir = TempDir::new().unwrap();
        let q = ApprovalQueue::new(dir.path().to_path_buf(), DEFAULT_MAX_PENDING);
        (dir, q)
    }

    fn system_principal() -> PrincipalId {
        PrincipalId::system().clone()
    }

    fn gr_op(reason: &str) -> SelfModifyOp {
        SelfModifyOp::GrantCapability {
            capability: "fs:read".into(),
            reason: reason.into(),
        }
    }

    #[test]
    fn insert_returns_id_and_persists_atomically() {
        let (_dir, q) = mk_queue();
        let req = ApprovalRequest::from_op(gr_op("need to read /tmp"), system_principal());
        let id = q.insert(req).unwrap();
        assert_eq!(q.get(id).unwrap().status, ApprovalStatus::Pending);

        let path = q.artifact_path(id);
        assert!(path.exists(), "artifact file should exist");
        let meta = std::fs::metadata(&path).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn insert_enforces_capacity() {
        let dir = TempDir::new().unwrap();
        // cap of 2 to make the limit observable
        let q = ApprovalQueue::new(dir.path().to_path_buf(), 2);
        q.insert(ApprovalRequest::from_op(gr_op("a"), system_principal())).unwrap();
        q.insert(ApprovalRequest::from_op(gr_op("b"), system_principal())).unwrap();
        let err = q.insert(ApprovalRequest::from_op(gr_op("c"), system_principal())).unwrap_err();
        assert_eq!(err, SelfModifyError::QueueFull(2));
    }

    #[test]
    fn list_pending_filters_decided() {
        let (_dir, q) = mk_queue();
        let a = q.insert(ApprovalRequest::from_op(gr_op("a"), system_principal())).unwrap();
        let b = q.insert(ApprovalRequest::from_op(gr_op("b"), system_principal())).unwrap();
        let pending = q.list_pending();
        assert_eq!(pending.len(), 2);

        // Approve one; should drop from list_pending.
        q.decide(
            a,
            ApprovalStatus::Approved {
                decided_at_secs: 0,
                by: Subject::Public,
            },
        )
        .unwrap();
        let pending = q.list_pending();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, b);
    }

    #[test]
    fn decide_rejects_double_decision() {
        let (_dir, q) = mk_queue();
        let id = q.insert(ApprovalRequest::from_op(gr_op("a"), system_principal())).unwrap();
        let decided = ApprovalStatus::Approved {
            decided_at_secs: 0,
            by: Subject::Public,
        };
        q.decide(id, decided.clone()).unwrap();
        let err = q.decide(id, decided).unwrap_err();
        assert!(matches!(err, DecisionError::AlreadyDecided(_, _)));
    }

    #[test]
    fn decide_rejects_unknown_id() {
        let (_dir, q) = mk_queue();
        let ghost = Uuid::new_v4();
        let err = q
            .decide(
                ghost,
                ApprovalStatus::Approved {
                    decided_at_secs: 0,
                    by: Subject::Public,
                },
            )
            .unwrap_err();
        assert!(matches!(err, DecisionError::NotFound(_)));
    }

    #[test]
    fn decide_rewrites_artifact_with_new_status() {
        let (_dir, q) = mk_queue();
        let id = q.insert(ApprovalRequest::from_op(gr_op("a"), system_principal())).unwrap();
        let path = q.artifact_path(id);
        let before: ApprovalRequest = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(before.status.is_pending());

        q.decide(
            id,
            ApprovalStatus::Denied {
                decided_at_secs: 0,
                by: Subject::Public,
                reason: "no".into(),
            },
        )
        .unwrap();
        let after: ApprovalRequest = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(!after.status.is_pending());
        match after.status {
            ApprovalStatus::Denied { reason, .. } => assert_eq!(reason, "no"),
            other => panic!("expected Denied, got {other:?}"),
        }
    }

    #[test]
    fn rehydrate_reads_persisted_files() {
        let (_dir, q) = mk_queue();
        let a = q.insert(ApprovalRequest::from_op(gr_op("a"), system_principal())).unwrap();
        let b = q.insert(ApprovalRequest::from_op(gr_op("b"), system_principal())).unwrap();

        // Build a fresh queue over the same directory; rehydrate
        // should re-populate the map.
        let fresh = ApprovalQueue::new(_dir.path().to_path_buf(), DEFAULT_MAX_PENDING);
        let loaded = fresh.rehydrate().unwrap();
        assert_eq!(loaded, 2);
        assert_eq!(fresh.get(a).unwrap().status, ApprovalStatus::Pending);
        assert!(fresh.get(b).is_some());
    }

    #[test]
    fn rehydrate_skips_malformed_files() {
        let dir = TempDir::new().unwrap();
        let q = ApprovalQueue::new(dir.path().to_path_buf(), DEFAULT_MAX_PENDING);
        // Write a junk file alongside a valid one.
        std::fs::write(dir.path().join("garbage.json"), "not json").unwrap();
        let id = q.insert(ApprovalRequest::from_op(gr_op("good"), system_principal())).unwrap();
        let loaded = q.rehydrate().unwrap();
        // Should have loaded only the valid one (garbage.json gets skipped).
        assert_eq!(loaded, 1);
        assert!(q.get(id).is_some());
    }

    #[test]
    fn artifact_path_is_under_persist_root() {
        let (_dir, q) = mk_queue();
        let id = Uuid::new_v4();
        let p = q.artifact_path(id);
        assert!(p.starts_with(q.persist_root()));
        assert!(p.to_string_lossy().ends_with(".json"));
    }

    #[test]
    fn no_temp_files_left_behind_on_success() {
        // Atomic write uses .<uuid>.json.tmp + rename; on success no
        // temp files should remain.
        let (_dir, q) = mk_queue();
        let id = q.insert(ApprovalRequest::from_op(gr_op("a"), system_principal())).unwrap();
        let entries: Vec<_> = std::fs::read_dir(_dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        let tmp_files: Vec<_> = entries.iter().filter(|n| n.contains(".tmp")).collect();
        assert!(
            tmp_files.is_empty(),
            "no temp files should remain; found {tmp_files:?}",
        );
        // The real artifact should exist.
        let _ = q.artifact_path(id);
    }
}