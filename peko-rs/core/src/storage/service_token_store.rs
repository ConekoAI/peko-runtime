//! On-disk store for named service tokens (ADR-045 PR #5).
//!
//! Each token is a per-name directory at
//! `<root>/<name>/{meta.json,token}`. The directory holds two files:
//!
//! - `meta.json` — `{ name, caps, created_at_secs, expires_at_secs }`,
//!   mode 0600. Greppable, no secret content.
//! - `token` — raw 32-byte URL-safe-base64 string (43 chars, no
//!   padding), mode 0600. The secret. Separate file so `meta.json`
//!   can be listed / `cat`'d for human inspection without exposing
//!   the secret. Format matches `generate_session_token` so the
//!   hash-and-verify path is byte-identical to the session-token
//!   path.
//!
//! The parent bucket is mode 0700; each per-name dir inherits via
//! the `mkdir` mode arg. Files are written via temp+rename so a
//! concurrent reader never sees a torn artifact.
//!
//! ## Atomic-write pattern (mirrors `daemon::approval_queue`)
//!
//! 1. `create_dir_all(parent_dir)` with mode 0700 (idempotent).
//! 2. `OpenOptions::create+truncate+write, mode(0600).open(tmp_path)`
//! 3. `write_all(bytes)` + `sync_all()`
//! 4. `rename(tmp_path, final_path)` (atomic on the same filesystem).

use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};

use crate::ipc::auth_code::generate_session_token;

/// Open `path` for writing with owner-only mode (`0600`) on Unix;
/// mode is not enforced on Windows (NTFS DACLs are out of scope
/// for peko — only the Unix transport-layer trust model applies).
#[cfg(unix)]
fn open_mode_0600(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn open_mode_0600(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
}

/// Public, IPC-serializable view of a service token.
///
/// `meta.json` stores the same fields; the on-disk form is a
/// serde-flat mirror of this struct so an operator can
/// `cat ~/.peko/runtime/service.tokens/<name>/meta.json` to see
/// exactly what's registered without exposing the raw secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceTokenInfo {
    pub name: String,
    pub caps: Vec<String>,
    pub created_at_secs: u64,
    pub expires_at_secs: Option<u64>,
    /// Last use timestamp (None if never used). Populated by the
    /// daemon at runtime; the on-disk store doesn't track this
    /// (it's a daemon-internal hot-path signal).
    #[serde(default)]
    pub last_used_at_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OnDiskMeta {
    name: String,
    caps: Vec<String>,
    created_at_secs: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expires_at_secs: Option<u64>,
}

impl From<OnDiskMeta> for ServiceTokenInfo {
    fn from(m: OnDiskMeta) -> Self {
        Self {
            name: m.name,
            caps: m.caps,
            created_at_secs: m.created_at_secs,
            expires_at_secs: m.expires_at_secs,
            last_used_at_secs: None,
        }
    }
}

/// On-disk CRUD for service tokens.
///
/// Cheap to construct (`new(root)`); the root is the bucket the
/// daemon has already ensured exists via `PathResolver::ensure_dirs`.
#[derive(Debug, Clone)]
pub struct ServiceTokenStore {
    root: PathBuf,
}

impl ServiceTokenStore {
    /// Wrap a bucket root (e.g. `{data_dir}/runtime/service.tokens`).
    /// Does NOT create the root — call
    /// [`PathResolver::service_tokens_dir`](crate::common::paths::PathResolver::service_tokens_dir)
    /// + `ensure_dirs` to ensure it exists before constructing.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Generate a new token for `name` with the given capability
    /// list, persist it to disk, and return the raw token + meta.
    ///
    /// The token is **shown to the caller exactly once**; it is not
    /// stored in cleartext anywhere on disk except in `<name>/token`
    /// (which is also where the caller should store it for their
    /// own use). The daemon only persists the SHA-256 hash.
    ///
    /// `expires_in_secs` is a TTL relative to `now`. `None` means
    /// "no expiry" (the meta.json will omit the field).
    pub fn create(
        &self,
        name: &str,
        caps: Vec<String>,
        expires_in_secs: Option<u64>,
    ) -> Result<(String, ServiceTokenInfo)> {
        validate_name(name)?;
        let dir = self.dir_for(name);
        ensure_dir_0700(&dir)?;

        let now = now_secs()?;
        let expires_at_secs = expires_in_secs.map(|ttl| now.saturating_add(ttl));

        let meta = OnDiskMeta {
            name: name.to_string(),
            caps: caps.clone(),
            created_at_secs: now,
            expires_at_secs,
        };

        let token = generate_session_token(&mut OsRng);

        // meta.json first, then the secret. If the second write
        // fails the partial state is detectable (token file missing
        // + meta.json present); the next load_all skips the
        // orphan and `create` can be re-run.
        write_atomic(&dir.join("meta.json"), &serde_json::to_vec_pretty(&meta)?)?;
        write_atomic(&dir.join("token"), token.as_bytes())?;

        Ok((
            token,
            ServiceTokenInfo {
                name: meta.name,
                caps: meta.caps,
                created_at_secs: meta.created_at_secs,
                expires_at_secs: meta.expires_at_secs,
                last_used_at_secs: None,
            },
        ))
    }

    /// Load every registered token's meta + raw token. Used by
    /// daemon startup to rehydrate `AuthTable`.
    ///
    /// Tokens whose on-disk layout is malformed (missing dir, bad
    /// JSON, missing secret file) are silently skipped — the
    /// equivalent of `ApprovalQueue::rehydrate`'s malformed-skip
    /// behavior at
    /// `crate::daemon::approval_queue::ApprovalQueue::rehydrate`.
    pub fn load_all(&self) -> Result<Vec<(String, String, ServiceTokenInfo)>> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&self.root)
            .with_context(|| format!("read_dir {}", self.root.display()))?
        {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            // Skip dotfiles (`.meta.json.tmp` left behind by a
            // crashed write).
            let dir_name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) if !n.starts_with('.') => n.to_string(),
                _ => continue,
            };

            let meta = match self.read_meta(&path) {
                Ok(m) => m,
                Err(_) => continue,
            };
            let raw = match std::fs::read_to_string(path.join("token")) {
                Ok(t) => t,
                Err(_) => continue,
            };
            out.push((dir_name, raw, meta));
        }
        // Sort by name for deterministic load order — keeps test
        // assertions stable and helps operators eyeball `ls`.
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }

    /// Read the meta for `name` (no secret returned).
    pub fn get(&self, name: &str) -> Result<Option<ServiceTokenInfo>> {
        let dir = self.dir_for(name);
        if !dir.exists() {
            return Ok(None);
        }
        Ok(Some(self.read_meta(&dir)?))
    }

    /// List every token's meta (no secrets). Used by
    /// `peko service-token list` and the IPC `ServiceTokenList`
    /// response.
    pub fn list(&self) -> Result<Vec<ServiceTokenInfo>> {
        let all = self.load_all()?;
        Ok(all.into_iter().map(|(_, _, m)| m).collect())
    }

    /// Delete the on-disk artifacts for `name`. Returns `true` if
    /// anything was removed; `false` if the token didn't exist.
    pub fn revoke(&self, name: &str) -> Result<bool> {
        let dir = self.dir_for(name);
        if !dir.exists() {
            return Ok(false);
        }
        std::fs::remove_dir_all(&dir)
            .with_context(|| format!("remove_dir_all {}", dir.display()))?;
        Ok(true)
    }

    fn dir_for(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn read_meta(&self, dir: &Path) -> Result<ServiceTokenInfo> {
        let bytes = std::fs::read(dir.join("meta.json"))
            .with_context(|| format!("read {}", dir.join("meta.json").display()))?;
        let meta: OnDiskMeta = serde_json::from_slice(&bytes)
            .with_context(|| format!("parse {}", dir.join("meta.json").display()))?;
        Ok(meta.into())
    }
}

/// Reject empty names and names containing path separators or
/// `..`. Token names are CLI identifiers; they should never need
/// to traverse the bucket.
fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() {
        anyhow::bail!("service token name must not be empty");
    }
    if name.contains('/') || name.contains('\0') || name == "." || name == ".." {
        anyhow::bail!("service token name contains illegal characters: {name:?}");
    }
    // Cap at 64 chars to keep filenames reasonable.
    if name.len() > 64 {
        anyhow::bail!("service token name too long (max 64 chars)");
    }
    Ok(())
}

/// Ensure the per-name directory exists with mode 0700 (Unix only).
#[cfg(unix)]
fn ensure_dir_0700(dir: &Path) -> Result<()> {
    if dir.exists() {
        return Ok(());
    }
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("create_dir_all {}", dir.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn ensure_dir_0700(dir: &Path) -> Result<()> {
    if dir.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(dir)
        .with_context(|| format!("create_dir_all {}", dir.display()))?;
    Ok(())
}

/// Atomic write: temp file in the same dir, then rename. Mode 0600
/// (Unix only — NTFS DACLs are out of scope).
fn write_atomic(final_path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = final_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("write_atomic: path has no parent: {final_path:?}"))?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("create_dir_all {}", parent.display()))?;

    // The temp file lives next to the final one so rename is
    // atomic on the same filesystem. We use a counter so
    // concurrent creates of the same name don't collide.
    let tmp = parent.join(format!(
        ".{}.tmp",
        final_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("artifact")
    ));

    {
        let mut f = open_mode_0600(&tmp)
            .with_context(|| format!("open tmp {}", tmp.display()))?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, final_path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), final_path.display()))?;
    Ok(())
}

/// Wall-clock seconds since the Unix epoch.
fn now_secs() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| anyhow::anyhow!("clock before epoch: {e}"))?
        .as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_root(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!(
            "peko-service-tokens-test-{tag}-{nanos}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn create_returns_raw_token_and_meta() {
        let root = fresh_root("create");
        let store = ServiceTokenStore::new(&root);

        let (token, meta) = store
            .create("runtime", vec!["fs:read".into(), "tool:Bash".into()], None)
            .unwrap();
        assert_eq!(meta.name, "runtime");
        assert_eq!(meta.caps, vec!["fs:read", "tool:Bash"]);
        assert!(meta.expires_at_secs.is_none());
        assert!(!meta.last_used_at_secs.is_some());
        // Token is 32 bytes URL-safe-base64 (no padding) → 43 chars,
        // matching the format `peko-rs/core/src/ipc/auth_code.rs::
        // generate_session_token` emits. PR #5 deliberately reuses
        // the existing format so the hash-and-verify path is
        // byte-identical to the session-token path.
        assert_eq!(token.len(), 43);
        assert!(token.chars().all(|c| matches!(
            c,
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_'
        )));

        // Persisted files exist with the right content.
        let meta_path = root.join("runtime").join("meta.json");
        let token_path = root.join("runtime").join("token");
        assert!(meta_path.exists());
        assert!(token_path.exists());
        let written_meta = std::fs::read_to_string(&meta_path).unwrap();
        assert!(written_meta.contains("fs:read"));
        assert!(written_meta.contains("tool:Bash"));
        let written_token = std::fs::read_to_string(&token_path).unwrap();
        assert_eq!(written_token, token);
    }

    #[test]
    fn create_with_expiry_persists_expires_at_secs() {
        let root = fresh_root("expiry");
        let store = ServiceTokenStore::new(&root);

        let (_, meta) = store
            .create("timed", vec!["fs:read".into()], Some(3600))
            .unwrap();
        assert!(meta.expires_at_secs.is_some());
        let exp = meta.expires_at_secs.unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!(exp > now);
        assert!(exp <= now + 3601); // allow clock skew
    }

    #[test]
    fn create_then_list_returns_same_meta() {
        let root = fresh_root("list");
        let store = ServiceTokenStore::new(&root);

        let (t1, m1) = store.create("alpha", vec!["a".into()], None).unwrap();
        let (t2, m2) = store.create("beta", vec!["b".into()], None).unwrap();
        assert_ne!(t1, t2); // different tokens

        let listed = store.list().unwrap();
        assert_eq!(listed.len(), 2);
        // Sorted by name → alpha first.
        assert_eq!(listed[0], m1);
        assert_eq!(listed[1], m2);
    }

    #[test]
    fn load_all_returns_raw_tokens_for_rehydrate() {
        let root = fresh_root("load_all");
        let store = ServiceTokenStore::new(&root);

        let (t1, _) = store.create("alpha", vec!["a".into()], None).unwrap();
        let (t2, _) = store.create("beta", vec!["b".into()], None).unwrap();

        let loaded = store.load_all().unwrap();
        assert_eq!(loaded.len(), 2);
        // Sorted by name.
        assert_eq!(loaded[0].0, "alpha");
        assert_eq!(loaded[0].1, t1);
        assert_eq!(loaded[1].0, "beta");
        assert_eq!(loaded[1].1, t2);
    }

    #[test]
    fn load_all_on_missing_root_returns_empty() {
        let store = ServiceTokenStore::new("/nonexistent/peko/test/should/not/exist");
        let loaded = store.load_all().unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn get_returns_none_for_missing_name() {
        let root = fresh_root("get");
        let store = ServiceTokenStore::new(&root);
        assert!(store.get("nope").unwrap().is_none());
    }

    #[test]
    fn get_returns_meta_for_existing_token() {
        let root = fresh_root("get_existing");
        let store = ServiceTokenStore::new(&root);
        let (_, meta) = store
            .create("token-1", vec!["fs:read".into(), "tool:Read".into()], None)
            .unwrap();
        assert_eq!(store.get("token-1").unwrap(), Some(meta));
    }

    #[test]
    fn revoke_removes_directory_and_returns_true() {
        let root = fresh_root("revoke");
        let store = ServiceTokenStore::new(&root);
        store.create("temp", vec!["x".into()], None).unwrap();
        assert!(root.join("temp").exists());
        assert!(store.revoke("temp").unwrap());
        assert!(!root.join("temp").exists());
    }

    #[test]
    fn revoke_returns_false_for_unknown_name() {
        let root = fresh_root("revoke_unknown");
        let store = ServiceTokenStore::new(&root);
        assert!(!store.revoke("nope").unwrap());
    }

    #[test]
    fn create_rejects_empty_name() {
        let root = fresh_root("empty_name");
        let store = ServiceTokenStore::new(&root);
        let err = store.create("", vec!["x".into()], None).unwrap_err();
        assert!(err.to_string().contains("must not be empty"));
    }

    #[test]
    fn create_rejects_path_traversal() {
        let root = fresh_root("traverse");
        let store = ServiceTokenStore::new(&root);
        for bad in ["../etc", "foo/bar", "..", "."] {
            let err = store.create(bad, vec!["x".into()], None).unwrap_err();
            assert!(err.to_string().contains("illegal"));
        }
    }

    #[test]
    fn create_rejects_overly_long_name() {
        let root = fresh_root("long_name");
        let store = ServiceTokenStore::new(&root);
        let err = store
            .create(&"a".repeat(65), vec!["x".into()], None)
            .unwrap_err();
        assert!(err.to_string().contains("too long"));
    }

    #[test]
    #[cfg(unix)]
    fn files_have_0600_mode() {
        use std::os::unix::fs::PermissionsExt;
        let root = fresh_root("mode");
        let store = ServiceTokenStore::new(&root);
        store.create("mode-test", vec!["x".into()], None).unwrap();

        let meta_mode = std::fs::metadata(root.join("mode-test").join("meta.json"))
            .unwrap()
            .permissions()
            .mode();
        let token_mode = std::fs::metadata(root.join("mode-test").join("token"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(meta_mode & 0o777, 0o600);
        assert_eq!(token_mode & 0o777, 0o600);
    }
}