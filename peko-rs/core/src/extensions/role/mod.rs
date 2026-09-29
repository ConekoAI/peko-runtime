//! Role Extension Type Implementation
//!
//! This module contains the Role adapter for ROLE.md-based extensions
//! (ADR-064: roles are the workspace templates live agents are
//! initiated from).

pub mod adapter;

pub use adapter::{
    load_roles_from_directory, DiscoveredRole, RoleAdapter, WorkspaceRolesPromptHandler,
    ROLE_HOOK_PRIORITY,
};
