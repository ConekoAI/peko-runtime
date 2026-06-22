//! Agent Command Handlers
//!
//! NOTE: All business logic is delegated to the daemon via IPC.
//! This module only handles CLI-specific concerns like output formatting and
//! user interaction.

use crate::commands::agent::AgentConfigCommands;
use crate::commands::GlobalPaths;
use crate::common::config_path;
use crate::common::identifiers::parse_agent_identifier_with_override;
use crate::common::services::ConfigAuthority;
use crate::registry::client::ProgressEvent;

/// Handle agent list command
pub async fn handle_agent_list(_paths: &GlobalPaths, long: bool, json: bool) -> anyhow::Result<()> {
    let client = crate::ipc::DaemonClient::connect().await?;
    let packet = crate::ipc::RequestPacket::AgentList {
        request_id: 1,
        team_filter: None,
    };
    let response = client.request_response(packet).await?;

    match response {
        crate::ipc::ResponsePacket::AgentList { agents, .. } => {
            if json {
                let entries: Vec<_> = agents
                    .iter()
                    .map(|agent| {
                        serde_json::json!({
                            "name": agent.name,
                            "preferred_provider": agent.config.preferred_provider_id,
                            "preferred_model": agent.config.preferred_model_id,
                            "description": agent.config.description,
                        })
                    })
                    .collect();

                let output = serde_json::json!({"agents": entries, "total_agents": agents.len()});
                println!("{}", serde_json::to_string_pretty(&output)?);
            } else if agents.is_empty() {
                println!("No agents configured.");
                println!("Create one with: peko agent create <name>");
            } else {
                println!("🐱 Configured Agents ({}):", agents.len());

                for agent in agents {
                    if long {
                        println!("\n  📦 {}", agent.name);
                        println!(
                            "     Preferred provider: {}",
                            agent.config.preferred_provider_id.as_deref().unwrap_or("<none>")
                        );
                        println!(
                            "     Preferred model: {}",
                            agent.config.preferred_model_id.as_deref().unwrap_or("<none>")
                        );
                        if let Some(desc) = &agent.config.description {
                            println!("     Description: {desc}");
                        }
                    } else {
                        println!("  📦 {}", agent.name);
                    }
                }
            }

            Ok(())
        }
        _ => anyhow::bail!("Unexpected response"),
    }
}

/// Handle agent show command
pub async fn handle_agent_show(
    _paths: &GlobalPaths,
    name: String,
    team: Option<String>,
    json: bool,
) -> anyhow::Result<()> {
    let client = crate::ipc::DaemonClient::connect().await?;
    let packet = crate::ipc::RequestPacket::AgentGet {
        request_id: 1,
        name,
        team,
    };
    let response = client.request_response(packet).await?;

    match response {
        crate::ipc::ResponsePacket::AgentGet { agent, .. } => {
            let agent = agent.ok_or_else(|| anyhow::anyhow!("Agent not found"))?;

            if json {
                let output = serde_json::json!({
                    "name": agent.name,
                    "config": agent.config,
                    "session_count": agent.session_count,
                });
                println!("{}", serde_json::to_string_pretty(&output)?);
            } else {
                println!("📦 Agent: {}", agent.name);
                println!("   Config: {}", agent.config_path.display());
                println!(
                    "   Preferred provider: {}",
                    agent.config.preferred_provider_id.as_deref().unwrap_or("<none>")
                );
                println!(
                    "   Preferred model: {}",
                    agent.config.preferred_model_id.as_deref().unwrap_or("<none>")
                );
                println!("   Sessions: {}", agent.session_count);
                if let Some(desc) = &agent.config.description {
                    println!("   Description: {desc}");
                }
            }

            Ok(())
        }
        _ => anyhow::bail!("Unexpected response"),
    }
}

/// Handle agent create command
pub async fn handle_agent_create(
    _paths: &GlobalPaths,
    name: String,
    _team: Option<String>,
    provider: String,
    force: bool,
) -> anyhow::Result<()> {
    let client = crate::ipc::DaemonClient::connect().await?;
    let request =
        crate::common::types::agent::AgentCreateRequest::new(&name, &provider).with_force(force);
    let packet = crate::ipc::RequestPacket::AgentCreate {
        request_id: 1,
        request,
    };
    let response = client.request_response(packet).await?;

    match response {
        crate::ipc::ResponsePacket::AgentCreated { result, .. } => {
            println!("✅ Created agent '{}'", result.name);
            println!("   Provider: {}", result.provider);
            println!("   Config: {}", result.config_path.display());

            Ok(())
        }
        _ => anyhow::bail!("Unexpected response"),
    }
}

/// Handle agent remove command
pub async fn handle_agent_remove(
    _paths: &GlobalPaths,
    name: String,
    team: Option<String>,
    purge: bool,
    force: bool,
    json: bool,
) -> anyhow::Result<()> {
    let client = crate::ipc::DaemonClient::connect().await?;
    let packet = crate::ipc::RequestPacket::AgentDelete {
        request_id: 1,
        name: name.clone(),
        team,
        force,
    };
    let response = client.request_response(packet).await?;

    match response {
        crate::ipc::ResponsePacket::AgentDeleted { result, .. } => {
            if json {
                println!(
                    "{{\"success\": true, \"name\": \"{}\", \"purged\": {}}}",
                    result.name, purge
                );
            } else {
                if purge {
                    println!("🗑️  Purged identity for '{}'", result.name);
                }

                println!("✅ Deleted agent '{}'", result.name);
            }
            Ok(())
        }
        _ => anyhow::bail!("Unexpected response"),
    }
}

/// Handle agent move command
pub async fn handle_agent_move(
    _paths: &GlobalPaths,
    old_name: String,
    new_name: String,
    team: Option<String>,
    _to_team: Option<String>,
    json: bool,
) -> anyhow::Result<()> {
    let client = crate::ipc::DaemonClient::connect().await?;
    let packet = crate::ipc::RequestPacket::AgentMove {
        request_id: 1,
        old_name,
        new_name,
        team,
    };
    let response = client.request_response(packet).await?;

    match response {
        crate::ipc::ResponsePacket::AgentMoved { result, .. } => {
            if json {
                println!(
                    "{{\"success\": true, \"old_name\": \"{}\", \"new_name\": \"{}\"}}",
                    result.old_name, result.new_name
                );
            } else {
                println!(
                    "✅ Renamed agent '{}' to '{}'",
                    result.old_name, result.new_name
                );
                println!("   Config: {}", result.new_config_path.display());
            }
            Ok(())
        }
        _ => anyhow::bail!("Unexpected response"),
    }
}

/// Handle agent export command
pub async fn handle_agent_export(
    _paths: &GlobalPaths,
    name: String,
    team: Option<String>,
    output: Option<String>,
    include_sessions: bool,
    with_extensions: bool,
) -> anyhow::Result<()> {
    let client = crate::ipc::DaemonClient::connect().await?;
    let packet = crate::ipc::RequestPacket::AgentExport {
        request_id: 1,
        name: name.clone(),
        team,
        output,
        include_sessions,
        with_extensions,
    };
    let response = client.request_response(packet).await?;

    match response {
        crate::ipc::ResponsePacket::AgentExported {
            name, output_path, ..
        } => {
            println!("📦 Exported agent '{}' to '{}'", name, output_path);
            Ok(())
        }
        _ => anyhow::bail!("Unexpected response"),
    }
}

/// Handle agent import command
pub async fn handle_agent_import(
    _paths: &GlobalPaths,
    file_path: String,
    name: Option<String>,
    team: Option<String>,
    allow_unsigned_agent: bool,
) -> anyhow::Result<()> {
    let client = crate::ipc::DaemonClient::connect().await?;
    let packet = crate::ipc::RequestPacket::AgentImport {
        request_id: 1,
        file_path: file_path.clone(),
        name,
        team,
        allow_unsigned: allow_unsigned_agent,
    };
    let response = client.request_response(packet).await?;

    match response {
        crate::ipc::ResponsePacket::AgentImported {
            name, config_path, ..
        } => {
            println!("📥 Imported '{}' as '{}'", file_path, name);
            println!("   Config: {}", config_path);
            Ok(())
        }
        _ => anyhow::bail!("Unexpected response"),
    }
}

/// Handle agent inspect command
pub async fn handle_agent_inspect(
    paths: &GlobalPaths,
    file: String,
    json: bool,
) -> anyhow::Result<()> {
    let info = paths.services().agent().inspect_agent_package(&file).await?;

    if json {
        let output = serde_json::json!({
            "name": info.name,
            "version": info.version,
            "description": info.description,
            "did": info.did,
            "created_at": info.created_at,
            "export_format": info.export_format,
            "peko_version": info.peko_version,
            "encrypted": info.encrypted,
            "layers": info.layers,
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

/// Handle agent ownership transfer
pub async fn handle_agent_transfer(
    _paths: &GlobalPaths,
    name: String,
    new_owner: String,
    json: bool,
) -> anyhow::Result<()> {
    let client = crate::ipc::DaemonClient::connect().await?;
    let new_owner_principal =
        crate::auth::principal::principal_from_string_with_default_user(&new_owner);
    let packet = crate::ipc::RequestPacket::AgentTransferOwner {
        request_id: 1,
        agent: name.clone(),
        new_owner: new_owner_principal,
    };
    let response = client.request_response(packet).await?;
    match response {
        crate::ipc::ResponsePacket::Done { success, error, .. } => {
            if success {
                if json {
                    println!("{{\"success\": true, \"agent\": \"{name}\", \"new_owner\": \"{new_owner}\"}}");
                } else {
                    println!("✅ Transferred ownership of '{name}' to {new_owner}");
                }
                Ok(())
            } else {
                anyhow::bail!("Transfer failed: {}", error.unwrap_or_default())
            }
        }
        crate::ipc::ResponsePacket::Error { message, .. } => {
            anyhow::bail!("Transfer failed: {message}")
        }
        _ => anyhow::bail!("Unexpected response"),
    }
}

/// Handle agent permit command
pub async fn handle_agent_permit(
    _paths: &GlobalPaths,
    name: String,
    subject: String,
    permission: String,
    json: bool,
) -> anyhow::Result<()> {
    let client = crate::ipc::DaemonClient::connect().await?;
    let principal = if subject == "public" {
        crate::auth::principal::Principal::Public
    } else {
        crate::auth::principal::Principal::User(subject.clone())
    };
    let permission = parse_permission(&permission)?;
    let packet = crate::ipc::RequestPacket::AgentGrantPermission {
        request_id: 1,
        agent: name.clone(),
        subject: principal,
        permission,
    };
    let response = client.request_response(packet).await?;
    match response {
        crate::ipc::ResponsePacket::Done { success, error, .. } => {
            if success {
                if json {
                    println!(
                        "{{\"success\": true, \"agent\": \"{name}\", \"subject\": \"{subject}\"}}"
                    );
                } else {
                    println!("✅ Granted permission on '{name}' to {subject}");
                }
                Ok(())
            } else {
                anyhow::bail!("Permit failed: {}", error.unwrap_or_default())
            }
        }
        crate::ipc::ResponsePacket::Error { message, .. } => {
            anyhow::bail!("Permit failed: {message}")
        }
        _ => anyhow::bail!("Unexpected response"),
    }
}

/// Handle agent revoke command
pub async fn handle_agent_revoke(
    _paths: &GlobalPaths,
    name: String,
    subject: String,
    permission: String,
    json: bool,
) -> anyhow::Result<()> {
    let client = crate::ipc::DaemonClient::connect().await?;
    let principal = if subject == "public" {
        crate::auth::principal::Principal::Public
    } else {
        crate::auth::principal::Principal::User(subject.clone())
    };
    let permission = parse_permission(&permission)?;
    let packet = crate::ipc::RequestPacket::AgentRevokePermission {
        request_id: 1,
        agent: name.clone(),
        subject: principal,
        permission,
    };
    let response = client.request_response(packet).await?;
    match response {
        crate::ipc::ResponsePacket::Done { success, error, .. } => {
            if success {
                if json {
                    println!(
                        "{{\"success\": true, \"agent\": \"{name}\", \"subject\": \"{subject}\"}}"
                    );
                } else {
                    println!("✅ Revoked permission on '{name}' from {subject}");
                }
                Ok(())
            } else {
                anyhow::bail!("Revoke failed: {}", error.unwrap_or_default())
            }
        }
        crate::ipc::ResponsePacket::Error { message, .. } => {
            anyhow::bail!("Revoke failed: {message}")
        }
        _ => anyhow::bail!("Unexpected response"),
    }
}

/// Handle agent permissions list command
pub async fn handle_agent_permissions(
    _paths: &GlobalPaths,
    name: String,
    json: bool,
) -> anyhow::Result<()> {
    let client = crate::ipc::DaemonClient::connect().await?;
    let packet = crate::ipc::RequestPacket::AgentGet {
        request_id: 1,
        name: name.clone(),
        team: None,
    };
    let response = client.request_response(packet).await?;
    match response {
        crate::ipc::ResponsePacket::AgentGet {
            agent: Some(agent), ..
        } => {
            if json {
                let grants: Vec<_> = agent
                    .config
                    .permissions
                    .iter()
                    .map(|g| {
                        serde_json::json!({
                            "subject": g.subject.to_string(),
                            "permission": g.permission,
                            "granted_at": g.granted_at,
                            "granted_by": g.granted_by.to_string(),
                        })
                    })
                    .collect();
                println!(
                    "{{\"agent\": \"{name}\", \"owner\": \"{}\", \"permissions\": {}}}",
                    agent.config.owner,
                    serde_json::to_string(&grants)?
                );
            } else {
                println!("📦 Agent: {}", name);
                println!("   Owner: {}", agent.config.owner);
                if agent.config.permissions.is_empty() {
                    println!("   Permissions: none");
                } else {
                    println!("   Permissions:");
                    for g in &agent.config.permissions {
                        println!(
                            "     - {} ({}): {:?} (by {} at {})",
                            g.subject,
                            g.subject.kind(),
                            g.permission,
                            g.granted_by,
                            g.granted_at
                        );
                    }
                }
            }
            Ok(())
        }
        crate::ipc::ResponsePacket::AgentGet { agent: None, .. } => {
            anyhow::bail!("Agent '{name}' not found")
        }
        _ => anyhow::bail!("Unexpected response"),
    }
}

fn parse_permission(s: &str) -> anyhow::Result<crate::auth::ownership::Permission> {
    use crate::auth::ownership::Permission;
    match s.to_lowercase().as_str() {
        "chat" => Ok(Permission::Chat),
        "viewsettings" | "view_settings" => Ok(Permission::ViewSettings),
        "managesettings" | "manage_settings" => Ok(Permission::ManageSettings),
        "manageextensions" | "manage_extensions" => Ok(Permission::ManageExtensions),
        "managemembers" | "manage_members" => Ok(Permission::ManageMembers),
        "expose" => Ok(Permission::Expose),
        "delete" => Ok(Permission::Delete),
        _ => anyhow::bail!("Unknown permission: {s}. Valid: Chat, ViewSettings, ManageSettings, ManageExtensions, ManageMembers, Expose, Delete"),
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
        .get(agent_name)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Agent '{agent_name}' not found in team '{team}'"))?;

    config_path::set_config_value(&mut entry.config, &key, &value)?;
    config_service.save(agent_name, &entry.config).await?;

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

/// Handle agent push command
pub async fn handle_agent_push(
    paths: &GlobalPaths,
    local_tag: String,
    registry_ref: String,
    file: Option<String>,
    json: bool,
    cli_registry: Option<&str>,
) -> anyhow::Result<()> {
    let file_ref = file.as_deref();
    let progress_local_tag = local_tag.clone();
    let progress_registry_ref = registry_ref.clone();

    let result = paths
        .services()
        .agent()
        .push_agent(&local_tag, &registry_ref, file_ref, cli_registry, move |event| {
            if json {
                return;
            }
            match event {
                ProgressEvent::Resolving { .. } => {
                    println!("Pushing {} to {}...", progress_local_tag, progress_registry_ref);
                }
                ProgressEvent::Pushing {
                    layer,
                    bytes_sent,
                    bytes_total,
                } => {
                    let short_digest = if layer.len() > 19 {
                        format!("{}...", &layer[..19])
                    } else {
                        layer.clone()
                    };

                    if bytes_sent == bytes_total && bytes_sent != Some(0) {
                        println!("  Layer {}  ✓ uploaded", short_digest);
                    } else if bytes_sent == Some(0) {
                        println!("  Layer {}  → uploading", short_digest);
                    }
                }
                ProgressEvent::Done { .. } => {
                    println!("  Manifest         pushed");
                    println!("Done.");
                }
                ProgressEvent::Error { code, message } => {
                    eprintln!("  Error: {code} - {message}");
                }
                _ => {}
            }
        })
        .await?;

    if json {
        let output = serde_json::json!({
            "success": true,
            "local_tag": result.local_tag,
            "registry_ref": result.registry_ref,
            "manifest": {
                "name": result.name,
                "version": result.version,
                "digest": result.digest,
                "layers": result.layers,
                "total_size": result.total_size,
            }
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    }

    Ok(())
}

/// Handle agent pull command
pub async fn handle_agent_pull(
    paths: &GlobalPaths,
    registry_ref: String,
    output: Option<String>,
    _team: Option<String>,
    force: bool,
    json: bool,
    cli_registry: Option<&str>,
    allow_unsigned_agent: bool,
) -> anyhow::Result<()> {
    let output_ref = output.as_deref();
    let progress_registry_ref = registry_ref.clone();

    let result = paths
        .services()
        .agent()
        .pull_agent(
            &registry_ref,
            None,
            output_ref,
            force,
            allow_unsigned_agent,
            cli_registry,
            move |event| {
                if json {
                    return;
                }
                match event {
                    ProgressEvent::Resolving { .. } => {
                        println!("Pulling {progress_registry_ref}...");
                        println!("  Resolving...");
                    }
                    ProgressEvent::Pulling {
                        layer,
                        bytes_received,
                        bytes_total,
                    } => {
                        let short_digest = if layer.len() > 19 {
                            format!("{}...", &layer[..19])
                        } else {
                            layer.clone()
                        };
                        let size = bytes_total.unwrap_or(0);
                        let size_str = format_size(size);

                        if bytes_received == bytes_total && bytes_received != Some(0) {
                            println!(
                                "  Layer {:<10} {} ({})  ✓ downloaded",
                                "unknown", short_digest, size_str
                            );
                        } else if bytes_received == Some(0) {
                            println!(
                                "  Layer {:<10} {} ({})  → downloading",
                                "unknown", short_digest, size_str
                            );
                        }
                    }
                    ProgressEvent::Verifying { layer } => {
                        println!("  Verifying {layer}...");
                    }
                    ProgressEvent::Extracting { .. } => {}
                    ProgressEvent::Done { .. } => {
                        println!("Done.");
                    }
                    ProgressEvent::Error { code, message } => {
                        eprintln!("  Error: {code} - {message}");
                    }
                    _ => {}
                }
            },
        )
        .await?;

    if let Some(output_path) = result.output_path {
        if !json {
            println!("📦 Saved package to {}", output_path.display());
        }
        return Ok(());
    }

    if json {
        let output_json = serde_json::json!({
            "success": true,
            "registry_ref": registry_ref,
            "name": result.name,
            "config_path": result.config_path,
            "extensions": {
                "pulled": result.extension_results.pulled,
                "already_present": result.extension_results.already_present,
                "failed": result.extension_results.failed.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>(),
            },
            "manifest": {
                "name": result.name,
                "version": result.version,
                "digest": result.manifest_digest,
                "layers": result.manifest_layers,
                "total_size": result.manifest_total_size,
            }
        });
        println!("{}", serde_json::to_string_pretty(&output_json)?);
    } else {
        println!("📥 Imported '{}'", result.name);
        if let Some(config_path) = result.config_path {
            println!("   Config: {}", config_path.display());
        }
        if !result.extension_results.pulled.is_empty() {
            println!("   Extensions pulled: {}", result.extension_results.pulled.len());
            for ext in &result.extension_results.pulled {
                println!("     ✓ {}", ext);
            }
        }
        if !result.extension_results.already_present.is_empty() {
            println!(
                "   Extensions already present: {}",
                result.extension_results.already_present.len()
            );
            for ext in &result.extension_results.already_present {
                println!("     ✓ {}", ext);
            }
        }
        if !result.extension_results.failed.is_empty() {
            eprintln!();
            eprintln!(
                "Warning: {} extension(s) could not be pulled:",
                result.extension_results.failed.len()
            );
            for (ext_id, err) in &result.extension_results.failed {
                eprintln!("  - {}: {}", ext_id, err);
            }
        }
        println!("   Use: peko send {} <message>", result.name);
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Format a byte size for human-readable display
fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}
