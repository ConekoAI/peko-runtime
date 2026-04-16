//! Team Management Commands
//!
//! Provides CLI commands for managing teams - logical groupings of agents.
//! Teams are stored as directories under ~/.pekobot/teams/{team}/agents/
//!
//! NOTE: All business logic is delegated to TeamService in common::services.
//! This module only handles CLI argument parsing and output formatting.

use crate::commands::GlobalPaths;
use crate::common::time::format_timestamp;
use crate::common::types::team::{
    TeamCreationResult, TeamDeletionResult, TeamInfo, TeamMoveResult,
};
use anyhow::Result;
use clap::Subcommand;

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

    /// Remove a team and all its agents
    #[command(alias = "delete")]
    Remove {
        /// Team name
        name: String,
        /// Skip confirmation
        #[arg(short, long)]
        force: bool,
    },

    /// Move/rename a team
    Move {
        /// Current team name
        old_name: String,
        /// New team name
        new_name: String,
        /// Skip confirmation
        #[arg(short, long)]
        force: bool,
    },

    /// Export a team to a .team package
    Export {
        /// Team name to export
        name: String,
        /// Output file path
        #[arg(short, long)]
        output: Option<String>,
        /// Include session history (can be large)
        #[arg(long)]
        include_sessions: bool,
        /// Exclude workspace files
        #[arg(long)]
        exclude_workspace: bool,
        /// Exclude MCP servers
        #[arg(long)]
        exclude_mcp: bool,
    },

    /// Import a team from a .team package
    Import {
        /// Path to .team package file
        file: String,
        /// New team name (optional, uses package name if not specified)
        #[arg(short, long)]
        name: Option<String>,
        /// Skip confirmation if team exists
        #[arg(short, long)]
        force: bool,
        /// Don't rotate keys on import
        #[arg(long)]
        no_rotate_keys: bool,
    },
}

/// Handle team commands
pub async fn handle_team(cmd: TeamCommands, paths: &GlobalPaths, json: bool) -> Result<()> {
    let service = paths.services().team();

    match cmd {
        TeamCommands::Create { name, description } => {
            let result = service.create_team(&name, description.as_deref()).await?;
            render_team_created(&result, json);
            Ok(())
        }
        TeamCommands::List { long } => {
            let teams = service.list_teams().await?;
            render_team_list(&teams, long, json);
            Ok(())
        }
        TeamCommands::Show { name } => {
            let team = service.get_team(&name).await?;
            match team {
                Some(team_info) => {
                    let agents = service.get_team_agents(&name).await?;
                    render_team_show(&team_info, &agents, json);
                    Ok(())
                }
                None => {
                    anyhow::bail!("Team '{}' not found", name);
                }
            }
        }
        TeamCommands::Remove { name, force } => {
            // Get team info for confirmation
            let team_info = match service.get_team(&name).await? {
                Some(info) => info,
                None => {
                    anyhow::bail!("Team '{}' not found", name);
                }
            };

            // Confirm deletion
            if !force {
                if !confirm_team_deletion(&name, team_info.agent_count)? {
                    if json {
                        println!("{{\"success\": false, \"reason\": \"cancelled\"}}");
                    } else {
                        println!("Cancelled.");
                    }
                    return Ok(());
                }
            }

            let result = service.delete_team(&name).await?;
            render_team_deleted(&result, json);
            Ok(())
        }

        TeamCommands::Move {
            old_name,
            new_name,
            force,
        } => {
            // Get team info for confirmation
            let team_info = match service.get_team(&old_name).await? {
                Some(info) => info,
                None => {
                    anyhow::bail!("Team '{}' not found", old_name);
                }
            };

            // Confirm move
            if !force {
                if !confirm_team_move(&old_name, &new_name, team_info.agent_count)? {
                    if json {
                        println!("{{\"success\": false, \"reason\": \"cancelled\"}}");
                    } else {
                        println!("Cancelled.");
                    }
                    return Ok(());
                }
            }

            let result = service.move_team(&old_name, &new_name).await?;
            render_team_moved(&result, json);
            Ok(())
        }

        TeamCommands::Export {
            name,
            output,
            include_sessions,
            exclude_workspace,
            exclude_mcp,
        } => {
            let result = service
                .export_team(
                    &name,
                    output,
                    !include_sessions,
                    exclude_workspace,
                    exclude_mcp,
                )
                .await?;
            render_team_exported(&result, json);
            Ok(())
        }

        TeamCommands::Import {
            file,
            name,
            force,
            no_rotate_keys,
        } => {
            let result = service
                .import_team(&file, name, force, !no_rotate_keys)
                .await?;
            render_team_imported(&result, json);
            Ok(())
        }
    }
}

// ================================================================================
// Rendering Functions (CLI-specific output formatting)
// ================================================================================

fn render_team_created(result: &TeamCreationResult, json: bool) {
    if json {
        println!(
            "{{\"success\": true, \"name\": \"{}\", \"path\": \"{}\"}}",
            result.metadata.name,
            result.path.display()
        );
    } else {
        println!("✅ Created team '{}'", result.metadata.name);
        println!("   Path: {}", result.path.display());
        if let Some(desc) = &result.metadata.description {
            println!("   Description: {}", desc);
        }
        println!();
        println!("   You can now create agents in this team:");
        println!(
            "     pekobot agent create {}/<agent-name>",
            result.metadata.name
        );
    }
}

fn render_team_list(teams: &[TeamInfo], long: bool, json: bool) {
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

        for team in teams {
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
                let desc_marker = if team
                    .metadata
                    .as_ref()
                    .and_then(|m| m.description.as_ref())
                    .is_some()
                {
                    " •"
                } else {
                    ""
                };
                println!(
                    "{} {}{} ({} agents)",
                    icon, team.name, desc_marker, team.agent_count
                );
            }
        }
    }
}

fn render_team_show(
    team: &TeamInfo,
    agents: &[(String, crate::types::agent::AgentConfig)],
    json: bool,
) {
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
                "name": team.name,
                "path": team.path.display().to_string(),
                "description": team.metadata.as_ref().and_then(|m| m.description.clone()),
                "created_at": team.metadata.as_ref().map(|m| m.created_at.clone()),
                "agents": agents_json,
                "agent_count": agents.len(),
            })
        );
    } else {
        println!("📁 Team: {}", team.name);
        if let Some(ref metadata) = team.metadata {
            if let Some(ref desc) = metadata.description {
                println!("   Description: {}", desc);
            }
            println!("   Created: {}", format_timestamp(&metadata.created_at));
        }
        println!("   Path: {}", team.path.display());
        println!();

        if agents.is_empty() {
            println!("   No agents in this team.");
            println!(
                "   Create one with: pekobot agent create {}/<agent-name>",
                team.name
            );
        } else {
            println!("   Agents ({}):", agents.len());
            for (agent_name, config) in agents {
                println!("   📦 {}", agent_name);
                if let Some(ref desc) = config.description {
                    println!("      {}", desc);
                }
                println!("      Provider: {:?}", config.provider.provider_type);
            }
        }
    }
}

fn render_team_deleted(result: &TeamDeletionResult, json: bool) {
    if json {
        println!(
            "{{\"success\": true, \"name\": \"{}\", \"agents_deleted\": {}}}",
            result.name, result.agents_deleted
        );
    } else {
        println!("✅ Deleted team '{}'", result.name);
        if result.agents_deleted > 0 {
            println!("   Removed {} agent(s)", result.agents_deleted);
        }
    }
}

fn render_team_moved(result: &TeamMoveResult, json: bool) {
    if json {
        println!(
            "{{\"success\": true, \"old_name\": \"{}\", \"new_name\": \"{}\", \"agents_moved\": {}}}",
            result.old_name, result.new_name, result.agents_moved
        );
    } else {
        println!(
            "✅ Moved team '{}' to '{}'",
            result.old_name, result.new_name
        );
        if result.agents_moved > 0 {
            println!("   Moved {} agent(s)", result.agents_moved);
        }
        println!("   New path: {}", result.new_path.display());
    }
}

// ================================================================================
// Helper Functions
// ================================================================================

fn render_team_exported(result: &crate::common::types::team::TeamExportResult, json: bool) {
    if json {
        println!(
            "{{\"success\": true, \"name\": \"{}\", \"output_path\": \"{}\", \"agent_count\": {}}}",
            result.name,
            result.output_path.display(),
            result.agent_count
        );
    } else {
        println!("📦 Exported team '{}'", result.name);
        println!("   Agents: {}", result.agent_count);
        println!("   Output: {}", result.output_path.display());
    }
}

fn render_team_imported(result: &crate::common::types::team::TeamImportResult, json: bool) {
    if json {
        println!(
            "{{\"success\": true, \"name\": \"{}\", \"path\": \"{}\", \"agents_imported\": {}}}",
            result.name,
            result.path.display(),
            result.agents_imported
        );
    } else {
        println!("📥 Imported team '{}'", result.name);
        println!("   Agents: {}", result.agents_imported);
        println!("   Path: {}", result.path.display());
    }
}

fn confirm_team_deletion(name: &str, agent_count: usize) -> Result<bool> {
    println!("⚠️  This will permanently delete team '{}'.", name);
    if agent_count > 0 {
        println!(
            "   It contains {} agent(s) that will also be deleted.",
            agent_count
        );
    }
    println!("   This action cannot be undone.");
    print!("   Continue? [y/N] ");
    std::io::Write::flush(&mut std::io::stdout())?;

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;

    Ok(input.trim().eq_ignore_ascii_case("y"))
}

fn confirm_team_move(old_name: &str, new_name: &str, agent_count: usize) -> Result<bool> {
    println!(
        "⚠️  This will rename team '{}' to '{}'.",
        old_name, new_name
    );
    if agent_count > 0 {
        println!(
            "   It contains {} agent(s) that will be moved.",
            agent_count
        );
    }
    print!("   Continue? [y/N] ");
    std::io::Write::flush(&mut std::io::stdout())?;

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;

    Ok(input.trim().eq_ignore_ascii_case("y"))
}
