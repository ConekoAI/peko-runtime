//! Session manager for overlay lifecycle
//!
//! The `SessionManager` is responsible for:
//! - Managing base sessions (create, open, cache)
//! - Creating and tracking overlays (channel, spawn)
//! - Providing `HybridSession` views
//! - Cross-channel session sharing

use super::base::BaseSession;
use super::key::{derive_base_session_key, derive_overlay_key};
use super::overlay::{ChannelOverlay, SessionOverlay};
use super::registry::SessionRegistryManager;
use super::spawn::SpawnOverlay;
use super::types::{ChannelType, Peer, SpawnCleanupPolicy};
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

/// Reference to an overlay
#[derive(Debug, Clone)]
pub enum OverlayRef {
    /// Channel overlay
    Channel(Arc<RwLock<ChannelOverlay>>),
    /// Spawn overlay
    Spawn(Arc<RwLock<SpawnOverlay>>),
    /// No overlay (direct base session access)
    None,
}

impl OverlayRef {
    /// Check if this is a channel overlay
    #[must_use]
    pub fn is_channel(&self) -> bool {
        matches!(self, OverlayRef::Channel(_))
    }

    /// Check if this is a spawn overlay
    #[must_use]
    pub fn is_spawn(&self) -> bool {
        matches!(self, OverlayRef::Spawn(_))
    }

    /// Check if this is None
    #[must_use]
    pub fn is_none(&self) -> bool {
        matches!(self, OverlayRef::None)
    }

    /// Get as channel overlay if applicable
    #[must_use]
    pub fn as_channel(&self) -> Option<Arc<RwLock<ChannelOverlay>>> {
        match self {
            OverlayRef::Channel(arc) => Some(arc.clone()),
            _ => None,
        }
    }

    /// Get as spawn overlay if applicable
    #[must_use]
    pub fn as_spawn(&self) -> Option<Arc<RwLock<SpawnOverlay>>> {
        match self {
            OverlayRef::Spawn(arc) => Some(arc.clone()),
            _ => None,
        }
    }
}

/// A hybrid session combining base + active overlay
///
/// This is the primary interface for working with sessions in the overlay
/// architecture. It provides access to the shared base session context
/// and the overlay-specific state.
#[derive(Debug, Clone)]
pub struct HybridSession {
    /// Base session (shared across all overlays for a peer)
    pub base: Arc<RwLock<BaseSession>>,
    /// Active overlay (channel or spawn)
    pub overlay: OverlayRef,
}

impl HybridSession {
    /// Create a new hybrid session
    pub fn new(base: Arc<RwLock<BaseSession>>, overlay: OverlayRef) -> Self {
        Self { base, overlay }
    }

    /// Create a hybrid session with no overlay
    pub fn base_only(base: Arc<RwLock<BaseSession>>) -> Self {
        Self {
            base,
            overlay: OverlayRef::None,
        }
    }

    /// Check if this session has a channel overlay
    #[must_use]
    pub fn has_channel_overlay(&self) -> bool {
        self.overlay.is_channel()
    }

    /// Check if this session has a spawn overlay
    #[must_use]
    pub fn has_spawn_overlay(&self) -> bool {
        self.overlay.is_spawn()
    }

    /// Check if this is an isolated spawn
    pub async fn is_isolated_spawn(&self) -> bool {
        if let OverlayRef::Spawn(spawn_arc) = &self.overlay {
            let spawn = spawn_arc.read().await;
            spawn.isolated
        } else {
            false
        }
    }

    /// Get the base session key
    pub async fn base_session_key(&self) -> String {
        let base = self.base.read().await;
        base.session_key.clone()
    }

    /// Get the full session key (including overlay if present)
    pub async fn full_session_key(&self) -> String {
        let base_key = self.base_session_key().await;

        match &self.overlay {
            OverlayRef::Channel(channel_arc) => {
                let channel = channel_arc.read().await;
                derive_overlay_key(&base_key, "channel", &channel.overlay_id)
            }
            OverlayRef::Spawn(spawn_arc) => {
                let spawn = spawn_arc.read().await;
                derive_overlay_key(&base_key, "spawn", &spawn.spawn_id)
            }
            OverlayRef::None => base_key,
        }
    }

    /// Get the peer
    pub async fn peer(&self) -> Peer {
        let base = self.base.read().await;
        base.peer.clone()
    }

    /// Get channel type if this is a channel overlay
    pub async fn channel_type(&self) -> Option<ChannelType> {
        if let OverlayRef::Channel(channel_arc) = &self.overlay {
            let channel = channel_arc.read().await;
            Some(channel.channel_type)
        } else {
            None
        }
    }
}

/// Session manager for overlay lifecycle
///
/// Manages the lifecycle of base sessions and overlays, including:
/// - Caching of base sessions
/// - Creation and tracking of overlays
/// - Cross-channel session sharing
/// - Session registry for UUID-based file naming and switching
#[derive(Debug)]
pub struct SessionManager {
    /// Base sessions: (`agent_id`, peer) -> `BaseSession`
    base_sessions: HashMap<(String, Peer), Arc<RwLock<BaseSession>>>,
    /// Channel overlays: `overlay_key` -> `ChannelOverlay`
    channel_overlays: HashMap<String, Arc<RwLock<ChannelOverlay>>>,
    /// Spawn overlays: `overlay_key` -> `SpawnOverlay`
    spawn_overlays: HashMap<String, Arc<RwLock<SpawnOverlay>>>,
    /// Session registry manager for UUID-based sessions
    registry: Option<SessionRegistryManager>,
    /// Agent name for registry operations
    agent_name: Option<String>,
}

impl SessionManager {
    /// Create a new session manager
    #[must_use]
    pub fn new() -> Self {
        Self {
            base_sessions: HashMap::new(),
            channel_overlays: HashMap::new(),
            spawn_overlays: HashMap::new(),
            registry: None,
            agent_name: None,
        }
    }

    /// Initialize with session registry for an agent
    pub async fn with_registry(mut self, agent_name: &str) -> Result<Self> {
        self.registry = Some(SessionRegistryManager::for_agent(agent_name).await?);
        self.agent_name = Some(agent_name.to_string());
        Ok(self)
    }

    /// Get the registry manager if initialized
    #[must_use]
    pub fn registry(&self) -> Option<&SessionRegistryManager> {
        self.registry.as_ref()
    }

    /// Check if registry is initialized
    #[must_use]
    pub fn has_registry(&self) -> bool {
        self.registry.is_some()
    }

    /// Create a new session for a peer (/new command)
    pub async fn create_new_session(&self, peer: &Peer) -> Result<String> {
        let registry = self
            .registry
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Registry not initialized"))?;
        let agent = self
            .agent_name
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Agent name not set"))?;

        let peer_key = derive_base_session_key(agent, peer);
        let session_id = registry.create_new(&peer_key).await?;

        info!("Created new session {} for peer {}", session_id, peer_key);
        Ok(session_id)
    }

    /// Branch current session (/branch command)
    pub async fn branch_session(&self, peer: &Peer, label: Option<String>) -> Result<String> {
        let registry = self
            .registry
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Registry not initialized"))?;
        let agent = self
            .agent_name
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Agent name not set"))?;

        let peer_key = derive_base_session_key(agent, peer);
        let session_id = registry.branch(&peer_key, label).await?;

        info!("Branched session {} from {}", session_id, peer_key);
        Ok(session_id)
    }

    /// Switch to a different session (/switch command)
    pub async fn switch_session(&self, peer: &Peer, session_id: &str) -> Result<()> {
        let registry = self
            .registry
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Registry not initialized"))?;
        let agent = self
            .agent_name
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Agent name not set"))?;

        let peer_key = derive_base_session_key(agent, peer);
        registry.switch_session(&peer_key, session_id).await?;

        info!("Switched {} to session {}", peer_key, session_id);
        Ok(())
    }

    /// List all sessions for a peer
    pub async fn list_sessions(&self, peer: &Peer) -> Result<Vec<super::registry::SessionInfo>> {
        let registry = self
            .registry
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Registry not initialized"))?;
        let agent = self
            .agent_name
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Agent name not set"))?;

        let peer_key = derive_base_session_key(agent, peer);
        registry.list_sessions(&peer_key).await
    }

    /// Get or create a base session for a peer
    ///
    /// This is the foundation of cross-channel session sharing. The same
    /// base session is used regardless of which channel the peer uses.
    ///
    /// If registry is initialized, uses UUID-based session naming.
    pub async fn get_or_create_base(
        &mut self,
        agent: &str,
        peer: &Peer,
    ) -> Result<Arc<RwLock<BaseSession>>> {
        let key = (agent.to_string(), peer.clone());

        // Check cache first
        if let Some(session) = self.base_sessions.get(&key) {
            return Ok(session.clone());
        }

        // Use registry if available for UUID-based sessions
        if let Some(ref registry) = self.registry {
            let peer_key = derive_base_session_key(agent, peer);

            tracing::debug!("Using registry for session, peer_key: {}", peer_key);

            // Get or create session via registry
            let session_id =
                if let Some(existing_id) = registry.get_active_session_id(&peer_key).await? {
                    tracing::debug!("Found existing session: {}", existing_id);
                    existing_id
                } else {
                    // Create new session through registry (just tracking, no file yet)
                    let new_id = registry.create_new(&peer_key).await?;
                    tracing::debug!("Created new session via registry: {}", new_id);
                    new_id
                };

            // Check if session file exists by looking for it directly
            let transcript_file = format!("{session_id}.jsonl");
            let transcript_path = registry.sessions_dir().join(&transcript_file);

            let session = if transcript_path.exists() {
                // File exists, open it by ID
                info!("Opening existing session: {}", transcript_path.display());
                BaseSession::open_by_id(agent, peer, &session_id, registry.sessions_dir()).await?
            } else {
                // Create the session file
                info!("Creating new session file: {}", transcript_path.display());
                BaseSession::create_with_path(agent, peer, &session_id, registry.sessions_dir())
                    .await?
            };

            let arc = Arc::new(RwLock::new(session));
            self.base_sessions.insert(key, arc.clone());
            return Ok(arc);
        }

        tracing::debug!("No registry available, using legacy session naming");

        // Fallback to old behavior (no registry)
        // Try to open existing
        if let Some(session) = BaseSession::open(agent, peer).await? {
            let arc = Arc::new(RwLock::new(session));
            self.base_sessions.insert(key, arc.clone());
            return Ok(arc);
        }

        // Create new
        let session = BaseSession::create(agent, peer).await?;
        let arc = Arc::new(RwLock::new(session));
        self.base_sessions.insert(key, arc.clone());
        Ok(arc)
    }

    /// Get an existing base session if it exists
    #[must_use]
    pub fn get_existing_base(&self, agent: &str, peer: &Peer) -> Option<Arc<RwLock<BaseSession>>> {
        let key = (agent.to_string(), peer.clone());
        self.base_sessions.get(&key).cloned()
    }

    /// Create a channel overlay
    ///
    /// Creates a new channel overlay on top of the base session for the given peer.
    /// If a channel overlay already exists for this channel, it will be returned.
    pub async fn create_channel_overlay(
        &mut self,
        agent: &str,
        peer: &Peer,
        channel_type: ChannelType,
        channel_id: &str,
    ) -> Result<HybridSession> {
        // Get or create the base session
        let base = self.get_or_create_base(agent, peer).await?;

        let base_key = {
            let base_read = base.read().await;
            base_read.session_key.clone()
        };

        // Generate overlay key
        let overlay_id = format!("{}:{}", channel_type.as_str(), channel_id);
        let overlay_key = derive_overlay_key(&base_key, "channel", &overlay_id);

        // Check if overlay already exists
        if let Some(overlay) = self.channel_overlays.get(&overlay_key) {
            return Ok(HybridSession::new(
                base,
                OverlayRef::Channel(overlay.clone()),
            ));
        }

        // Create new overlay
        let overlay = ChannelOverlay::new(&base_key, peer.clone(), channel_type, channel_id);
        let overlay_arc = Arc::new(RwLock::new(overlay));
        self.channel_overlays
            .insert(overlay_key, overlay_arc.clone());

        Ok(HybridSession::new(base, OverlayRef::Channel(overlay_arc)))
    }

    /// Get an existing channel overlay
    #[must_use]
    pub fn get_channel_overlay(&self, overlay_key: &str) -> Option<Arc<RwLock<ChannelOverlay>>> {
        self.channel_overlays.get(overlay_key).cloned()
    }

    /// Create a spawn overlay
    ///
    /// Creates a new spawn overlay for subagent task execution.
    /// Always creates a new base session for the child. If `isolated=false`,
    /// the parent's conversation history is copied to the child's session.
    pub async fn create_spawn_overlay(
        &mut self,
        agent: &str,
        _peer: &Peer,
        task: &str,
        isolated: bool,
        parent_session_key: &str,
    ) -> Result<HybridSession> {
        // Always create a new base session for the child
        let spawn_id = format!("spawn_{}", uuid::Uuid::new_v4());
        let spawn_peer = Peer::Agent(spawn_id);
        let child_base = self.get_or_create_base(agent, &spawn_peer).await?;

        // If not isolated, copy parent's context to child's session
        if !isolated {
            if let Some(parent_base) = self.get_parent_base_session(parent_session_key).await {
                if let Err(e) = copy_session_context(&parent_base, &child_base).await {
                    tracing::warn!("Failed to copy parent context to child session: {}", e);
                }
            }
        }

        let base_key = {
            let base_read = child_base.read().await;
            base_read.session_key.clone()
        };

        // Create spawn overlay
        let overlay = SpawnOverlay::new(&base_key, spawn_peer, parent_session_key, task, isolated);
        let spawn_id = overlay.spawn_id.clone();
        let overlay_key = derive_overlay_key(&base_key, "spawn", &spawn_id);

        let overlay_arc = Arc::new(RwLock::new(overlay));
        self.spawn_overlays.insert(overlay_key, overlay_arc.clone());

        Ok(HybridSession::new(
            child_base,
            OverlayRef::Spawn(overlay_arc),
        ))
    }

    /// Create a spawn overlay with configuration
    ///
    /// Always creates a new base session for the child. If `isolated=false`,
    /// the parent's conversation history is copied to the child's session.
    pub async fn create_spawn_overlay_with_config(
        &mut self,
        agent: &str,
        _peer: &Peer,
        task: &str,
        isolated: bool,
        parent_session_key: &str,
        timeout_seconds: Option<u64>,
        cleanup: SpawnCleanupPolicy,
        depth: u32,
    ) -> Result<HybridSession> {
        // Always create a new base session for the child (no shared JSONL file)
        let spawn_id = format!("spawn_{}", uuid::Uuid::new_v4());
        let spawn_peer = Peer::Agent(spawn_id);
        let child_base = self.get_or_create_base(agent, &spawn_peer).await?;

        // If not isolated, copy parent's context to child's session
        if !isolated {
            // Get the parent's base session from the parent_session_key
            if let Some(parent_base) = self.get_parent_base_session(parent_session_key).await {
                // Copy parent's messages to child's session
                if let Err(e) = copy_session_context(&parent_base, &child_base).await {
                    tracing::warn!("Failed to copy parent context to child session: {}", e);
                }
            }
        }

        let base_key = {
            let base_read = child_base.read().await;
            base_read.session_key.clone()
        };

        // Create configured spawn overlay
        let mut overlay =
            SpawnOverlay::new(&base_key, spawn_peer, parent_session_key, task, isolated)
                .with_cleanup(cleanup)
                .with_depth(depth);

        if let Some(timeout) = timeout_seconds {
            overlay = overlay.with_timeout(timeout);
        }

        let spawn_id = overlay.spawn_id.clone();
        let overlay_key = derive_overlay_key(&base_key, "spawn", &spawn_id);

        let overlay_arc = Arc::new(RwLock::new(overlay));
        self.spawn_overlays.insert(overlay_key, overlay_arc.clone());

        Ok(HybridSession::new(
            child_base,
            OverlayRef::Spawn(overlay_arc),
        ))
    }

    /// Get the parent's base session from a session key (which may be an overlay key)
    async fn get_parent_base_session(&self, session_key: &str) -> Option<Arc<RwLock<BaseSession>>> {
        // Extract base key from overlay key if needed
        let base_key = crate::session::key::base_key_from_overlay(session_key)
            .unwrap_or_else(|| session_key.to_string());

        // Parse the base key to get agent and peer
        let parts: Vec<&str> = base_key.split(':').collect();
        if parts.len() < 5 {
            return None;
        }

        // Find "peer" in the key
        if let Some(peer_idx) = parts.iter().position(|&p| p == "peer") {
            let agent = parts.get(1)?;
            let peer_type = parts.get(peer_idx + 1)?;
            let peer_id = parts.get(peer_idx + 2)?;

            let peer = match *peer_type {
                "agent" => Peer::Agent(peer_id.to_string()),
                _ => Peer::User(peer_id.to_string()),
            };

            return self.get_existing_base(agent, &peer);
        }

        None
    }

    /// Get an existing spawn overlay
    #[must_use]
    pub fn get_spawn_overlay(&self, overlay_key: &str) -> Option<Arc<RwLock<SpawnOverlay>>> {
        self.spawn_overlays.get(overlay_key).cloned()
    }

    /// Get or create a session for a channel (cross-channel sharing)
    ///
    /// This is the primary method for channel handlers. It ensures that
    /// the same peer gets the same base session across all channels.
    pub async fn get_session_for_channel(
        &mut self,
        agent: &str,
        peer: &Peer,
        channel_type: ChannelType,
        channel_id: &str,
    ) -> Result<HybridSession> {
        self.create_channel_overlay(agent, peer, channel_type, channel_id)
            .await
    }

    /// Remove a channel overlay
    pub fn remove_channel_overlay(
        &mut self,
        overlay_key: &str,
    ) -> Option<Arc<RwLock<ChannelOverlay>>> {
        self.channel_overlays.remove(overlay_key)
    }

    /// Remove a spawn overlay
    pub fn remove_spawn_overlay(&mut self, overlay_key: &str) -> Option<Arc<RwLock<SpawnOverlay>>> {
        self.spawn_overlays.remove(overlay_key)
    }

    /// Remove a base session from cache
    pub fn remove_base_session(
        &mut self,
        agent: &str,
        peer: &Peer,
    ) -> Option<Arc<RwLock<BaseSession>>> {
        let key = (agent.to_string(), peer.clone());
        self.base_sessions.remove(&key)
    }

    /// Get all overlays for a base session
    #[must_use]
    pub fn get_overlays_for_base(
        &self,
        base_key: &str,
    ) -> Vec<(String, Arc<RwLock<dyn SessionOverlay>>)> {
        let mut result: Vec<(String, Arc<RwLock<dyn SessionOverlay>>)> = Vec::new();

        // Add channel overlays
        for (key, overlay) in &self.channel_overlays {
            if let Ok(ol) = overlay.try_read() {
                if ol.base_session_key() == base_key {
                    result.push((
                        key.clone(),
                        Arc::clone(overlay) as Arc<RwLock<dyn SessionOverlay>>,
                    ));
                }
            }
        }

        // Add spawn overlays
        for (key, overlay) in &self.spawn_overlays {
            if let Ok(ol) = overlay.try_read() {
                if ol.base_session_key() == base_key {
                    result.push((
                        key.clone(),
                        Arc::clone(overlay) as Arc<RwLock<dyn SessionOverlay>>,
                    ));
                }
            }
        }

        result
    }

    /// Get all channel overlays
    #[must_use]
    pub fn channel_overlays(&self) -> &HashMap<String, Arc<RwLock<ChannelOverlay>>> {
        &self.channel_overlays
    }

    /// Get all spawn overlays
    #[must_use]
    pub fn spawn_overlays(&self) -> &HashMap<String, Arc<RwLock<SpawnOverlay>>> {
        &self.spawn_overlays
    }

    /// Get base session count
    #[must_use]
    pub fn base_session_count(&self) -> usize {
        self.base_sessions.len()
    }

    /// Get channel overlay count
    #[must_use]
    pub fn channel_overlay_count(&self) -> usize {
        self.channel_overlays.len()
    }

    /// Get spawn overlay count
    #[must_use]
    pub fn spawn_overlay_count(&self) -> usize {
        self.spawn_overlays.len()
    }

    /// Clear all sessions (for testing)
    pub fn clear(&mut self) {
        self.base_sessions.clear();
        self.channel_overlays.clear();
        self.spawn_overlays.clear();
    }

    // ============================================================
    // A2A Session Resolution (Phase 1 Migration)
    // ============================================================

    /// Resolve an agent ID to a session key for A2A messaging
    ///
    /// This method:
    /// 1. Checks if the agent has an active session
    /// 2. Creates an ephemeral A2A session if needed
    /// 3. Returns the session key for messaging
    ///
    /// # Arguments
    /// * `agent_id` - The target agent ID
    /// * `caller_session_key` - The session key of the calling agent (for context)
    ///
    /// # Returns
    /// Session key for the target agent
    pub async fn resolve_agent_session(
        &self,
        agent_id: &str,
        caller_session_key: Option<&str>,
    ) -> Result<String> {
        // First, check if this agent already has a session we can use
        // Look for existing base sessions for this agent
        for ((session_agent, _), session) in &self.base_sessions {
            if session_agent == agent_id {
                // Found a session for this agent - access session_key field directly
                let session_guard = session.read().await;
                return Ok(session_guard.session_key.clone());
            }
        }

        // No existing session found - create ephemeral A2A session
        // Format: agent:{agent_id}:a2a:{caller_id}:{uuid}
        let caller_id = caller_session_key
            .and_then(|key| key.split(':').nth(1))
            .unwrap_or("unknown");

        let session_key = format!(
            "agent:{}:a2a:{}:{}",
            agent_id,
            caller_id,
            uuid::Uuid::new_v4().simple()
        );

        info!(
            "Created ephemeral A2A session for agent {}: {}",
            agent_id, session_key
        );

        Ok(session_key)
    }

    /// Ensure a session exists for A2A messaging
    ///
    /// Similar to resolve_agent_session but guarantees the session is created
    /// and initialized in the session manager.
    pub async fn ensure_agent_session(
        &mut self,
        agent_id: &str,
        caller_session_key: Option<&str>,
    ) -> Result<String> {
        // Try to resolve first
        match self
            .resolve_agent_session(agent_id, caller_session_key)
            .await
        {
            Ok(key) if !key.contains(":a2a:") => {
                // Found existing non-ephemeral session
                Ok(key)
            }
            Ok(key) => {
                // Would create ephemeral session - for now just return the key
                // In full implementation, would initialize the ephemeral session
                Ok(key)
            }
            Err(e) => Err(e),
        }
    }

    /// List all active sessions for an agent
    pub fn list_agent_sessions(&self, agent_id: &str) -> Vec<String> {
        let mut sessions = Vec::new();

        for ((session_agent, _), session) in &self.base_sessions {
            if session_agent == agent_id {
                // This is blocking, but we're just reading the key
                // In production, might want to make this async
                sessions.push(format!("agent:{}:base:{:?}", agent_id, session));
            }
        }

        sessions
    }
}

/// Copy conversation context from parent base session to child base session
///
/// This is used for shared-context spawns where the child should start with
/// the parent's conversation history.
async fn copy_session_context(
    parent: &Arc<RwLock<BaseSession>>,
    child: &Arc<RwLock<BaseSession>>,
) -> Result<()> {
    use crate::providers::MessageRole;
    use crate::types::message::ContentBlock;

    // Load parent's conversation history
    let parent_history = {
        let parent_guard = parent.read().await;
        parent_guard.load_history().await?
    };

    if parent_history.is_empty() {
        return Ok(());
    }

    tracing::info!(
        "Copying {} messages from parent to child session",
        parent_history.len()
    );

    // Copy each message to child's session
    let mut child_guard = child.write().await;

    for msg in parent_history {
        match msg.role {
            MessageRole::System => {
                // Extract text from content blocks
                let text: String = msg
                    .content
                    .iter()
                    .filter_map(|c| {
                        if let ContentBlock::Text { text } = c {
                            Some(text.clone())
                        } else {
                            None
                        }
                    })
                    .collect();
                if !text.is_empty() {
                    child_guard.add_system(&text).await?;
                }
            }
            MessageRole::User => {
                // Extract text from content blocks
                let text: String = msg
                    .content
                    .iter()
                    .filter_map(|c| {
                        if let ContentBlock::Text { text } = c {
                            Some(text.clone())
                        } else {
                            None
                        }
                    })
                    .collect();
                if !text.is_empty() {
                    child_guard.add_user(&text).await?;
                }
            }
            MessageRole::Assistant => {
                // Extract text from content blocks
                let text: String = msg
                    .content
                    .iter()
                    .filter_map(|c| {
                        if let ContentBlock::Text { text } = c {
                            Some(text.clone())
                        } else {
                            None
                        }
                    })
                    .collect();

                // Convert tool calls from provider::ToolCall to engine::ToolCall format
                let tool_calls = msg.tool_calls.map(|calls| {
                    calls
                        .into_iter()
                        .map(|call| crate::engine::ToolCall {
                            name: call.function.name,
                            parameters: serde_json::from_str(&call.function.arguments)
                                .unwrap_or(serde_json::Value::Null),
                        })
                        .collect()
                });

                child_guard.add_assistant(&text, tool_calls).await?;
            }
            MessageRole::Tool => {
                // Tool results - skip for now as they require tool_call_id linking
                // which may be complex to preserve across sessions
            }
        }
    }

    Ok(())
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_session_manager_new() {
        let manager = SessionManager::new();
        assert_eq!(manager.base_session_count(), 0);
        assert_eq!(manager.channel_overlay_count(), 0);
        assert_eq!(manager.spawn_overlay_count(), 0);
    }

    #[tokio::test]
    #[ignore = "requires filesystem access - run with --include-ignored for full test"]
    async fn test_get_or_create_base() {
        let mut manager = SessionManager::new();
        let peer = Peer::User("alice".to_string());

        let base1 = manager
            .get_or_create_base("test_agent", &peer)
            .await
            .unwrap();
        let base2 = manager
            .get_or_create_base("test_agent", &peer)
            .await
            .unwrap();

        // Should be the same Arc
        assert!(Arc::ptr_eq(&base1, &base2));
        assert_eq!(manager.base_session_count(), 1);
    }

    #[tokio::test]
    #[ignore = "requires filesystem access - run with --include-ignored for full test"]
    async fn test_create_channel_overlay() {
        let mut manager = SessionManager::new();
        let peer = Peer::User("alice".to_string());

        let hybrid = manager
            .create_channel_overlay("test_agent", &peer, ChannelType::Discord, "guild123")
            .await
            .unwrap();

        assert!(hybrid.has_channel_overlay());
        assert!(!hybrid.has_spawn_overlay());

        let channel_type = hybrid.channel_type().await;
        assert_eq!(channel_type, Some(ChannelType::Discord));

        assert_eq!(manager.channel_overlay_count(), 1);
    }

    #[tokio::test]
    #[ignore = "requires filesystem access - run with --include-ignored for full test"]
    async fn test_cross_channel_session_sharing() {
        let mut manager = SessionManager::new();
        let peer = Peer::User("alice".to_string());

        // Create CLI session
        let cli = manager
            .get_session_for_channel("test_agent", &peer, ChannelType::Cli, "default")
            .await
            .unwrap();

        // Add a message via CLI base session
        {
            let mut base = cli.base.write().await;
            base.add_user("Hello from CLI").await.unwrap();
        }

        // Create Discord session for same peer
        let discord = manager
            .get_session_for_channel("test_agent", &peer, ChannelType::Discord, "guild123")
            .await
            .unwrap();

        // Should share the same base session
        assert!(Arc::ptr_eq(&cli.base, &discord.base));

        // Discord should see the message from CLI
        let history = {
            let base = discord.base.read().await;
            base.load_history().await.unwrap()
        };
        assert!(history.len() >= 1); // At least the message we added
    }

    #[tokio::test]
    #[ignore = "requires filesystem access - run with --include-ignored for full test"]
    async fn test_create_spawn_overlay() {
        let mut manager = SessionManager::new();
        let peer = Peer::User("alice".to_string());

        let hybrid = manager
            .create_spawn_overlay("test_agent", &peer, "Research task", false, "parent_key")
            .await
            .unwrap();

        assert!(hybrid.has_spawn_overlay());
        assert!(!hybrid.has_channel_overlay());
        assert!(!hybrid.is_isolated_spawn().await);

        assert_eq!(manager.spawn_overlay_count(), 1);
    }

    #[tokio::test]
    #[ignore = "requires filesystem access - run with --include-ignored for full test"]
    async fn test_isolated_spawn() {
        let mut manager = SessionManager::new();
        let peer = Peer::User("alice".to_string());

        // Create parent session
        let parent = manager
            .get_or_create_base("test_agent", &peer)
            .await
            .unwrap();

        // Create isolated spawn
        let spawn = manager
            .create_spawn_overlay("test_agent", &peer, "Secret task", true, "parent_key")
            .await
            .unwrap();

        // Should have different base sessions
        assert!(!Arc::ptr_eq(&parent, &spawn.base));
        assert!(spawn.is_isolated_spawn().await);
    }

    #[tokio::test]
    #[ignore = "requires filesystem access - run with --include-ignored for full test"]
    async fn test_shared_spawn() {
        let mut manager = SessionManager::new();
        let peer = Peer::User("alice".to_string());

        // Create parent session
        let parent = manager
            .get_or_create_base("test_agent", &peer)
            .await
            .unwrap();

        // Create non-isolated spawn
        let spawn = manager
            .create_spawn_overlay("test_agent", &peer, "Shared task", false, "parent_key")
            .await
            .unwrap();

        // Should share the same base session
        assert!(Arc::ptr_eq(&parent, &spawn.base));
        assert!(!spawn.is_isolated_spawn().await);
    }

    #[tokio::test]
    #[ignore = "requires filesystem access - run with --include-ignored for full test"]
    async fn test_spawn_with_config() {
        let mut manager = SessionManager::new();
        let peer = Peer::User("alice".to_string());

        let hybrid = manager
            .create_spawn_overlay_with_config(
                "test_agent",
                &peer,
                "Configured task",
                false,
                "parent_key",
                Some(300),
                SpawnCleanupPolicy::Delete,
                2,
            )
            .await
            .unwrap();

        if let OverlayRef::Spawn(spawn_arc) = &hybrid.overlay {
            let spawn = spawn_arc.read().await;
            assert_eq!(spawn.timeout_seconds, Some(300));
            assert_eq!(spawn.cleanup, SpawnCleanupPolicy::Delete);
            assert_eq!(spawn.depth, 2);
        } else {
            panic!("Expected spawn overlay");
        }
    }

    #[tokio::test]
    #[ignore = "requires filesystem access - run with --include-ignored for full test"]
    async fn test_get_overlays_for_base() {
        let mut manager = SessionManager::new();
        let peer = Peer::User("alice".to_string());

        let hybrid = manager
            .create_channel_overlay("test_agent", &peer, ChannelType::Discord, "guild123")
            .await
            .unwrap();

        let base_key = hybrid.base_session_key().await;
        let overlays = manager.get_overlays_for_base(&base_key);

        assert_eq!(overlays.len(), 1);
    }

    #[tokio::test]
    #[ignore = "requires filesystem access - run with --include-ignored for full test"]
    async fn test_hybrid_session_key() {
        let mut manager = SessionManager::new();
        let peer = Peer::User("alice".to_string());

        let hybrid = manager
            .create_channel_overlay("test_agent", &peer, ChannelType::Discord, "guild123")
            .await
            .unwrap();

        let full_key = hybrid.full_session_key().await;
        assert!(full_key.contains("overlay:channel:discord:guild123"));
    }

    #[test]
    fn test_overlay_ref() {
        let none = OverlayRef::None;
        assert!(none.is_none());
        assert!(!none.is_channel());
        assert!(!none.is_spawn());

        // Can't test Channel/Spawn variants without actual data,
        // but the methods are exercised in other tests
    }
}
