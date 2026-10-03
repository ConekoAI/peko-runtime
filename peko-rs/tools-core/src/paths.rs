//! Default runtime data and fallback workspace paths.
//! Keep these helpers in sync with the host PathResolver.

use std::path::PathBuf;

/// Default runtime data directory.
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
    default_data_dir().join("roles").join(agent_name)
}
