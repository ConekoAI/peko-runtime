//! Path-safety helpers for unpacking archive contents.
//!
//! `tar` 0.4's `Archive::unpack()` applies its own `..` filter, but the
//! unpackagers in this crate iterate entries manually and slurp bytes into a
//! `HashMap` — the filter never runs. [`safe_join`] is the single chokepoint
//! for joining a parent directory with an attacker-controlled relative path
//! from an archive entry. Any code path that turns an archive entry into a
//! filesystem write must funnel through here.

use std::path::{Component, Path, PathBuf};

/// Safely join `parent` and `rel`, rejecting paths that could escape `parent`.
///
/// # Rejections
///
/// - empty `rel`
/// - `rel` containing a NUL byte
/// - `rel` that is absolute
/// - `rel` containing a `..` segment (after lexical normalization)
/// - `rel` with a root or drive-letter component (`/`, `C:\`, `\\?\`, …)
/// - `parent.join(rel)` whose components don't start with `parent`'s
///   components (backstop; should be unreachable if the rules above fire)
///
/// `rel` containing a `.` segment is allowed — it's a no-op after
/// normalization and rejecting it would clutter the rule without blocking
/// any attack.
pub fn safe_join(parent: &Path, rel: &str) -> anyhow::Result<PathBuf> {
    if rel.is_empty() {
        anyhow::bail!("[unsafe_path] relative path is empty");
    }
    if rel.contains('\0') {
        anyhow::bail!("[unsafe_path] relative path contains NUL byte");
    }

    let rel_path = Path::new(rel);
    if rel_path.is_absolute() {
        anyhow::bail!("[unsafe_path] relative path is absolute: {rel}");
    }

    for component in rel_path.components() {
        match component {
            Component::ParentDir => {
                anyhow::bail!("[unsafe_path] relative path contains '..' segment: {rel}");
            }
            Component::RootDir | Component::Prefix(_) => {
                anyhow::bail!(
                    "[unsafe_path] relative path contains root or drive prefix: {rel}"
                );
            }
            Component::CurDir | Component::Normal(_) => {}
        }
    }

    let joined = parent.join(rel_path);
    if !joined.starts_with(parent) {
        anyhow::bail!(
            "[unsafe_path] joined path '{joined:?}' escapes parent '{parent:?}'"
        );
    }

    Ok(joined)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parent() -> PathBuf {
        PathBuf::from("/sandbox")
    }

    #[test]
    fn safe_join_accepts_simple_relative() {
        let p = safe_join(&parent(), "foo/bar.md").unwrap();
        assert_eq!(p, PathBuf::from("/sandbox/foo/bar.md"));
    }

    #[test]
    fn safe_join_rejects_empty_rel() {
        let err = safe_join(&parent(), "").unwrap_err().to_string();
        assert!(err.contains("[unsafe_path]"), "got: {err}");
        assert!(err.contains("empty"), "got: {err}");
    }

    #[test]
    fn safe_join_rejects_nul_byte() {
        let err = safe_join(&parent(), "foo\0bar").unwrap_err().to_string();
        assert!(err.contains("[unsafe_path]"), "got: {err}");
        assert!(err.contains("NUL"), "got: {err}");
    }

    #[test]
    fn safe_join_rejects_dotdot_segment() {
        for rel in ["foo/../bar", "..", "../bar", "foo/.."] {
            let err = safe_join(&parent(), rel).unwrap_err().to_string();
            assert!(err.contains("[unsafe_path]"), "rel={rel} err={err}");
        }
    }

    #[test]
    fn safe_join_rejects_absolute_posix() {
        let err = safe_join(&parent(), "/etc/passwd").unwrap_err().to_string();
        assert!(err.contains("[unsafe_path]"), "got: {err}");
    }

    #[test]
    #[cfg(windows)]
    fn safe_join_rejects_absolute_windows_drive() {
        // On Windows, `is_absolute()` returns true for drive-letter paths,
        // so the absolute-path rejection fires. On Unix this is just a
        // single relative component containing a backslash, which is why
        // the test is Windows-only.
        let err = safe_join(&parent(), "C:\\windows\\system32")
            .unwrap_err()
            .to_string();
        assert!(err.contains("[unsafe_path]"), "got: {err}");
    }

    #[test]
    fn safe_join_allows_curdir_segment() {
        // `.` is harmless after normalization.
        let p = safe_join(&parent(), "./foo/bar").unwrap();
        assert_eq!(p, PathBuf::from("/sandbox/./foo/bar"));
    }

    #[test]
    fn safe_join_rejects_traversal_inside_legit_prefix() {
        // `agents/sub/../../escape.md` is exactly the production attack:
        // starts with a real prefix segment but escapes via `..`.
        let err = safe_join(&parent(), "sub/../../escape.md")
            .unwrap_err()
            .to_string();
        assert!(err.contains("[unsafe_path]"), "got: {err}");
    }
}
