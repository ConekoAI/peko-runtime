//! Legacy data migration for ADR-032
//!
//! Backfills `host_runtime_id` for existing agents and teams that were
//! created before runtime identity was introduced.

use anyhow::Result;
use tracing::{info, warn};

use crate::common::paths::PathResolver;

/// Migrate legacy agents and teams to include `host_runtime_id`.
///
/// This is idempotent — it only updates entries where `host_runtime_id` is empty.
pub async fn migrate_legacy_data(resolver: &PathResolver, runtime_id: &str) -> Result<()> {
    let mut migrated_agents = 0;
    let mut migrated_teams = 0;

    // Backfill agent configs
    let agents_root = resolver.agents_root_dir();
    if agents_root.exists() {
        if let Ok(mut entries) = tokio::fs::read_dir(&agents_root).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let agent_path = entry.path();
                if !agent_path.is_dir() {
                    continue;
                }
                let config_path = agent_path.join("config.toml");
                if config_path.exists() {
                    match tokio::fs::read_to_string(&config_path).await {
                        Ok(content) => {
                            if let Ok(mut config) =
                                toml::from_str::<crate::types::agent::AgentConfig>(&content)
                            {
                                if config.host_runtime_id.is_empty() {
                                    config.host_runtime_id = runtime_id.to_string();
                                    match toml::to_string_pretty(&config) {
                                        Ok(updated) => {
                                            if let Err(e) =
                                                tokio::fs::write(&config_path, updated).await
                                            {
                                                warn!(
                                                    "Failed to write migrated agent config {}: {}",
                                                    config_path.display(),
                                                    e
                                                );
                                            } else {
                                                migrated_agents += 1;
                                            }
                                        }
                                        Err(e) => {
                                            warn!(
                                                "Failed to serialize migrated agent config {}: {}",
                                                config_path.display(),
                                                e
                                            );
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            warn!(
                                "Failed to read agent config {}: {}",
                                config_path.display(),
                                e
                            );
                        }
                    }
                }
            }
        }
    }

    // Backfill team metadata
    let teams_dir = resolver.teams_dir();
    if teams_dir.exists() {
        if let Ok(mut entries) = tokio::fs::read_dir(&teams_dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let team_path = entry.path();
                if !team_path.is_dir() {
                    continue;
                }
                let meta_path = team_path.join("team.toml");
                if meta_path.exists() {
                    match tokio::fs::read_to_string(&meta_path).await {
                        Ok(content) => {
                            if let Ok(mut meta) = toml::from_str::<
                                crate::common::types::team::TeamMetadata,
                            >(&content)
                            {
                                if meta.host_runtime_id.is_empty() {
                                    meta.host_runtime_id = runtime_id.to_string();
                                    match toml::to_string_pretty(&meta) {
                                        Ok(updated) => {
                                            if let Err(e) =
                                                tokio::fs::write(&meta_path, updated).await
                                            {
                                                warn!(
                                                    "Failed to write migrated team metadata {}: {}",
                                                    meta_path.display(),
                                                    e
                                                );
                                            } else {
                                                migrated_teams += 1;
                                            }
                                        }
                                        Err(e) => {
                                            warn!(
                                                "Failed to serialize migrated team metadata {}: {}",
                                                meta_path.display(),
                                                e
                                                );
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            warn!(
                                "Failed to read team metadata {}: {}",
                                meta_path.display(),
                                e
                            );
                        }
                    }
                }
            }
        }
    }

    if migrated_agents > 0 || migrated_teams > 0 {
        info!(
            "ADR-032 migration complete: {} agent(s), {} team(s) backfilled with runtime_id {}",
            migrated_agents, migrated_teams, runtime_id
        );
    }

    Ok(())
}
