//! `DefaultPathResolver` — concrete `PathResolverLike` impl that
//! resolves paths via the standard PEKO_HOME/data layout.
//!
//! Before Phase 7.4, session code called `crate::common::paths::PathResolver::new()`.
//! That constructor is root-only and depends on `dirs` + env-var
//! resolution, which are runtime concerns that shouldn't live in
//! peko-session (a persistence crate). `DefaultPathResolver`
//! replicates the minimal subset of the logic the session manager
//! needs (`agent_sessions_dir`) using `dirs` directly; the daemon
//! can substitute a different `PathResolverLike` impl when it has
//! its own layout.

use std::path::PathBuf;

use peko_subject::PathResolverLike;

/// Default implementation backed by `$PEKO_HOME` / `~/.peko`.
///
/// `agent_sessions_dir(agent)` resolves to
/// `<data_dir>/sessions/<agent>/personal`, matching root's
/// `PathResolver::agent_sessions_dir` for the personal-sessions
/// layout used by `SessionManager`.
pub struct DefaultPathResolver {
    data_dir: PathBuf,
}

impl DefaultPathResolver {
    /// Resolve the default data dir (`$PEKO_HOME/data` or
    /// `~/.peko/data`).
    #[must_use]
    pub fn new() -> Self {
        let data_dir = if let Ok(peko_home) = std::env::var("PEKO_HOME") {
            PathBuf::from(peko_home).join("data")
        } else {
            dirs::home_dir()
                .map(|h| h.join(".peko").join("data"))
                .unwrap_or_else(|| PathBuf::from(".peko/data"))
        };
        Self { data_dir }
    }

    /// Override the data dir explicitly (matches the
    /// `PathResolver::from_overrides` shape).
    #[must_use]
    pub fn with_data_dir(data_dir: PathBuf) -> Self {
        Self { data_dir }
    }
}

impl Default for DefaultPathResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl PathResolverLike for DefaultPathResolver {
    fn agent_sessions_dir(&self, agent: &str) -> PathBuf {
        self.data_dir.join("sessions").join(agent).join("personal")
    }
}
