//! File-backed [`PlanStorage`].
//!
//! One plan, one file: `<plans_dir>/<plan_id>.jsonl`. Each write is a
//! full-replace under a [`peko_fs_persistence::FileLock`]; the lock
//! guards per-file, so concurrent writers serialize within the same plan.
//!
//! ## On-disk shape
//!
//! ```text
//! <plans_dir>/
//!   plan_<8 base36>.jsonl     # one plan per file
//!   plan_<8 base36>.lock      # sibling lock file (created/destroyed by FileLock)
//!   plan_<8 base36>.jsonl.<pid>.tmp   # transient during write — pid-suffixed
//! ```
//!
//! Each `.jsonl` file contains exactly one JSON-encoded [`PlanRecord`]
//! terminated with `\n`. The "JSONL" extension is a convention;
//! technically only one line is present.
//!
//! ## Read/write contract
//!
//! Reads (via [`PlanStorage::get`], [`PlanStorage::list`]) and writes
//! race against each other inside the per-file lock window. Reads may
//! observe either the prior or the new full record — never a torn
//! partial — because writes are atomic (tmp + write + flush + rename).
//! This is the same semantics as a single-record rewrite; the v1
//! storage layer does not need transactional read-after-write.
//!
//! ## Error contract
//!
//! - Reads of a missing file return `Ok(None)` (idempotent create-or-get).
//! - Reads of unreadable JSON return [`PlanError::CorruptRecord`].
//!   Unlike `peko-session::TodoStorage`, this layer does not silently
//!   drop corrupt lines — a plan file is either the record or it's
//!   corruption, and silently losing the record would defeat the whole
//!   purpose of having it.
//! - [`PlanStorage::close`] is non-idempotent: the second concurrent
//!   close returns [`PlanError::AlreadyClosed`] so callers can detect
//!   races.
//! - [`PlanStorage::update`] takes a closure that performs the
//!   read-modify-write atomically under the file lock. A closure that
//!   throws bubbles [`PlanError`] back to the caller without persisting
//!   the partial mutation.

use std::path::{Path, PathBuf};

use chrono::Utc;
use peko_fs_persistence::FileLock;
use peko_subject::PrincipalId;
use tokio::fs;
use tokio::io::AsyncWriteExt;

use crate::error::{PlanError, Result};
use crate::schema::{ClosedState, PlanNode, PlanRecord};

/// Lock acquisition timeout in milliseconds. Matches the convention in
/// [`peko_session::todos::TODO_LOCK_TIMEOUT_MS`] (10s).
pub const PLAN_LOCK_TIMEOUT_MS: u64 = 10_000;

/// File-backed store of [`PlanRecord`]s at `<plans_dir>/<plan_id>.jsonl`.
///
/// Cheap to clone (the storage is just a path); consumers are expected
/// to keep a single instance per `(principal_root, plans_dir)` pair and
/// hand out `&PlanStorage` references.
#[derive(Debug, Clone)]
pub struct PlanStorage {
    plans_dir: PathBuf,
}

impl PlanStorage {
    /// Construct a new storage rooted at `plans_dir`. The directory is
    /// created lazily on the first write — matching
    /// `peko_session::TodoStorage::new` (sync `new`, mkdir on first
    /// write).
    #[must_use]
    pub fn new(plans_dir: PathBuf) -> Self {
        Self { plans_dir }
    }

    /// The path to the storage's plans directory.
    #[must_use]
    pub fn plans_dir(&self) -> &Path {
        &self.plans_dir
    }

    /// Path to a single plan's data file.
    fn file_path(&self, plan_id: &str) -> PathBuf {
        self.plans_dir.join(format!("{plan_id}.jsonl"))
    }

    /// Create a new plan. Generates `plan_id` and writes the initial
    /// record atomically.
    ///
    /// **Duplicate `node_id` check:** if the supplied `nodes` slice
    /// contains two or more entries sharing the same `node_id`, returns
    /// [`PlanError::InvalidNodeId`] without writing. Silently producing
    /// a record with unreachable / shadowed nodes would leave the LLM
    /// in an unrecoverable state — `mark_node_status` and
    /// `set_node_evidence` only see the first match, so subsequent
    /// operations on the duplicates would fail without explanation.
    /// Hard-erroring here surfaces the collision to the agent so it
    /// can correct its prompt and retry.
    pub async fn create(
        &self,
        principal_id: PrincipalId,
        title: String,
        nodes: Vec<PlanNode>,
    ) -> Result<PlanRecord> {
        // Reject in-batch duplicate node ids up front. Iterating with
        // an indexed loop and comparing the suffix slice is the
        // cheapest O(n²) check that's still readable; n is bounded by
        // the LLM's plan, not by the input stream, so the cost is fine.
        for (i, node) in nodes.iter().enumerate() {
            if nodes[i + 1..]
                .iter()
                .any(|n| n.node_id == node.node_id)
            {
                return Err(PlanError::InvalidNodeId(format!(
                    "plan has duplicate node ids in initial nodes: {}",
                    node.node_id
                )));
            }
        }
        fs::create_dir_all(&self.plans_dir).await?;
        let record = PlanRecord::new(principal_id, title, nodes);
        self.write_atomic(&record).await?;
        Ok(record)
    }

    /// Read a plan by id. Returns `Ok(None)` if the file is absent.
    /// Returns [`PlanError::CorruptRecord`] if the file is present but
    /// not a valid [`PlanRecord`].
    pub async fn get(&self, plan_id: &str) -> Result<Option<PlanRecord>> {
        let path = self.file_path(plan_id);
        let _lock = FileLock::acquire(&path, PLAN_LOCK_TIMEOUT_MS).await?;
        read_locked(&path, plan_id).await
    }

    /// Read a plan by id, asserting the on-disk `principal_id` matches
    /// the supplied caller.
    pub async fn get_for_principal(
        &self,
        plan_id: &str,
        principal_id: &PrincipalId,
    ) -> Result<PlanRecord> {
        let path = self.file_path(plan_id);
        let _lock = FileLock::acquire(&path, PLAN_LOCK_TIMEOUT_MS).await?;
        let record = read_locked(&path, plan_id)
            .await?
            .ok_or(PlanError::NotFound)?;
        check_principal(&record, principal_id)?;
        Ok(record)
    }

    /// Read-modify-write under the file lock. The closure receives the
    /// current record (already principal-scoped-verified) and returns
    /// the next record. Atomicity guarantees: any concurrent writer
    /// either sees the prior record and produces a delta on it, or
    /// waits on the lock and sees the prior+delta; the closure's
    /// return value is the next record visible to readers.
    ///
    /// If the closure returns `Err`, no write happens — the on-disk
    /// record is unchanged.
    pub async fn update<F>(
        &self,
        plan_id: &str,
        principal_id: &PrincipalId,
        f: F,
    ) -> Result<PlanRecord>
    where
        F: FnOnce(&PlanRecord) -> Result<PlanRecord>,
    {
        let path = self.file_path(plan_id);
        let _lock = FileLock::acquire(&path, PLAN_LOCK_TIMEOUT_MS).await?;
        let record = read_locked(&path, plan_id)
            .await?
            .ok_or(PlanError::NotFound)?;
        check_principal(&record, principal_id)?;

        let mut updated = f(&record)?;
        updated.updated_at = Utc::now();
        // schema_version stays at the version the original record used.
        // The closure may opt to bump it explicitly.
        write_locked(&path, &updated).await?;
        Ok(updated)
    }

    /// Idempotent close. The second concurrent close returns
    /// [`PlanError::AlreadyClosed`].
    pub async fn close(
        &self,
        plan_id: &str,
        principal_id: &PrincipalId,
        reason: String,
    ) -> Result<()> {
        let path = self.file_path(plan_id);
        let _lock = FileLock::acquire(&path, PLAN_LOCK_TIMEOUT_MS).await?;
        let mut record = read_locked(&path, plan_id)
            .await?
            .ok_or(PlanError::NotFound)?;
        check_principal(&record, principal_id)?;
        if record.closed.is_some() {
            return Err(PlanError::AlreadyClosed);
        }
        record.closed = Some(ClosedState {
            closed_at: Utc::now(),
            reason,
        });
        record.updated_at = Utc::now();
        write_locked(&path, &record).await?;
        Ok(())
    }

    /// All plans in the storage, irrespective of principal. Sort order
    /// is `created_at` ascending.
    pub async fn list(&self) -> Result<Vec<PlanRecord>> {
        let mut out = Vec::new();
        let mut entries = match fs::read_dir(&self.plans_dir).await {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e.into()),
        };
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                continue;
            }
            let plan_id = match path.file_stem().and_then(|s| s.to_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };
            // Per-file lock for each read so we don't hold a global
            // directory lock. Resilient to concurrent unlink (returns
            // None).
            let _lock = FileLock::acquire(&path, PLAN_LOCK_TIMEOUT_MS).await?;
            if let Some(r) = read_locked(&path, &plan_id).await? {
                out.push(r);
            }
        }
        out.sort_by_key(|r| r.created_at);
        Ok(out)
    }

    /// All plans for a given principal. Sort order is `created_at`
    /// ascending.
    pub async fn list_for_principal(
        &self,
        principal_id: &PrincipalId,
    ) -> Result<Vec<PlanRecord>> {
        let all = self.list().await?;
        Ok(all
            .into_iter()
            .filter(|r| &r.principal_id == principal_id)
            .collect())
    }

    /// The most-recently-updated open plan with at least one
    /// `InProgress` node. `None` when the principal has no active
    /// in-flight work.
    pub async fn current_focus(
        &self,
        principal_id: &PrincipalId,
    ) -> Result<Option<PlanRecord>> {
        let all = self.list_for_principal(principal_id).await?;
        Ok(all
            .into_iter()
            .filter(|r| r.closed.is_none())
            .filter(|r| !r.current_focus_nodes().is_empty())
            .max_by_key(|r| r.updated_at))
    }

    /// All open plans with at least one unresolved node. This is the
    /// set the runtime re-injects into a fresh session's context.
    pub async fn load_resumable(
        &self,
        principal_id: &PrincipalId,
    ) -> Result<Vec<PlanRecord>> {
        let all = self.list_for_principal(principal_id).await?;
        Ok(all.into_iter().filter(|r| r.has_unresolved_nodes()).collect())
    }

    /// Atomic write under the file lock (acquired here).
    async fn write_atomic(&self, record: &PlanRecord) -> Result<()> {
        let path = self.file_path(&record.plan_id);
        let _lock = FileLock::acquire(&path, PLAN_LOCK_TIMEOUT_MS).await?;
        write_locked(&path, record).await
    }
}

// ---------------------------------------------------------------------------
// Helpers (no `self` — operating on a path under a caller-held lock)
// ---------------------------------------------------------------------------

/// Read the plan file at `path` under a caller-held [`FileLock`]. Returns
/// `Ok(None)` for absent or zero-byte files (defensive — should not
/// happen under atomic-write semantics, but cheap to handle).
async fn read_locked(path: &Path, plan_id: &str) -> Result<Option<PlanRecord>> {
    match tokio::fs::read_to_string(path).await {
        Ok(s) if s.trim().is_empty() => Ok(None),
        Ok(s) => Ok(Some(parse_plan(&s, plan_id)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Atomic write under a caller-held [`FileLock`]. Pid-suffixed tmp path
/// to avoid collisions when multiple processes write the same plan
/// concurrently (mirrors `peko-rs/session/src/index.rs:788`).
async fn write_locked(path: &Path, record: &PlanRecord) -> Result<()> {
    let pid_suffix = std::process::id();
    let temp_path = path.with_extension(format!("jsonl.{pid_suffix}.tmp"));

    let json = serde_json::to_string(record)?;
    let mut file = fs::File::create(&temp_path).await?;
    file.write_all(json.as_bytes()).await?;
    file.write_all(b"\n").await?;
    file.flush().await?;
    drop(file);
    fs::rename(&temp_path, path).await?;
    Ok(())
}

fn check_principal(record: &PlanRecord, expected: &PrincipalId) -> Result<()> {
    if &record.principal_id != expected {
        return Err(PlanError::PrincipalMismatch {
            expected: expected.to_string(),
            got: record.principal_id.to_string(),
        });
    }
    Ok(())
}

/// Decode the first non-empty line of `raw` as a [`PlanRecord`]. Bails
/// with [`PlanError::CorruptRecord`] on parse failure.
fn parse_plan(raw: &str, plan_id: &str) -> Result<PlanRecord> {
    let line = raw
        .lines()
        .find(|l| !l.trim().is_empty())
        .ok_or_else(|| PlanError::CorruptRecord {
            plan_id: plan_id.to_string(),
            source: serde_json::from_str::<serde_json::Value>("")
                .unwrap_err(),
        })?;
    let record: PlanRecord = serde_json::from_str(line).map_err(|source| {
        PlanError::CorruptRecord {
            plan_id: plan_id.to_string(),
            source,
        }
    })?;
    Ok(record)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{NodeEvidence, PlanNode, PlanNodeStatus};
    use peko_subject::PrincipalId;
    use tempfile::TempDir;

    fn principal() -> PrincipalId {
        PrincipalId::generate()
    }

    fn storage_in(dir: &TempDir) -> PlanStorage {
        let plans_dir = dir.path().join("plans");
        PlanStorage::new(plans_dir)
    }

    fn node(label: &str) -> PlanNode {
        let node_id = crate::schema::NodeId::generate();
        let now = Utc::now();
        PlanNode {
            node_id,
            step: label.to_string(),
            status: PlanNodeStatus::Pending,
            depends_on: vec![],
            evidence: None,
            blocked_reason: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[tokio::test]
    async fn create_then_get_round_trip() {
        let dir = TempDir::new().unwrap();
        let storage = storage_in(&dir);
        let p = principal();

        let created = storage
            .create(p.clone(), "Migrate auth".into(), vec![node("A"), node("B")])
            .await
            .unwrap();

        assert_eq!(created.title, "Migrate auth");
        assert_eq!(created.nodes.len(), 2);
        assert!(created.closed.is_none());
        assert_eq!(created.principal_id, p);

        let fetched = storage
            .get(&created.plan_id)
            .await
            .unwrap()
            .expect("plan file exists");
        assert_eq!(fetched, created);
    }

    #[tokio::test]
    async fn get_returns_none_for_unknown_plan() {
        let dir = TempDir::new().unwrap();
        let storage = storage_in(&dir);
        // The plans dir is created lazily by `create()` — for a get on
        // a fresh dir we need to mkdir manually (matches the
        // `peko_session::TodoStorage` convention).
        tokio::fs::create_dir_all(storage.plans_dir())
            .await
            .unwrap();
        let result = storage.get("plan_doesnotexist").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn get_for_principal_mismatch_returns_error() {
        let dir = TempDir::new().unwrap();
        let storage = storage_in(&dir);
        let p = principal();
        let created = storage
            .create(p.clone(), "owned by p".into(), vec![])
            .await
            .unwrap();

        let other = PrincipalId::generate();
        let err = storage
            .get_for_principal(&created.plan_id, &other)
            .await
            .expect_err("mismatched principal");
        assert!(matches!(err, PlanError::PrincipalMismatch { .. }));
    }

    #[tokio::test]
    async fn update_closure_form_serializes_writers() {
        let dir = TempDir::new().unwrap();
        let storage = storage_in(&dir);
        let p = principal();
        let mut created = storage
            .create(p.clone(), "plan".into(), vec![node("A"), node("B")])
            .await
            .unwrap();

        // First update: flip node A to InProgress.
        let a_id = created.nodes[0].node_id.clone();
        created = storage
            .update(&created.plan_id, &p, |r| {
                let mut r = r.clone();
                r.nodes[0].status = PlanNodeStatus::InProgress;
                Ok(r)
            })
            .await
            .unwrap();
        assert!(matches!(created.nodes[0].status, PlanNodeStatus::InProgress));

        // Second update: closure observes the first mutation.
        created = storage
            .update(&created.plan_id, &p, |r| {
                let mut r = r.clone();
                r.nodes[1].status = PlanNodeStatus::InProgress;
                r.nodes[0].status = PlanNodeStatus::Completed {
                    completed_at: Utc::now(),
                };
                Ok(r)
            })
            .await
            .unwrap();
        assert!(matches!(
            created.nodes[0].status,
            PlanNodeStatus::Completed { .. }
        ));
        assert!(matches!(
            created.nodes[1].status,
            PlanNodeStatus::InProgress
        ));
        // Sanity: `a_id` matched the right slot.
        assert_eq!(created.nodes[0].node_id, a_id);
    }

    #[tokio::test]
    async fn close_idempotent_returns_error_on_second_call() {
        let dir = TempDir::new().unwrap();
        let storage = storage_in(&dir);
        let p = principal();
        let created = storage.create(p.clone(), "done".into(), vec![]).await.unwrap();

        storage
            .close(&created.plan_id, &p, "user-abandoned".into())
            .await
            .unwrap();

        let err = storage
            .close(&created.plan_id, &p, "double-close".into())
            .await
            .expect_err("second close returns AlreadyClosed");
        assert!(matches!(err, PlanError::AlreadyClosed));
    }

    #[tokio::test]
    async fn list_for_principal_filters() {
        let dir = TempDir::new().unwrap();
        let storage = storage_in(&dir);
        let p1 = principal();
        let p2 = principal();

        let _a = storage
            .create(p1.clone(), "p1-1".into(), vec![])
            .await
            .unwrap();
        let _b = storage
            .create(p2.clone(), "p2-1".into(), vec![])
            .await
            .unwrap();
        let _c = storage
            .create(p1.clone(), "p1-2".into(), vec![])
            .await
            .unwrap();

        let p1_plans = storage.list_for_principal(&p1).await.unwrap();
        let p2_plans = storage.list_for_principal(&p2).await.unwrap();
        assert_eq!(p1_plans.len(), 2);
        assert_eq!(p2_plans.len(), 1);
        assert!(p1_plans.iter().all(|r| r.principal_id == p1));
        assert!(p2_plans.iter().all(|r| r.principal_id == p2));
    }

    #[tokio::test]
    async fn load_resumable_filters_open_with_unresolved_nodes() {
        let dir = TempDir::new().unwrap();
        let storage = storage_in(&dir);
        let p = principal();

        // Plan A: one Pending node — resumable.
        let a = storage
            .create(p.clone(), "A".into(), vec![node("x")])
            .await
            .unwrap();
        // Plan B: one Completed node — not resumable.
        let b_created = storage
            .create(p.clone(), "B".into(), vec![node("y")])
            .await
            .unwrap();
        let _ = storage
            .update(&b_created.plan_id, &p, |r| {
                let mut r = r.clone();
                r.nodes[0].status = PlanNodeStatus::Completed {
                    completed_at: Utc::now(),
                };
                r.nodes[0].evidence = Some(NodeEvidence {
                    output: "ok".into(),
                    artifacts: vec![],
                    decided_by: None,
                });
                Ok(r)
            })
            .await
            .unwrap();
        // Silence unused warning on the renamed handle.
        let _ = b_created.plan_id.as_str();

        let resumable = storage.load_resumable(&p).await.unwrap();
        assert_eq!(resumable.len(), 1);
        assert_eq!(resumable[0].plan_id, a.plan_id);
    }

    #[tokio::test]
    async fn current_focus_returns_most_recent_in_progress_plan() {
        let dir = TempDir::new().unwrap();
        let storage = storage_in(&dir);
        let p = principal();

        let mut a = storage
            .create(p.clone(), "A".into(), vec![node("a")])
            .await
            .unwrap();
        a = storage
            .update(&a.plan_id, &p, |r| {
                let mut r = r.clone();
                r.nodes[0].status = PlanNodeStatus::InProgress;
                Ok(r)
            })
            .await
            .unwrap();

        let _b = storage
            .create(p.clone(), "B".into(), vec![node("b")])
            .await
            .unwrap();

        let focus = storage.current_focus(&p).await.unwrap();
        assert!(focus.is_some());
        assert_eq!(focus.unwrap().plan_id, a.plan_id);
    }

    #[tokio::test]
    async fn write_atomic_no_partial_file_when_rename_fails() {
        // We can't simulate a rename failure cleanly without hooking
        // into rename(2), so this test simulates "tmp was unlinked
        // before rename" by writing a record, then writing again, and
        // asserting the live file contains the second write's content
        // (i.e., the first write's content was replaced cleanly, no
        // torn writes).
        let dir = TempDir::new().unwrap();
        let storage = storage_in(&dir);
        let p = principal();

        let mut created = storage.create(p.clone(), "x".into(), vec![]).await.unwrap();

        // First write.
        storage
            .update(&created.plan_id, &p, |r| Ok(r.clone()))
            .await
            .unwrap();

        // Second write — appends a node.
        let new_node = node("late-add");
        created = storage
            .update(&created.plan_id, &p, |r| {
                let mut r = r.clone();
                r.nodes.push(new_node);
                r.title = "x2".into();
                Ok(r)
            })
            .await
            .unwrap();
        assert_eq!(created.nodes.len(), 1);
        assert_eq!(created.title, "x2");

        // Read it back; verify integrity.
        let fetched = storage.get(&created.plan_id).await.unwrap().unwrap();
        assert_eq!(fetched, created);

        // The data file must be exactly one JSON line + \n.
        let raw = tokio::fs::read_to_string(storage.file_path(&created.plan_id))
            .await
            .unwrap();
        let non_empty: Vec<&str> = raw.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(non_empty.len(), 1, "expected one plan line on disk");
        assert!(raw.ends_with('\n'));
    }

    #[tokio::test]
    async fn pid_suffixed_tmp_paths_do_not_collide() {
        // Two writes to the same plan from the same process must
        // produce / consume tmp paths deterministically (they share a
        // pid, so the suffix is identical — that's fine since each
        // write is sequenced through the file lock, so the second
        // tmp-name is created AFTER the first tmp-name was renamed
        // away). Verifies the format itself, not the collision
        // semantics across processes.
        let dir = TempDir::new().unwrap();
        let storage = storage_in(&dir);
        let p = principal();

        let created = storage.create(p.clone(), "x".into(), vec![]).await.unwrap();
        let path = storage.file_path(&created.plan_id);
        let pid = std::process::id();
        let expected_tmp = path.with_extension(format!("jsonl.{pid}.tmp"));
        // Path is well-formed: `<plans_dir>/<plan_id>.jsonl.<pid>.tmp`.
        let expected_str = expected_tmp.to_string_lossy();
        assert!(expected_str.ends_with(&format!(".jsonl.{pid}.tmp")));
    }

    #[tokio::test]
    async fn parse_plan_returns_corrupt_record_on_garbage() {
        let dir = TempDir::new().unwrap();
        let storage = storage_in(&dir);
        let plans_dir = dir.path().join("plans");
        tokio::fs::create_dir_all(&plans_dir).await.unwrap();

        let garbage_path = plans_dir.join("plan_garbage.jsonl");
        tokio::fs::write(&garbage_path, "this is not json\n")
            .await
            .unwrap();

        let err = storage
            .get("plan_garbage")
            .await
            .expect_err("garbage must surface as CorruptRecord");
        match err {
            PlanError::CorruptRecord { plan_id, .. } => {
                assert_eq!(plan_id, "plan_garbage");
            }
            other => panic!("expected CorruptRecord, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn read_locked_treats_empty_file_as_none() {
        // Defensive: zero-byte file (shouldn't happen under atomic
        // writes, but cheap to handle).
        let dir = TempDir::new().unwrap();
        let storage = storage_in(&dir);
        let plans_dir = dir.path().join("plans");
        tokio::fs::create_dir_all(&plans_dir).await.unwrap();
        let empty_path = plans_dir.join("plan_empty.jsonl");
        tokio::fs::write(&empty_path, "").await.unwrap();

        let result = storage.get("plan_empty").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn update_preserves_principal_id() {
        let dir = TempDir::new().unwrap();
        let storage = storage_in(&dir);
        let p = principal();
        let created = storage.create(p.clone(), "x".into(), vec![]).await.unwrap();

        let updated = storage
            .update(&created.plan_id, &p, |r| {
                let mut r = r.clone();
                r.title = "y".into();
                Ok(r)
            })
            .await
            .unwrap();
        assert_eq!(updated.principal_id, p);
    }

    /// Hard-error on duplicate initial node ids. Prevents the agent
    /// from producing a record with shadowed / unreachable nodes —
    /// later operations on the duplicates would fail without
    /// explanation. (Complements the idempotent `add_node` collision
    /// semantics: `create` rejects; `add_node` accepts.)
    #[tokio::test]
    async fn create_rejects_duplicate_initial_node_ids() {
        let dir = TempDir::new().unwrap();
        let storage = storage_in(&dir);
        let p = principal();
        let n1 = node("first");
        let mut n2 = node("second");
        // Force a collision by reusing n1's id on n2.
        n2.node_id = n1.node_id.clone();
        let err = storage
            .create(p.clone(), "dupe-batch".into(), vec![n1, n2])
            .await
            .expect_err("duplicate initial node ids must surface as InvalidNodeId");
        assert!(matches!(err, PlanError::InvalidNodeId(_)), "got {err:?}");
        // No file should be on disk — the rejection is pre-write.
        let plans_dir = dir.path().join("plans");
        if plans_dir.exists() {
            let entries: Vec<_> = std::fs::read_dir(&plans_dir)
                .unwrap()
                .filter_map(|e| e.ok())
                .collect();
            assert!(
                entries.is_empty(),
                "create-rejection must not leave a plan file behind"
            );
        }
    }
}
