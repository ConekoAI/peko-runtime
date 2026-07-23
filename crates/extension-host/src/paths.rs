//! Path resolver trait + async-task data dir helper.
//!
//! ## Why a trait?
//!
//! The framework's `ExtensionStore::load_all` walks the
//! skills/agents/commands directories to discover installed
//! extensions. The directory layout is a root-owned concern
//! (`src/common/paths.rs::PathResolver`), and the framework can't
//! import the concrete `PathResolver` struct from the leaf host
//! crate. The trait below carries the three directory methods
//! the framework uses; root's concrete `PathResolver` impls it.
//!
//! Phase 8 commit 1 previously shipped a placeholder trait so the
//! lib.rs doc-link resolved; commit 2 fills in the real method
//! shapes (matching `crate::common::paths::PathResolver`).
//!
//! [`PathResolver`]: crate::paths::PathResolver

use std::path::PathBuf;

/// Default data directory for async-task records (mirrors
/// `src/common::paths::default_data_dir()` exactly).
///
/// **Must stay in sync with
/// `src/common::paths::default_data_dir()`.**
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

/// Default directory for async-task file records
/// (`default_data_dir()` joined with `async_tasks`).
#[must_use]
pub fn default_async_tasks_dir() -> PathBuf {
    default_data_dir().join("async_tasks")
}

/// Default per-agent workspace directory.
///
/// Phase 9b.N.5b.9: added so `peko_engine::AgenticLoop` can fall back to
/// a per-agent default when `AgentView::principal_workspace()` returns
/// `None` (test paths that bypass the principal setup). Mirrors
/// `src/common::paths::PathResolver::agent_workspace`.
#[must_use]
pub fn default_agent_workspace(agent_name: &str) -> PathBuf {
    default_data_dir().join("agents").join(agent_name)
}

/// Cross-boundary view of `crate::common::paths::PathResolver`.
///
/// The framework's `ExtensionStore::load_all_with(&self, &dyn
/// PathResolver)` (or root-shim equivalent) takes this trait so
/// it doesn't depend on the concrete type. Root's concrete
/// `PathResolver` impls it via `#[automatically_derived]`-style
/// delegation.
pub trait PathResolver: Send + Sync {
    /// Path to the skills directory (`{data_dir}/skills`).
    /// Discovery walks this directory for skill manifests.
    fn skills_dir(&self) -> PathBuf;

    /// Path to the agents directory (`{data_dir}/agents`).
    /// Discovery walks this directory for agent manifests.
    fn agents_dir(&self) -> PathBuf;

    /// Path to the slash-commands directory
    /// (`{data_dir}/commands`). Discovery walks this directory
    /// for command manifests.
    fn commands_dir(&self) -> PathBuf;
}
