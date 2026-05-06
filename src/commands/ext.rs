//! Extension management commands
//!
//! Thin CLI dispatcher — all business logic lives in domain modules:
//! - `extension::services::ExtensionConfigService` — config persistence
//! - `extension::adapters::ExtensionValidationService` — manifest validation
//! - `ipc::client_service::DaemonClientService` — daemon IPC
//! - `common::services::ConfigAuthorityImpl` — agent whitelist management

use crate::commands::GlobalPaths;
use crate::extension::manager::{ExtensionManager, ExtensionStorage, LoadedExtension};
use crate::extension::services::{ConfigScope, ExtensionConfigService, Services};
use crate::extension::types::ExtensionId;
use crate::ipc::client_service::DaemonClientService;
use clap::Subcommand;
use std::collections::HashMap;
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
}

/// Create an `ExtensionManager` with all default adapters registered
async fn create_manager_with_adapters(storage: Option<ExtensionStorage>) -> ExtensionManager {
    use crate::extensions::builtin::{BuiltinToolAdapter, BuiltinToolRegistrarConfig};
    use crate::extensions::gateway::GatewayAdapter;
    use crate::extensions::general::GeneralExtensionAdapter;
    use crate::extensions::mcp::McpAdapter;
    use crate::extensions::skill::SkillAdapter;
    use crate::extensions::universal::UniversalToolAdapter;

    let core = crate::extension::core::global_core().expect("Global ExtensionCore not initialized");

    if let Err(e) = BuiltinToolAdapter::register_all(&core, &BuiltinToolRegistrarConfig::default()).await {
        tracing::warn!("Failed to register built-in tools with ExtensionCore: {}", e);
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

/// Handle extension subcommands
pub async fn handle_ext_command(command: ExtCommands, paths: &GlobalPaths) -> anyhow::Result<()> {
    match command {
        ExtCommands::Validate { path, verbose } => {
            let report = crate::extension::adapters::ExtensionValidationService::validate(&path, verbose).await?;
            print_validation_report(&report, verbose)?;
            Ok(())
        }
        ExtCommands::Debug { id } => {
            let storage = ExtensionStorage::with_dir(paths.data_dir.join("extensions"));
            let mut manager = create_manager_with_adapters(Some(storage)).await;
            manager.load_all().await?;
            handle_debug(&manager, id).await
        }
        _ => {
            let storage = ExtensionStorage::with_dir(paths.data_dir.join("extensions"));
            let mut manager = create_manager_with_adapters(Some(storage)).await;
            manager.load_all().await?;

            match command {
                ExtCommands::Install { path, r#type } => handle_install(&mut manager, path, r#type).await,
                ExtCommands::List { enabled_only, r#type, agent, team } => {
                    handle_list(&manager, paths, enabled_only, r#type, agent, team).await
                }
                ExtCommands::Enable { id, target } => handle_enable(&mut manager, paths, id, target).await,
                ExtCommands::Disable { id, target } => handle_disable(&mut manager, paths, id, target).await,
                ExtCommands::Uninstall { id } => handle_uninstall(&mut manager, id).await,
                ExtCommands::Info { id } => handle_info(&manager, id),
                ExtCommands::Bundle { name, ids } => handle_bundle(&manager, name, ids),
                ExtCommands::Config { id, show, set, unset, global, team, agent } => {
                    handle_config(paths, id, show, set, unset, global, team, agent).await
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
                ExtCommands::Validate { .. } | ExtCommands::Debug { .. } => unreachable!(),
            }
        }
    }
}

// --- Validation Report Rendering ---

fn print_validation_report(report: &crate::extension::adapters::ValidationReport, verbose: bool) -> anyhow::Result<()> {
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

async fn handle_install(
    manager: &mut ExtensionManager,
    path: PathBuf,
    ext_type: Option<String>,
) -> anyhow::Result<()> {
    println!("Installing extension from: {}", path.display());
    if let Some(ref t) = ext_type {
        println!("   Type: {t}");
    }
    match manager.install(&path).await {
        Ok(id) => {
            println!("Extension installed successfully");
            println!("   ID: {id}");
            Ok(())
        }
        Err(e) => {
            eprintln!("Failed to install extension: {e}");
            Err(e)
        }
    }
}

// --- List ---

async fn handle_list(
    manager: &ExtensionManager,
    paths: &GlobalPaths,
    _enabled_only: bool,
    ext_type: Option<String>,
    agent_filter: Option<String>,
    team_filter: Option<String>,
) -> anyhow::Result<()> {
    let extensions = manager.list_extensions();
    let builtins = if let Some(core) = crate::extension::core::global_core() {
        core.list_builtin_extensions().await
    } else {
        Vec::new()
    };

    let filtered_installed: Vec<&LoadedExtension> = extensions
        .into_iter()
        .filter(|ext| ext_type.as_ref().map_or(true, |t| &ext.extension_type == t))
        .collect();
    let filtered_builtins: Vec<&crate::extension::core::BuiltinExtensionInfo> = builtins
        .iter()
        .filter(|b| ext_type.as_ref().map_or(true, |t| &b.ext_type == t))
        .collect();

    if filtered_installed.is_empty() && filtered_builtins.is_empty() {
        println!("No extensions match the specified criteria.");
        return Ok(());
    }

    let show_permissions = agent_filter.is_some() || team_filter.is_some();
    let agent_configs = if show_permissions {
        collect_agent_extension_configs(paths, agent_filter.as_deref(), team_filter.as_deref()).await?
    } else {
        Vec::new()
    };

    let runtime_states = fetch_runtime_states(&filtered_installed).await;

    if show_permissions {
        let access_header = if agent_configs.len() == 1 {
            format!("ACCESS ({}/{})", agent_configs[0].0, agent_configs[0].1)
        } else {
            "ACCESS".to_string()
        };
        println!(
            "{:<24} {:<14} {:<18} {:<10} {:<12} {}",
            "ID", "TYPE", "NAME", "SOURCE", "RUNTIME", access_header
        );
        println!("{}", "-".repeat(105));
    } else {
        println!(
            "{:<24} {:<14} {:<18} {:<10} {:<12}",
            "ID", "TYPE", "NAME", "SOURCE", "RUNTIME"
        );
        println!("{}", "-".repeat(85));
    }

    for b in &filtered_builtins {
        let status = if b.enabled { "" } else { " [disabled]" };
        let source = format!("built-in{status}");
        if show_permissions {
            let access = format_access_for_agent_configs(b.name.as_str(), &agent_configs);
            println!(
                "{:<24} {:<14} {:<18} {:<10} {:<12} {}",
                b.id, b.ext_type, b.name, source, "n/a", access
            );
        } else {
            println!(
                "{:<24} {:<14} {:<18} {:<10} {:<12}",
                b.id, b.ext_type, b.name, source, "n/a"
            );
        }
    }

    for ext in &filtered_installed {
        let source = "installed";
        let tool_name = ext.manifest.name.as_str();
        let runtime_status = runtime_states
            .get(ext.manifest.id.0.as_str())
            .cloned()
            .unwrap_or_else(|| "n/a".to_string());
        if show_permissions {
            let access = format_access_for_agent_configs(tool_name, &agent_configs);
            println!(
                "{:<24} {:<14} {:<18} {:<10} {:<12} {}",
                ext.manifest.id, ext.extension_type, ext.manifest.name, source, runtime_status, access
            );
        } else {
            println!(
                "{:<24} {:<14} {:<18} {:<10} {:<12}",
                ext.manifest.id, ext.extension_type, ext.manifest.name, source, runtime_status
            );
        }
    }

    println!();
    println!("Total: {} extension(s)", filtered_installed.len() + filtered_builtins.len());

    if show_permissions {
        if agent_configs.len() > 1 {
            println!();
            println!("Access legend:");
            for (team, agent, _) in &agent_configs {
                println!("  + {team}/{agent} = allowed");
                println!("  - {team}/{agent} = denied");
            }
        } else if agent_configs.len() == 1 {
            println!();
            println!("  + = allowed  |  - = denied");
        }
    }

    if !filtered_builtins.is_empty() {
        println!();
        println!("Note: Built-in extensions are compiled into the binary.");
        println!("      Installed extensions are loaded from disk.");
    }

    Ok(())
}

async fn fetch_runtime_states(
    extensions: &[&LoadedExtension],
) -> HashMap<String, String> {
    let mut states = HashMap::new();
    let runtime_types: std::collections::HashSet<&str> = ["gateway", "mcp"].iter().cloned().collect();

    let client = match crate::ipc::DaemonClient::connect().await {
        Ok(c) => c,
        Err(_) => return states,
    };

    for ext in extensions {
        if !runtime_types.contains(ext.extension_type.as_str()) {
            continue;
        }
        let id = ext.manifest.id.0.as_str();
        if let Ok(crate::ipc::ResponsePacket::ExtStatus { state, .. }) = client.ext_status(id).await {
            states.insert(id.to_string(), state);
        }
    }

    states
}

async fn collect_agent_extension_configs(
    paths: &GlobalPaths,
    agent_filter: Option<&str>,
    team_filter: Option<&str>,
) -> anyhow::Result<Vec<(String, String, crate::types::agent::ExtensionConfig)>> {
    let mut result = Vec::new();

    if let Some(agent_id) = agent_filter {
        let (team, agent_name) = crate::common::paths::resolve_team_agent(agent_id)?;
        let config_path = paths.resolver().agent_config(&agent_name, Some(&team));
        if config_path.exists() {
            let content = std::fs::read_to_string(&config_path)?;
            let config: crate::types::agent::AgentConfig = toml::from_str(&content)?;
            let ext_config = config.extensions.unwrap_or_default();
            result.push((team, agent_name, ext_config));
        } else {
            anyhow::bail!("Agent '{agent_name}' not found in team '{team}'");
        }
    } else if let Some(team) = team_filter {
        let agents_dir = paths.resolver().agents_dir(Some(team));
        if !agents_dir.exists() {
            anyhow::bail!("Team '{team}' not found (no agents directory)");
        }
        for entry in std::fs::read_dir(&agents_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let agent_name = entry.file_name().to_string_lossy().to_string();
            let config_path = paths.resolver().agent_config(&agent_name, Some(team));
            if config_path.exists() {
                let content = std::fs::read_to_string(&config_path)?;
                let config: crate::types::agent::AgentConfig = toml::from_str(&content)?;
                let ext_config = config.extensions.unwrap_or_default();
                result.push((team.to_string(), agent_name, ext_config));
            }
        }
        if result.is_empty() {
            println!("No agents found in team '{team}'.");
        }
    }

    Ok(result)
}

fn format_access_for_agent_configs(
    tool_name: &str,
    agent_configs: &[(String, String, crate::types::agent::ExtensionConfig)],
) -> String {
    if agent_configs.len() == 1 {
        let (_, _, ext) = &agent_configs[0];
        if ext.is_extension_enabled(tool_name) { "OK".to_string() } else { "NO".to_string() }
    } else {
        let parts: Vec<String> = agent_configs
            .iter()
            .map(|(team, agent, ext)| {
                let symbol = if ext.is_extension_enabled(tool_name) { "+" } else { "-" };
                format!("{symbol}:{team}/{agent}")
            })
            .collect();
        parts.join(" ")
    }
}

// --- Enable / Disable ---

async fn handle_enable(
    manager: &mut ExtensionManager,
    paths: &GlobalPaths,
    id: String,
    target: Option<String>,
) -> anyhow::Result<()> {
    let is_builtin = crate::extensions::builtin::BuiltinToolAdapter::is_builtin(&id) || id.starts_with("builtin:");
    if is_builtin {
        let capability = if id.starts_with("builtin:") {
            id.splitn(3, ':').nth(2).unwrap_or(&id).to_string()
        } else {
            id.clone()
        };
        return handle_enable_builtin(paths, &capability, target.as_deref()).await;
    }

    let ext_id = ExtensionId::new(&id);
    let (tool_name, ext_type) = {
        let ext = manager
            .get_extension(&ext_id)
            .ok_or_else(|| anyhow::anyhow!("Extension '{id}' not found"))?;
        (ext.manifest.name.clone(), ext.extension_type.clone())
    };

    match manager.enable(&ext_id).await {
        Ok(()) => {
            println!("Extension '{id}' enabled");
            let needs_whitelist = ext_type == "universal-tool" || ext_type == "mcp";
            if needs_whitelist {
                let target = target.as_deref().unwrap_or("default");
                let whitelist_entry = if ext_type == "mcp" {
                    format!("mcp:{tool_name}:*")
                } else {
                    tool_name.clone()
                };
                if let Err(e) = add_tool_to_agent_whitelist(paths, &whitelist_entry, target).await {
                    tracing::warn!("Failed to add tool '{}' to agent whitelist: {}", whitelist_entry, e);
                }
            }
            Ok(())
        }
        Err(e) => {
            eprintln!("Failed to enable extension '{id}': {e}");
            Err(e)
        }
    }
}

async fn add_tool_to_agent_whitelist(
    paths: &GlobalPaths,
    tool_name: &str,
    target: &str,
) -> anyhow::Result<()> {
    let (team, agent) = if target.contains('/') {
        let parts: Vec<&str> = target.splitn(2, '/').collect();
        (parts[0].to_string(), Some(parts[1].to_string()))
    } else {
        (target.to_string(), None)
    };

    let config_service = paths.services().agent_config();

    if let Some(agent_name) = agent {
        config_service.enable_tool_sync(&agent_name, &team, tool_name)?;
        println!("Added '{tool_name}' to agent '{team}/{agent_name}' tool whitelist");
        tracing::info!("Added '{}' to agent '{}/{}' tool whitelist", tool_name, team, agent_name);
    } else {
        let updated_count = config_service.enable_tool_for_team(&team, tool_name)?;
        if updated_count > 0 {
            println!("Added '{tool_name}' to {updated_count} agent(s) in team '{team}' tool whitelist");
            tracing::info!("Added '{}' to {} agent(s) in team '{}' tool whitelist", tool_name, updated_count, team);
        } else {
            tracing::warn!("No agents found in team '{}' to add tool '{}' to", team, tool_name);
        }
    }

    Ok(())
}

async fn handle_enable_builtin(
    paths: &GlobalPaths,
    capability: &str,
    target: Option<&str>,
) -> anyhow::Result<()> {
    let target = target.unwrap_or("default");
    let (team, agent) = if target.contains('/') {
        let parts: Vec<&str> = target.splitn(2, '/').collect();
        (parts[0].to_string(), Some(parts[1].to_string()))
    } else {
        (target.to_string(), None)
    };

    let config_service = paths.services().agent_config();

    if let Some(agent_name) = agent {
        config_service.enable_tool_sync(&agent_name, &team, capability)?;
        println!("Enabled '{capability}' for agent '{agent_name}' in team '{team}'");
    } else {
        let team_dir = paths.data_dir.join("teams").join(&team);
        let ext_config_path = team_dir.join("extensions.toml");
        let mut config = crate::common::types::TeamExtConfig::load(&ext_config_path)?;
        config.enable(capability);
        config.save(&ext_config_path)?;
        println!("Enabled '{capability}' for team '{team}'");
    }

    Services::enable_builtin_hooks(capability).await;
    Ok(())
}

async fn handle_disable(
    manager: &mut ExtensionManager,
    paths: &GlobalPaths,
    id: String,
    target: Option<String>,
) -> anyhow::Result<()> {
    let is_builtin = crate::extensions::builtin::BuiltinToolAdapter::is_builtin(&id) || id.starts_with("builtin:");
    if is_builtin {
        let capability = if id.starts_with("builtin:") {
            id.splitn(3, ':').nth(2).unwrap_or(&id).to_string()
        } else {
            id.clone()
        };
        return handle_disable_builtin(paths, &capability, target.as_deref()).await;
    }

    let ext_id = ExtensionId::new(&id);
    if manager.get_extension(&ext_id).is_none() {
        anyhow::bail!("Extension '{id}' not found");
    }

    match manager.disable(&ext_id).await {
        Ok(()) => {
            println!("Extension '{id}' disabled");
            Ok(())
        }
        Err(e) => {
            eprintln!("Failed to disable extension '{id}': {e}");
            Err(e)
        }
    }
}

async fn handle_disable_builtin(
    paths: &GlobalPaths,
    capability: &str,
    target: Option<&str>,
) -> anyhow::Result<()> {
    let target = target.unwrap_or("default");
    let (team, agent) = if target.contains('/') {
        let parts: Vec<&str> = target.splitn(2, '/').collect();
        (parts[0].to_string(), Some(parts[1].to_string()))
    } else {
        (target.to_string(), None)
    };

    let config_service = paths.services().agent_config();

    if let Some(agent_name) = agent {
        config_service.disable_tool_sync(&agent_name, &team, capability)?;
        println!("Disabled '{capability}' for agent '{agent_name}' in team '{team}'");
    } else {
        let team_dir = paths.data_dir.join("teams").join(&team);
        let ext_config_path = team_dir.join("extensions.toml");
        let mut config = crate::common::types::TeamExtConfig::load(&ext_config_path)?;
        config.disable(capability);
        config.save(&ext_config_path)?;
        println!("Disabled '{capability}' for team '{team}'");
    }

    Services::disable_builtin_hooks(capability).await;
    Ok(())
}

// --- Uninstall ---

async fn handle_uninstall(manager: &mut ExtensionManager, id: String) -> anyhow::Result<()> {
    let ext_id = ExtensionId::new(&id);
    if manager.get_extension(&ext_id).is_none() {
        anyhow::bail!("Extension '{id}' not found");
    }
    println!("Uninstalling extension '{id}'...");
    match manager.uninstall(&ext_id).await {
        Ok(()) => {
            println!("Extension '{id}' uninstalled");
            Ok(())
        }
        Err(e) => {
            eprintln!("Failed to uninstall extension '{id}': {e}");
            Err(e)
        }
    }
}

// --- Info ---

fn handle_info(manager: &ExtensionManager, id: String) -> anyhow::Result<()> {
    let ext_id = ExtensionId::new(&id);
    let ext = manager
        .get_extension(&ext_id)
        .ok_or_else(|| anyhow::anyhow!("Extension '{id}' not found"))?;

    println!("Extension Details");
    println!();
    println!("ID:          {}", ext.manifest.id);
    println!("Name:        {}", ext.manifest.name);
    println!("Type:        {}", ext.extension_type);
    println!("Version:     {}", ext.manifest.version);
    println!("Status:      installed");
    println!("Description: {}", ext.manifest.description);
    println!("Path:        {}", ext.path.display());
    println!();
    println!("Note: Tool access is controlled per-agent via 'tools.enabled' in agent config.");
    println!("Use 'pekobot ext enable {id} --target <team>/<agent>' to enable for a specific agent.");

    if !ext.hook_ids.is_empty() {
        println!();
        println!("Registered hooks: {}", ext.hook_ids.len());
    }

    Ok(())
}

// --- Bundle ---

fn handle_bundle(manager: &ExtensionManager, name: String, ids: Vec<String>) -> anyhow::Result<()> {
    if ids.is_empty() {
        anyhow::bail!("At least one extension ID is required to create a bundle");
    }
    let mut ext_ids = Vec::new();
    for id in &ids {
        let ext_id = ExtensionId::new(id);
        if manager.get_extension(&ext_id).is_none() {
            anyhow::bail!("Extension '{id}' not found");
        }
        ext_ids.push(ext_id);
    }

    println!("Creating bundle '{}' with {} extension(s)...", name, ids.len());
    match manager.create_bundle(ext_ids, &name) {
        Ok(bundle) => {
            println!("Bundle '{}' created successfully", bundle.name);
            println!("Extensions included:");
            for manifest in &bundle.extensions {
                println!("  - {} ({})", manifest.id, manifest.name);
            }
            Ok(())
        }
        Err(e) => {
            eprintln!("Failed to create bundle: {e}");
            Err(e)
        }
    }
}

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
        (Some(t), Some(a)) => {
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

// --- Debug ---

async fn handle_debug(manager: &ExtensionManager, id: String) -> anyhow::Result<()> {
    let ext_id = ExtensionId::new(&id);
    let ext = manager
        .get_extension(&ext_id)
        .ok_or_else(|| anyhow::anyhow!("Extension '{id}' not found"))?;

    println!("Debug Information for Extension: {id}");
    println!("{}", "=".repeat(60));
    println!();
    println!("Basic Information:");
    println!("  ID:          {}", ext.manifest.id);
    println!("  Name:        {}", ext.manifest.name);
    println!("  Type:        {}", ext.extension_type);
    println!("  Version:     {}", ext.manifest.version);
    println!("  Status:      installed");
    println!("  Description: {}", ext.manifest.description);
    println!("  Path:        {}", ext.path.display());
    println!();
    println!("Note: Tool access is controlled per-agent via 'tools.enabled' in agent config.");
    println!();

    println!("Hook Registrations:");
    if ext.hook_ids.is_empty() {
        println!("  (no hooks registered)");
    } else {
        println!("  Total hooks: {}", ext.hook_ids.len());
        let core = manager.core();
        let all_hooks = core.get_all_hooks().await;
        for hook_id in &ext.hook_ids {
            if let Some(hook) = all_hooks.iter().find(|h| &h.id == hook_id) {
                println!();
                println!("  Hook ID:     {hook_id}");
                println!("    Point:     {}", hook.point.name());
                println!("    Category:  {}", hook.point.category());
                println!("    Priority:  {}", hook.priority);
                println!("    Enabled:   {}", hook.enabled);
            } else {
                println!();
                println!("  Hook ID:     {hook_id}");
                println!("    (details unavailable - hook may be pending)");
            }
        }
    }
    println!();

    println!("Manifest Metadata:");
    if ext.manifest.metadata.is_empty() {
        println!("  (no additional metadata)");
    } else {
        for (key, value) in &ext.manifest.metadata {
            let value_str = format!("{value}");
            let display_value = if value_str.len() > 100 {
                format!("{}... (truncated)", &value_str[..100])
            } else {
                value_str
            };
            println!("  {key}: {display_value}");
        }
    }
    println!();

    println!("Extension Core Statistics:");
    let core = manager.core();
    println!("  Total hooks registered: {}", core.hook_count().await);
    println!("  Hooks for this extension: {}", ext.hook_ids.len());
    println!();

    println!("Debug complete.");
    Ok(())
}
