//! Team Management Commands
//!
//! Provides CLI commands for managing teams - logical groupings of agents.
//! Teams are stored as directories under ~/.peko/teams/{team}/agents/
//!
//! NOTE: All business logic is delegated to `TeamService` / `TeamManagementService`
//! in `common::services`. This module only handles CLI argument parsing and
//! output formatting.

use crate::commands::GlobalPaths;
use crate::registry::packaging::types::ExtensionRef;
use anyhow::Result;
use clap::Subcommand;

/// Helper: connect to daemon and send a request/response packet
async fn ipc_request(
    packet: crate::ipc::RequestPacket,
) -> anyhow::Result<crate::ipc::ResponsePacket> {
    let client = crate::ipc::DaemonClient::connect().await?;
    client.request_response(packet).await
}

/// Team management subcommands
///
/// Teams are logical groupings for agents. Each team has its own directory
/// structure under ~/.peko/teams/{team}/
///
/// Examples:
///   # Create a new team
///   peko team create dev-team
///
///   # List all teams
///   peko team list
///
///   # Show team details
///   peko team show dev-team
///
///   # Delete a team (and all its agents)
///   peko team delete dev-team --force
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

    /// Push a team snapshot to a registry
    Push {
        /// Team name to push
        name: String,
        /// Registry reference (host/path:tag)
        registry_ref: String,
    },

    /// Pull a team snapshot from a registry
    Pull {
        /// Registry reference (host/path:tag)
        registry_ref: String,
        /// New team name (optional, uses package name if not specified)
        #[arg(short, long)]
        name: Option<String>,
        /// Force overwrite if team already exists
        #[arg(short, long)]
        force: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Skip auto-pull of extensions
        #[arg(long)]
        no_extensions: bool,
    },

    /// Transfer ownership of a team
    Transfer {
        /// Team name
        name: String,
        /// New owner ID (e.g., user:456)
        #[arg(short, long)]
        to: String,
    },

    /// Grant a permission on a team
    Permit {
        /// Team name
        name: String,
        /// Subject to grant permission to
        #[arg(short, long)]
        subject: String,
        /// Permission to grant
        #[arg(short, long)]
        permission: String,
    },

    /// Revoke a permission from a team
    Revoke {
        /// Team name
        name: String,
        /// Subject to revoke permission from
        #[arg(short, long)]
        subject: String,
        /// Permission to revoke
        #[arg(short, long)]
        permission: String,
    },

    /// List permission grants on a team
    Permissions {
        /// Team name
        name: String,
    },
}

/// Handle team commands
pub async fn handle_team(
    cmd: TeamCommands,
    paths: &GlobalPaths,
    json: bool,
    cli_registry: Option<&str>,
) -> Result<()> {
    match cmd {
        TeamCommands::Create { name, description } => {
            let packet = crate::ipc::RequestPacket::TeamCreate {
                request_id: 1,
                name: name.clone(),
                description,
                members: None,
            };
            let response = ipc_request(packet).await?;
            match response {
                crate::ipc::ResponsePacket::TeamCreated { result, .. } => {
                    render::render_team_created(&result, json);
                    Ok(())
                }
                _ => anyhow::bail!("Unexpected response"),
            }
        }
        TeamCommands::List { long } => {
            let packet = crate::ipc::RequestPacket::TeamList { request_id: 1 };
            let response = ipc_request(packet).await?;
            match response {
                crate::ipc::ResponsePacket::TeamList { teams, .. } => {
                    render::render_team_list(&teams, long, json);
                    Ok(())
                }
                _ => anyhow::bail!("Unexpected response"),
            }
        }
        TeamCommands::Show { name } => {
            let packet = crate::ipc::RequestPacket::TeamGet {
                request_id: 1,
                name: name.clone(),
            };
            let response = ipc_request(packet).await?;
            let team_info = match response {
                crate::ipc::ResponsePacket::TeamGet {
                    team: Some(team_info),
                    ..
                } => team_info,
                crate::ipc::ResponsePacket::TeamGet { team: None, .. } => {
                    anyhow::bail!("Team '{name}' not found");
                }
                _ => anyhow::bail!("Unexpected response"),
            };

            let agents_packet = crate::ipc::RequestPacket::AgentList {
                request_id: 2,
                team_filter: Some(name.clone()),
            };
            let agents_response = ipc_request(agents_packet).await?;
            let agents: Vec<(String, crate::agents::agent_config::AgentConfig)> =
                match agents_response {
                    crate::ipc::ResponsePacket::AgentList { agents, .. } => {
                        agents.into_iter().map(|a| (a.name, a.config)).collect()
                    }
                    _ => Vec::new(),
                };

            render::render_team_show(&team_info, &agents, json);
            Ok(())
        }
        TeamCommands::Remove { name, force } => {
            let info_packet = crate::ipc::RequestPacket::TeamGet {
                request_id: 1,
                name: name.clone(),
            };
            let info_response = ipc_request(info_packet).await?;
            let team_info = match info_response {
                crate::ipc::ResponsePacket::TeamGet { team: Some(t), .. } => t,
                _ => anyhow::bail!("Team '{name}' not found"),
            };

            if !force && !render::confirm_team_deletion(&name, team_info.agent_count)? {
                render::render_cancelled(json);
                return Ok(());
            }

            let packet = crate::ipc::RequestPacket::TeamDelete {
                request_id: 1,
                name: name.clone(),
                force,
            };
            let response = ipc_request(packet).await?;
            match response {
                crate::ipc::ResponsePacket::TeamDeleted { result, .. } => {
                    render::render_team_deleted(&result, json);
                    Ok(())
                }
                _ => anyhow::bail!("Unexpected response"),
            }
        }

        TeamCommands::Move {
            old_name,
            new_name,
            force,
        } => {
            let info_packet = crate::ipc::RequestPacket::TeamGet {
                request_id: 1,
                name: old_name.clone(),
            };
            let info_response = ipc_request(info_packet).await?;
            let team_info = match info_response {
                crate::ipc::ResponsePacket::TeamGet { team: Some(t), .. } => t,
                _ => anyhow::bail!("Team '{old_name}' not found"),
            };

            if !force && !render::confirm_team_move(&old_name, &new_name, team_info.agent_count)? {
                render::render_cancelled(json);
                return Ok(());
            }

            let packet = crate::ipc::RequestPacket::TeamMove {
                request_id: 1,
                old_name: old_name.clone(),
                new_name: new_name.clone(),
            };
            let response = ipc_request(packet).await?;
            match response {
                crate::ipc::ResponsePacket::TeamMoved { .. } => {
                    render::render_team_moved(&old_name, &new_name, team_info.agent_count, json);
                    Ok(())
                }
                _ => anyhow::bail!("Unexpected response"),
            }
        }

        TeamCommands::Export {
            name,
            output,
            include_sessions,
            exclude_workspace: _,
            exclude_mcp: _,
        } => {
            let packet = crate::ipc::RequestPacket::TeamExport {
                request_id: 1,
                name: name.clone(),
                output: output.clone(),
                include_sessions,
            };
            let response = ipc_request(packet).await?;
            match response {
                crate::ipc::ResponsePacket::TeamExported {
                    name: resp_name,
                    output_path,
                    ..
                } => {
                    let result = crate::common::types::team::TeamExportResult {
                        name: resp_name,
                        output_path: std::path::PathBuf::from(output_path),
                        agent_count: 0,
                    };
                    render::render_team_exported(&result, json);
                    Ok(())
                }
                _ => anyhow::bail!("Unexpected response"),
            }
        }

        TeamCommands::Import {
            file,
            name,
            force,
            no_rotate_keys: _,
        } => {
            let packet = crate::ipc::RequestPacket::TeamImport {
                request_id: 1,
                file_path: file.clone(),
                name: name.clone(),
                force,
            };
            let response = ipc_request(packet).await?;
            match response {
                crate::ipc::ResponsePacket::TeamImported {
                    name: resp_name,
                    path,
                    ..
                } => {
                    let result = crate::common::types::team::TeamImportResult {
                        name: resp_name,
                        path: std::path::PathBuf::from(path),
                        agents_imported: 0,
                    };
                    render::render_team_imported(&result, json);
                    Ok(())
                }
                _ => anyhow::bail!("Unexpected response"),
            }
        }

        TeamCommands::Push { name, registry_ref } => {
            let service = paths.services().team_management();
            let progress = if json {
                |_event: crate::registry::client::ProgressEvent| {}
            } else {
                |event| render::render_team_push_progress(&event)
            };
            let result = service
                .push_team(&name, &registry_ref, cli_registry, progress)
                .await?;
            render::render_team_pushed(&result, json);
            Ok(())
        }

        TeamCommands::Pull {
            registry_ref,
            name,
            force,
            json: pull_json,
            no_extensions,
        } => {
            let service = paths.services().team_management();
            let progress = if pull_json {
                |_event: crate::registry::client::ProgressEvent| {}
            } else {
                |event| render::render_team_pull_progress(&event)
            };

            if !pull_json {
                println!("Pulling team snapshot {registry_ref}...");
            }

            let pull_result = service
                .pull_team(
                    &registry_ref,
                    name.as_deref(),
                    force,
                    cli_registry,
                    progress,
                )
                .await?;

            let extension_results = if no_extensions {
                crate::common::types::team::TeamExtensionPullResult::default()
            } else {
                ensure_extensions_for_team(
                    paths,
                    &pull_result.extension_refs,
                    &pull_result.name,
                    cli_registry,
                )
                .await
            };

            if !pull_json {
                render::render_team_pull_extension_warnings(&extension_results.failed);
            }
            render::render_team_pull_report(&pull_result, &extension_results, pull_json);
            Ok(())
        }
        TeamCommands::Transfer { name, to } => {
            let packet = crate::ipc::RequestPacket::TeamTransferOwner {
                request_id: 1,
                team: name.clone(),
                new_owner_id: to.clone(),
            };
            let response = ipc_request(packet).await?;
            match response {
                crate::ipc::ResponsePacket::Done { success, error, .. } => {
                    if success {
                        render::render_team_transfer_success(&name, &to, json);
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
        TeamCommands::Permit {
            name,
            subject,
            permission,
        } => {
            let principal = if subject == "public" {
                crate::auth::principal::Principal::Public
            } else {
                crate::auth::principal::Principal::User(subject.clone())
            };
            let permission = parse_team_permission(&permission)?;
            let packet = crate::ipc::RequestPacket::TeamGrantPermission {
                request_id: 1,
                team: name.clone(),
                subject: principal,
                permission,
            };
            let response = ipc_request(packet).await?;
            match response {
                crate::ipc::ResponsePacket::Done { success, error, .. } => {
                    if success {
                        render::render_permission_change(&name, &subject, "granted", json);
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
        TeamCommands::Revoke {
            name,
            subject,
            permission,
        } => {
            let principal = if subject == "public" {
                crate::auth::principal::Principal::Public
            } else {
                crate::auth::principal::Principal::User(subject.clone())
            };
            let permission = parse_team_permission(&permission)?;
            let packet = crate::ipc::RequestPacket::TeamRevokePermission {
                request_id: 1,
                team: name.clone(),
                subject: principal,
                permission,
            };
            let response = ipc_request(packet).await?;
            match response {
                crate::ipc::ResponsePacket::Done { success, error, .. } => {
                    if success {
                        render::render_permission_change(&name, &subject, "revoked", json);
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
        TeamCommands::Permissions { name } => {
            let packet = crate::ipc::RequestPacket::TeamGet {
                request_id: 1,
                name: name.clone(),
            };
            let response = ipc_request(packet).await?;
            match response {
                crate::ipc::ResponsePacket::TeamGet {
                    team: Some(team_info),
                    ..
                } => {
                    render::render_team_permissions(&name, &team_info.metadata, json);
                    Ok(())
                }
                crate::ipc::ResponsePacket::TeamGet { team: None, .. } => {
                    anyhow::bail!("Team '{name}' not found")
                }
                _ => anyhow::bail!("Unexpected response"),
            }
        }
    }
}

fn parse_team_permission(s: &str) -> anyhow::Result<crate::auth::ownership::Permission> {
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

/// Ensure extensions referenced by a pulled team are installed and enabled.
///
/// Failures for individual extensions are collected and returned; the team
/// pull itself is not aborted.
async fn ensure_extensions_for_team(
    paths: &GlobalPaths,
    extensions: &[ExtensionRef],
    team_name: &str,
    cli_registry: Option<&str>,
) -> crate::common::types::team::TeamExtensionPullResult {
    use crate::extensions::builtin::{BuiltinToolAdapter, BuiltinToolRegistrarConfig};
    use crate::extensions::framework::core::global_core;
    use crate::extensions::framework::manager::{ExtensionManager, ExtensionStorage};
    use crate::extensions::framework::types::ExtensionId;
    use crate::extensions::gateway::GatewayAdapter;
    use crate::extensions::general::GeneralExtensionAdapter;
    use crate::extensions::mcp::McpAdapter;
    use crate::extensions::skill::SkillAdapter;
    use crate::extensions::universal::UniversalToolAdapter;

    let mut result = crate::common::types::team::TeamExtensionPullResult::default();

    let core = match global_core() {
        Some(c) => c,
        None => {
            tracing::warn!("Global ExtensionCore not initialized; skipping extension ensure");
            for ext in extensions {
                result
                    .failed
                    .push((ext.id.clone(), "ExtensionCore not initialized".to_string()));
            }
            return result;
        }
    };

    let storage = ExtensionStorage::with_dir(paths.data_dir.join("extensions"));
    let mut manager = ExtensionManager::with_core(core);
    manager = manager.with_storage_dir(storage.dir().unwrap().to_path_buf());

    let _ = BuiltinToolAdapter::register_all(
        &manager.core_arc(),
        &BuiltinToolRegistrarConfig::default(),
    )
    .await;
    manager.register_adapter(Box::new(SkillAdapter::new()));
    manager.register_adapter(Box::new(McpAdapter::with_default_manager()));
    manager.register_adapter(Box::new(UniversalToolAdapter::new()));
    manager.register_adapter(Box::new(GatewayAdapter::new(manager.core_arc())));
    manager.register_adapter(Box::new(GeneralExtensionAdapter::new()));

    if let Err(e) = manager.load_all().await {
        tracing::warn!("Failed to load extensions during team pull: {}", e);
    }

    for ext_ref in extensions {
        let ext_id = ExtensionId::new(&ext_ref.id);

        if manager.get_extension(&ext_id).is_some() {
            result.already_present.push(ext_ref.id.clone());
            continue;
        }

        match crate::commands::ext::handle_ext_pull(
            &mut manager,
            &ext_ref.registry_ref,
            false,
            false,
            cli_registry,
            paths,
        )
        .await
        {
            Ok(()) => {
                result.pulled.push(ext_ref.id.clone());
            }
            Err(e) => {
                tracing::warn!("Failed to pull extension '{}': {}", ext_ref.id, e);
                result.failed.push((ext_ref.id.clone(), e.to_string()));
            }
        }
    }

    // Ensure all extensions are enabled in the team's extensions.toml
    let team_dir = paths.data_dir.join("teams").join(team_name);
    let ext_config_path = team_dir.join("extensions.toml");
    let mut ext_config =
        crate::common::types::team::TeamExtConfig::load(&ext_config_path).unwrap_or_default();

    for ext_ref in extensions {
        let ext_id = ExtensionId::new(&ext_ref.id);
        let enable_name = manager
            .get_extension(&ext_id)
            .map(|e| e.manifest.name.clone())
            .unwrap_or_else(|| ext_ref.id.clone());
        ext_config.enable(&enable_name);
    }

    if let Err(e) = ext_config.save(&ext_config_path) {
        tracing::warn!("Failed to save team extensions.toml: {}", e);
    }

    result
}

mod render;
