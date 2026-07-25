//! Default data directory helpers — moved here from root's
//! `crate::extensions::framework::paths` (Phase F2 foldback) so the
//! engine and tools can reach `default_data_dir()` /
//! `default_agent_workspace()` without depending on root (which
//! would create a cycle: root→engine, engine→root).
//!
//! **Must stay in sync with `src/common/paths.rs::PathResolver`**.
//! The trait lives there; these helpers are pure path math.

use std::path::PathBuf;

/// Default data directory for async-task records.
#[must_use]
pub fn default_data_dir() -> PathBuf {
    std::env::var_os("PEKO_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::data_dir()
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join("peko")
        })
}

/// Default per-agent workspace directory.
///
/// Mirrors `src/common::paths::PathResolver::agent_workspace`.
/// `peko_engine::AgenticLoop` falls back to this when
/// `AgentView::principal_workspace()` returns `None` (test paths that
/// bypass the principal setup).
#[must_use]
pub fn default_agent_workspace(agent_name: &str) -> PathBuf {
    default_data_dir().join("agents").join(agent_name)
}
