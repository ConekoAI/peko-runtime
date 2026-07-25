//! MCP Runtime Adapter
//!
//! Bridges MCP servers with the `BackgroundRuntimeManager` by implementing
//! `BackgroundRuntimeAdapter`. This adapter:
//! - Extracts stdin/stdout from a `RuntimeKind::Process` managed runtime
//! - Wraps them in an `McpClient`
//! - Performs JSON-RPC initialization and tool discovery
//! - Stores the client in a shared registry for later use by `McpManager`

use crate::common::vault::Vault;
use crate::daemon::background_runtime::adapter::{BackgroundRuntimeAdapter, CrashAction};
use crate::daemon::background_runtime::supervisor::ManagedRuntime;
use crate::extensions::mcp::protocol::{
    client::{ClientError, McpClient, ServerRequestHandler},
    config::McpServerConfig,
    transport::{McpTransport, SseTransport, StdioTransport, TransportError},
    types::{Prompt, Resource, Tool},
};
use anyhow::Context;
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Errors that can occur in the MCP runtime adapter
#[derive(Debug, thiserror::Error)]
pub enum McpRuntimeAdapterError {
    #[error(
        "Runtime kind is not supported by MCP adapter (expected process or external connection)"
    )]
    UnexpectedRuntimeKind,

    #[error("Transport error: {0}")]
    Transport(#[from] TransportError),

    #[error("Client error: {0}")]
    Client(#[from] ClientError),

    #[error("Initialization timeout")]
    InitTimeout,

    #[error("Client not found in registry for runtime: {0}")]
    ClientNotFound(String),
}

/// Result type for adapter operations
pub type Result<T> = std::result::Result<T, McpRuntimeAdapterError>;

/// Information about a running MCP server, stored in the registry.
#[derive(Clone, Debug)]
pub struct McpServerInfo {
    /// The MCP client for this server
    pub client: Arc<RwLock<McpClient>>,
    /// Discovered tools (cached after initialization)
    pub tools: Vec<Tool>,
    /// Discovered resources (cached after initialization)
    pub resources: Vec<Resource>,
    /// Discovered prompts (cached after initialization)
    pub prompts: Vec<Prompt>,
    /// Server info string (name + version)
    pub server_info: Option<String>,
    /// Optional instructions from the server's initialize response
    pub instructions: Option<String>,
}

/// Shared registry that maps runtime IDs to `McpServerInfo`.
///
/// This allows the `McpManager` (and other callers) to access clients
/// and discovered tools after the `BackgroundRuntimeManager` has started
/// the runtime.
#[derive(Clone, Debug)]
pub struct McpClientRegistry {
    servers: Arc<RwLock<HashMap<String, McpServerInfo>>>,
}

impl McpClientRegistry {
    /// Create a new empty registry
    #[must_use]
    pub fn new() -> Self {
        Self {
            servers: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Insert server info for the given runtime ID
    pub async fn insert(&self, runtime_id: String, info: McpServerInfo) {
        let mut servers = self.servers.write().await;
        servers.insert(runtime_id, info);
    }

    /// Get server info by runtime ID
    pub async fn get(&self, runtime_id: &str) -> Option<McpServerInfo> {
        let servers = self.servers.read().await;
        servers.get(runtime_id).cloned()
    }

    /// Get just the client by runtime ID
    pub async fn get_client(&self, runtime_id: &str) -> Option<Arc<RwLock<McpClient>>> {
        let servers = self.servers.read().await;
        servers.get(runtime_id).map(|info| info.client.clone())
    }

    /// Remove a server by runtime ID
    pub async fn remove(&self, runtime_id: &str) -> Option<McpServerInfo> {
        let mut servers = self.servers.write().await;
        servers.remove(runtime_id)
    }

    /// Check if a server exists for the given runtime ID
    pub async fn contains(&self, runtime_id: &str) -> bool {
        let servers = self.servers.read().await;
        servers.contains_key(runtime_id)
    }

    /// List all registered runtime IDs
    pub async fn list(&self) -> Vec<String> {
        let servers = self.servers.read().await;
        servers.keys().cloned().collect()
    }

    /// Get all tools from all registered servers
    pub async fn list_all_tools(&self) -> Vec<(String, Tool)> {
        let servers = self.servers.read().await;
        let mut all_tools = Vec::new();
        for (runtime_id, info) in servers.iter() {
            for tool in &info.tools {
                all_tools.push((runtime_id.clone(), tool.clone()));
            }
        }
        all_tools
    }

    /// Get all resources from all registered servers
    pub async fn list_all_resources(&self) -> Vec<(String, Resource)> {
        let servers = self.servers.read().await;
        let mut all_resources = Vec::new();
        for (runtime_id, info) in servers.iter() {
            for resource in &info.resources {
                all_resources.push((runtime_id.clone(), resource.clone()));
            }
        }
        all_resources
    }

    /// Get all prompts from all registered servers
    pub async fn list_all_prompts(&self) -> Vec<(String, Prompt)> {
        let servers = self.servers.read().await;
        let mut all_prompts = Vec::new();
        for (runtime_id, info) in servers.iter() {
            for prompt in &info.prompts {
                all_prompts.push((runtime_id.clone(), prompt.clone()));
            }
        }
        all_prompts
    }
}

impl Default for McpClientRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// On-disk cache for MCP capability metadata so agents can see tool, resource,
/// and prompt definitions even when a server is offline at agent-init time.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct McpCapabilityCache {
    /// Server info string cached at initialization
    pub server_info: Option<String>,
    /// Optional instructions from the server's initialize response
    pub instructions: Option<String>,
    /// Cached tool definitions
    pub tools: Vec<Tool>,
    /// Cached resource definitions
    pub resources: Vec<Resource>,
    /// Cached prompt definitions
    pub prompts: Vec<Prompt>,
}

impl McpCapabilityCache {
    /// Path to the per-server cache file inside an extension/server directory.
    pub fn cache_path(cwd: &Path, server_name: &str) -> std::path::PathBuf {
        cwd.join(format!(".peko-tools.{server_name}.json"))
    }

    /// Write capability metadata to the on-disk cache.
    pub async fn write(
        cwd: &Path,
        server_name: &str,
        server_info: Option<String>,
        instructions: Option<String>,
        tools: &[Tool],
        resources: &[Resource],
        prompts: &[Prompt],
    ) -> anyhow::Result<()> {
        let cache = McpCapabilityCache {
            server_info,
            instructions,
            tools: tools.to_vec(),
            resources: resources.to_vec(),
            prompts: prompts.to_vec(),
        };
        let path = Self::cache_path(cwd, server_name);
        let content = serde_json::to_string_pretty(&cache)?;
        tokio::fs::write(&path, content)
            .await
            .with_context(|| format!("Failed to write MCP capability cache to {path:?}"))?;
        Ok(())
    }

    /// Read capability metadata from the on-disk cache, if it exists.
    pub async fn read(cwd: &Path, server_name: &str) -> anyhow::Result<Option<McpCapabilityCache>> {
        let path = Self::cache_path(cwd, server_name);
        if !path.exists() {
            return Ok(None);
        }
        let content = tokio::fs::read_to_string(&path)
            .await
            .with_context(|| format!("Failed to read MCP capability cache from {path:?}"))?;
        let cache: McpCapabilityCache = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse MCP capability cache at {path:?}"))?;
        Ok(Some(cache))
    }
}

/// Build an `McpClient` from a transport, wiring in an optional server-request handler.
fn build_client(
    transport: impl McpTransport + 'static,
    request_handler: Option<Arc<dyn ServerRequestHandler>>,
) -> McpClient {
    match request_handler {
        Some(handler) => McpClient::with_handler(Box::new(transport), handler),
        None => McpClient::new(Box::new(transport)),
    }
}

/// Adapter that implements `BackgroundRuntimeAdapter` for MCP servers.
///
/// This adapter is given to `BackgroundRuntimeManager::start()` when
/// launching an MCP server process. It handles:
/// - JSON-RPC initialization after the OS process starts
/// - Tool discovery
/// - Periodic health checks via JSON-RPC ping
/// - Graceful shutdown via JSON-RPC exit notification
#[derive(Clone)]
pub struct McpRuntimeAdapter {
    /// Server configuration (timeouts, capabilities, etc.)
    server_config: McpServerConfig,
    /// Shared registry where the initialized client is stored
    client_registry: Arc<McpClientRegistry>,
    /// Optional handler for server-to-client requests (e.g. sampling).
    request_handler: Option<Arc<dyn ServerRequestHandler>>,
    /// Optional encrypted vault for OAuth token storage/refresh.
    vault: Option<Arc<Vault>>,
}

impl std::fmt::Debug for McpRuntimeAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpRuntimeAdapter")
            .field("server_config", &self.server_config)
            .field("has_request_handler", &self.request_handler.is_some())
            .field("has_vault", &self.vault.is_some())
            .finish_non_exhaustive()
    }
}

impl McpRuntimeAdapter {
    /// Create a new MCP runtime adapter
    ///
    /// # Arguments
    /// * `server_config` - Configuration for this MCP server
    /// * `client_registry` - Shared registry to store the client after initialization
    /// * `request_handler` - Optional handler for server-initiated requests such as
    ///   `sampling/createMessage`
    /// * `vault` - Optional encrypted vault for OAuth token storage/refresh
    #[must_use]
    pub fn new(
        server_config: McpServerConfig,
        client_registry: Arc<McpClientRegistry>,
        request_handler: Option<Arc<dyn ServerRequestHandler>>,
        vault: Option<Arc<Vault>>,
    ) -> Self {
        Self {
            server_config,
            client_registry,
            request_handler,
            vault,
        }
    }

    /// Get the server configuration
    #[must_use]
    pub fn config(&self) -> &McpServerConfig {
        &self.server_config
    }

    /// Get the client registry
    #[must_use]
    pub fn registry(&self) -> Arc<McpClientRegistry> {
        self.client_registry.clone()
    }
}

#[async_trait]
impl BackgroundRuntimeAdapter for McpRuntimeAdapter {
    fn clone_box(&self) -> Arc<dyn BackgroundRuntimeAdapter> {
        Arc::new(self.clone())
    }

    /// Called after the OS process has started.
    ///
    /// 1. Extract stdin/stdout from `RuntimeKind::Process`
    /// 2. Create `StdioTransport::from_handles`
    /// 3. Create `McpClient`, perform JSON-RPC initialization
    /// 4. Discover tools
    /// 5. Store the client in the shared registry
    async fn initialize(&self, runtime: &mut ManagedRuntime) -> anyhow::Result<()> {
        info!("Initializing MCP runtime adapter for '{}'", runtime.id);

        // Create the MCP client from the runtime kind (stdio process or external SSE connection)
        let mut client = match &mut runtime.kind {
            crate::daemon::background_runtime::supervisor::RuntimeKind::Process {
                stdin,
                stdout,
                pid,
                ..
            } => {
                // Take ownership of stdin/stdout from the runtime.
                // The supervisor no longer needs them directly after initialization;
                // the transport now owns them.
                let stdin = stdin.take().ok_or_else(|| {
                    anyhow::anyhow!("MCP runtime '{}': stdin already taken", runtime.id)
                })?;
                let stdout = stdout.take().ok_or_else(|| {
                    anyhow::anyhow!("MCP runtime '{}': stdout already taken", runtime.id)
                })?;
                let pid = *pid;
                let transport = StdioTransport::from_handles(stdin, stdout, pid);
                build_client(transport, self.request_handler.clone())
            }
            crate::daemon::background_runtime::supervisor::RuntimeKind::External {
                endpoint,
                connected,
            } => {
                let endpoint = endpoint.clone();
                let transport = SseTransport::connect_with_auth(
                    &endpoint,
                    self.server_config.auth.clone(),
                    self.vault.clone(),
                    runtime.id.clone(),
                )
                .await
                .map_err(|e| {
                    anyhow::anyhow!("SSE connection failed for '{}': {}", runtime.id, e)
                })?;
                *connected = true;
                build_client(transport, self.request_handler.clone())
            }
            _ => {
                anyhow::bail!(
                    "McpRuntimeAdapter only supports RuntimeKind::Process or RuntimeKind::External, got {:?}",
                    runtime.kind
                );
            }
        };

        let init_timeout = Duration::from_secs(self.server_config.init_timeout_secs);
        match tokio::time::timeout(init_timeout, client.initialize()).await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                anyhow::bail!(
                    "MCP client initialization failed for '{}': {}",
                    runtime.id,
                    e
                );
            }
            Err(_) => {
                anyhow::bail!("MCP client initialization timed out for '{}'", runtime.id);
            }
        };

        let server_info = client.server_info().cloned();
        let (server_name, server_version, instructions) = match &server_info {
            Some(info) => (
                info.server_info.name.clone(),
                info.server_info.version.clone(),
                info.instructions.clone(),
            ),
            None => ("unknown".to_string(), "unknown".to_string(), None),
        };

        info!(
            "MCP server '{}' initialized: {} v{}",
            runtime.id, server_name, server_version
        );

        // Discover tools if supported
        let tools = if client.supports_capability("tools") {
            match client.list_tools().await {
                Ok(tools) => {
                    debug!(
                        "MCP server '{}' discovered {} tools",
                        runtime.id,
                        tools.len()
                    );
                    tools
                }
                Err(e) => {
                    warn!("MCP server '{}' tool discovery failed: {}", runtime.id, e);
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };

        // Discover resources if supported
        let resources = if client.supports_capability("resources") {
            match client.list_resources().await {
                Ok(resources) => {
                    debug!(
                        "MCP server '{}' discovered {} resources",
                        runtime.id,
                        resources.len()
                    );
                    resources
                }
                Err(e) => {
                    warn!(
                        "MCP server '{}' resource discovery failed: {}",
                        runtime.id, e
                    );
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };

        // Discover prompts if supported
        let prompts = if client.supports_capability("prompts") {
            match client.list_prompts().await {
                Ok(prompts) => {
                    debug!(
                        "MCP server '{}' discovered {} prompts",
                        runtime.id,
                        prompts.len()
                    );
                    prompts
                }
                Err(e) => {
                    warn!("MCP server '{}' prompt discovery failed: {}", runtime.id, e);
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };

        // Store client and discovered capabilities in registry so McpManager can access them
        let server_info_str = format!("{} v{}", server_name, server_version);

        // Cache capability metadata so agents can see definitions even when the
        // server is offline at agent-init time.
        if let Some(ref cwd) = self.server_config.cwd {
            if let Err(e) = McpCapabilityCache::write(
                cwd,
                &runtime.id,
                Some(server_info_str.clone()),
                instructions.clone(),
                &tools,
                &resources,
                &prompts,
            )
            .await
            {
                warn!(
                    server_name = %runtime.id,
                    error = %e,
                    "Failed to write MCP capability cache"
                );
            }
        }

        let info = McpServerInfo {
            client: Arc::new(RwLock::new(client)),
            tools,
            resources,
            prompts,
            server_info: Some(server_info_str),
            instructions,
        };
        self.client_registry.insert(runtime.id.clone(), info).await;

        info!(
            "MCP runtime adapter for '{}' initialized successfully",
            runtime.id
        );
        Ok(())
    }

    /// Periodic health check — ping the MCP server via JSON-RPC.
    async fn health_check(&self, runtime: &ManagedRuntime) -> bool {
        let Some(info) = self.client_registry.get(&runtime.id).await else {
            warn!(
                "Health check for '{}': client not found in registry",
                runtime.id
            );
            return false;
        };

        let client_guard = info.client.read().await;
        let healthy = client_guard.ping().await.is_ok();
        drop(client_guard);

        if !healthy {
            warn!("MCP server '{}' health check failed", runtime.id);
        }

        healthy
    }

    /// When the runtime crashes, always request a restart.
    ///
    /// MCP servers are expected to be long-running; a crash is usually
    /// transient (e.g. OOM, segfault) and should be retried.
    async fn on_crash(&self, _runtime: &mut ManagedRuntime) -> CrashAction {
        info!("MCP runtime crashed — requesting restart");
        CrashAction::Restart
    }

    /// Graceful shutdown — send JSON-RPC exit notification and close client.
    async fn shutdown(&self, runtime: &mut ManagedRuntime) -> anyhow::Result<()> {
        info!("Shutting down MCP runtime '{}'", runtime.id);

        if let Some(info) = self.client_registry.remove(&runtime.id).await {
            let mut client_guard = info.client.write().await;
            if let Err(e) = client_guard.shutdown().await {
                warn!("Error shutting down MCP client for '{}': {}", runtime.id, e);
            }
        } else {
            debug!(
                "No client found in registry for '{}' during shutdown",
                runtime.id
            );
        }

        info!("MCP runtime '{}' shut down", runtime.id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_registry_insert_get_remove() {
        use crate::extensions::mcp::protocol::transport::InMemoryTransport;

        let registry = McpClientRegistry::new();

        // Create a dummy client
        let (transport, _other) = InMemoryTransport::pair();
        let client = McpClient::new(Box::new(transport));
        let info = McpServerInfo {
            client: Arc::new(RwLock::new(client)),
            tools: Vec::new(),
            resources: Vec::new(),
            prompts: Vec::new(),
            server_info: None,
            instructions: None,
        };

        // Insert
        registry.insert("test-server".to_string(), info).await;
        assert!(registry.contains("test-server").await);

        // Get client
        let got = registry.get_client("test-server").await;
        assert!(got.is_some());

        // Remove
        let removed = registry.remove("test-server").await;
        assert!(removed.is_some());
        assert!(!registry.contains("test-server").await);
    }

    #[tokio::test]
    async fn test_registry_list() {
        let registry = McpClientRegistry::new();
        let list = registry.list().await;
        assert!(list.is_empty());
    }

    #[test]
    fn test_adapter_new() {
        let registry = Arc::new(McpClientRegistry::new());
        let config = McpServerConfig::stdio("test", "echo", vec![]);
        let adapter = McpRuntimeAdapter::new(config, registry, None, None);

        assert_eq!(adapter.config().name, "test");
    }

    #[tokio::test]
    async fn test_capability_cache_roundtrip() {
        use crate::extensions::mcp::protocol::types::{Prompt, Resource, Tool};
        use serde_json::json;
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path();

        let tools = vec![
            Tool {
                name: "web_fetch".to_string(),
                description: "Fetch a URL".to_string(),
                input_schema: json!({"type": "object"}),
            },
            Tool {
                name: "web_search".to_string(),
                description: "Search the web".to_string(),
                input_schema: json!({"type": "object"}),
            },
        ];
        let resources = vec![Resource {
            uri: "file:///tmp/test.txt".to_string(),
            name: "test.txt".to_string(),
            description: Some("A test file".to_string()),
            mime_type: Some("text/plain".to_string()),
        }];
        let prompts = vec![Prompt {
            name: "review".to_string(),
            description: Some("Review code".to_string()),
            arguments: None,
        }];

        McpCapabilityCache::write(
            cwd,
            "test-server",
            Some("test-server v1.0.0".to_string()),
            Some("Use these tools for web access.".to_string()),
            &tools,
            &resources,
            &prompts,
        )
        .await
        .unwrap();

        let cache = McpCapabilityCache::read(cwd, "test-server")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cache.server_info, Some("test-server v1.0.0".to_string()));
        assert_eq!(
            cache.instructions,
            Some("Use these tools for web access.".to_string())
        );
        assert_eq!(cache.tools.len(), 2);
        assert!(cache.tools.iter().any(|t| t.name == "web_fetch"));
        assert_eq!(cache.resources.len(), 1);
        assert_eq!(cache.prompts.len(), 1);
    }
}
