//! Team Management Commands
//!
//! Provides CLI commands for managing teams - logical groupings of agents.
//! Teams are stored as directories under ~/.pekobot/teams/{team}/agents/

use crate::commands::identifier::{validate_team_name, ValidationError};
use crate::commands::GlobalPaths;
use crate::types::agent::AgentConfig;
use anyhow::Result;
use clap::Subcommand;
use std::collections::HashMap;

/// Team management subcommands
///
/// Teams are logical groupings for agents. Each team has its own directory
/// structure under ~/.pekobot/teams/{team}/
///
/// Examples:
///   # Create a new team
///   pekobot team create dev-team
///
///   # List all teams
///   pekobot team list
///
///   # Show team details
///   pekobot team show dev-team
///
///   # Delete a team (and all its agents)
///   pekobot team delete dev-team --force
#[derive(Subcommand)]
#[command(disable_version_flag = true)]
pub enum TeamCommands {
    /// Create a new team
    Create {
        /// Team name (alphanumeric, hyphens, underscores)
        name: String,
        /// Description (optional)
        #[arg(short, long)]
        description: Option<String>,
    },

    /// List all teams
    List {
        /// Show detailed information including agents
        #[arg(short, long)]
        long: bool,
    },

    /// Show team details
    Show {
        /// Team name
        name: String,
    },

    /// Delete a team and all its agents
    Delete {
        /// Team name
        name: String,
        /// Skip confirmation
        #[arg(short, long)]
        force: bool,
    },
}

/// Handle team commands
pub async fn handle_team(cmd: TeamCommands, paths: &GlobalPaths, json: bool) -> Result<()> {
    match cmd {
        TeamCommands::Create { name, description } => {
            handle_team_create(paths, &name, description, json).await
        }
        TeamCommands::List { long } => handle_team_list(paths, long, json).await,
        TeamCommands::Show { name } => handle_team_show(paths, &name, json).await,
        TeamCommands::Delete { name, force } => {
            handle_team_delete(paths, &name, force, json).await
        }
    }
}

/// Handle team create command
async fn handle_team_create(
    paths: &GlobalPaths,
    name: &str,
    description: Option<String>,
    json: bool,
) -> Result<()> {
    // Validate team name
    if let Err(e) = validate_team_name(name) {
        match e {
            ValidationError::Empty => anyhow::bail!("Team name cannot be empty"),
            ValidationError::TooLong(max) => {
                anyhow::bail!("Team name exceeds maximum length of {} characters", max)
            }
            ValidationError::Reserved(reserved) => {
                anyhow::bail!("'{}' is a reserved name and cannot be used", reserved)
            }
            ValidationError::ContainsPathSeparators => {
                anyhow::bail!("Team name cannot contain path separators (/ or \\)")
            }
            ValidationError::InvalidHyphenPlacement => {
                anyhow::bail!("Team name cannot start or end with a hyphen")
            }
            ValidationError::InvalidCharacter(ch) => {
                anyhow::bail!("Team name contains invalid character: '{}'", ch)
            }
        }
    }

    let team_dir = paths.team_dir(name);

    // Check if team already exists
    if team_dir.exists() {
        anyhow::bail!("Team '{}' already exists", name);
    }

    // Create team directory structure
    let agents_dir = team_dir.join("agents");
    tokio::fs::create_dir_all(&agents_dir).await?;

    // Create team metadata file
    let metadata = TeamMetadata {
        name: name.to_string(),
        description,
        created_at: chrono::Utc::now().to_rfc3339(),
    };

    let metadata_path = team_dir.join("team.toml");
    let metadata_content = toml::to_string_pretty(&metadata)?;
    tokio::fs::write(&metadata_path, metadata_content).await?;

    if json {
        println!(
            "{{\"success\": true, \"name\": \"{}\", \"path\": \"{}\"}}",
            name,
            team_dir.display()
        );
    } else {
        println!("✅ Created team '{}'", name);
        println!("   Path: {}", team_dir.display());
        if let Some(desc) = &metadata.description {
            println!("   Description: {}", desc);
        }
        println!();
        println!("   You can now create agents in this team:");
        println!("     pekobot agent create {}/<agent-name>", name);
    }

    Ok(())
}

/// Handle team list command
async fn handle_team_list(paths: &GlobalPaths, long: bool, json: bool) -> Result<()> {
    let teams_dir = paths.teams_dir();

    if !teams_dir.exists() {
        if json {
            println!("{{\"teams\": []}}");
        } else {
            println!("No teams found.");
            println!("Create one with: pekobot team create <name>");
        }
        return Ok(());
    }

    let mut teams: Vec<TeamInfo> = Vec::new();
    let mut entries = tokio::fs::read_dir(&teams_dir).await?;

    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let team_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        // Skip invalid team names (could be temp files, etc.)
        if validate_team_name(&team_name).is_err() {
            continue;
        }

        let metadata = load_team_metadata(&path).await.ok();
        let agent_count = count_agents_in_team(&path).await;

        teams.push(TeamInfo {
            name: team_name,
            metadata,
            agent_count,
            path,
        });
    }

    // Sort teams: default first, then alphabetically
    teams.sort_by(|a, b| {
        if a.name == "default" {
            return std::cmp::Ordering::Less;
        }
        if b.name == "default" {
            return std::cmp::Ordering::Greater;
        }
        a.name.cmp(&b.name)
    });

    if json {
        let teams_json: Vec<_> = teams
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.name,
                    "agent_count": t.agent_count,
                    "description": t.metadata.as_ref().and_then(|m| m.description.clone()),
                    "created_at": t.metadata.as_ref().map(|m| m.created_at.clone()),
                })
            })
            .collect();
        println!("{}", serde_json::json!({"teams": teams_json}));
    } else if teams.is_empty() {
        println!("No teams found.");
        println!("Create one with: pekobot team create <name>");
    } else {
        println!("📁 Teams ({} found):", teams.len());
        println!();

        for team in &teams {
            let is_default = team.name == "default";
            let icon = if is_default { "⭐" } else { "📁" };

            if long {
                println!("{} {}", icon, team.name);
                if let Some(ref metadata) = team.metadata {
                    if let Some(ref desc) = metadata.description {
                        println!("   Description: {}", desc);
                    }
                    println!("   Created: {}", format_timestamp(&metadata.created_at));
                }
                println!("   Agents: {}", team.agent_count);
                println!("   Path: {}", team.path.display());
                println!();
            } else {
                let desc_marker = if team.metadata.as_ref().and_then(|m| m.description.as_ref()).is_some() {
                    " •"
                } else {
                    ""
                };
                println!("{} {}{} ({} agents)", icon, team.name, desc_marker, team.agent_count);
            }
        }
    }

    Ok(())
}

/// Handle team show command
async fn handle_team_show(paths: &GlobalPaths, name: &str, json: bool) -> Result<()> {
    // Validate team name
    if let Err(e) = validate_team_name(name) {
        anyhow::bail!("Invalid team name '{}': {}", name, e);
    }

    let team_dir = paths.team_dir(name);

    if !team_dir.exists() {
        anyhow::bail!("Team '{}' not found", name);
    }

    let metadata = load_team_metadata(&team_dir).await.ok();
    let agents = list_agents_in_team(&team_dir).await?;

    if json {
        let agents_json: Vec<_> = agents
            .iter()
            .map(|(name, config)| {
                serde_json::json!({
                    "name": name,
                    "provider": format!("{:?}", config.provider.provider_type),
                    "model": config.provider.default_model,
                    "description": config.description,
                })
            })
            .collect();

        println!(
            "{}",
            serde_json::json!({
                "name": name,
                "path": team_dir.display().to_string(),
                "description": metadata.as_ref().and_then(|m| m.description.clone()),
                "created_at": metadata.as_ref().map(|m| m.created_at.clone()),
                "agents": agents_json,
                "agent_count": agents.len(),
            })
        );
    } else {
        println!("📁 Team: {}", name);
        if let Some(ref metadata) = metadata {
            if let Some(ref desc) = metadata.description {
                println!("   Description: {}", desc);
            }
            println!("   Created: {}", format_timestamp(&metadata.created_at));
        }
        println!("   Path: {}", team_dir.display());
        println!();

        if agents.is_empty() {
            println!("   No agents in this team.");
            println!("   Create one with: pekobot agent create {}/<agent-name>", name);
        } else {
            println!("   Agents ({}):", agents.len());
            for (agent_name, config) in &agents {
                println!("   📦 {}", agent_name);
                if let Some(ref desc) = config.description {
                    println!("      {}", desc);
                }
                println!(
                    "      Provider: {:?}",
                    config.provider.provider_type
                );
            }
        }
    }

    Ok(())
}

/// Handle team delete command
async fn handle_team_delete(
    paths: &GlobalPaths,
    name: &str,
    force: bool,
    json: bool,
) -> Result<()> {
    // Validate team name
    if let Err(e) = validate_team_name(name) {
        anyhow::bail!("Invalid team name '{}': {}", name, e);
    }

    // Prevent deletion of default team
    if name == "default" {
        anyhow::bail!("Cannot delete the 'default' team");
    }

    let team_dir = paths.team_dir(name);

    if !team_dir.exists() {
        anyhow::bail!("Team '{}' not found", name);
    }

    let agent_count = count_agents_in_team(&team_dir).await;

    // Confirm deletion
    if !force {
        println!("⚠️  This will permanently delete team '{}'.", name);
        if agent_count > 0 {
            println!("   It contains {} agent(s) that will also be deleted.", agent_count);
        }
        println!("   This action cannot be undone.");
        print!("   Continue? [y/N] ");
        std::io::Write::flush(&mut std::io::stdout())?;

        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;

        if !input.trim().eq_ignore_ascii_case("y") {
            if json {
                println!("{{\"success\": false, \"reason\": \"cancelled\"}}");
            } else {
                println!("Cancelled.");
            }
            return Ok(());
        }
    }

    // Delete team directory
    tokio::fs::remove_dir_all(&team_dir).await?;

    if json {
        println!(
            "{{\"success\": true, \"name\": \"{}\", \"agents_deleted\": {}}}",
            name, agent_count
        );
    } else {
        println!("✅ Deleted team '{}'", name);
        if agent_count > 0 {
            println!("   Removed {} agent(s)", agent_count);
        }
    }

    Ok(())
}

// ================================================================================
// Helper Types and Functions
// ================================================================================

/// Team metadata stored in team.toml
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct TeamMetadata {
    name: String,
    description: Option<String>,
    created_at: String,
}

/// Team information for listing
#[derive(Debug, Clone)]
struct TeamInfo {
    name: String,
    metadata: Option<TeamMetadata>,
    agent_count: usize,
    path: std::path::PathBuf,
}

/// Load team metadata from team.toml
async fn load_team_metadata(team_dir: &std::path::PathBuf) -> Result<TeamMetadata> {
    let metadata_path = team_dir.join("team.toml");
    let content = tokio::fs::read_to_string(&metadata_path).await?;
    let metadata: TeamMetadata = toml::from_str(&content)?;
    Ok(metadata)
}

/// Count agents in a team
async fn count_agents_in_team(team_dir: &std::path::PathBuf) -> usize {
    let agents_dir = team_dir.join("agents");

    if !agents_dir.exists() {
        return 0;
    }

    match tokio::fs::read_dir(&agents_dir).await {
        Ok(mut entries) => {
            let mut count = 0;
            while let Ok(Some(entry)) = entries.next_entry().await {
                if entry.path().is_dir() {
                    count += 1;
                }
            }
            count
        }
        Err(_) => 0,
    }
}

/// List agents in a team with their configs
async fn list_agents_in_team(
    team_dir: &std::path::PathBuf,
) -> Result<Vec<(String, AgentConfig)>> {
    let agents_dir = team_dir.join("agents");
    let mut agents = Vec::new();

    if !agents_dir.exists() {
        return Ok(agents);
    }

    let mut entries = tokio::fs::read_dir(&agents_dir).await?;

    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let agent_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        let config_path = path.join("config.toml");
        if let Ok(content) = tokio::fs::read_to_string(&config_path).await {
            if let Ok(config) = toml::from_str::<AgentConfig>(&content) {
                agents.push((agent_name, config));
            }
        }
    }

    // Sort alphabetically
    agents.sort_by(|a, b| a.0.cmp(&b.0));

    Ok(agents)
}

/// Format timestamp for display
fn format_timestamp(ts: &str) -> String {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(ts) {
        dt.format("%Y-%m-%d %H:%M").to_string()
    } else {
        ts.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_timestamp() {
        assert_eq!(
            format_timestamp("2026-03-19T10:30:00+00:00"),
            "2026-03-19 10:30"
        );
        assert_eq!(format_timestamp("invalid"), "invalid");
    }
}
