//! Extension management commands
//!
//! Provides CLI commands for managing extensions:
//! - Install, uninstall, list extensions
//! - Enable/disable extensions
//! - Show extension details
//! - Create bundles from extensions
//! - Configure extensions (global, team, agent levels)

use crate::commands::GlobalPaths;
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

    /// Enable an extension
    Enable {
        /// Extension ID
        id: String,
    },

    /// Disable an extension
    Disable {
        /// Extension ID
        id: String,
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
}

/// Create an ExtensionManager with all default adapters registered
fn create_manager_with_adapters(storage: Option<ExtensionStorage>) -> ExtensionManager {
    use crate::extensions::adapters::{
        mcp_adapter::McpAdapter, skill_adapter::SkillAdapter,
        universal_tool_adapter::UniversalToolAdapter,
    };

    let mut manager = if let Some(storage) = storage {
        ExtensionManager::with_storage(storage)
    } else {
        ExtensionManager::new()
    };

    // Register extension type adapters that don't require ExtensionCore
    // Note: ChannelAdapter, HookAdapter, and GatewayAdapter require ExtensionCore
    // and are typically used internally. They can be registered when needed.
    manager.register_adapter(Box::new(SkillAdapter::new()));
    manager.register_adapter(Box::new(McpAdapter::with_default_manager()));
    manager.register_adapter(Box::new(UniversalToolAdapter::new()));

    manager
}

/// Handle extension subcommands
pub async fn handle_ext_command(command: ExtCommands, paths: &GlobalPaths) -> anyhow::Result<()> {
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
        } => handle_list(&manager, enabled_only, r#type),
        ExtCommands::Enable { id } => handle_enable(&mut manager, id).await,
        ExtCommands::Disable { id } => handle_disable(&mut manager, id).await,
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
fn handle_list(
    manager: &ExtensionManager,
    enabled_only: bool,
    ext_type: Option<String>,
) -> anyhow::Result<()> {
    let extensions = manager.list_extensions();

    if extensions.is_empty() {
        println!("No extensions installed.");
        println!("Use 'pekobot ext install <path>' to install an extension.");
        return Ok(());
    }

    // Filter extensions based on criteria
    let filtered: Vec<&LoadedExtension> = extensions
        .into_iter()
        .filter(|ext| {
            if enabled_only && !ext.enabled {
                return false;
            }
            if let Some(ref t) = ext_type {
                if &ext.extension_type != t {
                    return false;
                }
            }
            true
        })
        .collect();

    if filtered.is_empty() {
        println!("No extensions match the specified criteria.");
        return Ok(());
    }

    println!("Installed Extensions:");
    println!();
    println!("{:<20} {:<12} {:<8} {}", "ID", "TYPE", "STATUS", "NAME");
    println!("{}", "-".repeat(60));

    for ext in &filtered {
        let status = if ext.enabled { "enabled" } else { "disabled" };
        println!(
            "{:<20} {:<12} {:<8} {}",
            ext.manifest.id, ext.extension_type, status, ext.manifest.name
        );
    }

    println!();
    println!("Total: {} extension(s)", filtered.len());

    Ok(())
}

/// Handle enable command
async fn handle_enable(manager: &mut ExtensionManager, id: String) -> anyhow::Result<()> {
    let ext_id = ExtensionId::new(&id);

    // Check if extension exists
    if manager.get_extension(&ext_id).is_none() {
        anyhow::bail!("Extension '{}' not found", id);
    }

    match manager.enable(&ext_id).await {
        Ok(()) => {
            println!("Extension '{}' enabled", id);
            Ok(())
        }
        Err(e) => {
            eprintln!("Failed to enable extension '{}': {}", id, e);
            Err(e)
        }
    }
}

/// Handle disable command
async fn handle_disable(manager: &mut ExtensionManager, id: String) -> anyhow::Result<()> {
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
    println!("Status:      {}", if ext.enabled { "enabled" } else { "disabled" });
    println!("Description: {}", ext.manifest.description);
    println!("Path:        {}", ext.path.display());

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
