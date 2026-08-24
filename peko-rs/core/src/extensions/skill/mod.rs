//! Workspace-backed skill surface (ADR-047 §2.4).
//!
//! Phase 2 PR 1 deletes the `SkillAdapter` + the per-extension
//! `SkillCatalog` registration path. Skills are now files inside the
//! principal's workspace (`<workspace>/skills/<id>/SKILL.md`); the
//! [`WorkspaceSkillRuntime`](reader::WorkspaceSkillRuntime) is the
//! `SkillRuntime` impl the `SkillTool` uses to resolve them.
//!
//! The re-exports below preserve the legacy
//! `crate::extensions::skill::{SkillFrontmatter, parse_yaml_frontmatter,
//! parse_yaml_frontmatter_typed}` paths used by other parts of the
//! codebase (validation, the builtin `Skill` tool, etc.).

pub mod reader;

// Canonical DTOs and parser re-exports — `peko_tools_builtin::skill`
// is the source of truth.
pub use crate::tools::builtin::skill::{
    parse_yaml_frontmatter, parse_yaml_frontmatter_typed, SkillFrontmatter,
};
pub use reader::WorkspaceSkillRuntime;