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
use crate::common::services::CredentialsService;
use crate::common::types::agent::AgentImportOptions;
use crate::portable::manifest::{AgentLayers, AgentManifest};
use crate::portable::registry::AgentRegistry;
use crate::portable::types::{ImageDigest, Layer, LayerType};
use crate::registry::client::{ProgressEvent, RegistryClient, RegistryRef, ResourceType};
use crate::registry::config::{RegistryConfig, RegistrySource};
use crate::registry::manifest::RegistryManifest;
use std::collections::{HashMap, HashSet};
use std::io::Read;

/// Handle agent list command
pub async fn handle_agent_list(_paths: &GlobalPaths, long: bool, json: bool) -> anyhow::Result<()> {
    let client = crate::ipc::DaemonClient::connect().await?;
    let packet = crate::ipc::RequestPacket::AgentList { request_id: 1, team_filter: None };
    let response = client.request_response(packet).await?;

    match response {
        crate::ipc::ResponsePacket::AgentList { agents, .. } => {
            if json {
                let entries: Vec<_> = agents
                    .iter()
                    .map(|agent| {
                        serde_json::json!({
                            "name": agent.name,
                            "provider": format!("{:?}", agent.config.provider.provider_type),
                            "model": agent.config.provider.default_model,
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
                        println!("     Provider: {:?}", agent.config.provider.provider_type);
                        println!("     Model: {}", agent.config.provider.default_model);
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
    let packet = crate::ipc::RequestPacket::AgentGet { request_id: 1, name, team };
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
                println!("   Provider: {:?}", agent.config.provider.provider_type);
                println!("   Model: {}", agent.config.provider.default_model);
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
    let request = crate::common::types::agent::AgentCreateRequest::new(&name, &provider)
        .with_force(force);
    let packet = crate::ipc::RequestPacket::AgentCreate { request_id: 1, request };
    let response = client.request_response(packet).await?;

    match response {
        crate::ipc::ResponsePacket::AgentCreated { result, .. } => {
            println!(
                "✅ Created agent '{}'",
                result.name
            );
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
    let packet = crate::ipc::RequestPacket::AgentDelete { request_id: 1, name: name.clone(), team, force };
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

                println!(
                    "✅ Deleted agent '{}'",
                    result.name
                );
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
) -> anyhow::Result<()> {
    let client = crate::ipc::DaemonClient::connect().await?;
    let packet = crate::ipc::RequestPacket::AgentExport { request_id: 1, name: name.clone(), team, output, include_sessions };
    let response = client.request_response(packet).await?;

    match response {
        crate::ipc::ResponsePacket::AgentExported { name, output_path, .. } => {
            println!(
                "📦 Exported agent '{}' to '{}'",
                name, output_path
            );
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
) -> anyhow::Result<()> {
    let client = crate::ipc::DaemonClient::connect().await?;
    let packet = crate::ipc::RequestPacket::AgentImport { request_id: 1, file_path: file_path.clone(), name, team };
    let response = client.request_response(packet).await?;

    match response {
        crate::ipc::ResponsePacket::AgentImported { name, config_path, .. } => {
            println!("📥 Imported '{}' as '{}'", file_path, name);
            println!("   Config: {}", config_path);
            Ok(())
        }
        _ => anyhow::bail!("Unexpected response"),
    }
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
    team: Option<String>,
    force: bool,
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

    let manifest = if json && output.is_some() {
        // JSON mode with --output: just pull and store, print download result
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
    } else if json {
        // JSON mode without --output: pull silently, will print single JSON after import
        let manifest = client.pull(&resolved_ref, |_| {}).await?;

        // Store in AgentRegistry format as well
        let agent_manifest = registry_to_agent_manifest(&manifest);
        let tag = format!("{}:{}", manifest.name, reg_ref.tag);
        agent_registry
            .store_manifest(&agent_manifest, Some(&tag))
            .await?;

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
    // and skip local registration (preserves existing "download only" behavior)
    if let Some(output_path) = output {
        let tag = format!("{}:{}", manifest.name, reg_ref.tag);
        let exported = agent_registry.export_package(&tag, &output_path).await?;
        if !json {
            println!("📦 Saved package to {}", exported.display());
        }
        return Ok(());
    }

    // Otherwise: register the pulled agent locally by exporting from registry
    // to a temp .agent file and importing it
    let tag = format!("{}:{}", manifest.name, reg_ref.tag);
    let temp_path = std::env::temp_dir().join(format!(
        "peko-pull-{}-{}.agent",
        manifest.name,
        std::process::id()
    ));

    // Export from registry to temp file
    agent_registry.export_package(&tag, &temp_path).await?;

    // Ensure temp file is cleaned up even if import fails
    let cleanup = scopeguard::guard(temp_path.clone(), |p| {
        let _ = std::fs::remove_file(&p);
    });

    // Import using AgentService (properly registers config/identity/workspace/etc.)
    let service = paths.services().agent();
    let import_opts = AgentImportOptions {
        name: None, // Use manifest name from package
        force,
    };
    let result = service.import_agent(&temp_path, import_opts).await?;

    // Temp file will be cleaned up by scopeguard when `cleanup` drops
    drop(cleanup);

    if json {
        let output_json = serde_json::json!({
            "success": true,
            "registry_ref": resolved_ref,
            "name": result.name,
            "config_path": result.config_path,
            "manifest": {
                "name": manifest.name,
                "version": manifest.version,
                "digest": manifest.digest,
                "layers": manifest.layers.len(),
                "total_size": manifest.total_size_bytes(),
            }
        });
        println!("{}", serde_json::to_string_pretty(&output_json)?);
    } else {
        println!("📥 Imported '{}'", result.name);
        println!("   Config: {}", result.config_path.display());
        println!("   Use: peko send {} <message>", result.name);
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
    .with_ref(reg_ref)
    .with_bundle_type("agent");

    // Plumb discovery metadata from agent manifest
    if let Some(desc) = &agent_manifest.agent.description {
        manifest = manifest.with_description(desc.clone());
    }

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

    // Create a config blob (required by OCI spec) from agent metadata
    let config_json = serde_json::to_string(&serde_json::json!({
        "name": agent_manifest.agent.name,
        "version": agent_manifest.agent.version,
        "kind": "agent",
    }))?;
    let config_digest = ImageDigest::from_bytes(config_json.as_bytes());
    registry.store_layer(config_digest.as_str(), config_json.as_bytes()).await?;
    manifest.config = crate::registry::manifest::ConfigDescriptor {
        media_type: "application/vnd.oci.image.config.v1+json".to_string(),
        digest: config_digest.as_str().to_string(),
        size: config_json.len() as u64,
    };

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
