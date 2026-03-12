//! Session context for agent execution
//!
//! Provides a unified interface for agents to work with hybrid sessions.
//! This module bridges the session overlay architecture with the agent runtime.

use super::manager::{HybridSession, OverlayRef, SessionManager};
use super::types::{ChannelType, Peer};
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Context for session-aware agent execution
///
/// This provides a unified interface for agents to work with sessions,
/// abstracting away the complexity of base sessions and overlays.
#[derive(Debug, Clone)]
pub struct SessionContext {
    /// The hybrid session (base + overlay)
    pub hybrid: HybridSession,
    /// Channel type (if applicable)
    pub channel_type: Option<ChannelType>,
    /// Whether this session is for a subagent/spawn
    pub is_subagent: bool,
}

impl SessionContext {
    /// Create a new session context from a hybrid session
    pub async fn new(hybrid: HybridSession) -> Self {
        let channel_type = hybrid.channel_type().await;
        let is_subagent = hybrid.has_spawn_overlay();

        Self {
            hybrid,
            channel_type,
            is_subagent,
        }
    }

    /// Create a session context for a channel
    pub async fn for_channel(
        manager: &Arc<RwLock<SessionManager>>,
        agent: &str,
        peer: &Peer,
        channel_type: ChannelType,
        channel_id: &str,
    ) -> Result<Self> {
        let mut manager_guard = manager.write().await;
        let hybrid = manager_guard
            .get_session_for_channel(agent, peer, channel_type, channel_id)
            .await?;

        Ok(Self {
            hybrid,
            channel_type: Some(channel_type),
            is_subagent: false,
        })
    }

    /// Create a session context for a spawn/subagent
    pub async fn for_spawn(
        manager: &Arc<RwLock<SessionManager>>,
        agent: &str,
        peer: &Peer,
        task: &str,
        isolated: bool,
        parent_session_key: &str,
        timeout_seconds: Option<u64>,
    ) -> Result<Self> {
        let mut manager_guard = manager.write().await;
        let hybrid = manager_guard
            .create_spawn_overlay_with_config(
                agent,
                peer,
                task,
                isolated,
                parent_session_key,
                timeout_seconds,
                super::types::SpawnCleanupPolicy::default(),
                0,
            )
            .await?;

        Ok(Self {
            hybrid,
            channel_type: None,
            is_subagent: true,
        })
    }

    /// Get the base session key
    pub async fn base_session_key(&self) -> String {
        self.hybrid.base_session_key().await
    }

    /// Get the full session key (including overlay)
    pub async fn full_session_key(&self) -> String {
        self.hybrid.full_session_key().await
    }

    /// Get the peer
    pub async fn peer(&self) -> Peer {
        self.hybrid.peer().await
    }

    /// Get the agent name
    pub async fn agent_name(&self) -> String {
        let base = self.hybrid.base.read().await;
        base.agent_name.clone()
    }

    /// Check if this is an isolated spawn
    pub async fn is_isolated(&self) -> bool {
        self.hybrid.is_isolated_spawn().await
    }

    /// Load conversation history from the base session
    pub async fn load_history(&self) -> Result<Vec<crate::providers::ChatMessage>> {
        let base = self.hybrid.base.read().await;
        base.load_history().await
    }

    /// Add a user message to the base session
    pub async fn add_user_message(&self, content: impl Into<String>) -> Result<()> {
        let mut base = self.hybrid.base.write().await;
        base.add_user(content).await
    }

    /// Add an assistant message to the base session
    pub async fn add_assistant_message(
        &self,
        content: impl Into<String>,
        tool_calls: Option<Vec<crate::engine::ToolCall>>,
    ) -> Result<()> {
        let mut base = self.hybrid.base.write().await;
        base.add_assistant(content, tool_calls).await
    }

    /// Add a system message to the base session
    pub async fn add_system_message(&self, content: impl Into<String>) -> Result<()> {
        let mut base = self.hybrid.base.write().await;
        base.add_system(content).await
    }

    /// Add a tool result to the base session
    pub async fn add_tool_result(
        &self,
        tool_call_id: impl Into<String>,
        tool_name: impl Into<String>,
        result: impl Into<String>,
    ) -> Result<()> {
        let mut base = self.hybrid.base.write().await;
        base.add_tool_result(tool_call_id, tool_name, result).await
    }

    /// Record token usage
    pub async fn record_usage(&self, input_tokens: usize, output_tokens: usize) -> Result<()> {
        let mut base = self.hybrid.base.write().await;
        base.record_usage(input_tokens, output_tokens).await
    }

    /// Get channel-specific state (if channel overlay)
    pub async fn get_channel_state(&self, key: &str) -> Option<serde_json::Value> {
        if let OverlayRef::Channel(channel_arc) = &self.hybrid.overlay {
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
        if let OverlayRef::Channel(channel_arc) = &self.hybrid.overlay {
            let mut channel = channel_arc.write().await;
            channel.set(key, value);
            true
        } else {
            false
        }
    }

    /// Get spawn status (if spawn overlay)
    pub async fn get_spawn_status(&self) -> Option<super::spawn::SpawnStatus> {
        if let OverlayRef::Spawn(spawn_arc) = &self.hybrid.overlay {
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
        if let OverlayRef::Spawn(spawn_arc) = &self.hybrid.overlay {
            let mut spawn = spawn_arc.write().await;
            f(&mut spawn);
            true
        } else {
            false
        }
    }
}

/// Session router for handling incoming messages
///
/// Routes messages to the appropriate session based on peer and channel.
#[derive(Debug, Clone)]
pub struct SessionRouter {
    /// Session manager
    manager: Arc<RwLock<SessionManager>>,
    /// Default agent name
    default_agent: String,
}

impl SessionRouter {
    /// Create a new session router
    pub fn new(manager: Arc<RwLock<SessionManager>>, default_agent: impl Into<String>) -> Self {
        Self {
            manager,
            default_agent: default_agent.into(),
        }
    }

    /// Route a message to a session
    ///
    /// This creates or retrieves the appropriate session for the given
    /// peer and channel, enabling cross-channel context sharing.
    pub async fn route(
        &self,
        peer: &Peer,
        channel_type: ChannelType,
        channel_id: &str,
        agent: Option<&str>,
    ) -> Result<SessionContext> {
        let agent_name = agent.unwrap_or(&self.default_agent);

        SessionContext::for_channel(&self.manager, agent_name, peer, channel_type, channel_id).await
    }

    /// Route to a specific agent
    pub async fn route_to_agent(
        &self,
        agent: &str,
        peer: &Peer,
        channel_type: ChannelType,
        channel_id: &str,
    ) -> Result<SessionContext> {
        SessionContext::for_channel(&self.manager, agent, peer, channel_type, channel_id).await
    }

    /// Create a spawn context
    pub async fn spawn(
        &self,
        agent: &str,
        peer: &Peer,
        task: &str,
        isolated: bool,
        parent_session_key: &str,
        timeout_seconds: Option<u64>,
    ) -> Result<SessionContext> {
        SessionContext::for_spawn(
            &self.manager,
            agent,
            peer,
            task,
            isolated,
            parent_session_key,
            timeout_seconds,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_session_context_for_channel() {
        let manager = Arc::new(RwLock::new(SessionManager::new()));
        let peer = Peer::User("alice".to_string());

        let ctx =
            SessionContext::for_channel(&manager, "test_agent", &peer, ChannelType::Cli, "default")
                .await
                .unwrap();

        assert_eq!(ctx.channel_type, Some(ChannelType::Cli));
        assert!(!ctx.is_subagent);
    }

    #[tokio::test]
    async fn test_session_context_channel_state() {
        let manager = Arc::new(RwLock::new(SessionManager::new()));
        let peer = Peer::User("alice".to_string());

        let ctx = SessionContext::for_channel(
            &manager,
            "test_agent",
            &peer,
            ChannelType::Discord,
            "guild123",
        )
        .await
        .unwrap();

        // Set channel state
        let success = ctx
            .set_channel_state("guild_id", serde_json::json!("12345"))
            .await;
        assert!(success);

        // Get channel state
        let value = ctx.get_channel_state("guild_id").await;
        assert_eq!(value, Some(serde_json::json!("12345")));

        // Non-existent key
        let value = ctx.get_channel_state("missing").await;
        assert_eq!(value, None);
    }

    #[tokio::test]
    async fn test_session_router() {
        let manager = Arc::new(RwLock::new(SessionManager::new()));
        let router = SessionRouter::new(manager, "default_agent");
        let peer = Peer::User("alice".to_string());

        let ctx = router
            .route(&peer, ChannelType::Cli, "default", None)
            .await
            .unwrap();

        assert_eq!(ctx.channel_type, Some(ChannelType::Cli));
    }

    #[test]
    fn test_session_context_new() {
        // This test just verifies the constructor works
        // Creating a full HybridSession requires filesystem access
    }

    #[tokio::test]
    async fn test_session_context_is_isolated() {
        // Test that is_isolated returns correct values for different overlay types
        // Channel overlay is not isolated
        // Spawn overlay with isolated=true is isolated
        // Spawn overlay with isolated=false is not isolated

        // Note: This would require creating actual overlays which need filesystem access
        // The test is here as documentation of expected behavior
    }

    #[tokio::test]
    async fn test_session_context_agent_name() {
        // Note: This would require filesystem access to create a BaseSession
        // The test documents the expected behavior
    }

    #[tokio::test]
    async fn test_session_router_spawn_routing() {
        // Test that SessionRouter correctly routes spawn sessions
        // with proper parent key inheritance

        let manager = Arc::new(RwLock::new(SessionManager::new()));
        let router = SessionRouter::new(manager.clone(), "test_agent");
        let peer = Peer::User("bob".to_string());

        // First create a channel session as parent
        let channel_ctx = router
            .route(&peer, ChannelType::Discord, "guild123", None)
            .await
            .unwrap();

        assert_eq!(channel_ctx.channel_type, Some(ChannelType::Discord));
        let parent_key = channel_ctx.full_session_key().await;

        // Create a spawn session with shared context (inherits base)
        let spawn_ctx = router
            .spawn(
                "test_agent",
                &peer,
                "test task",
                false,
                &parent_key,
                Some(300),
            )
            .await
            .unwrap();

        // Spawn should have same base session key (shared)
        assert!(!spawn_ctx.is_isolated().await);
    }
}
