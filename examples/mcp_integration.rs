//! MCP Integration Example
//!
//! This example demonstrates how to use MCP (Model Context Protocol) servers
//! with Pekobot to extend agent capabilities.
//!
//! # Running the Example
//!
//! ```bash
//! cargo run --example mcp_integration
//! ```
//!
//! # Prerequisites
//!
//! You need to have MCP servers installed. For example:
//! - `npm install -g @anthropic/mcp-filesystem-server`
//! - `npm install -g @anthropic/mcp-browser-server`

use pekobot::mcp::{McpConfig, McpManager, McpServerConfig};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt::init();

    println!("=== Pekobot MCP Integration Example ===\n");

    // Create MCP configuration
    let mut config = McpConfig::new();

    // Add a filesystem MCP server (stdio transport)
    config.add_server(
        McpServerConfig::stdio(
            "filesystem",
            "mcp-filesystem-server",
            vec!["/home/user/documents".to_string()],
        )
        .with_env(HashMap::from([(
            "MCP_LOG_LEVEL".to_string(),
            "debug".to_string(),
        )]))
        .with_auto_start(true),
    );

    // Add a browser MCP server (stdio transport)
    config.add_server(
        McpServerConfig::stdio(
            "browser",
            "mcp-browser-server",
            vec!["--headless".to_string()],
        )
        .with_auto_start(true),
    );

    // Add a remote MCP server (SSE transport)
    // config.add_server(
    //     McpServerConfig::sse("remote-tools", "https://tools.example.com/mcp")
    //         .with_auto_start(false), // Don't auto-start, start manually later
    // );

    println!("Configuration:");
    println!("{}", config.to_toml()?);

    // Create and initialize MCP manager
    let manager = Arc::new(RwLock::new(McpManager::new(config)));
    {
        let mgr = manager.read().await;
        mgr.init().await?;
    }

    println!("\nMCP Manager initialized\n");

    // List all servers and their states
    let servers = {
        let mgr = manager.read().await;
        mgr.list_servers().await
    };

    println!("Servers:");
    for server in servers {
        println!(
            "  - {}: running={}, healthy={}",
            server.name, server.running, server.healthy
        );
        if let Some(info) = server.server_info {
            println!("    Info: {}", info);
        }
        if !server.tools.is_empty() {
            println!("    Tools:");
            for tool in &server.tools {
                println!("      - {}", tool.name);
            }
        }
    }

    // Get all tools from MCP servers as Pekobot Tool trait objects
    let mcp_tools = {
        let mgr = manager.read().await;
        mgr.get_tools().await
    };

    println!("\nMCP Tools ({} total):", mcp_tools.len());
    for tool in &mcp_tools {
        println!("  - {}: {}", tool.name(), tool.description());
    }

    // Example: Create combined tool list with built-in and MCP tools
    let mut all_tools =
        pekobot::tools::ToolFactory::create_full_tools(std::path::PathBuf::from("."));

    // Add MCP tools
    for tool in mcp_tools {
        all_tools.push(tool);
    }

    println!("\nCombined tool list ({} tools):", all_tools.len());
    for tool in &all_tools {
        println!("  - {}", tool.name());
    }

    // Example: Call an MCP tool directly through the manager
    // This would work if you have a real MCP server running
    // let result = {
    //     let mgr = manager.read().await;
    //     mgr.call_tool(
    //         "filesystem",
    //         "read_file",
    //         serde_json::json!({"path": "/home/user/documents/README.md"}),
    //     ).await?
    // };
    // println!("Tool result: {:?}", result);

    // Shutdown
    {
        let mgr = manager.read().await;
        mgr.shutdown().await?;
    }

    println!("\nMCP Manager shut down");

    Ok(())
}

/// Example: Using MCP tools with an Agent
#[allow(dead_code)]
async fn example_with_agent() -> anyhow::Result<()> {
    use pekobot::mcp::McpConfig;

    // Create agent configuration
    let agent_config = pekobot::config::Config::default();

    // Create MCP manager
    let mcp_config = McpConfig::default();
    let mcp_manager = Arc::new(RwLock::new(McpManager::new(mcp_config)));

    // Initialize MCP
    {
        let mgr = mcp_manager.read().await;
        mgr.init().await?;
    }

    // Get built-in tools
    let workspace_dir = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let mut tools = pekobot::tools::ToolFactory::create_full_tools(workspace_dir);

    // Add MCP tools
    let mcp_tools = {
        let mgr = mcp_manager.read().await;
        mgr.get_tools().await
    };
    tools.extend(mcp_tools);

    // Now use these tools with an agent
    // let agent = Agent::new(agent_config, tools)?;
    // ...

    Ok(())
}

/// Example: Dynamic MCP tool discovery
#[allow(dead_code)]
async fn example_dynamic_discovery() -> anyhow::Result<()> {
    use pekobot::mcp::McpManager;
    use std::time::Duration;

    let manager = Arc::new(RwLock::new(McpManager::new(McpConfig::default())));

    // Initialize
    {
        let mgr = manager.read().await;
        mgr.init().await?;
    }

    // Periodically refresh tools
    loop {
        tokio::time::sleep(Duration::from_secs(60)).await;

        let tools = {
            let mgr = manager.read().await;
            mgr.get_tools().await
        };

        println!("Discovered {} MCP tools", tools.len());
    }
}
