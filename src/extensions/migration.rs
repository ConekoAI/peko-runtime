//! Extension Migration Module
//!
//! Handles auto-migration of legacy extensions to the new Extension 2.0 system.
//! This module provides idempotent migration functionality that safely transforms
//! legacy skills, MCP servers, and universal tools into modern Extension format.
//!
//! # Migration State
//!
//! Migration progress is tracked in `<data_dir>/extensions/migration-state.json`.
//! This file stores flags indicating which migration steps have been completed,
//! making the migration safe to run multiple times.
//!
//! # Legacy Sources
//!
//! - **Skills**: `~/.pekobot/skills/` - Markdown files with YAML frontmatter
//! - **MCP Servers**: `~/.pekobot/mcp.toml` - TOML configuration file
//! - **Universal Tools**: `~/.pekobot/tools/` - JSON manifest + executable
//!
//! # Example Usage
//!
//! ```rust,ignore
//! use pekobot::extensions::migration::migrate_legacy_extensions;
//! use pekobot::extensions::manager::ExtensionManager;
//!
//! let mut manager = ExtensionManager::new();
//! let report = migrate_legacy_extensions(&mut manager).await?;
//!
//! println!("Migrated {} skills", report.skills_migrated.len());
//! println!("Migrated {} MCP servers", report.mcp_servers_migrated.len());
//! println!("Migrated {} tools", report.tools_migrated.len());
//! ```

use crate::common::paths::default_data_dir;
use crate::extensions::adapters::parsing;
use crate::extensions::manager::ExtensionManager;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

// =============================================================================
// Migration Report
// =============================================================================

/// Report of what was migrated during the migration process
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MigrationReport {
    /// List of skill names that were successfully migrated
    pub skills_migrated: Vec<String>,
    /// List of MCP server names that were successfully migrated
    pub mcp_servers_migrated: Vec<String>,
    /// List of universal tool names that were successfully migrated
    pub tools_migrated: Vec<String>,
    /// List of errors that occurred during migration (item name, error message)
    pub errors: Vec<(String, String)>,
}

impl MigrationReport {
    /// Create a new empty migration report
    pub fn new() -> Self {
        Self::default()
    }

    /// Check if the migration was completely successful
    pub fn is_success(&self) -> bool {
        self.errors.is_empty()
    }

    /// Get the total number of successfully migrated items
    pub fn total_migrated(&self) -> usize {
        self.skills_migrated.len()
            + self.mcp_servers_migrated.len()
            + self.tools_migrated.len()
    }

    /// Add a successfully migrated skill
    pub fn add_skill(&mut self, name: String) {
        self.skills_migrated.push(name);
    }

    /// Add a successfully migrated MCP server
    pub fn add_mcp_server(&mut self, name: String) {
        self.mcp_servers_migrated.push(name);
    }

    /// Add a successfully migrated tool
    pub fn add_tool(&mut self, name: String) {
        self.tools_migrated.push(name);
    }

    /// Add an error that occurred during migration
    pub fn add_error(&mut self, item: String, error: String) {
        self.errors.push((item, error));
    }

    /// Merge another report into this one
    pub fn merge(&mut self, other: MigrationReport) {
        self.skills_migrated.extend(other.skills_migrated);
        self.mcp_servers_migrated.extend(other.mcp_servers_migrated);
        self.tools_migrated.extend(other.tools_migrated);
        self.errors.extend(other.errors);
    }
}

// =============================================================================
// Migration State Storage
// =============================================================================

/// Migration state stored in the JSON file
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct MigrationState {
    /// Whether the initial migration has been completed
    #[serde(default)]
    pub migration_completed: bool,
    /// Timestamp of when migration was completed
    #[serde(default)]
    pub completed_at: Option<String>,
    /// Version of the migration system
    #[serde(default = "default_migration_version")]
    pub version: String,
    /// Individual item migration tracking (for granular idempotency)
    #[serde(default)]
    pub migrated_items: HashMap<String, bool>,
}

fn default_migration_version() -> String {
    "1.0.0".to_string()
}

/// Get the path to the migration state file
///
/// Returns: `<data_dir>/extensions/migration-state.json`
pub fn migration_state_path() -> PathBuf {
    default_data_dir()
        .join("extensions")
        .join("migration-state.json")
}

/// Ensure the extensions directory exists
async fn ensure_extensions_dir() -> Result<()> {
    let ext_dir = default_data_dir().join("extensions");
    if !ext_dir.exists() {
        tokio::fs::create_dir_all(&ext_dir)
            .await
            .with_context(|| format!("Failed to create extensions directory: {:?}", ext_dir))?;
    }
    Ok(())
}

/// Check if migration has already been completed
///
/// Reads the migration state file and returns true if the migration flag is set.
/// Returns false if the state file doesn't exist or migration hasn't been completed.
pub async fn is_migration_completed() -> Result<bool> {
    let state_path = migration_state_path();

    if !state_path.exists() {
        return Ok(false);
    }

    let content = tokio::fs::read_to_string(&state_path)
        .await
        .with_context(|| format!("Failed to read migration state from {:?}", state_path))?;

    let state: MigrationState = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse migration state from {:?}", state_path))?;

    Ok(state.migration_completed)
}

/// Check if a specific item has been migrated
///
/// This provides granular idempotency for individual migration items.
pub async fn is_item_migrated(item_id: &str) -> Result<bool> {
    let state_path = migration_state_path();

    if !state_path.exists() {
        return Ok(false);
    }

    let content = tokio::fs::read_to_string(&state_path)
        .await
        .with_context(|| format!("Failed to read migration state from {:?}", state_path))?;

    let state: MigrationState = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse migration state from {:?}", state_path))?;

    Ok(state.migrated_items.get(item_id).copied().unwrap_or(false))
}

/// Set the migration completed flag
///
/// Creates or updates the migration state file to mark migration as complete.
pub async fn set_migration_completed() -> Result<()> {
    ensure_extensions_dir().await?;

    let state_path = migration_state_path();

    // Load existing state or create new
    let mut state = if state_path.exists() {
        let content = tokio::fs::read_to_string(&state_path)
            .await
            .with_context(|| format!("Failed to read migration state from {:?}", state_path))?;
        serde_json::from_str(&content).unwrap_or_default()
    } else {
        MigrationState::default()
    };

    state.migration_completed = true;
    state.completed_at = Some(chrono::Utc::now().to_rfc3339());

    let content = serde_json::to_string_pretty(&state)
        .context("Failed to serialize migration state")?;

    tokio::fs::write(&state_path, content)
        .await
        .with_context(|| format!("Failed to write migration state to {:?}", state_path))?;

    info!("Migration state marked as completed");
    Ok(())
}

/// Mark a specific item as migrated
///
/// This provides granular tracking of migrated items for idempotency.
pub async fn set_item_migrated(item_id: &str) -> Result<()> {
    ensure_extensions_dir().await?;

    let state_path = migration_state_path();

    // Load existing state or create new
    let mut state = if state_path.exists() {
        let content = tokio::fs::read_to_string(&state_path)
            .await
            .with_context(|| format!("Failed to read migration state from {:?}", state_path))?;
        serde_json::from_str(&content).unwrap_or_default()
    } else {
        MigrationState::default()
    };

    state.migrated_items.insert(item_id.to_string(), true);

    let content = serde_json::to_string_pretty(&state)
        .context("Failed to serialize migration state")?;

    tokio::fs::write(&state_path, content)
        .await
        .with_context(|| format!("Failed to write migration state to {:?}", state_path))?;

    debug!("Marked item '{}' as migrated", item_id);
    Ok(())
}

/// Reset migration state (for testing or re-migration)
///
/// Clears all migration flags. Use with caution.
pub async fn reset_migration_state() -> Result<()> {
    let state_path = migration_state_path();

    if state_path.exists() {
        tokio::fs::remove_file(&state_path)
            .await
            .with_context(|| format!("Failed to remove migration state file: {:?}", state_path))?;
        info!("Migration state reset");
    }

    Ok(())
}

// =============================================================================
// Legacy Discovery Functions
// =============================================================================

/// Legacy skill information
#[derive(Debug, Clone)]
pub struct LegacySkill {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    pub tags: Vec<String>,
    pub author: Option<String>,
}

/// Legacy MCP server information
#[derive(Debug, Clone)]
pub struct LegacyMcpServer {
    pub name: String,
    pub config: crate::mcp::config::McpServerConfig,
}

/// Legacy universal tool information
#[derive(Debug, Clone)]
pub struct LegacyUniversalTool {
    pub name: String,
    pub path: PathBuf,
    pub manifest_path: PathBuf,
}

/// Discover legacy skills in the skills directory
///
/// Scans `~/.pekobot/skills/` for directories containing `SKILL.md` files.
/// Returns a list of legacy skills found.
pub async fn discover_legacy_skills() -> Result<Vec<LegacySkill>> {
    let skills_dir = dirs::home_dir()
        .map(|d| d.join(".pekobot").join("skills"))
        .unwrap_or_else(|| PathBuf::from(".pekobot").join("skills"));

    let mut skills = Vec::new();

    if !skills_dir.exists() {
        debug!("Legacy skills directory does not exist: {:?}", skills_dir);
        return Ok(skills);
    }

    debug!("Scanning for legacy skills in {:?}", skills_dir);

    let mut entries = tokio::fs::read_dir(&skills_dir)
        .await
        .with_context(|| format!("Failed to read skills directory: {:?}", skills_dir))?;

    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();

        // Skip non-directories and hidden entries
        if !path.is_dir() {
            continue;
        }

        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with('.') {
            continue;
        }

        // Check for SKILL.md
        let skill_md = path.join("SKILL.md");
        if !skill_md.exists() {
            debug!("No SKILL.md found in {:?}, skipping", path);
            continue;
        }

        // Parse the SKILL.md file to extract metadata
        match parse_skill_md(&skill_md).await {
            Ok((name, description, tags, author)) => {
                debug!("Discovered legacy skill: {} at {:?}", name, path);
                skills.push(LegacySkill {
                    name,
                    description,
                    path: path.clone(),
                    tags,
                    author,
                });
            }
            Err(e) => {
                warn!("Failed to parse SKILL.md at {:?}: {}", skill_md, e);
            }
        }
    }

    info!("Discovered {} legacy skills", skills.len());
    Ok(skills)
}

/// Parse a SKILL.md file to extract metadata from YAML frontmatter
async fn parse_skill_md(
    path: &Path,
) -> Result<(String, String, Vec<String>, Option<String>)> {
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct SkillFrontmatter {
        name: String,
        description: String,
        #[serde(default)]
        tags: Vec<String>,
        #[serde(default)]
        author: Option<String>,
    }

    let (meta, _): (SkillFrontmatter, _) = parsing::parse_yaml_frontmatter_file(path).await
        .with_context(|| format!("Failed to parse SKILL.md at {:?}", path))?;

    if meta.name.is_empty() {
        anyhow::bail!("Skill name cannot be empty");
    }
    if meta.description.is_empty() {
        anyhow::bail!("Skill description cannot be empty");
    }

    Ok((meta.name, meta.description, meta.tags, meta.author))
}

/// Discover legacy MCP servers from mcp.toml
///
/// Reads the MCP configuration file at `~/.pekobot/mcp.toml` and returns
/// a list of configured MCP servers.
pub async fn discover_legacy_mcp_servers() -> Result<Vec<LegacyMcpServer>> {
    let mcp_toml = dirs::home_dir()
        .map(|d| d.join(".pekobot").join("mcp.toml"))
        .unwrap_or_else(|| PathBuf::from(".pekobot").join("mcp.toml"));

    let mut servers = Vec::new();

    if !mcp_toml.exists() {
        debug!("Legacy MCP config file does not exist: {:?}", mcp_toml);
        return Ok(servers);
    }

    debug!("Reading legacy MCP config from {:?}", mcp_toml);

    let content = tokio::fs::read_to_string(&mcp_toml)
        .await
        .with_context(|| format!("Failed to read MCP config from {:?}", mcp_toml))?;

    let config: crate::mcp::config::McpConfig = toml::from_str(&content)
        .with_context(|| format!("Failed to parse MCP config from {:?}", mcp_toml))?;

    for server_config in config.servers {
        let name = server_config.name.clone();
        debug!("Discovered legacy MCP server: {}", name);
        servers.push(LegacyMcpServer {
            name,
            config: server_config,
        });
    }

    info!("Discovered {} legacy MCP servers", servers.len());
    Ok(servers)
}

/// Discover legacy universal tools in the tools directory
///
/// Scans `~/.pekobot/tools/` for directories containing `manifest.json` files.
/// Returns a list of legacy universal tools found.
pub async fn discover_legacy_universal_tools() -> Result<Vec<LegacyUniversalTool>> {
    let tools_dir = dirs::home_dir()
        .map(|d| d.join(".pekobot").join("tools"))
        .unwrap_or_else(|| PathBuf::from(".pekobot").join("tools"));

    let mut tools = Vec::new();

    if !tools_dir.exists() {
        debug!("Legacy tools directory does not exist: {:?}", tools_dir);
        return Ok(tools);
    }

    debug!("Scanning for legacy universal tools in {:?}", tools_dir);

    let mut entries = tokio::fs::read_dir(&tools_dir)
        .await
        .with_context(|| format!("Failed to read tools directory: {:?}", tools_dir))?;

    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();

        // Skip non-directories and hidden entries
        if !path.is_dir() {
            continue;
        }

        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with('.') {
            continue;
        }

        // Check for manifest.json
        let manifest_path = path.join("manifest.json");
        if !manifest_path.exists() {
            debug!("No manifest.json found in {:?}, skipping", path);
            continue;
        }

        // Parse manifest to get the actual tool name
        match parse_tool_manifest(&manifest_path).await {
            Ok(tool_name) => {
                debug!("Discovered legacy universal tool: {} at {:?}", tool_name, path);
                tools.push(LegacyUniversalTool {
                    name: tool_name,
                    path: path.clone(),
                    manifest_path,
                });
            }
            Err(e) => {
                warn!("Failed to parse tool manifest at {:?}: {}", manifest_path, e);
            }
        }
    }

    info!("Discovered {} legacy universal tools", tools.len());
    Ok(tools)
}

/// Parse a tool manifest.json to extract the tool name
async fn parse_tool_manifest(path: &Path) -> Result<String> {
    let content = tokio::fs::read_to_string(path)
        .await
        .with_context(|| format!("Failed to read manifest at {:?}", path))?;

    #[derive(serde::Deserialize)]
    struct ToolManifest {
        name: String,
    }

    let manifest: ToolManifest = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse manifest JSON at {:?}", path))?;

    if manifest.name.is_empty() {
        anyhow::bail!("Tool name cannot be empty");
    }

    Ok(manifest.name)
}

// =============================================================================
// Migration Functions
// =============================================================================

/// Migrate a single legacy skill to Extension 2.0 format
async fn migrate_skill(
    manager: &mut ExtensionManager,
    skill: &LegacySkill,
) -> Result<String> {
    let item_id = format!("skill:{}", skill.name);

    // Check if already migrated
    if is_item_migrated(&item_id).await? {
        debug!("Skill '{}' already migrated, skipping", skill.name);
        return Ok(skill.name.clone());
    }

    // The skill directory is already in the correct format for Extension 2.0
    // We just need to install it using the ExtensionManager
    info!("Migrating skill '{}' from {:?}", skill.name, skill.path);

    let extension_id = manager
        .install(&skill.path)
        .await
        .with_context(|| format!("Failed to install skill '{}'", skill.name))?;

    // Mark as migrated
    set_item_migrated(&item_id).await?;

    info!("Successfully migrated skill '{}' as extension '{}'", skill.name, extension_id);
    Ok(skill.name.clone())
}

/// Migrate a single legacy MCP server to Extension 2.0 format
async fn migrate_mcp_server(
    manager: &mut ExtensionManager,
    server: &LegacyMcpServer,
) -> Result<String> {
    let item_id = format!("mcp:{}", server.name);

    // Check if already migrated
    if is_item_migrated(&item_id).await? {
        debug!("MCP server '{}' already migrated, skipping", server.name);
        return Ok(server.name.clone());
    }

    // Create a temporary directory with the MCP server as an extension
    let temp_dir = create_mcp_extension_dir(server).await?;

    info!("Migrating MCP server '{}'", server.name);

    let extension_id = manager
        .install(&temp_dir)
        .await
        .with_context(|| format!("Failed to install MCP server '{}'", server.name))?;

    // Mark as migrated
    set_item_migrated(&item_id).await?;

    // Clean up temp directory
    let _ = tokio::fs::remove_dir_all(&temp_dir).await;

    info!("Successfully migrated MCP server '{}' as extension '{}'", server.name, extension_id);
    Ok(server.name.clone())
}

/// Create a temporary extension directory for an MCP server
async fn create_mcp_extension_dir(server: &LegacyMcpServer) -> Result<PathBuf> {
    let temp_dir = std::env::temp_dir().join(format!("pekobot_mcp_migration_{}", server.name));

    // Remove if exists from previous failed attempt
    if temp_dir.exists() {
        tokio::fs::remove_dir_all(&temp_dir).await?;
    }

    tokio::fs::create_dir_all(&temp_dir)
        .await
        .with_context(|| format!("Failed to create temp directory: {:?}", temp_dir))?;

    // Create extension manifest
    let manifest = serde_json::json!({
        "id": format!("mcp-{}", server.name),
        "extension_type": "mcp",
        "name": server.name.clone(),
        "description": format!("MCP server: {}", server.name),
        "version": "1.0.0",
        "mcp_config": {
            "name": server.config.name,
            "transport": match server.config.transport {
                crate::mcp::config::TransportType::Stdio => "stdio",
                crate::mcp::config::TransportType::Sse => "sse",
            },
            "command": server.config.command,
            "args": server.config.args,
            "env": server.config.env,
            "endpoint": server.config.endpoint,
            "auto_start": server.config.auto_start,
        }
    });

    let manifest_path = temp_dir.join("manifest.json");
    tokio::fs::write(&manifest_path, serde_json::to_string_pretty(&manifest)?)
        .await
        .with_context(|| format!("Failed to write manifest to {:?}", manifest_path))?;

    // Create a marker file for the MCP configuration source
    let config_marker = temp_dir.join(".mcp-source");
    tokio::fs::write(&config_marker, "migrated from mcp.toml")
        .await
        .with_context(|| format!("Failed to write config marker to {:?}", config_marker))?;

    Ok(temp_dir)
}

/// Migrate a single legacy universal tool to Extension 2.0 format
async fn migrate_universal_tool(
    manager: &mut ExtensionManager,
    tool: &LegacyUniversalTool,
) -> Result<String> {
    let item_id = format!("tool:{}", tool.name);

    // Check if already migrated
    if is_item_migrated(&item_id).await? {
        debug!("Universal tool '{}' already migrated, skipping", tool.name);
        return Ok(tool.name.clone());
    }

    info!("Migrating universal tool '{}' from {:?}", tool.name, tool.path);

    let extension_id = manager
        .install(&tool.path)
        .await
        .with_context(|| format!("Failed to install universal tool '{}'", tool.name))?;

    // Mark as migrated
    set_item_migrated(&item_id).await?;

    info!("Successfully migrated universal tool '{}' as extension '{}'", tool.name, extension_id);
    Ok(tool.name.clone())
}

// =============================================================================
// Main Migration Function
// =============================================================================

/// Migrate all legacy extensions to Extension 2.0 format
///
/// This is the main entry point for extension migration. It:
/// 1. Checks if migration has already been completed (idempotent)
/// 2. Discovers all legacy extensions (skills, MCP servers, tools)
/// 3. Migrates each one using the ExtensionManager
/// 4. Sets the migration flag when complete
/// 5. Returns a report of what was migrated
///
/// # Arguments
/// * `manager` - The ExtensionManager to use for installing migrated extensions
///
/// # Returns
/// * `MigrationReport` - A report of what was migrated and any errors that occurred
///
/// # Example
/// ```rust,ignore
/// let mut manager = ExtensionManager::new();
/// let report = migrate_legacy_extensions(&mut manager).await?;
///
/// if report.is_success() {
///     println!("Migration completed successfully!");
/// } else {
///     println!("Migration completed with {} errors", report.errors.len());
/// }
/// ```
pub async fn migrate_legacy_extensions(manager: &mut ExtensionManager) -> Result<MigrationReport> {
    info!("Starting legacy extension migration");

    // Check if migration already completed
    if is_migration_completed().await? {
        info!("Migration already completed, skipping");
        return Ok(MigrationReport::new());
    }

    let mut report = MigrationReport::new();

    // =============================================================================
    // Discover Legacy Extensions
    // =============================================================================

    let legacy_skills = discover_legacy_skills().await.unwrap_or_else(|e| {
        warn!("Failed to discover legacy skills: {}", e);
        report.add_error("discover_skills".to_string(), e.to_string());
        Vec::new()
    });

    let legacy_mcp_servers = discover_legacy_mcp_servers().await.unwrap_or_else(|e| {
        warn!("Failed to discover legacy MCP servers: {}", e);
        report.add_error("discover_mcp_servers".to_string(), e.to_string());
        Vec::new()
    });

    let legacy_tools = discover_legacy_universal_tools().await.unwrap_or_else(|e| {
        warn!("Failed to discover legacy universal tools: {}", e);
        report.add_error("discover_tools".to_string(), e.to_string());
        Vec::new()
    });

    info!(
        "Discovered: {} skills, {} MCP servers, {} tools",
        legacy_skills.len(),
        legacy_mcp_servers.len(),
        legacy_tools.len()
    );

    // =============================================================================
    // Migrate Skills
    // =============================================================================

    for skill in &legacy_skills {
        match migrate_skill(manager, skill).await {
            Ok(name) => {
                report.add_skill(name);
            }
            Err(e) => {
                let err_msg = format!("Failed to migrate skill '{}': {}", skill.name, e);
                warn!("{}", err_msg);
                report.add_error(skill.name.clone(), err_msg);
            }
        }
    }

    // =============================================================================
    // Migrate MCP Servers
    // =============================================================================

    for server in &legacy_mcp_servers {
        match migrate_mcp_server(manager, server).await {
            Ok(name) => {
                report.add_mcp_server(name);
            }
            Err(e) => {
                let err_msg = format!("Failed to migrate MCP server '{}': {}", server.name, e);
                warn!("{}", err_msg);
                report.add_error(server.name.clone(), err_msg);
            }
        }
    }

    // =============================================================================
    // Migrate Universal Tools
    // =============================================================================

    for tool in &legacy_tools {
        match migrate_universal_tool(manager, tool).await {
            Ok(name) => {
                report.add_tool(name);
            }
            Err(e) => {
                let err_msg = format!("Failed to migrate tool '{}': {}", tool.name, e);
                warn!("{}", err_msg);
                report.add_error(tool.name.clone(), err_msg);
            }
        }
    }

    // =============================================================================
    // Finalize Migration
    // =============================================================================

    // Set migration flag
    set_migration_completed().await?;

    info!(
        "Migration completed: {} skills, {} MCP servers, {} tools, {} errors",
        report.skills_migrated.len(),
        report.mcp_servers_migrated.len(),
        report.tools_migrated.len(),
        report.errors.len()
    );

    Ok(report)
}

/// Migrate legacy extensions with options
///
/// Extended version that allows for more control over the migration process.
#[derive(Debug, Clone)]
pub struct MigrationOptions {
    /// Force migration even if already completed
    pub force: bool,
    /// Only migrate specific types
    pub types: Option<Vec<MigrationType>>,
    /// Skip specific items by name
    pub skip_items: Vec<String>,
}

impl Default for MigrationOptions {
    fn default() -> Self {
        Self {
            force: false,
            types: None,
            skip_items: Vec::new(),
        }
    }
}

/// Types of extensions that can be migrated
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationType {
    Skills,
    McpServers,
    UniversalTools,
}

/// Migrate legacy extensions with custom options
pub async fn migrate_legacy_extensions_with_options(
    manager: &mut ExtensionManager,
    options: MigrationOptions,
) -> Result<MigrationReport> {
    if options.force {
        reset_migration_state().await?;
    }

    // Check if migration already completed
    if !options.force && is_migration_completed().await? {
        info!("Migration already completed, skipping (use force=true to override)");
        return Ok(MigrationReport::new());
    }

    let mut report = MigrationReport::new();
    let has_type_filter = options.types.is_some();
    let types = options.types.unwrap_or_else(|| {
        vec![
            MigrationType::Skills,
            MigrationType::McpServers,
            MigrationType::UniversalTools,
        ]
    });

    // Migrate skills if enabled
    if types.contains(&MigrationType::Skills) {
        let skills = discover_legacy_skills().await?;
        for skill in skills {
            if options.skip_items.contains(&skill.name) {
                debug!("Skipping skill '{}' (in skip list)", skill.name);
                continue;
            }
            match migrate_skill(manager, &skill).await {
                Ok(name) => report.add_skill(name),
                Err(e) => report.add_error(skill.name, e.to_string()),
            }
        }
    }

    // Migrate MCP servers if enabled
    if types.contains(&MigrationType::McpServers) {
        let servers = discover_legacy_mcp_servers().await?;
        for server in servers {
            if options.skip_items.contains(&server.name) {
                debug!("Skipping MCP server '{}' (in skip list)", server.name);
                continue;
            }
            match migrate_mcp_server(manager, &server).await {
                Ok(name) => report.add_mcp_server(name),
                Err(e) => report.add_error(server.name, e.to_string()),
            }
        }
    }

    // Migrate universal tools if enabled
    if types.contains(&MigrationType::UniversalTools) {
        let tools = discover_legacy_universal_tools().await?;
        for tool in tools {
            if options.skip_items.contains(&tool.name) {
                debug!("Skipping tool '{}' (in skip list)", tool.name);
                continue;
            }
            match migrate_universal_tool(manager, &tool).await {
                Ok(name) => report.add_tool(name),
                Err(e) => report.add_error(tool.name, e.to_string()),
            }
        }
    }

    // Set migration flag unless we're in partial mode
    if !has_type_filter && options.skip_items.is_empty() {
        set_migration_completed().await?;
    }

    Ok(report)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_migration_report() {
        let mut report = MigrationReport::new();
        assert!(report.is_success());
        assert_eq!(report.total_migrated(), 0);

        report.add_skill("test-skill".to_string());
        assert_eq!(report.skills_migrated.len(), 1);
        assert_eq!(report.total_migrated(), 1);

        report.add_mcp_server("test-mcp".to_string());
        assert_eq!(report.mcp_servers_migrated.len(), 1);
        assert_eq!(report.total_migrated(), 2);

        report.add_tool("test-tool".to_string());
        assert_eq!(report.tools_migrated.len(), 1);
        assert_eq!(report.total_migrated(), 3);

        report.add_error("bad-item".to_string(), "something went wrong".to_string());
        assert!(!report.is_success());
        assert_eq!(report.errors.len(), 1);
    }

    #[test]
    fn test_migration_report_merge() {
        let mut report1 = MigrationReport::new();
        report1.add_skill("skill1".to_string());
        report1.add_mcp_server("mcp1".to_string());

        let mut report2 = MigrationReport::new();
        report2.add_skill("skill2".to_string());
        report2.add_tool("tool1".to_string());
        report2.add_error("error".to_string(), "msg".to_string());

        report1.merge(report2);

        assert_eq!(report1.skills_migrated.len(), 2);
        assert_eq!(report1.mcp_servers_migrated.len(), 1);
        assert_eq!(report1.tools_migrated.len(), 1);
        assert_eq!(report1.errors.len(), 1);
    }

    #[test]
    fn test_migration_state_path() {
        let path = migration_state_path();
        let path_str = path.to_string_lossy();
        assert!(path_str.contains("extensions"));
        assert!(path_str.contains("migration-state.json"));
    }

    #[test]
    fn test_migration_options_default() {
        let opts = MigrationOptions::default();
        assert!(!opts.force);
        assert!(opts.types.is_none());
        assert!(opts.skip_items.is_empty());
    }

    #[tokio::test]
    async fn test_parse_skill_md_valid() {
        let temp_dir = tempfile::tempdir().unwrap();
        let skill_file = temp_dir.path().join("SKILL.md");

        let content = r#"---
name: test-skill
description: A test skill
tags: [test, example]
author: Test Author
---

# Test Skill

This is the skill content.
"#;

        tokio::fs::write(&skill_file, content).await.unwrap();

        let (name, desc, tags, author) = parse_skill_md(&skill_file).await.unwrap();
        assert_eq!(name, "test-skill");
        assert_eq!(desc, "A test skill");
        assert_eq!(tags, vec!["test", "example"]);
        assert_eq!(author, Some("Test Author".to_string()));
    }

    #[tokio::test]
    async fn test_parse_skill_md_missing_frontmatter() {
        let temp_dir = tempfile::tempdir().unwrap();
        let skill_file = temp_dir.path().join("SKILL.md");

        tokio::fs::write(&skill_file, "# Just markdown\nNo frontmatter.").await.unwrap();

        assert!(parse_skill_md(&skill_file).await.is_err());
    }

    #[tokio::test]
    async fn test_parse_tool_manifest_valid() {
        let temp_dir = tempfile::tempdir().unwrap();
        let manifest_file = temp_dir.path().join("manifest.json");

        let content = r#"{"name": "test-tool", "description": "A test tool", "parameters": {"type": "object"}}"#;
        tokio::fs::write(&manifest_file, content).await.unwrap();

        let name = parse_tool_manifest(&manifest_file).await.unwrap();
        assert_eq!(name, "test-tool");
    }

    #[tokio::test]
    async fn test_parse_tool_manifest_invalid() {
        let temp_dir = tempfile::tempdir().unwrap();
        let manifest_file = temp_dir.path().join("manifest.json");

        tokio::fs::write(&manifest_file, "not valid json").await.unwrap();

        assert!(parse_tool_manifest(&manifest_file).await.is_err());
    }
}
