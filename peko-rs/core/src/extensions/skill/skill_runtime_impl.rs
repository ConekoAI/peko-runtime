//! `SkillCatalogRuntime` — root-side adapter for the `SkillRuntime` port.
//!
//! Phase 10d lifts the `Skill` tool into `peko_tools_builtin::skill`.
//! The tool surface there speaks to a
//! [`peko_tools_builtin::skill::SkillRuntime`] port trait so the
//! built-in crate can stay free of root-only deps. This file is the
//! production adapter: it wraps the global `SkillCatalog` populated by
//! `ExtensionStore` whenever skills are loaded or installed.
//!
//! The adapter is intentionally thin — three one-liners over the
//! catalog. The catalog already exposes `resolve`, `list`, etc. on
//! `&self`, so the runtime trait's required methods just delegate.

use async_trait::async_trait;

use peko_tools_builtin::skill::{SkillEntry, SkillRuntime};

use crate::extensions::framework::skill_catalog::SkillCatalog;

/// Adapter exposing the global `SkillCatalog` as a `SkillRuntime`.
pub struct SkillCatalogRuntime;

impl SkillCatalogRuntime {
    /// Build a runtime adapter over the global catalog.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for SkillCatalogRuntime {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SkillRuntime for SkillCatalogRuntime {
    fn resolve_skill(&self, name: &str) -> Option<SkillEntry> {
        SkillCatalog::global()
            .resolve(name)
            .map(|entry| SkillEntry {
                name: entry.name,
                path: entry.path,
                // `extension_id` is `peko_extension_api::ExtensionId` on
                // the root side and `Option<String>` in
                // peko_tools_builtin; we collapse at the boundary.
                extension_id: entry.extension_id.map(|id| id.0),
            })
    }

    fn list_skills(&self) -> Vec<String> {
        SkillCatalog::global().list()
    }
}
