//! Skill Extension Type Implementation
//!
//! This module contains the Skill adapter for SKILL.md-based extensions.

pub mod adapter;

pub use adapter::{
    load_skills_from_directory, register_skills_with_core, DiscoveredSkill, SkillAdapter,
    SkillFrontmatter,
};
