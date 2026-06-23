//! Extension-related shared types
//!
//! These types represent extension operation results and are used by both
//! CLI commands and services for consistent extension data representation.

use std::path::PathBuf;

/// Extension push result
#[derive(Debug, Clone)]
pub struct ExtensionPushResult {
    pub id: String,
    pub registry_ref: String,
    pub name: String,
    pub version: String,
    pub digest: String,
    pub kind: String,
    pub layers: usize,
    pub total_size: u64,
}

/// Extension pull result
#[derive(Debug, Clone)]
pub struct ExtensionPullResult {
    pub registry_ref: String,
    pub output_path: PathBuf,
    pub manifest_name: String,
    pub manifest_version: String,
    pub manifest_digest: String,
    pub manifest_kind: String,
    pub manifest_layers: usize,
    pub manifest_total_size: u64,
    pub dependencies: Vec<ExtensionDependencyResult>,
}

/// Result of pulling a single extension dependency
#[derive(Debug, Clone)]
pub struct ExtensionDependencyResult {
    pub registry_ref: String,
    pub success: bool,
    pub error: Option<String>,
}
