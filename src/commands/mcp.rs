//! MCP (Model Context Protocol) management commands
//!
//! Provides CLI commands for managing MCP servers:
//! - List, add, remove MCP servers
//! - Start, stop, restart servers
//! - List tools from servers
//! - Test server connections

use crate::mcp::{McpConfig, McpServerConfig, TransportType};
use clap::Subcommand;
use std::collections::HashMap;
use std::path::PathBuf;

/// MCP management commands
#[derive(Subcommand)]
pub enum McpCommands {
    /// List configured MCP servers
    List {
        /// Show detailed information
        #[arg(short, long)]
        long: bool,
    },

    /// Show MCP server details
    Show {
        /// Server name
        name: String,
    },

    /// Add a new MCP server
    Add {
        /// Server name
        name: String,

        /// Transport type (stdio or sse)
        #[arg(short, long, value_enum)]
        transport: TransportTypeArg,

        /// Command to execute (for stdio transport)
        #[arg(short, long, required_if_eq("transport", "stdio"))]
        command: Option<String>,

        /// Arguments for the command
        #[arg(long)]
        args: Vec<String>,

        /// Endpoint URL (for SSE transport)
        #[arg(short, long, required_if_eq("transport", "sse"))]
        endpoint: Option<String>,

        /// Environment variables (KEY=VALUE)
        #[arg(long)]
        env: Vec<String>,

        /// Working directory
        #[arg(long)]
        cwd: Option<PathBuf>,

        /// Don't auto-start the server
        #[arg(long)]
        no_auto_start: bool,
    },

    /// Remove an MCP server
    Remove {
        /// Server name
        name: String,

        /// Force removal even if server is running
        #[arg(short, long)]
        force: bool,
    },

    /// Start an MCP server
    Start {
        /// Server name
        name: String,
    },

    /// Stop an MCP server
    Stop {
        /// Server name
        name: String,

        /// Force stop
        #[arg(short, long)]
        force: bool,
    },

    /// Restart an MCP server
    Restart {
        /// Server name
        name: String,
    },

    /// Test connection to an MCP server
    Test {
        /// Server name
        name: String,
    },

    /// List tools from all MCP servers or a specific server
    Tools {
        /// Filter by server name
        #[arg(short, long)]
        server: Option<String>,
    },

    /// Call a tool on an MCP server
    Call {
        /// Server name
        server: String,

        /// Tool name
        tool: String,

        /// Tool arguments as JSON
        #[arg(short, long)]
        args: Option<String>,
    },

    /// Edit MCP configuration
    Config {
        /// Open config in editor
        #[arg(short, long)]
        edit: bool,
    },
}

/// Transport type argument
#[derive(Clone, Debug, clap::ValueEnum)]
pub enum TransportTypeArg {
    Stdio,
    Sse,
}

impl From<TransportTypeArg> for TransportType {
    fn from(arg: TransportTypeArg) -> Self {
        match arg {
            TransportTypeArg::Stdio => TransportType::Stdio,
            TransportTypeArg::Sse => TransportType::Sse,
        }
    }
}

/// MCP command handler
pub struct McpCommandHandler {
    config_path: PathBuf,
}

impl McpCommandHandler {
    /// Create a new handler
    pub fn new(config_path: PathBuf) -> Self {
        Self { config_path }
    }

    /// Load MCP configuration
    fn load_config(&self) -> anyhow::Result<McpConfig> {
        if self.config_path.exists() {
            let content = std::fs::read_to_string(&self.config_path)?;
            McpConfig::from_toml(&content)
        } else {
            Ok(McpConfig::default())
        }
    }

    /// Save MCP configuration
    fn save_config(&self, config: &McpConfig) -> anyhow::Result<()> {
        let content = config.to_toml()?;
        std::fs::write(&self.config_path, content)?;
        Ok(())
    }

    /// Handle list command
    pub fn list(&self, long: bool) -> anyhow::Result<()> {
        let config = self.load_config()?;

        if config.servers.is_empty() {
            println!("No MCP servers configured.");
            println!("Use 'pekobot mcp add' to add a server.");
            return Ok(());
        }

        if long {
            println!(
                "{:<20} {:<10} {:<12} {:<30}",
                "NAME", "TRANSPORT", "AUTO-START", "COMMAND/ENDPOINT"
            );
            println!("{}", "-".repeat(80));
            for server in &config.servers {
                let transport = match server.transport {
                    TransportType::Stdio => "stdio",
                    TransportType::Sse => "sse",
                };
                let target = server
                    .command
                    .as_deref()
                    .or(server.endpoint.as_deref())
                    .unwrap_or("-");
                println!(
                    "{:<20} {:<10} {:<12} {}",
                    server.name,
                    transport,
                    if server.auto_start { "yes" } else { "no" },
                    target
                );
            }
        } else {
            for server in &config.servers {
                println!("{}", server.name);
            }
        }

        Ok(())
    }

    /// Handle show command
    pub fn show(&self, name: &str) -> anyhow::Result<()> {
        let config = self.load_config()?;

        let server = config
            .get_server(name)
            .ok_or_else(|| anyhow::anyhow!("Server '{}' not found", name))?;

        println!("Name: {}", server.name);
        println!("Transport: {:?}", server.transport);
        println!("Auto-start: {}", server.auto_start);
        println!(
            "Health check interval: {}s",
            server.health_check_interval_secs
        );
        println!("Max restarts: {}", server.max_restarts);
        println!("Init timeout: {}s", server.init_timeout_secs);
        println!("Tool timeout: {}s", server.tool_timeout_secs);

        match server.transport {
            TransportType::Stdio => {
                if let Some(cmd) = &server.command {
                    println!("Command: {}", cmd);
                }
                if !server.args.is_empty() {
                    println!("Args: {:?}", server.args);
                }
            }
            TransportType::Sse => {
                if let Some(endpoint) = &server.endpoint {
                    println!("Endpoint: {}", endpoint);
                }
            }
        }

        if let Some(cwd) = &server.cwd {
            println!("Working directory: {}", cwd.display());
        }

        if !server.env.is_empty() {
            println!("Environment variables:");
            for (key, value) in &server.env {
                println!("  {}={}", key, value);
            }
        }

        Ok(())
    }

    /// Handle add command
    pub fn add(
        &self,
        name: String,
        transport: TransportTypeArg,
        command: Option<String>,
        args: Vec<String>,
        endpoint: Option<String>,
        env: Vec<String>,
        cwd: Option<PathBuf>,
        no_auto_start: bool,
    ) -> anyhow::Result<()> {
        let mut config = self.load_config()?;

        if config.get_server(&name).is_some() {
            anyhow::bail!("Server '{}' already exists", name);
        }

        // Parse environment variables
        let mut env_map = HashMap::new();
        for env_var in env {
            if let Some((key, value)) = env_var.split_once('=') {
                env_map.insert(key.to_string(), value.to_string());
            } else {
                anyhow::bail!(
                    "Invalid environment variable format: {} (expected KEY=VALUE)",
                    env_var
                );
            }
        }

        let server_config = match transport {
            TransportTypeArg::Stdio => {
                let command = command
                    .ok_or_else(|| anyhow::anyhow!("Command is required for stdio transport"))?;
                McpServerConfig::stdio(name.clone(), command, args)
                    .with_env(env_map)
                    .with_cwd(cwd.unwrap_or_else(|| std::env::current_dir().unwrap_or_default()))
                    .with_auto_start(!no_auto_start)
            }
            TransportTypeArg::Sse => {
                let endpoint = endpoint
                    .ok_or_else(|| anyhow::anyhow!("Endpoint is required for SSE transport"))?;
                McpServerConfig::sse(name.clone(), endpoint).with_auto_start(!no_auto_start)
            }
        };

        config.add_server(server_config);
        self.save_config(&config)?;

        println!("Added MCP server '{}'", name);
        if !no_auto_start {
            println!("Use 'pekobot mcp start {}' to start the server", name);
        }

        Ok(())
    }

    /// Handle remove command
    pub fn remove(&self, name: &str, force: bool) -> anyhow::Result<()> {
        let mut config = self.load_config()?;

        if config.get_server(name).is_none() {
            anyhow::bail!("Server '{}' not found", name);
        }

        // TODO: Check if server is running (requires runtime)
        if !force {
            println!("Warning: This will remove the server configuration.");
            println!("Use --force to skip this warning.");
        }

        config.remove_server(name);
        self.save_config(&config)?;

        println!("Removed MCP server '{}'", name);
        Ok(())
    }

    /// Handle tools command
    pub async fn tools(&self, server_filter: Option<&str>) -> anyhow::Result<()> {
        use crate::mcp::McpManager;

        let config = self.load_config()?;

        if config.servers.is_empty() {
            println!("No MCP servers configured.");
            return Ok(());
        }

        // Initialize manager to get tool list
        let manager = McpManager::new(config);
        manager.init().await?;

        let all_tools = manager.list_all_tools().await;

        if all_tools.is_empty() {
            println!("No tools available from MCP servers.");
            manager.shutdown().await?;
            return Ok(());
        }

        // Group by server
        let mut by_server: HashMap<String, Vec<String>> = HashMap::new();
        for (server_name, tool) in all_tools {
            if let Some(filter) = server_filter {
                if server_name != filter {
                    continue;
                }
            }
            by_server.entry(server_name).or_default().push(tool.name);
        }

        // Print results
        for (server_name, tools) in by_server {
            println!("{}:", server_name);
            for tool_name in tools {
                println!("  - {}", tool_name);
            }
        }

        manager.shutdown().await?;
        Ok(())
    }

    /// Handle test command
    pub async fn test(&self, name: &str) -> anyhow::Result<()> {
        use crate::mcp::McpManager;

        let config = self.load_config()?;

        if config.get_server(name).is_none() {
            anyhow::bail!("Server '{}' not found", name);
        }

        println!("Testing MCP server '{}'...", name);

        let manager = McpManager::new(config);
        manager.init().await?;

        match manager.start_server(name).await {
            Ok(()) => {
                // Try to ping
                let client = manager.get_client(name).await?;
                let client = client.read().await;
                match client.ping().await {
                    Ok(()) => {
                        println!("✓ Server '{}' is healthy", name);

                        // Show capabilities
                        if let Some(info) = client.server_info() {
                            println!("  Protocol version: {}", info.protocol_version);
                            println!(
                                "  Server: {} v{}",
                                info.server_info.name, info.server_info.version
                            );
                            if client.supports_capability("tools") {
                                println!("  Supports: tools");
                            }
                            if client.supports_capability("resources") {
                                println!("  Supports: resources");
                            }
                            if client.supports_capability("prompts") {
                                println!("  Supports: prompts");
                            }
                        }
                    }
                    Err(e) => {
                        println!("✗ Server '{}' ping failed: {}", name, e);
                    }
                }
            }
            Err(e) => {
                println!("✗ Failed to start server '{}': {}", name, e);
            }
        }

        manager.shutdown().await?;
        Ok(())
    }

    /// Handle config command
    pub fn config(&self, edit: bool) -> anyhow::Result<()> {
        if edit {
            let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
            std::process::Command::new(&editor)
                .arg(&self.config_path)
                .status()?;
        } else {
            // Show current config
            if self.config_path.exists() {
                let content = std::fs::read_to_string(&self.config_path)?;
                println!("{}", content);
            } else {
                println!(
                    "# No MCP configuration file found at {}",
                    self.config_path.display()
                );
                println!("# Run 'pekobot mcp add' to create one.");
            }
        }
        Ok(())
    }
}

/// Handle MCP subcommands
pub async fn handle(command: McpCommands, config_path: PathBuf) -> anyhow::Result<()> {
    let handler = McpCommandHandler::new(config_path);

    match command {
        McpCommands::List { long } => handler.list(long),
        McpCommands::Show { name } => handler.show(&name),
        McpCommands::Add {
            name,
            transport,
            command,
            args,
            endpoint,
            env,
            cwd,
            no_auto_start,
        } => handler.add(
            name,
            transport,
            command,
            args,
            endpoint,
            env,
            cwd,
            no_auto_start,
        ),
        McpCommands::Remove { name, force } => handler.remove(&name, force),
        McpCommands::Start { name } => {
            println!("Starting server '{}'...", name);
            // TODO: Implement runtime server management
            println!("Note: Runtime server management not yet implemented.");
            println!("Servers are started automatically when used.");
            Ok(())
        }
        McpCommands::Stop { name, force: _ } => {
            println!("Stopping server '{}'...", name);
            // TODO: Implement runtime server management
            println!("Note: Runtime server management not yet implemented.");
            Ok(())
        }
        McpCommands::Restart { name } => {
            println!("Restarting server '{}'...", name);
            // TODO: Implement runtime server management
            println!("Note: Runtime server management not yet implemented.");
            Ok(())
        }
        McpCommands::Test { name } => handler.test(&name).await,
        McpCommands::Tools { server } => handler.tools(server.as_deref()).await,
        McpCommands::Call { server, tool, args } => {
            println!("Calling {}::{}...", server, tool);
            if let Some(args) = args {
                println!("Arguments: {}", args);
            }
            // TODO: Implement tool calling
            println!("Note: Direct tool calling not yet implemented.");
            Ok(())
        }
        McpCommands::Config { edit } => handler.config(edit),
    }
}
