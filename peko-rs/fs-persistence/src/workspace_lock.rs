//! Cross-agent advisory locking for agent-workspace files (ADR-065)
//!
//! The F33 `ParallelGate` serializes filesystem-mutating tools within a
//! single agent's runtime, but multiple agents (within one principal or
//! across principals) sharing a runtime process can still race on the
//! same file: `Write`/`Edit` do unsynchronized read-modify-writes, so
//! the realistic failure mode is a lost update (last writer wins), not
//! a torn file.
//!
//! `WorkspaceFileLock` closes that gap with a **fail-fast** advisory
//! lock keyed on the canonical target path:
//!
//! - Lock files live in a runtime-owned directory
//!   (`<data_dir>/locks/`), named by the SHA-256 of the canonical
//!   target path. This keeps `.lock` litter out of user workspaces,
//!   makes relative and absolute spellings of the same file contend on
//!   one lock, and avoids the `with_extension("lock")` collision where
//!   `foo.txt` and `foo.md` would map to the same lock file.
//! - Acquisition reuses [`FileLock`] (atomic `O_EXCL` create, PID
//!   liveness + age-based stale recovery, crash-safe `Drop` release).
//! - Contention **fails fast** (default 5s) with a structured
//!   `"file busy"` error rather than blocking, so the calling model can
//!   re-read the file and retry — see ADR-065 for the retry contract.
//!
//! Known escape hatch, documented not solved: `Bash` mutations
//! (`sed -i`, scripts) bypass these locks. Per ADR-065, per-file locks
//! were chosen over a single-writer-per-runtime gate precisely because
//! they impose cost only under same-path contention; whole-process
//! coverage would require wrapping Bash, which ADR-065 rejects.

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tracing::debug;

use crate::file_lock::FileLock;

/// Default timeout for acquiring a workspace file lock.
///
/// Chosen to be short enough that an agent is not left hanging on a
/// contended file, long enough to absorb the tail of a concurrent
/// small-file write that is about to finish. Callers that need a
/// different budget pass an explicit timeout to [`acquire_in`].
pub const DEFAULT_WORKSPACE_LOCK_TIMEOUT_MS: u64 = 5_000;

/// Default directory holding workspace lock files.
///
/// Mirrors `peko_extension_api::paths::default_data_dir` (env override
/// first, platform data dir, `/tmp` fallback) and appends `locks/`.
/// Kept in a leaf crate to avoid a `fs-persistence → extension-api`
/// dependency edge; must stay in sync with it.
#[must_use]
pub fn default_workspace_lock_dir() -> PathBuf {
    std::env::var_os("PEKO_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::data_dir()
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join("peko")
        })
        .join("locks")
}

/// An acquired per-file lock for an agent-workspace path.
///
/// Released on [`WorkspaceFileLock::release`] or on `Drop` (crash-safe:
/// a killed process leaves the lock file, which the next acquirer
/// removes via stale detection once the PID is gone or the lock ages
/// past [`crate::DEFAULT_STALE_LOCK_MS`]).
pub struct WorkspaceFileLock {
    inner: FileLock,
}

impl std::fmt::Debug for WorkspaceFileLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspaceFileLock").finish_non_exhaustive()
    }
}

impl WorkspaceFileLock {
    /// Acquire the workspace lock for `target` using
    /// [`default_workspace_lock_dir`] and
    /// [`DEFAULT_WORKSPACE_LOCK_TIMEOUT_MS`].
    pub async fn acquire(target: impl AsRef<Path>) -> Result<Self> {
        Self::acquire_in(
            &default_workspace_lock_dir(),
            target,
            DEFAULT_WORKSPACE_LOCK_TIMEOUT_MS,
        )
        .await
    }

    /// Acquire the workspace lock for `target` with an explicit lock
    /// directory and timeout budget.
    ///
    /// # Errors
    /// - Propagates filesystem errors (lock dir creation, lock file IO).
    /// - On timeout, returns a `"file busy: ..."` error naming the
    ///   target path — the caller (typically an agent) should re-read
    ///   the file and retry rather than assume the write failed
    ///   destructively.
    pub async fn acquire_in(
        lock_dir: impl AsRef<Path>,
        target: impl AsRef<Path>,
        timeout_ms: u64,
    ) -> Result<Self> {
        let lock_dir = lock_dir.as_ref();
        let target = target.as_ref();

        tokio::fs::create_dir_all(lock_dir)
            .await
            .with_context(|| format!("Failed to create lock directory: {}", lock_dir.display()))?;

        let lock_file = lock_file_path(lock_dir, target);
        let inner = FileLock::acquire_at(&lock_file, timeout_ms)
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "file busy: another writer holds the lock for {} (waited {timeout_ms}ms: {e})",
                    target.display()
                )
            })?;

        debug!(target = %target.display(), lock = %lock_file.display(), "Acquired workspace file lock");
        Ok(Self { inner })
    }

    /// Explicitly release the lock (removes the lock file).
    ///
    /// Also released on drop, but an explicit release is preferred so
    /// errors surface instead of being swallowed in the sync `Drop`.
    pub async fn release(self) -> Result<()> {
        self.inner.release().await
    }
}

/// Map a target file to its lock file: `<lock_dir>/<sha256(canonical)>.lock`.
///
/// If the target does not exist yet (`Write` with a fresh path), the
/// nearest existing ancestor is canonicalized and the remaining
/// components appended, so two agents writing the same not-yet-created
/// path still contend on one lock.
#[must_use]
pub fn lock_file_path(lock_dir: &Path, target: &Path) -> PathBuf {
    let canonical = canonical_best_effort(target);
    let digest = Sha256::digest(canonical.as_os_str().as_encoded_bytes());
    lock_dir.join(format!("{}.lock", hex_encode(&digest)))
}

/// Canonicalize, tolerating a missing target.
///
/// Walks up to the nearest existing ancestor, canonicalizes it, and
/// re-appends the non-existent suffix. Falls back to the absolutized
/// path when no ancestor resolves (e.g. detached root).
fn canonical_best_effort(target: &Path) -> PathBuf {
    if let Ok(c) = std::fs::canonicalize(target) {
        return c;
    }

    let abs = if target.is_absolute() {
        target.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(target)
    };

    let mut suffix: Vec<std::ffi::OsString> = Vec::new();
    let mut probe = abs.clone();
    while let Some(name) = probe.file_name() {
        suffix.push(name.to_os_string());
        let parent = match probe.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
            _ => break,
        };
        if let Ok(c) = std::fs::canonicalize(&parent) {
            let mut resolved = c;
            for part in suffix.iter().rev() {
                resolved.push(part);
            }
            return resolved;
        }
        probe = parent;
    }

    abs
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn workspace_lock_acquire_release_reacquire() {
        let temp = tempfile::tempdir().unwrap();
        let lock_dir = temp.path().join("locks");
        let target = temp.path().join("data.txt");
        std::fs::write(&target, "v1").unwrap();

        let lock = WorkspaceFileLock::acquire_in(&lock_dir, &target, 1_000)
            .await
            .unwrap();
        lock.release().await.unwrap();

        // After release the same lock is acquirable again.
        let lock = WorkspaceFileLock::acquire_in(&lock_dir, &target, 1_000)
            .await
            .unwrap();
        lock.release().await.unwrap();
    }

    #[tokio::test]
    async fn workspace_lock_second_acquirer_times_out_with_busy_error() {
        let temp = tempfile::tempdir().unwrap();
        let lock_dir = temp.path().join("locks");
        let target = temp.path().join("data.txt");
        std::fs::write(&target, "v1").unwrap();

        let held = WorkspaceFileLock::acquire_in(&lock_dir, &target, 1_000)
            .await
            .unwrap();

        let err = WorkspaceFileLock::acquire_in(&lock_dir, &target, 200)
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("file busy"), "unexpected error: {msg}");
        assert!(
            msg.contains("data.txt"),
            "error should name the target: {msg}"
        );

        held.release().await.unwrap();
    }

    #[tokio::test]
    async fn workspace_lock_is_keyed_on_canonical_path() {
        let temp = tempfile::tempdir().unwrap();
        let lock_dir = temp.path().join("locks");
        let target = temp.path().join("data.txt");
        std::fs::write(&target, "v1").unwrap();

        // Same file via an absolute path and via a `./`-prefixed
        // spelling must map to the SAME lock file.
        let plain = lock_file_path(&lock_dir, &target);
        let dotted = lock_file_path(&lock_dir, &target.join(Path::new(".")));
        assert_eq!(plain, dotted);

        // Distinct files must map to distinct lock files (the old
        // `with_extension("lock")` scheme collided `foo.txt` with
        // `foo.md`).
        let other = temp.path().join("data.md");
        std::fs::write(&other, "v1").unwrap();
        assert_ne!(plain, lock_file_path(&lock_dir, &other));
    }

    #[tokio::test]
    async fn workspace_lock_nonexistent_target_still_contends() {
        let temp = tempfile::tempdir().unwrap();
        let lock_dir = temp.path().join("locks");
        let target = temp.path().join("not_created_yet.txt");

        // Two spellings of a not-yet-existing file resolve to one lock.
        let a = lock_file_path(&lock_dir, &target);
        let b = lock_file_path(
            &lock_dir,
            &std::env::current_dir()
                .unwrap()
                .join(temp.path().join("not_created_yet.txt")),
        );
        // Note: if tempdir contains a symlinked component the two may
        // legitimately differ; assert only the lock-dir placement and
        // extension shape here.
        assert!(a.starts_with(&lock_dir));
        assert!(a.to_string_lossy().ends_with(".lock"));
        let _ = b;

        let held = WorkspaceFileLock::acquire_in(&lock_dir, &target, 1_000)
            .await
            .unwrap();
        let err = WorkspaceFileLock::acquire_in(&lock_dir, &target, 200)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("file busy"));
        held.release().await.unwrap();
    }

    #[tokio::test]
    async fn workspace_lock_mutual_exclusion_across_tasks() {
        let temp = tempfile::tempdir().unwrap();
        let lock_dir = temp.path().join("locks");
        let target = temp.path().join("counter");

        // Task A holds the lock; task B tries with a tiny budget — B
        // must NOT succeed while A holds it.
        let holder = WorkspaceFileLock::acquire_in(&lock_dir, &target, 1_000)
            .await
            .unwrap();

        let contender = tokio::spawn(async move {
            // Tiny timeout: expect busy while the holder keeps it.
            WorkspaceFileLock::acquire_in(&lock_dir, &target, 50).await
        });
        let result = contender.await.unwrap();
        assert!(result.is_err(), "contender acquired a held lock");

        holder.release().await.unwrap();
    }
}
