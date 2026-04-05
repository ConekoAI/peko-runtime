//! Unified Capability Framework CLI Commands
//!
//! Provides a single entry point for all capability management:
//! - `pekobot cap list` - List all capabilities
//! - `pekobot cap search <query>` - Search remote registry
//! - `pekobot cap enable <target> <cap>` - Enable capability for team/agent
//! - `pekobot cap disable <target> <cap>` - Disable capability for team/agent
//! - `pekobot cap status [target]` - Show capability status
//! - `pekobot cap mcp ...` - MCP server management
//! - `pekobot cap universal ...` - Universal Capability management

use crate::cap::{CapabilityInfo, CapabilityManager, CapabilityType};
use crate::commands::{mcp, tool, GlobalPaths};
use crate::team::capability::TeamCapabilityManager;
use clap::Subcommand;
use serde::Serialize;
use std::path::PathBuf;

/// Unified capability command
#[derive(Subcommand)]
#[command(disable_version_flag = true)]
pub enum CapCommands {
    /// List all capabilities
    List {
        /// Show detailed information
        #[arg(short, long)]
        long: bool,

        /// Filter by capability type (builtin, mcp, universal, downloaded, all)
        #[arg(short, long)]
        type_: Option<String>,
    },

    /// Search remote registry for capabilities
    Search {
        /// Search query
        query: String,

        /// Filter by category
        #[arg(short, long)]
        category: Option<String>,
    },

    /// List available capabilities from remote registry
    Available {
        /// Filter by category
        #[arg(short, long)]
        category: Option<String>,
    },

    /// Show detailed information about a capability
    Info {
        /// Capability name
        name: String,
    },

    /// Enable a capability for a team or agent
    ///
    /// Examples:
    ///   pekobot cap enable myteam shell          # Enable for team
    ///   pekobot cap enable myteam/my-agent shell # Enable for agent
    Enable {
        /// Target (team or team/agent)
        target: String,
        /// Capability name
        capability: String,
    },

    /// Disable a capability for a team or agent
    ///
    /// Examples:
    ///   pekobot cap disable myteam glob          # Disable for team
    ///   pekobot cap disable myteam/my-agent glob # Disable for agent
    Disable {
        /// Target (team or team/agent)
        target: String,
        /// Capability name
        capability: String,
    },

    /// Show capability status
    ///
    /// Without target: shows all teams and agents
    /// With team: shows team capabilities
    /// With team/agent: shows agent capabilities
    Status {
        /// Target (team or team/agent)
        target: Option<String>,
    },

    /// Test a capability (for MCP servers)
    Test {
        /// Capability name
        name: String,

        /// Capability arguments as JSON
        #[arg(short, long)]
        args: Option<String>,
    },

    /// Start a capability (MCP servers only)
    Start {
        /// Capability name
        name: String,
    },

    /// Stop a capability (MCP servers only)
    Stop {
        /// Capability name
        name: String,

        /// Force stop
        #[arg(short, long)]
        force: bool,
    },

    /// Restart a capability (MCP servers only)
    Restart {
        /// Capability name
        name: String,
    },

    /// MCP server management (delegates to existing pekobot mcp)
    #[command(subcommand)]
    Mcp(mcp::McpCommands),

    /// Universal Capability management (delegates to existing pekobot tool)
    #[command(subcommand)]
    Universal(tool::ToolCommands),
}

/// Handle unified capability command
pub async fn handle_cap_command(
    command: CapCommands,
    paths: &GlobalPaths,
    json: bool,
) -> anyhow::Result<()> {
    match command {
        CapCommands::List { long, type_ } => handle_list(long, type_.as_deref(), paths, json).await,
        CapCommands::Search { query, category } => handle_search(&query, category.as_deref()).await,
        CapCommands::Available { category } => handle_available(category.as_deref()).await,
        CapCommands::Info { name } => handle_info(&name, paths, json).await,
        CapCommands::Enable { target, capability } => handle_enable(&target, &capability, paths).await,
        CapCommands::Disable { target, capability } => handle_disable(&target, &capability, paths).await,
        CapCommands::Status { target } => handle_status(target.as_deref(), paths, json).await,
        CapCommands::Test { name, args } => handle_test(&name, args.as_deref(), paths).await,
        CapCommands::Start { name } => handle_start(&name, paths).await,
        CapCommands::Stop { name, force: _ } => handle_stop(&name, paths).await,
        CapCommands::Restart { name } => handle_restart(&name, paths).await,
        CapCommands::Mcp(mcp_cmd) => mcp::handle(mcp_cmd, paths.mcp_config()).await,
        CapCommands::Universal(universal_cmd) => tool::handle_tool(universal_cmd, paths, json).await,
    }
}

/// Parse target into (team, agent) tuple
fn parse_target(target: &str) -> (String, Option<String>) {
    if target.contains('/') {
        let parts: Vec<&str> = target.splitn(2, '/').collect();
        (parts[0].to_string(), Some(parts[1].to_string()))
    } else {
        (target.to_string(), None)
    }
}

/// Handle list command
async fn handle_list(
    long: bool,
    filter_type: Option<&str>,
    paths: &GlobalPaths,
    json: bool,
) -> anyhow::Result<()> {
    let manager = CapabilityManager::with_defaults(paths.resolver().clone());
    let caps = manager.list_capabilities().await;

    let filtered: Vec<CapabilityInfo> = match filter_type {
        Some("mcp") => caps.iter().filter(|c| c.cap_type == CapabilityType::Mcp).cloned().collect(),
        Some("universal") => caps.iter().filter(|c| c.cap_type == CapabilityType::Universal).cloned().collect(),
        Some("downloaded") => caps.iter().filter(|c| c.cap_type == CapabilityType::Downloaded).cloned().collect(),
        Some("builtin") | Some("built-in") => caps.iter().filter(|c| c.cap_type == CapabilityType::BuiltIn).cloned().collect(),
        _ => caps,
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&filtered)?);
        return Ok(());
    }

    if filtered.is_empty() {
        println!("No capabilities found.");
        return Ok(());
    }

    // Group by type for cleaner output
    let mut by_type: std::collections::HashMap<CapabilityType, Vec<&CapabilityInfo>> = std::collections::HashMap::new();
    for cap in &filtered {
        by_type.entry(cap.cap_type).or_default().push(cap);
    }

    println!("Capabilities ({}):", filtered.len());
    println!();

    for cap_type in &[CapabilityType::BuiltIn, CapabilityType::Mcp, CapabilityType::Universal, CapabilityType::Downloaded] {
        if let Some(caps) = by_type.get(cap_type) {
            println!("[{}]", cap_type);
            for cap in caps {
                if long {
                    println!("  {} - {}", cap.name, cap.description);
                    if !cap.version.is_empty() {
                        println!("    Version: {}", cap.version);
                    }
                    if !cap.install_path.as_os_str().is_empty() {
                        println!("    Path: {:?}", cap.install_path);
                    }
                    if let Some(ref server_config) = cap.server_config {
                        if let Some(ref cmd) = server_config.command {
                            println!("    Command: {}", cmd);
                        }
                    }
                } else {
                    println!("  {}", cap.name);
                }
            }
            println!();
        }
    }

    Ok(())
}

/// Handle search command
async fn handle_search(query: &str, category: Option<&str>) -> anyhow::Result<()> {
    let manager = CapabilityManager::with_defaults(crate::common::paths::PathResolver::new());
    let results = manager.search_registry(query).await?;

    if results.is_empty() {
        println!("No capabilities found matching '{}'.", query);
        return Ok(());
    }

    println!("Search results for '{}':", query);
    println!();

    for result in results {
        println!("  {} ({})", result.name, result.version);
        println!("    {}", result.description);
        if let Some(author) = result.author {
            println!("    Author: {}", author);
        }
        if !result.categories.is_empty() {
            println!("    Categories: {}", result.categories.join(", "));
        }
        println!("    Downloads: {}", result.downloads);
        println!("    Rating: {:.1}/5", result.rating);
        println!();
    }

    Ok(())
}

/// Handle available command
async fn handle_available(category: Option<&str>) -> anyhow::Result<()> {
    let manager = CapabilityManager::with_defaults(crate::common::paths::PathResolver::new());
    let results = manager.list_available().await?;

    if results.is_empty() {
        println!("No available capabilities found.");
        return Ok(());
    }

    println!("Available capabilities from registry:");
    println!();

    for result in results {
        println!("  {} ({})", result.name, result.version);
        println!("    {}", result.description);
        if let Some(author) = result.author {
            println!("    Author: {}", author);
        }
        println!();
    }

    Ok(())
}

/// Handle info command
async fn handle_info(name: &str, paths: &GlobalPaths, json: bool) -> anyhow::Result<()> {
    let manager = CapabilityManager::with_defaults(paths.resolver().clone());
    let cap = manager.get(name).await;

    match cap {
        Some(info) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&info)?);
            } else {
                println!("Capability: {}", info.name);
                println!("Type: {}", info.cap_type);
                if !info.version.is_empty() {
                    println!("Version: {}", info.version);
                }
                if !info.description.is_empty() {
                    println!("Description: {}", info.description);
                }
                if !info.install_path.as_os_str().is_empty() {
                    println!("Path: {:?}", info.install_path);
                }
                if let Some(ref manifest) = info.manifest_path {
                    println!("Manifest: {:?}", manifest);
                }
                println!("Active: {}", info.is_active);
                if let Some(ref server_config) = info.server_config {
                    println!("Transport: {:?}", server_config.transport);
                    if let Some(ref cmd) = server_config.command {
                        println!("Command: {}", cmd);
                    }
                    println!("Auto-start: {}", server_config.auto_start);
                    if !server_config.args.is_empty() {
                        println!("Args: {:?}", server_config.args);
                    }
                }
            }
            Ok(())
        }
        None => {
            anyhow::bail!("Capability '{}' not found", name);
        }
    }
}

/// Handle enable command
async fn handle_enable(target: &str, capability: &str, paths: &GlobalPaths) -> anyhow::Result<()> {
    let (team, agent) = parse_target(target);

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
        // Team-level: update team capabilities.toml
        let team_mgr = TeamCapabilityManager::new(paths.resolver().clone());
        team_mgr.enable(&team, capability)?;
    }

    Ok(())
}

/// Handle disable command
async fn handle_disable(target: &str, capability: &str, paths: &GlobalPaths) -> anyhow::Result<()> {
    let (team, agent) = parse_target(target);

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
        // Team-level: update team capabilities.toml
        let team_mgr = TeamCapabilityManager::new(paths.resolver().clone());
        team_mgr.disable(&team, capability)?;
    }

    Ok(())
}

/// Handle status command
async fn handle_status(target: Option<&str>, paths: &GlobalPaths, json: bool) -> anyhow::Result<()> {
    let manager = CapabilityManager::with_defaults(paths.resolver().clone());

    if let Some(target) = target {
        let (team, agent) = parse_target(target);

        if let Some(agent_name) = agent {
            // Agent-level status
            let config_path = paths.resolver().agent_config(&agent_name, Some(&team));
            if !config_path.exists() {
                anyhow::bail!("Agent '{}' not found in team '{}'", agent_name, team);
            }

            let content = std::fs::read_to_string(&config_path)?;
            let config: crate::types::agent::AgentConfig = toml::from_str(&content)?;

            let caps = manager.list_capabilities().await;

            if json {
                #[derive(Serialize)]
                struct AgentCapStatus<'a> {
                    agent: &'a str,
                    team: &'a str,
                    enabled: Vec<String>,
                    capabilities: Vec<CapabilityInfo>,
                }
                let status = AgentCapStatus {
                    agent: &agent_name,
                    team: &team,
                    enabled: config.tools.as_ref().map(|t| t.enabled.clone()).unwrap_or_default(),
                    capabilities: caps,
                };
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else {
                println!("Agent: {}/{}", team, agent_name);
                println!("Enabled capabilities: {:?}", config.tools.as_ref().map(|t| &t.enabled).unwrap_or(&vec![]));
            }
        } else {
            // Team-level status
            let team_mgr = TeamCapabilityManager::new(paths.resolver().clone());
            if let Some(team_config) = team_mgr.list(&team)? {
                if json {
                    println!("{}", serde_json::to_string_pretty(&team_config)?);
                } else {
                    println!("Team: {}", team);
                    println!("Enabled: {:?}", team_config.enabled);
                    println!("Disabled: {:?}", team_config.disabled);
                }
            } else {
                println!("Team '{}' has no capability configuration (defaults apply)", team);
            }
        }
    } else {
        // No target: show all teams
        let teams_dir = paths.resolver().teams_dir();
        if !teams_dir.exists() {
            println!("No teams found.");
            return Ok(());
        }

        let mut teams = Vec::new();
        for entry in std::fs::read_dir(&teams_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    teams.push(name.to_string());
                }
            }
        }

        if json {
            println!("{}", serde_json::to_string_pretty(&teams)?);
        } else {
            println!("Teams:");
            for team in teams {
                println!("  {}", team);
            }
        }
    }

    Ok(())
}

/// Handle test command (delegates to MCP handler)
async fn handle_test(name: &str, args: Option<&str>, paths: &GlobalPaths) -> anyhow::Result<()> {
    let mcp_cmd = mcp::McpCommands::Test { name: name.to_string() };
    mcp::handle(mcp_cmd, paths.mcp_config()).await
}

/// Handle start command
async fn handle_start(name: &str, paths: &GlobalPaths) -> anyhow::Result<()> {
    let manager = CapabilityManager::with_defaults(paths.resolver().clone());
    manager.start(name).await?;
    println!("Started '{}'", name);
    Ok(())
}

/// Handle stop command
async fn handle_stop(name: &str, paths: &GlobalPaths) -> anyhow::Result<()> {
    let manager = CapabilityManager::with_defaults(paths.resolver().clone());
    manager.stop(name).await?;
    println!("Stopped '{}'", name);
    Ok(())
}

/// Handle restart command
async fn handle_restart(name: &str, paths: &GlobalPaths) -> anyhow::Result<()> {
    let manager = CapabilityManager::with_defaults(paths.resolver().clone());
    manager.restart(name).await?;
    println!("Restarted '{}'", name);
    Ok(())
}
