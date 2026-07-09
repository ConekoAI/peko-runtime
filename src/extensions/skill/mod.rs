//! Skill Extension Type Implementation
//!
//! This module contains the Skill adapter for SKILL.md-based extensions and
//! re-exports the global skill location catalog used by the builtin `Skill` tool.

pub mod adapter;

pub use crate::extensions::framework::skill_catalog::{SkillCatalog, SkillEntry};
pub use adapter::{
    load_skills_from_directory, register_skills_with_core, DiscoveredSkill, SkillAdapter,
    SkillFrontmatter,
};
