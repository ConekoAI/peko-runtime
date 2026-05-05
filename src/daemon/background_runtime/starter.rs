//! Extension Runtime Starter trait
//!
//! Defines the interface for type-specific background runtime starters.
//! Each extension type that supports background runtimes (gateway, mcp, etc.)
//! implements `ExtensionRuntimeStarter` and registers itself with the
//! `ExtensionRuntimeStarterRegistry`.
//!
//! This eliminates hardcoded type checks in the IPC server — the server simply
//! asks the registry to start an extension by ID, and the registry dispatches
//! to the appropriate starter based on the extension's manifest.

use super::manager::BackgroundRuntimeManager;
use crate::agent::stateless_service::StatelessAgentService;
use crate::extensions::runtime::mcp_runtime_adapter::McpClientRegistry;
use crate::extensions::runtime::gateway_router::GatewayRouter;
use std::path::PathBuf;
use std::sync::Arc;

/// Context provided to a runtime starter when asked to start an extension.
///
/// Contains all daemon-scoped services the starter may need.
#[derive(Clone)]
pub struct StarterContext {
    /// Shared background runtime manager
    pub background_runtime_manager: Arc<BackgroundRuntimeManager>,
    /// Agent service for executing messages
    pub agent_service: Arc<StatelessAgentService>,
    /// Gateway router for channel→agent mapping
    pub gateway_router: Arc<GatewayRouter>,
    /// Shared MCP client registry
    pub mcp_client_registry: Arc<McpClientRegistry>,
    /// Data directory where extensions are installed
    pub data_dir: PathBuf,
}

impl std::fmt::Debug for StarterContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StarterContext")
            .field("data_dir", &self.data_dir)
            .finish_non_exhaustive()
    }
}

/// A type-specific starter that knows how to read an extension manifest
/// and launch its background runtime via BackgroundRuntimeManager.
#[async_trait::async_trait]
pub trait ExtensionRuntimeStarter: Send + Sync + std::fmt::Debug {
    /// The extension type this starter handles (e.g., "gateway", "mcp")
    fn extension_type(&self) -> &'static str;

    /// Start the background runtime for the given extension.
    ///
    /// The starter reads the extension manifest from disk, validates it,
    /// creates the appropriate BackgroundRuntimeAdapter + RuntimeSpawnConfig,
    /// and calls BackgroundRuntimeManager::start().
    async fn start(
        &self,
        extension_id: &str,
        ctx: &StarterContext,
    ) -> anyhow::Result<()>;

    /// Optional: called during daemon startup to auto-start extensions
    /// of this type. Return list of extension IDs that were auto-started.
    async fn auto_start(
        &self,
        _ctx: &StarterContext,
    ) -> anyhow::Result<Vec<String>> {
        Ok(Vec::new())
    }
}
