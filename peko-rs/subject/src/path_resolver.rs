//! `PathResolverLike` — narrow trait port for the root-owned
//! `PathResolver` (`src/common/paths.rs`). The full `PathResolver`
//! struct lives in root and provides many paths
//! (`config_dir`/`data_dir`/`cache_dir`/`agent_sessions_dir`/etc.).
//! The session manager only needs `agent_sessions_dir(agent)` for
//! session storage layout, so peko-session consumes that one method
//! through this trait. Root's `PathResolver` implements
//! `PathResolverLike` next to its definition; the impl is removed in
//! Phase 16 once root-side callers migrate.

use std::path::PathBuf;

/// Minimum surface `SessionManager` / `Session` need from a path
/// resolver.
pub trait PathResolverLike: Send + Sync + 'static {
    /// Resolve the directory where an agent's session storage lives.
    fn agent_sessions_dir(&self, agent: &str) -> PathBuf;

    /// Resolve the root sessions directory (parent of all per-agent
    /// session dirs). Default impl strips the `agent/personal` suffix
    /// from `agent_sessions_dir` for callers that don't override.
    /// `peko-session`'s `DefaultPathResolver` follows root's
    /// `<data_dir>/sessions` layout, so the default is exact; root's
    /// `PathResolver` overrides explicitly.
    fn sessions_root(&self) -> PathBuf {
        // Strip the trailing `/{agent}/personal` from any agent's
        // sessions dir to recover the root. Use `agent_sessions_dir`
        // with a placeholder agent to avoid hard-coding the layout
        // here — it returns the same root for any agent name.
        self.agent_sessions_dir("_layout_probe")
            .ancestors()
            .nth(2)
            .map(|p| p.to_path_buf())
            .unwrap_or_default()
    }
}
