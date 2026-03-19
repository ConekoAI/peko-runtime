//! Team-related shared types
//!
//! These types represent team entities and are used by both
//! CLI commands and API routes for consistent data representation.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Team metadata stored in team.toml
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamMetadata {
    pub name: String,
    pub description: Option<String>,
    pub created_at: String,
}

/// Team information for listing and display
#[derive(Debug, Clone)]
pub struct TeamInfo {
    pub name: String,
    pub metadata: Option<TeamMetadata>,
    pub agent_count: usize,
    pub path: PathBuf,
}

/// Team creation result
#[derive(Debug, Clone)]
pub struct TeamCreationResult {
    pub metadata: TeamMetadata,
    pub path: PathBuf,
}

/// Team deletion result
#[derive(Debug, Clone)]
pub struct TeamDeletionResult {
    pub name: String,
    pub agents_deleted: usize,
}
