//! Agent Command Handlers
//!
//! NOTE: All business logic is delegated to `AgentService` in `common::services`.
//! This module only handles CLI-specific concerns like output formatting and
//! user interaction.

use crate::commands::agent::AgentConfigCommands;
use crate::commands::GlobalPaths;
use crate::common::config_path;
use crate::common::identifiers::parse_agent_identifier_with_override;
use crate::common::services::ConfigAuthority;
use crate::common::services::CredentialsService;
use crate::common::types::agent::{
    AgentCreateRequest, AgentDeleteOptions, AgentExportOptions, AgentImportOptions,
};
use crate::portable::manifest::{AgentLayers, AgentManifest};
use crate::portable::registry::AgentRegistry;
use crate::portable::types::{ImageDigest, Layer, LayerType};
use crate::registry::client::{ProgressEvent, RegistryClient, RegistryRef, ResourceType};
use crate::registry::config::{RegistryConfig, RegistrySource};
use crate::registry::manifest::RegistryManifest;
use std::collections::{HashMap, HashSet};
use std::io::Read;

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
        println!("Create one with: peko agent create <name>");
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
            println!("\n  📁 Team: {team_name}");

            for agent in team_agents {
                if long {
                    println!("\n    📦 {}", agent.name);
                    println!("       Provider: {:?}", agent.config.provider.provider_type);
                    println!("       Model: {}", agent.config.provider.default_model);
                    if let Some(desc) = &agent.config.description {
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
        .ok_or_else(|| anyhow::anyhow!("Agent '{agent_name}' not found in team '{team}'"))?;

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
            println!("   Description: {desc}");
        }
    }

    Ok(())
}

/// Handle agent create command
pub async fn handle_agent_create(
    paths: &GlobalPaths,
    name: String,
    team: Option<String>,
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
    force: bool,
    json: bool,
) -> anyhow::Result<()> {
    let service = paths.services().agent();

    let (team, agent_name) = parse_agent_identifier_with_override(&name, team.as_deref())?;

    let opts = AgentDeleteOptions {
        purge_identity: purge,
        force,
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
    include_sessions: bool,
) -> anyhow::Result<()> {
    let service = paths.services().agent();

    let opts = AgentExportOptions {
        output_path: output.map(std::convert::Into::into),
        include_sessions,
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
        anyhow::bail!("File not found: {file}");
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

/// Resolve registry configuration for push/pull operations
///
/// Loads config from `~/.peko/config.toml`, applies CLI `--registry` override,
/// and wires in the authentication token for the target host.
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
    let creds = CredentialsService::new(paths.clone());
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

/// Handle agent push command
pub async fn handle_agent_push(
    paths: &GlobalPaths,
    local_tag: String,
    registry_ref: String,
    file: Option<String>,
    json: bool,
    cli_registry: Option<&str>,
) -> anyhow::Result<()> {
    let registry = AgentRegistry::new(AgentRegistry::default_path());
    registry.init().await?;

    let agent_manifest = if let Some(ref file_path) = file {
        // Load .agent file and store layers in local registry
        load_agent_file_into_registry(file_path, &registry).await?
    } else {
        registry
            .get_manifest_by_tag(&local_tag)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to load local tag '{local_tag}': {e}"))?
    };

    let reg_ref = RegistryRef::parse_with_default(
        &registry_ref,
        cli_registry.or(Some(&paths.registry_config().default)),
        Some(ResourceType::Agent),
    )?;
    let config = resolve_registry_config(paths, cli_registry, &reg_ref.host)?;

    let registry_manifest =
        agent_to_registry_manifest(&agent_manifest, &registry, &registry_ref).await?;
    let digest = ImageDigest::new(&registry_manifest.digest)?;

    let client = RegistryClient::new(config, registry.clone());

    // Ensure RegistryClient can find the manifest
    store_registry_manifest_for_client(&registry, &registry_manifest).await?;

    let push_tag = if file.is_some() {
        "<file>".to_string()
    } else {
        local_tag.clone()
    };
    let push_tag_ref: &str = &push_tag;

    let resolved_ref = reg_ref.full_ref();

    if json {
        let manifest = client.push(&digest, &resolved_ref, |_| {}).await?;
        let output = serde_json::json!({
            "success": true,
            "local_tag": push_tag,
            "registry_ref": resolved_ref,
            "manifest": {
                "name": manifest.name,
                "version": manifest.version,
                "digest": manifest.digest,
                "layers": manifest.layers.len(),
                "total_size": manifest.total_size_bytes(),
            }
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        let layer_info: HashMap<String, (LayerType, u64)> = registry_manifest
            .layers
            .iter()
            .map(|l| (l.digest.clone(), (l.layer_type, l.size_bytes)))
            .collect();
        let mut seen_layers = HashSet::new();

        let _manifest = client
            .push(&digest, &resolved_ref, |event| match event {
                ProgressEvent::Resolving { .. } => {
                    println!("Pushing {push_tag_ref} to {resolved_ref}...");
                }
                ProgressEvent::Pushing {
                    layer,
                    bytes_sent,
                    bytes_total,
                } => {
                    if let Some((layer_type, size)) = layer_info.get(&layer) {
                        let short_digest = if layer.len() > 19 {
                            format!("{}...", &layer[..19])
                        } else {
                            layer.clone()
                        };
                        let size_str = format_size(*size);
                        let type_name = layer_type.dir_name();

                        if bytes_sent == bytes_total && bytes_sent != Some(0) {
                            if seen_layers.contains(&layer) {
                                println!(
                                    "  Layer {:<10} {} ({})  ✓ uploaded",
                                    type_name, short_digest, size_str
                                );
                            } else {
                                println!(
                                    "  Layer {:<10} {} ({})  ✓ exists",
                                    type_name, short_digest, size_str
                                );
                            }
                        } else if bytes_sent == Some(0) {
                            seen_layers.insert(layer.clone());
                            println!(
                                "  Layer {:<10} {} ({})  → uploading",
                                type_name, short_digest, size_str
                            );
                        }
                    }
                }
                ProgressEvent::Done { .. } => {
                    println!("  Manifest         pushed");
                    println!("Done.");
                }
                ProgressEvent::Error { code, message } => {
                    eprintln!("  Error: {code} - {message}");
                }
                ProgressEvent::Pulling { .. }
                | ProgressEvent::Extracting { .. }
                | ProgressEvent::Verifying { .. } => {}
            })
            .await?;
    }

    Ok(())
}

/// Load a `.agent` file into the local registry, storing layers and returning the manifest.
async fn load_agent_file_into_registry(
    file_path: &str,
    registry: &AgentRegistry,
) -> anyhow::Result<AgentManifest> {
    let file = std::fs::File::open(file_path)?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);

    let mut files = std::collections::HashMap::new();
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?;
        let path_str = path.to_string_lossy().to_string();

        let mut content = Vec::new();
        entry.read_to_end(&mut content)?;
        files.insert(path_str, content);
    }

    let manifest_bytes = files
        .get("manifest.toml")
        .ok_or_else(|| anyhow::anyhow!("Missing manifest.toml in package"))?;
    let manifest_str = std::str::from_utf8(manifest_bytes)?;
    let manifest = AgentManifest::from_toml(manifest_str)?;

    // Store each layer in the registry
    if let Some(ref layers) = manifest.layers {
        let layer_types = [
            (layers.config.as_ref(), LayerType::Config),
            (layers.identity.as_ref(), LayerType::Identity),
            (layers.skills.as_ref(), LayerType::Skills),
            (layers.workspace.as_ref(), LayerType::Workspace),
            (layers.sessions.as_ref(), LayerType::Sessions),
            (layers.mcp.as_ref(), LayerType::Mcp),
        ];

        for (digest_opt, layer_type) in layer_types {
            if let Some(digest) = digest_opt {
                let prefix = layer_type.dir_name();
                // Collect all files for this layer
                let mut layer_files: std::collections::BTreeMap<String, Vec<u8>> =
                    std::collections::BTreeMap::new();
                for (path, content) in &files {
                    if path.starts_with(&format!("{prefix}/")) {
                        let layer_path = path.strip_prefix(&format!("{prefix}/")).unwrap_or(path);
                        layer_files.insert(layer_path.to_string(), content.clone());
                    }
                }

                if !layer_files.is_empty() {
                    // Build tarball
                    let mut buf = Vec::new();
                    {
                        let enc =
                            flate2::write::GzEncoder::new(&mut buf, flate2::Compression::default());
                        let mut tar = tar::Builder::new(enc);
                        for (path, content) in layer_files {
                            let mut header = tar::Header::new_gnu();
                            header.set_path(&path)?;
                            header.set_size(content.len() as u64);
                            header.set_mode(0o644);
                            header.set_cksum();
                            tar.append(&header, content.as_slice())?;
                        }
                        tar.finish()?;
                    }
                    registry.store_layer(digest, &buf).await?;
                }
            }
        }
    }

    Ok(manifest)
}

/// Handle agent pull command
pub async fn handle_agent_pull(
    paths: &GlobalPaths,
    registry_ref: String,
    output: Option<String>,
    json: bool,
    cli_registry: Option<&str>,
) -> anyhow::Result<()> {
    let agent_registry = AgentRegistry::new(AgentRegistry::default_path());
    agent_registry.init().await?;

    let reg_ref = RegistryRef::parse_with_default(
        &registry_ref,
        cli_registry.or(Some(&paths.registry_config().default)),
        Some(ResourceType::Agent),
    )?;
    let config = resolve_registry_config(paths, cli_registry, &reg_ref.host)?;

    let client = RegistryClient::new(config, agent_registry.clone());

    let resolved_ref = reg_ref.full_ref();

    let manifest = if json {
        let manifest = client.pull(&resolved_ref, |_| {}).await?;

        // Store in AgentRegistry format as well
        let agent_manifest = registry_to_agent_manifest(&manifest);
        let tag = format!("{}:{}", manifest.name, reg_ref.tag);
        agent_registry
            .store_manifest(&agent_manifest, Some(&tag))
            .await?;

        let output_json = serde_json::json!({
            "success": true,
            "registry_ref": resolved_ref,
            "manifest": {
                "name": manifest.name,
                "version": manifest.version,
                "digest": manifest.digest,
                "layers": manifest.layers.len(),
                "total_size": manifest.total_size_bytes(),
            }
        });
        println!("{}", serde_json::to_string_pretty(&output_json)?);
        manifest
    } else {
        let mut seen_layers = HashSet::new();

        let manifest = client
            .pull(&registry_ref, |event| match event {
                ProgressEvent::Resolving { .. } => {
                    println!("Pulling {registry_ref}...");
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
                        if seen_layers.contains(&layer) {
                            println!(
                                "  Layer {:<10} {} ({})  ✓ downloaded",
                                "unknown", short_digest, size_str
                            );
                        } else {
                            println!(
                                "  Layer {:<10} {} ({})  ✓ cached",
                                "unknown", short_digest, size_str
                            );
                        }
                    } else if bytes_received == Some(0) {
                        seen_layers.insert(layer.clone());
                        println!(
                            "  Layer {:<10} {} ({})  → downloading",
                            "unknown", short_digest, size_str
                        );
                    }
                }
                ProgressEvent::Verifying { layer } => {
                    println!("  Verifying {layer}...");
                }
                ProgressEvent::Extracting { layer } => {
                    let _ = layer;
                }
                ProgressEvent::Done {
                    manifest: done_manifest,
                } => {
                    println!("Done. Stored as {}:{}", done_manifest.name, reg_ref.tag);
                }
                ProgressEvent::Error { code, message } => {
                    eprintln!("  Error: {code} - {message}");
                }
                _ => {}
            })
            .await?;

        // Store in AgentRegistry format as well
        let agent_manifest = registry_to_agent_manifest(&manifest);
        let tag = format!("{}:{}", manifest.name, reg_ref.tag);
        agent_registry
            .store_manifest(&agent_manifest, Some(&tag))
            .await?;

        manifest
    };

    // If --output was specified, export the pulled package to a .agent file
    if let Some(output_path) = output {
        let tag = format!("{}:{}", manifest.name, reg_ref.tag);
        let exported = agent_registry.export_package(&tag, &output_path).await?;
        if !json {
            println!("📦 Saved package to {}", exported.display());
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert an `AgentManifest` to a `RegistryManifest`
async fn agent_to_registry_manifest(
    agent_manifest: &AgentManifest,
    registry: &AgentRegistry,
    reg_ref: &str,
) -> anyhow::Result<RegistryManifest> {
    let mut manifest = RegistryManifest::new(
        agent_manifest.agent.name.clone(),
        agent_manifest.agent.version.clone(),
    )
    .with_ref(reg_ref);

    if let Some(layers) = &agent_manifest.layers {
        if let Some(digest) = &layers.config {
            let size = layer_size(registry, digest).await?;
            manifest.add_layer(Layer::new(digest.clone(), LayerType::Config, size));
        }
        if let Some(digest) = &layers.identity {
            let size = layer_size(registry, digest).await?;
            manifest.add_layer(Layer::new(digest.clone(), LayerType::Identity, size));
        }
        if let Some(digest) = &layers.skills {
            let size = layer_size(registry, digest).await?;
            manifest.add_layer(Layer::new(digest.clone(), LayerType::Skills, size));
        }
        if let Some(digest) = &layers.workspace {
            let size = layer_size(registry, digest).await?;
            manifest.add_layer(Layer::new(digest.clone(), LayerType::Workspace, size));
        }
        if let Some(digest) = &layers.sessions {
            let size = layer_size(registry, digest).await?;
            manifest.add_layer(Layer::new(digest.clone(), LayerType::Sessions, size));
        }
        if let Some(digest) = &layers.mcp {
            let size = layer_size(registry, digest).await?;
            manifest.add_layer(Layer::new(digest.clone(), LayerType::Mcp, size));
        }
    }

    // Compute digest from JSON representation
    let json = manifest.to_json()?;
    let digest = ImageDigest::from_bytes(json.as_bytes());
    manifest.digest = digest.as_str().to_string();

    Ok(manifest)
}

/// Convert a `RegistryManifest` to an `AgentManifest`
fn registry_to_agent_manifest(registry_manifest: &RegistryManifest) -> AgentManifest {
    let mut agent_manifest = AgentManifest::new(
        &registry_manifest.name,
        &registry_manifest.version,
        "did:peko:pulled",
    );

    let mut layers = AgentLayers::default();
    for layer in &registry_manifest.layers {
        match layer.layer_type {
            LayerType::Config => layers.config = Some(layer.digest.clone()),
            LayerType::Identity => layers.identity = Some(layer.digest.clone()),
            LayerType::Skills => layers.skills = Some(layer.digest.clone()),
            LayerType::Workspace => layers.workspace = Some(layer.digest.clone()),
            LayerType::Sessions => layers.sessions = Some(layer.digest.clone()),
            LayerType::Mcp => layers.mcp = Some(layer.digest.clone()),
            LayerType::TeamConfig => {
                // TeamConfig layers are not part of agent manifests;
                // they are skipped when reconstructing an agent manifest.
            }
        }
    }
    agent_manifest.layers = Some(layers);
    agent_manifest
}

/// Get the size of a layer from the `AgentRegistry`
async fn layer_size(registry: &AgentRegistry, digest: &str) -> anyhow::Result<u64> {
    let path = registry.layer_path(digest);
    let metadata = tokio::fs::metadata(&path).await?;
    Ok(metadata.len())
}

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
