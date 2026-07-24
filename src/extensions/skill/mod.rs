//! Skill Extension Type Implementation
//!
//! This module contains the Skill adapter for SKILL.md-based extensions and
//! re-exports the global skill location catalog used by the builtin `Skill` tool.
//!
//! Phase 10d moved the `Skill` tool, `SkillFrontmatter` DTO, and the YAML
//! frontmatter parser into `peko_tools_builtin::skill`. Root-side callers
//! (including this module's `SkillAdapter`) continue to use the same types
//! via the re-exports below; the adapter for the new port trait lives in
//! `skill_runtime_impl`.

pub mod adapter;
pub mod skill_runtime_impl;

// Canonical DTOs and parser re-exports — `peko_tools_builtin::skill` is
// the source of truth. Keeping these here preserves the legacy
// `crate::extensions::skill::{SkillFrontmatter, parse_yaml_frontmatter,
// parse_yaml_frontmatter_typed}` paths used by the adapter and any
// downstream consumers.
pub use adapter::{
    load_skills_from_directory, register_skills_with_core, DiscoveredSkill, SkillAdapter,
};
pub use peko_extension_host::skill_catalog::{SkillCatalog, SkillEntry};
pub use peko_tools_builtin::skill::{
    parse_yaml_frontmatter, parse_yaml_frontmatter_typed, SkillFrontmatter,
};
