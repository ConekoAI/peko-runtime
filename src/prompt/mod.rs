//! System prompt generation and management
//!
//! Provides OpenClaw-compatible prompt assembly with:
//! - Bootstrap file injection (AGENTS.md, SOUL.md, etc.)
//! - Multi-section prompt building
//! - Prompt modes (full, minimal, none)

pub mod bootstrap;
pub mod builder;
pub mod placeholder;

pub use builder::{PromptMode, SystemPromptBuilder};
pub use placeholder::{Placeholder, replace_placeholders};
