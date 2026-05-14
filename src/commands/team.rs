//! Team Management Commands
//!
//! Provides CLI commands for managing teams - logical groupings of agents.
//! Teams are stored as directories under ~/.pekobot/teams/{team}/agents/
//!
//! NOTE: All business logic is delegated to `TeamService` in `common::services`.
//! This module only handles CLI argument parsing and output formatting.

use crate::commands::GlobalPaths;
use crate::common::time::format_timestamp;
use crate::common::types::team::{
    TeamCreationResult, TeamDeletionResult, TeamInfo, TeamMoveResult,
};
use crate::portable::registry::AgentRegistry;
use crate::portable::team_layer_builder::decompose_team_archive;
use crate::portable::team_layer_reconstructor::reconstruct_team;
use crate::portable::types::{ImageDigest, Layer, LayerType};
use crate::registry::client::{ProgressEvent, RegistryClient, RegistryRef};
use crate::registry::config::{RegistryConfig, RegistrySource};
use crate::registry::manifest::RegistryManifest;
use anyhow::{Context, Result};
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
                    anyhow::bail!("Team '{name}' not found");
                }
            }
        }
        TeamCommands::Remove { name, force } => {
            // Get team info for confirmation
            let team_info = match service.get_team(&name).await? {
                Some(info) => info,
                None => {
                    anyhow::bail!("Team '{name}' not found");
                }
            };

            // Confirm deletion
            if !force && !confirm_team_deletion(&name, team_info.agent_count)? {
                if json {
                    println!("{{\"success\": false, \"reason\": \"cancelled\"}}");
                } else {
                    println!("Cancelled.");
                }
                return Ok(());
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
                    anyhow::bail!("Team '{old_name}' not found");
                }
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

        TeamCommands::Push {
            name,
            registry_ref,
        } => handle_team_push(service, &name, &registry_ref, json).await,

        TeamCommands::Pull {
            registry_ref,
            name,
            force,
            json: pull_json,
        } => handle_team_pull(service, &registry_ref, name, force, pull_json).await,
    }
}

// ================================================================================
// Rendering Functions (CLI-specific output formatting)
// ================================================================================

fn render_team_created(result: &TeamCreationResult, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::json!({
                "success": true,
                "name": result.metadata.name,
                "path": result.path.display().to_string(),
            })
        );
    } else {
        println!("✅ Created team '{}'", result.metadata.name);
        println!("   Path: {}", result.path.display());
        if let Some(desc) = &result.metadata.description {
            println!("   Description: {desc}");
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
                        println!("   Description: {desc}");
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
                println!("   Description: {desc}");
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
                println!("   📦 {agent_name}");
                if let Some(ref desc) = config.description {
                    println!("      {desc}");
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
            "{}",
            serde_json::json!({
                "success": true,
                "name": result.name,
                "output_path": result.output_path.display().to_string(),
                "agent_count": result.agent_count,
            })
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
            "{}",
            serde_json::json!({
                "success": true,
                "name": result.name,
                "path": result.path.display().to_string(),
                "agents_imported": result.agents_imported,
            })
        );
    } else {
        println!("📥 Imported team '{}'", result.name);
        println!("   Agents: {}", result.agents_imported);
        println!("   Path: {}", result.path.display());
    }
}

fn confirm_team_deletion(name: &str, agent_count: usize) -> Result<bool> {
    println!("⚠️  This will permanently delete team '{name}'.");
    if agent_count > 0 {
        println!("   It contains {agent_count} agent(s) that will also be deleted.");
    }
    println!("   This action cannot be undone.");
    print!("   Continue? [y/N] ");
    std::io::Write::flush(&mut std::io::stdout())?;

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;

    Ok(input.trim().eq_ignore_ascii_case("y"))
}

async fn handle_team_push(
    service: &crate::common::services::team_service::TeamService,
    name: &str,
    registry_ref: &str,
    json: bool,
) -> Result<()> {
    // ── 1. Export team to a temp .team file ─────────────────────────────
    let temp_dir = std::env::temp_dir().join("pekobot_team_push");
    std::fs::create_dir_all(&temp_dir)?;
    let temp_path = temp_dir.join(format!("{name}.team"));

    let export_result = service
        .export_team(name, Some(temp_path.to_string_lossy().to_string()), false, false, false)
        .await?;

    // ── 2. Extract .team archive in-memory ──────────────────────────────
    let archive_bytes = tokio::fs::read(&export_result.output_path).await?;
    let files = extract_team_archive_bytes(&archive_bytes)?;

    // ── 3. Decompose into content-addressable layers ────────────────────
    let decomposed = decompose_team_archive(&files)?;

    // ── 4. Store layers in AgentRegistry ────────────────────────────────
    let registry = AgentRegistry::new(AgentRegistry::default_path());
    registry.init().await?;

    // TeamConfig layer
    registry
        .store_layer(&decomposed.team_config_layer.digest, &decomposed.team_config_layer.bytes)
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

    // ── 5. Build RegistryManifest with kind="team" ──────────────────────
    let mut manifest = RegistryManifest::new(name.to_string(), "1.0.0".to_string())
        .with_kind("team")
        .with_ref(registry_ref);

    for (digest, layer_type, size) in all_layers {
        manifest.add_layer(Layer::new(digest, layer_type, size));
    }

    // Compute manifest digest
    let manifest_json = manifest.to_json()?;
    let manifest_digest = ImageDigest::from_bytes(manifest_json.as_bytes());
    manifest.digest = manifest_digest.as_str().to_string();

    // Store manifest for RegistryClient
    store_registry_manifest_for_client(&registry, &manifest).await?;

    // ── 6. Push via RegistryClient ──────────────────────────────────────
    let reg_ref = RegistryRef::parse(registry_ref)?;
    let mut config = RegistryConfig::default();
    config.add_source(RegistrySource {
        url: reg_ref.host.clone(),
        priority: 1,
        auth: None,
    });

    let client = RegistryClient::new(config, registry);

    if json {
        let result = client.push(&manifest_digest, registry_ref, |_| {}).await?;
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
            .push(&manifest_digest, registry_ref, |event| match event {
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
fn extract_team_archive_bytes(data: &[u8]) -> anyhow::Result<std::collections::HashMap<String, Vec<u8>>> {
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

async fn handle_team_pull(
    _service: &crate::common::services::team_service::TeamService,
    registry_ref: &str,
    new_name: Option<String>,
    force: bool,
    json: bool,
) -> Result<()> {
    // ── 1. Pull manifest and layers from registry ───────────────────────
    let agent_registry = AgentRegistry::new(AgentRegistry::default_path());
    agent_registry.init().await?;

    let reg_ref = RegistryRef::parse(registry_ref)?;
    let mut config = RegistryConfig::default();
    config.add_source(RegistrySource {
        url: reg_ref.host.clone(),
        priority: 1,
        auth: None,
    });

    let client = RegistryClient::new(config, agent_registry.clone());

    let manifest = if json {
        client.pull(registry_ref, |_| {}).await?
    } else {
        println!("Pulling team snapshot {registry_ref}...");
        client
            .pull(registry_ref, |event| match event {
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
        anyhow::bail!(
            "Expected manifest kind 'team', got '{}'",
            manifest.kind
        );
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
        let result = import_agent_from_files(
            agent_name,
            files,
            &team_name,
            &team_dir,
        )
        .await
        .with_context(|| format!("Failed to import agent: {agent_name}"))?;

        imported_agents.push(result);
    }

    let agent_count = imported_agents.len();

    // ── 6. Output results ───────────────────────────────────────────────
    if json {
        let output = serde_json::json!({
            "success": true,
            "registry_ref": registry_ref,
            "name": team_name,
            "path": team_dir.display().to_string(),
            "agents_imported": agent_count,
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
    }

    Ok(())
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
    };

    let result = unpackager
        .import_from_files(files, agent_opts)
        .await?;

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
        format!("did:pekobot:local:{name}")
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
