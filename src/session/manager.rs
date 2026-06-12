//! Session manager for overlay lifecycle
//!
//! The `SessionManager` is responsible for SESSION LIFECYCLE only:
//! - Managing base sessions (create, open, cache)
//! - Creating and tracking overlays (channel, spawn)
//! - Providing `SessionHandle` views with overlay awareness
//! - Cross-channel session sharing
//! - Session branching, switching, and deletion
//!
//! For SESSION OPERATIONS (messages, metadata updates), obtain a `SessionHandle`
//! via `open_session()` and use its methods.
//!
//! # Architecture
//!
//! ```text
//! SessionManager (lifecycle)
//!        │
//!        │ open_session()
//!        ▼
//! SessionHandle (operations)
//!        │
//!        ▼
//! MetadataController (persistence)
//! ```
//!
//! ## Responsibility Boundaries
//!
//! | Operation | Use This | Via |
//! |-----------|----------|-----|
//! | Create session | `SessionManager::create_session()` | Returns `SessionHandle` |
//! | Open session | `SessionManager::open_session()` | Returns `Option<SessionHandle>` |
//! | Branch session | `SessionManager::branch_session*()` | Internally uses handles |
//! | Read metadata (lightweight) | `SessionManager::get_session_metadata()` | Direct controller access |
//! | Record token usage | `SessionHandle::record_usage()` | Requires valid handle |
//! | Add messages | `SessionHandle::add_*()` | Requires valid handle |
//!
//! # Single Point of Truth
//!
//! The `SessionManager` is the SOLE authority for session lifecycle operations.
//! The `MetadataController` is the SOLE authority for session metadata.
//! All session listings are verified for consistency.

use super::index::{SessionEntry, SessionIndex};
use super::jsonl::SessionStorage;
use super::key::{derive_base_session_key, derive_overlay_key};
use super::metadata::SessionMetadata;
use super::metadata_controller::MetadataController;
use super::overlay::{ChannelOverlay, SessionOverlay};
use super::spawn::SpawnOverlay;
use super::types::{ChannelType, Peer, SpawnCleanupPolicy};
use super::unified::Session;
use crate::common::paths::PathResolver;
use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info};

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

/// Opaque handle to a managed session
///
/// External code uses this handle to interact with sessions.
/// All metadata operations go through a shared `MetadataController` to ensure
/// consistency and avoid circular references.
#[derive(Debug, Clone)]
pub struct SessionHandle {
    session_id: String,
    base: Arc<RwLock<Session>>,
    overlay: Option<OverlayRef>,
    /// Shared metadata controller for metadata operations
    /// This avoids the circular reference from holding `SessionManager`
    metadata: Arc<RwLock<MetadataController>>,
}

impl SessionHandle {
    /// Create a new session handle
    pub(crate) fn new(
        session_id: impl Into<String>,
        base: Arc<RwLock<Session>>,
        overlay: Option<OverlayRef>,
        metadata: Arc<RwLock<MetadataController>>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            base,
            overlay,
            metadata,
        }
    }

    /// Get the session ID
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Get the base session (for internal operations)
    pub(crate) fn base(&self) -> &Arc<RwLock<Session>> {
        &self.base
    }

    /// Get the overlay reference (for internal/test access)
    #[cfg(test)]
    pub(crate) fn overlay(&self) -> Option<&OverlayRef> {
        self.overlay.as_ref()
    }

    /// Check if this handle has an overlay
    #[must_use]
    pub fn has_overlay(&self) -> bool {
        self.overlay.is_some()
    }

    /// Check if this handle has a channel overlay
    #[must_use]
    pub fn has_channel_overlay(&self) -> bool {
        matches!(&self.overlay, Some(OverlayRef::Channel(_)))
    }

    /// Check if this handle has a spawn overlay
    #[must_use]
    pub fn has_spawn_overlay(&self) -> bool {
        matches!(&self.overlay, Some(OverlayRef::Spawn(_)))
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
            Some(OverlayRef::Channel(channel_arc)) => {
                let channel = channel_arc.read().await;
                derive_overlay_key(&base_key, "channel", &channel.overlay_id)
            }
            Some(OverlayRef::Spawn(spawn_arc)) => {
                let spawn = spawn_arc.read().await;
                derive_overlay_key(&base_key, "spawn", &spawn.spawn_id)
            }
            _ => base_key,
        }
    }

    /// Get the peer
    pub async fn peer(&self) -> Peer {
        let base = self.base.read().await;
        base.peer.clone()
    }

    /// Get channel type if this has a channel overlay
    pub async fn channel_type(&self) -> Option<ChannelType> {
        if let Some(OverlayRef::Channel(channel_arc)) = &self.overlay {
            let channel = channel_arc.read().await;
            Some(channel.channel_type)
        } else {
            None
        }
    }

    /// Check if this is an isolated spawn
    pub async fn is_isolated(&self) -> bool {
        if let Some(OverlayRef::Spawn(spawn_arc)) = &self.overlay {
            let spawn = spawn_arc.read().await;
            spawn.isolated
        } else {
            false
        }
    }

    /// Get channel-specific state (if channel overlay)
    pub async fn get_channel_state(&self, key: &str) -> Option<serde_json::Value> {
        if let Some(OverlayRef::Channel(channel_arc)) = &self.overlay {
            let channel = channel_arc.read().await;
            channel.get(key).cloned()
        } else {
            None
        }
    }

    /// Set channel-specific state (if channel overlay)
    pub async fn set_channel_state(
        &self,
        key: impl Into<String>,
        value: serde_json::Value,
    ) -> bool {
        if let Some(OverlayRef::Channel(channel_arc)) = &self.overlay {
            let mut channel = channel_arc.write().await;
            channel.set(key, value);
            true
        } else {
            false
        }
    }

    /// Get spawn status (if spawn overlay)
    pub async fn get_spawn_status(&self) -> Option<super::spawn::SpawnStatus> {
        if let Some(OverlayRef::Spawn(spawn_arc)) = &self.overlay {
            let spawn = spawn_arc.read().await;
            Some(spawn.status)
        } else {
            None
        }
    }

    /// Update spawn status (if spawn overlay)
    pub async fn update_spawn_status<F>(&self, f: F) -> bool
    where
        F: FnOnce(&mut super::spawn::SpawnOverlay),
    {
        if let Some(OverlayRef::Spawn(spawn_arc)) = &self.overlay {
            let mut spawn = spawn_arc.write().await;
            f(&mut spawn);
            true
        } else {
            false
        }
    }

    /// Add a user message to the session
    ///
    /// Note: Metadata updates are handled by `MetadataController` at turn boundary
    pub async fn add_user(&self, content: impl Into<String>) -> Result<()> {
        let mut base = self.base.write().await;
        base.add_user(content).await
    }

    /// Add an assistant message to the session
    ///
    /// Note: Metadata updates are handled by `MetadataController` at turn boundary
    pub async fn add_assistant(
        &self,
        content: impl Into<String>,
        tool_calls: Option<Vec<crate::engine::ToolCall>>,
        usage: Option<crate::providers::TokenUsage>,
    ) -> Result<()> {
        let mut base = self.base.write().await;
        base.add_assistant(content, tool_calls, usage).await
    }

    /// Add a tool result to the session
    pub async fn add_tool_result(
        &self,
        tool_call_id: impl Into<String>,
        tool_name: impl Into<String>,
        result: impl Into<String>,
    ) -> Result<()> {
        let mut base = self.base.write().await;
        base.add_tool_result(tool_call_id, tool_name, result).await
    }

    /// Load conversation history
    pub async fn load_history(&self) -> Result<Vec<crate::types::message::LlmMessage>> {
        let base = self.base.read().await;
        base.load_history().await
    }

    /// Get context as text
    pub async fn get_context_text(&self, limit: usize) -> String {
        let base = self.base.read().await;
        base.get_context_text(limit).await
    }

    /// Get session metadata (via shared controller)
    pub async fn get_metadata(&self) -> Result<SessionMetadata> {
        let mut controller = self.metadata.write().await;
        match controller.get_metadata(&self.session_id, false).await? {
            Some(m) => Ok(m),
            None => Err(anyhow::anyhow!("Session {} not found", self.session_id)),
        }
    }

    /// Record token usage (via shared controller)
    ///
    /// `context_window` is the `total_tokens` from the current assistant message.
    /// `input` and `output` are the incremental tokens for this turn.
    pub async fn record_usage(
        &self,
        context_window: usize,
        input: usize,
        output: usize,
    ) -> Result<()> {
        let mut controller = self.metadata.write().await;
        controller
            .record_token_usage(&self.session_id, context_window, input, output)
            .await
    }

    /// Sync metadata from JSONL (source of truth) (via shared controller)
    ///
    /// This should be called at the end of an engine turn to update
    /// the index with the actual message count from JSONL.
    pub async fn sync_metadata(&self) -> Result<()> {
        let mut controller = self.metadata.write().await;
        controller.sync_from_jsonl(&self.session_id).await?;
        Ok(())
    }

    /// Check if the session exists and is accessible
    ///
    /// Returns true if the session can be found in the metadata store.
    pub async fn exists(&self) -> bool {
        self.metadata
            .write()
            .await
            .get_metadata(&self.session_id, false)
            .await
            .is_ok()
    }
}

/// Options for creating a new session
#[derive(Debug, Clone, Default)]
pub struct SessionCreateOptions {
    pub parent_session_id: Option<String>,
    pub title: Option<String>,
    pub trigger: String,
    /// Specific session ID to use (if not provided, a UUID will be generated)
    pub session_id: Option<String>,
}

impl SessionCreateOptions {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_parent(mut self, parent_id: impl Into<String>) -> Self {
        self.parent_session_id = Some(parent_id.into());
        self.trigger = "branch".to_string();
        self
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn with_trigger(mut self, trigger: impl Into<String>) -> Self {
        self.trigger = trigger.into();
        self
    }

    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }
}

/// Session resolution strategy
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ResolutionStrategy {
    /// Auto-resume active session, create new if none exists
    AutoResume,
    /// Always create a new session
    ForceNew,
    /// Resume specific session by ID, fail if not found
    Specific,
}

/// Result of session resolution
///
/// This is the canonical return type for all session resolution operations.
/// It clearly separates read-only routing metadata (`context`) from the
/// operations facade (`handle`), eliminating the ambiguity that previously
/// existed between `SessionContext` and `SessionHandle`.
///
/// ## Usage
///
/// ```rust,ignore
/// let resolved = manager.resolve_session(...).await?;
///
/// // Read-only metadata access
/// let session_id = resolved.context.session_id;
/// let is_subagent = resolved.context.is_subagent;
///
/// // All session operations go through the handle
/// resolved.handle.add_user("Hello").await?;
/// let history = resolved.handle.load_history().await?;
/// ```
#[derive(Debug)]
pub struct ResolvedSession {
    /// Routing metadata DTO — read-only, no operations.
    ///
    /// Use this for: session identification, routing decisions, logging,
    /// and any code that needs lightweight metadata without acquiring locks.
    pub context: super::context::SessionContext,
    /// Operations facade — the SOLE authority for session mutations.
    ///
    /// Use this for: adding messages, loading history, recording usage,
    /// updating overlay state, and all other session mutations.
    pub handle: SessionHandle,
    /// Whether this is a newly created session
    pub is_new: bool,
    /// The session ID (convenience alias for `context.session_id`)
    pub session_id: String,
}

/// Session manager for overlay lifecycle
///
/// Manages the LIFECYCLE of base sessions and overlays:
/// - Caching of base sessions
/// - Creating and tracking overlays
/// - Cross-channel session sharing
/// - Session index for UUID-based file naming and switching
/// - Session branching, switching, deletion
///
/// For SESSION OPERATIONS (messages, metadata updates), use `SessionHandle`
/// obtained via `open_session()`.
///
/// # Single Point of Truth
///
/// The `SessionManager` is the SOLE authority for session LIFECYCLE operations.
/// The `MetadataController` is the SOLE authority for session metadata.
/// All session resolution goes through this manager.
#[derive(Debug)]
pub struct SessionManager {
    /// Base sessions: (`agent_id`, peer) -> `Session`
    base_sessions: HashMap<(String, Peer), Arc<RwLock<Session>>>,
    /// Channel overlays: `overlay_key` -> `ChannelOverlay`
    channel_overlays: HashMap<String, Arc<RwLock<ChannelOverlay>>>,
    /// Spawn overlays: `overlay_key` -> `SpawnOverlay`
    spawn_overlays: HashMap<String, Arc<RwLock<SpawnOverlay>>>,
    /// Metadata controller (single point of truth for metadata)
    /// Wrapped in Arc<`RwLock`<>> for sharing with `SessionHandles`
    metadata_controller: Arc<RwLock<MetadataController>>,
    /// Session index for peer routing
    index: Option<SessionIndex>,
    /// Sessions directory path
    sessions_dir: Option<PathBuf>,
    /// Agent name for index operations
    agent_name: Option<String>,
    /// Path resolver for consistent path resolution
    path_resolver: Option<PathResolver>,
    /// User identifier for CLI session isolation
    user: String,
}

impl SessionManager {
    /// Create a new session manager
    #[must_use]
    pub fn new() -> Self {
        // Create a temporary metadata controller (will be replaced in with_path_resolver)
        let temp_dir = std::env::temp_dir();
        let metadata_controller = Arc::new(RwLock::new(MetadataController::new(temp_dir)));

        Self {
            base_sessions: HashMap::new(),
            channel_overlays: HashMap::new(),
            spawn_overlays: HashMap::new(),
            metadata_controller,
            index: None,
            sessions_dir: None,
            agent_name: None,
            path_resolver: None,
            user: "default".to_string(),
        }
    }

    /// Initialize with session index for an agent using `PathResolver`
    ///
    /// This is the PREFERRED way to initialize `SessionManager` as it ensures
    /// consistent path resolution across all components.
    pub async fn with_path_resolver(
        mut self,
        path_resolver: PathResolver,
        agent_name: &str,
        team: Option<&str>,
    ) -> Result<Self> {
        let sessions_dir = path_resolver.agent_sessions_dir(agent_name, team);

        // Ensure directory exists
        tokio::fs::create_dir_all(&sessions_dir).await.ok();

        self.index = Some(SessionIndex::open(&sessions_dir));
        self.sessions_dir = Some(sessions_dir.clone());
        self.agent_name = Some(agent_name.to_string());
        self.path_resolver = Some(path_resolver);

        // Initialize metadata controller with correct directory (shared Arc)
        self.metadata_controller = Arc::new(RwLock::new(MetadataController::new(sessions_dir)));

        Ok(self)
    }

    /// Create a `SessionManager` for CLI operations (offline)
    ///
    /// This is the PRIMARY factory method for creating a `SessionManager`.
    /// It ensures proper team-aware path resolution and consistent behavior.
    ///
    /// # Arguments
    /// * `path_resolver` - The path resolver for consistent path resolution
    /// * `agent_name` - The agent name
    /// * `team` - Optional team name (defaults to "default")
    /// * `user` - User identifier for session isolation (defaults to "default")
    ///
    /// # Example
    /// ```rust,ignore
    /// let manager = SessionManager::for_cli(
    ///     path_resolver,
    ///     "myagent",
    ///     Some("myteam"),
    ///     "alice"
    /// );
    /// ```
    #[must_use]
    pub fn for_cli(
        path_resolver: PathResolver,
        agent_name: &str,
        team: Option<&str>,
        user: &str,
    ) -> Self {
        let sessions_dir = path_resolver.agent_sessions_dir(agent_name, team);
        Self::new()
            .with_sessions_dir_internal(sessions_dir)
            .with_agent_name(agent_name)
            .with_path_resolver_internal(path_resolver)
            .with_user(user)
    }

    /// Initialize with a specific sessions directory (internal use, tests)
    #[doc(hidden)]
    pub fn with_sessions_dir_internal(mut self, sessions_dir: impl Into<PathBuf>) -> Self {
        let sessions_dir = sessions_dir.into();
        self.sessions_dir = Some(sessions_dir.clone());
        self.index = Some(SessionIndex::open(&sessions_dir));
        self.metadata_controller = Arc::new(RwLock::new(MetadataController::new(sessions_dir)));
        self
    }

    /// Set the path resolver (internal use only, for builder pattern)
    fn with_path_resolver_internal(mut self, path_resolver: PathResolver) -> Self {
        self.path_resolver = Some(path_resolver);
        self
    }

    /// Set the agent name
    #[must_use]
    pub fn with_agent_name(mut self, agent_name: &str) -> Self {
        self.agent_name = Some(agent_name.to_string());
        self
    }

    /// Set the user identifier for CLI session isolation
    #[must_use]
    pub fn with_user(mut self, user: &str) -> Self {
        self.user = user.to_string();
        self
    }

    /// Get the user identifier
    #[must_use]
    pub fn user(&self) -> &str {
        &self.user
    }

    /// Get the metadata controller (for internal use)
    #[must_use]
    pub(crate) fn metadata_controller(&self) -> &Arc<RwLock<MetadataController>> {
        &self.metadata_controller
    }

    /// Get the path resolver if available
    #[must_use]
    pub fn path_resolver(&self) -> Option<&PathResolver> {
        self.path_resolver.as_ref()
    }

    /// Get the sessions directory if initialized
    #[must_use]
    pub fn sessions_dir(&self) -> Option<&PathBuf> {
        self.sessions_dir.as_ref()
    }

    /// Check if index is initialized
    #[must_use]
    pub fn has_registry(&self) -> bool {
        self.index.is_some()
    }

    // ====================================================================================
    // UNIFIED SESSION RESOLUTION API (Single Point of Truth)
    // ====================================================================================

    /// Resolve a session for an agent - SINGLE POINT OF TRUTH
    ///
    /// This method centralizes all session resolution logic:
    /// - Auto-resumes active session when no `session_id` provided
    /// - Creates new session when explicitly requested or no active session exists
    /// - Resumes specific session when `session_id` is provided
    ///
    /// # Arguments
    /// * `agent_name` - Name of the agent
    /// * `team` - Optional team name
    /// * `channel` - Channel type (Cli, Http, etc.)
    /// * `channel_id` - Channel identifier (used as peer ID)
    /// * `session_id` - Optional specific session ID to resume
    /// * `force_new` - Force creation of a new session
    ///
    /// # Returns
    /// A `ResolvedSession` containing the context, handle, and whether it's new
    pub async fn resolve_session(
        &mut self,
        agent_name: &str,
        team: Option<&str>,
        channel: ChannelType,
        channel_id: &str,
        session_id: Option<String>,
        force_new: bool,
    ) -> Result<ResolvedSession> {
        let strategy = if force_new {
            ResolutionStrategy::ForceNew
        } else if session_id.is_some() {
            ResolutionStrategy::Specific
        } else {
            ResolutionStrategy::AutoResume
        };

        info!(
            "Resolving session for agent '{}' (team: {:?}) with strategy {:?}, session_id={:?}, force_new={}",
            agent_name, team, strategy, session_id, force_new
        );

        match strategy {
            ResolutionStrategy::ForceNew => {
                let (ctx, handle, session_id) = self
                    .create_fresh_session(agent_name, team, channel, channel_id)
                    .await?;
                Ok(ResolvedSession {
                    context: ctx,
                    handle,
                    is_new: true,
                    session_id,
                })
            }
            ResolutionStrategy::Specific => {
                let sid = session_id.unwrap();
                let (ctx, handle) = self
                    .resume_specific_session(agent_name, team, channel, channel_id, &sid)
                    .await?;
                Ok(ResolvedSession {
                    context: ctx,
                    handle,
                    is_new: false,
                    session_id: sid,
                })
            }
            ResolutionStrategy::AutoResume => {
                self.auto_resume_session(agent_name, team, channel, channel_id)
                    .await
            }
        }
    }

    /// Auto-resume session: try to resume active, create new if none exists
    async fn auto_resume_session(
        &mut self,
        agent_name: &str,
        team: Option<&str>,
        channel: ChannelType,
        channel_id: &str,
    ) -> Result<ResolvedSession> {
        let peer = Peer::User(self.user.clone());

        // Derive peer key ONCE and use it consistently
        let peer_key = derive_base_session_key(agent_name, &peer);

        debug!("Auto-resuming session for peer_key: {}", peer_key);

        // Check peer routing via metadata controller (single lookup)
        let active_session_id = if self.index.is_some() {
            self.metadata_controller
                .write()
                .await
                .get_active_session_id(&peer_key)
                .await?
        } else {
            None
        };

        if let Some(session_id) = active_session_id {
            info!(
                "Found active session '{}' for peer_key '{}'",
                session_id, peer_key
            );
            let (ctx, handle) = self
                .resume_specific_session(agent_name, team, channel, channel_id, &session_id)
                .await?;
            return Ok(ResolvedSession {
                context: ctx,
                handle,
                is_new: false,
                session_id,
            });
        }

        // No active session found, create new
        debug!(
            "No active session found for agent '{}' (peer_key: {}), creating new",
            agent_name, peer_key
        );
        let (ctx, handle, session_id) = self
            .create_fresh_session(agent_name, team, channel, channel_id)
            .await?;
        Ok(ResolvedSession {
            context: ctx,
            handle,
            is_new: true,
            session_id,
        })
    }

    /// Create a fresh session (internal helper for resolution)
    ///
    /// This is different from the public `create_new_session` which is deprecated.
    /// This method handles the full context creation for the resolution flow.
    async fn create_fresh_session(
        &mut self,
        agent_name: &str,
        _team: Option<&str>,
        channel: ChannelType,
        channel_id: &str,
    ) -> Result<(super::context::SessionContext, SessionHandle, String)> {
        info!("Creating fresh session for agent '{}'", agent_name);

        let peer = Peer::User(self.user.clone());

        // Clear any existing base session for this peer to ensure fresh start
        self.remove_base_session(agent_name, &peer);

        // Create session using the existing create_session method
        let options = SessionCreateOptions::new().with_trigger("user");
        let handle = self.create_session(agent_name, &peer, options).await?;
        let session_id = handle.session_id().to_string();

        // Create channel overlay on the new base session
        // Note: channel_id is used for overlay identification, peer is used for session isolation
        let base = handle.base().clone();
        let handle = self
            .create_channel_overlay_on_base_as_handle(base, &peer, channel, channel_id)
            .await?;

        let ctx = build_session_context(&handle, Some(channel), false).await;

        info!(
            "Created fresh session '{}' for agent '{}', peer={:?}",
            session_id, agent_name, peer
        );
        Ok((ctx, handle, session_id))
    }

    /// Resume a specific session by ID
    async fn resume_specific_session(
        &mut self,
        agent_name: &str,
        _team: Option<&str>,
        channel: ChannelType,
        channel_id: &str,
        session_id: &str,
    ) -> Result<(super::context::SessionContext, SessionHandle)> {
        info!(
            "Resuming specific session '{}' for agent '{}'",
            session_id, agent_name
        );

        let peer = Peer::User(self.user.clone());

        // Open the SPECIFIC session by ID, creating it if it doesn't exist
        let handle = match self.open_session(session_id).await? {
            Some(handle) => handle,
            None => {
                info!("Session '{}' not found, creating new", session_id);
                let options = crate::session::SessionCreateOptions::new()
                    .with_trigger("api")
                    .with_session_id(session_id);
                self.create_session(agent_name, &peer, options).await?
            }
        };

        // Get the base session from the handle
        let base = handle.base().clone();

        // Create channel overlay on the opened base session
        // Note: channel_id is used for overlay identification, peer is used for session isolation
        let handle = self
            .create_channel_overlay_on_base_as_handle(base, &peer, channel, channel_id)
            .await?;

        let ctx = build_session_context(&handle, Some(channel), false).await;

        info!("Successfully resumed session '{}'", session_id);
        Ok((ctx, handle))
    }

    // ====================================================================================
    // Routing methods (ported from SessionRouter)
    // ====================================================================================

    /// Route a message to a session
    ///
    /// This creates or retrieves the appropriate session for the given
    /// peer and channel, enabling cross-channel context sharing.
    pub async fn route(
        &mut self,
        _peer: &Peer,
        channel_type: ChannelType,
        channel_id: &str,
        agent: Option<&str>,
    ) -> Result<ResolvedSession> {
        let agent_name = agent
            .map(|a| a.to_string())
            .or_else(|| self.agent_name.clone())
            .unwrap_or_else(|| "default".to_string());
        self.resolve_session(&agent_name, None, channel_type, channel_id, None, false)
            .await
    }

    /// Route to a specific agent
    pub async fn route_to_agent(
        &mut self,
        agent: &str,
        _peer: &Peer,
        channel_type: ChannelType,
        channel_id: &str,
    ) -> Result<ResolvedSession> {
        self.resolve_session(agent, None, channel_type, channel_id, None, false)
            .await
    }

    /// Create a spawn session
    pub async fn spawn_session(
        &mut self,
        agent: &str,
        peer: &Peer,
        task: &str,
        isolated: bool,
        parent_session_key: &str,
        timeout_seconds: Option<u64>,
    ) -> Result<ResolvedSession> {
        let handle = self
            .create_spawn_overlay_with_config(
                agent,
                peer,
                task,
                isolated,
                parent_session_key,
                timeout_seconds,
                SpawnCleanupPolicy::default(),
                0,
            )
            .await?;

        let session_id = handle.session_id().to_string();
        let ctx = build_session_context(&handle, None, true).await;

        Ok(ResolvedSession {
            context: ctx,
            handle,
            is_new: true,
            session_id,
        })
    }

    // ====================================================================================
    // NEW API: Session Lifecycle (Phase 2)
    // ====================================================================================

    /// Create a completely new session
    ///
    /// This is the PREFERRED way to create a session. It ensures:
    /// - JSONL file is created
    /// - Index entry is created
    /// - Metadata is properly initialized
    pub async fn create_session(
        &mut self,
        agent: &str,
        peer: &Peer,
        options: SessionCreateOptions,
    ) -> Result<SessionHandle> {
        let sessions_dir = self
            .sessions_dir
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Sessions directory not set"))?
            .clone();

        // Use provided session ID or generate a new one
        let session_id = options
            .session_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let session_key = derive_base_session_key(agent, peer);

        // 1. Create JSONL file directly using SessionStorage
        let storage = SessionStorage::new(sessions_dir.clone());
        tokio::fs::create_dir_all(&sessions_dir).await?;
        let cwd = std::env::current_dir()
            .ok()
            .map(|p| p.to_string_lossy().to_string());
        storage.create_session(&session_id, cwd).await?;

        // 2. Create Session from components
        let session = Session::from_components(
            session_id.clone(),
            agent.to_string(),
            session_key.clone(),
            peer.clone(),
            storage,
        );

        // 2. Create metadata
        let mut metadata = SessionMetadata::new(&session_id, agent, format!("{session_id}.jsonl"));
        if let Some(parent_id) = options.parent_session_id {
            metadata.parent_session_id = Some(parent_id);
        }
        if let Some(title) = options.title {
            metadata.title = Some(title);
        }
        metadata.trigger = options.trigger;

        // 3. Store metadata (via shared controller)
        self.metadata_controller
            .write()
            .await
            .create_metadata(metadata)
            .await?;

        // 4. Update peer routing in index via metadata controller
        if self.index.is_some() {
            let entry = SessionEntry::with_peer(
                session_id.clone(),
                agent.to_string(),
                format!("{session_id}.jsonl"),
                peer.peer_type(),
                peer.id(),
            );
            self.metadata_controller
                .write()
                .await
                .create_for_peer(entry, &session_key)
                .await?;
            self.metadata_controller.write().await.save_index().await?;
        }

        // 5. Cache and return handle
        let arc = Arc::new(RwLock::new(session));
        let key = (agent.to_string(), peer.clone());
        self.base_sessions.insert(key, arc.clone());

        info!(
            "Created new session {} for peer {}",
            session_id, session_key
        );

        // Create handle with shared metadata controller (no circular reference)
        let metadata_arc = self.metadata_controller.clone();
        Ok(SessionHandle::new(session_id, arc, None, metadata_arc))
    }

    /// Open an existing session by ID
    ///
    /// Automatically reconciles metadata with JSONL content.
    pub async fn open_session(&mut self, session_id: &str) -> Result<Option<SessionHandle>> {
        let sessions_dir = self
            .sessions_dir
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Sessions directory not set"))?
            .clone();

        // 1. Get metadata (with consistency check)
        let metadata = match self
            .metadata_controller
            .write()
            .await
            .get_metadata(session_id, true)
            .await?
        {
            Some(m) => m,
            None => return Ok(None),
        };

        // 2. Try to get peer info from index via metadata controller
        let peer = if self.index.is_some() {
            if let Ok(Some(entry)) = self
                .metadata_controller
                .write()
                .await
                .get_entry_from_index(session_id)
                .await
            {
                // Restore peer from entry
                match (entry.peer_type.as_deref(), entry.peer_id) {
                    (Some("agent"), Some(id)) => Peer::Agent(id),
                    (Some("user"), Some(id)) => Peer::User(id),
                    _ => Peer::User("default".to_string()),
                }
            } else {
                Peer::User("default".to_string())
            }
        } else {
            Peer::User("default".to_string())
        };

        // 3. Load Session from JSONL with peer info
        let session =
            Session::open_by_id(&metadata.agent_name, session_id, &sessions_dir, Some(&peer))
                .await?;
        let peer = session.peer.clone();

        // 4. Cache and return handle
        let arc = Arc::new(RwLock::new(session));
        let key = (metadata.agent_name.clone(), peer);
        self.base_sessions.insert(key, arc.clone());

        // Create handle with shared metadata controller (no circular reference)
        let metadata_arc = self.metadata_controller.clone();
        Ok(Some(SessionHandle::new(
            session_id.to_string(),
            arc,
            None,
            metadata_arc,
        )))
    }

    /// Get metadata for any session (lightweight read-only lookup)
    ///
    /// This is a CONVENIENCE METHOD for read-only metadata access without
    /// needing to open a session. Use this for:
    /// - Existence checks
    /// - Session listing/validation
    /// - Read-only metadata display
    ///
    /// For operations on an active session, prefer `SessionHandle::get_metadata()`
    /// via `open_session()` to ensure proper session lifecycle management.
    ///
    /// Uses the shared metadata controller for cache consistency.
    pub async fn get_session_metadata(&self, session_id: &str) -> Result<SessionMetadata> {
        // Use the shared controller (with write lock for cache updates)
        let mut controller = self.metadata_controller.write().await;

        match controller.get_metadata(session_id, false).await? {
            Some(m) => Ok(m),
            None => Err(anyhow::anyhow!("Session {session_id} not found")),
        }
    }

    /// List all sessions with metadata
    ///
    /// By default, verifies consistency for each session.
    pub async fn list_all_sessions(
        &mut self,
        verify_consistency: bool,
    ) -> Result<Vec<SessionMetadata>> {
        self.metadata_controller
            .write()
            .await
            .list_metadata(verify_consistency)
            .await
    }

    /// Reconcile all sessions (for maintenance)
    pub async fn reconcile_all_sessions(
        &mut self,
    ) -> Result<Vec<super::metadata::ReconciliationResult>> {
        self.metadata_controller.write().await.reconcile_all().await
    }

    // ====================================================================================
    // Session Branching and Switching
    // ====================================================================================

    /// Branch current session (/branch command)
    pub async fn branch_session(&mut self, peer: &Peer, label: Option<String>) -> Result<String> {
        // Get agent name first to avoid borrow issues
        let agent = self
            .agent_name
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Agent name not set"))?
            .clone();

        let index = self
            .index
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("Session index not initialized"))?;

        let peer_key = derive_base_session_key(&agent, peer);

        // Get current active session as parent
        let parent_id = index
            .get_active_session_id(&peer_key)
            .await?
            .ok_or_else(|| anyhow::anyhow!("No active session to branch from"))?;

        // Create new session with parent
        let options = SessionCreateOptions::new()
            .with_parent(&parent_id)
            .with_title(label.unwrap_or_default());

        let handle = self.create_session(&agent, peer, options).await?;
        let session_id = handle.session_id().to_string();

        info!("Branched session {} from {}", session_id, parent_id);
        Ok(session_id)
    }

    /// Branch a specific session by ID (for CLI operations)
    ///
    /// Creates a new session with the parent's history copied.
    /// Updates the index atomically.
    ///
    /// # Arguments
    /// * `parent_session_id` - The session ID to branch from
    /// * `label` - Optional label/title for the new session
    ///
    /// # Returns
    /// The new session ID
    pub async fn branch_session_by_id(
        &mut self,
        parent_session_id: &str,
        label: Option<String>,
    ) -> Result<String> {
        let agent = self
            .agent_name
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Agent name not set"))?
            .clone();

        let sessions_dir = self
            .sessions_dir
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Sessions directory not set"))?
            .clone();

        // Verify parent session exists
        let parent_metadata = self
            .metadata_controller
            .write()
            .await
            .get_metadata_fast(parent_session_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Parent session '{parent_session_id}' not found"))?;

        // Generate new session ID
        let new_session_id = uuid::Uuid::new_v4().to_string();

        // Copy parent JSONL file to new session
        let storage = SessionStorage::new(sessions_dir.clone());
        storage
            .copy_session(parent_session_id, &new_session_id)
            .await?;

        // Create metadata for new session
        let mut new_metadata =
            SessionMetadata::new(&new_session_id, &agent, format!("{new_session_id}.jsonl"));
        new_metadata.parent_session_id = Some(parent_session_id.to_string());
        new_metadata.title = label.or_else(|| {
            parent_metadata
                .title
                .as_ref()
                .map(|t| format!("Branch: {t}"))
        });
        new_metadata.trigger = "branch".to_string();
        new_metadata.message_count = parent_metadata.message_count;
        new_metadata.context_window = parent_metadata.context_window;
        new_metadata.total_input_tokens = parent_metadata.total_input_tokens;
        new_metadata.total_output_tokens = parent_metadata.total_output_tokens;

        // Store metadata
        self.metadata_controller
            .write()
            .await
            .create_metadata(new_metadata)
            .await?;

        info!(
            "Branched session {} from {}",
            new_session_id, parent_session_id
        );
        Ok(new_session_id)
    }

    /// Switch to a different session (/switch command)
    pub async fn switch_session(&mut self, peer: &Peer, session_id: &str) -> Result<()> {
        if self.index.is_none() {
            return Err(anyhow::anyhow!("Session index not initialized"));
        }
        let agent = self
            .agent_name
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Agent name not set"))?;

        let peer_key = derive_base_session_key(agent, peer);
        self.metadata_controller
            .write()
            .await
            .set_active_for_peer(&peer_key, session_id)
            .await?;
        self.metadata_controller.write().await.save_index().await?;

        info!("Switched {} to session {}", peer_key, session_id);
        Ok(())
    }

    /// List all sessions for a peer (legacy)
    ///
    /// NOTE: This returns `SessionEntry` for backward compatibility.
    /// Consider using `list_sessions()` for new code.
    pub async fn list_sessions_for_peer(&mut self, peer: &Peer) -> Result<Vec<SessionEntry>> {
        let agent = self
            .agent_name
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Agent name not set"))?
            .clone();

        if self.index.is_none() {
            return Err(anyhow::anyhow!("Session index not initialized"));
        }

        let peer_key = derive_base_session_key(&agent, peer);
        self.metadata_controller
            .write()
            .await
            .list_for_peer_from_index(&peer_key)
            .await
    }

    /// Get active session ID for a peer
    pub async fn get_active_session_id(&mut self, peer: &Peer) -> Result<Option<String>> {
        let agent = self
            .agent_name
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Agent name not set"))?
            .clone();

        if self.index.is_none() {
            return Err(anyhow::anyhow!("Session index not initialized"));
        }

        let peer_key = derive_base_session_key(&agent, peer);
        self.metadata_controller
            .write()
            .await
            .get_active_session_id(&peer_key)
            .await
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
    ) -> Result<Arc<RwLock<Session>>> {
        let key = (agent.to_string(), peer.clone());

        // Check cache first
        if let Some(session) = self.base_sessions.get(&key) {
            return Ok(session.clone());
        }

        // Use index if available for UUID-based sessions
        if self.index.is_some() {
            let peer_key = derive_base_session_key(agent, peer);
            let sessions_dir = self.sessions_dir.as_ref().unwrap();

            // Get or create session via metadata controller
            let session_id = {
                let mut controller = self.metadata_controller.write().await;
                if let Some(existing_id) = controller.get_active_session_id(&peer_key).await? {
                    existing_id
                } else {
                    // Create new session through metadata controller
                    let new_id = uuid::Uuid::new_v4().to_string();
                    let transcript_file = format!("{new_id}.jsonl");
                    let entry = SessionEntry::with_peer(
                        new_id.clone(),
                        agent.to_string(),
                        transcript_file,
                        peer.peer_type(),
                        peer.id(),
                    );
                    controller.create_for_peer(entry, &peer_key).await?;
                    controller.save_index().await?;
                    tracing::info!("Created new session via index: {}", new_id);
                    new_id
                }
            };

            // Check if session file exists by looking for it directly
            let transcript_file = format!("{session_id}.jsonl");
            let transcript_path = sessions_dir.join(&transcript_file);

            let session = if transcript_path.exists() {
                // File exists, open it by ID
                info!("Opening existing session: {}", transcript_path.display());
                Session::open_by_id(agent, &session_id, sessions_dir, Some(peer)).await?
            } else {
                // Create the session file directly using SessionStorage
                info!("Creating new session file: {}", transcript_path.display());
                let storage = SessionStorage::new(sessions_dir.clone());
                let cwd = std::env::current_dir()
                    .ok()
                    .map(|p| p.to_string_lossy().to_string());
                storage.create_session(&session_id, cwd).await?;

                // Create Session from components
                Session::from_components(
                    session_id.clone(),
                    agent.to_string(),
                    peer_key.clone(),
                    peer.clone(),
                    storage,
                )
            };

            let arc = Arc::new(RwLock::new(session));
            self.base_sessions.insert(key, arc.clone());
            return Ok(arc);
        }

        // SessionManager not initialized - require initialization
        Err(anyhow::anyhow!(
            "SessionManager not initialized. Call with_path_resolver() first."
        ))
    }

    /// Get an existing base session if it exists
    #[must_use]
    pub fn get_existing_base(&self, agent: &str, peer: &Peer) -> Option<Arc<RwLock<Session>>> {
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
    ) -> Result<SessionHandle> {
        // Get or create the base session
        let base = self.get_or_create_base(agent, peer).await?;
        self.create_channel_overlay_on_base_as_handle(base, peer, channel_type, channel_id)
            .await
    }

    /// Create a channel overlay on an existing base session
    ///
    /// This is used when a session has been explicitly opened by ID (e.g., for
    /// session resumption), and we need to create a channel overlay on top of it.
    pub async fn create_channel_overlay_on_base(
        &mut self,
        base: Arc<RwLock<Session>>,
        peer: &Peer,
        channel_type: ChannelType,
        channel_id: &str,
    ) -> Result<SessionHandle> {
        self.create_channel_overlay_on_base_as_handle(base, peer, channel_type, channel_id)
            .await
    }

    /// Internal: create a channel overlay on base and return a SessionHandle
    async fn create_channel_overlay_on_base_as_handle(
        &mut self,
        base: Arc<RwLock<Session>>,
        peer: &Peer,
        channel_type: ChannelType,
        channel_id: &str,
    ) -> Result<SessionHandle> {
        let base_key = {
            let base_read = base.read().await;
            base_read.session_key.clone()
        };

        // Generate overlay key
        let overlay_id = format!("{}:{}", channel_type.as_str(), channel_id);
        let overlay_key = derive_overlay_key(&base_key, "channel", &overlay_id);

        // Check if overlay already exists
        if let Some(overlay) = self.channel_overlays.get(&overlay_key) {
            let session_id = {
                let base_read = base.read().await;
                base_read.id.clone()
            };
            return Ok(SessionHandle::new(
                session_id,
                base,
                Some(OverlayRef::Channel(overlay.clone())),
                self.metadata_controller.clone(),
            ));
        }

        // Create new overlay
        let overlay = ChannelOverlay::new(&base_key, peer.clone(), channel_type, channel_id);
        let overlay_arc = Arc::new(RwLock::new(overlay));
        self.channel_overlays
            .insert(overlay_key, overlay_arc.clone());

        let session_id = {
            let base_read = base.read().await;
            base_read.id.clone()
        };
        Ok(SessionHandle::new(
            session_id,
            base,
            Some(OverlayRef::Channel(overlay_arc)),
            self.metadata_controller.clone(),
        ))
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
    ) -> Result<SessionHandle> {
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

        let session_id = {
            let base_read = child_base.read().await;
            base_read.id.clone()
        };
        Ok(SessionHandle::new(
            session_id,
            child_base,
            Some(OverlayRef::Spawn(overlay_arc)),
            self.metadata_controller.clone(),
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
    ) -> Result<SessionHandle> {
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

        let session_id = {
            let base_read = child_base.read().await;
            base_read.id.clone()
        };
        Ok(SessionHandle::new(
            session_id,
            child_base,
            Some(OverlayRef::Spawn(overlay_arc)),
            self.metadata_controller.clone(),
        ))
    }

    /// Get the parent's base session from a session key (which may be an overlay key)
    async fn get_parent_base_session(&self, session_key: &str) -> Option<Arc<RwLock<Session>>> {
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
    ) -> Result<SessionHandle> {
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
    ) -> Option<Arc<RwLock<Session>>> {
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

    /// Get mutable access to the session index (for cleanup operations)
    pub fn index_mut(&mut self) -> Option<&mut SessionIndex> {
        self.index.as_mut()
    }

    /// Clean up a spawn session (overlay + base session + index entry)
    ///
    /// This is the unified cleanup path for subagent spawn sessions.
    /// It removes the spawn overlay, the underlying base session, and
    /// clears the peer from the session index if available.
    ///
    /// # Arguments
    /// * `child_session_key` - The full overlay session key (e.g. `agent:{agent}:peer:{type}:{id}:overlay:spawn:{spawn_id}`)
    ///
    /// # Returns
    /// `Ok(true)` if cleanup occurred, `Ok(false)` if the overlay was not found
    pub async fn cleanup_spawn(&mut self, child_session_key: &str) -> Result<bool> {
        // Remove the spawn overlay
        if self.remove_spawn_overlay(child_session_key).is_none() {
            return Ok(false);
        }

        // Extract base key from overlay key
        let base_key = crate::session::key::base_key_from_overlay(child_session_key)
            .unwrap_or_else(|| child_session_key.to_string());

        // Parse the base key to get agent and peer
        if let Some(parsed) = crate::session::key::parse_session_key_v2(&base_key) {
            let peer = match parsed.peer_type.as_str() {
                "agent" => Peer::Agent(parsed.peer_id),
                "user" => Peer::User(parsed.peer_id),
                _ => Peer::Agent(parsed.peer_id),
            };

            // Remove the base session
            self.remove_base_session(&parsed.agent, &peer);

            // Clear peer from session index if available
            if let Some(ref mut index) = self.index_mut() {
                if let Err(e) = index.clear_active_for_peer(&base_key).await {
                    tracing::warn!("Failed to clear peer from index: {}", e);
                } else if let Err(e) = index.save_peers().await {
                    tracing::warn!("Failed to save peers index: {}", e);
                } else {
                    tracing::info!("Cleared peer from session index: {}", base_key);
                }
            }
        }

        Ok(true)
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
    /// Similar to `resolve_agent_session` but guarantees the session is created
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
    #[must_use]
    pub fn list_agent_sessions(&self, agent_id: &str) -> Vec<String> {
        let mut sessions = Vec::new();

        for ((session_agent, _), session) in &self.base_sessions {
            if session_agent == agent_id {
                // This is blocking, but we're just reading the key
                // In production, might want to make this async
                sessions.push(format!("agent:{agent_id}:base:{session:?}"));
            }
        }

        sessions
    }

    /// Get or create an A2A session for messaging between agents
    ///
    /// This method ensures there's a session available for A2A communication.
    /// If the target agent has an existing session, it returns that.
    /// Otherwise, it creates an ephemeral A2A session.
    ///
    /// # Arguments
    /// * `target_agent_id` - The agent to message
    /// * `caller_agent_id` - The agent initiating the message
    ///
    /// # Returns
    /// Session key for A2A messaging
    pub async fn get_or_create_a2a_session(
        &self,
        target_agent_id: &str,
        caller_agent_id: &str,
    ) -> Result<String> {
        // First check if target has any existing sessions
        let existing = self.list_agent_sessions(target_agent_id);
        if !existing.is_empty() {
            // Return the first existing session
            return Ok(existing.into_iter().next().unwrap());
        }

        // Create ephemeral A2A session
        let session_key = format!(
            "agent:{}:a2a:{}:{}",
            target_agent_id,
            caller_agent_id,
            uuid::Uuid::new_v4().simple()
        );

        tracing::info!(
            "Created ephemeral A2A session: {} -> {}",
            caller_agent_id,
            session_key
        );

        Ok(session_key)
    }

    /// Register a session for an agent
    ///
    /// This allows the runtime to track which sessions belong to which agents,
    /// enabling proper session ownership and cleanup.
    pub fn register_agent_session(&mut self, agent_id: &str, session_key: &str) {
        tracing::debug!("Registered session {} for agent {}", session_key, agent_id);
        // The session is already stored in base_sessions with agent_id as key
        // This method exists for explicit registration if needed
    }

    /// Check if an agent has any active sessions
    #[must_use]
    pub fn agent_has_sessions(&self, agent_id: &str) -> bool {
        self.base_sessions
            .keys()
            .any(|(session_agent, _)| session_agent == agent_id)
    }

    /// Get the number of active sessions for an agent
    #[must_use]
    pub fn agent_session_count(&self, agent_id: &str) -> usize {
        self.base_sessions
            .keys()
            .filter(|(session_agent, _)| session_agent == agent_id)
            .count()
    }

    // Helper to clone manager for SessionHandle (DEPRECATED: use shared controller instead)
    #[allow(dead_code)]
    fn clone_manager(&self) -> Self {
        // Create a new manager with same state
        // Shares the metadata controller Arc for cache consistency
        let _sessions_dir = self.sessions_dir.clone().unwrap_or_else(std::env::temp_dir);
        Self {
            base_sessions: self.base_sessions.clone(),
            channel_overlays: self.channel_overlays.clone(),
            spawn_overlays: self.spawn_overlays.clone(),
            // Clone the Arc to share the same controller (cache consistency)
            metadata_controller: self.metadata_controller.clone(),
            // Share the same index (since MetadataController owns it, we keep our reference)
            index: self.index.clone(),
            sessions_dir: self.sessions_dir.clone(),
            agent_name: self.agent_name.clone(),
            path_resolver: self.path_resolver.clone(),
            user: self.user.clone(),
        }
    }
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Build a `SessionContext` DTO from a `SessionHandle`
async fn build_session_context(
    handle: &SessionHandle,
    channel_type: Option<ChannelType>,
    is_subagent: bool,
) -> super::context::SessionContext {
    let base = handle.base().read().await;
    super::context::SessionContext::new(
        handle.session_id().to_string(),
        base.agent_name.clone(),
        base.session_key.clone(),
        handle.full_session_key().await,
        base.peer.clone(),
        channel_type,
        is_subagent,
        handle.is_isolated().await,
    )
}

/// Copy conversation context from parent base session to child base session
///
/// This is used for shared-context spawns where the child should start with
/// the parent's conversation history.
async fn copy_session_context(
    parent: &Arc<RwLock<Session>>,
    child: &Arc<RwLock<Session>>,
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

                // Extract tool calls from content blocks (ContentBlock::ToolCall)
                let tool_calls: Vec<crate::engine::ToolCall> = msg
                    .content
                    .iter()
                    .filter_map(|c| {
                        if let ContentBlock::ToolCall {
                            name, arguments, ..
                        } = c
                        {
                            Some(crate::engine::ToolCall {
                                name: name.clone(),
                                parameters: arguments.clone(),
                            })
                        } else {
                            None
                        }
                    })
                    .collect();

                let tool_calls_opt = if tool_calls.is_empty() {
                    None
                } else {
                    Some(tool_calls)
                };

                child_guard
                    .add_assistant(&text, tool_calls_opt, None)
                    .await?;
            }
            MessageRole::Tool => {
                // Tool results - skip for now as they require tool_call_id linking
                // which may be complex to preserve across sessions
            }
        }
    }

    Ok(())
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
    async fn test_get_or_create_base() {
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let mut manager = SessionManager::new().with_sessions_dir_internal(temp.path());
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
    async fn test_create_channel_overlay() {
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let mut manager = SessionManager::new().with_sessions_dir_internal(temp.path());
        let peer = Peer::User("alice".to_string());

        let handle = manager
            .create_channel_overlay("test_agent", &peer, ChannelType::Discord, "guild123")
            .await
            .unwrap();

        assert!(handle.has_channel_overlay());
        assert!(!handle.has_spawn_overlay());

        let channel_type = handle.channel_type().await;
        assert_eq!(channel_type, Some(ChannelType::Discord));

        assert_eq!(manager.channel_overlay_count(), 1);
    }

    #[tokio::test]
    async fn test_cross_channel_session_sharing() {
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let mut manager = SessionManager::new().with_sessions_dir_internal(temp.path());
        let peer = Peer::User("alice".to_string());

        // Create CLI session
        let cli = manager
            .get_session_for_channel("test_agent", &peer, ChannelType::Cli, "default")
            .await
            .unwrap();

        // Add a message via CLI base session
        {
            let mut base = cli.base().write().await;
            base.add_user("Hello from CLI").await.unwrap();
        }

        // Create Discord session for same peer
        let discord = manager
            .get_session_for_channel("test_agent", &peer, ChannelType::Discord, "guild123")
            .await
            .unwrap();

        // Should share the same base session
        assert!(Arc::ptr_eq(cli.base(), discord.base()));

        // Discord should see the message from CLI
        let history = {
            let base = discord.base().read().await;
            base.load_history().await.unwrap()
        };
        assert!(!history.is_empty()); // At least the message we added
    }

    #[tokio::test]
    async fn test_create_spawn_overlay() {
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let mut manager = SessionManager::new().with_sessions_dir_internal(temp.path());
        let peer = Peer::User("alice".to_string());

        let handle = manager
            .create_spawn_overlay("test_agent", &peer, "Research task", false, "parent_key")
            .await
            .unwrap();

        assert!(handle.has_spawn_overlay());
        assert!(!handle.has_channel_overlay());
        assert!(!handle.is_isolated().await);

        assert_eq!(manager.spawn_overlay_count(), 1);
    }

    #[tokio::test]
    async fn test_isolated_spawn() {
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let mut manager = SessionManager::new().with_sessions_dir_internal(temp.path());
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
        assert!(!Arc::ptr_eq(&parent, spawn.base()));
        assert!(spawn.is_isolated().await);
    }

    #[tokio::test]
    async fn test_shared_spawn() {
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let mut manager = SessionManager::new().with_sessions_dir_internal(temp.path());
        let peer = Peer::User("alice".to_string());

        // Create parent session and add a message
        let parent = manager
            .get_or_create_base("test_agent", &peer)
            .await
            .unwrap();
        {
            let mut base = parent.write().await;
            base.add_user("parent message").await.unwrap();
        }

        // Get the actual parent session key so context copying can work
        let parent_key = {
            let base = parent.read().await;
            base.session_key.clone()
        };

        // Create non-isolated spawn — always gets a new base session,
        // but parent's conversation history is copied to the child
        let spawn = manager
            .create_spawn_overlay("test_agent", &peer, "Shared task", false, &parent_key)
            .await
            .unwrap();

        // Spawn always has its own base session (not shared)
        assert!(!Arc::ptr_eq(&parent, spawn.base()));
        assert!(!spawn.is_isolated().await);

        // But non-isolated spawn should have copied parent's context
        let history = spawn.load_history().await.unwrap();
        assert!(!history.is_empty(), "Non-isolated spawn should copy parent context");
    }

    #[tokio::test]
    async fn test_spawn_with_config() {
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let mut manager = SessionManager::new().with_sessions_dir_internal(temp.path());
        let peer = Peer::User("alice".to_string());

        let handle = manager
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

        if let Some(OverlayRef::Spawn(spawn_arc)) = handle.overlay() {
            let spawn = spawn_arc.read().await;
            assert_eq!(spawn.timeout_seconds, Some(300));
            assert_eq!(spawn.cleanup, SpawnCleanupPolicy::Delete);
            assert_eq!(spawn.depth, 2);
        } else {
            panic!("Expected spawn overlay");
        }
    }

    #[tokio::test]
    async fn test_get_overlays_for_base() {
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let mut manager = SessionManager::new().with_sessions_dir_internal(temp.path());
        let peer = Peer::User("alice".to_string());

        let handle = manager
            .create_channel_overlay("test_agent", &peer, ChannelType::Discord, "guild123")
            .await
            .unwrap();

        let base_key = handle.base_session_key().await;
        let overlays = manager.get_overlays_for_base(&base_key);

        assert_eq!(overlays.len(), 1);
    }

    #[tokio::test]
    async fn test_hybrid_session_key() {
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let mut manager = SessionManager::new().with_sessions_dir_internal(temp.path());
        let peer = Peer::User("alice".to_string());

        let handle = manager
            .create_channel_overlay("test_agent", &peer, ChannelType::Discord, "guild123")
            .await
            .unwrap();

        let full_key = handle.full_session_key().await;
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

    #[tokio::test]
    async fn test_session_handle() {
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let mut manager = SessionManager::new().with_sessions_dir_internal(temp.path());
        let peer = Peer::User("alice".to_string());

        // Create session
        let handle = manager
            .create_session("test_agent", &peer, SessionCreateOptions::new())
            .await
            .unwrap();

        // Add messages
        handle.add_user("Hello").await.unwrap();
        handle.add_assistant("Hi there!", None, None).await.unwrap();

        // Verify metadata (message_count is not auto-updated on add_user/add_assistant;
        // it reflects the count at creation or last explicit sync)
        let metadata = handle.get_metadata().await.unwrap();
        assert_eq!(metadata.message_count, 0);

        // Load history — messages are stored in JSONL
        let history = handle.load_history().await.unwrap();
        assert_eq!(history.len(), 2); // user + assistant
    }

    // ====================================================================================
    // Phase 2 Tests: Cache Consistency and Shared MetadataController
    // ====================================================================================

    #[tokio::test]
    async fn test_session_handle_metadata_cache_consistency() {
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let mut manager = SessionManager::new().with_sessions_dir_internal(temp.path());
        let peer = Peer::User("alice".to_string());

        // Create session
        let handle = manager
            .create_session("test_agent", &peer, SessionCreateOptions::new())
            .await
            .unwrap();

        // Record token usage via handle (context_window=1000, input=100, output=50)
        handle.record_usage(1000, 100, 50).await.unwrap();

        // Get metadata via handle - should see the updated tokens
        let metadata = handle.get_metadata().await.unwrap();
        assert_eq!(metadata.context_window, 1000);
        assert_eq!(metadata.total_input_tokens, 100);
        assert_eq!(metadata.total_output_tokens, 50);
    }

    #[tokio::test]
    async fn test_shared_metadata_controller_cache_hit() {
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let mut manager = SessionManager::new().with_sessions_dir_internal(temp.path());
        let peer = Peer::User("alice".to_string());

        // Create session
        let handle = manager
            .create_session("test_agent", &peer, SessionCreateOptions::new())
            .await
            .unwrap();

        // First call to get_metadata should populate cache
        let _ = handle.get_metadata().await.unwrap();

        // Second call should use cache (this verifies shared controller is working)
        let metadata = handle.get_metadata().await.unwrap();
        assert_eq!(metadata.session_id, handle.session_id());

        // Verify via manager's method also uses same cache
        let metadata2 = manager
            .get_session_metadata(handle.session_id())
            .await
            .unwrap();
        assert_eq!(metadata2.session_id, handle.session_id());
    }

    #[tokio::test]
    async fn test_multiple_handles_share_metadata_cache() {
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let mut manager = SessionManager::new().with_sessions_dir_internal(temp.path());
        let peer = Peer::User("alice".to_string());

        // Create first session
        let handle1 = manager
            .create_session("test_agent", &peer, SessionCreateOptions::new())
            .await
            .unwrap();

        // Open same session via different handle
        let handle2 = manager
            .open_session(handle1.session_id())
            .await
            .unwrap()
            .expect("Session should exist");

        // Record usage via handle1 (context_window=2000, input=200, output=100)
        handle1.record_usage(2000, 200, 100).await.unwrap();

        // Get metadata via handle2 - should see the changes (shared cache)
        let metadata = handle2.get_metadata().await.unwrap();
        assert_eq!(metadata.context_window, 2000);
        assert_eq!(metadata.total_input_tokens, 200);
        assert_eq!(metadata.total_output_tokens, 100);
    }

    // ====================================================================================
    // Phase 3 Tests: Single Creation and Deletion Pathway
    // ====================================================================================

    #[tokio::test]
    async fn test_single_creation_pathway() {
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let mut manager = SessionManager::new().with_sessions_dir_internal(temp.path());
        let peer = Peer::User("alice".to_string());

        // Create session via SessionManager (the ONLY way)
        let handle = manager
            .create_session("test_agent", &peer, SessionCreateOptions::new())
            .await
            .unwrap();

        // Verify session was created
        let session_id = handle.session_id().to_string();
        assert!(!session_id.is_empty());

        // Verify metadata was created
        let metadata = handle.get_metadata().await.unwrap();
        assert_eq!(metadata.agent_name, "test_agent");

        // Verify we can open it
        let handle2 = manager
            .open_session(&session_id)
            .await
            .unwrap()
            .expect("Session should exist");
        assert_eq!(handle2.session_id(), session_id);
    }

    #[tokio::test]
    async fn test_session_creation_creates_jsonl_and_metadata() {
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let mut manager = SessionManager::new().with_sessions_dir_internal(temp.path());
        let peer = Peer::User("alice".to_string());

        // Create session
        let handle = manager
            .create_session("test_agent", &peer, SessionCreateOptions::new())
            .await
            .unwrap();

        let session_id = handle.session_id();

        // Verify JSONL file exists
        let jsonl_path = temp.path().join(format!("{session_id}.jsonl"));
        assert!(jsonl_path.exists(), "JSONL file should exist");

        // Verify metadata exists in index
        let metadata = handle.get_metadata().await.unwrap();
        assert_eq!(metadata.session_id, session_id);

        // Verify can be listed
        let sessions = manager.list_all_sessions(false).await.unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, session_id);
    }

    #[tokio::test]
    async fn test_single_deletion_pathway() {
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let mut manager = SessionManager::new().with_sessions_dir_internal(temp.path());
        let peer = Peer::User("alice".to_string());

        // Create session
        let handle = manager
            .create_session("test_agent", &peer, SessionCreateOptions::new())
            .await
            .unwrap();
        let session_id = handle.session_id().to_string();

        // Verify it exists
        assert!(manager.open_session(&session_id).await.unwrap().is_some());

        // Delete via MetadataController (the ONLY way)
        manager
            .metadata_controller
            .write()
            .await
            .delete_session(&session_id)
            .await
            .unwrap();

        // Verify it's gone from metadata
        let metadata_result = manager.get_session_metadata(&session_id).await;
        assert!(
            metadata_result.is_err(),
            "Session metadata should be deleted"
        );

        // Verify JSONL is also deleted
        let jsonl_path = temp.path().join(format!("{session_id}.jsonl"));
        assert!(!jsonl_path.exists(), "JSONL file should be deleted");
    }

    #[tokio::test]
    async fn test_legacy_fallback_removed() {
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let mut manager = SessionManager::new().with_sessions_dir_internal(temp.path());
        let peer = Peer::User("alice".to_string());

        // get_or_create_base should require initialized SessionManager
        // (no legacy fallback to Session::open)
        let result = manager.get_or_create_base("test_agent", &peer).await;

        // Should succeed because we have a directory set
        assert!(result.is_ok());

        // The returned session should be properly initialized
        let base = result.unwrap();
        let base_guard = base.read().await;
        assert_eq!(base_guard.agent_name, "test_agent");
    }
}
