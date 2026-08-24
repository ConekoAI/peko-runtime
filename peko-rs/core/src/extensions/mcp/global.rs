//! Process-global `McpManager` accessor (ADR-047 §2.3).
//!
//! Phase 2 PR 2 deletes the framework-coupled `McpAdapter`. The
//! daemon-shared `McpManager` survives — the workspace scanner
//! ([`crate::extensions::mcp::workspace::load_workspace_mcp_servers`])
//! and the prompt context renderer both reach it through this global
//! accessor so the per-principal boot path doesn't have to thread the
//! manager through every `PrincipalContext` constructor.
//!
//! In standalone / test contexts the manager is lazy-initialised on
//! first access with an empty config; in production `init_global_mcp_manager_with_shared_resources`
//! is called once during daemon startup with the daemon-wide
//! `BackgroundRuntimeManager` + `McpClientRegistry` so MCP servers
//! are visible to `peko ext start/stop`.

use crate::common::vault::Vault;
use crate::extensions::mcp::protocol::manager::McpManager;
use crate::daemon::background_runtime::BackgroundRuntimeManager;
use crate::extensions::mcp::runtime::McpClientRegistry;
use std::sync::{Arc, OnceLock};
use tokio::sync::RwLock;

static GLOBAL_MCP_MANAGER: OnceLock<Arc<RwLock<McpManager>>> = OnceLock::new();

/// Get the process-global `McpManager`. Returns `None` when no daemon
/// has initialised it AND no caller has accessed it yet — the lazy
/// initialisation in the global is intentionally a no-op (the manager
/// is only meaningful once the daemon's shared resources exist). Tests
/// that need a manager should construct one inline.
#[must_use]
pub fn global_mcp_manager() -> Option<Arc<RwLock<McpManager>>> {
    GLOBAL_MCP_MANAGER.get().cloned()
}

/// Initialise the global MCP manager with daemon-shared resources.
///
/// This must be called exactly once during daemon startup, before any
/// caller reaches [`global_mcp_manager`]. Subsequent calls are
/// silently ignored (the `OnceLock` only takes the first value).
#[allow(clippy::too_many_arguments)]
pub fn init_global_mcp_manager_with_shared_resources(
    runtime_manager: Arc<BackgroundRuntimeManager>,
    client_registry: Arc<McpClientRegistry>,
    llm_resolver: Option<Arc<peko_providers::LlmResolver>>,
    vault: Option<Arc<Vault>>,
    principal_manager: Option<Arc<crate::principal::manager::PrincipalManager>>,
) {
    let config = crate::extensions::mcp::protocol::config::McpConfig::default();
    let manager = McpManager::with_shared_resources(
        config,
        runtime_manager,
        client_registry,
        llm_resolver,
        vault,
    );
    let mut manager = manager;
    if let Some(pm) = principal_manager {
        manager = manager.with_principal_manager(pm);
    }
    let _ = GLOBAL_MCP_MANAGER.set(Arc::new(RwLock::new(manager)));
}