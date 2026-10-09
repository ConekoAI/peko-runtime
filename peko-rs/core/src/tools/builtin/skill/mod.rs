//! `Skill` tool surface + `SkillRuntime` port.
//!
//! The tool does not scan the workspace itself. It speaks to a runtime
//! port ([`SkillRuntime`]) that the daemon implements with
//! `extensions::skill::reader::WorkspaceSkillRuntime`, which reads
//! `<workspace>/skills/<name>/SKILL.md` directly. Tests point the same
//! runtime at a tempdir.

pub mod body;
pub mod frontmatter;
pub mod tool;

pub use body::{preprocess_dynamic_context, SHELL_TIMEOUT_MS};
pub use frontmatter::{parse_yaml_frontmatter, parse_yaml_frontmatter_typed, SkillFrontmatter};
pub use tool::{SkillTool, ESCAPE_SENTINEL};

use std::path::PathBuf;
use std::sync::Arc;

// ─── DTOs ──────────────────────────────────────────────────────────

/// Entry for a single discovered skill.
///
#[derive(Debug, Clone)]
pub struct SkillEntry {
    /// Skill name (from SKILL.md frontmatter).
    pub name: String,
    /// Absolute path to the skill's `SKILL.md`.
    pub path: PathBuf,
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
}

/// Type alias for the shared runtime handle threaded through every
/// `SkillTool` constructor.
pub type SharedSkillRuntime = Arc<dyn SkillRuntime>;
