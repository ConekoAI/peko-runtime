//! MCP (Model Context Protocol) management commands
//!
//! Provides CLI commands for managing MCP servers:
//! - List, add, remove MCP servers
//! - Install built-in MCP servers (web, browser, memory)
//! - Check status of all servers
//! - Start, stop, restart servers
//! - List tools from servers
//! - Test server connections
//! - Call tools directly

use crate::mcp::{
    discover_servers, ensure_default_config, is_server_installed, list_available_servers,
    mcp_config_path, mcp_install_dir, McpConfig, McpServerConfig, TransportType,
};
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

    /// Install a built-in MCP server (web, browser, memory)
    Install {
        /// Server name (web, browser, memory)
        name: String,

        /// Force reinstall even if already installed
        #[arg(short, long)]
        force: bool,

        /// Build from source instead of downloading
        #[arg(long)]
        build: bool,
    },

    /// Check status of MCP servers
    Status {
        /// Check a specific server
        #[arg(short, long)]
        server: Option<String>,
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

        /// Arguments as key=value pairs
        #[arg(long)]
        kv: Vec<String>,
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
    #[must_use] 
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
            .ok_or_else(|| anyhow::anyhow!("Server '{name}' not found"))?;

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
                    println!("Command: {cmd}");
                }
                if !server.args.is_empty() {
                    println!("Args: {:?}", server.args);
                }
            }
            TransportType::Sse => {
                if let Some(endpoint) = &server.endpoint {
                    println!("Endpoint: {endpoint}");
                }
            }
        }

        if let Some(cwd) = &server.cwd {
            println!("Working directory: {}", cwd.display());
        }

        if !server.env.is_empty() {
            println!("Environment variables:");
            for (key, value) in &server.env {
                println!("  {key}={value}");
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
            anyhow::bail!("Server '{name}' already exists");
        }

        // Parse environment variables
        let mut env_map = HashMap::new();
        for env_var in env {
            if let Some((key, value)) = env_var.split_once('=') {
                env_map.insert(key.to_string(), value.to_string());
            } else {
                anyhow::bail!(
                    "Invalid environment variable format: {env_var} (expected KEY=VALUE)"
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

        println!("Added MCP server '{name}'");
        if !no_auto_start {
            println!("Use 'pekobot mcp start {name}' to start the server");
        }

        Ok(())
    }

    /// Handle remove command
    pub fn remove(&self, name: &str, force: bool) -> anyhow::Result<()> {
        let mut config = self.load_config()?;

        if config.get_server(name).is_none() {
            anyhow::bail!("Server '{name}' not found");
        }

        // TODO: Check if server is running (requires runtime)
        if !force {
            println!("Warning: This will remove the server configuration.");
            println!("Use --force to skip this warning.");
        }

        config.remove_server(name);
        self.save_config(&config)?;

        println!("Removed MCP server '{name}'");
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
            println!("{server_name}:");
            for tool_name in tools {
                println!("  - {tool_name}");
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
            anyhow::bail!("Server '{name}' not found");
        }

        println!("Testing MCP server '{name}'...");

        let manager = McpManager::new(config);
        manager.init().await?;

        match manager.start_server(name).await {
            Ok(()) => {
                // Try to ping
                let client = manager.get_client(name).await?;
                let client = client.read().await;
                match client.ping().await {
                    Ok(()) => {
                        println!("✓ Server '{name}' is healthy");

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
                        println!("✗ Server '{name}' ping failed: {e}");
                    }
                }
            }
            Err(e) => {
                println!("✗ Failed to start server '{name}': {e}");
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
                println!("{content}");
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

    /// Handle install command
    pub async fn install(&self, name: &str, force: bool, build: bool) -> anyhow::Result<()> {
        let valid_servers = ["web", "browser", "memory"];
        
        if !valid_servers.contains(&name) {
            anyhow::bail!(
                "Unknown MCP server '{}'. Valid servers: {}",
                name,
                valid_servers.join(", ")
            );
        }

        // Check if already installed
        if is_server_installed(name).await && !force {
            println!("✓ MCP server '{}' is already installed", name);
            println!("  Use --force to reinstall");
            return Ok(());
        }

        println!("Installing MCP server '{}'...", name);

        // Ensure install directory exists
        let install_dir = mcp_install_dir();
        tokio::fs::create_dir_all(&install_dir).await?;

        if build {
            // Build from source
            self.build_and_install(name, &install_dir).await?;
        } else {
            // Try to download pre-built binary or build
            println!("  Building from source...");
            self.build_and_install(name, &install_dir).await?;
        }

        // Ensure config exists
        ensure_default_config().await?;

        println!("✓ MCP server '{}' installed successfully", name);
        println!("  Binary: {}", install_dir.join(format!("mcp-{}", name)).display());
        println!("  Config: {}", mcp_config_path().display());
        println!("\nTest the installation with:");
        println!("  pekobot mcp test {}", name);

        Ok(())
    }

    /// Build and install an MCP server from source
    async fn build_and_install(&self, name: &str, install_dir: &PathBuf) -> anyhow::Result<()> {
        let source_dir = match name {
            "web" => "mcp-servers/mcp-web",
            "browser" => "mcp-servers/mcp-browser",
            "memory" => "mcp-servers/mcp-memory",
            _ => anyhow::bail!("Unknown server: {}", name),
        };

        // Check if source exists
        if !PathBuf::from(source_dir).exists() {
            anyhow::bail!(
                "Source directory '{}' not found. Make sure you're running from the project root.",
                source_dir
            );
        }

        println!("  Building {}...", source_dir);

        // Build the server
        let output = tokio::process::Command::new("cargo")
            .args(["build", "--release", "-p", &format!("mcp-{}", name)])
            .current_dir(".")
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Build failed:\n{}", stderr);
        }

        // Copy binary to install dir
        let binary_name = format!("mcp-{}", name);
        let source_binary = PathBuf::from("mcp-servers")
            .join(format!("mcp-{}", name))
            .join("target")
            .join("release")
            .join(&binary_name);
        
        // Also check workspace target
        let workspace_binary = PathBuf::from("mcp-servers")
            .join("target")
            .join("release")
            .join(&binary_name);

        let binary_to_copy = if source_binary.exists() {
            source_binary
        } else if workspace_binary.exists() {
            workspace_binary
        } else {
            anyhow::bail!("Built binary not found at expected location");
        };

        let target_binary = install_dir.join(&binary_name);
        tokio::fs::copy(&binary_to_copy, &target_binary).await?;

        // Make executable (on Unix)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = tokio::fs::metadata(&target_binary).await?.permissions();
            perms.set_mode(0o755);
            tokio::fs::set_permissions(&target_binary, perms).await?;
        }

        Ok(())
    }

    /// Handle status command
    pub async fn status(&self, server_filter: Option<&str>) -> anyhow::Result<()> {
        println!("MCP Server Status\n");

        // Show config path
        let config_path = mcp_config_path();
        println!("Config: {}", config_path.display());
        
        if !config_path.exists() {
            println!("  Status: Not configured");
            println!("\nRun 'pekobot mcp install <server>' to get started.");
            return Ok(());
        }

        // Discover servers
        match discover_servers().await {
            Ok(servers) => {
                if servers.is_empty() {
                    println!("\nNo MCP servers configured.");
                    println!("Run 'pekobot mcp install <web|browser|memory>' to add servers.");
                    return Ok(());
                }

                println!("\n{:<15} {:<12} {:<10} {}", "NAME", "STATUS", "TOOLS", "BINARY");
                println!("{}", "-".repeat(60));

                for server in &servers {
                    // Apply filter if specified
                    if let Some(filter) = server_filter {
                        if server.name != filter {
                            continue;
                        }
                    }

                    let status_icon = match server.status {
                        crate::mcp::McpServerStatus::Available => "✅",
                        crate::mcp::McpServerStatus::NotInstalled => "❌",
                        _ => "⚠️ ",
                    };

                    let binary_status = if is_server_installed(&server.name).await {
                        "installed"
                    } else {
                        "not found"
                    };

                    println!(
                        "{:<15} {} {:<10} {:<10} {}",
                        server.name,
                        status_icon,
                        format!("{}", server.tools_count),
                        "",
                        binary_status
                    );

                    if let Some(ref error) = server.error {
                        println!("  → {}", error);
                    }
                }

                // Show available but uninstalled servers
                let available = list_available_servers().await;
                let mut installed = std::collections::HashSet::new();
                for server in &servers {
                    if is_server_installed(&server.name).await {
                        installed.insert(server.name.clone());
                    }
                }

                let not_installed: Vec<_> = available
                    .iter()
                    .filter(|s| !installed.contains(*s))
                    .collect();

                if !not_installed.is_empty() {
                    println!("\nAvailable to install:");
                    for name in not_installed {
                        println!("  • {} (run: pekobot mcp install {})", name, name);
                    }
                }
            }
            Err(e) => {
                println!("Error discovering servers: {}", e);
            }
        }

        Ok(())
    }

    /// Handle call command
    pub async fn call(
        &self,
        server: &str,
        tool: &str,
        args_json: Option<&str>,
        kv_args: &[String],
    ) -> anyhow::Result<()> {
        use crate::mcp::McpManager;
        use serde_json::Value;

        // Parse arguments
        let mut args: Value = if let Some(json_str) = args_json {
            serde_json::from_str(json_str)?
        } else {
            Value::Object(serde_json::Map::new())
        };

        // Add key=value pairs
        if let Value::Object(ref mut map) = args {
            for kv in kv_args {
                if let Some((key, value)) = kv.split_once('=') {
                    // Try to parse as number, bool, or string
                    let parsed: Value = if let Ok(n) = value.parse::<i64>() {
                        Value::Number(n.into())
                    } else if let Ok(b) = value.parse::<bool>() {
                        Value::Bool(b)
                    } else {
                        Value::String(value.to_string())
                    };
                    map.insert(key.to_string(), parsed);
                } else {
                    anyhow::bail!("Invalid key=value format: {}", kv);
                }
            }
        }

        println!("Calling {}::{}...", server, tool);
        println!("Arguments: {}", serde_json::to_string_pretty(&args)?);

        // Load config and initialize manager
        let config = self.load_config()?;

        if config.get_server(server).is_none() {
            anyhow::bail!("Server '{}' not found in config", server);
        }

        let manager = McpManager::new(config);
        manager.init().await?;

        // Get client and call tool
        let client = manager.get_client(server).await?;
        let result = {
            let client_guard = client.read().await;
            client_guard.call_tool(tool, args).await
        };

        match result {
            Ok(tool_result) => {
                println!("\n✓ Tool executed successfully");
                
                // Print results
                for content in &tool_result.content {
                    match content {
                        crate::mcp::types::ToolResultContent::Text(text) => {
                            println!("\n{}", text.text);
                        }
                        crate::mcp::types::ToolResultContent::Image(img) => {
                            println!("\n[Image: {} ({} bytes)]", img.mime_type, img.data.len());
                        }
                        crate::mcp::types::ToolResultContent::Resource(res) => {
                            println!("\n[Resource: {:?}]", res);
                        }
                    }
                }

                if tool_result.is_error {
                    println!("\n⚠️ Tool reported an error");
                }
            }
            Err(e) => {
                println!("\n✗ Tool call failed: {}", e);
            }
        }

        manager.shutdown().await?;
        Ok(())
    }
}

/// Handle MCP subcommands
pub async fn handle(command: McpCommands, config_path: PathBuf) -> anyhow::Result<()> {
    let handler = McpCommandHandler::new(config_path);

    match command {
        McpCommands::List { long } => handler.list(long),
        McpCommands::Show { name } => handler.show(&name),
        McpCommands::Install { name, force, build } => handler.install(&name, force, build).await,
        McpCommands::Status { server } => handler.status(server.as_deref()).await,
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
            println!("Starting server '{name}'...");
            // TODO: Implement runtime server management
            println!("Note: Runtime server management not yet implemented.");
            println!("Servers are started automatically when used.");
            Ok(())
        }
        McpCommands::Stop { name, force: _ } => {
            println!("Stopping server '{name}'...");
            // TODO: Implement runtime server management
            println!("Note: Runtime server management not yet implemented.");
            Ok(())
        }
        McpCommands::Restart { name } => {
            println!("Restarting server '{name}'...");
            // TODO: Implement runtime server management
            println!("Note: Runtime server management not yet implemented.");
            Ok(())
        }
        McpCommands::Test { name } => handler.test(&name).await,
        McpCommands::Tools { server } => handler.tools(server.as_deref()).await,
        McpCommands::Call { server, tool, args, kv } => {
            handler.call(&server, &tool, args.as_deref(), &kv).await
        }
        McpCommands::Config { edit } => handler.config(edit),
    }
}
