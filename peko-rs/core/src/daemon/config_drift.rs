//! Principal-config drift detector (ADR-046 §"Configuration drift
//! detection").
//!
//! Hashes every `principal.toml` at daemon startup and compares
//! against a baseline written the last time the daemon ran. Any
//! mismatch is emitted as a `principal.config_drift` Security
//! audit event with severity `Security` so operators see it in
//! `peko audit tail --since 5m` and in the JSONL file.
//!
//! **v1 limitation:** drift is only detected at daemon startup —
//! in-session edits to `principal.toml` go undetected. Adding the
//! `notify` crate for live watching is deferred to a follow-up PR;
//! the start-up check catches the most damaging case (someone
//! editing the file while the daemon is stopped, then starting the
//! daemon).
//!
//! The baseline file is `<data_dir>/runtime/principal-hashes.json` —
//! a JSON object of `{principal_name: hex_sha256_hash}`. It's
//! written via atomic rename (`.tmp` → `real`) so a crash mid-write
//! doesn't corrupt the baseline and trigger a flood of false drift
//! events on the next boot.
//!
//! **What's hashed:** the entire `principal.toml` file as raw
//! bytes. Hashing the parsed config would miss whitespace-only
//! changes (which are still a sign of external tampering), and
//! canonicalizing TOML would require a stable serializer that we
//! don't have. Raw-byte SHA-256 is the simplest drift signal that
//! catches every external change.
//!
//! **What's NOT hashed:** the local tier (`{data_dir}/principals/.../local/`)
//! — that's runtime-only state that changes on every cron run.
//! Drift detection is only meaningful for the shared tier, which
//! holds the cap grants, agent definitions, and identity that
//! the user has signed off on.

use crate::common::paths::PathResolver;
use anyhow::{Context, Result};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::warn;
use walkdir::WalkDir;

use peko_observability::{AuditSeverity, Observability};

/// Run the drift check. Walks the principals dir, hashes each
/// `principal.toml`, compares to the baseline, and emits a
/// `principal.config_drift` Security event per drifted principal.
/// On successful completion, writes a fresh baseline.
///
/// Returns the number of drifted principals detected (0 on a clean
/// boot, 0 on first boot, N on a botched boot). Never fails the
/// daemon startup — the worst case (baseline read failure) logs a
/// warning and treats the baseline as empty so the first event
/// recorded this session is "no baseline" rather than aborting the
/// daemon over a corrupted audit artifact.
pub async fn run_drift_check(
    path_resolver: &PathResolver,
    observability: Arc<Observability>,
) -> Result<usize> {
    let principals_root = path_resolver.principals_root_dir();
    let baseline_path = path_resolver.principal_hashes_file();

    // Read the previous baseline (if any). A corrupt baseline
    // shouldn't kill the daemon — we just treat the boot as the
    // first boot and write a fresh baseline. We also track
    // `is_first_boot` so we don't emit "principal added" drift
    // events for every principal on the very first run (that's
    // noise; the user is the one who created them).
    let (previous, is_first_boot) = match read_baseline(&baseline_path) {
        Ok(Some(b)) => (b, false),
        Ok(None) => (BTreeMap::new(), true),
        Err(e) => {
            warn!(
                "drift: baseline read failed at {} (treating as first boot): {e}",
                baseline_path.display()
            );
            (BTreeMap::new(), true)
        }
    };

    // Walk principals_root and hash each `principal.toml`. We use
    // walkdir so an off-pattern file (e.g. an accidentally-created
    // `foo.toml` directly under principals_root) is skipped without
    // erroring.
    let mut current: BTreeMap<String, String> = BTreeMap::new();
    if principals_root.is_dir() {
        for entry in WalkDir::new(&principals_root)
            .min_depth(2)
            .max_depth(2)
            .into_iter()
            .filter_map(Result::ok)
        {
            let path = entry.path();
            if path.file_name().and_then(|n| n.to_str()) != Some("principal.toml") {
                continue;
            }
            // The principal name is the parent directory's basename.
            // We strip the principals_root prefix and take the next
            // path segment as the name.
            let Some(principal_name) = principal_name_from_path(&principals_root, path) else {
                continue;
            };
            match hash_file(path) {
                Ok(hash) => {
                    current.insert(principal_name, hash);
                }
                Err(e) => {
                    warn!(
                        "drift: failed to hash principal.toml at {}: {e}",
                        path.display()
                    );
                }
            }
        }
    }

    // Diff. Three categories: (a) changed (in both, hashes differ),
    // (b) new (in current, not in previous), (c) removed (in
    // previous, not in current). On the first boot, "new" is
    // suppressed — every principal is new on a first boot, and
    // emitting an event for each one is just noise. "changed" and
    // "removed" still fire (a first boot that sees a principal
    // matching the previous baseline can't have a "changed" event,
    // but a previous baseline can never exist on a first boot, so
    // "removed" is also vacuous; both are kept for symmetry and so
    // the logic doesn't branch on is_first_boot downstream).
    let mut drift_count = 0;
    for (name, current_hash) in &current {
        let previous_hash = previous.get(name);
        if previous_hash != Some(current_hash) {
            // Suppress "added" on the first boot.
            if is_first_boot && previous_hash.is_none() {
                continue;
            }
            let details = match previous_hash {
                Some(prev) => json!({
                    "principal_name": name,
                    "expected_hash": prev,
                    "actual_hash": current_hash,
                    "kind": "changed",
                }),
                None => json!({
                    "principal_name": name,
                    "expected_hash": null,
                    "actual_hash": current_hash,
                    "kind": "added",
                }),
            };
            observability
                .audit_security("principal.config_drift", None, details)
                .await
                .with_context(|| format!("emit principal.config_drift event for {name}"))?;
            warn!(
                "drift: principal {name} changed (kind={})",
                if previous_hash.is_some() {
                    "changed"
                } else {
                    "added"
                }
            );
            drift_count += 1;
        }
    }
    for (name, prev_hash) in &previous {
        if !current.contains_key(name) {
            // Same first-boot suppression as above: a removed
            // principal on the first boot is just "the previous
            // baseline recorded nothing" being interpreted as
            // "everything was removed", which is meaningless.
            if is_first_boot {
                continue;
            }
            observability
                .audit_security(
                    "principal.config_drift",
                    None,
                    json!({
                        "principal_name": name,
                        "expected_hash": prev_hash,
                        "actual_hash": null,
                        "kind": "removed",
                    }),
                )
                .await
                .with_context(|| format!("emit principal.config_drift event for {name}"))?;
            warn!("drift: principal {name} removed");
            drift_count += 1;
        }
    }

    // Write the fresh baseline. Atomic: write to `.tmp` then rename.
    // On any error here we log a warning but don't fail the boot —
    // the next boot will treat it as first-boot and re-baseline.
    if let Err(e) = write_baseline(&baseline_path, &current) {
        warn!(
            "drift: failed to write baseline at {}: {e}",
            baseline_path.display()
        );
    }

    Ok(drift_count)
}

/// Hash the file at `path` with SHA-256 and return the lowercase
/// hex digest. Returns an error if the file can't be opened or read.
fn hash_file(path: &std::path::Path) -> Result<String> {
    let mut file = fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = file
            .read(&mut buf)
            .with_context(|| format!("read {}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex_lower(&hasher.finalize()))
}

/// Lowercase hex of a 32-byte SHA-256 digest.
fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// Extract the principal name from a `principal.toml` path. The
/// expected shape is `{principals_root}/{name}/principal.toml`;
/// returns `None` if the path doesn't match that shape.
fn principal_name_from_path(
    principals_root: &std::path::Path,
    path: &std::path::Path,
) -> Option<String> {
    let rel = path.strip_prefix(principals_root).ok()?;
    let mut components = rel.components();
    let first = components.next()?;
    let second = components.next()?;
    // First component is the principal name, second is `principal.toml`.
    if second.as_os_str() != "principal.toml" {
        return None;
    }
    Some(first.as_os_str().to_string_lossy().into_owned())
}

/// Read the baseline file. Returns `Ok(None)` if the file doesn't
/// exist (first boot), `Ok(Some(map))` on success, or an error for
/// any other I/O or parse failure. The `None` vs `Some(empty)`
/// distinction matters: a present-but-empty baseline means "I know
/// the principal set is empty" (a real signal), while `None` means
/// "I have no idea what the principal set is" (first boot — every
/// principal is new and not a drift event).
fn read_baseline(path: &PathBuf) -> Result<Option<BTreeMap<String, String>>> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    let map: BTreeMap<String, String> = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse baseline JSON at {}", path.display()))?;
    Ok(Some(map))
}

/// Write the baseline atomically: serialize → write to `.tmp` →
/// fsync the file → rename into place. The rename is the atomic
/// step; the fsync ensures the new file's data is on disk before
/// the rename (otherwise a crash between rename and the data being
/// on disk would leave a baseline file pointing at empty bytes).
fn write_baseline(path: &PathBuf, baseline: &BTreeMap<String, String>) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create parent dir {}", parent.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_vec_pretty(baseline).context("serialize baseline")?;
    {
        let mut file =
            fs::File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
        use std::io::Write;
        file.write_all(&json)
            .with_context(|| format!("write {}", tmp.display()))?;
        file.sync_all()
            .with_context(|| format!("fsync {}", tmp.display()))?;
    }
    fs::rename(&tmp, path)
        .with_context(|| format!("rename {} → {}", tmp.display(), path.display()))?;
    Ok(())
}

/// Re-export the severity enum so callers can reference it
/// without depending on the observability crate's exact path
/// (e.g. from a test).
#[allow(dead_code)]
pub(crate) fn severity() -> AuditSeverity {
    AuditSeverity::Security
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::paths::PathResolver;
    use peko_observability::Observability;
    use std::sync::Arc;
    use tempfile::tempdir;

    fn write_principal(root: &std::path::Path, name: &str, body: &str) {
        let dir = root.join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("principal.toml"), body).unwrap();
    }

    #[test]
    fn hash_is_stable_and_sensitive() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("a.txt");
        fs::write(&p, "hello").unwrap();
        let h1 = hash_file(&p).unwrap();
        let h2 = hash_file(&p).unwrap();
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64); // SHA-256 hex is 64 chars.

        fs::write(&p, "hello!").unwrap();
        let h3 = hash_file(&p).unwrap();
        assert_ne!(h1, h3);
    }

    #[test]
    fn principal_name_extraction_works() {
        let root = std::path::Path::new("/config/principals");
        let p = std::path::Path::new("/config/principals/alice/principal.toml");
        assert_eq!(principal_name_from_path(root, p).as_deref(), Some("alice"));
        // Wrong filename — second component is not principal.toml.
        let p2 = std::path::Path::new("/config/principals/alice/foo.toml");
        assert_eq!(principal_name_from_path(root, p2), None);
    }

    /// First boot: no baseline exists, no drift is reported, and
    /// the baseline is written.
    #[tokio::test]
    async fn first_boot_creates_baseline_no_drift() {
        let dir = tempdir().unwrap();
        let config_dir = dir.path().join("config");
        let data_dir = dir.path().join("data");
        let cache_dir = dir.path().join("cache");
        fs::create_dir_all(&config_dir).unwrap();
        fs::create_dir_all(&data_dir).unwrap();
        write_principal(
            &config_dir.join("principals"),
            "alice",
            "[capabilities]\ngrants=[]\n",
        );
        write_principal(
            &config_dir.join("principals"),
            "bob",
            "[capabilities]\ngrants=[\"tool:Read\"]\n",
        );

        let resolver =
            PathResolver::with_dirs(config_dir.clone(), data_dir.clone(), cache_dir.clone());
        let obs = Arc::new(Observability::new("test"));

        let drift_count = run_drift_check(&resolver, obs).await.unwrap();
        assert_eq!(drift_count, 0);

        // Baseline file now exists and contains both principals.
        let baseline_path = resolver.principal_hashes_file();
        assert!(baseline_path.exists());
        let baseline = read_baseline(&baseline_path)
            .unwrap()
            .expect("baseline should exist after first boot");
        assert_eq!(baseline.len(), 2);
        assert!(baseline.contains_key("alice"));
        assert!(baseline.contains_key("bob"));
    }

    /// Edit a principal.toml between boots → drift is reported
    /// (kind=changed), the audit log shows the event, and the
    /// baseline is updated.
    #[tokio::test]
    async fn edit_between_boots_emits_drift_event() {
        let dir = tempdir().unwrap();
        let config_dir = dir.path().join("config");
        let data_dir = dir.path().join("data");
        let cache_dir = dir.path().join("cache");
        fs::create_dir_all(&config_dir).unwrap();
        fs::create_dir_all(&data_dir).unwrap();
        write_principal(
            &config_dir.join("principals"),
            "alice",
            "[capabilities]\ngrants=[]\n",
        );

        let resolver = PathResolver::with_dirs(config_dir.clone(), data_dir.clone(), cache_dir);
        // First boot — establishes baseline.
        let obs1 = Arc::new(Observability::new("test"));
        let first = run_drift_check(&resolver, obs1).await.unwrap();
        assert_eq!(first, 0);

        // Edit alice's principal.toml.
        write_principal(
            &config_dir.join("principals"),
            "alice",
            "[capabilities]\ngrants=[\"tool:Bash\"]\n",
        );

        // Second boot — drift should fire.
        let obs2 = Arc::new(Observability::new("test"));
        let second = run_drift_check(&resolver, obs2.clone()).await.unwrap();
        assert_eq!(second, 1);

        let entries = obs2.get_audit_log(10).await;
        let drift = entries
            .iter()
            .find(|e| e.event_type == "principal.config_drift")
            .expect("expected principal.config_drift event");
        assert_eq!(drift.severity, AuditSeverity::Security);
        assert_eq!(drift.details["principal_name"], "alice");
        assert_eq!(drift.details["kind"], "changed");
        assert!(drift.details["expected_hash"].is_string());
        assert!(drift.details["actual_hash"].is_string());
        assert_ne!(
            drift.details["expected_hash"], drift.details["actual_hash"],
            "expected and actual hashes must differ for a real drift"
        );
    }

    /// New principal created between boots → drift is reported
    /// (kind=added) so users can spot principals they didn't create
    /// (e.g. one created via external FS write).
    #[tokio::test]
    async fn new_principal_emits_added_drift() {
        let dir = tempdir().unwrap();
        let config_dir = dir.path().join("config");
        let data_dir = dir.path().join("data");
        let cache_dir = dir.path().join("cache");
        fs::create_dir_all(&config_dir).unwrap();
        fs::create_dir_all(&data_dir).unwrap();
        write_principal(
            &config_dir.join("principals"),
            "alice",
            "[capabilities]\ngrants=[]\n",
        );

        let resolver = PathResolver::with_dirs(config_dir.clone(), data_dir.clone(), cache_dir);
        let obs1 = Arc::new(Observability::new("test"));
        assert_eq!(run_drift_check(&resolver, obs1).await.unwrap(), 0);

        write_principal(
            &config_dir.join("principals"),
            "mallory",
            "[capabilities]\ngrants=[\"tool:Bash\"]\n",
        );

        let obs2 = Arc::new(Observability::new("test"));
        assert_eq!(run_drift_check(&resolver, obs2.clone()).await.unwrap(), 1);
        let entries = obs2.get_audit_log(10).await;
        let drift = entries
            .iter()
            .find(|e| e.event_type == "principal.config_drift")
            .unwrap();
        assert_eq!(drift.details["principal_name"], "mallory");
        assert_eq!(drift.details["kind"], "added");
        assert!(drift.details["expected_hash"].is_null());
    }

    /// Principal removed between boots → drift is reported
    /// (kind=removed) so users can spot principals that disappeared
    /// (e.g. an admin principal someone "cleaned up").
    #[tokio::test]
    async fn removed_principal_emits_removed_drift() {
        let dir = tempdir().unwrap();
        let config_dir = dir.path().join("config");
        let data_dir = dir.path().join("data");
        let cache_dir = dir.path().join("cache");
        fs::create_dir_all(&config_dir).unwrap();
        fs::create_dir_all(&data_dir).unwrap();
        write_principal(
            &config_dir.join("principals"),
            "alice",
            "[capabilities]\ngrants=[]\n",
        );
        write_principal(
            &config_dir.join("principals"),
            "bob",
            "[capabilities]\ngrants=[]\n",
        );

        let resolver = PathResolver::with_dirs(config_dir.clone(), data_dir.clone(), cache_dir);
        let obs1 = Arc::new(Observability::new("test"));
        assert_eq!(run_drift_check(&resolver, obs1).await.unwrap(), 0);

        fs::remove_dir_all(config_dir.join("principals").join("bob")).unwrap();

        let obs2 = Arc::new(Observability::new("test"));
        assert_eq!(run_drift_check(&resolver, obs2.clone()).await.unwrap(), 1);
        let entries = obs2.get_audit_log(10).await;
        let drift = entries
            .iter()
            .find(|e| e.event_type == "principal.config_drift")
            .unwrap();
        assert_eq!(drift.details["principal_name"], "bob");
        assert_eq!(drift.details["kind"], "removed");
        assert!(drift.details["actual_hash"].is_null());
    }

    /// Clean restart with no edits → no drift, baseline file is
    /// rewritten with the same hashes.
    #[tokio::test]
    async fn clean_restart_emits_no_drift() {
        let dir = tempdir().unwrap();
        let config_dir = dir.path().join("config");
        let data_dir = dir.path().join("data");
        let cache_dir = dir.path().join("cache");
        fs::create_dir_all(&config_dir).unwrap();
        fs::create_dir_all(&data_dir).unwrap();
        write_principal(
            &config_dir.join("principals"),
            "alice",
            "[capabilities]\ngrants=[]\n",
        );

        let resolver = PathResolver::with_dirs(config_dir, data_dir, cache_dir);
        let obs1 = Arc::new(Observability::new("test"));
        run_drift_check(&resolver, obs1).await.unwrap();

        let obs2 = Arc::new(Observability::new("test"));
        let n = run_drift_check(&resolver, obs2.clone()).await.unwrap();
        assert_eq!(n, 0);
        let entries = obs2.get_audit_log(10).await;
        assert!(entries
            .iter()
            .all(|e| e.event_type != "principal.config_drift"));
    }

    /// Empty principals dir → no drift, baseline is written empty.
    #[tokio::test]
    async fn empty_principals_dir_writes_empty_baseline() {
        let dir = tempdir().unwrap();
        let config_dir = dir.path().join("config");
        let data_dir = dir.path().join("data");
        let cache_dir = dir.path().join("cache");
        fs::create_dir_all(&config_dir).unwrap();
        fs::create_dir_all(&data_dir).unwrap();
        // Don't create any principals.

        let resolver = PathResolver::with_dirs(config_dir, data_dir, cache_dir);
        let obs = Arc::new(Observability::new("test"));
        let n = run_drift_check(&resolver, obs).await.unwrap();
        assert_eq!(n, 0);
        let baseline = read_baseline(&resolver.principal_hashes_file())
            .unwrap()
            .expect("baseline should exist after first boot");
        assert!(baseline.is_empty());
    }
}
