//! `chapters.json` — pending chapter changes for live conversational
//! sessions (agent-owned session management, plan D1).
//!
//! The `session` tool writes a pending chapter change for a live
//! session id (`root:{peer}` / `root:cron:{peer}`); the principal's
//! `agent_runner` consumes it at the next run start, *before*
//! open/create, under the existing `session_creation_lock`:
//!
//! - `New` — rename the live id to a chapter id (`{live}#{UTC-ts}`),
//!   then the normal create path mints a fresh live session.
//! - `Resume` — rename the live id to a chapter id, then rename the
//!   target chapter/session back onto the live id.
//!
//! Because the live id is *reused*, `InboxRegistry` mappings, queued
//! steering, and subagent completion announcements keyed by
//! `root:{peer}` are untouched; messages arriving after the request
//! land in the new chapter. The sidecar is durable — a restart
//! between request and the next message loses nothing.
//!
//! ## Atomic-write pattern
//!
//! Mirrors `peko-rs/core/src/daemon/config_drift.rs` and
//! `peko-rs/core/src/principal/seen_models.rs`: serialize → write a
//! per-PID sibling tmp file → fsync → `rename(2)` over the target. A
//! crash before the rename leaves the previous good copy intact.
//!
//! ## Tolerance
//!
//! A missing or corrupt `chapters.json` loads as an empty map (warn
//! on corruption, never fail the caller) — mirrors the
//! `seen_models.rs` fall-open pattern. A `request` on a corrupt file
//! therefore replaces it with just the new entry.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::key::safe_filename_component;

/// A pending chapter change for one live session id.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ChapterRequest {
    /// Rotate the live session into a chapter and start fresh.
    /// `title` (when set) is applied to the archived chapter's
    /// metadata as a best-effort label.
    New { title: Option<String> },
    /// Rotate the live session into a chapter and resume `target`
    /// (a chapter or session id) under the live id.
    Resume { target: String },
}

/// On-disk shape: map of live session id → pending request.
/// `BTreeMap` so the serialized form is stable (sorted) across saves.
type ChapterMap = BTreeMap<String, ChapterRequest>;

/// Resolve the canonical `chapters.json` path for a sessions dir.
#[must_use]
pub fn chapters_path(sessions_dir: &Path) -> PathBuf {
    sessions_dir.join("chapters.json")
}

/// Load the pending-request map. Missing file ⇒ empty map; corrupt
/// file ⇒ warn + empty map (fall open, mirroring `seen_models.rs`).
fn load(path: &Path) -> ChapterMap {
    match std::fs::read(path) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(map) => map,
            Err(e) => {
                tracing::warn!(
                    "chapters.json at {} is corrupt ({}); treating as empty",
                    path.display(),
                    e
                );
                ChapterMap::new()
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => ChapterMap::new(),
        Err(e) => {
            tracing::warn!(
                "chapters.json at {} unreadable ({}); treating as empty",
                path.display(),
                e
            );
            ChapterMap::new()
        }
    }
}

/// Atomic save: serialize → per-PID `.tmp` → fsync → rename over the
/// real file (same shape as `SessionIndex::write_json_atomic`).
/// Creates the parent dir if missing.
fn save(path: &Path, map: &ChapterMap) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create parent dir {}", parent.display()))?;
    }
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    let json = serde_json::to_vec_pretty(map).context("serialize chapters.json")?;
    {
        let mut file =
            std::fs::File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
        file.write_all(&json)
            .with_context(|| format!("write {}", tmp.display()))?;
        file.sync_all()
            .with_context(|| format!("fsync {}", tmp.display()))?;
    }
    std::fs::rename(&tmp, path)
        .with_context(|| format!("rename {} → {}", tmp.display(), path.display()))?;
    Ok(())
}

/// Record (or replace) a pending chapter change for `live_id`.
pub fn request(sessions_dir: &Path, live_id: &str, req: ChapterRequest) -> Result<()> {
    let path = chapters_path(sessions_dir);
    let mut map = load(&path);
    map.insert(live_id.to_string(), req);
    save(&path, &map)
}

/// Read-and-clear the pending chapter change for `live_id`.
///
/// Returns `Ok(None)` when no request is pending (including missing
/// or corrupt `chapters.json`). When an entry was consumed the file
/// is rewritten without it; an emptied map is left on disk.
pub fn take(sessions_dir: &Path, live_id: &str) -> Result<Option<ChapterRequest>> {
    let path = chapters_path(sessions_dir);
    let mut map = load(&path);
    let req = map.remove(live_id);
    if req.is_some() {
        save(&path, &map)?;
    }
    Ok(req)
}

/// Transactional `take`: remove the pending chapter change for
/// `live_id`, hand it to `f`, and only persist the removal if `f`
/// succeeds. When `f` returns an error the entry is restored to the
/// sidecar and the error is propagated, so the next run retries the
/// rotation instead of silently dropping the user's request.
///
/// `Ok(None)` ⇒ no pending request (closure not invoked).
/// `Ok(Some(v))` ⇒ request was consumed and `f` returned `v`.
/// `Err(e)` ⇒ `f` failed; the entry has been restored to the file.
pub async fn consume<F, T>(
    sessions_dir: &Path,
    live_id: &str,
    f: F,
) -> Result<Option<T>>
where
    F: AsyncFnOnce(ChapterRequest) -> Result<T>,
{
    let path = chapters_path(sessions_dir);
    let mut map = load(&path);
    let Some(req) = map.remove(live_id) else {
        return Ok(None);
    };
    // Keep a clone for the rollback path; `f` consumes its argument.
    let rollback = req.clone();
    match f(req).await {
        Ok(v) => {
            save(&path, &map)?;
            Ok(Some(v))
        }
        Err(e) => {
            // Rollback: re-insert the entry so the next run retries.
            map.insert(live_id.to_string(), rollback);
            save(&path, &map)?;
            Err(e)
        }
    }
}

/// Derive the chapter id for a live session id being rotated out:
/// `{live}#{YYYYMMDD-HHMMSS}` in UTC. When that id's transcript file
/// already exists in `sessions_dir` (two rotations within the same
/// second), a short uuid suffix (`-{8 hex}`) disambiguates.
#[must_use]
pub fn chapter_id(sessions_dir: &Path, live_id: &str) -> String {
    let base = format!("{live_id}#{}", chrono::Utc::now().format("%Y%m%d-%H%M%S"));
    if !transcript_exists(sessions_dir, &base) {
        return base;
    }
    let suffix = &uuid::Uuid::new_v4().simple().to_string()[..8];
    format!("{base}-{suffix}")
}

/// Whether a transcript JSONL exists for `session_id` in `sessions_dir`
/// (filename derived with the same `safe_filename_component` mapping
/// `SessionStorage` uses).
fn transcript_exists(sessions_dir: &Path, session_id: &str) -> bool {
    sessions_dir
        .join(format!("{}.jsonl", safe_filename_component(session_id)))
        .exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn request_take_round_trip() {
        let dir = TempDir::new().unwrap();

        request(
            dir.path(),
            "root:user:alice",
            ChapterRequest::New {
                title: Some("morning".to_string()),
            },
        )
        .unwrap();

        let taken = take(dir.path(), "root:user:alice").unwrap();
        assert_eq!(
            taken,
            Some(ChapterRequest::New {
                title: Some("morning".to_string())
            })
        );

        // The entry is consumed: a second take returns None.
        assert_eq!(take(dir.path(), "root:user:alice").unwrap(), None);
        // The sidecar file itself is left behind (emptied map).
        assert!(chapters_path(dir.path()).exists());
    }

    #[test]
    fn take_on_missing_file_returns_none() {
        let dir = TempDir::new().unwrap();
        assert!(!chapters_path(dir.path()).exists());
        assert_eq!(take(dir.path(), "root:user:alice").unwrap(), None);
        // A take that consumed nothing must not create the file.
        assert!(!chapters_path(dir.path()).exists());
    }

    #[test]
    fn take_on_corrupt_file_returns_none() {
        let dir = TempDir::new().unwrap();
        std::fs::write(chapters_path(dir.path()), "{ not json !!!").unwrap();
        assert_eq!(take(dir.path(), "root:user:alice").unwrap(), None);
    }

    #[test]
    fn request_on_corrupt_file_replaces_it() {
        let dir = TempDir::new().unwrap();
        std::fs::write(chapters_path(dir.path()), "garbage").unwrap();

        request(
            dir.path(),
            "root:user:alice",
            ChapterRequest::New { title: None },
        )
        .unwrap();
        assert_eq!(
            take(dir.path(), "root:user:alice").unwrap(),
            Some(ChapterRequest::New { title: None })
        );
    }

    #[test]
    fn two_live_ids_are_independent() {
        let dir = TempDir::new().unwrap();

        request(
            dir.path(),
            "root:user:alice",
            ChapterRequest::New { title: None },
        )
        .unwrap();
        request(
            dir.path(),
            "root:cron:alice",
            ChapterRequest::Resume {
                target: "root:cron:alice#20260101-000000".to_string(),
            },
        )
        .unwrap();

        // Taking one leaves the other intact.
        assert_eq!(
            take(dir.path(), "root:user:alice").unwrap(),
            Some(ChapterRequest::New { title: None })
        );
        assert_eq!(
            take(dir.path(), "root:cron:alice").unwrap(),
            Some(ChapterRequest::Resume {
                target: "root:cron:alice#20260101-000000".to_string()
            })
        );
        assert_eq!(take(dir.path(), "root:user:alice").unwrap(), None);
    }

    #[test]
    fn request_replaces_pending_entry_for_same_live_id() {
        let dir = TempDir::new().unwrap();

        request(
            dir.path(),
            "root:user:alice",
            ChapterRequest::New {
                title: Some("first".to_string()),
            },
        )
        .unwrap();
        request(
            dir.path(),
            "root:user:alice",
            ChapterRequest::Resume {
                target: "sess_old".to_string(),
            },
        )
        .unwrap();

        // Latest write wins; only one entry exists.
        assert_eq!(
            take(dir.path(), "root:user:alice").unwrap(),
            Some(ChapterRequest::Resume {
                target: "sess_old".to_string()
            })
        );
        assert_eq!(take(dir.path(), "root:user:alice").unwrap(), None);
    }

    #[tokio::test]
    async fn consume_success_removes_entry() {
        let dir = TempDir::new().unwrap();

        request(
            dir.path(),
            "root:user:alice",
            ChapterRequest::New {
                title: Some("morning".to_string()),
            },
        )
        .unwrap();

        // Closure receives the entry and returns Ok — file is rewritten
        // without it.
        let observed = consume(dir.path(), "root:user:alice", async |req| {
            assert_eq!(
                req,
                ChapterRequest::New {
                    title: Some("morning".to_string())
                }
            );
            Ok::<_, anyhow::Error>(())
        })
        .await
        .unwrap();
        assert_eq!(observed, Some(()));

        // The entry is consumed: a second take returns None.
        assert_eq!(take(dir.path(), "root:user:alice").unwrap(), None);
    }

    #[tokio::test]
    async fn consume_no_entry_is_a_noop() {
        let dir = TempDir::new().unwrap();
        // Closure never runs, no file is touched.
        let observed: Option<()> = consume(dir.path(), "root:user:alice", async |_| -> Result<(), anyhow::Error> {
            panic!("closure must not run when no entry is pending")
        })
        .await
        .unwrap();
        assert_eq!(observed, None);
        // No file should have been created.
        assert!(!chapters_path(dir.path()).exists());
    }

    #[tokio::test]
    async fn consume_failure_restores_entry_for_retry() {
        let dir = TempDir::new().unwrap();

        request(
            dir.path(),
            "root:user:alice",
            ChapterRequest::Resume {
                target: "sess_old".to_string(),
            },
        )
        .unwrap();

        // Simulate the rename failing (e.g. cross-device rename, ENOSPC):
        // the entry must be restored so the next run retries.
        let err = consume(dir.path(), "root:user:alice", async |req| {
            // Touch the request to prove the closure received it.
            assert_eq!(
                req,
                ChapterRequest::Resume {
                    target: "sess_old".to_string()
                }
            );
            Err::<(), _>(anyhow::anyhow!("rename failed"))
        })
        .await
        .unwrap_err();
        assert_eq!(err.to_string(), "rename failed");

        // The entry was rolled back; a follow-up take returns it intact.
        assert_eq!(
            take(dir.path(), "root:user:alice").unwrap(),
            Some(ChapterRequest::Resume {
                target: "sess_old".to_string()
            })
        );
    }

    #[test]
    fn chapter_id_format() {
        let dir = TempDir::new().unwrap();
        let id = chapter_id(dir.path(), "root:user:alice");

        let (live, ts) = id.split_once('#').expect("chapter id contains '#'");
        assert_eq!(live, "root:user:alice");
        // YYYYMMDD-HHMMSS: 8 digits, '-', 6 digits.
        assert_eq!(ts.len(), 15);
        let (date, time) = ts.split_once('-').expect("ts contains '-'");
        assert_eq!(date.len(), 8);
        assert!(date.chars().all(|c| c.is_ascii_digit()));
        assert_eq!(time.len(), 6);
        assert!(time.chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn chapter_id_collision_gets_uuid_suffix() {
        let dir = TempDir::new().unwrap();
        let live = "root:user:alice";

        let first = chapter_id(dir.path(), live);
        // Simulate a transcript already rotated to that chapter id.
        std::fs::write(
            dir.path()
                .join(format!("{}.jsonl", safe_filename_component(&first))),
            "{}\n",
        )
        .unwrap();

        let second = chapter_id(dir.path(), live);
        assert_ne!(first, second);
        // Same second ⇒ same base, so the collision path must append
        // `-{8 hex}`. (If the wall clock ticked over, the new base
        // differs before the '#'-suffix compare and this check is
        // simply skipped — no flake.)
        if let Some(suffix) = second.strip_prefix(&first) {
            assert_eq!(suffix.len(), 9);
            assert!(suffix.starts_with('-'));
            assert!(suffix[1..].chars().all(|c| c.is_ascii_hexdigit()));
        }
    }
}
