//! Path resolver trait + async-task data dir helper.
//!
//! ## Why a trait?
//!
//! The directory layout is a root-owned concern
//! (`src/common/paths.rs::PathResolver`), and the framework can't
//! import the concrete `PathResolver` struct from the leaf host
//! crate. The trait below carries the directory method
//! the framework uses; root's concrete `PathResolver` impls it.
//!
//! [`PathResolver`]: crate::extensions::framework::paths::PathResolver

use std::path::PathBuf;

pub use peko_tools_core::paths::{default_agent_workspace, default_data_dir};

/// Default directory for async task records.
#[must_use]
pub fn default_async_tasks_dir() -> PathBuf {
    default_data_dir().join("async_tasks")
}

/// Cross-boundary view of `crate::common::paths::PathResolver`.
///
/// The framework takes this trait so it doesn't depend on the
/// concrete type. Root's concrete `PathResolver` impls it via
/// `#[automatically_derived]`-style delegation.
pub trait PathResolver: Send + Sync {
    /// Path to the agents directory (`{data_dir}/agents`).
    fn roles_dir(&self) -> PathBuf;
}
