//! Extension management commands
//!
//! Provides CLI commands for managing extensions:
//! - Install, uninstall, list extensions
//! - Enable/disable extensions
//! - Show extension details
//! - Create bundles from extensions
//! - Configure extensions (global, team, agent levels)

use crate::commands::GlobalPaths;
use crate::extension::manager::{ExtensionManager, ExtensionStorage, LoadedExtension};
use crate::extension::types::ExtensionId;
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
    ///
    /// Examples:
    ///   pekobot ext enable my-extension
    ///   pekobot ext enable shell --target default
    ///   pekobot ext enable shell --target myteam/my-agent
    Enable {
        /// Extension ID or built-in capability name (e.g., shell, `read_file`)
        id: String,
        /// Target team or team/agent for built-in capabilities
        #[arg(short, long, value_name = "TARGET")]
        target: Option<String>,
    },

    /// Disable an extension or built-in capability
    ///
    /// Examples:
    ///   pekobot ext disable my-extension
    ///   pekobot ext disable shell --target default
    Disable {
        /// Extension ID or built-in capability name
        id: String,
        /// Target team or team/agent for built-in capabilities
        #[arg(short, long, value_name = "TARGET")]
        target: Option<String>,
    },

    /// Uninstall an extension
    Uninstall {
        /// Extension ID
        id: String,
    },

    /// Show extension details
    Info {
        /// Extension ID
        id: String,
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
    ///
    /// Examples:
    ///   pekobot ext config my-extension --show
    ///   pekobot ext config my-extension --global --set `api_key=secret`
    ///   pekobot ext config my-extension --team myteam --set endpoint=<https://api.example.com>
    ///   pekobot ext config my-extension --agent myteam/myagent --set timeout=30
    ///   pekobot ext config my-extension --unset `api_key`
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
    ///
    /// Examples:
    ///   pekobot ext validate ./my-extension
    ///   pekobot ext validate ./my-skill --verbose
    Validate {
        /// Path to the extension directory or manifest
        path: PathBuf,

        /// Show detailed validation output
        #[arg(long)]
        verbose: bool,
    },

    /// Debug an installed extension
    ///
    /// Shows detailed information about the extension including:
    /// - Resolved hook bindings
    /// - Handler registrations
    /// - Configuration
    ///
    /// Examples:
    ///   pekobot ext debug my-extension
    Debug {
        /// Extension ID
        id: String,
    },

    /// Start a background runtime for an extension (daemon-scoped)
    ///
    /// Only extensions that declare a background runtime (e.g., gateway, mcp)
    /// can be started. Stateless extensions (skills, general) should use
    /// 'pekobot ext enable' instead.
    ///
    /// Examples:
    ///   pekobot ext start discord-gateway
    ///   pekobot ext start mcp-filesystem
    Start {
        /// Extension ID
        id: String,
    },

    /// Stop a background runtime for an extension (daemon-scoped)
    ///
    /// Examples:
    ///   pekobot ext stop discord-gateway
    ///   pekobot ext stop mcp-filesystem
    Stop {
        /// Extension ID
        id: String,
    },

    /// Restart a background runtime for an extension (daemon-scoped)
    ///
    /// Examples:
    ///   pekobot ext restart discord-gateway
    ///   pekobot ext restart mcp-filesystem
    Restart {
        /// Extension ID
        id: String,
    },

    /// Show background runtime status for an extension
    ///
    /// Examples:
    ///   pekobot ext status discord-gateway
    Status {
        /// Extension ID
        id: String,
    },
}

/// Create an `ExtensionManager` with all default adapters registered
async fn create_manager_with_adapters(storage: Option<ExtensionStorage>) -> ExtensionManager {
    use crate::extensions::gateway::GatewayAdapter;
    use crate::extensions::general::GeneralExtensionAdapter;
    use crate::extensions::mcp::McpAdapter;
    use crate::extensions::skill::SkillAdapter;
    use crate::extensions::universal::UniversalToolAdapter;
    use crate::extension::core::global_core;
    use crate::extensions::builtin::{BuiltinToolAdapter, BuiltinToolRegistrarConfig};

    // Global ExtensionCore is always initialized in main.rs before command dispatch.
    let core = global_core().expect("Global ExtensionCore not initialized");

    // Register built-in tools so they appear in ExtensionCore queries (e.g. ext list)
    if let Err(e) = BuiltinToolAdapter::register_all(&core, &BuiltinToolRegistrarConfig::default()).await {
        tracing::warn!(
            "Failed to register built-in tools with ExtensionCore: {}",
            e
        );
    }

    let mut manager = ExtensionManager::with_core(core.clone());

    if let Some(storage) = storage {
        manager = manager.with_storage_dir(storage.dir().unwrap().to_path_buf());
    }

    // Register extension type adapters
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
        ExtCommands::Validate { path, verbose } => handle_validate(path, verbose).await,
        ExtCommands::Debug { id } => {
            // Create storage in the data directory
            let storage = ExtensionStorage::with_dir(paths.data_dir.join("extensions"));
            let mut manager = create_manager_with_adapters(Some(storage)).await;
            // Load all extensions to populate the manager
            manager.load_all().await?;
            handle_debug(&manager, id).await
        }
        _ => {
            // Create storage in the data directory
            let storage = ExtensionStorage::with_dir(paths.data_dir.join("extensions"));
            let mut manager = create_manager_with_adapters(Some(storage)).await;

            // Load all extensions to populate the manager
            manager.load_all().await?;

            match command {
                ExtCommands::Install { path, r#type } => {
                    handle_install(&mut manager, path, r#type).await
                }
                ExtCommands::List {
                    enabled_only,
                    r#type,
                    agent,
                    team,
                } => handle_list(&manager, paths, enabled_only, r#type, agent, team).await,
                ExtCommands::Enable { id, target } => {
                    handle_enable(&mut manager, paths, id, target).await
                }
                ExtCommands::Disable { id, target } => {
                    handle_disable(&mut manager, paths, id, target).await
                }
                ExtCommands::Uninstall { id } => handle_uninstall(&mut manager, id).await,
                ExtCommands::Info { id } => handle_info(&manager, id),
                ExtCommands::Bundle { name, ids } => handle_bundle(&manager, name, ids),
                ExtCommands::Config {
                    id,
                    show,
                    set,
                    unset,
                    global,
                    team,
                    agent,
                } => handle_config(paths, id, show, set, unset, global, team, agent).await,
                ExtCommands::Start { id } => handle_start(id).await,
                ExtCommands::Stop { id } => handle_stop(id).await,
                ExtCommands::Restart { id } => handle_restart(id).await,
                ExtCommands::Status { id } => handle_status(id).await,
                // These are handled above
                ExtCommands::Validate { .. } | ExtCommands::Debug { .. } => unreachable!(),
            }
        }
    }
}

/// Best-effort fetch of runtime states from daemon for installed extensions
async fn fetch_runtime_states(
    extensions: &[&LoadedExtension],
) -> std::collections::HashMap<String, String> {
    let mut states = std::collections::HashMap::new();

    // Only query daemon for runtime-bearing extension types
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
        match client.ext_status(id).await {
            Ok(crate::ipc::ResponsePacket::ExtStatus { state, .. }) => {
                states.insert(id.to_string(), state);
            }
            _ => {}
        }
    }

    states
}

/// Handle install command
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

/// Handle list command
///
/// Shows both installed extensions and built-in extensions registered with
/// `ExtensionCore`. Built-ins are compiled into the binary and always available.
///
/// When `--agent` or `--team` is provided, shows access permissions for the
/// specified agent(s) by checking their `tools.enabled` whitelist.
async fn handle_list(
    manager: &ExtensionManager,
    paths: &GlobalPaths,
    _enabled_only: bool, // Kept for CLI compatibility, but ignored
    ext_type: Option<String>,
    agent_filter: Option<String>,
    team_filter: Option<String>,
) -> anyhow::Result<()> {
    let extensions = manager.list_extensions();

    // Get built-in extensions from ExtensionCore
    let builtins = if let Some(core) = crate::extension::core::global_core() {
        core.list_builtin_extensions().await
    } else {
        Vec::new()
    };

    // Filter installed extensions by type
    let filtered_installed: Vec<&LoadedExtension> = extensions
        .into_iter()
        .filter(|ext| {
            if let Some(ref t) = ext_type {
                if &ext.extension_type != t {
                    return false;
                }
            }
            true
        })
        .collect();

    // Filter built-ins by type
    let filtered_builtins: Vec<&crate::extension::core::BuiltinExtensionInfo> = builtins
        .iter()
        .filter(|b| {
            if let Some(ref t) = ext_type {
                if &b.ext_type != t {
                    return false;
                }
            }
            true
        })
        .collect();

    if filtered_installed.is_empty() && filtered_builtins.is_empty() {
        println!("No extensions match the specified criteria.");
        return Ok(());
    }

    // Determine if we're showing permissions
    let show_permissions = agent_filter.is_some() || team_filter.is_some();

    // Collect agent configs to check permissions against
    let agent_configs: Vec<(String, String, crate::types::agent::ToolConfig)> = if show_permissions {
        collect_agent_extension_configs(paths, agent_filter.as_deref(), team_filter.as_deref()).await?
    } else {
        Vec::new()
    };

    // ADR-026: Fetch runtime states from daemon (best-effort)
    let runtime_states = fetch_runtime_states(&filtered_installed).await;

    if show_permissions {
        // Print header with access columns
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
    println!(
        "Total: {} extension(s)",
        filtered_installed.len() + filtered_builtins.len()
    );

    if show_permissions {
        if agent_configs.len() > 1 {
            println!();
            println!("Access legend:");
            for (team, agent, _) in &agent_configs {
                println!("  ✓ {team}/{agent} = allowed");
                println!("  ✗ {team}/{agent} = denied");
            }
        } else if agent_configs.len() == 1 {
            println!();
            println!("  ✓ = allowed  |  ✗ = denied");
        }
    }

    if !filtered_builtins.is_empty() {
        println!();
        println!("Note: Built-in extensions are compiled into the binary.");
        println!("      Installed extensions are loaded from disk.");
    }

    Ok(())
}

/// Collect agent extension configurations for permission checking
async fn collect_agent_extension_configs(
    paths: &GlobalPaths,
    agent_filter: Option<&str>,
    team_filter: Option<&str>,
) -> anyhow::Result<Vec<(String, String, crate::types::agent::ExtensionConfig)>> {
    let mut result = Vec::new();
    let _config_service = paths.services().agent_config();

    if let Some(agent_id) = agent_filter {
        // Single agent mode
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
        // All agents in a team
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

/// Format access status for a tool against multiple agent configs
fn format_access_for_agent_configs(
    tool_name: &str,
    agent_configs: &[(String, String, crate::types::agent::ExtensionConfig)],
) -> String {
    if agent_configs.len() == 1 {
        let (_, _, ext) = &agent_configs[0];
        if ext.is_extension_enabled(tool_name) {
            "✓".to_string()
        } else {
            "✗".to_string()
        }
    } else {
        let mut parts = Vec::new();
        for (team, agent, ext) in agent_configs {
            let symbol = if ext.is_extension_enabled(tool_name) { "✓" } else { "✗" };
            parts.push(format!("{symbol}:{team}/{agent}"));
        }
        parts.join(" ")
    }
}

/// Handle enable command
async fn handle_enable(
    manager: &mut ExtensionManager,
    paths: &GlobalPaths,
    id: String,
    target: Option<String>,
) -> anyhow::Result<()> {
    // Normalize built-in IDs: accept both "shell" and "builtin:tool:shell"
    let is_builtin = crate::extensions::builtin::BuiltinToolAdapter::is_builtin(&id)
        || id.starts_with("builtin:");
    if is_builtin {
        let capability = if id.starts_with("builtin:") {
            // Extract name from builtin:{type}:{name}
            id.splitn(3, ':').nth(2).unwrap_or(&id).to_string()
        } else {
            id.clone()
        };
        return handle_enable_builtin(paths, &capability, target.as_deref()).await;
    }
    let ext_id = ExtensionId::new(&id);

    // Check if extension exists and get data we need
    let (tool_name, ext_type) = {
        let ext = manager
            .get_extension(&ext_id)
            .ok_or_else(|| anyhow::anyhow!("Extension '{id}' not found"))?;
        (ext.manifest.name.clone(), ext.extension_type.clone())
    };

    match manager.enable(&ext_id).await {
        Ok(()) => {
            println!("Extension '{id}' enabled");

            // Also add to agent tool whitelist for tools that need it (universal-tool and mcp)
            let needs_whitelist = ext_type == "universal-tool" || ext_type == "mcp";
            if needs_whitelist {
                let target = target.as_deref().unwrap_or("default");
                // For MCP tools, use wildcard pattern to match all tools from this server
                let whitelist_entry = if ext_type == "mcp" {
                    format!("mcp:{tool_name}:*")
                } else {
                    tool_name.clone()
                };
                if let Err(e) = add_tool_to_agent_whitelist(paths, &whitelist_entry, target).await {
                    tracing::warn!(
                        "Failed to add tool '{}' to agent whitelist: {}",
                        whitelist_entry,
                        e
                    );
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

/// Add a tool to an agent's tool whitelist
async fn add_tool_to_agent_whitelist(
    paths: &GlobalPaths,
    tool_name: &str,
    target: &str,
) -> anyhow::Result<()> {
    // Parse target into team and optional agent
    // Format: "team/agent" or "team" (team only, applies to all agents)
    let (team, agent) = if target.contains('/') {
        let parts: Vec<&str> = target.splitn(2, '/').collect();
        (parts[0].to_string(), Some(parts[1].to_string()))
    } else {
        // Target is just a team name - apply to all agents in that team
        (target.to_string(), None)
    };

    let config_service = paths.services().agent_config();

    if let Some(agent_name) = agent {
        config_service.enable_tool_sync(&agent_name, &team, tool_name)?;
        println!("Added '{tool_name}' to agent '{team}/{agent_name}' tool whitelist");
        tracing::info!(
            "Added '{}' to agent '{}/{}' tool whitelist",
            tool_name,
            team,
            agent_name
        );
    } else {
        // Team-level: update all agents in the team
        let agents_dir = paths.resolver().agents_dir(Some(&team));
        if !agents_dir.exists() {
            anyhow::bail!("Team '{team}' not found (no agents directory)");
        }

        let mut updated_count = 0;

        // Find all agents in the team
        for entry in std::fs::read_dir(&agents_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }

            let agent_name = entry.file_name().to_string_lossy().to_string();
            if paths
                .resolver()
                .agent_config(&agent_name, Some(&team))
                .exists()
            {
                config_service.enable_tool_sync(&agent_name, &team, tool_name)?;
                updated_count += 1;
            }
        }

        if updated_count > 0 {
            println!(
                "Added '{tool_name}' to {updated_count} agent(s) in team '{team}' tool whitelist"
            );
            tracing::info!(
                "Added '{}' to {} agent(s) in team '{}' tool whitelist",
                tool_name,
                updated_count,
                team
            );
        } else {
            tracing::warn!(
                "No agents found in team '{}' to add tool '{}' to",
                team,
                tool_name
            );
        }
    }

    Ok(())
}

/// Enable a built-in capability for a team or agent
async fn handle_enable_builtin(
    paths: &GlobalPaths,
    capability: &str,
    target: Option<&str>,
) -> anyhow::Result<()> {
    let target = target.unwrap_or("default");

    // Parse target into team and optional agent
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
        // Team-level: update team extensions config
        let team_dir = paths.data_dir.join("teams").join(&team);
        let ext_config_path = team_dir.join("extensions.toml");

        let mut config = crate::common::types::TeamExtConfig::load(&ext_config_path)?;
        config.enable(capability);
        config.save(&ext_config_path)?;

        println!("Enabled '{capability}' for team '{team}'");
    }

    // Also enable ExtensionCore hooks for immediate effect
    if let Some(core) = crate::extension::core::global_core() {
        let builtins = core.list_builtin_extensions().await;
        for b in &builtins {
            if b.name.eq_ignore_ascii_case(capability) {
                let ext_id = ExtensionId::new(&b.id);
                let hooks = core.get_hooks_for_extension(&ext_id).await;
                for hook in hooks {
                    let _ = core.enable_hook(&hook.id).await;
                }
                tracing::info!("Enabled built-in hooks for '{}'", b.id);
            }
        }
    }

    Ok(())
}

/// Handle disable command
async fn handle_disable(
    manager: &mut ExtensionManager,
    paths: &GlobalPaths,
    id: String,
    target: Option<String>,
) -> anyhow::Result<()> {
    // Normalize built-in IDs: accept both "shell" and "builtin:tool:shell"
    let is_builtin = crate::extensions::builtin::BuiltinToolAdapter::is_builtin(&id)
        || id.starts_with("builtin:");
    if is_builtin {
        let capability = if id.starts_with("builtin:") {
            id.splitn(3, ':').nth(2).unwrap_or(&id).to_string()
        } else {
            id.clone()
        };
        return handle_disable_builtin(paths, &capability, target.as_deref()).await;
    }

    let ext_id = ExtensionId::new(&id);

    // Check if extension exists
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

/// Disable a built-in capability for a team or agent
async fn handle_disable_builtin(
    paths: &GlobalPaths,
    capability: &str,
    target: Option<&str>,
) -> anyhow::Result<()> {
    let target = target.unwrap_or("default");

    // Parse target into team and optional agent
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
        // Team-level: update team extensions config
        let team_dir = paths.data_dir.join("teams").join(&team);
        let ext_config_path = team_dir.join("extensions.toml");

        let mut config = crate::common::types::TeamExtConfig::load(&ext_config_path)?;
        config.disable(capability);
        config.save(&ext_config_path)?;

        println!("Disabled '{capability}' for team '{team}'");
    }

    // Also disable ExtensionCore hooks for immediate effect
    if let Some(core) = crate::extension::core::global_core() {
        let builtins = core.list_builtin_extensions().await;
        for b in &builtins {
            if b.name.eq_ignore_ascii_case(capability) {
                let ext_id = ExtensionId::new(&b.id);
                let hooks = core.get_hooks_for_extension(&ext_id).await;
                for hook in hooks {
                    let _ = core.disable_hook(&hook.id).await;
                }
                tracing::info!("Disabled built-in hooks for '{}'", b.id);
            }
        }
    }

    Ok(())
}

/// Handle uninstall command
async fn handle_uninstall(manager: &mut ExtensionManager, id: String) -> anyhow::Result<()> {
    let ext_id = ExtensionId::new(&id);

    // Check if extension exists
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

/// Handle info command
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
    println!(
        "Use 'pekobot ext enable {id} --target <team>/<agent>' to enable for a specific agent."
    );

    if !ext.hook_ids.is_empty() {
        println!();
        println!("Registered hooks: {}", ext.hook_ids.len());
    }

    Ok(())
}

/// Handle bundle command
fn handle_bundle(manager: &ExtensionManager, name: String, ids: Vec<String>) -> anyhow::Result<()> {
    if ids.is_empty() {
        anyhow::bail!("At least one extension ID is required to create a bundle");
    }

    // Validate all extension IDs exist
    let mut ext_ids = Vec::new();
    for id in &ids {
        let ext_id = ExtensionId::new(id);
        if manager.get_extension(&ext_id).is_none() {
            anyhow::bail!("Extension '{id}' not found");
        }
        ext_ids.push(ext_id);
    }

    println!(
        "Creating bundle '{}' with {} extension(s)...",
        name,
        ids.len()
    );

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

/// Handle start command — start a background runtime via daemon IPC
async fn handle_start(id: String) -> anyhow::Result<()> {
    match crate::ipc::DaemonClient::connect().await {
        Ok(client) => {
            match client.ext_start(&id).await {
                Ok(crate::ipc::ResponsePacket::ExtStarted { extension_id, .. }) => {
                    println!("Background runtime for '{}' started", extension_id);
                    Ok(())
                }
                Ok(crate::ipc::ResponsePacket::Error { message, .. }) => {
                    anyhow::bail!("Failed to start '{}': {}", id, message)
                }
                Ok(other) => {
                    anyhow::bail!("Unexpected response from daemon: {:?}", other)
                }
                Err(e) => {
                    anyhow::bail!("Failed to communicate with daemon: {e}")
                }
            }
        }
        Err(e) => {
            anyhow::bail!("Daemon is not running. Start it with 'pekobot daemon start'. ({e})")
        }
    }
}

/// Handle stop command — stop a background runtime via daemon IPC
async fn handle_stop(id: String) -> anyhow::Result<()> {
    match crate::ipc::DaemonClient::connect().await {
        Ok(client) => {
            match client.ext_stop(&id).await {
                Ok(crate::ipc::ResponsePacket::ExtStopped { extension_id, .. }) => {
                    println!("Background runtime for '{}' stopped", extension_id);
                    Ok(())
                }
                Ok(crate::ipc::ResponsePacket::Error { message, .. }) => {
                    anyhow::bail!("Failed to stop '{}': {}", id, message)
                }
                Ok(other) => {
                    anyhow::bail!("Unexpected response from daemon: {:?}", other)
                }
                Err(e) => {
                    anyhow::bail!("Failed to communicate with daemon: {e}")
                }
            }
        }
        Err(e) => {
            anyhow::bail!("Daemon is not running. Start it with 'pekobot daemon start'. ({e})")
        }
    }
}

/// Handle restart command — restart a background runtime via daemon IPC
async fn handle_restart(id: String) -> anyhow::Result<()> {
    match crate::ipc::DaemonClient::connect().await {
        Ok(client) => {
            match client.ext_restart(&id).await {
                Ok(crate::ipc::ResponsePacket::ExtRestarted { extension_id, .. }) => {
                    println!("Background runtime for '{}' restarted", extension_id);
                    Ok(())
                }
                Ok(crate::ipc::ResponsePacket::Error { message, .. }) => {
                    anyhow::bail!("Failed to restart '{}': {}", id, message)
                }
                Ok(other) => {
                    anyhow::bail!("Unexpected response from daemon: {:?}", other)
                }
                Err(e) => {
                    anyhow::bail!("Failed to communicate with daemon: {e}")
                }
            }
        }
        Err(e) => {
            anyhow::bail!("Daemon is not running. Start it with 'pekobot daemon start'. ({e})")
        }
    }
}

/// Handle status command — show background runtime status via daemon IPC
async fn handle_status(id: String) -> anyhow::Result<()> {
    match crate::ipc::DaemonClient::connect().await {
        Ok(client) => {
            match client.ext_status(&id).await {
                Ok(crate::ipc::ResponsePacket::ExtStatus { extension_id, state, restart_count, last_error, .. }) => {
                    println!("Background runtime status for '{}'", extension_id);
                    println!("  State:          {}", state);
                    println!("  Restart count:  {}", restart_count);
                    if let Some(err) = last_error {
                        println!("  Last error:     {}", err);
                    }
                    Ok(())
                }
                Ok(crate::ipc::ResponsePacket::Error { message, .. }) => {
                    anyhow::bail!("Failed to get status for '{}': {}", id, message)
                }
                Ok(other) => {
                    anyhow::bail!("Unexpected response from daemon: {:?}", other)
                }
                Err(e) => {
                    anyhow::bail!("Failed to communicate with daemon: {e}")
                }
            }
        }
        Err(e) => {
            anyhow::bail!("Daemon is not running. Start it with 'pekobot daemon start'. ({e})")
        }
    }
}

/// Extension configuration storage
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
struct ExtensionConfig {
    /// Global settings (apply to all agents/teams)
    #[serde(default)]
    global: HashMap<String, serde_json::Value>,

    /// Per-team settings
    #[serde(default)]
    teams: HashMap<String, HashMap<String, serde_json::Value>>,

    /// Per-agent settings (format: "team/agent")
    #[serde(default)]
    agents: HashMap<String, HashMap<String, serde_json::Value>>,
}

impl ExtensionConfig {
    fn config_path(data_dir: &std::path::Path, extension_id: &str) -> PathBuf {
        data_dir
            .join("extensions")
            .join(extension_id)
            .join("config.toml")
    }

    fn load(data_dir: &std::path::Path, extension_id: &str) -> anyhow::Result<Self> {
        let path = Self::config_path(data_dir, extension_id);
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(&path)?;
        let config: Self = toml::from_str(&content)?;
        Ok(config)
    }

    fn save(&self, data_dir: &std::path::Path, extension_id: &str) -> anyhow::Result<()> {
        let path = Self::config_path(data_dir, extension_id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = toml::to_string_pretty(self)?;
        std::fs::write(&path, content)?;
        Ok(())
    }

    fn set(
        &mut self,
        team: Option<&str>,
        agent: Option<&str>,
        key: String,
        value: serde_json::Value,
    ) {
        let target = match (team, agent) {
            (Some(_), Some(_)) => {
                let agent_id = agent.unwrap().to_string();
                self.agents.entry(agent_id).or_default()
            }
            (Some(team_id), None) => self.teams.entry(team_id.to_string()).or_default(),
            _ => &mut self.global,
        };
        target.insert(key, value);
    }

    fn unset(&mut self, team: Option<&str>, agent: Option<&str>, key: &str) -> bool {
        match (team, agent) {
            (Some(_), Some(_)) => {
                if let Some(agent_config) = self.agents.get_mut(agent.unwrap()) {
                    agent_config.remove(key).is_some()
                } else {
                    false
                }
            }
            (Some(team_id), None) => {
                if let Some(team_config) = self.teams.get_mut(team_id) {
                    team_config.remove(key).is_some()
                } else {
                    false
                }
            }
            _ => self.global.remove(key).is_some(),
        }
    }
}

/// Handle config command
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
    // Parse agent ID if provided
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

    // Load or create config
    let mut config = ExtensionConfig::load(&paths.data_dir, &id)?;

    // Handle --show (default if no other actions)
    if show || (set_values.is_empty() && unset_keys.is_empty()) {
        println!("Configuration for extension '{id}' ({scope_label} scope):");
        println!();

        let target_config: &HashMap<String, serde_json::Value> = match (&team_id, &agent_id) {
            (Some(_), Some(a)) => config.agents.get(a).unwrap_or(&config.global),
            (Some(t), None) => config.teams.get(&t.to_string()).unwrap_or(&config.global),
            _ => &config.global,
        };

        if target_config.is_empty() {
            println!("  No configuration set at this scope.");
        } else {
            for (key, value) in target_config {
                println!("  {key} = {value}");
            }
        }

        // Also show inherited values
        if team_id.is_some() || agent_id.is_some() {
            println!();
            println!("Inherited from global:");
            let mut inherited = false;
            for (key, value) in &config.global {
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

        // Try to parse as JSON, fallback to string
        let json_value = serde_json::from_str(value)
            .unwrap_or_else(|_| serde_json::Value::String(value.to_string()));

        config.set(team_id, agent_id.as_deref(), key.clone(), json_value);
        println!("Set {key} = {value} for extension '{id}' at {scope_label} scope");
    }

    // Handle --unset
    for key in &unset_keys {
        if config.unset(team_id, agent_id.as_deref(), key) {
            println!("Unset '{key}' for extension '{id}' at {scope_label} scope");
        } else {
            println!("Key '{key}' not found for extension '{id}' at {scope_label} scope");
        }
    }

    // Save config
    config.save(&paths.data_dir, &id)?;

    Ok(())
}

/// Handle validate command
///
/// Uses the ADR-024 two-tier detection hierarchy:
/// Tier 1: Ecosystem standards (SKILL.md, server.json)
/// Tier 2: Unified manifest (manifest.yaml with `extension_type`)
async fn handle_validate(path: PathBuf, verbose: bool) -> anyhow::Result<()> {
    use crate::extension::adapters::extract_extension_type_from_yaml;
    use crate::extensions::general::discover_general_extensions;
    use crate::extensions::mcp::McpAdapter;
    use crate::extensions::skill::SkillAdapter;
    use crate::extensions::universal::UniversalToolAdapter;

    println!("Validating extension at: {}", path.display());
    println!();

    if !path.exists() {
        anyhow::bail!("Path does not exist: {}", path.display());
    }

    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    // ─── TIER 1: Ecosystem Standards ─────────────────────────────────────────

    if path.join("SKILL.md").exists() {
        if verbose {
            println!("✓ Detected as: skill extension (SKILL.md) [Tier 1 ecosystem standard]");
        }

        let skill_adapter = SkillAdapter::new();
        let skills = skill_adapter.discover_skills(&path);
        if skills.is_empty() {
            errors.push("No valid skills found in directory".to_string());
        } else if verbose {
            for skill in &skills {
                println!(
                    "  ✓ Skill: {} - {}",
                    skill.manifest.name, skill.manifest.description
                );
            }
        }

        print_summary(&errors, &warnings)?;
        return Ok(());
    }

    if path.join("server.json").exists() {
        if verbose {
            println!("✓ Detected as: MCP server extension (server.json) [Tier 1 ecosystem standard]");
        }

        // server.json is a registry metadata file; basic validation
        let server_json_path = path.join("server.json");
        match std::fs::read_to_string(&server_json_path) {
            Ok(content) => match serde_json::from_str::<serde_json::Value>(&content) {
                Ok(manifest) => {
                    if manifest.get("name").is_none() {
                        warnings.push("server.json missing 'name' field".to_string());
                    }
                    if verbose {
                        if let Some(name) = manifest.get("name").and_then(|v| v.as_str()) {
                            println!("  ✓ Server: {}", name);
                        }
                    }
                }
                Err(e) => errors.push(format!("Invalid server.json: {}", e)),
            },
            Err(e) => errors.push(format!("Failed to read server.json: {}", e)),
        }

        print_summary(&errors, &warnings)?;
        return Ok(());
    }

    // ─── TIER 2: Unified Manifest ────────────────────────────────────────────

    let manifest_yaml = path.join("manifest.yaml");
    if manifest_yaml.exists() {
        match extract_extension_type_from_yaml(&manifest_yaml) {
            Ok(Some(ext_type)) => {
                if verbose {
                    println!(
                        "✓ Detected as: {} extension (manifest.yaml) [Tier 2 unified manifest]",
                        ext_type
                    );
                }

                match ext_type.as_str() {
                    "universal-tool" => {
                        let adapter = UniversalToolAdapter::new();
                        let tools = adapter.discover_tools(&path).await;
                        if tools.is_empty() {
                            errors.push("No valid tools found in directory".to_string());
                        } else if verbose {
                            for tool in &tools {
                                println!(
                                    "  ✓ Tool: {} - {}",
                                    tool.manifest.name, tool.manifest.description
                                );
                            }
                        }
                    }
                    "mcp" => {
                        let adapter = McpAdapter::with_default_manager();
                        let servers = adapter.discover_servers(&path).await;
                        if servers.is_empty() {
                            errors.push("No valid MCP servers found in directory".to_string());
                        } else if verbose {
                            for server in &servers {
                                println!("  ✓ Server: {}", server.manifest.name);
                            }
                        }
                    }
                    "gateway" => {
                        if verbose {
                            println!("  ✓ Gateway extension validated");
                        }
                    }
                    "general" => {
                        let extensions = discover_general_extensions(&path).await?;
                        if extensions.is_empty() {
                            errors.push("No valid general extensions found in directory".to_string());
                        } else if verbose {
                            for ext in &extensions {
                                println!(
                                    "  ✓ Extension: {} - {}",
                                    ext.manifest.name, ext.manifest.description
                                );
                            }
                        }
                    }
                    custom if custom.starts_with("custom:") => {
                        if verbose {
                            println!("  ✓ Custom extension type: {}", custom);
                        }
                    }
                    other => {
                        warnings.push(format!(
                            "Unknown extension_type '{}'. Supported: universal-tool, mcp, gateway, general, custom:*",
                            other
                        ));
                    }
                }

                print_summary(&errors, &warnings)?;
                return Ok(());
            }
            Ok(None) => {
                // manifest.yaml exists but has no extension_type
            }
            Err(e) => {
                warnings.push(format!("Failed to parse manifest.yaml: {}", e));
            }
        }
    }

    // Nothing detected
    anyhow::bail!(
        "Could not detect extension type. Expected one of:\n\
         - SKILL.md (skill extension) [Tier 1]\n\
         - server.json (bare MCP server) [Tier 1]\n\
         - manifest.yaml with extension_type (unified manifest) [Tier 2]"
    );
}

fn print_summary(errors: &[String], warnings: &[String]) -> anyhow::Result<()> {
    println!();

    if errors.is_empty() && warnings.is_empty() {
        println!("✓ Validation passed! Extension is valid and ready to install.");
    } else if errors.is_empty() {
        println!("⚠ Validation passed with warnings:");
        for warning in warnings {
            println!("  ⚠ {warning}");
        }
    } else {
        println!("✗ Validation failed with errors:");
        for error in errors {
            println!("  ✗ {error}");
        }
        if !warnings.is_empty() {
            println!();
            println!("Additional warnings:");
            for warning in warnings {
                println!("  ⚠ {warning}");
            }
        }
        anyhow::bail!("Extension validation failed");
    }

    Ok(())
}

/// Handle debug command
async fn handle_debug(manager: &ExtensionManager, id: String) -> anyhow::Result<()> {
    let ext_id = ExtensionId::new(&id);

    let ext = manager
        .get_extension(&ext_id)
        .ok_or_else(|| anyhow::anyhow!("Extension '{id}' not found"))?;

    println!("Debug Information for Extension: {id}");
    println!("{}", "=".repeat(60));
    println!();

    // Basic info
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

    // Hook registrations
    println!("Hook Registrations:");
    if ext.hook_ids.is_empty() {
        println!("  (no hooks registered)");
    } else {
        println!("  Total hooks: {}", ext.hook_ids.len());

        // Get hook details from core
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

    // Manifest metadata
    println!("Manifest Metadata:");
    if ext.manifest.metadata.is_empty() {
        println!("  (no additional metadata)");
    } else {
        for (key, value) in &ext.manifest.metadata {
            // Truncate long values
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

    // Extension Core stats
    println!("Extension Core Statistics:");
    let core = manager.core();
    println!("  Total hooks registered: {}", core.hook_count().await);
    println!("  Hooks for this extension: {}", ext.hook_ids.len());
    println!();

    println!("Debug complete.");

    Ok(())
}
