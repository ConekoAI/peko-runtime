//! `Skill` tool — re-export shim.
//!
//! Phase 10d moves the `Skill` tool, the YAML frontmatter parser, and
//! the dynamic-context preprocessor into [`peko_tools_builtin::skill`].
//! This file preserves the legacy
//! `crate::tools::builtin::skill::SkillTool` path so existing callers
//! continue to compile, and forwards `pub mod preprocess` to keep the
//! `crate::tools::builtin::skill::preprocess::preprocess_dynamic_context`
//! path available for any external references (none currently).

pub mod preprocess;

pub use peko_tools_builtin::skill::SkillTool;
