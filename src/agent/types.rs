//! Agent manager types and events

use crate::engine::state::AgentState;
use std::path::PathBuf;

/// Events emitted by the manager
#[derive(Debug, Clone)]
pub enum ManagerEvent {
    /// Agent spawned
    AgentSpawned { did: String, name: String },
    /// Agent started
    AgentStarted { did: String },
    /// Agent stopped
    AgentStopped { did: String },
    /// Agent crashed
    AgentCrashed { did: String, error: String },
    /// Agent registered (discovered)
    AgentDiscovered {
        did: String,
        capabilities: Vec<String>,
    },
    /// Context updated
    ContextUpdated { did: String },
    /// Agent exported
    AgentExported { did: String, path: PathBuf },
    /// Agent imported
    AgentImported { did: String, name: String },
}

/// Agent info with full details
#[derive(Debug, Clone)]
pub struct AgentInfo {
    /// Agent DID
    pub did: String,
    /// Agent name
    pub name: String,
    /// Current state
    pub state: AgentState,
    /// Capabilities
    pub capabilities: Vec<String>,
    /// Uptime (seconds)
    pub uptime_secs: u64,
    /// Identity info
    pub identity_info: IdentityInfo,
}

/// Identity information
#[derive(Debug, Clone)]
pub struct IdentityInfo {
    /// DID
    pub did: String,
    /// Scope (local, tenant, global)
    pub scope: String,
    /// Created at
    pub created_at: Option<String>,
}
