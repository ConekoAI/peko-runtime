//! JSON to TOML Migration Utilities
//!
//! Provides utilities for migrating agent configurations from the old
//! JSON-based ConfigRegistry format to the new TOML format.

use crate::common::paths::PathResolver;
use anyhow::{Context, Result};
use std::path::Path;
use tracing::{info, warn};

/// Summary of migration results
#[derive(Debug, Default)]
pub struct MigrationSummary {
    /// Number of entries successfully migrated
    pub migrated: usize,
    /// Number of entries skipped (TOML already exists)
    pub skipped: usize,
    /// Number of entries that failed
    pub failed: usize,
    /// Paths of migrated files
    pub migrated_paths: Vec<String>,
    /// Error messages for failed entries
    pub errors: Vec<String>,
}

/// Migrate a JSON registry entry to TOML format
///
/// Reads a JSON file from the old ConfigRegistry format and writes
/// the agent configuration to the canonical TOML location.
///
/// # Arguments
/// * `json_path` - Path to the JSON file
/// * `resolver` - PathResolver for determining TOML destination
/// * `backup_json` - If true, backup the JSON file after migration
///
/// Returns the TOML path on success, None if TOML already exists.
pub async fn migrate_json_entry(
    json_path: &Path,
    resolver: &PathResolver,
    backup_json: bool,
) -> Result<Option<std::path::PathBuf>> {
    use tokio::io::AsyncReadExt;

    // Read JSON file
    let mut file = tokio::fs::File::open(json_path)
        .await
        .with_context(|| format!("Failed to open JSON file: {}", json_path.display()))?;

    let mut content = String::new();
    file.read_to_string(&mut content)
        .await
        .with_context(|| format!("Failed to read JSON file: {}", json_path.display()))?;

    // Parse JSON using a local struct to avoid dependency on deleted config_registry
    #[derive(serde::Deserialize)]
    struct JsonAgentConfigEntry {
        name: String,
        #[serde(default)]
        team_id: Option<String>,
        config: crate::types::agent::AgentConfig,
    }

    let entry: JsonAgentConfigEntry = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse JSON: {}", json_path.display()))?;

    // Determine TOML path
    let team = entry.team_id.as_deref().unwrap_or("default");
    let toml_path = resolver.agent_config(&entry.name, Some(team));

    // Skip if TOML already exists
    if toml_path.exists() {
        info!(
            "TOML already exists at {}, skipping migration of {}",
            toml_path.display(),
            json_path.display()
        );
        return Ok(None);
    }

    // Ensure parent directory exists
    if let Some(parent) = toml_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
    }

    // Write TOML
    let toml_content =
        toml::to_string_pretty(&entry.config).context("Failed to serialize config to TOML")?;

    tokio::fs::write(&toml_path, &toml_content)
        .await
        .with_context(|| format!("Failed to write TOML file: {}", toml_path.display()))?;

    info!(
        "Migrated {} to {}",
        json_path.display(),
        toml_path.display()
    );

    // Optionally backup JSON
    if backup_json {
        let backup_path = json_path.with_extension("json.bak");
        tokio::fs::rename(json_path, &backup_path)
            .await
            .with_context(|| format!("Failed to backup JSON to: {}", backup_path.display()))?;
    }

    Ok(Some(toml_path))
}

/// Migrate all JSON entries from a directory to TOML
///
/// Scans the directory for .json files and migrates each to the
/// canonical TOML location.
pub async fn migrate_json_dir(
    data_dir: &Path,
    resolver: &PathResolver,
    backup_json: bool,
) -> Result<MigrationSummary> {
    let mut summary = MigrationSummary::default();

    let mut entries = match tokio::fs::read_dir(data_dir).await {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(summary),
        Err(e) => return Err(e.into()),
    };

    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();

        // Only process .json files
        if path.extension().map_or(false, |e| e == "json") {
            match migrate_json_entry(&path, resolver, backup_json).await {
                Ok(Some(toml_path)) => {
                    summary.migrated += 1;
                    summary.migrated_paths.push(toml_path.display().to_string());
                }
                Ok(None) => {
                    summary.skipped += 1;
                }
                Err(e) => {
                    summary.failed += 1;
                    summary.errors.push(format!("{}: {}", path.display(), e));
                    warn!("Failed to migrate {}: {}", path.display(), e);
                }
            }
        }
    }

    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_migration_summary_default() {
        let summary = MigrationSummary::default();
        assert_eq!(summary.migrated, 0);
        assert_eq!(summary.skipped, 0);
        assert_eq!(summary.failed, 0);
        assert!(summary.migrated_paths.is_empty());
        assert!(summary.errors.is_empty());
    }
}
