//! MCP (Model Context Protocol) Adapter for the Extension system
//!
//! This adapter integrates MCP servers into the unified Extension Architecture.
//! MCP servers are stateful (maintain connections) and can provide tools,
//! resources, and prompts.
//!
//! # MCP Server Format
//!
//! MCP servers are configured via config files (TOML/JSON) or discovered
//! from the mcp_servers/ directory:
//! ```toml
//! [[servers]]
//! name = "filesystem"
//! transport = "stdio"
//! command = "npx"
//! args = ["-y", "@modelcontextprotocol/server-filesystem", "/path"]
//! auto_start = true
//! ```
//!
//! # Hook Points
//!
//! MCP servers hook into:
//! - `ToolRegister` - Registers all tools provided by the server
//! - `PromptSystemSection { section: "tools" }` - Adds tool descriptions
//! - `ToolExecute { tool_name: "mcp:{server}:{tool}" }` - Handles tool execution
//! - `AgentInit` - Starts the MCP server connection
//! - `AgentShutdown` - Stops the MCP server connection

use crate::extensions::adapters::{ExtensionState, ExtensionTypeAdapter, ManifestFormat};
use crate::extensions::core::{
    ExtensionCore, HookBinding, HookContext, HookHandler, HookHandlerFactory, HookPoint,
    ToolExecutionConfig, ToolMetadata, ToolSource, // NEW
};
use crate::extensions::services::ReservedParamsConfig; // NEW
use crate::extensions::types::{
    AsyncReceipt, ExtensionId, ExtensionManifest, HookId, HookOutput, HookResult,
};
use crate::agent::async_tool_framework::AsyncTaskStatus;
use uuid::Uuid;
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

/// MCP extension type identifier
pub const MCP_EXTENSION_TYPE: &str = "mcp";

/// Default priority for MCP hooks
pub const MCP_HOOK_PRIORITY: i32 = 50;

/// Prefix for MCP tool names
pub const MCP_TOOL_PREFIX: &str = "mcp";

/// MCP adapter for Extension system
pub struct McpAdapter {
    /// Shared MCP manager for all servers
    manager: Arc<RwLock<crate::mcp::McpManager>>,
}

impl std::fmt::Debug for McpAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpAdapter")
            .field("manager", &"<McpManager>")
            .finish()
    }
}

impl McpAdapter {
    /// Create a new MCP adapter
    pub fn new(manager: Arc<RwLock<crate::mcp::McpManager>>) -> Self {
        Self { manager }
    }

    /// Create a new MCP adapter with default manager
    pub fn with_default_manager() -> Self {
        let config = crate::mcp::McpConfig::default();
        let manager = Arc::new(RwLock::new(crate::mcp::McpManager::new(config)));
        Self { manager }
    }

    /// Discover MCP servers from a directory
    pub async fn discover_servers(&self, path: &Path) -> Vec<DiscoveredMcpServer> {
        let mut servers = Vec::new();

        if !path.exists() {
            debug!("MCP servers directory does not exist: {:?}", path);
            return servers;
        }

        // Look for server config files
        let entries = match tokio::fs::read_dir(path).await {
            Ok(entries) => entries,
            Err(e) => {
                warn!("Failed to read MCP directory {:?}: {}", path, e);
                return servers;
            }
        };

        // Collect entries first to avoid lifetime issues
        let mut dir_entries = Vec::new();
        let mut entries = entries;
        while let Ok(Some(entry)) = entries.next_entry().await {
            dir_entries.push(entry);
        }

        for entry in dir_entries {
            let path = entry.path();
            let name = entry.file_name();
            let name_str = name.to_string_lossy();

            // Skip hidden files
            if name_str.starts_with('.') {
                continue;
            }

            // Look for config files
            let config_path = if path.is_dir() {
                // Check for config.toml or config.json in subdirectory
                let toml_path = path.join("config.toml");
                let json_path = path.join("config.json");
                if toml_path.exists() {
                    toml_path
                } else if json_path.exists() {
                    json_path
                } else {
                    continue;
                }
            } else {
                // Direct config file
                path.clone()
            };

            if let Some(ext) = config_path.extension() {
                if ext == "toml" || ext == "json" {
                    match self.parse_server_config(&config_path).await {
                        Ok(manifest) => {
                            servers.push(DiscoveredMcpServer {
                                manifest,
                                config_path,
                                server_dir: if path.is_dir() {
                                    path.clone()
                                } else {
                                    path.parent().unwrap_or(Path::new(".")).to_path_buf()
                                },
                            });
                        }
                        Err(e) => {
                            warn!("Failed to parse MCP config {:?}: {}", config_path, e);
                        }
                    }
                }
            }
        }

        servers
    }

    /// Parse a server config file into an extension manifest
    async fn parse_server_config(&self, path: &Path) -> Result<ExtensionManifest> {
        let content = tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("Failed to read config {:?}", path))?;

        // Try TOML first, then JSON
        let server_configs: Vec<crate::mcp::McpServerConfig> = if path.extension().map(|e| e == "toml").unwrap_or(false) {
            let config: crate::mcp::McpConfig = toml::from_str(&content)
                .with_context(|| format!("Failed to parse TOML config {:?}", path))?;
            config.servers
        } else {
            let config: crate::mcp::McpConfig = serde_json::from_str(&content)
                .with_context(|| format!("Failed to parse JSON config {:?}", path))?;
            config.servers
        };

        // For simplicity, we take the first server config
        // In practice, a config file might have multiple servers
        let server_config = server_configs
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("No server configurations found"))?;

        let mut manifest = ExtensionManifest::new(
            &server_config.name,
            MCP_EXTENSION_TYPE,
            &server_config.name,
            &format!("MCP server: {}", server_config.name),
            "1.0.0",
            path.parent().unwrap_or(Path::new(".")).to_path_buf(),
        );

        // Store server configuration
        manifest.set("config_path", path.to_string_lossy().to_string());
        manifest.set("transport", format!("{:?}", server_config.transport));
        manifest.set("command", server_config.command.unwrap_or_default());
        manifest.set("args", server_config.args);
        manifest.set("auto_start", server_config.auto_start);
        manifest.set("endpoint", server_config.endpoint.unwrap_or_default());

        Ok(manifest)
    }

    /// Get the MCP manager
    pub fn manager(&self) -> Arc<RwLock<crate::mcp::McpManager>> {
        self.manager.clone()
    }

    /// Register MCP server tools with the unified registry (ADR-018b)
    ///
    /// This method queries the MCP server for its tools and registers them
    /// with ExtensionCore using the unified tool registry.
    ///
    /// # Arguments
    /// * `core` - The ExtensionCore to register tools with
    /// * `server_name` - Name of the MCP server
    ///
    /// # Returns
    /// Number of tools registered
    pub async fn register_server_tools(
        &self,
        core: &ExtensionCore,
        server_name: &str,
    ) -> Result<usize> {
        let manager = self.manager.read().await;
        
        // Get tools from all MCP servers and filter by server name
        let all_tools = manager.list_all_tools().await;
        let tools: Vec<_> = all_tools.into_iter()
            .filter(|(srv, _)| srv == server_name)
            .map(|(_, tool)| tool)
            .collect();
        
        let ext_id = ExtensionId::new(format!("mcp:{}", server_name));
        let mut registered_count = 0;
        
        for tool in tools {
            let tool_name = format!("{}:{}:{}", MCP_TOOL_PREFIX, server_name, tool.name);
            
            // Create tool metadata
            let metadata = ToolMetadata {
                name: tool_name.clone(),
                description: if tool.description.is_empty() {
                    format!("MCP tool: {}", tool.name)
                } else {
                    tool.description
                },
                parameters: tool.input_schema,
                source: ToolSource::Mcp { server: server_name.to_string() },
                reserved_params: ReservedParamsConfig::new(), // MCP tools can configure this
            };
            
            // Create execution handler with specific tool name
            let exec_handler = Arc::new(McpToolExecuteHandler {
                manager: self.manager.clone(),
                server_name: server_name.to_string(),
                tool_name: Some(tool.name),
            });
            
            // Register with unified registry
            match core.register_tool(metadata, exec_handler, &ext_id).await {
                Ok(_) => {
                    debug!(tool_name = %tool_name, "Registered MCP tool");
                    registered_count += 1;
                }
                Err(e) => {
                    // Tool might already be registered or not in whitelist
                    warn!(tool_name = %tool_name, error = %e, "Failed to register MCP tool");
                }
            }
        }
        
        info!(
            server_name = %server_name,
            registered = registered_count,
            "Registered MCP server tools"
        );
        
        Ok(registered_count)
    }
}

impl Default for McpAdapter {
    fn default() -> Self {
        Self::with_default_manager()
    }
}

#[async_trait]
impl ExtensionTypeAdapter for McpAdapter {
    fn extension_type(&self) -> &'static str {
        MCP_EXTENSION_TYPE
    }

    fn manifest_format(&self) -> ManifestFormat {
        ManifestFormat::Custom {
            detector: |path| {
                path.join("config.toml").exists()
                    || path.join("config.json").exists()
                    || path.extension().map(|e| e == "toml" || e == "json").unwrap_or(false)
            },
        }
    }

    fn resolve_hooks(&self, manifest: &ExtensionManifest) -> Vec<HookBinding> {
        vec![
            // Agent init - start MCP server
            HookBinding::new(
                HookPoint::AgentInit,
                Box::new(McpServerInitFactory {
                    manager: self.manager.clone(),
                    server_name: manifest.name.clone(),
                    config_path: manifest
                        .get("config_path")
                        .and_then(|v| v.as_str())
                        .map(PathBuf::from)
                        .unwrap_or_default(),
                }),
            ),
            // Agent shutdown - stop MCP server
            HookBinding::new(
                HookPoint::AgentShutdown,
                Box::new(McpServerShutdownFactory {
                    manager: self.manager.clone(),
                    server_name: manifest.name.clone(),
                }),
            ),
            // Tool execute - handle MCP tool calls
            HookBinding::new(
                HookPoint::ToolExecute {
                    tool_name: format!("{}:{}:*", MCP_TOOL_PREFIX, manifest.name),
                },
                Box::new(McpToolExecuteFactory {
                    manager: self.manager.clone(),
                    server_name: manifest.name.clone(),
                }),
            ),
            // Async tool execution - for long-running MCP tools
            HookBinding::new(
                HookPoint::ToolExecuteAsync {
                    tool_name: format!("{}:{}:*", MCP_TOOL_PREFIX, manifest.name),
                },
                Box::new(McpToolExecuteAsyncFactory {
                    manager: self.manager.clone(),
                    server_name: manifest.name.clone(),
                }),
            ),
            // Check status - for async tasks
            HookBinding::new(
                HookPoint::ToolCheckStatus {
                    tool_name: format!("{}:{}:*", MCP_TOOL_PREFIX, manifest.name),
                },
                Box::new(McpToolCheckStatusFactory {
                    manager: self.manager.clone(),
                    server_name: manifest.name.clone(),
                }),
            ),
            // Cancel - for async tasks
            HookBinding::new(
                HookPoint::ToolCancel {
                    tool_name: format!("{}:{}:*", MCP_TOOL_PREFIX, manifest.name),
                },
                Box::new(McpToolCancelFactory {
                    manager: self.manager.clone(),
                    server_name: manifest.name.clone(),
                }),
            ),
        ]
    }

    async fn initialize(&self, manifest: &ExtensionManifest) -> Result<ExtensionState> {
        // Start the MCP server and return state
        let server_name = manifest.name.clone();
        let manager = self.manager.read().await;
        
        // Add server config if not already present
        let config_path = manifest
            .get("config_path")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("Missing config_path in manifest"))?;

        info!(server_name = %server_name, "Initializing MCP server");
        
        // Try to start the server
        match manager.start_server(&server_name).await {
            Ok(()) => {
                info!(server_name = %server_name, "MCP server started");
                Ok(ExtensionState::Unit) // MCP manages its own state
            }
            Err(crate::mcp::manager::ManagerError::ServerNotFound(_)) => {
                // Server not configured yet, that's OK - it will be started on first use
                debug!(server_name = %server_name, "MCP server not yet configured, will start on demand");
                Ok(ExtensionState::Unit)
            }
            Err(e) => Err(anyhow::anyhow!("Failed to start MCP server: {}", e)),
        }
    }

    async fn shutdown(&self, _state: ExtensionState) -> Result<()> {
        // MCP manager handles shutdown centrally
        Ok(())
    }
}

/// A discovered MCP server before registration
#[derive(Debug, Clone)]
pub struct DiscoveredMcpServer {
    /// Extension manifest
    pub manifest: ExtensionManifest,
    /// Path to config file
    pub config_path: PathBuf,
    /// Server directory
    pub server_dir: PathBuf,
}

/// Factory for MCP server init handlers
#[derive(Clone)]
struct McpServerInitFactory {
    manager: Arc<RwLock<crate::mcp::McpManager>>,
    server_name: String,
    config_path: PathBuf,
}

impl std::fmt::Debug for McpServerInitFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpServerInitFactory")
            .field("manager", &"<McpManager>")
            .field("server_name", &self.server_name)
            .field("config_path", &self.config_path)
            .finish()
    }
}

impl HookHandlerFactory for McpServerInitFactory {
    fn create(&self, _manifest: ExtensionManifest) -> Box<dyn HookHandler> {
        Box::new(McpServerInitHandler {
            manager: self.manager.clone(),
            server_name: self.server_name.clone(),
            config_path: self.config_path.clone(),
        })
    }
}

/// Handler that initializes MCP server on agent init
#[derive(Clone)]
struct McpServerInitHandler {
    manager: Arc<RwLock<crate::mcp::McpManager>>,
    server_name: String,
    config_path: PathBuf,
}

impl std::fmt::Debug for McpServerInitHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpServerInitHandler")
            .field("manager", &"<McpManager>")
            .field("server_name", &self.server_name)
            .field("config_path", &self.config_path)
            .finish()
    }
}

#[async_trait]
impl HookHandler for McpServerInitHandler {
    async fn handle(&self, _ctx: HookContext) -> HookResult {
        debug!(server_name = %self.server_name, "Initializing MCP server on agent init");
        
        // The MCP manager is already initialized with configs
        // This hook ensures the server is started if auto_start is enabled
        let manager = self.manager.read().await;
        
        match manager.get_server_state(&self.server_name).await {
            Ok(state) if state.running => {
                debug!(server_name = %self.server_name, "MCP server already running");
                HookResult::Continue(HookOutput::Unit)
            }
            _ => {
                // Server not running, try to start if auto_start
                drop(manager);
                let manager = self.manager.write().await;
                match manager.start_server(&self.server_name).await {
                    Ok(()) => {
                        info!(server_name = %self.server_name, "MCP server started");
                        HookResult::Continue(HookOutput::Unit)
                    }
                    Err(e) => {
                        warn!(server_name = %self.server_name, error = %e, "Failed to start MCP server");
                        // Don't fail, server will be started on demand
                        HookResult::Continue(HookOutput::Unit)
                    }
                }
            }
        }
    }

    fn hook_point(&self) -> HookPoint {
        HookPoint::AgentInit
    }

    fn priority(&self) -> i32 {
        MCP_HOOK_PRIORITY
    }

    fn name(&self) -> String {
        format!("McpServerInitHandler({})", self.server_name)
    }
}

/// Factory for MCP server shutdown handlers
#[derive(Clone)]
struct McpServerShutdownFactory {
    manager: Arc<RwLock<crate::mcp::McpManager>>,
    server_name: String,
}

impl std::fmt::Debug for McpServerShutdownFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpServerShutdownFactory")
            .field("manager", &"<McpManager>")
            .field("server_name", &self.server_name)
            .finish()
    }
}

impl HookHandlerFactory for McpServerShutdownFactory {
    fn create(&self, _manifest: ExtensionManifest) -> Box<dyn HookHandler> {
        Box::new(McpServerShutdownHandler {
            manager: self.manager.clone(),
            server_name: self.server_name.clone(),
        })
    }
}

/// Handler that stops MCP server on agent shutdown
#[derive(Clone)]
struct McpServerShutdownHandler {
    manager: Arc<RwLock<crate::mcp::McpManager>>,
    server_name: String,
}

impl std::fmt::Debug for McpServerShutdownHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpServerShutdownHandler")
            .field("manager", &"<McpManager>")
            .field("server_name", &self.server_name)
            .finish()
    }
}

#[async_trait]
impl HookHandler for McpServerShutdownHandler {
    async fn handle(&self, _ctx: HookContext) -> HookResult {
        debug!(server_name = %self.server_name, "Shutting down MCP server");
        
        let manager = self.manager.write().await;
        match manager.stop_server(&self.server_name).await {
            Ok(()) => {
                info!(server_name = %self.server_name, "MCP server stopped");
                HookResult::Continue(HookOutput::Unit)
            }
            Err(e) => {
                warn!(server_name = %self.server_name, error = %e, "Failed to stop MCP server");
                HookResult::Continue(HookOutput::Unit) // Non-fatal
            }
        }
    }

    fn hook_point(&self) -> HookPoint {
        HookPoint::AgentShutdown
    }

    fn priority(&self) -> i32 {
        MCP_HOOK_PRIORITY
    }

    fn name(&self) -> String {
        format!("McpServerShutdownHandler({})", self.server_name)
    }
}

/// Factory for MCP tool execution handlers
#[derive(Clone)]
struct McpToolExecuteFactory {
    manager: Arc<RwLock<crate::mcp::McpManager>>,
    server_name: String,
}

impl std::fmt::Debug for McpToolExecuteFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpToolExecuteFactory")
            .field("manager", &"<McpManager>")
            .field("server_name", &self.server_name)
            .finish()
    }
}

impl HookHandlerFactory for McpToolExecuteFactory {
    fn create(&self, _manifest: ExtensionManifest) -> Box<dyn HookHandler> {
        Box::new(McpToolExecuteHandler {
            manager: self.manager.clone(),
            server_name: self.server_name.clone(),
            tool_name: None, // Wildcard pattern handler
        })
    }
}

/// Handler that executes MCP tools
#[derive(Clone)]
struct McpToolExecuteHandler {
    manager: Arc<RwLock<crate::mcp::McpManager>>,
    server_name: String,
    /// Specific tool name (optional - for unified registry registration)
    /// If None, handles wildcard pattern mcp:{server}:*
    tool_name: Option<String>,
}

impl std::fmt::Debug for McpToolExecuteHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpToolExecuteHandler")
            .field("manager", &"<McpManager>")
            .field("server_name", &self.server_name)
            .field("tool_name", &self.tool_name)
            .finish()
    }
}

#[async_trait]
impl HookHandler for McpToolExecuteHandler {
    async fn handle(&self, ctx: HookContext) -> HookResult {
        // Parse tool name from pattern mcp:{server}:{tool}
        let (tool_name, params) = match ctx.as_tool_call() {
            Some((tool_name, params)) => {
                // Check if this is an MCP tool for our server
                let expected_prefix = format!("{}:{}", MCP_TOOL_PREFIX, self.server_name);
                if !tool_name.starts_with(&expected_prefix) {
                    return HookResult::PassThrough;
                }
                (tool_name.to_string(), params.clone())
            }
            None => return HookResult::PassThrough,
        };

        // Use specific tool name if set (unified registry), otherwise extract from pattern
        let actual_tool = match &self.tool_name {
            Some(name) => name.as_str(),
            None => tool_name
                .strip_prefix(&format!("{}:{}:", MCP_TOOL_PREFIX, self.server_name))
                .unwrap_or(&tool_name),
        };

        debug!(
            server_name = %self.server_name,
            tool_name = %actual_tool,
            "Executing MCP tool via Extension Framework"
        );

        // Get server configuration for reserved parameters
        let server_config = {
            let manager = self.manager.read().await;
            manager.get_server_config(&self.server_name).await
        };
        
        // Get reserved params config from server config
        let reserved_params = server_config
            .as_ref()
            .map(|c| c.reserved_parameters.clone())
            .unwrap_or_default();
        
        // Use basic object schema (MCP doesn't expose full JSON schema for validation)
        let tool_schema = serde_json::json!({"type": "object"});
        
        // Build execution config
        let exec_config = ToolExecutionConfig::new(reserved_params, tool_schema);
        
        // Use the unified ToolExecutionService for parameter injection and execution
        use crate::extensions::services::ToolExecutionService;
        
        // Validate that user didn't provide reserved params
        if let Err(e) = ToolExecutionService::validate_user_params(&params, &exec_config.reserved_params) {
            return HookResult::Error(e);
        }
        
        // Inject reserved parameters
        let merged_params = ToolExecutionService::inject_reserved_params(
            params,
            &exec_config.reserved_params,
            ctx.as_tool_context()
        );
        
        // Execute via MCP manager
        let manager = self.manager.read().await;
        let result = manager.call_tool(&self.server_name, actual_tool, merged_params).await;
        drop(manager);

        match result {
            Ok(mcp_result) => {
                // Convert MCP result to JSON
                let json_result = serde_json::json!({
                    "success": !mcp_result.is_error,
                    "contents": mcp_result.content.iter().map(|c| match c {
                        crate::mcp::types::ToolResultContent::Text(t) => serde_json::json!({
                            "type": "text",
                            "text": t.text
                        }),
                        crate::mcp::types::ToolResultContent::Image(i) => serde_json::json!({
                            "type": "image",
                            "data": i.data,
                            "mime_type": i.mime_type
                        }),
                        crate::mcp::types::ToolResultContent::Resource(r) => serde_json::json!({
                            "type": "resource",
                            "resource": r.resource
                        }),
                    }).collect::<Vec<_>>()
                });
                HookResult::Continue(HookOutput::Json(json_result))
            }
            Err(e) => {
                error!(server_name = %self.server_name, tool_name = %actual_tool, error = %e, "MCP tool execution failed");
                HookResult::Error(anyhow::anyhow!("MCP tool error: {}", e))
            }
        }
    }

    fn hook_point(&self) -> HookPoint {
        HookPoint::ToolExecute {
            tool_name: format!("{}:{}:*", MCP_TOOL_PREFIX, self.server_name),
        }
    }

    fn priority(&self) -> i32 {
        MCP_HOOK_PRIORITY
    }

    fn name(&self) -> String {
        format!("McpToolExecuteHandler({})", self.server_name)
    }
}

/// Factory for MCP async tool execution handlers
#[derive(Clone)]
struct McpToolExecuteAsyncFactory {
    manager: Arc<RwLock<crate::mcp::McpManager>>,
    server_name: String,
}

impl std::fmt::Debug for McpToolExecuteAsyncFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpToolExecuteAsyncFactory")
            .field("manager", &"<McpManager>")
            .field("server_name", &self.server_name)
            .finish()
    }
}

impl HookHandlerFactory for McpToolExecuteAsyncFactory {
    fn create(&self, _manifest: ExtensionManifest) -> Box<dyn HookHandler> {
        Box::new(McpToolExecuteAsyncHandler {
            manager: self.manager.clone(),
            server_name: self.server_name.clone(),
        })
    }
}

/// Handler that executes MCP tools asynchronously
#[derive(Clone)]
struct McpToolExecuteAsyncHandler {
    manager: Arc<RwLock<crate::mcp::McpManager>>,
    server_name: String,
}

impl std::fmt::Debug for McpToolExecuteAsyncHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpToolExecuteAsyncHandler")
            .field("manager", &"<McpManager>")
            .field("server_name", &self.server_name)
            .finish()
    }
}

#[async_trait]
impl HookHandler for McpToolExecuteAsyncHandler {
    async fn handle(&self, ctx: HookContext) -> HookResult {
        // Parse tool name and params from context
        let (tool_name, params) = match ctx.as_tool_call() {
            Some((tool_name, params)) => {
                let expected_prefix = format!("{}:{}", MCP_TOOL_PREFIX, self.server_name);
                if !tool_name.starts_with(&expected_prefix) {
                    return HookResult::PassThrough;
                }
                (tool_name.to_string(), params.clone())
            }
            None => return HookResult::PassThrough,
        };

        // Extract actual tool name
        let actual_tool = tool_name
            .strip_prefix(&format!("{}:{}:", MCP_TOOL_PREFIX, self.server_name))
            .unwrap_or(&tool_name);

        debug!(
            server_name = %self.server_name,
            tool_name = %actual_tool,
            "Executing MCP tool asynchronously"
        );

        // For MCP, we execute synchronously but wrap in async receipt
        // The actual async nature comes from the unified executor spawning
        let manager = self.manager.clone();
        let server_name = self.server_name.clone();
        let actual_tool = actual_tool.to_string();
        
        // Generate task ID for tracking
        let task_id = format!("mcp:{}:{}:{}", server_name, actual_tool, Uuid::new_v4());
        
        // Create receipt for async execution
        let receipt = AsyncReceipt {
            task_id: task_id.clone(),
            estimated_duration_secs: None,
            check_status_tool: format!("{}:{}", MCP_TOOL_PREFIX, server_name),
            metadata: Some(serde_json::json!({
                "server": server_name,
                "tool": actual_tool,
                "params": params,
            })),
        };

        HookResult::Continue(HookOutput::Receipt(receipt))
    }

    fn hook_point(&self) -> HookPoint {
        HookPoint::ToolExecuteAsync {
            tool_name: format!("{}:{}:*", MCP_TOOL_PREFIX, self.server_name),
        }
    }

    fn priority(&self) -> i32 {
        MCP_HOOK_PRIORITY
    }

    fn name(&self) -> String {
        format!("McpToolExecuteAsyncHandler({})", self.server_name)
    }
}

/// Factory for MCP tool status check handlers
#[derive(Clone)]
struct McpToolCheckStatusFactory {
    manager: Arc<RwLock<crate::mcp::McpManager>>,
    server_name: String,
}

impl std::fmt::Debug for McpToolCheckStatusFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpToolCheckStatusFactory")
            .field("manager", &"<McpManager>")
            .field("server_name", &self.server_name)
            .finish()
    }
}

impl HookHandlerFactory for McpToolCheckStatusFactory {
    fn create(&self, _manifest: ExtensionManifest) -> Box<dyn HookHandler> {
        Box::new(McpToolCheckStatusHandler {
            manager: self.manager.clone(),
            server_name: self.server_name.clone(),
        })
    }
}

/// Handler that checks status of async MCP tasks
#[derive(Clone)]
struct McpToolCheckStatusHandler {
    manager: Arc<RwLock<crate::mcp::McpManager>>,
    server_name: String,
}

impl std::fmt::Debug for McpToolCheckStatusHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpToolCheckStatusHandler")
            .field("manager", &"<McpManager>")
            .field("server_name", &self.server_name)
            .finish()
    }
}

#[async_trait]
impl HookHandler for McpToolCheckStatusHandler {
    async fn handle(&self, ctx: HookContext) -> HookResult {
        // Parse task status request
        let (task_id, _tool_name) = match ctx.as_task_status() {
            Some((task_id, tool_name)) => {
                let expected_prefix = format!("{}:{}", MCP_TOOL_PREFIX, self.server_name);
                if !tool_name.starts_with(&expected_prefix) {
                    return HookResult::PassThrough;
                }
                (task_id.to_string(), tool_name.to_string())
            }
            None => return HookResult::PassThrough,
        };

        debug!(
            server_name = %self.server_name,
            task_id = %task_id,
            "Checking MCP task status"
        );

        // For now, MCP doesn't have native async task tracking
        // We would need to query the MCP server if it supports async operations
        // For compatibility, return pending status
        HookResult::Continue(HookOutput::TaskStatus(AsyncTaskStatus::Pending))
    }

    fn hook_point(&self) -> HookPoint {
        HookPoint::ToolCheckStatus {
            tool_name: format!("{}:{}:*", MCP_TOOL_PREFIX, self.server_name),
        }
    }

    fn priority(&self) -> i32 {
        MCP_HOOK_PRIORITY
    }

    fn name(&self) -> String {
        format!("McpToolCheckStatusHandler({})", self.server_name)
    }
}

/// Factory for MCP tool cancel handlers
#[derive(Clone)]
struct McpToolCancelFactory {
    manager: Arc<RwLock<crate::mcp::McpManager>>,
    server_name: String,
}

impl std::fmt::Debug for McpToolCancelFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpToolCancelFactory")
            .field("manager", &"<McpManager>")
            .field("server_name", &self.server_name)
            .finish()
    }
}

impl HookHandlerFactory for McpToolCancelFactory {
    fn create(&self, _manifest: ExtensionManifest) -> Box<dyn HookHandler> {
        Box::new(McpToolCancelHandler {
            manager: self.manager.clone(),
            server_name: self.server_name.clone(),
        })
    }
}

/// Handler that cancels async MCP tasks
#[derive(Clone)]
struct McpToolCancelHandler {
    manager: Arc<RwLock<crate::mcp::McpManager>>,
    server_name: String,
}

impl std::fmt::Debug for McpToolCancelHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpToolCancelHandler")
            .field("manager", &"<McpManager>")
            .field("server_name", &self.server_name)
            .finish()
    }
}

#[async_trait]
impl HookHandler for McpToolCancelHandler {
    async fn handle(&self, ctx: HookContext) -> HookResult {
        // Parse task cancel request
        let (task_id, _tool_name) = match ctx.as_task_cancel() {
            Some((task_id, tool_name)) => {
                let expected_prefix = format!("{}:{}", MCP_TOOL_PREFIX, self.server_name);
                if !tool_name.starts_with(&expected_prefix) {
                    return HookResult::PassThrough;
                }
                (task_id.to_string(), tool_name.to_string())
            }
            None => return HookResult::PassThrough,
        };

        debug!(
            server_name = %self.server_name,
            task_id = %task_id,
            "Cancelling MCP task"
        );

        // MCP doesn't have native cancel support in the standard protocol
        // Return false to indicate cancellation not supported
        HookResult::Continue(HookOutput::Bool(false))
    }

    fn hook_point(&self) -> HookPoint {
        HookPoint::ToolCancel {
            tool_name: format!("{}:{}:*", MCP_TOOL_PREFIX, self.server_name),
        }
    }

    fn priority(&self) -> i32 {
        MCP_HOOK_PRIORITY
    }

    fn name(&self) -> String {
        format!("McpToolCancelHandler({})", self.server_name)
    }
}

/// Helper to load MCP servers from directory using the adapter
pub async fn load_servers_from_directory(
    path: &Path,
    manager: Arc<RwLock<crate::mcp::McpManager>>,
) -> Vec<DiscoveredMcpServer> {
    let adapter = McpAdapter::new(manager);
    adapter.discover_servers(path).await
}

/// Register MCP servers with an ExtensionCore
pub async fn register_servers_with_core(
    core: &crate::extensions::ExtensionCore,
    servers: Vec<DiscoveredMcpServer>,
    manager: Arc<RwLock<crate::mcp::McpManager>>,
) -> Result<Vec<HookId>> {
    let mut hook_ids = Vec::new();

    for server in servers {
        let extension_id = ExtensionId::new(&server.manifest.id.0);

        // Register agent init handler
        let init_handler = Arc::new(McpServerInitHandler {
            manager: manager.clone(),
            server_name: server.manifest.name.clone(),
            config_path: server.config_path.clone(),
        });

        let init_reg = core
            .register_hook(HookPoint::AgentInit, init_handler, &extension_id)
            .await?;
        hook_ids.push(init_reg.id);

        // Register agent shutdown handler
        let shutdown_handler = Arc::new(McpServerShutdownHandler {
            manager: manager.clone(),
            server_name: server.manifest.name.clone(),
        });

        let shutdown_reg = core
            .register_hook(HookPoint::AgentShutdown, shutdown_handler, &extension_id)
            .await?;
        hook_ids.push(shutdown_reg.id);

        // Register tool execution handler
        let exec_handler = Arc::new(McpToolExecuteHandler {
            manager: manager.clone(),
            server_name: server.manifest.name.clone(),
            tool_name: None, // Wildcard pattern handler
        });

        let exec_reg = core
            .register_hook(
                HookPoint::ToolExecute {
                    tool_name: format!("{}:{}:*", MCP_TOOL_PREFIX, server.manifest.name),
                },
                exec_handler,
                &extension_id,
            )
            .await?;
        hook_ids.push(exec_reg.id);

        // Register async tool execution handler
        let exec_async_handler = Arc::new(McpToolExecuteAsyncHandler {
            manager: manager.clone(),
            server_name: server.manifest.name.clone(),
        });

        let exec_async_reg = core
            .register_hook(
                HookPoint::ToolExecuteAsync {
                    tool_name: format!("{}:{}:*", MCP_TOOL_PREFIX, server.manifest.name),
                },
                exec_async_handler,
                &extension_id,
            )
            .await?;
        hook_ids.push(exec_async_reg.id);

        // Register check status handler
        let check_status_handler = Arc::new(McpToolCheckStatusHandler {
            manager: manager.clone(),
            server_name: server.manifest.name.clone(),
        });

        let check_status_reg = core
            .register_hook(
                HookPoint::ToolCheckStatus {
                    tool_name: format!("{}:{}:*", MCP_TOOL_PREFIX, server.manifest.name),
                },
                check_status_handler,
                &extension_id,
            )
            .await?;
        hook_ids.push(check_status_reg.id);

        // Register cancel handler
        let cancel_handler = Arc::new(McpToolCancelHandler {
            manager: manager.clone(),
            server_name: server.manifest.name.clone(),
        });

        let cancel_reg = core
            .register_hook(
                HookPoint::ToolCancel {
                    tool_name: format!("{}:{}:*", MCP_TOOL_PREFIX, server.manifest.name),
                },
                cancel_handler,
                &extension_id,
            )
            .await?;
        hook_ids.push(cancel_reg.id);

        info!(
            server_name = %server.manifest.name,
            hook_count = 6,
            "Registered MCP server with ExtensionCore (including async hooks)"
        );
    }

    Ok(hook_ids)
}

/// Convenience function to load and register MCP servers
pub async fn load_and_register_servers(
    core: &crate::extensions::ExtensionCore,
    servers_dir: impl AsRef<Path>,
    manager: Arc<RwLock<crate::mcp::McpManager>>,
) -> Result<usize> {
    let servers = load_servers_from_directory(servers_dir.as_ref(), manager.clone()).await;
    let hook_ids = register_servers_with_core(core, servers, manager).await?;
    Ok(hook_ids.len() / 6) // Each server registers 6 hooks
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_server_config(dir: &Path, name: &str) -> PathBuf {
        // MCP adapter discovers subdirectories with config.toml or config.json
        let server_dir = dir.join(name);
        std::fs::create_dir_all(&server_dir).unwrap();
        let config_path = server_dir.join("config.toml");
        // Note: McpConfig uses #[serde(rename = "server")] for the servers field
        let config = format!(
            r#"[[server]]
name = "{}"
transport = "stdio"
command = "echo"
args = ["test"]
auto_start = true
"#,
            name
        );
        std::fs::write(&config_path, config).unwrap();
        config_path
    }

    #[test]
    fn test_mcp_adapter_manifest_format() {
        let adapter = McpAdapter::with_default_manager();
        let format = adapter.manifest_format();

        assert!(matches!(format, ManifestFormat::Custom { .. }));
    }

    #[tokio::test]
    async fn test_discover_servers() {
        let temp = TempDir::new().unwrap();

        create_test_server_config(temp.path(), "server1");
        create_test_server_config(temp.path(), "server2");

        let adapter = McpAdapter::with_default_manager();
        let servers = adapter.discover_servers(temp.path()).await;

        assert_eq!(servers.len(), 2);
        assert!(servers.iter().any(|s| s.manifest.name == "server1"));
        assert!(servers.iter().any(|s| s.manifest.name == "server2"));
    }

    #[tokio::test]
    async fn test_parse_server_config() {
        let temp = TempDir::new().unwrap();
        let config_path = create_test_server_config(temp.path(), "filesystem");

        let adapter = McpAdapter::with_default_manager();
        let manifest = adapter.parse_server_config(&config_path).await.unwrap();

        assert_eq!(manifest.name, "filesystem");
        assert_eq!(manifest.extension_type, "mcp");
    }

    #[tokio::test]
    async fn test_mcp_tool_execute_handler() {
        // This test would require a running MCP server
        // For now, just verify the handler can be created
        let manager = Arc::new(RwLock::new(crate::mcp::McpManager::new(
            crate::mcp::McpConfig::default(),
        )));

        let handler = McpToolExecuteHandler {
            manager,
            server_name: "test".to_string(),
        };

        assert_eq!(handler.server_name, "test");
    }
}

