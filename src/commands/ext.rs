//! Extension management commands
//!
//! Provides CLI commands for managing extensions:
//! - Install, uninstall, list extensions
//! - Enable/disable extensions
//! - Show extension details
//! - Create bundles from extensions
//! - Configure extensions (global, team, agent levels)

use crate::commands::GlobalPaths;
use crate::extensions::adapters::{ExtensionTypeAdapter, ManifestFormat, general_adapter};
use crate::extensions::manager::{ExtensionManager, ExtensionStorage, LoadedExtension};
use crate::extensions::types::ExtensionId;
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
    },

    /// Enable an extension or built-in capability
    ///
    /// Examples:
    ///   pekobot ext enable my-extension
    ///   pekobot ext enable shell --target default
    ///   pekobot ext enable shell --target myteam/my-agent
    Enable {
        /// Extension ID or built-in capability name (e.g., shell, read_file)
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
    ///   pekobot ext config my-extension --global --set api_key=secret
    ///   pekobot ext config my-extension --team myteam --set endpoint=https://api.example.com
    ///   pekobot ext config my-extension --agent myteam/myagent --set timeout=30
    ///   pekobot ext config my-extension --unset api_key
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
}

/// Create an ExtensionManager with all default adapters registered
fn create_manager_with_adapters(storage: Option<ExtensionStorage>) -> ExtensionManager {
    use crate::extensions::adapters::{
        mcp_adapter::McpAdapter, skill_adapter::SkillAdapter,
        universal_tool_adapter::UniversalToolAdapter,
    };
    use crate::extensions::core::{global_core, init_global_core, ExtensionCore};
    use std::sync::Arc;

    // Get or initialize global ExtensionCore for consistency with agent
    let core = global_core().unwrap_or_else(|| {
        let core = Arc::new(ExtensionCore::new());
        init_global_core(core.clone());
        core
    });

    let mut manager = ExtensionManager::with_core(core);
    
    if let Some(storage) = storage {
        manager = manager.with_storage_dir(storage.dir().unwrap().to_path_buf());
    }

    // Register extension type adapters
    // Note: GatewayAdapter requires ExtensionCore and is registered by
    // ExtensionManager when needed.
    manager.register_adapter(Box::new(SkillAdapter::new()));
    manager.register_adapter(Box::new(McpAdapter::with_default_manager()));
    manager.register_adapter(Box::new(UniversalToolAdapter::new()));

    manager
}

/// Handle extension subcommands
pub async fn handle_ext_command(command: ExtCommands, paths: &GlobalPaths) -> anyhow::Result<()> {
    match command {
        ExtCommands::Validate { path, verbose } => {
            handle_validate(path, verbose).await
        }
        ExtCommands::Debug { id } => {
            // Create storage in the data directory
            let storage = ExtensionStorage::with_dir(paths.data_dir.join("extensions"));
            let mut manager = create_manager_with_adapters(Some(storage));
            // Load all extensions to populate the manager
            manager.load_all().await?;
            handle_debug(&manager, id).await
        }
        _ => {
            // Create storage in the data directory
            let storage = ExtensionStorage::with_dir(paths.data_dir.join("extensions"));
            let mut manager = create_manager_with_adapters(Some(storage));

            // Load all extensions to populate the manager
            manager.load_all().await?;

            match command {
                ExtCommands::Install { path, r#type } => handle_install(&mut manager, path, r#type).await,
                ExtCommands::List {
                    enabled_only,
                    r#type,
                } => handle_list(&manager, enabled_only, r#type).await,
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
                // These are handled above
                ExtCommands::Validate { .. } | ExtCommands::Debug { .. } => unreachable!(),
            }
        }
    }
}

/// Handle install command
async fn handle_install(
    manager: &mut ExtensionManager,
    path: PathBuf,
    ext_type: Option<String>,
) -> anyhow::Result<()> {
    println!("Installing extension from: {}", path.display());

    if let Some(ref t) = ext_type {
        println!("   Type: {}", t);
    }

    match manager.install(&path).await {
        Ok(id) => {
            println!("Extension installed successfully");
            println!("   ID: {}", id);
            Ok(())
        }
        Err(e) => {
            eprintln!("Failed to install extension: {}", e);
            Err(e)
        }
    }
}

/// Handle list command
/// 
/// Shows both installed extensions and built-in extensions registered with
/// ExtensionCore. Built-ins are compiled into the binary and always available.
async fn handle_list(
    manager: &ExtensionManager,
    _enabled_only: bool,  // Kept for CLI compatibility, but ignored
    ext_type: Option<String>,
) -> anyhow::Result<()> {
    let extensions = manager.list_extensions();

    // Get built-in extensions from ExtensionCore
    let builtins = if let Some(core) = crate::extensions::core::global_core() {
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
    let filtered_builtins: Vec<&crate::extensions::core::BuiltinExtensionInfo> = builtins
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

    println!("{:<24} {:<14} {:<18} {}", "ID", "TYPE", "NAME", "SOURCE");
    println!("{}", "-".repeat(72));

    for b in &filtered_builtins {
        let status = if b.enabled { "" } else { " [disabled]" };
        println!(
            "{:<24} {:<14} {:<18} {}{}",
            b.id, b.ext_type, b.name, "built-in", status
        );
    }

    for ext in &filtered_installed {
        println!(
            "{:<24} {:<14} {:<18} {}",
            ext.manifest.id, ext.extension_type, ext.manifest.name, "installed"
        );
    }

    println!();
    println!("Total: {} extension(s)", filtered_installed.len() + filtered_builtins.len());
    if !filtered_builtins.is_empty() {
        println!("Note: Built-in extensions are compiled into the binary.");
        println!("      Installed extensions are loaded from disk.");
    }

    Ok(())
}

/// Handle enable command
async fn handle_enable(
    manager: &mut ExtensionManager,
    paths: &GlobalPaths,
    id: String,
    target: Option<String>,
) -> anyhow::Result<()> {
    // Normalize built-in IDs: accept both "shell" and "builtin:tool:shell"
    let is_builtin = crate::tools::builtin_registry::BuiltinRegistry::is_builtin(&id)
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
        let ext = manager.get_extension(&ext_id)
            .ok_or_else(|| anyhow::anyhow!("Extension '{}' not found", id))?;
        (ext.manifest.name.clone(), ext.extension_type.clone())
    };

    match manager.enable(&ext_id).await {
        Ok(()) => {
            println!("Extension '{}' enabled", id);
            
            // Also add to agent tool whitelist for tools that need it (universal-tool and mcp)
            let needs_whitelist = ext_type == "universal-tool" || ext_type == "mcp";
            if needs_whitelist {
                let target = target.as_deref().unwrap_or("default");
                // For MCP tools, use wildcard pattern to match all tools from this server
                let whitelist_entry = if ext_type == "mcp" {
                    format!("mcp:{}:*", tool_name)
                } else {
                    tool_name.clone()
                };
                if let Err(e) = add_tool_to_agent_whitelist(paths, &whitelist_entry, target).await {
                    tracing::warn!("Failed to add tool '{}' to agent whitelist: {}", whitelist_entry, e);
                } else {
                    println!("Added '{}' to agent '{}' tool whitelist", whitelist_entry, target);
                }
            }
            
            Ok(())
        }
        Err(e) => {
            eprintln!("Failed to enable extension '{}': {}", id, e);
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

    if let Some(agent_name) = agent {
        // Agent-level: update specific agent config
        let config_path = paths.resolver().agent_config(&agent_name, Some(&team));
        if !config_path.exists() {
            anyhow::bail!("Agent '{}' not found in team '{}'", agent_name, team);
        }

        update_agent_config_with_tool(&config_path, tool_name)?;
        println!("Added '{}' to agent '{}/{}' tool whitelist", tool_name, team, agent_name);
        tracing::info!("Added '{}' to agent '{}/{}' tool whitelist", tool_name, team, agent_name);
    } else {
        // Team-level: update all agents in the team
        let agents_dir = paths.resolver().agents_dir(Some(&team));
        if !agents_dir.exists() {
            anyhow::bail!("Team '{}' not found (no agents directory)", team);
        }
        
        let mut updated_count = 0;
        
        // Find all agents in the team
        for entry in std::fs::read_dir(&agents_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            
            let agent_name = entry.file_name().to_string_lossy().to_string();
            let config_path = paths.resolver().agent_config(&agent_name, Some(&team));
            
            if config_path.exists() {
                update_agent_config_with_tool(&config_path, tool_name)?;
                updated_count += 1;
            }
        }
        
        if updated_count > 0 {
            println!("Added '{}' to {} agent(s) in team '{}' tool whitelist", tool_name, updated_count, team);
            tracing::info!("Added '{}' to {} agent(s) in team '{}' tool whitelist", tool_name, updated_count, team);
        } else {
            tracing::warn!("No agents found in team '{}' to add tool '{}' to", team, tool_name);
        }
    }
    
    Ok(())
}

/// Helper to update a single agent's config with a tool
fn update_agent_config_with_tool(config_path: &std::path::Path, tool_name: &str) -> anyhow::Result<()> {
    let content = std::fs::read_to_string(config_path)?;
    let mut config: crate::types::agent::AgentConfig = toml::from_str(&content)?;

    // Ensure tools config exists
    let tools = config.tools.get_or_insert_with(Default::default);

    // Add to enabled whitelist if not already present
    if !tools.enabled.iter().any(|e| e.eq_ignore_ascii_case(tool_name)) {
        tools.enabled.push(tool_name.to_string());
    }

    // Save
    let updated = toml::to_string_pretty(&config)?;
    std::fs::write(config_path, updated)?;
    
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

    if let Some(agent_name) = agent {
        // Agent-level: update agent config
        let config_path = paths.resolver().agent_config(&agent_name, Some(&team));
        if !config_path.exists() {
            anyhow::bail!("Agent '{}' not found in team '{}'", agent_name, team);
        }

        let content = std::fs::read_to_string(&config_path)?;
        let mut config: crate::types::agent::AgentConfig = toml::from_str(&content)?;

        // Ensure tools config exists
        let tools = config.tools.get_or_insert_with(Default::default);

        // Add to enabled whitelist if not already present
        if !tools.enabled.iter().any(|e| e.eq_ignore_ascii_case(capability)) {
            tools.enabled.push(capability.to_string());
        }

        // Save
        let updated = toml::to_string_pretty(&config)?;
        std::fs::write(&config_path, updated)?;

        println!("Enabled '{}' for agent '{}' in team '{}'", capability, agent_name, team);
    } else {
        // Team-level: update team extensions config
        let team_dir = paths.data_dir.join("teams").join(&team);
        let ext_config_path = team_dir.join("extensions.toml");
        
        #[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
        struct TeamExtConfig {
            #[serde(default)]
            enabled: Vec<String>,
            #[serde(default)]
            disabled: Vec<String>,
        }
        
        let mut config: TeamExtConfig = if ext_config_path.exists() {
            let content = std::fs::read_to_string(&ext_config_path)?;
            toml::from_str(&content).unwrap_or_default()
        } else {
            TeamExtConfig::default()
        };
        
        // Add to enabled, remove from disabled
        if !config.enabled.iter().any(|e| e.eq_ignore_ascii_case(capability)) {
            config.enabled.push(capability.to_string());
        }
        config.disabled.retain(|e| !e.eq_ignore_ascii_case(capability));
        
        // Save
        std::fs::create_dir_all(&team_dir)?;
        let updated = toml::to_string_pretty(&config)?;
        std::fs::write(&ext_config_path, updated)?;
        
        println!("Enabled '{}' for team '{}'", capability, team);
    }

    // Also enable ExtensionCore hooks for immediate effect
    if let Some(core) = crate::extensions::core::global_core() {
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
    let is_builtin = crate::tools::builtin_registry::BuiltinRegistry::is_builtin(&id)
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
        anyhow::bail!("Extension '{}' not found", id);
    }

    match manager.disable(&ext_id).await {
        Ok(()) => {
            println!("Extension '{}' disabled", id);
            Ok(())
        }
        Err(e) => {
            eprintln!("Failed to disable extension '{}': {}", id, e);
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

    if let Some(agent_name) = agent {
        // Agent-level: update agent config
        let config_path = paths.resolver().agent_config(&agent_name, Some(&team));
        if !config_path.exists() {
            anyhow::bail!("Agent '{}' not found in team '{}'", agent_name, team);
        }

        let content = std::fs::read_to_string(&config_path)?;
        let mut config: crate::types::agent::AgentConfig = toml::from_str(&content)?;

        // Ensure tools config exists
        let tools = config.tools.get_or_insert_with(Default::default);

        // Remove from enabled whitelist
        tools.enabled.retain(|e| !e.eq_ignore_ascii_case(capability));

        // Save
        let updated = toml::to_string_pretty(&config)?;
        std::fs::write(&config_path, updated)?;

        println!("Disabled '{}' for agent '{}' in team '{}'", capability, agent_name, team);
    } else {
        // Team-level: update team extensions config
        let team_dir = paths.data_dir.join("teams").join(&team);
        let ext_config_path = team_dir.join("extensions.toml");
        
        #[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
        struct TeamExtConfig {
            #[serde(default)]
            enabled: Vec<String>,
            #[serde(default)]
            disabled: Vec<String>,
        }
        
        let mut config: TeamExtConfig = if ext_config_path.exists() {
            let content = std::fs::read_to_string(&ext_config_path)?;
            toml::from_str(&content).unwrap_or_default()
        } else {
            TeamExtConfig::default()
        };
        
        // Remove from enabled, add to disabled
        config.enabled.retain(|e| !e.eq_ignore_ascii_case(capability));
        if !config.disabled.iter().any(|e| e.eq_ignore_ascii_case(capability)) {
            config.disabled.push(capability.to_string());
        }
        
        // Save
        std::fs::create_dir_all(&team_dir)?;
        let updated = toml::to_string_pretty(&config)?;
        std::fs::write(&ext_config_path, updated)?;
        
        println!("Disabled '{}' for team '{}'", capability, team);
    }

    // Also disable ExtensionCore hooks for immediate effect
    if let Some(core) = crate::extensions::core::global_core() {
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
        anyhow::bail!("Extension '{}' not found", id);
    }

    println!("Uninstalling extension '{}'...", id);

    match manager.uninstall(&ext_id).await {
        Ok(()) => {
            println!("Extension '{}' uninstalled", id);
            Ok(())
        }
        Err(e) => {
            eprintln!("Failed to uninstall extension '{}': {}", id, e);
            Err(e)
        }
    }
}

/// Handle info command
fn handle_info(manager: &ExtensionManager, id: String) -> anyhow::Result<()> {
    let ext_id = ExtensionId::new(&id);

    let ext = manager
        .get_extension(&ext_id)
        .ok_or_else(|| anyhow::anyhow!("Extension '{}' not found", id))?;

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
    println!("Use 'pekobot ext enable {} --target <team>/<agent>' to enable for a specific agent.", id);

    if !ext.hook_ids.is_empty() {
        println!();
        println!("Registered hooks: {}", ext.hook_ids.len());
    }

    Ok(())
}

/// Handle bundle command
fn handle_bundle(
    manager: &ExtensionManager,
    name: String,
    ids: Vec<String>,
) -> anyhow::Result<()> {
    if ids.is_empty() {
        anyhow::bail!("At least one extension ID is required to create a bundle");
    }

    // Validate all extension IDs exist
    let mut ext_ids = Vec::new();
    for id in &ids {
        let ext_id = ExtensionId::new(id);
        if manager.get_extension(&ext_id).is_none() {
            anyhow::bail!("Extension '{}' not found", id);
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
            eprintln!("Failed to create bundle: {}", e);
            Err(e)
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
        data_dir.join("extensions").join(extension_id).join("config.toml")
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
    
    fn get(&self, team: Option<&str>, agent: Option<&str>, key: &str) -> Option<&serde_json::Value> {
        // Agent scope has highest priority
        if let Some(agent_id) = agent {
            if let Some(agent_config) = self.agents.get(agent_id) {
                if let Some(value) = agent_config.get(key) {
                    return Some(value);
                }
            }
        }
        
        // Team scope has medium priority
        if let Some(team_id) = team {
            if let Some(team_config) = self.teams.get(team_id) {
                if let Some(value) = team_config.get(key) {
                    return Some(value);
                }
            }
        }
        
        // Global scope has lowest priority
        self.global.get(key)
    }
    
    fn set(&mut self, team: Option<&str>, agent: Option<&str>, key: String, value: serde_json::Value) {
        let target = match (team, agent) {
            (Some(_), Some(_)) => {
                let agent_id = agent.unwrap().to_string();
                self.agents.entry(agent_id).or_default()
            }
            (Some(team_id), None) => {
                self.teams.entry(team_id.to_string()).or_default()
            }
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
        (Some(t), Some(a)) => (Some(t.as_str()), Some(format!("{}/{}", t, a))),
        (None, Some(a)) => {
            if a.contains('/') {
                let parts: Vec<&str> = a.split('/').collect();
                (Some(parts[0]), Some(a.clone()))
            } else {
                (Some("default"), Some(format!("default/{}", a)))
            }
        }
        (Some(t), None) => (Some(t.as_str()), None),
        _ => (None, None),
    };
    
    let scope_label = match (&team_id, &agent_id) {
        (Some(t), Some(a)) => format!("agent '{}'", a),
        (Some(t), None) => format!("team '{}'", t),
        _ => "global".to_string(),
    };
    
    // Load or create config
    let mut config = ExtensionConfig::load(&paths.data_dir, &id)?;
    
    // Handle --show (default if no other actions)
    if show || (set_values.is_empty() && unset_keys.is_empty()) {
        println!("Configuration for extension '{}' ({} scope):", id, scope_label);
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
                println!("  {} = {}", key, value);
            }
        }
        
        // Also show inherited values
        if team_id.is_some() || agent_id.is_some() {
            println!();
            println!("Inherited from global:");
            let mut inherited = false;
            for (key, value) in &config.global {
                if !target_config.contains_key(key) {
                    println!("  {} = {} (global)", key, value);
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
            anyhow::bail!("Invalid format '{}'. Use KEY=VALUE", pair);
        }
        let key = parts[0].to_string();
        let value = parts[1];
        
        // Try to parse as JSON, fallback to string
        let json_value = serde_json::from_str(value)
            .unwrap_or_else(|_| serde_json::Value::String(value.to_string()));
        
        config.set(team_id, agent_id.as_deref(), key.clone(), json_value);
        println!("Set {} = {} for extension '{}' at {} scope", key, value, id, scope_label);
    }
    
    // Handle --unset
    for key in &unset_keys {
        if config.unset(team_id, agent_id.as_deref(), key) {
            println!("Unset '{}' for extension '{}' at {} scope", key, id, scope_label);
        } else {
            println!("Key '{}' not found for extension '{}' at {} scope", key, id, scope_label);
        }
    }
    
    // Save config
    config.save(&paths.data_dir, &id)?;
    
    Ok(())
}

/// Handle validate command
async fn handle_validate(path: PathBuf, verbose: bool) -> anyhow::Result<()> {
    use crate::extensions::adapters::{
        skill_adapter::SkillAdapter,
        universal_tool_adapter::UniversalToolAdapter,
        mcp_adapter::McpAdapter,
        general_adapter::GeneralExtensionAdapter,
    };

    println!("Validating extension at: {}", path.display());
    println!();

    if !path.exists() {
        anyhow::bail!("Path does not exist: {}", path.display());
    }

    // Try to detect extension type
    let mut detected = false;
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    // Check for Skill (SKILL.md)
    let skill_adapter = SkillAdapter::new();
    if skill_adapter.manifest_format().detect(&path) {
        detected = true;
        if verbose {
            println!("✓ Detected as: skill extension (SKILL.md)");
        }
        
        let skills = skill_adapter.discover_skills(&path);
        if skills.is_empty() {
            errors.push("No valid skills found in directory".to_string());
        } else {
            if verbose {
                for skill in &skills {
                    println!("  ✓ Skill: {} - {}", skill.manifest.name, skill.manifest.description);
                }
            }
            
            // Validate hook resolution
            for skill in &skills {
                let bindings = skill_adapter.resolve_hooks(&skill.manifest);
                if bindings.is_empty() {
                    warnings.push(format!("Skill '{}' has no hook bindings", skill.manifest.name));
                } else if verbose {
                    println!("  ✓ Resolved {} hook(s)", bindings.len());
                }
            }
        }
    }

    // Check for Universal Tool (manifest.json)
    let universal_adapter = UniversalToolAdapter::new();
    if universal_adapter.manifest_format().detect(&path) {
        detected = true;
        if verbose {
            println!("✓ Detected as: universal tool extension (manifest.json)");
        }
        
        let tools = universal_adapter.discover_tools(&path).await;
        if tools.is_empty() {
            errors.push("No valid tools found in directory".to_string());
        } else {
            if verbose {
                for tool in &tools {
                    println!("  ✓ Tool: {} - {}", tool.manifest.name, tool.manifest.description);
                }
            }
            
            // Validate hook resolution
            for tool in &tools {
                let bindings = universal_adapter.resolve_hooks(&tool.manifest);
                if bindings.is_empty() {
                    warnings.push(format!("Tool '{}' has no hook bindings", tool.manifest.name));
                } else if verbose {
                    println!("  ✓ Resolved {} hook(s)", bindings.len());
                }
            }
        }
    }

    // Check for MCP Server (config.toml/config.json)
    let mcp_adapter = McpAdapter::with_default_manager();
    if mcp_adapter.manifest_format().detect(&path) {
        detected = true;
        if verbose {
            println!("✓ Detected as: MCP server extension");
        }
        
        let servers = mcp_adapter.discover_servers(&path).await;
        if servers.is_empty() {
            errors.push("No valid MCP servers found in directory".to_string());
        } else {
            if verbose {
                for server in &servers {
                    println!("  ✓ Server: {}", server.manifest.name);
                }
            }
            
            // Validate hook resolution
            for server in &servers {
                let bindings = mcp_adapter.resolve_hooks(&server.manifest);
                if bindings.is_empty() {
                    warnings.push(format!("Server '{}' has no hook bindings", server.manifest.name));
                } else if verbose {
                    println!("  ✓ Resolved {} hook(s)", bindings.len());
                }
            }
        }
    }

    // Check for General Extension (manifest.yaml with hooks)
    let general_adapter = GeneralExtensionAdapter::new();
    if general_adapter.manifest_format().detect(&path) {
        detected = true;
        if verbose {
            println!("✓ Detected as: general extension");
        }
        
        let extensions = general_adapter::discover_general_extensions(&path).await?;
        if extensions.is_empty() {
            errors.push("No valid general extensions found in directory".to_string());
        } else {
            for ext in &extensions {
                if verbose {
                    println!("  ✓ Extension: {} - {}", ext.manifest.name, ext.manifest.description);
                    println!("    Hooks declared: {}", ext.hooks.len());
                }
                
                // Validate each hook declaration
                for hook in &ext.hooks {
                    // Check if the hook point is valid
                    let valid_points = [
                        "prompt.system_section", "prompt.pre_process", "prompt.post_process",
                        "tool.register", "tool.execute", "tool.execute_async",
                        "tool.check_status", "tool.cancel", "tool.result_transform",
                        "session.state_change", "session.compaction", "session.context_build",
                        "io.channel_input", "io.channel_output", 
                        "io.message_pre_send", "io.message_post_receive",
                        "event.subscribe", "event.emit",
                        "agent.init", "agent.shutdown", "agent.iteration",
                    ];
                    
                    if !valid_points.contains(&hook.point.as_str()) {
                        warnings.push(format!(
                            "Extension '{}' has unknown hook point: {}",
                            ext.manifest.name, hook.point
                        ));
                    } else if verbose {
                        println!("    ✓ Hook: {} -> {}", hook.point, hook.handler);
                    }
                }
                
                // Validate hook resolution
                let bindings = general_adapter.resolve_hooks(&ext.manifest);
                if bindings.is_empty() {
                    warnings.push(format!("Extension '{}' has no valid hook bindings", ext.manifest.name));
                } else if verbose {
                    println!("  ✓ Resolved {} hook binding(s)", bindings.len());
                }
            }
        }
    }

    println!();

    if !detected {
        anyhow::bail!(
            "Could not detect extension type. Expected one of:\n\
             - SKILL.md (skill extension)\n\
             - manifest.json (universal tool)\n\
             - config.toml/config.json (MCP server)\n\
             - manifest.yaml with hooks (general extension)"
        );
    }

    // Print summary
    if errors.is_empty() && warnings.is_empty() {
        println!("✓ Validation passed! Extension is valid and ready to install.");
    } else if errors.is_empty() {
        println!("⚠ Validation passed with warnings:");
        for warning in &warnings {
            println!("  ⚠ {}", warning);
        }
    } else {
        println!("✗ Validation failed with errors:");
        for error in &errors {
            println!("  ✗ {}", error);
        }
        if !warnings.is_empty() {
            println!();
            println!("Additional warnings:");
            for warning in &warnings {
                println!("  ⚠ {}", warning);
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
        .ok_or_else(|| anyhow::anyhow!("Extension '{}' not found", id))?;

    println!("Debug Information for Extension: {}", id);
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
                println!("  Hook ID:     {}", hook_id);
                println!("    Point:     {}", hook.point.name());
                println!("    Category:  {}", hook.point.category());
                println!("    Priority:  {}", hook.priority);
                println!("    Enabled:   {}", hook.enabled);
            } else {
                println!();
                println!("  Hook ID:     {}", hook_id);
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
            let value_str = format!("{}", value);
            let display_value = if value_str.len() > 100 {
                format!("{}... (truncated)", &value_str[..100])
            } else {
                value_str
            };
            println!("  {}: {}", key, display_value);
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
