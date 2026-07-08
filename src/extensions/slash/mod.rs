//! Slash Command Extension Type Implementation
//!
//! This module contains the Slash adapter for COMMAND.md-based extensions.

pub mod adapter;

pub use adapter::{
    load_commands_from_directory, register_commands_with_core, DiscoveredCommand, SlashAdapter,
    SlashFrontmatter,
};
