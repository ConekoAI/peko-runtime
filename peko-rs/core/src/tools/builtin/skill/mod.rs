//! `peko_tools_builtin::skill` — `Skill` tool surface + `SkillRuntime` port.
//!
//! Phase 10d extracts the `Skill` tool, the YAML-frontmatter parser,
//! the `SkillFrontmatter` / `SkillEntry` DTOs, and the body-substitution
//! helpers out of root. Per the Phase 10 plan rule ("Built-ins must not
//! import daemon state"), the tool here does not scan the workspace
//! itself. It speaks to a runtime port trait ([`SkillRuntime`]) that the
//! daemon side implements (root's
//! `src/extensions/skill/reader.rs::WorkspaceSkillRuntime`).
//!
//! ## DTOs
//!
//! [`SkillFrontmatter`] (parsed YAML frontmatter) and [`SkillEntry`]
//! (resolved skill record) are the canonical types. The root re-exports these
//! from peko-tools-builtin via `pub use crate::tools::builtin::skill::{...};`
//! for backwards compatibility.
//!
//! ## Port
//!
//! [`SkillRuntime`] is the resolve / list surface the `SkillTool` needs.
//! The daemon side implements it with `WorkspaceSkillRuntime`, which
//! reads `<workspace>/skills/<name>/SKILL.md` directly. Tests point the
//! same runtime at a tempdir.

pub mod body;
pub mod frontmatter;
pub mod tool;

pub use body::{preprocess_dynamic_context, SHELL_TIMEOUT_MS};
pub use frontmatter::{parse_yaml_frontmatter, parse_yaml_frontmatter_typed, SkillFrontmatter};
pub use tool::{SkillTool, ESCAPE_SENTINEL};

use std::path::PathBuf;
use std::sync::Arc;

// ─── DTOs (canonical home; root re-exports these) ─────────────────

/// Entry for a single discovered skill.
///
/// `extension_id` is opaque to peko-tools-builtin (it is the
/// `peko_extension_api::ExtensionId` newtype on the root side) so we
/// keep it as a `String` here. The daemon adapter converts at the
/// boundary.
#[derive(Debug, Clone)]
pub struct SkillEntry {
    /// Skill name (from SKILL.md frontmatter).
    pub name: String,
    /// Absolute path to the skill's `SKILL.md`.
    pub path: PathBuf,
    /// Optional owning extension id, for cleanup on uninstall.
    pub extension_id: Option<String>,
}

// ─── SkillRuntime port trait ───────────────────────────────────────

/// Runtime port the `SkillTool` uses to resolve skill files.
///
/// The daemon side implements this with `WorkspaceSkillRuntime`
/// (root's `src/extensions/skill/reader.rs`), which reads directly
/// from the principal's workspace `skills/` directory. Tests point
/// the same runtime at a tempdir.
#[async_trait::async_trait]
pub trait SkillRuntime: Send + Sync {
    /// Resolve a skill by name. Returns `None` if no such skill is
    /// registered.
    fn resolve_skill(&self, name: &str) -> Option<SkillEntry>;

    /// Return all registered skill names, sorted.
    fn list_skills(&self) -> Vec<String>;

    /// Whether a skill with `name` is currently registered.
    fn skill_exists(&self, name: &str) -> bool {
        self.resolve_skill(name).is_some()
    }
}

/// Type alias for the shared runtime handle threaded through every
/// `SkillTool` constructor.
pub type SharedSkillRuntime = Arc<dyn SkillRuntime>;

// ─── Test fixture ──────────────────────────────────────────────────

/// In-memory [`SkillRuntime`] for tests. Mirrors the production
/// workspace-runtime semantics: name → entry, sorted list.
#[cfg(test)]
pub struct TestSkillRuntime {
    entries: std::sync::Mutex<std::collections::HashMap<String, SkillEntry>>,
}

#[cfg(test)]
impl TestSkillRuntime {
    /// Build an empty in-memory skill runtime.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Register (or overwrite) a skill entry.
    pub fn register(&self, name: impl Into<String>, path: impl Into<PathBuf>) {
        let name = name.into();
        let entry = SkillEntry {
            name: name.clone(),
            path: path.into(),
            extension_id: None,
        };
        self.entries
            .lock()
            .expect("TestSkillRuntime mutex poisoned")
            .insert(name, entry);
    }

    /// Remove a skill entry by name. Idempotent.
    pub fn unregister(&self, name: &str) {
        self.entries
            .lock()
            .expect("TestSkillRuntime mutex poisoned")
            .remove(name);
    }
}

#[cfg(test)]
impl Default for TestSkillRuntime {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[async_trait::async_trait]
impl SkillRuntime for TestSkillRuntime {
    fn resolve_skill(&self, name: &str) -> Option<SkillEntry> {
        self.entries
            .lock()
            .expect("TestSkillRuntime mutex poisoned")
            .get(name)
            .cloned()
    }

    fn list_skills(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .entries
            .lock()
            .expect("TestSkillRuntime mutex poisoned")
            .keys()
            .cloned()
            .collect();
        names.sort();
        names
    }
}

// ─── JSON-roundtrip pin ────────────────────────────────────────────
//
// Comprehensive frontmatter tests live in [`frontmatter`] itself; this
// module's tests focus on the runtime port surface.
