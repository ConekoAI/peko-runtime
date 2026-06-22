//! Team Management Commands
//!
//! Provides CLI commands for managing teams - logical groupings of agents.
//! Teams are stored as directories under ~/.peko/teams/{team}/agents/
//!
//! NOTE: All business logic is delegated to `TeamService` in `common::services`.
//! This module only handles CLI argument parsing and output formatting.

use crate::commands::GlobalPaths;
use crate::common::services::CredentialsService;
use crate::common::types::team::{TeamCreationResult, TeamDeletionResult, TeamInfo};
use crate::extension::core::global_core;
use crate::extension::manager::{ExtensionManager, ExtensionStorage};
use crate::registry::AgentRegistry;
use crate::portable::team_layer_builder::{build_team_config_layer, decompose_team_archive};
use crate::portable::team_layer_reconstructor::reconstruct_team;
use crate::portable::types::ExtensionRef;
use crate::portable::types::{ImageDigest, Layer, LayerType};
use crate::registry::client::{ProgressEvent, RegistryClient, RegistryRef, ResourceType};
use crate::registry::config::{RegistryConfig, RegistrySource};
use crate::registry::manifest::RegistryManifest;
use anyhow::{Context, Result};
use clap::Subcommand;
use std::collections::HashSet;

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

/// Resolve registry configuration for push/pull operations
fn resolve_registry_config(
    paths: &GlobalPaths,
    cli_registry: Option<&str>,
    host: &str,
) -> anyhow::Result<RegistryConfig> {
    let mut config = paths.registry_config();

    // Apply CLI --registry override
    if let Some(url) = cli_registry {
        config.default = url.to_string();
        if config.get_source(url).is_none() {
            config.add_source(RegistrySource {
                url: url.to_string(),
                priority: 0,
                auth: None,
                token: None,
            });
        }
    }

    // Check for registry token and wire auth into the source
    let creds = CredentialsService::new(paths.clone())?;
    let token = creds.get_registry_token()?.map(|t| t.token);

    if token.is_none() {
        anyhow::bail!(
            "No registry authentication found.\n\
             Run: peko login --api-key <key>"
        );
    }

    config.add_source(RegistrySource {
        url: host.to_string(),
        priority: 1,
        auth: None,
        token,
    });

    Ok(config)
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
            // Get team info
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

            // Get agents in the team
            let agents_packet = crate::ipc::RequestPacket::AgentList {
                request_id: 2,
                team_filter: Some(name.clone()),
            };
            let agents_response = ipc_request(agents_packet).await?;
            let agents: Vec<(String, crate::agents::agent_config::AgentConfig)> = match agents_response {
                crate::ipc::ResponsePacket::AgentList { agents, .. } => {
                    agents.into_iter().map(|a| (a.name, a.config)).collect()
                }
                _ => Vec::new(),
            };

            render::render_team_show(&team_info, &agents, json);
            Ok(())
        }
        TeamCommands::Remove { name, force } => {
            // Get team info for confirmation via IPC
            let info_packet = crate::ipc::RequestPacket::TeamGet {
                request_id: 1,
                name: name.clone(),
            };
            let info_response = ipc_request(info_packet).await?;
            let team_info = match info_response {
                crate::ipc::ResponsePacket::TeamGet { team: Some(t), .. } => t,
                _ => anyhow::bail!("Team '{name}' not found"),
            };

            // Confirm deletion
            if !force && !render::confirm_team_deletion(&name, team_info.agent_count)? {
                if json {
                    println!("{{\"success\": false, \"reason\": \"cancelled\"}}");
                } else {
                    println!("Cancelled.");
                }
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
            // Get team info for confirmation via IPC
            let info_packet = crate::ipc::RequestPacket::TeamGet {
                request_id: 1,
                name: old_name.clone(),
            };
            let info_response = ipc_request(info_packet).await?;
            let team_info = match info_response {
                crate::ipc::ResponsePacket::TeamGet { team: Some(t), .. } => t,
                _ => anyhow::bail!("Team '{old_name}' not found"),
            };

            // Confirm move
            if !force && !confirm_team_move(&old_name, &new_name, team_info.agent_count)? {
                if json {
                    println!("{{\"success\": false, \"reason\": \"cancelled\"}}");
                } else {
                    println!("Cancelled.");
                }
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
                    if json {
                        println!(
                            "{}",
                            serde_json::json!({
                                "success": true,
                                "old_name": old_name,
                                "new_name": new_name,
                                "agents_moved": team_info.agent_count,
                            })
                        );
                    } else {
                        println!("Moved team '{}' -> '{}'", old_name, new_name);
                    }
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
                        agent_count: 0, // Not returned via IPC
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
                        agents_imported: 0, // Not returned via IPC
                    };
                    render::render_team_imported(&result, json);
                    Ok(())
                }
                _ => anyhow::bail!("Unexpected response"),
            }
        }

        TeamCommands::Push { name, registry_ref } => {
            handle_team_push(paths, name, registry_ref, json, cli_registry).await
        }

        TeamCommands::Pull {
            registry_ref,
            name,
            force,
            json: pull_json,
            no_extensions,
        } => {
            handle_team_pull(
                paths,
                registry_ref,
                name,
                force,
                pull_json,
                cli_registry,
                no_extensions,
            )
            .await
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
                        if json {
                            println!("{{\"success\": true, \"team\": \"{name}\", \"new_owner\": \"{to}\"}}");
                        } else {
                            println!("✅ Transferred ownership of team '{name}' to {to}");
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
                        if json {
                            println!("{{\"success\": true, \"team\": \"{name}\", \"subject\": \"{subject}\"}}");
                        } else {
                            println!("✅ Granted permission on team '{name}' to {subject}");
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
                        if json {
                            println!("{{\"success\": true, \"team\": \"{name}\", \"subject\": \"{subject}\"}}");
                        } else {
                            println!("✅ Revoked permission on team '{name}' from {subject}");
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
                    if json {
                        let grants: Vec<_> = team_info
                            .metadata
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
                            "{{\"team\": \"{name}\", \"owner\": \"{}\", \"permissions\": {}}}",
                            team_info.metadata.owner,
                            serde_json::to_string(&grants).unwrap_or_default()
                        );
                    } else {
                        println!("📁 Team: {}", name);
                        println!("   Owner: {}", team_info.metadata.owner);
                        if team_info.metadata.permissions.is_empty() {
                            println!("   Permissions: none");
                        } else {
                            println!("   Permissions:");
                            for g in &team_info.metadata.permissions {
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


// ================================================================================
// Rendering / CLI presentation lives in `team::render`.
// ================================================================================

mod render;

async fn handle_team_push(
    paths: &GlobalPaths,
    name: String,
    registry_ref: String,
    json: bool,
    cli_registry: Option<&str>,
) -> Result<()> {
    let service = paths.services().team();
    // ── 1. Export team to a temp .team file ─────────────────────────────
    let temp_dir = std::env::temp_dir().join("PEKO_team_push");
    std::fs::create_dir_all(&temp_dir)?;
    let temp_path = temp_dir.join(format!("{name}.team"));

    let export_result = service
        .export_team(
            &name,
            Some(temp_path.to_string_lossy().to_string()),
            false,
            false,
            false,
        )
        .await?;

    // ── 2. Extract .team archive in-memory ──────────────────────────────
    let archive_bytes = tokio::fs::read(&export_result.output_path).await?;
    let files = extract_team_archive_bytes(&archive_bytes)?;

    // ── 3. Collect extension references from agent configs ───────────────
    let extension_refs = collect_extension_refs_from_team_files(&files, paths).await?;

    // ── 4. Decompose into content-addressable layers ────────────────────
    let mut decomposed = decompose_team_archive(&files)?;
    decomposed.extensions = extension_refs;

    // Rebuild the TeamConfig layer with extension refs included
    let team_manifest = {
        let manifest_bytes = files
            .get("team/manifest.toml")
            .ok_or_else(|| anyhow::anyhow!("Missing team/manifest.toml in package"))?;
        let manifest_str = std::str::from_utf8(manifest_bytes)?;
        crate::portable::team_packager::TeamManifest::from_toml(manifest_str)?
    };
    let team_toml = files.get("team/team.toml").cloned();
    let rebuilt_team_config = build_team_config_layer(
        &team_manifest,
        &decomposed.agent_index,
        team_toml.as_ref(),
        &decomposed.extensions,
    )?;
    decomposed.team_config_layer = rebuilt_team_config;

    // ── 5. Store layers in AgentRegistry ────────────────────────────────
    let registry = AgentRegistry::new(AgentRegistry::default_path());
    registry.init().await?;

    // TeamConfig layer
    registry
        .store_layer(
            &decomposed.team_config_layer.digest,
            &decomposed.team_config_layer.bytes,
        )
        .await?;

    // Per-agent layers (deduplication happens naturally via digest)
    let mut all_layers: Vec<(String, LayerType, u64)> = Vec::new();
    all_layers.push((
        decomposed.team_config_layer.digest.clone(),
        LayerType::TeamConfig,
        decomposed.team_config_layer.size,
    ));

    for layers in decomposed.agent_layers.values() {
        for (layer_type, layer_bytes) in layers {
            registry
                .store_layer(&layer_bytes.digest, &layer_bytes.bytes)
                .await?;
            all_layers.push((layer_bytes.digest.clone(), *layer_type, layer_bytes.size));
        }
    }

    // ── 6. Build RegistryManifest with kind="team" ──────────────────────
    let mut manifest = RegistryManifest::new(name.to_string(), "1.0.0".to_string())
        .with_kind("team")
        .with_ref(&registry_ref)
        .with_bundle_type("team");

    // Plumb team description from team metadata if available
    if let Ok(Some(team_info)) = service.get_team(&name).await {
        if let Some(desc) = &team_info.metadata.description {
            manifest = manifest.with_description(desc.clone());
        }
    }

    for (digest, layer_type, size) in all_layers {
        manifest.add_layer(Layer::new(digest, layer_type, size));
    }

    // Compute manifest digest
    let manifest_json = manifest.to_json()?;
    let manifest_digest = ImageDigest::from_bytes(manifest_json.as_bytes());
    manifest.digest = manifest_digest.as_str().to_string();

    // Store manifest for RegistryClient
    store_registry_manifest_for_client(&registry, &manifest).await?;

    // ── 7. Push via RegistryClient ──────────────────────────────────────
    let reg_ref = RegistryRef::parse_with_default(
        &registry_ref,
        cli_registry.or(Some(&paths.registry_config().default)),
        Some(ResourceType::Team),
    )?;
    let config = resolve_registry_config(paths, cli_registry, &reg_ref.host)?;

    let client = RegistryClient::new(config, registry);

    let resolved_ref = reg_ref.full_ref();

    if json {
        let result = client.push(&manifest_digest, &resolved_ref, |_| {}).await?;
        let output = serde_json::json!({
            "success": true,
            "team_name": name,
            "registry_ref": registry_ref,
            "manifest": {
                "name": result.name,
                "version": result.version,
                "digest": result.digest,
                "kind": result.kind,
                "layers": result.layers.len(),
                "total_size": result.total_size_bytes(),
            }
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("Pushing team '{name}' to {registry_ref}...");
        let _result = client
            .push(&manifest_digest, &registry_ref, |event| match event {
                ProgressEvent::Resolving { .. } => {}
                ProgressEvent::Pushing {
                    layer,
                    bytes_sent,
                    bytes_total,
                } => {
                    if bytes_sent == bytes_total && bytes_sent != Some(0) {
                        println!("  Layer {}  ✓ uploaded", &layer[..19.min(layer.len())]);
                    } else if bytes_sent == Some(0) {
                        println!("  Layer {}  → uploading", &layer[..19.min(layer.len())]);
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
            })
            .await?;
    }

    // Clean up temp file
    let _ = tokio::fs::remove_file(&export_result.output_path).await;

    Ok(())
}

/// Extract a `.team` tar.gz archive into a map of file paths to bytes.
fn extract_team_archive_bytes(
    data: &[u8],
) -> anyhow::Result<std::collections::HashMap<String, Vec<u8>>> {
    use std::collections::HashMap;
    use std::io::Read;

    let tar = flate2::read::GzDecoder::new(data);
    let mut archive = tar::Archive::new(tar);

    let mut files = HashMap::new();

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_string_lossy().to_string();

        let mut content = Vec::new();
        entry.read_to_end(&mut content)?;

        files.insert(path, content);
    }

    Ok(files)
}

/// Collect extension registry references from agent configs within a .team archive.
///
/// Reads each agent's `config/agent.toml`, extracts `extensions.enabled`, and
/// resolves tool names to installed extensions with known registry refs.
async fn collect_extension_refs_from_team_files(
    files: &std::collections::HashMap<String, Vec<u8>>,
    paths: &GlobalPaths,
) -> anyhow::Result<Vec<ExtensionRef>> {
    use crate::agents::agent_config::AgentConfig;

    let core = match global_core() {
        Some(c) => c,
        None => {
            tracing::warn!("Global ExtensionCore not initialized; skipping extension collection");
            return Ok(Vec::new());
        }
    };

    let storage = ExtensionStorage::with_dir(paths.data_dir.join("extensions"));
    let mut manager = ExtensionManager::with_core(core);
    manager = manager.with_storage_dir(storage.dir().unwrap().to_path_buf());
    // Register adapters (minimal set needed for name resolution)
    use crate::extensions::builtin::{BuiltinToolAdapter, BuiltinToolRegistrarConfig};
    use crate::extensions::gateway::GatewayAdapter;
    use crate::extensions::general::GeneralExtensionAdapter;
    use crate::extensions::mcp::McpAdapter;
    use crate::extensions::skill::SkillAdapter;
    use crate::extensions::universal::UniversalToolAdapter;
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
        tracing::warn!("Failed to load extensions for push: {}", e);
    }

    let mut seen = HashSet::new();
    let mut extension_refs = Vec::new();

    for (path, content) in files {
        // Look for agent config files: agents/{name}/config.toml
        let Some(rest) = path.strip_prefix("agents/") else {
            continue;
        };
        let Some((agent_name, file_path)) = rest.split_once('/') else {
            continue;
        };
        if file_path != "config.toml" {
            continue;
        }

        let config_str = match std::str::from_utf8(content) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let config: AgentConfig = match toml::from_str(config_str) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("Failed to parse agent config for '{}': {}", agent_name, e);
                continue;
            }
        };

        let ext_config = match config.extensions {
            Some(e) => e,
            None => continue,
        };

        for tool_name in &ext_config.enabled {
            // Skip wildcard patterns — can't resolve them to a single extension
            if tool_name.ends_with('*') {
                continue;
            }

            match manager.resolve_tool_name(tool_name) {
                Some(resolution) => {
                    if let Some(registry_ref) = resolution.registry_ref {
                        let key = format!("{}:{}", resolution.id, registry_ref);
                        if seen.insert(key) {
                            extension_refs.push(ExtensionRef {
                                id: resolution.id,
                                registry_ref,
                            });
                        }
                    }
                }
                None => {
                    // Built-in or unknown — silently skip
                }
            }
        }
    }

    Ok(extension_refs)
}

async fn handle_team_pull(
    paths: &GlobalPaths,
    registry_ref: String,
    new_name: Option<String>,
    force: bool,
    json: bool,
    cli_registry: Option<&str>,
    no_extensions: bool,
) -> Result<()> {
    // ── 1. Pull manifest and layers from registry ───────────────────────
    let agent_registry = AgentRegistry::new(AgentRegistry::default_path());
    agent_registry.init().await?;

    let reg_ref = RegistryRef::parse_with_default(
        &registry_ref,
        cli_registry.or(Some(&paths.registry_config().default)),
        Some(ResourceType::Team),
    )?;
    let config = resolve_registry_config(paths, cli_registry, &reg_ref.host)?;

    let client = RegistryClient::new(config, agent_registry.clone());

    let resolved_ref = reg_ref.full_ref();

    let manifest = if json {
        client.pull(&resolved_ref, |_| {}).await?
    } else {
        println!("Pulling team snapshot {resolved_ref}...");
        client
            .pull(&resolved_ref, |event| match event {
                ProgressEvent::Resolving { .. } => {
                    println!("  Resolving...");
                }
                ProgressEvent::Pulling {
                    layer,
                    bytes_received,
                    bytes_total,
                } => {
                    if bytes_received == bytes_total && bytes_received != Some(0) {
                        println!("  Layer {}  ✓ downloaded", &layer[..19.min(layer.len())]);
                    } else if bytes_received == Some(0) {
                        println!("  Layer {}  → downloading", &layer[..19.min(layer.len())]);
                    }
                }
                ProgressEvent::Verifying { layer } => {
                    println!("  Verifying {layer}...");
                }
                ProgressEvent::Done { .. } => {
                    println!("Done.");
                }
                ProgressEvent::Error { code, message } => {
                    eprintln!("  Error: {code} - {message}");
                }
                _ => {}
            })
            .await?
    };

    // Verify this is a team manifest
    if manifest.kind != "team" {
        anyhow::bail!("Expected manifest kind 'team', got '{}'", manifest.kind);
    }

    // ── 2. Find TeamConfig layer ────────────────────────────────────────
    let team_config_layer = manifest
        .layers
        .iter()
        .find(|l| l.layer_type == LayerType::TeamConfig)
        .ok_or_else(|| anyhow::anyhow!("Team manifest missing TeamConfig layer"))?;

    // ── 3. Reconstruct team from layers ─────────────────────────────────
    let reconstructed = reconstruct_team(&agent_registry, &team_config_layer.digest)
        .await
        .context("Failed to reconstruct team from layers")?;

    let team_name = new_name
        .clone()
        .unwrap_or_else(|| reconstructed.team_info.team.name.clone());

    // ── 4. Create team directory and write team.toml ────────────────────
    let config_dir = crate::common::paths::default_config_dir();
    let team_dir = config_dir.join("teams").join(&team_name);

    if team_dir.exists() && !force {
        anyhow::bail!("Team '{team_name}' already exists. Use --force to overwrite.");
    }

    if team_dir.exists() && force {
        // Remove existing team directory to allow clean re-import
        tokio::fs::remove_dir_all(&team_dir).await?;
    }

    tokio::fs::create_dir_all(&team_dir).await?;

    if let Some(team_toml) = reconstructed.team_toml {
        tokio::fs::write(team_dir.join("team.toml"), team_toml).await?;
    }

    // ── 5. Import each agent directly from reconstructed files ──────────
    let mut imported_agents = Vec::new();

    for (agent_name, files) in &reconstructed.agent_files {
        let result = import_agent_from_files(agent_name, files, &team_name, &team_dir)
            .await
            .with_context(|| format!("Failed to import agent: {agent_name}"))?;

        imported_agents.push(result);
    }

    let agent_count = imported_agents.len();

    // ── 6. Auto-pull and enable extensions ──────────────────────────────
    let mut extension_results = ExtensionPullResult::default();
    if !no_extensions && !reconstructed.team_info.extensions.is_empty() {
        extension_results = ensure_extensions_for_team(
            paths,
            &reconstructed.team_info.extensions,
            &team_name,
            cli_registry,
        )
        .await;
        // Log warnings for any failures, but don't fail the whole pull
        if !extension_results.failed.is_empty() && !json {
            eprintln!();
            eprintln!(
                "Warning: {} extension(s) could not be pulled:",
                extension_results.failed.len()
            );
            for (ext_id, err) in &extension_results.failed {
                eprintln!("  - {}: {}", ext_id, err);
            }
        }
    }

    // ── 7. Output results ───────────────────────────────────────────────
    if json {
        let output = serde_json::json!({
            "success": true,
            "registry_ref": registry_ref,
            "name": team_name,
            "path": team_dir.display().to_string(),
            "agents_imported": agent_count,
            "extensions": {
                "pulled": extension_results.pulled,
                "already_present": extension_results.already_present,
                "failed": extension_results.failed.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>(),
            },
            "manifest": {
                "name": manifest.name,
                "version": manifest.version,
                "digest": manifest.digest,
                "kind": manifest.kind,
                "layers": manifest.layers.len(),
                "total_size": manifest.total_size_bytes(),
            }
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("📥 Imported team '{team_name}'");
        println!("   Agents: {agent_count}");
        println!("   Path: {}", team_dir.display());
        if !extension_results.pulled.is_empty() {
            println!("   Extensions pulled: {}", extension_results.pulled.len());
            for ext in &extension_results.pulled {
                println!("     ✓ {}", ext);
            }
        }
        if !extension_results.already_present.is_empty() {
            println!(
                "   Extensions already present: {}",
                extension_results.already_present.len()
            );
            for ext in &extension_results.already_present {
                println!("     ✓ {}", ext);
            }
        }
    }

    Ok(())
}

/// Result of ensuring extensions for a pulled team.
#[derive(Debug, Default)]
struct ExtensionPullResult {
    pulled: Vec<String>,
    already_present: Vec<String>,
    failed: Vec<(String, String)>,
}

/// Ensure all extensions referenced by a team are installed and enabled.
///
/// For each `ExtensionRef`:
/// - If already installed, ensure it's enabled in the team's extensions.toml
/// - If not installed, call `handle_ext_pull` (which handles dependencies)
///
/// Failures for individual extensions are collected and returned; the team
/// pull itself is not aborted.
async fn ensure_extensions_for_team(
    paths: &GlobalPaths,
    extensions: &[ExtensionRef],
    team_name: &str,
    cli_registry: Option<&str>,
) -> ExtensionPullResult {
    use crate::extension::types::ExtensionId;

    let mut result = ExtensionPullResult::default();

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
    // Register adapters
    use crate::extensions::builtin::{BuiltinToolAdapter, BuiltinToolRegistrarConfig};
    use crate::extensions::gateway::GatewayAdapter;
    use crate::extensions::general::GeneralExtensionAdapter;
    use crate::extensions::mcp::McpAdapter;
    use crate::extensions::skill::SkillAdapter;
    use crate::extensions::universal::UniversalToolAdapter;
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

        // Check if already installed
        if manager.get_extension(&ext_id).is_some() {
            result.already_present.push(ext_ref.id.clone());
        } else {
            // Need to pull
            match crate::commands::ext::handle_ext_pull(
                &mut manager,
                &ext_ref.registry_ref,
                false, // json
                false, // no_deps — let dependency resolution work
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
    }

    // Ensure all extensions are enabled in the team's extensions.toml
    let team_dir = paths.data_dir.join("teams").join(team_name);
    let ext_config_path = team_dir.join("extensions.toml");
    let mut ext_config =
        crate::common::types::TeamExtConfig::load(&ext_config_path).unwrap_or_default();

    for ext_ref in extensions {
        // Enable by extension name (not ID) to match agent whitelist conventions
        // Try to get the extension's display name from the manager
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

/// Import a single agent from in-memory reconstructed files.
async fn import_agent_from_files(
    name: &str,
    files: &std::collections::HashMap<String, Vec<u8>>,
    team_name: &str,
    team_dir: &std::path::Path,
) -> anyhow::Result<crate::portable::team_unpackager::AgentImportSummary> {
    use crate::portable::unpackager::{ImportOptions, Unpackager};

    let unpackager = Unpackager::new("dummy.agent")
        .with_base_dir(team_dir)
        .with_team(team_name);

    // Reconstruct a minimal manifest.toml if missing (layers don't include it).
    let mut files = files.clone();
    if !files.contains_key("manifest.toml") {
        let manifest = build_minimal_manifest(name, &files)?;
        let manifest_toml = manifest.to_toml()?;
        files.insert("manifest.toml".to_string(), manifest_toml.into_bytes());
    }

    let agent_opts = ImportOptions {
        new_name: Some(name.to_string()),
        passphrase: None,
        rotate_keys: true,
        import_sessions: true,
        import_workspace: true,
        skip_validation: false,
        force: true,
        team: Some(team_name.to_string()),
        // Reconstructed agents from a team import do not surface the
        // unsigned opt-in; default to false (secure by default).
        allow_unsigned: false,
    };

    let result = unpackager.import_from_files(files, agent_opts).await?;

    Ok(crate::portable::team_unpackager::AgentImportSummary {
        name: result.name,
        did: result.did,
        keys_rotated: result.keys_rotated,
    })
}

/// Build a minimal AgentManifest from reconstructed files for import validation.
fn build_minimal_manifest(
    name: &str,
    files: &std::collections::HashMap<String, Vec<u8>>,
) -> anyhow::Result<crate::portable::manifest::AgentManifest> {
    use crate::portable::manifest::AgentManifest;

    let did = if let Some(did_bytes) = files.get("identity/did.json") {
        let did_doc: crate::identity::DIDDocument = serde_json::from_slice(did_bytes)?;
        did_doc.id
    } else {
        format!("did:peko:local:{name}")
    };

    let mut manifest = AgentManifest::new(name, "1.0.0", &did);

    // Add all present files to the manifest so validation passes
    for (path, content) in files {
        manifest.add_file(path, content);
    }

    Ok(manifest)
}

/// Store a `RegistryManifest` in the format expected by `RegistryClient`
async fn store_registry_manifest_for_client(
    registry: &AgentRegistry,
    manifest: &RegistryManifest,
) -> anyhow::Result<ImageDigest> {
    let digest = ImageDigest::new(&manifest.digest)?;
    let image_dir = registry
        .root_path()
        .join("registry_manifests")
        .join(digest.dir_name());
    tokio::fs::create_dir_all(&image_dir).await?;
    let manifest_path = image_dir.join("manifest.json");
    let json = manifest.to_json()?;
    tokio::fs::write(&manifest_path, json).await?;
    Ok(digest)
}

fn confirm_team_move(old_name: &str, new_name: &str, agent_count: usize) -> Result<bool> {
    println!("⚠️  This will rename team '{old_name}' to '{new_name}'.");
    if agent_count > 0 {
        println!("   It contains {agent_count} agent(s) that will be moved.");
    }
    print!("   Continue? [y/N] ");
    std::io::Write::flush(&mut std::io::stdout())?;

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;

    Ok(input.trim().eq_ignore_ascii_case("y"))
}
