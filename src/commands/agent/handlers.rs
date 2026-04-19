//! Agent Command Handlers
//!
//! NOTE: Query/mutation commands (list, show, create, remove, move) use the
//! HTTP API via `ApiClient`. File-system commands (init, export, import,
//! inspect, config) remain local.

use crate::commands::agent::AgentConfigCommands;
use crate::commands::GlobalPaths;
use crate::common::config_path;
use crate::common::identifiers::parse_agent_identifier_with_override;
use crate::common::services::ConfigAuthority;
use crate::common::types::agent::{
    AgentCreateRequest, AgentDeleteOptions, AgentExportOptions, AgentImportOptions,
    AgentInitRequest,
};

/// Helper: create an API client and verify daemon is running
fn api_client() -> anyhow::Result<crate::api::client::ApiClient> {
    crate::api::client::ApiClient::new()
}

/// Helper: verify daemon is running via health check
async fn ensure_daemon_running(client: &crate::api::client::ApiClient) -> anyhow::Result<()> {
    client.health_check().await.map_err(|_| {
        anyhow::anyhow!(
            "Daemon not running. Start it with: pekobot daemon start --foreground"
        )
    })?;
    Ok(())
}

/// Helper: get provider type string from agent response
fn agent_provider_type(agent: &crate::api::client::AgentConfigResponse) -> String {
    agent
        .config
        .as_ref()
        .map(|c| format!("{:?}", c.provider.provider_type))
        .unwrap_or_else(|| "unknown".to_string())
}

/// Helper: get model from agent response
fn agent_model(agent: &crate::api::client::AgentConfigResponse) -> String {
    agent
        .config
        .as_ref()
        .map(|c| c.provider.default_model.clone())
        .unwrap_or_default()
}

/// Helper: get description from agent response
fn agent_description(agent: &crate::api::client::AgentConfigResponse) -> Option<String> {
    agent.config.as_ref().and_then(|c| c.description.clone())
}

/// Handle agent list command
pub async fn handle_agent_list(_paths: &GlobalPaths, long: bool, json: bool) -> anyhow::Result<()> {
    let client = api_client()?;
    ensure_daemon_running(&client).await?;

    let response = client.list_agents(None).await?;
    let agents = response.items;

    if json {
        // Build JSON output with team structure
        let mut teams: std::collections::HashMap<String, Vec<_>> = std::collections::HashMap::new();
        let mut total_agents = 0;

        for agent in &agents {
            let entry = serde_json::json!({
                "name": agent.name,
                "provider": agent_provider_type(agent),
                "model": agent_model(agent),
                "description": agent_description(agent),
            });
            let team = agent.team_id.clone().unwrap_or_else(|| "default".to_string());
            teams.entry(team).or_default().push(entry);
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
            let team = agent.team_id.clone().unwrap_or_else(|| "default".to_string());
            teams.entry(team).or_default().push(agent);
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
            println!("\n  📁 Team: {team_name}");

            for agent in team_agents {
                if long {
                    println!("\n    📦 {}", agent.name);
                    println!("       Provider: {}", agent_provider_type(agent));
                    println!("       Model: {}", agent_model(agent));
                    if let Some(desc) = agent_description(agent) {
                        println!("       Description: {desc}");
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
    _paths: &GlobalPaths,
    name: String,
    team: Option<String>,
    json: bool,
) -> anyhow::Result<()> {
    let client = api_client()?;
    ensure_daemon_running(&client).await?;

    let (team, agent_name) = parse_agent_identifier_with_override(&name, team.as_deref())?;

    // The API uses agent name as the path parameter; team is not part of the path
    // We list all agents and filter by name + team
    let response = client.list_agents(None).await?;
    let agent = response
        .items
        .into_iter()
        .find(|a| {
            a.name == agent_name && a.team_id.as_deref().unwrap_or("default") == team
        })
        .ok_or_else(|| anyhow::anyhow!("Agent '{agent_name}' not found in team '{team}'"))?;

    if json {
        let output = serde_json::json!({
            "name": agent.name,
            "team": agent.team_id.clone(),
            "config": agent.config,
            "session_count": agent.session_count,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("📦 Agent: {}", agent.name);
        println!("   Team: {}", agent.team_id.clone().unwrap_or_else(|| "default".to_string()));
        if let Some(path) = &agent.config_path {
            println!("   Config: {path}");
        }
        println!("   Provider: {}", agent_provider_type(&agent));
        println!("   Model: {}", agent_model(&agent));
        println!("   Sessions: {}", agent.session_count.unwrap_or(0));
        if let Some(desc) = agent_description(&agent) {
            println!("   Description: {desc}");
        }
    }

    Ok(())
}

/// Handle agent create command
pub async fn handle_agent_create(
    _paths: &GlobalPaths,
    name: String,
    team: Option<String>,
    provider: String,
    force: bool,
) -> anyhow::Result<()> {
    let client = api_client()?;
    ensure_daemon_running(&client).await?;

    let (team, agent_name) = parse_agent_identifier_with_override(&name, team.as_deref())?;

    let result = client
        .register_agent(&provider, Some(&agent_name), Some(&team), None)
        .await?;

    println!(
        "✅ Created agent '{}' in team '{}'",
        result.name,
        result.team_id.unwrap_or_else(|| "default".to_string())
    );
    println!("   Provider: {}", result.image_ref);

    Ok(())
}

/// Handle agent remove command
pub async fn handle_agent_remove(
    _paths: &GlobalPaths,
    name: String,
    team: Option<String>,
    purge: bool,
    _force: bool,
    json: bool,
) -> anyhow::Result<()> {
    let client = api_client()?;
    ensure_daemon_running(&client).await?;

    let (team, agent_name) = parse_agent_identifier_with_override(&name, team.as_deref())?;

    // The HTTP API doesn't support purge via unregister; we just delete the agent config
    client.unregister_agent(&agent_name).await?;

    if json {
        println!(
            "{{\"success\": true, \"name\": \"{}\", \"team\": \"{}\", \"purged\": {}}}",
            agent_name, team, purge
        );
    } else {
        if purge {
            println!("🗑️  Purged identity for '{agent_name}'");
        }
        println!("✅ Deleted agent '{agent_name}' from team '{team}'");
    }
    Ok(())
}

/// Handle agent move command
///
/// NOTE: This command performs filesystem operations (moving config files)
/// and remains local. There is no HTTP equivalent.
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
        output_path: output.map(std::convert::Into::into),
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

    let opts = AgentImportOptions { name, team };

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
        eprintln!("❌ File not found: {file}");
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
        println!("   pekobot send default/{} \"Hello\"", result.name);
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
        AgentConfigCommands::Set {
            name,
            key,
            value,
            team,
        } => handle_agent_config_set(paths, name, team, key, value, json).await,
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
        .ok_or_else(|| anyhow::anyhow!("Agent '{agent_name}' not found in team '{team}'"))?;

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
        .ok_or_else(|| anyhow::anyhow!("Agent '{agent_name}' not found in team '{team}'"))?;

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
        println!("✅ Set '{key}' for agent '{team}/{agent_name}'");
    }

    Ok(())
}
