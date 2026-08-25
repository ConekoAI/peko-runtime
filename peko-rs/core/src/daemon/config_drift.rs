//! Principal-config drift detector (ADR-046 §"Configuration drift
//! detection", extended by ADR-047 §2.6 to cover workspace tooling).
//!
//! At daemon startup this module:
//!
//! 1. Hashes every `principal.toml` and emits a
//!    `principal.config_drift` (Security) event on mismatch.
//! 2. Hashes every workspace subdirectory under
//!    `<workspace>/{tools,hooks,mcp}/<id>/` and emits per-category
//!    install/remove events:
//!    - `principal.tool_installed` / `principal.tool_removed` (Info)
//!    - `principal.hook_installed` / `principal.hook_removed` (Warning)
//!    - `principal.mcp_installed`  / `principal.mcp_removed`  (Info)
//!
//! Hooks are Warning because, per ADR-047 §4.2, a malicious hook
//! fires on every subsequent turn — the highest residual risk of the
//! workspace-trust model. Tools and MCP servers only activate on
//! explicit invocation, so Info is enough.
//!
//! **v1 limitation:** drift is only detected at daemon startup —
//! in-session edits to `principal.toml` or workspace directories go
//! undetected. Adding the `notify` crate for live watching is
//! deferred; the start-up check catches the most damaging case
//! (someone editing while the daemon is stopped, then starting the
//! daemon).
//!
//! The baseline file is `<data_dir>/runtime/principal-hashes.json`.
//! Schema (per ADR-047 §2.6):
//!
//! ```json
//! {
//!   "<principal>": {
//!     "principal": "<sha256 of principal.toml>",
//!     "tools":     {"<id>": "<sha256 of tools/<id>/*>"},
//!     "hooks":     {"<id>": "<sha256 of hooks/<id>/*>"},
//!     "mcp":       {"<id>": "<sha256 of mcp/<id>/*>"}
//!   }
//! }
//! ```
//!
//! Written via atomic rename (`.tmp` → `real`) so a crash mid-write
//! doesn't corrupt the baseline and trigger a flood of false drift
//! events on the next boot.
//!
//! **What's hashed:** the entire `principal.toml` file as raw bytes,
//! plus every file under each workspace subdirectory (path +
//! content). Hashing the parsed config would miss whitespace-only
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
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::warn;
use walkdir::WalkDir;

use peko_observability::{AuditSeverity, Observability};

/// Per-principal baseline entry. `principal` covers the shared-tier
/// `principal.toml` (Phase A); `tools`/`hooks`/`mcp` cover the
/// workspace subdirectories introduced by ADR-047.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BaselineEntry {
    /// SHA-256 of the raw bytes of `<shared>/principal.toml`.
    pub principal: String,
    /// `<workspace>/tools/<id>` → SHA-256 of all files inside.
    #[serde(default)]
    pub tools: BTreeMap<String, String>,
    /// `<workspace>/hooks/<id>` → SHA-256 of all files inside.
    #[serde(default)]
    pub hooks: BTreeMap<String, String>,
    /// `<workspace>/mcp/<id>` → SHA-256 of all files inside.
    #[serde(default)]
    pub mcp: BTreeMap<String, String>,
}

pub type Baseline = BTreeMap<String, BaselineEntry>;

/// One workspace subdirectory kind and its event-name / severity.
struct WorkspaceCategory {
    /// Directory name under `<workspace>/`.
    dir_name: &'static str,
    /// `installed` event name.
    installed_event: &'static str,
    /// `removed` event name.
    removed_event: &'static str,
    /// Severity for both events (Info for tool/mcp; Warning for hooks).
    severity: AuditSeverity,
}

const WORKSPACE_CATEGORIES: &[WorkspaceCategory] = &[
    WorkspaceCategory {
        dir_name: "tools",
        installed_event: "principal.tool_installed",
        removed_event: "principal.tool_removed",
        severity: AuditSeverity::Info,
    },
    WorkspaceCategory {
        dir_name: "hooks",
        installed_event: "principal.hook_installed",
        removed_event: "principal.hook_removed",
        severity: AuditSeverity::Warning,
    },
    WorkspaceCategory {
        dir_name: "mcp",
        installed_event: "principal.mcp_installed",
        removed_event: "principal.mcp_removed",
        severity: AuditSeverity::Info,
    },
];

/// Run the drift check. Walks the principals dir, hashes each
/// `principal.toml`, compares to the baseline, and emits a
/// `principal.config_drift` Security event per drifted principal.
/// Then walks each principal's workspace subdirectories and emits
/// per-category install/remove events at the configured severity.
/// On successful completion, writes a fresh baseline.
///
/// Returns the number of drift events detected (0 on a clean boot,
/// 0 on first boot, N on a botched boot). Never fails the daemon
/// startup — the worst case (baseline read failure) logs a warning
/// and treats the baseline as empty so the first event recorded
/// this session is "no baseline" rather than aborting the daemon
/// over a corrupted audit artifact.
pub async fn run_drift_check(
    path_resolver: &PathResolver,
    observability: Arc<Observability>,
) -> Result<usize> {
    let principals_root = path_resolver.principals_root_dir();
    let baseline_path = path_resolver.principal_hashes_file();

    // Read the previous baseline (if any). A corrupt baseline
    // shouldn't kill the daemon — we just treat the boot as the
    // first boot and write a fresh baseline. We also track
    // `is_first_boot` so we don't emit "principal added" / "tool
    // installed" / etc. drift events for everything on the very
    // first run (that's noise; the user is the one who created them).
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

    // Walk principals_root and hash each `principal.toml` + each
    // workspace subdirectory. We use walkdir so an off-pattern file
    // (e.g. an accidentally-created `foo.toml` directly under
    // principals_root) is skipped without erroring.
    let mut current: Baseline = BTreeMap::new();
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
            let principal_hash = match hash_file(path) {
                Ok(h) => h,
                Err(e) => {
                    warn!(
                        "drift: failed to hash principal.toml at {}: {e}",
                        path.display()
                    );
                    continue;
                }
            };
            let workspace_root = path.parent().map(Path::to_path_buf);
            let entry = build_baseline_entry(
                &principal_name,
                &principal_hash,
                workspace_root.as_deref(),
            );
            current.insert(principal_name, entry);
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
    for (name, current_entry) in &current {
        match previous.get(name) {
            None => {
                if is_first_boot {
                    continue;
                }
                // New principal — fire a config_drift Security event
                // (same surface as ADR-046) so operators see the
                // principal and the workspace content as a unit.
                observability
                    .audit_security(
                        "principal.config_drift",
                        None,
                        json!({
                            "principal_name": name,
                            "expected_hash": null,
                            "actual_hash": current_entry.principal,
                            "kind": "added",
                        }),
                    )
                    .await
                    .with_context(|| format!("emit principal.config_drift event for {name}"))?;
                warn!("drift: principal {name} added");
                drift_count += 1;
                // Workspace drift for a newly-seen principal is also
                // reported at the per-category severity.
                drift_count += emit_workspace_drift(
                    name,
                    &BTreeMap::new(),
                    &category_maps(current_entry),
                    is_first_boot,
                    &observability,
                )
                .await?;
            }
            Some(prev_entry) => {
                if prev_entry.principal != current_entry.principal {
                    observability
                        .audit_security(
                            "principal.config_drift",
                            None,
                            json!({
                                "principal_name": name,
                                "expected_hash": prev_entry.principal,
                                "actual_hash": current_entry.principal,
                                "kind": "changed",
                            }),
                        )
                        .await
                        .with_context(|| {
                            format!("emit principal.config_drift event for {name}")
                        })?;
                    warn!("drift: principal {name} changed (kind=changed)");
                    drift_count += 1;
                }
                drift_count += emit_workspace_drift(
                    name,
                    &category_maps(prev_entry),
                    &category_maps(current_entry),
                    is_first_boot,
                    &observability,
                )
                .await?;
            }
        }
    }
    for (name, prev_entry) in &previous {
        if current.contains_key(name) {
            continue;
        }
        if is_first_boot {
            continue;
        }
        observability
            .audit_security(
                "principal.config_drift",
                None,
                json!({
                    "principal_name": name,
                    "expected_hash": prev_entry.principal,
                    "actual_hash": null,
                    "kind": "removed",
                }),
            )
            .await
            .with_context(|| format!("emit principal.config_drift event for {name}"))?;
        warn!("drift: principal {name} removed");
        drift_count += 1;
        // Workspace contents for a removed principal are reported
        // as "removed" events at the per-category severity.
        drift_count += emit_workspace_drift(
            name,
            &category_maps(prev_entry),
            &BTreeMap::new(),
            is_first_boot,
            &observability,
        )
        .await?;
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

/// Build a `BaselineEntry` for a single principal by hashing
/// `principal.toml` (already done) and walking each workspace
/// subdirectory. Missing workspace dirs produce empty maps.
fn build_baseline_entry(
    _principal_name: &str,
    principal_hash: &str,
    workspace_root: Option<&Path>,
) -> BaselineEntry {
    let mut entry = BaselineEntry {
        principal: principal_hash.to_string(),
        ..BaselineEntry::default()
    };
    let Some(workspace_root) = workspace_root else {
        return entry;
    };
    for cat in WORKSPACE_CATEGORIES {
        let map = hash_workspace_category(workspace_root, cat.dir_name);
        match cat.dir_name {
            "tools" => entry.tools = map,
            "hooks" => entry.hooks = map,
            "mcp" => entry.mcp = map,
            _ => {}
        }
    }
    entry
}

/// Hash every immediate-child directory of
/// `<workspace_root>/<category>/<id>/` for a single category.
/// Returns a map from `<id>` to the SHA-256 of that directory's
/// contents (path + content of every file, sorted by path).
fn hash_workspace_category(workspace_root: &Path, category: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let dir = workspace_root.join(category);
    let read = match fs::read_dir(&dir) {
        Ok(it) => it,
        Err(_) => return out,
    };
    for entry in read.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(id) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        match hash_directory(&path) {
            Ok(hash) => {
                out.insert(id, hash);
            }
            Err(e) => {
                warn!(
                    "drift: failed to hash workspace {category}/{}: {e}",
                    path.display()
                );
            }
        }
    }
    out
}

/// Hash a directory by walking it recursively and concatenating
/// (relative_path, file_sha256) pairs in sorted order, then SHA-256'ing
/// the concatenation. Catches both presence changes (different file
/// set) and content changes (different file hash).
fn hash_directory(path: &Path) -> Result<String> {
    let mut entries: Vec<(PathBuf, String)> = Vec::new();
    for entry in WalkDir::new(path).into_iter().filter_map(Result::ok) {
        let p = entry.path();
        if !p.is_file() {
            continue;
        }
        let rel = p
            .strip_prefix(path)
            .with_context(|| format!("strip prefix {} from {}", path.display(), p.display()))?;
        let hash = hash_file(p).with_context(|| format!("hash {}", p.display()))?;
        entries.push((rel.to_path_buf(), hash));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let mut hasher = Sha256::new();
    for (rel, hash) in &entries {
        // Use lossy so non-UTF-8 paths don't panic — they still
        // produce a deterministic hash that changes if renamed.
        hasher.update(rel.to_string_lossy().as_bytes());
        hasher.update(b"\0");
        hasher.update(hash.as_bytes());
        hasher.update(b"\n");
    }
    Ok(hex_lower(&hasher.finalize()))
}

/// Project a `BaselineEntry` into a per-category map keyed by
/// `(category, id)`. Lets `emit_workspace_drift` walk categories
/// uniformly without naming each one.
fn category_maps(entry: &BaselineEntry) -> BTreeMap<(&'static str, String), String> {
    let mut out = BTreeMap::new();
    for (id, hash) in &entry.tools {
        out.insert(("tools", id.clone()), hash.clone());
    }
    for (id, hash) in &entry.hooks {
        out.insert(("hooks", id.clone()), hash.clone());
    }
    for (id, hash) in &entry.mcp {
        out.insert(("mcp", id.clone()), hash.clone());
    }
    out
}

/// Diff previous vs current workspace contents. For each
/// (category, id) pair, emit an `installed` event if the id is new
/// or a `removed` event if the id is gone. `changed` (hash differs
/// but id is in both) is reported as an `installed` event with the
/// expected_hash / actual_hash fields set, so operators see content
/// drift without a new event name. Returns the number of events
/// emitted.
async fn emit_workspace_drift(
    principal_name: &str,
    prev: &BTreeMap<(&'static str, String), String>,
    curr: &BTreeMap<(&'static str, String), String>,
    is_first_boot: bool,
    observability: &Observability,
) -> Result<usize> {
    let mut count = 0;
    for cat in WORKSPACE_CATEGORIES {
        let prev_for_cat: BTreeMap<&String, &String> = prev
            .iter()
            .filter(|((c, _), _)| *c == cat.dir_name)
            .map(|((_, id), h)| (id, h))
            .collect();
        let curr_for_cat: BTreeMap<&String, &String> = curr
            .iter()
            .filter(|((c, _), _)| *c == cat.dir_name)
            .map(|((_, id), h)| (id, h))
            .collect();

        for (id, current_hash) in &curr_for_cat {
            match prev_for_cat.get(id) {
                None => {
                    if is_first_boot {
                        continue;
                    }
                    observability
                        .audit_with_severity(
                            cat.severity,
                            None,
                            cat.installed_event,
                            None,
                            json!({
                                "principal_name": principal_name,
                                "id": id.as_str(),
                                "category": cat.dir_name,
                                "expected_hash": null,
                                "actual_hash": current_hash.as_str(),
                                "kind": "added",
                            }),
                        )
                        .await
                        .with_context(|| {
                            format!(
                                "emit {} event for {principal_name}/{id}",
                                cat.installed_event
                            )
                        })?;
                    warn!(
                        "drift: {principal_name} {} {id} added",
                        cat.dir_name
                    );
                    count += 1;
                }
                Some(prev_hash) if prev_hash.as_str() != current_hash.as_str() => {
                    // Content drift within an existing id — fire the
                    // installed event with both hashes so operators
                    // see the actual content change.
                    observability
                        .audit_with_severity(
                            cat.severity,
                            None,
                            cat.installed_event,
                            None,
                            json!({
                                "principal_name": principal_name,
                                "id": id.as_str(),
                                "category": cat.dir_name,
                                "expected_hash": prev_hash.as_str(),
                                "actual_hash": current_hash.as_str(),
                                "kind": "changed",
                            }),
                        )
                        .await
                        .with_context(|| {
                            format!(
                                "emit {} event for {principal_name}/{id}",
                                cat.installed_event
                            )
                        })?;
                    warn!(
                        "drift: {principal_name} {} {id} changed",
                        cat.dir_name
                    );
                    count += 1;
                }
                _ => {}
            }
        }
        for (id, prev_hash) in &prev_for_cat {
            if curr_for_cat.contains_key(id) {
                continue;
            }
            observability
                .audit_with_severity(
                    cat.severity,
                    None,
                    cat.removed_event,
                    None,
                    json!({
                        "principal_name": principal_name,
                        "id": id.as_str(),
                        "category": cat.dir_name,
                        "expected_hash": prev_hash.as_str(),
                        "actual_hash": null,
                        "kind": "removed",
                    }),
                )
                .await
                .with_context(|| {
                    format!(
                        "emit {} event for {principal_name}/{id}",
                        cat.removed_event
                    )
                })?;
            warn!(
                "drift: {principal_name} {} {id} removed",
                cat.dir_name
            );
            count += 1;
        }
    }
    Ok(count)
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
fn read_baseline(path: &PathBuf) -> Result<Option<Baseline>> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    let map: Baseline = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse baseline JSON at {}", path.display()))?;
    Ok(Some(map))
}

/// Write the baseline atomically: serialize → write to `.tmp` →
/// fsync the file → rename into place. The rename is the atomic
/// step; the fsync ensures the new file's data is on disk before
/// the rename (otherwise a crash between rename and the data being
/// on disk would leave a baseline file pointing at empty bytes).
fn write_baseline(path: &PathBuf, baseline: &Baseline) -> Result<()> {
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

    /// Convenience: create `<principal>/<category>/<id>/<file>` with
    /// `body` so a workspace scan can find it.
    fn write_workspace_entry(root: &std::path::Path, category: &str, id: &str, body: &[u8]) {
        let dir = root.join(category).join(id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("index.js"), body).unwrap();
    }

    /// First boot with workspace content present → no drift, baseline
    /// captures every workspace id and category.
    #[tokio::test]
    async fn first_boot_with_workspace_content_records_baseline() {
        let dir = tempdir().unwrap();
        let config_dir = dir.path().join("config");
        let data_dir = dir.path().join("data");
        let cache_dir = dir.path().join("cache");
        fs::create_dir_all(&config_dir).unwrap();
        fs::create_dir_all(&data_dir).unwrap();

        let principal_root = config_dir.join("principals").join("alice");
        fs::create_dir_all(&principal_root).unwrap();
        fs::write(
            principal_root.join("principal.toml"),
            "[capabilities]\ngrants=[]\n",
        )
        .unwrap();
        write_workspace_entry(&principal_root, "tools", "my-tool", b"v1");
        write_workspace_entry(&principal_root, "hooks", "pre-llm", b"hook-body");
        write_workspace_entry(&principal_root, "mcp", "linear", b"server");

        let resolver = PathResolver::with_dirs(config_dir, data_dir, cache_dir);
        let obs = Arc::new(Observability::new("test"));
        let n = run_drift_check(&resolver, obs).await.unwrap();
        assert_eq!(n, 0);

        let baseline = read_baseline(&resolver.principal_hashes_file())
            .unwrap()
            .expect("baseline should exist after first boot");
        let entry = baseline.get("alice").expect("alice should be baselined");
        assert!(entry.tools.contains_key("my-tool"));
        assert!(entry.hooks.contains_key("pre-llm"));
        assert!(entry.mcp.contains_key("linear"));
    }

    /// Add a workspace tool between boots → `principal.tool_installed`
    /// Info event fires; baseline gains the new id.
    #[tokio::test]
    async fn tool_installed_emits_info_event() {
        let dir = tempdir().unwrap();
        let config_dir = dir.path().join("config");
        let data_dir = dir.path().join("data");
        let cache_dir = dir.path().join("cache");
        fs::create_dir_all(&config_dir).unwrap();
        fs::create_dir_all(&data_dir).unwrap();
        let principal_root = config_dir.join("principals").join("alice");
        fs::create_dir_all(&principal_root).unwrap();
        fs::write(
            principal_root.join("principal.toml"),
            "[capabilities]\ngrants=[]\n",
        )
        .unwrap();
        write_workspace_entry(&principal_root, "tools", "old-tool", b"v1");

        let resolver = PathResolver::with_dirs(config_dir.clone(), data_dir.clone(), cache_dir);
        let obs1 = Arc::new(Observability::new("test"));
        assert_eq!(run_drift_check(&resolver, obs1).await.unwrap(), 0);

        write_workspace_entry(&principal_root, "tools", "new-tool", b"v1");

        let obs2 = Arc::new(Observability::new("test"));
        let n = run_drift_check(&resolver, obs2.clone()).await.unwrap();
        assert_eq!(n, 1, "expected one drift event for the new tool");

        let entries = obs2.get_audit_log(10).await;
        let installed = entries
            .iter()
            .find(|e| e.event_type == "principal.tool_installed")
            .expect("expected principal.tool_installed event");
        assert_eq!(installed.severity, AuditSeverity::Info);
        assert_eq!(installed.details["principal_name"], "alice");
        assert_eq!(installed.details["id"], "new-tool");
        assert_eq!(installed.details["category"], "tools");
        assert_eq!(installed.details["kind"], "added");
    }

    /// Add a workspace hook between boots → `principal.hook_installed`
    /// **Warning** event fires (per ADR-047 §4.2 — hooks are the
    /// highest-residual-risk workspace entry).
    #[tokio::test]
    async fn hook_installed_emits_warning_event() {
        let dir = tempdir().unwrap();
        let config_dir = dir.path().join("config");
        let data_dir = dir.path().join("data");
        let cache_dir = dir.path().join("cache");
        fs::create_dir_all(&config_dir).unwrap();
        fs::create_dir_all(&data_dir).unwrap();
        let principal_root = config_dir.join("principals").join("alice");
        fs::create_dir_all(&principal_root).unwrap();
        fs::write(
            principal_root.join("principal.toml"),
            "[capabilities]\ngrants=[]\n",
        )
        .unwrap();

        let resolver = PathResolver::with_dirs(config_dir, data_dir, cache_dir);
        let obs1 = Arc::new(Observability::new("test"));
        assert_eq!(run_drift_check(&resolver, obs1).await.unwrap(), 0);

        write_workspace_entry(&principal_root, "hooks", "pre-llm", b"hook-body");

        let obs2 = Arc::new(Observability::new("test"));
        let n = run_drift_check(&resolver, obs2.clone()).await.unwrap();
        assert_eq!(n, 1);

        let entries = obs2.get_audit_log(10).await;
        let installed = entries
            .iter()
            .find(|e| e.event_type == "principal.hook_installed")
            .expect("expected principal.hook_installed event");
        assert_eq!(
            installed.severity,
            AuditSeverity::Warning,
            "hook installation must fire Warning, not Info"
        );
        assert_eq!(installed.details["id"], "pre-llm");
    }

    /// Remove a workspace mcp between boots → `principal.mcp_removed`
    /// Info event fires.
    #[tokio::test]
    async fn mcp_removed_emits_info_event() {
        let dir = tempdir().unwrap();
        let config_dir = dir.path().join("config");
        let data_dir = dir.path().join("data");
        let cache_dir = dir.path().join("cache");
        fs::create_dir_all(&config_dir).unwrap();
        fs::create_dir_all(&data_dir).unwrap();
        let principal_root = config_dir.join("principals").join("alice");
        fs::create_dir_all(&principal_root).unwrap();
        fs::write(
            principal_root.join("principal.toml"),
            "[capabilities]\ngrants=[]\n",
        )
        .unwrap();
        write_workspace_entry(&principal_root, "mcp", "linear", b"server");

        let resolver = PathResolver::with_dirs(config_dir.clone(), data_dir.clone(), cache_dir);
        let obs1 = Arc::new(Observability::new("test"));
        assert_eq!(run_drift_check(&resolver, obs1).await.unwrap(), 0);

        fs::remove_dir_all(principal_root.join("mcp").join("linear")).unwrap();

        let obs2 = Arc::new(Observability::new("test"));
        let n = run_drift_check(&resolver, obs2.clone()).await.unwrap();
        assert_eq!(n, 1);

        let entries = obs2.get_audit_log(10).await;
        let removed = entries
            .iter()
            .find(|e| e.event_type == "principal.mcp_removed")
            .expect("expected principal.mcp_removed event");
        assert_eq!(removed.severity, AuditSeverity::Info);
        assert_eq!(removed.details["id"], "linear");
        assert_eq!(removed.details["kind"], "removed");
        assert!(removed.details["actual_hash"].is_null());
    }

    /// Edit a workspace tool's content between boots → fires the
    /// `principal.tool_installed` Info event with `kind=changed` and
    /// both hashes, so operators see content drift without a new
    /// event name.
    #[tokio::test]
    async fn tool_content_edit_emits_changed_event() {
        let dir = tempdir().unwrap();
        let config_dir = dir.path().join("config");
        let data_dir = dir.path().join("data");
        let cache_dir = dir.path().join("cache");
        fs::create_dir_all(&config_dir).unwrap();
        fs::create_dir_all(&data_dir).unwrap();
        let principal_root = config_dir.join("principals").join("alice");
        fs::create_dir_all(&principal_root).unwrap();
        fs::write(
            principal_root.join("principal.toml"),
            "[capabilities]\ngrants=[]\n",
        )
        .unwrap();
        write_workspace_entry(&principal_root, "tools", "my-tool", b"v1");

        let resolver = PathResolver::with_dirs(config_dir.clone(), data_dir.clone(), cache_dir);
        let obs1 = Arc::new(Observability::new("test"));
        assert_eq!(run_drift_check(&resolver, obs1).await.unwrap(), 0);

        // Same id, different content.
        write_workspace_entry(&principal_root, "tools", "my-tool", b"v2-totally-different");

        let obs2 = Arc::new(Observability::new("test"));
        let n = run_drift_check(&resolver, obs2.clone()).await.unwrap();
        assert_eq!(n, 1);

        let entries = obs2.get_audit_log(10).await;
        let changed = entries
            .iter()
            .find(|e| e.event_type == "principal.tool_installed")
            .expect("expected principal.tool_installed event for content change");
        assert_eq!(changed.severity, AuditSeverity::Info);
        assert_eq!(changed.details["id"], "my-tool");
        assert_eq!(changed.details["kind"], "changed");
        assert!(changed.details["expected_hash"].is_string());
        assert!(changed.details["actual_hash"].is_string());
        assert_ne!(
            changed.details["expected_hash"], changed.details["actual_hash"],
            "expected and actual hashes must differ for a content edit"
        );
    }
}
