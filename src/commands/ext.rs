//! Extension management commands
//!
//! Thin CLI dispatcher — all business logic lives in domain modules:
//! - `extension::services::ExtensionConfigService` — config persistence
//! - `extensions::validation::ExtensionValidationService` — manifest validation
//! - `ipc::client_service::DaemonClientService` — daemon IPC
//! - `common::services::ConfigAuthorityImpl` — agent whitelist management

use crate::commands::capability;
use crate::commands::mcp;
use crate::commands::GlobalPaths;
use crate::extensions::framework::scaffold::{ScaffoldEngine, ScaffoldLang, ScaffoldOptions};
use crate::extensions::framework::services::{ConfigScope, ExtensionConfigService};
use crate::ipc::client_service::DaemonClientService;
use crate::registry::client::ProgressEvent;
use clap::Subcommand;
use std::path::PathBuf;

/// Extension management subcommands
#[derive(Subcommand)]
#[command(disable_version_flag = true)]
pub enum ExtCommands {
    /// Install an extension
    Install {
        /// Path to the extension directory or manifest
        path: PathBuf,
        /// Extension type (auto-detect if not specified)
        #[arg(long)]
        r#type: Option<String>,
    },

    /// List installed extensions
    List {
        /// Show only enabled extensions
        #[arg(long)]
        enabled_only: bool,
        /// Filter by extension type
        #[arg(long)]
        r#type: Option<String>,
        /// Show access for a specific agent (format: team/agent or just agent)
        #[arg(long, value_name = "AGENT")]
        agent: Option<String>,
        /// Show access for all agents in a specific team
        #[arg(long, value_name = "TEAM", conflicts_with = "agent")]
        team: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Enable an extension or built-in tool
    Enable {
        /// Extension ID or built-in tool name (e.g., Bash, Read)
        id: String,
        /// Target team or team/agent for built-in tools
        #[arg(short, long, value_name = "TARGET", conflicts_with = "principal")]
        target: Option<String>,
        /// Target principal for principal-scoped extensions
        #[arg(long, value_name = "NAME", conflicts_with = "target")]
        principal: Option<String>,
    },

    /// Disable an extension or built-in tool
    Disable {
        /// Extension ID or built-in tool name
        id: String,
        /// Target team or team/agent for built-in tools
        #[arg(short, long, value_name = "TARGET", conflicts_with = "principal")]
        target: Option<String>,
        /// Target principal for principal-scoped extensions
        #[arg(long, value_name = "NAME", conflicts_with = "target")]
        principal: Option<String>,
    },

    /// Uninstall an extension
    Uninstall { id: String },

    /// Show extension details
    Info { id: String },

    /// Export an installed extension to a .ext package
    Export {
        /// Extension ID to export
        id: String,
        /// Output path
        #[arg(short, long)]
        output: String,
    },

    /// Create a bundle from installed extensions
    Bundle {
        /// Bundle name
        #[arg(short, long)]
        name: String,
        /// Extension IDs to include
        ids: Vec<String>,
    },

    /// Configure extension settings (global, team, or agent level)
    Config {
        /// Extension ID
        id: String,
        /// Show current configuration
        #[arg(long, conflicts_with_all = &["set", "unset"])]
        show: bool,
        /// Set a configuration value (key=value)
        #[arg(long, value_name = "KEY=VALUE")]
        set: Vec<String>,
        /// Unset a configuration key
        #[arg(long, value_name = "KEY")]
        unset: Vec<String>,
        /// Apply to global scope (default)
        #[arg(long, group = "scope")]
        global: bool,
        /// Apply to team scope
        #[arg(long, value_name = "TEAM", group = "scope")]
        team: Option<String>,
        /// Apply to agent scope (format: team/agent or just agent for default team)
        #[arg(long, value_name = "AGENT", group = "scope")]
        agent: Option<String>,
    },

    /// Validate an extension manifest
    Validate {
        /// Path to the extension directory or manifest
        path: PathBuf,
        /// Show detailed validation output
        #[arg(long)]
        verbose: bool,
        /// Enable semantic validation (check referenced files, commands, schemas)
        #[arg(long)]
        semantic: bool,
    },

    /// Debug an installed extension
    Debug { id: String },

    /// Start a background runtime for an extension (daemon-scoped)
    Start { id: String },

    /// Stop a background runtime for an extension (daemon-scoped)
    Stop { id: String },

    /// Restart a background runtime for an extension (daemon-scoped)
    Restart { id: String },

    /// Show background runtime status for an extension
    Status { id: String },

    /// MCP server management commands
    #[command(subcommand)]
    Mcp(mcp::McpCommands),

    /// Push an installed extension to a registry
    Push {
        /// Extension ID to push
        id: String,
        /// Registry reference (host/path:tag)
        registry_ref: String,
        /// Bundle transitive dependencies into the package
        #[arg(long)]
        with_deps: bool,
    },

    /// Pull an extension from a registry
    Pull {
        /// Registry reference (host/path:tag)
        registry_ref: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Skip pulling dependencies
        #[arg(long)]
        no_deps: bool,
    },

    /// Initialize a new extension project
    Init {
        /// Project name / extension ID
        name: String,
        /// Extension type
        #[arg(short, long)]
        r#type: String,
        /// Output directory (default: ./<name>)
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Programming language for stub code (python, javascript)
        #[arg(short, long)]
        lang: Option<String>,
        /// For MCP: ship server.json instead of manifest.yaml wrapper
        #[arg(long)]
        bare: bool,
        /// For gateway: the gateway type
        #[arg(long)]
        gateway_type: Option<String>,
    },
}

/// Handle extension subcommands
pub async fn handle_ext_command(
    command: ExtCommands,
    paths: &GlobalPaths,
    json: bool,
    cli_registry: Option<&str>,
) -> anyhow::Result<()> {
    match command {
        // --- IPC commands (thin client) ---
        ExtCommands::List {
            enabled_only,
            r#type,
            agent: _,
            team: _,
            json,
        } => {
            let client = crate::ipc::DaemonClient::connect().await?;
            let packet = crate::ipc::RequestPacket::ExtensionList {
                request_id: 1,
                enabled_only,
                ext_type: r#type,
            };
            let response = client.request_response(packet).await?;

            match response {
                crate::ipc::ResponsePacket::ExtensionList {
                    extensions, total, ..
                } => {
                    if json {
                        println!(
                            "{{\"extensions\": {}, \"total\": {}}}",
                            serde_json::to_string(&extensions)?,
                            total
                        );
                    } else {
                        println!("Extensions ({} total):", total);
                        for ext in &extensions {
                            println!(
                                "  {} | {} | {} | {}",
                                ext.id, ext.ext_type, ext.name, ext.source
                            );
                        }
                    }
                }
                _ => anyhow::bail!("Unexpected response"),
            }
            Ok(())
        }

        ExtCommands::Enable {
            id,
            target,
            principal,
        } => {
            if let Some(principal_name) = principal {
                let cap = capability_for_extension(&id);
                let response = capability::capability_grant(&principal_name, &cap).await?;
                match response {
                    crate::ipc::ResponsePacket::CapabilityGranted { message, .. } => {
                        println!("{}", message);
                    }
                    crate::ipc::ResponsePacket::Error { message, .. } => anyhow::bail!(message),
                    _ => anyhow::bail!("Unexpected response"),
                }
            } else {
                let client = crate::ipc::DaemonClient::connect().await?;
                let packet = crate::ipc::RequestPacket::ExtensionEnable {
                    request_id: 1,
                    id: id.clone(),
                    target,
                };
                let response = client.request_response(packet).await?;
                match response {
                    crate::ipc::ResponsePacket::ExtensionEnabled { message, .. } => {
                        println!("{}", message);
                    }
                    _ => anyhow::bail!("Unexpected response"),
                }
            }
            Ok(())
        }

        ExtCommands::Disable {
            id,
            target,
            principal,
        } => {
            if let Some(principal_name) = principal {
                let cap = capability_for_extension(&id);
                let response = capability::capability_revoke(&principal_name, &cap).await?;
                match response {
                    crate::ipc::ResponsePacket::CapabilityRevoked { message, .. } => {
                        println!("{}", message);
                    }
                    crate::ipc::ResponsePacket::Error { message, .. } => anyhow::bail!(message),
                    _ => anyhow::bail!("Unexpected response"),
                }
            } else {
                let client = crate::ipc::DaemonClient::connect().await?;
                let packet = crate::ipc::RequestPacket::ExtensionDisable {
                    request_id: 1,
                    id: id.clone(),
                    target,
                };
                let response = client.request_response(packet).await?;
                match response {
                    crate::ipc::ResponsePacket::ExtensionDisabled { message, .. } => {
                        println!("{}", message);
                    }
                    _ => anyhow::bail!("Unexpected response"),
                }
            }
            Ok(())
        }

        ExtCommands::Install { path, r#type: _ } => {
            let client = crate::ipc::DaemonClient::connect().await?;
            let packet = crate::ipc::RequestPacket::ExtensionInstall {
                request_id: 1,
                path: path.to_string_lossy().to_string(),
            };
            let response = client.request_response(packet).await?;
            match response {
                crate::ipc::ResponsePacket::ExtensionInstalled { id, message, .. } => {
                    println!("{}", message);
                    println!("   ID: {id}");
                }
                _ => anyhow::bail!("Unexpected response"),
            }
            Ok(())
        }

        ExtCommands::Uninstall { id } => {
            let client = crate::ipc::DaemonClient::connect().await?;
            let packet = crate::ipc::RequestPacket::ExtensionUninstall {
                request_id: 1,
                id: id.clone(),
            };
            let response = client.request_response(packet).await?;
            match response {
                crate::ipc::ResponsePacket::ExtensionUninstalled { message, .. } => {
                    println!("{}", message);
                }
                _ => anyhow::bail!("Unexpected response"),
            }
            Ok(())
        }

        ExtCommands::Start { id } => {
            DaemonClientService::ext_start(&id).await?;
            println!("Background runtime for '{}' started", id);
            Ok(())
        }

        ExtCommands::Stop { id } => {
            DaemonClientService::ext_stop(&id).await?;
            println!("Background runtime for '{}' stopped", id);
            Ok(())
        }

        ExtCommands::Restart { id } => {
            DaemonClientService::ext_restart(&id).await?;
            println!("Background runtime for '{}' restarted", id);
            Ok(())
        }

        ExtCommands::Status { id } => {
            let status = DaemonClientService::ext_status(&id).await?;
            println!("Background runtime status for '{}'", id);
            println!("  State:          {}", status.state);
            println!("  Restart count:  {}", status.restart_count);
            if let Some(err) = status.last_error {
                println!("  Last error:     {}", err);
            }
            Ok(())
        }

        ExtCommands::Mcp(cmd) => mcp::execute(cmd, paths).await,

        // --- IPC commands ---
        ExtCommands::Validate {
            path,
            verbose,
            semantic,
        } => {
            let client = crate::ipc::DaemonClient::connect().await?;
            let packet = crate::ipc::RequestPacket::ExtensionValidate {
                request_id: 1,
                path: path.to_string_lossy().to_string(),
                verbose,
                semantic,
            };
            let response = client.request_response(packet).await?;
            match response {
                crate::ipc::ResponsePacket::ExtensionValidated {
                    valid,
                    errors,
                    warnings,
                    ..
                } => {
                    let report = crate::extensions::validation::ValidationReport {
                        detected_type: "unknown".to_string(),
                        errors,
                        warnings,
                    };
                    print_validation_report(&report, verbose)?;
                    if !valid {
                        anyhow::bail!("Extension validation failed");
                    }
                    Ok(())
                }
                _ => anyhow::bail!("Unexpected response"),
            }
        }

        ExtCommands::Debug { id } => {
            let client = crate::ipc::DaemonClient::connect().await?;
            let packet = crate::ipc::RequestPacket::ExtensionDebug {
                request_id: 1,
                id: id.clone(),
            };
            let response = client.request_response(packet).await?;
            match response {
                crate::ipc::ResponsePacket::ExtensionDebugInfo { info, .. } => {
                    println!(
                        "Debug Information for Extension: {}",
                        info.get("id").and_then(|v| v.as_str()).unwrap_or(&id)
                    );
                    println!(
                        "  Name: {}",
                        info.get("name").and_then(|v| v.as_str()).unwrap_or("n/a")
                    );
                    println!(
                        "  Type: {}",
                        info.get("type").and_then(|v| v.as_str()).unwrap_or("n/a")
                    );
                    println!(
                        "  Version: {}",
                        info.get("version")
                            .and_then(|v| v.as_str())
                            .unwrap_or("n/a")
                    );
                    println!(
                        "  Path: {}",
                        info.get("path").and_then(|v| v.as_str()).unwrap_or("n/a")
                    );
                    println!(
                        "  Hooks: {}",
                        info.get("hooks").and_then(|v| v.as_u64()).unwrap_or(0)
                    );
                    Ok(())
                }
                _ => anyhow::bail!("Unexpected response"),
            }
        }

        ExtCommands::Info { id } => {
            let client = crate::ipc::DaemonClient::connect().await?;
            let packet = crate::ipc::RequestPacket::ExtensionInfo {
                request_id: 1,
                id: id.clone(),
            };
            let response = client.request_response(packet).await?;
            match response {
                crate::ipc::ResponsePacket::ExtensionInfoResponse { info, .. } => {
                    println!("Extension Details");
                    println!("=================");
                    println!("{}", serde_json::to_string_pretty(&info)?);
                    Ok(())
                }
                _ => anyhow::bail!("Unexpected response"),
            }
        }

        ExtCommands::Export { id, output } => {
            let client = crate::ipc::DaemonClient::connect().await?;
            let packet = crate::ipc::RequestPacket::ExtensionExport {
                request_id: 1,
                id: id.clone(),
                output: output.clone(),
            };
            let response = client.request_response(packet).await?;
            match response {
                crate::ipc::ResponsePacket::ExtensionExported { output, .. } => {
                    println!("Exported extension '{}' to {}", id, output);
                    Ok(())
                }
                _ => anyhow::bail!("Unexpected response"),
            }
        }

        ExtCommands::Bundle { name, ids } => {
            let client = crate::ipc::DaemonClient::connect().await?;
            let packet = crate::ipc::RequestPacket::ExtensionBundle {
                request_id: 1,
                name: name.clone(),
                ids,
            };
            let response = client.request_response(packet).await?;
            match response {
                crate::ipc::ResponsePacket::ExtensionBundled { count, .. } => {
                    println!("Created bundle '{}' with {} extension(s)", name, count);
                    Ok(())
                }
                _ => anyhow::bail!("Unexpected response"),
            }
        }

        // --- Direct file I/O / registry HTTP commands (keep local) ---
        // Config writes local TOML files; Push/Pull use registry HTTP directly.
        ExtCommands::Config {
            id,
            show,
            set,
            unset,
            global,
            team,
            agent,
        } => handle_config(paths, id, show, set, unset, global, team, agent).await,

        ExtCommands::Push {
            id,
            registry_ref,
            with_deps,
        } => handle_ext_push(paths, &id, &registry_ref, json, cli_registry, with_deps).await,

        ExtCommands::Pull {
            registry_ref,
            json: pull_json,
            no_deps,
        } => handle_ext_pull(paths, &registry_ref, pull_json, cli_registry, no_deps).await,

        ExtCommands::Init {
            name,
            r#type,
            output,
            lang,
            bare,
            gateway_type,
        } => {
            let output_dir = output.unwrap_or_else(|| PathBuf::from(&name));
            let lang = lang
                .and_then(|l| ScaffoldLang::from_str(&l))
                .unwrap_or_default();

            let options = ScaffoldOptions {
                id: name.clone(),
                name: name.clone(),
                description: format!("A {} extension", r#type),
                output_dir: output_dir.clone(),
                lang,
                bare_mcp: bare,
                gateway_type,
            };

            let result = ScaffoldEngine::scaffold(&r#type, &options)?;

            println!(
                "Created {} extension '{}' in {}",
                r#type,
                name,
                result.display()
            );
            println!();

            // List created files
            let mut entries: Vec<_> = std::fs::read_dir(&result)?.collect::<Result<Vec<_>, _>>()?;
            entries.sort_by_key(|e| e.file_name());
            for entry in entries {
                let file_name = entry.file_name().to_string_lossy().to_string();
                let marker = if file_name == "manifest.yaml"
                    || file_name == "SKILL.md"
                    || file_name == "server.json"
                {
                    "  — edit your extension metadata"
                } else if file_name.starts_with("handler.") || file_name.starts_with("gateway.") {
                    "  — implement your logic here"
                } else if file_name == "README.md" {
                    "  — documentation for users"
                } else {
                    ""
                };
                println!("  {}{}", file_name, marker);
            }

            println!();
            println!("Next steps:");
            println!("  peko ext validate {}", result.display());
            println!("  peko ext install {}", result.display());

            Ok(())
        }
    }
}

// --- Validation Report Rendering ---

fn print_validation_report(
    report: &crate::extensions::validation::ValidationReport,
    verbose: bool,
) -> anyhow::Result<()> {
    if !verbose {
        println!("Detected type: {}", report.detected_type);
    }
    println!();

    if report.errors.is_empty() && report.warnings.is_empty() {
        println!("Validation passed! Extension is valid and ready to install.");
    } else if report.errors.is_empty() {
        println!("Validation passed with warnings:");
        for warning in &report.warnings {
            println!("  ! {warning}");
        }
    } else {
        println!("Validation failed with errors:");
        for error in &report.errors {
            println!("  X {error}");
        }
        if !report.warnings.is_empty() {
            println!();
            println!("Additional warnings:");
            for warning in &report.warnings {
                println!("  ! {warning}");
            }
        }
        anyhow::bail!("Extension validation failed");
    }

    Ok(())
}

// --- Install ---

/// Extract a `.ext` package file to a temp directory if needed.
/// Returns the path to use for installation (either the extracted dir or the original path).
pub fn prepare_install_path(path: &std::path::Path) -> anyhow::Result<std::path::PathBuf> {
    if path.extension().map_or(false, |e| e == "ext") {
        let temp_dir = std::env::temp_dir().join("PEKO_ext_install").join(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
                .to_string(),
        );
        std::fs::create_dir_all(&temp_dir)?;
        let extracted =
            crate::extensions::framework::manager::packaging::ExtensionUnpackager::install(
                path, &temp_dir,
            )
            .map_err(|e| {
                anyhow::anyhow!("Failed to extract .ext package '{}': {}", path.display(), e)
            })?;
        Ok(extracted)
    } else {
        Ok(path.to_path_buf())
    }
}

// --- List ---
// (handled via IPC in handle_ext_command)

// --- Enable / Disable ---
// (handled via IPC in handle_ext_command)

// --- Uninstall ---
// (handled via IPC in handle_ext_command)

// --- Validate / Debug / Info / Export / Bundle ---
// (handled via IPC in handle_ext_command)

// --- Config ---

async fn handle_config(
    paths: &GlobalPaths,
    id: String,
    show: bool,
    set_values: Vec<String>,
    unset_keys: Vec<String>,
    _global: bool,
    team: Option<String>,
    agent: Option<String>,
) -> anyhow::Result<()> {
    let (team_id, agent_id) = match (&team, &agent) {
        (Some(t), Some(a)) => (Some(t.as_str()), Some(format!("{t}/{a}"))),
        (None, Some(a)) => {
            if a.contains('/') {
                let parts: Vec<&str> = a.split('/').collect();
                (Some(parts[0]), Some(a.clone()))
            } else {
                (Some("default"), Some(format!("default/{a}")))
            }
        }
        (Some(t), None) => (Some(t.as_str()), None),
        _ => (None, None),
    };

    let scope_label = match (&team_id, &agent_id) {
        (Some(_t), Some(a)) => format!("agent '{a}'"),
        (Some(t), None) => format!("team '{t}'"),
        _ => "global".to_string(),
    };

    let config_service = ExtensionConfigService::new(&paths.data_dir);
    let scope = match (&team_id, &agent_id) {
        (Some(_t), Some(a)) => {
            let parts: Vec<&str> = a.split('/').collect();
            ConfigScope::Agent(parts[0].to_string(), parts[1].to_string())
        }
        (Some(t), None) => ConfigScope::Team(t.to_string()),
        _ => ConfigScope::Global,
    };

    // Handle --show (default if no other actions)
    if show || (set_values.is_empty() && unset_keys.is_empty()) {
        println!("Configuration for extension '{id}' ({scope_label} scope):");
        println!();

        let target_config = config_service.show(&id, scope)?;

        if target_config.is_empty() {
            println!("  No configuration set at this scope.");
        } else {
            for (key, value) in &target_config {
                println!("  {key} = {value}");
            }
        }

        if team_id.is_some() || agent_id.is_some() {
            println!();
            println!("Inherited from global:");
            let global_config = config_service.global(&id)?;
            let mut inherited = false;
            for (key, value) in &global_config {
                if !target_config.contains_key(key) {
                    println!("  {key} = {value} (global)");
                    inherited = true;
                }
            }
            if !inherited {
                println!("  (none)");
            }
        }

        return Ok(());
    }

    // Handle --set
    for pair in &set_values {
        let parts: Vec<&str> = pair.splitn(2, '=').collect();
        if parts.len() != 2 {
            anyhow::bail!("Invalid format '{pair}'. Use KEY=VALUE");
        }
        let key = parts[0].to_string();
        let value = parts[1];
        let json_value = serde_json::from_str(value)
            .unwrap_or_else(|_| serde_json::Value::String(value.to_string()));
        config_service.set(&id, scope.clone(), &key, json_value)?;
        println!("Set {key} = {value} for extension '{id}' at {scope_label} scope");
    }

    // Handle --unset
    for key in &unset_keys {
        if config_service.unset(&id, scope.clone(), key)? {
            println!("Unset '{key}' for extension '{id}' at {scope_label} scope");
        } else {
            println!("Key '{key}' not found for extension '{id}' at {scope_label} scope");
        }
    }

    Ok(())
}

// --- Push / Pull ---

async fn handle_ext_push(
    paths: &GlobalPaths,
    id: &str,
    registry_ref: &str,
    json: bool,
    cli_registry: Option<&str>,
    with_deps: bool,
) -> anyhow::Result<()> {
    let result = paths
        .services()
        .extension_management()
        .push_extension(id, registry_ref, cli_registry, with_deps, move |event| {
            if json {
                return;
            }
            match event {
                ProgressEvent::Resolving { .. } => {}
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
            "extension_id": result.id,
            "registry_ref": result.registry_ref,
            "manifest": {
                "name": result.name,
                "version": result.version,
                "digest": result.digest,
                "kind": result.kind,
                "layers": result.layers,
                "total_size": result.total_size,
            }
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    }

    Ok(())
}

async fn handle_ext_pull(
    paths: &GlobalPaths,
    registry_ref: &str,
    json: bool,
    cli_registry: Option<&str>,
    no_deps: bool,
) -> anyhow::Result<()> {
    let result = paths
        .services()
        .extension_management()
        .pull_extension(registry_ref, cli_registry, no_deps, move |event| {
            if json {
                return;
            }
            match event {
                ProgressEvent::Resolving { .. } => {
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

                    if bytes_received == bytes_total && bytes_received != Some(0) {
                        println!("  Layer {}  ✓ downloaded", short_digest);
                    } else if bytes_received == Some(0) {
                        println!("  Layer {}  → downloading", short_digest);
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
            }
        })
        .await?;

    if json {
        let dep_json: Vec<_> = result
            .dependencies
            .iter()
            .map(|d| {
                serde_json::json!({
                    "registry_ref": d.registry_ref,
                    "success": d.success,
                    "error": d.error,
                })
            })
            .collect();

        let output = serde_json::json!({
            "success": true,
            "id": result.manifest_name,
            "registry_ref": result.registry_ref,
            "manifest": {
                "name": result.manifest_name,
                "version": result.manifest_version,
                "digest": result.manifest_digest,
                "kind": result.manifest_kind,
                "layers": result.manifest_layers,
                "total_size": result.manifest_total_size,
            },
            "dependencies": dep_json,
            "no_deps": no_deps,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!(
            "Extension installed successfully\n   ID: {}",
            result.manifest_name
        );

        let failed = result
            .dependencies
            .iter()
            .filter(|d| !d.success)
            .collect::<Vec<_>>();
        if !failed.is_empty() {
            eprintln!();
            eprintln!("WARNING: Some dependencies failed to pull:");
            for dep in &failed {
                eprintln!(
                    "  - {}: {}",
                    dep.registry_ref,
                    dep.error.as_deref().unwrap_or("unknown error")
                );
            }
        }
    }

    Ok(())
}

/// Map an extension/tool ID to the `tool:` capability used for principal-scoped
/// enable/disable.
///
/// Built-in tools are referenced by their bare name (e.g. `Bash` → `tool:Bash`).
/// The legacy `builtin:tool:` prefix is stripped so the capability matches the
/// tool-name gate in the registry.
fn capability_for_extension(id: &str) -> String {
    if id.starts_with("builtin:tool:") {
        let tool_name = id.splitn(3, ':').nth(2).unwrap_or(id);
        format!("tool:{tool_name}")
    } else {
        format!("tool:{id}")
    }
}
