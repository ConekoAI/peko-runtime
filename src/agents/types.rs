//! Agent manager types and events

use crate::engine::state::AgentState;
use chrono::{DateTime, Utc};
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
    /// Extensions (enabled extension names)
    pub extensions: Vec<String>,
    /// Uptime (seconds)
    pub uptime_secs: u64,
    /// Identity info
    pub identity_info: IdentityInfo,
    /// Image reference (e.g., "researcher:v2")
    pub image_ref: Option<String>,
    /// Image digest (SHA-256)
    pub image_digest: Option<String>,
    /// Active session ID
    pub active_session_id: Option<String>,
    /// Creation timestamp
    pub created_at: DateTime<Utc>,
    /// Skills
    pub skills: Option<Vec<String>>,
}

impl AgentInfo {
    /// Create a new `AgentInfo` with required fields
    #[must_use]
    pub fn new(did: String, name: String, state: AgentState) -> Self {
        Self {
            did: did.clone(),
            name,
            state,
            extensions: Vec::new(),
            uptime_secs: 0,
            identity_info: IdentityInfo {
                did,
                scope: "local".to_string(),
                created_at: None,
            },
            image_ref: None,
            image_digest: None,
            active_session_id: None,
            created_at: Utc::now(),
            skills: None,
        }
    }

    /// Set image reference
    #[must_use]
    pub fn with_image_ref(mut self, image_ref: String) -> Self {
        self.image_ref = Some(image_ref);
        self
    }
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
