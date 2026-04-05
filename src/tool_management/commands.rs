//! Unified Tool Management CLI Commands
//!
//! Provides a single entry point for all tool management:
//! - `pekobot tools list` - List all installed tools
//! - `pekobot tools search <query>` - Search remote registry
//! - `pekobot tools mcp ...` - MCP server management
//! - `pekobot tools universal ...` - Universal Tool management

use crate::commands::{mcp, tool, GlobalPaths};
use crate::tool_management::{InstalledToolInfo, ToolManager, ToolType};
use clap::Subcommand;
use std::sync::Arc;

/// Unified tools command
#[derive(Subcommand)]
#[command(disable_version_flag = true)]
pub enum ToolsCommands {
    /// List all installed tools
    List {
        /// Show detailed information
        #[arg(short, long)]
        long: bool,

        /// Filter by tool type (mcp, universal, downloaded, all)
        #[arg(short, long)]
        r#type: Option<String>,
    },

    /// Search remote registry for tools
    Search {
        /// Search query
        query: String,

        /// Filter by category
        #[arg(short, long)]
        category: Option<String>,
    },

    /// List available tools from remote registry
    Available {
        /// Filter by category
        #[arg(short, long)]
        category: Option<String>,
    },

    /// Show detailed information about a tool
    Info {
        /// Tool name
        name: String,
    },

    /// Test a tool (for MCP servers)
    Test {
        /// Tool name
        name: String,

        /// Tool arguments as JSON
        #[arg(short, long)]
        args: Option<String>,
    },

    /// Start a tool (MCP servers only)
    Start {
        /// Tool name
        name: String,
    },

    /// Stop a tool (MCP servers only)
    Stop {
        /// Tool name
        name: String,

        /// Force stop
        #[arg(short, long)]
        force: bool,
    },

    /// Restart a tool (MCP servers only)
    Restart {
        /// Tool name
        name: String,
    },

    /// Check tool status (MCP servers only)
    Status {
        /// Tool name (if not provided, shows all)
        name: Option<String>,
    },

    /// MCP server management (delegates to existing pekobot mcp)
    #[command(subcommand)]
    Mcp(mcp::McpCommands),

    /// Universal Tool management (delegates to existing pekobot tool)
    #[command(subcommand)]
    Universal(tool::ToolCommands),
}

/// Handle unified tools command
pub async fn handle_tools_command(
    command: ToolsCommands,
    paths: &GlobalPaths,
    json: bool,
) -> anyhow::Result<()> {
    match command {
        ToolsCommands::List { long, r#type } => handle_list(long, r#type.as_deref(), paths, json).await,
        ToolsCommands::Search { query, category } => handle_search(&query, category.as_deref(), json).await,
        ToolsCommands::Available { category } => handle_available(category.as_deref(), json).await,
        ToolsCommands::Info { name } => handle_info(&name, paths, json).await,
        ToolsCommands::Test { name, args } => handle_test(&name, args.as_deref(), paths).await,
        ToolsCommands::Start { name } => handle_start(&name, paths).await,
        ToolsCommands::Stop { name, force } => handle_stop(&name, force, paths).await,
        ToolsCommands::Restart { name } => handle_restart(&name, paths).await,
        ToolsCommands::Status { name } => handle_status(name.as_deref(), paths, json).await,
        ToolsCommands::Mcp(mcp_cmd) => mcp::handle(mcp_cmd, paths.mcp_config()).await,
        ToolsCommands::Universal(universal_cmd) => tool::handle_tool(universal_cmd, paths, json).await,
    }
}

/// Handle list command
async fn handle_list(
    long: bool,
    filter_type: Option<&str>,
    paths: &GlobalPaths,
    json: bool,
) -> anyhow::Result<()> {
    let manager = ToolManager::with_defaults(paths.resolver().clone());
    let tools = manager.list_tools().await;

    let filtered: Vec<InstalledToolInfo> = match filter_type {
        Some("mcp") => tools.into_iter().filter(|t| t.tool_type == ToolType::Mcp).collect(),
        Some("universal") => tools.into_iter().filter(|t| t.tool_type == ToolType::Universal).collect(),
        Some("downloaded") => tools.into_iter().filter(|t| t.tool_type == ToolType::Downloaded).collect(),
        Some("built-in") | Some("builtin") => tools.into_iter().filter(|t| t.tool_type == ToolType::BuiltIn).collect(),
        _ => tools,
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&filtered)?);
        return Ok(());
    }

    if filtered.is_empty() {
        println!("No tools found.");
        return Ok(());
    }

    println!("Installed tools ({}):", filtered.len());
    println!();

    for tool in &filtered {
        let type_str = tool.tool_type.to_string();
        if long {
            println!("  {} ({})", tool.name, type_str);
            if !tool.version.is_empty() {
                println!("    Version: {}", tool.version);
            }
            if !tool.description.is_empty() {
                println!("    Description: {}", tool.description);
            }
            if !tool.install_path.as_os_str().is_empty() {
                println!("    Path: {:?}", tool.install_path);
            }
            if let Some(ref server_config) = tool.server_config {
                println!("    Transport: {:?}", server_config.transport);
                if let Some(ref cmd) = server_config.command {
                    println!("    Command: {}", cmd);
                }
            }
            println!();
        } else {
            println!("  {} ({})", tool.name, type_str);
        }
    }

    Ok(())
}

/// Handle search command
async fn handle_search(query: &str, category: Option<&str>, json: bool) -> anyhow::Result<()> {
    let manager = ToolManager::with_defaults(crate::common::paths::PathResolver::new());
    let results = manager.search_registry(query).await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&results)?);
        return Ok(());
    }

    if results.is_empty() {
        println!("No tools found matching '{}'.", query);
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
async fn handle_available(category: Option<&str>, json: bool) -> anyhow::Result<()> {
    let manager = ToolManager::with_defaults(crate::common::paths::PathResolver::new());
    let results = manager.list_available().await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&results)?);
        return Ok(());
    }

    if results.is_empty() {
        println!("No available tools found.");
        return Ok(());
    }

    println!("Available tools from registry:");
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
    let manager = ToolManager::with_defaults(paths.resolver().clone());
    let tool = manager.get_tool(name).await;

    match tool {
        Some(info) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&info)?);
            } else {
                println!("Tool: {}", info.name);
                println!("Type: {}", info.tool_type);
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
                    if !server_config.env.is_empty() {
                        println!("Env: {:?}", server_config.env);
                    }
                }
            }
            Ok(())
        }
        None => {
            anyhow::bail!("Tool '{}' not found", name);
        }
    }
}

/// Handle test command (delegates to MCP handler)
async fn handle_test(name: &str, args: Option<&str>, paths: &GlobalPaths) -> anyhow::Result<()> {
    let mcp_cmd = mcp::McpCommands::Test { name: name.to_string() };
    mcp::handle(mcp_cmd, paths.mcp_config()).await
}

/// Handle start command
async fn handle_start(name: &str, paths: &GlobalPaths) -> anyhow::Result<()> {
    let manager = ToolManager::with_defaults(paths.resolver().clone());
    manager.start_tool(name).await?;
    println!("Started '{}'", name);
    Ok(())
}

/// Handle stop command
async fn handle_stop(name: &str, force: bool, paths: &GlobalPaths) -> anyhow::Result<()> {
    let manager = ToolManager::with_defaults(paths.resolver().clone());
    manager.stop_tool(name).await?;
    println!("Stopped '{}'", name);
    Ok(())
}

/// Handle restart command
async fn handle_restart(name: &str, paths: &GlobalPaths) -> anyhow::Result<()> {
    let manager = ToolManager::with_defaults(paths.resolver().clone());
    manager.restart_tool(name).await?;
    println!("Restarted '{}'", name);
    Ok(())
}

/// Handle status command
async fn handle_status(
    name: Option<&str>,
    paths: &GlobalPaths,
    json: bool,
) -> anyhow::Result<()> {
    let manager = ToolManager::with_defaults(paths.resolver().clone());

    if let Some(name) = name {
        let status = manager.tool_status(name).await?;
        if json {
            println!("{}", serde_json::json!({ "name": name, "status": status }));
        } else {
            println!("{}: {:?}", name, status);
        }
    } else {
        let tools = manager.list_tools().await;
        let mcp_tools: Vec<_> = tools.iter().filter(|t| t.tool_type == ToolType::Mcp).collect();

        if json {
            let mut statuses = Vec::new();
            for tool in &mcp_tools {
                let status = manager.tool_status(&tool.name).await.unwrap_or(crate::tool_management::ToolStatus::Unknown);
                statuses.push(serde_json::json!({
                    "name": tool.name,
                    "status": status
                }));
            }
            println!("{}", serde_json::to_string_pretty(&statuses)?);
        } else {
            for tool in mcp_tools {
                let status = manager.tool_status(&tool.name).await.unwrap_or(crate::tool_management::ToolStatus::Unknown);
                println!("  {}: {:?}", tool.name, status);
            }
        }
    }

    Ok(())
}
