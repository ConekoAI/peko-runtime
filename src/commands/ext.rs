//! Extension management commands
//!
//! Thin CLI dispatcher — all business logic lives in domain modules:
//! - `extension::services::ExtensionConfigService` — config persistence
//! - `extension::adapters::ExtensionValidationService` — manifest validation
//! - `ipc::client_service::DaemonClientService` — daemon IPC
//! - `common::services::ConfigAuthorityImpl` — agent whitelist management

use crate::commands::GlobalPaths;
use crate::common::services::CredentialsService;
use crate::extension::core::ExtensionCore;
use crate::extension::manager::packaging::ExtensionPackager;
use crate::extension::manager::{ExtensionManager, ExtensionStorage};
use crate::extension::scaffold::{ScaffoldEngine, ScaffoldLang, ScaffoldOptions};
use crate::extension::services::{ConfigScope, ExtensionConfigService};
use crate::extension::types::{ExtensionId, ExtensionManifest};
use crate::ipc::client_service::DaemonClientService;
use crate::portable::registry::AgentRegistry;
use crate::portable::types::{compute_digest, ImageDigest, Layer, LayerType};
use crate::registry::client::{ProgressEvent, RegistryClient, RegistryRef, ResourceType};
use crate::registry::config::{RegistryConfig, RegistrySource};
use crate::registry::manifest::RegistryManifest;
use anyhow::Context;
use clap::Subcommand;
use std::path::PathBuf;
use std::sync::Arc;

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

    /// Enable an extension or built-in capability
    Enable {
        /// Extension ID or built-in capability name (e.g., shell, `read_file`)
        id: String,
        /// Target team or team/agent for built-in capabilities
        #[arg(short, long, value_name = "TARGET")]
        target: Option<String>,
    },

    /// Disable an extension or built-in capability
    Disable {
        /// Extension ID or built-in capability name
        id: String,
        /// Target team or team/agent for built-in capabilities
        #[arg(short, long, value_name = "TARGET")]
        target: Option<String>,
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

/// Create an `ExtensionManager` with all default adapters registered
async fn create_manager_with_adapters(
    core: Arc<ExtensionCore>,
    storage: Option<ExtensionStorage>,
) -> ExtensionManager {
    use crate::extensions::builtin::{BuiltinToolAdapter, BuiltinToolRegistrarConfig};
    use crate::extensions::gateway::GatewayAdapter;
    use crate::extensions::general::GeneralExtensionAdapter;
    use crate::extensions::mcp::McpAdapter;
    use crate::extensions::skill::SkillAdapter;
    use crate::extensions::universal::UniversalToolAdapter;

    if let Err(e) =
        BuiltinToolAdapter::register_all(&core, &BuiltinToolRegistrarConfig::default()).await
    {
        tracing::warn!(
            "Failed to register built-in tools with ExtensionCore: {}",
            e
        );
    }

    let mut manager = ExtensionManager::with_core(core.clone());
    if let Some(storage) = storage {
        manager = manager.with_storage_dir(storage.dir().unwrap().to_path_buf());
    }

    manager.register_adapter(Box::new(SkillAdapter::new()));
    manager.register_adapter(Box::new(McpAdapter::with_default_manager()));
    manager.register_adapter(Box::new(UniversalToolAdapter::new()));
    manager.register_adapter(Box::new(GatewayAdapter::new(core)));
    manager.register_adapter(Box::new(GeneralExtensionAdapter::new()));

    manager
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

/// Handle extension subcommands
pub async fn handle_ext_command(
    command: ExtCommands,
    paths: &GlobalPaths,
    json: bool,
    cli_registry: Option<&str>,
) -> anyhow::Result<()> {
    // Get the global ExtensionCore — initialized by main.rs before command dispatch.
    // We extract it once and pass it down to eliminate direct global_core() calls
    // in subcommand handlers.
    let core = crate::extension::core::global_core().expect("Global ExtensionCore not initialized");

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

        ExtCommands::Enable { id, target } => {
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
            Ok(())
        }

        ExtCommands::Disable { id, target } => {
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

        // --- IPC commands ---
        ExtCommands::Validate { path, verbose, semantic } => {
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
                    let report = crate::extension::adapters::ValidationReport {
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
        } => {
            let storage = ExtensionStorage::with_dir(paths.data_dir.join("extensions"));
            let mut manager = create_manager_with_adapters(core.clone(), Some(storage)).await;
            manager.load_all().await?;
            handle_ext_push(&manager, &id, &registry_ref, json, cli_registry, paths, with_deps).await
        }

        ExtCommands::Pull {
            registry_ref,
            json: pull_json,
            no_deps,
        } => {
            let storage = ExtensionStorage::with_dir(paths.data_dir.join("extensions"));
            let mut manager = create_manager_with_adapters(core.clone(), Some(storage)).await;
            manager.load_all().await?;

            // Pull the extension to a temp file using local manager
            let (temp_path, _manifest) = handle_ext_pull_to_temp(
                &mut manager,
                &registry_ref,
                pull_json,
                no_deps,
                cli_registry,
                paths,
            )
            .await?;

            // Install via IPC so the daemon knows about it
            let client = crate::ipc::DaemonClient::connect().await?;
            let packet = crate::ipc::RequestPacket::ExtensionInstall {
                request_id: 1,
                path: temp_path.to_string_lossy().to_string(),
            };
            let response = client.request_response(packet).await?;
            match response {
                crate::ipc::ResponsePacket::ExtensionInstalled { id, message, .. } => {
                    if pull_json {
                        println!(
                            "{{\"success\": true, \"id\": \"{}\", \"message\": \"{}\"}}",
                            id, message
                        );
                    } else {
                        println!("{message}");
                        println!("   ID: {id}");
                    }
                    Ok(())
                }
                crate::ipc::ResponsePacket::Error { message, .. } => Err(anyhow::anyhow!(
                    "Failed to install pulled extension: {message}"
                )),
                _ => anyhow::bail!("Unexpected response from daemon during extension install"),
            }
        }

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

            println!("Created {} extension '{}' in {}", r#type, name, result.display());
            println!();

            // List created files
            let mut entries: Vec<_> = std::fs::read_dir(&result)?.collect::<Result<Vec<_>, _>>()?;
            entries.sort_by_key(|e| e.file_name());
            for entry in entries {
                let file_name = entry.file_name().to_string_lossy().to_string();
                let marker = if file_name == "manifest.yaml" || file_name == "SKILL.md" || file_name == "server.json" {
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
    report: &crate::extension::adapters::ValidationReport,
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
            crate::extension::manager::packaging::ExtensionUnpackager::install(path, &temp_dir)
                .map_err(|e| {
                    anyhow::anyhow!("Failed to extract .ext package '{}': {}", path.display(), e)
                })?;
        Ok(extracted)
    } else {
        Ok(path.to_path_buf())
    }
}

async fn handle_install(
    manager: &mut ExtensionManager,
    path: PathBuf,
    ext_type: Option<String>,
) -> anyhow::Result<ExtensionManifest> {
    println!("Installing extension from: {}", path.display());
    if let Some(ref t) = ext_type {
        println!("   Type: {t}");
    }

    let install_path = prepare_install_path(&path)?;
    if install_path != path {
        println!("   Extracted .ext package to: {}", install_path.display());
    }

    match manager.install(&install_path).await {
        Ok(id) => {
            println!("Extension installed successfully");
            println!("   ID: {id}");
            // Return the manifest of the installed extension
            manager
                .get_extension(&id)
                .map(|e| e.manifest.clone())
                .context("Installed extension not found in manager")
        }
        Err(e) => {
            eprintln!("Failed to install extension: {e}");
            Err(e)
        }
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
    manager: &ExtensionManager,
    id: &str,
    registry_ref: &str,
    json: bool,
    cli_registry: Option<&str>,
    paths: &GlobalPaths,
    with_deps: bool,
) -> anyhow::Result<()> {
    let ext_id = ExtensionId::new(id);

    // Verify extension exists before exporting
    let ext = manager
        .get_extension(&ext_id)
        .ok_or_else(|| anyhow::anyhow!("Extension '{id}' not found"))?;

    // Resolve dependencies if --with-deps
    let mut dep_ids = Vec::new();
    if with_deps {
        let resolution = manager.resolve_dependencies_root(&ext.manifest)?;
        if resolution.has_required_missing() {
            let missing: Vec<_> = resolution
                .missing
                .iter()
                .filter(|m| matches!(m, crate::extension::manager::DependencyStatus::Missing { required: true, .. }))
                .map(|m| format!("{m:?}"))
                .collect();
            anyhow::bail!(
                "Cannot push with --with-deps: required dependencies are not installed: {}",
                missing.join(", ")
            );
        }
        for dep in &resolution.satisfied {
            if let crate::extension::manager::DependencyStatus::Satisfied { package, .. } = dep {
                dep_ids.push(crate::extension::types::ExtensionId::new(package));
            }
        }
    }

    // Export to a temp .ext file
    let temp_dir = std::env::temp_dir().join("PEKO_ext_push");
    std::fs::create_dir_all(&temp_dir)?;
    let temp_path = temp_dir.join(format!("{}.ext", ext.manifest.id.0));

    ExtensionPackager::export_with_deps(manager, &ext_id, &dep_ids, temp_path.to_string_lossy().as_ref())?;

    // Read file bytes and compute digest
    let data = tokio::fs::read(&temp_path).await?;
    let layer_digest = compute_digest(&data);

    // Store as layer in AgentRegistry
    let registry = AgentRegistry::new(AgentRegistry::default_path());
    registry.init().await?;
    registry.store_layer(&layer_digest, &data).await?;

    // Build RegistryManifest with kind="extension", single layer.
    // The OCI top-level `config` descriptor must point to a real
    // sha256:<hex> blob, otherwise pekohub rejects the push with
    // 400 "Invalid digest format" (see
    // `pekohub/backend/src/routes/oci/manifests.ts:172` and the
    // `bundle_versions.config_digest` NOT NULL constraint). For
    // extensions the .ext payload itself serves as the config
    // blob — point the descriptor at the same digest/size as the
    // layer below.
    let mut manifest =
        RegistryManifest::new(ext.manifest.name.clone(), ext.manifest.version.clone())
            .with_kind("extension")
            .with_ref(registry_ref)
            .with_bundle_type("extension")
            .with_extension_type(&ext.extension_type)
            .with_description(&ext.manifest.description)
            .with_config(layer_digest.clone(), data.len() as u64, None::<String>);
    manifest.add_layer(Layer::new(
        layer_digest.clone(),
        LayerType::Config,
        data.len() as u64,
    ));

    // Compute manifest digest
    let manifest_json = manifest.to_json()?;
    let manifest_digest = ImageDigest::from_bytes(manifest_json.as_bytes());
    manifest.digest = manifest_digest.as_str().to_string();

    // Store manifest for RegistryClient
    store_registry_manifest_for_client(&registry, &manifest).await?;

    // Parse registry ref and configure client
    let reg_ref = RegistryRef::parse_with_default(
        registry_ref,
        cli_registry.or(Some(&paths.registry_config().default)),
        Some(ResourceType::Extension),
    )?;
    let config = resolve_registry_config(paths, cli_registry, &reg_ref.host)?;

    let client = RegistryClient::new(config, registry);

    let resolved_ref = reg_ref.full_ref();

    if json {
        let result = client.push(&manifest_digest, &resolved_ref, |_| {}).await?;
        let output = serde_json::json!({
            "success": true,
            "extension_id": id,
            "registry_ref": resolved_ref,
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
        println!("Pushing extension '{id}' to {resolved_ref}...");
        let _result = client
            .push(&manifest_digest, &resolved_ref, |event| match event {
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
    let _ = tokio::fs::remove_file(&temp_path).await;

    Ok(())
}

/// Pull an extension from a registry to a temp file, returning the temp path and manifest.
///
/// This is used by `peko ext pull` when it wants to install via IPC.
pub async fn handle_ext_pull_to_temp(
    manager: &mut ExtensionManager,
    registry_ref: &str,
    json: bool,
    _no_deps: bool,
    cli_registry: Option<&str>,
    paths: &GlobalPaths,
) -> anyhow::Result<(
    std::path::PathBuf,
    crate::registry::manifest::RegistryManifest,
)> {
    let agent_registry = AgentRegistry::new(AgentRegistry::default_path());
    agent_registry.init().await?;

    let reg_ref = RegistryRef::parse_with_default(
        registry_ref,
        cli_registry.or(Some(&paths.registry_config().default)),
        Some(ResourceType::Extension),
    )?;
    let config = resolve_registry_config(paths, cli_registry, &reg_ref.host)?;

    let client = RegistryClient::new(config, agent_registry.clone());

    let resolved_ref = reg_ref.full_ref();

    let manifest = if json {
        client.pull(&resolved_ref, |_| {}).await?
    } else {
        println!("Pulling extension {resolved_ref}...");
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

    // Read the layer bytes from AgentRegistry
    let layer = manifest
        .layers
        .first()
        .ok_or_else(|| anyhow::anyhow!("Manifest has no layers"))?;
    let data = agent_registry.get_layer(&layer.digest).await?;

    // Write to a temp .ext file
    let temp_dir = std::env::temp_dir().join("PEKO_ext_pull");
    std::fs::create_dir_all(&temp_dir)?;
    let temp_path = temp_dir.join(format!("{}.ext", manifest.name));
    tokio::fs::write(&temp_path, &data).await?;

    // Record the registry source for this extension in local manager
    let ext_id = crate::extension::types::ExtensionId::new(&manifest.name);
    if manager.storage_dir().is_some() {
        let _ = manager.storage().write_source(&ext_id, registry_ref);
    }

    Ok((temp_path, manifest))
}

/// Pull an extension from a registry and install it.
///
/// This is the public entry point used by both `peko ext pull` and
/// `peko team pull` (for auto-pulling team extensions).
pub async fn handle_ext_pull(
    manager: &mut ExtensionManager,
    registry_ref: &str,
    json: bool,
    no_deps: bool,
    cli_registry: Option<&str>,
    paths: &GlobalPaths,
) -> anyhow::Result<()> {
    handle_ext_pull_with_seen(
        manager,
        registry_ref,
        json,
        no_deps,
        cli_registry,
        paths,
        &mut std::collections::HashSet::new(),
    )
    .await
}

/// Internal implementation that tracks which packages have already been pulled
/// in this dependency tree to prevent infinite recursion on circular dependencies.
async fn handle_ext_pull_with_seen(
    manager: &mut ExtensionManager,
    registry_ref: &str,
    json: bool,
    no_deps: bool,
    cli_registry: Option<&str>,
    paths: &GlobalPaths,
    already_pulled: &mut std::collections::HashSet<String>,
) -> anyhow::Result<()> {
    use crate::extension::manager::{DependencyResolution, DependencyStatus};

    // Prevent infinite recursion: if we've already attempted to pull this ref
    // in the current dependency tree, skip it.
    if !already_pulled.insert(registry_ref.to_string()) {
        if !json {
            eprintln!(
                "  Skipping {} (already pulled in this dependency tree)",
                registry_ref
            );
        }
        return Ok(());
    }

    let (temp_path, manifest) =
        handle_ext_pull_to_temp(manager, registry_ref, json, no_deps, cli_registry, paths).await?;

    // Install the main extension first — on success we get the manifest back
    let install_result = handle_install(manager, temp_path.clone(), None).await;

    // Record the registry source for this extension
    if let Ok(ref ext_manifest) = install_result {
        let ext_id = ext_manifest.id.clone();
        if manager.storage_dir().is_some() {
            let _ = manager.storage().write_source(&ext_id, registry_ref);
        }
        // Also update the in-memory manifest if it's loaded
        if let Some(loaded) = manager.get_extension_mut(&ext_id) {
            loaded.manifest.source = Some(registry_ref.to_string());
        }
    }

    // Resolve dependencies using the manifest returned from install, or fail clearly
    let dep_resolution: DependencyResolution = match &install_result {
        Ok(ext_manifest) => manager.resolve_dependencies_root(ext_manifest)?,
        Err(e) => {
            // Install failed — clean up and report
            let _ = tokio::fs::remove_file(&temp_path).await;
            if json {
                let output = serde_json::json!({
                    "success": false,
                    "registry_ref": registry_ref,
                    "error": e.to_string(),
                    "dependencies": [],
                });
                println!("{}", serde_json::to_string_pretty(&output)?);
            }
            return Err(anyhow::anyhow!("{e}"));
        }
    };

    // Handle dependency resolution output and recursive pulling
    let mut dep_pull_results: Vec<(String, anyhow::Result<()>)> = Vec::new();

    if !no_deps && !dep_resolution.missing.is_empty() {
        if json {
            // JSON output for dependencies will be included in the final output
        } else {
            let required_count = dep_resolution
                .missing
                .iter()
                .filter(|d| matches!(d, DependencyStatus::Missing { required: true, .. }))
                .count();
            let optional_count = dep_resolution
                .missing
                .iter()
                .filter(|d| {
                    matches!(
                        d,
                        DependencyStatus::Missing {
                            required: false,
                            ..
                        }
                    )
                })
                .count();

            println!();
            if required_count > 0 && optional_count > 0 {
                println!(
                    "Dependencies ({} required, {} optional need installation):",
                    required_count, optional_count
                );
            } else if required_count > 0 {
                println!(
                    "Dependencies ({} required need installation):",
                    required_count
                );
            } else {
                println!(
                    "Dependencies ({} optional need installation):",
                    optional_count
                );
            }

            for dep in &dep_resolution.missing {
                if let DependencyStatus::Missing { package, required } = dep {
                    let label = if *required { "required" } else { "optional" };
                    println!("  - {} ({})", package, label);
                }
            }
            println!();
            println!("Pulling dependencies...");

            for dep in &dep_resolution.missing {
                if let DependencyStatus::Missing { package, .. } = dep {
                    let result = Box::pin(handle_ext_pull_with_seen(
                        manager,
                        package,
                        false, // non-JSON for recursive deps
                        false, // always pull deps of deps
                        cli_registry,
                        paths,
                        already_pulled,
                    ))
                    .await;
                    dep_pull_results.push((package.clone(), result));
                }
            }
        }
    } else if no_deps && !dep_resolution.missing.is_empty() {
        // Emit warning when --no-deps is used and dependencies are missing
        if json {
            // Warning included in JSON output below
        } else {
            let ext_name = install_result
                .as_ref()
                .map(|m| m.name.as_str())
                .unwrap_or(&manifest.name);
            let missing_count = dep_resolution.missing.len();
            eprintln!();
            eprintln!(
                "WARNING: Extension '{}' declares {} dependenc{} that are not installed:",
                ext_name,
                missing_count,
                if missing_count == 1 { "y" } else { "ies" }
            );
            for dep in &dep_resolution.missing {
                if let DependencyStatus::Missing { package, required } = dep {
                    let req_label = if *required { "required" } else { "optional" };
                    eprintln!("  - {}: {}", req_label, package);
                }
            }
            eprintln!("Run 'peko ext pull {}' to install them.", registry_ref);
        }
    }

    // Report circular dependencies
    if !dep_resolution.circular.is_empty() && !json {
        eprintln!();
        eprintln!("WARNING: Circular dependencies detected:");
        for cycle in &dep_resolution.circular {
            eprintln!("  {}", cycle.join(" -> "));
        }
    }

    // Clean up temp file
    let _ = tokio::fs::remove_file(&temp_path).await;

    // Build dependency JSON for output
    let mut dep_json = Vec::new();
    for dep in &dep_resolution.missing {
        if let DependencyStatus::Missing { package, required } = dep {
            let pulled = dep_pull_results.iter().find(|(p, _)| p == package);
            dep_json.push(serde_json::json!({
                "package": package,
                "status": "missing",
                "required": required,
                "pulled": pulled.map(|(_, r)| r.is_ok()).unwrap_or(false),
            }));
        }
    }
    for dep in &dep_resolution.satisfied {
        if let DependencyStatus::Satisfied {
            package,
            installed_version,
        } = dep
        {
            dep_json.push(serde_json::json!({
                "package": package,
                "status": "satisfied",
                "version": installed_version,
            }));
        }
    }
    for dep in &dep_resolution.version_mismatches {
        if let DependencyStatus::VersionMismatch {
            package,
            have,
            need,
        } = dep
        {
            dep_json.push(serde_json::json!({
                "package": package,
                "status": "version_mismatch",
                "installed_version": have,
                "required_version": need,
            }));
        }
    }

    if json {
        let output = serde_json::json!({
            "success": true,
            "registry_ref": registry_ref,
            "manifest": {
                "name": manifest.name,
                "version": manifest.version,
                "digest": manifest.digest,
                "kind": manifest.kind,
                "layers": manifest.layers.len(),
                "total_size": manifest.total_size_bytes(),
            },
            "dependencies": dep_json,
            "no_deps": no_deps,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        // Report any failed dependency pulls
        let failed_deps: Vec<_> = dep_pull_results
            .iter()
            .filter(|(_, r)| r.is_err())
            .collect();
        if !failed_deps.is_empty() {
            eprintln!();
            eprintln!("WARNING: Some dependencies failed to pull:");
            for (pkg, err) in &failed_deps {
                eprintln!("  - {}: {}", pkg, err.as_ref().unwrap_err());
            }
        }
    }

    Ok(())
}

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
