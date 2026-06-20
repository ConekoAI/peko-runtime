//! Legacy data migration for ADR-032 and ADR-033
//!
//! Backfills `host_runtime_id` and `owner_id` for existing agents and teams
//! that were created before these fields were introduced.

use anyhow::{Context, Result};
use tracing::{info, warn};

use crate::auth::principal::Principal;
use crate::common::paths::PathResolver;

/// True if the `Principal` is the legacy "no owner" sentinel
/// (`Principal::User("")`). Used to decide whether to backfill the
/// local runtime's owner.
fn is_owner_empty_user(owner: &Principal) -> bool {
    matches!(owner, Principal::User(s) if s.is_empty())
}

/// Migrate legacy agents and teams to include `host_runtime_id` and `owner_id`.
///
/// This is idempotent — it only updates entries where fields are empty.
pub async fn migrate_legacy_data(resolver: &PathResolver, runtime_id: &str) -> Result<()> {
    migrate_adr032(resolver, runtime_id).await?;
    migrate_adr033(resolver, runtime_id).await?;
    crate::runtime::migration_v3::migrate_adr_provider_catalog_v3(resolver).await?;
    Ok(())
}

/// ADR-032: Backfill `host_runtime_id` for existing agents and teams.
async fn migrate_adr032(resolver: &PathResolver, runtime_id: &str) -> Result<()> {
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
                            if let Ok(config) =
                                toml::from_str::<crate::types::agent::AgentConfig>(&content)
                            {
                                if config.host_runtime_id.is_empty() {
                                    match set_host_runtime_id_in_toml(&content, runtime_id) {
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
                                                "Failed to update host_runtime_id in agent config {}: {}",
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
                            if let Ok(meta) =
                                toml::from_str::<crate::common::types::team::TeamMetadata>(&content)
                            {
                                if meta.host_runtime_id.is_empty() {
                                    match set_host_runtime_id_in_toml(&content, runtime_id) {
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
                                                "Failed to update host_runtime_id in team metadata {}: {}",
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

/// ADR-033: Backfill `owner_id` and `permissions` for existing agents and teams.
async fn migrate_adr033(resolver: &PathResolver, runtime_id: &str) -> Result<()> {
    let local_owner = format!("local:{runtime_id}");
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
                            if let Ok(config) =
                                toml::from_str::<crate::types::agent::AgentConfig>(&content)
                            {
                                if is_owner_empty_user(&config.owner) {
                                    match set_owner_in_toml(&content, &local_owner) {
                                        Ok(updated) => {
                                            let tmp = config_path.with_extension("tmp");
                                            if tokio::fs::write(&tmp, updated).await.is_ok() {
                                                if let Err(e) =
                                                    tokio::fs::rename(&tmp, &config_path).await
                                                {
                                                    warn!("Failed to rename migrated agent config {}: {}", config_path.display(), e);
                                                    let _ = tokio::fs::remove_file(&tmp).await;
                                                } else {
                                                    migrated_agents += 1;
                                                    info!(
                                                        "Backfilled owner_id for agent {}",
                                                        config.name
                                                    );
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            warn!(
                                                "Failed to update owner in agent config {}: {}",
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
                            if let Ok(meta) =
                                toml::from_str::<crate::common::types::team::TeamMetadata>(&content)
                            {
                                if is_owner_empty_user(&meta.owner) {
                                    match set_owner_in_toml(&content, &local_owner) {
                                        Ok(updated) => {
                                            let tmp = meta_path.with_extension("tmp");
                                            if tokio::fs::write(&tmp, updated).await.is_ok() {
                                                if let Err(e) =
                                                    tokio::fs::rename(&tmp, &meta_path).await
                                                {
                                                    warn!("Failed to rename migrated team metadata {}: {}", meta_path.display(), e);
                                                    let _ = tokio::fs::remove_file(&tmp).await;
                                                } else {
                                                    migrated_teams += 1;
                                                    info!(
                                                        "Backfilled owner_id for team {}",
                                                        meta.name
                                                    );
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            warn!(
                                                "Failed to update owner in team metadata {}: {}",
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
            "ADR-033 migration complete: {} agent(s), {} team(s) backfilled with owner_id {}",
            migrated_agents, migrated_teams, local_owner
        );
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Targeted TOML helpers (preserves unknown fields, comments, and key ordering)
// ---------------------------------------------------------------------------

/// Set `host_runtime_id = "..."` in a TOML document without disturbing
/// other fields.  Parses as `toml::Value`, inserts/replaces the key, and
/// re-serializes.  This preserves `[provider]` and other fields that
/// `AgentConfig` knows about but marks with `skip_serializing`.
fn set_host_runtime_id_in_toml(content: &str, runtime_id: &str) -> Result<String> {
    let mut root: toml::Value = toml::from_str(content)
        .with_context(|| "Failed to parse TOML for host_runtime_id backfill")?;

    if let toml::Value::Table(ref mut tbl) = root {
        tbl.insert(
            "host_runtime_id".to_string(),
            toml::Value::String(runtime_id.to_string()),
        );
    } else {
        anyhow::bail!("TOML root is not a table");
    }

    toml::to_string_pretty(&root).context("Failed to serialize updated TOML")
}

/// Set `owner = { kind = "user", id = "..." }` in a TOML document
/// without disturbing other fields.  Uses the same targeted-edit
/// approach as `set_host_runtime_id_in_toml`.
fn set_owner_in_toml(content: &str, owner_id: &str) -> Result<String> {
    let mut root: toml::Value = toml::from_str(content)
        .with_context(|| "Failed to parse TOML for owner backfill")?;

    if let toml::Value::Table(ref mut tbl) = root {
        let mut owner_table = toml::map::Map::new();
        owner_table.insert("kind".to_string(), toml::Value::String("user".to_string()));
        owner_table.insert("id".to_string(), toml::Value::String(owner_id.to_string()));
        tbl.insert("owner".to_string(), toml::Value::Table(owner_table));
    } else {
        anyhow::bail!("TOML root is not a table");
    }

    toml::to_string_pretty(&root).context("Failed to serialize updated TOML")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_set_host_runtime_id_preserves_provider_block() {
        let input = r#"version = "1.0"
name = "test-agent"

[provider]
provider_type = "openai_compatible"
api_key = "mock-key"
base_url = "http://localhost:8080"
default_model = "default"
"#;
        let updated = set_host_runtime_id_in_toml(input, "runtime-123").unwrap();
        // The provider block must survive the edit.
        assert!(updated.contains("[provider]"), "provider block preserved: {updated}");
        assert!(updated.contains("api_key = \"mock-key\""), "api_key preserved: {updated}");
        assert!(updated.contains("base_url = \"http://localhost:8080\""), "base_url preserved: {updated}");
        assert!(updated.contains("host_runtime_id = \"runtime-123\""), "host_runtime_id set: {updated}");
    }

    #[test]
    fn test_set_owner_preserves_provider_block() {
        let input = r#"version = "1.0"
name = "test-agent"

[provider]
provider_type = "openai_compatible"
api_key = "mock-key"
base_url = "http://localhost:8080"
default_model = "default"
"#;
        let updated = set_owner_in_toml(input, "local:runtime-456").unwrap();
        // The provider block must survive the edit.
        assert!(updated.contains("[provider]"), "provider block preserved: {updated}");
        assert!(updated.contains("api_key = \"mock-key\""), "api_key preserved: {updated}");
        assert!(updated.contains("base_url = \"http://localhost:8080\""), "base_url preserved: {updated}");
        assert!(updated.contains("[owner]"), "owner table set: {updated}");
        assert!(updated.contains("kind = \"user\""), "owner kind is user: {updated}");
        assert!(updated.contains("id = \"local:runtime-456\""), "owner id set: {updated}");
    }

    #[test]
    fn test_set_host_runtime_id_idempotent() {
        let input = r#"version = "1.0"
name = "test-agent"
host_runtime_id = "old-runtime"
"#;
        let updated = set_host_runtime_id_in_toml(input, "new-runtime").unwrap();
        assert!(updated.contains("host_runtime_id = \"new-runtime\""), "host_runtime_id updated: {updated}");
        // Should only appear once
        let count = updated.matches("host_runtime_id").count();
        assert_eq!(count, 1, "host_runtime_id appears exactly once: {updated}");
    }
}