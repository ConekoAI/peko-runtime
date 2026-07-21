//! Path helpers shared by the built-in filesystem tools.
//!
//! Phase 10a lifted [`expand_tilde`] out of root's `common::paths` so
//! `peko-tools-builtin` can offer `Read`/`Write`/`Edit`/`Glob`/`Grep`
//! without taking a dep on the root crate's `common` module. The
//! implementation is identical to the root copy — tilde-expansion is
//! purely lexical and the only external dep is `dirs::home_dir()`.

use std::path::PathBuf;

/// Expand a leading `~` or `~/` to the user's home directory.
///
/// Falls back to the literal path (no expansion) if `dirs::home_dir()`
/// returns `None` (which happens on platforms without a HOME env var).
pub fn expand_tilde(path: impl AsRef<str>) -> PathBuf {
    let path = path.as_ref();
    if path == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"));
    }
    if let Some(rest) = path.strip_prefix("~/") {
        let mut home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"));
        home.push(rest);
        return home;
    }
    PathBuf::from(path)
}