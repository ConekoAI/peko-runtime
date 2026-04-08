//! Agent Command Handlers
//!
//! NOTE: All business logic is delegated to AgentService in common::services.
//! This module only handles CLI-specific concerns like output formatting and
//! user interaction.

use crate::commands::agent::AgentConfigCommands;
use crate::commands::GlobalPaths;
use crate::common::config_path;
use crate::common::identifiers::parse_agent_identifier_with_override;
use crate::common::types::agent::{
    AgentCreateRequest, AgentDeleteOptions, AgentExportOptions, AgentImportOptions,
    AgentInitRequest,
};

/// Handle agent list command
pub async fn handle_agent_list(paths: &GlobalPaths, long: bool, json: bool) -> anyhow::Result<()> {
    let service = paths.services().agent();
    let agents = service.list_agents(None).await?;

    if json {
        // Build JSON output with team structure
        let mut teams: std::collections::HashMap<String, Vec<_>> = std::collections::HashMap::new();
        let mut total_agents = 0;

        for agent in &agents {
            let entry = serde_json::json!({
                "name": agent.name,
                "provider": format!("{:?}", agent.config.provider.provider_type),
                "model": agent.config.provider.default_model,
                "description": agent.config.description,
            });
            teams.entry(agent.team.clone()).or_default().push(entry);
            total_agents += 1;
        }

        let teams_json: Vec<_> = teams
            .into_iter()
            .map(|(name, agents)| {
                serde_json::json!({
                    "name": name,
                    "agents": agents
                })
            })
            .collect();

        let output = serde_json::json!({"teams": teams_json, "total_agents": total_agents});
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else if agents.is_empty() {
        println!("No agents configured.");
        println!("Create one with: pekobot agent create <name>");
    } else {
        println!("🐱 Configured Agents ({}):", agents.len());

        // Group by team
        let mut teams: std::collections::HashMap<String, Vec<_>> = std::collections::HashMap::new();
        for agent in agents {
            teams.entry(agent.team.clone()).or_default().push(agent);
        }

        // Sort teams: default first, then alphabetically
        let mut team_names: Vec<_> = teams.keys().cloned().collect();
        team_names.sort_by(|a, b| {
            if a == "default" {
                return std::cmp::Ordering::Less;
            }
            if b == "default" {
                return std::cmp::Ordering::Greater;
            }
            a.cmp(b)
        });

        for team_name in team_names {
            let team_agents = teams.get(&team_name).unwrap();
            println!("\n  📁 Team: {}", team_name);

            for agent in team_agents {
                if long {
                    println!("\n    📦 {}", agent.name);
                    println!("       Provider: {:?}", agent.config.provider.provider_type);
                    println!("       Model: {}", agent.config.provider.default_model);
                    if let Some(desc) = &agent.config.description {
                        println!("       Description: {}", desc);
                    }
                } else {
                    println!("    📦 {}", agent.name);
                }
            }
        }
    }

    Ok(())
}

/// Handle agent show command
pub async fn handle_agent_show(
    paths: &GlobalPaths,
    name: String,
    team: Option<String>,
    json: bool,
) -> anyhow::Result<()> {
    let service = paths.services().agent();
    let (team, agent_name) = parse_agent_identifier_with_override(&name, team.as_deref())?;

    let agent = service
        .get_agent(agent_name, Some(team))
        .await?
        .ok_or_else(|| anyhow::anyhow!("Agent '{}' not found in team '{}'", agent_name, team))?;

    if json {
        let output = serde_json::json!({
            "name": agent.name,
            "team": agent.team,
            "config": agent.config,
            "session_count": agent.session_count,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("📦 Agent: {}", agent.name);
        println!("   Team: {}", agent.team);
        println!("   Config: {}", agent.config_path.display());
        println!("   Provider: {:?}", agent.config.provider.provider_type);
        println!("   Model: {}", agent.config.provider.default_model);
        println!("   Sessions: {}", agent.session_count);
        if let Some(desc) = &agent.config.description {
            println!("   Description: {}", desc);
        }
    }

    Ok(())
}

/// Handle agent create command
pub async fn handle_agent_create(
    paths: &GlobalPaths,
    name: String,
    team: Option<String>,
    _template: String,
    provider: String,
    force: bool,
) -> anyhow::Result<()> {
    let service = paths.services().agent();

    let (team, agent_name) = parse_agent_identifier_with_override(&name, team.as_deref())?;

    let request = AgentCreateRequest::new(agent_name, &provider)
        .with_team(team)
        .with_force(force);

    let result = service.create_agent(request).await?;

    println!(
        "✅ Created agent '{}' in team '{}'",
        result.name, result.team
    );
    println!("   Provider: {}", result.provider);
    println!("   Config: {}", result.config_path.display());

    Ok(())
}

/// Handle agent remove command
pub async fn handle_agent_remove(
    paths: &GlobalPaths,
    name: String,
    team: Option<String>,
    purge: bool,
    _force: bool,
    json: bool,
) -> anyhow::Result<()> {
    let service = paths.services().agent();

    let (team, agent_name) = parse_agent_identifier_with_override(&name, team.as_deref())?;

    let opts = AgentDeleteOptions {
        purge_identity: purge,
        force: _force,
    };

    let result = service.delete_agent(agent_name, Some(team), opts).await?;

    if json {
        println!(
            "{{\"success\": true, \"name\": \"{}\", \"team\": \"{}\", \"purged\": {}}}",
            result.name, result.team, purge
        );
    } else {
        if purge {
            println!("🗑️  Purged identity for '{}'", result.name);
        }

        println!(
            "✅ Deleted agent '{}' from team '{}'",
            result.name, result.team
        );
    }
    Ok(())
}

/// Handle agent move command
pub async fn handle_agent_move(
    paths: &GlobalPaths,
    old_name: String,
    new_name: String,
    team: Option<String>,
    to_team: Option<String>,
    json: bool,
) -> anyhow::Result<()> {
    let service = paths.services().agent();

    let result = service
        .rename_agent(&old_name, &new_name, team.as_deref(), to_team.as_deref())
        .await?;

    if json {
        println!(
            "{{\"success\": true, \"old_name\": \"{}\", \"new_name\": \"{}\", \"team\": \"{}\"}}",
            result.old_name, result.new_name, result.to_team
        );
    } else {
        if result.from_team == result.to_team {
            println!(
                "✅ Renamed agent '{}' to '{}' in team '{}'",
                result.old_name, result.new_name, result.from_team
            );
        } else {
            println!(
                "✅ Moved agent '{}' from team '{}' to '{}' as '{}'",
                result.old_name, result.from_team, result.to_team, result.new_name
            );
        }
        println!("   Config: {}", result.new_config_path.display());
    }

    Ok(())
}

/// Handle agent export command
pub async fn handle_agent_export(
    paths: &GlobalPaths,
    name: String,
    team: Option<String>,
    output: Option<String>,
) -> anyhow::Result<()> {
    let service = paths.services().agent();

    let opts = AgentExportOptions {
        output_path: output.map(|p| p.into()),
    };

    let result = service.export_agent(&name, team.as_deref(), opts).await?;

    println!(
        "📦 Exported agent '{}' from team '{}' to '{}'",
        result.name,
        result.team,
        result.output_path.display()
    );

    Ok(())
}

/// Handle agent import command
pub async fn handle_agent_import(
    paths: &GlobalPaths,
    file_path: String,
    name: Option<String>,
    team: Option<String>,
) -> anyhow::Result<()> {
    let service = paths.services().agent();

    let opts = AgentImportOptions {
        name,
        team,
    };

    let result = service.import_agent(file_path.as_ref(), opts).await?;

    println!("📥 Imported '{}' as '{}'", file_path, result.name);
    println!("   Team: {}", result.team);
    println!("   Config: {}", result.config_path.display());

    Ok(())
}

/// Handle agent inspect command
pub async fn handle_agent_inspect(file: String, json: bool) -> anyhow::Result<()> {
    use crate::portable::get_package_info;

    if !std::path::Path::new(&file).exists() {
        eprintln!("❌ File not found: {}", file);
        return Ok(());
    }

    let info = get_package_info(&file).await?;

    if json {
        let output = serde_json::json!({
            "name": info.name,
            "version": info.version,
            "description": info.description,
            "did": info.did,
            "created_at": info.created_at,
            "export_format": info.export_format,
            "pekobot_version": info.pekobot_version,
            "encrypted": info.encrypted,
            "capabilities": info.capabilities,
            "required_tools": info.required_tools,
            "valid": info.valid,
            "warnings": info.warnings,
            "errors": info.errors,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("{}", info.format());
    }

    Ok(())
}

/// Handle agent init command
pub async fn handle_agent_init(
    paths: &GlobalPaths,
    path: String,
    name: Option<String>,
    provider: String,
    model: Option<String>,
    force: bool,
    json: bool,
) -> anyhow::Result<()> {
    let service = paths.services().agent();

    let request = AgentInitRequest::new(&path)
        .with_provider(&provider)
        .with_force(force);

    let request = if let Some(name) = name {
        request.with_name(name)
    } else {
        request
    };

    let request = if let Some(model) = model {
        request.with_model(model)
    } else {
        request
    };

    let result = service.init_agent(request).await?;

    if json {
        println!(
            "{{\"success\": true, \"name\": \"{}\", \"path\": \"{}\", \"config\": \"{}\"}}",
            result.name,
            result.path.display(),
            result.config_path.display()
        );
    } else {
        println!("✅ Initialized agent '{}' in '{}'", result.name, path);
        println!();
        println!("📁 Structure created:");
        println!("   config.toml    - Agent configuration");
        println!("   AGENT.md       - Agent description");
        println!("   .gitignore     - Excludes sessions/, workspace/, memories/");
        println!("   tools/         - Custom tools directory");
        println!("   skills/        - Skills directory");
        println!("   workspace/     - Working directory");
        println!();
        println!("🚀 Run the agent:");
        println!("   pekobot send {}/{} \"Hello\"", "default", result.name);
    }

    Ok(())
}

/// Dispatch agent config subcommands
pub async fn handle_agent_config(
    cmd: AgentConfigCommands,
    paths: &GlobalPaths,
    json: bool,
) -> anyhow::Result<()> {
    match cmd {
        AgentConfigCommands::Get { name, key, team } => {
            handle_agent_config_get(paths, name, team, key, json).await
        }
        AgentConfigCommands::Set { name, key, value, team } => {
            handle_agent_config_set(paths, name, team, key, value, json).await
        }
    }
}

async fn handle_agent_config_get(
    paths: &GlobalPaths,
    name: String,
    team: Option<String>,
    key: String,
    json: bool,
) -> anyhow::Result<()> {
    let service = paths.services().agent();
    let (team, agent_name) = parse_agent_identifier_with_override(&name, team.as_deref())?;

    let agent = service
        .get_agent(agent_name, Some(team))
        .await?
        .ok_or_else(|| anyhow::anyhow!("Agent '{}' not found in team '{}'", agent_name, team))?;

    let value = config_path::get_config_value(&agent.config, &key)?;

    if json {
        println!(
            "{}",
            serde_json::json!({
                "agent": agent_name,
                "team": team,
                "key": key,
                "value": value
            })
        );
    } else {
        println!("{}", config_path::format_value(&value)?);
    }

    Ok(())
}

async fn handle_agent_config_set(
    paths: &GlobalPaths,
    name: String,
    team: Option<String>,
    key: String,
    value: String,
    json: bool,
) -> anyhow::Result<()> {
    let config_service = paths.services().agent_config();
    let (team, agent_name) = parse_agent_identifier_with_override(&name, team.as_deref())?;

    let mut entry = config_service
        .get(agent_name, Some(team))
        .await?
        .ok_or_else(|| anyhow::anyhow!("Agent '{}' not found in team '{}'", agent_name, team))?;

    config_path::set_config_value(&mut entry.config, &key, &value)?;
    config_service.save(agent_name, team, &entry.config).await?;

    if json {
        println!(
            "{}",
            serde_json::json!({
                "success": true,
                "agent": agent_name,
                "team": team,
                "key": key
            })
        );
    } else {
        println!("✅ Set '{}' for agent '{}/{}'", key, team, agent_name);
    }

    Ok(())
}
