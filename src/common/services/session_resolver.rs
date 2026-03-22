//! Session Resolver
//!
//! Provides unified session resolution logic for both CLI and HTTP API.
//! This ensures consistent behavior across interfaces:
//! - Auto-resume active session when no session_id provided
//! - Create new session when explicitly requested
//! - Resume specific session when session_id is provided
//!
//! This module addresses the architectural gap where CLI and HTTP API
//! had divergent session management behaviors.

use crate::common::paths::PathResolver;
use crate::session::context::SessionContext;
use crate::session::types::{ChannelType, Peer};
use crate::session::SessionManager;
use anyhow::{Context, Result};
use std::path::PathBuf;
use tracing::{debug, info};

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

/// Unified session resolver
///
/// This is the SINGLE POINT OF TRUTH for session resolution across all interfaces.
pub struct SessionResolver {
    path_resolver: PathResolver,
}

impl SessionResolver {
    /// Create a new session resolver
    pub fn new(path_resolver: PathResolver) -> Self {
        Self { path_resolver }
    }

    /// Resolve session for an agent
    ///
    /// # Arguments
    /// * `agent_name` - Name of the agent
    /// * `team` - Optional team name
    /// * `channel` - Channel type (Cli, Http, etc.)
    /// * `channel_id` - Channel identifier
    /// * `session_id` - Optional specific session ID to resume
    /// * `force_new` - Force creation of a new session
    ///
    /// # Returns
    /// A tuple of (SessionContext, bool) where the bool indicates if this is a new session
    pub async fn resolve_session(
        &self,
        agent_name: &str,
        team: Option<&str>,
        channel: ChannelType,
        channel_id: &str,
        session_id: Option<String>,
        force_new: bool,
    ) -> Result<(SessionContext, bool)> {
        let strategy = if force_new {
            ResolutionStrategy::ForceNew
        } else if session_id.is_some() {
            ResolutionStrategy::Specific
        } else {
            ResolutionStrategy::AutoResume
        };

        info!(
            "Resolving session for agent '{}' with strategy {:?}",
            agent_name, strategy
        );

        match strategy {
            ResolutionStrategy::ForceNew => self
                .create_new_session(agent_name, team, channel, channel_id)
                .await
                .map(|ctx| (ctx, true)),
            ResolutionStrategy::Specific => {
                let sid = session_id.unwrap();
                self.resume_specific_session(agent_name, team, channel, channel_id, &sid)
                    .await
                    .map(|ctx| (ctx, false))
            }
            ResolutionStrategy::AutoResume => {
                self.auto_resume_session(agent_name, team, channel, channel_id)
                    .await
            }
        }
    }

    /// Auto-resume session: try to resume active, create new if none exists
    async fn auto_resume_session(
        &self,
        agent_name: &str,
        team: Option<&str>,
        channel: ChannelType,
        channel_id: &str,
    ) -> Result<(SessionContext, bool)> {
        let sessions_dir = self.get_sessions_dir(agent_name, team).await?;
        let peer = Peer::User(channel_id.to_string());

        // Use SessionManager as the single authority for session lookup
        let config_dir = self.path_resolver.config_dir();
        let mut session_manager = SessionManager::for_cli(agent_name, team, Some(config_dir));

        // Check peer routing via SessionManager (which uses the index internally)
        let peer_key = crate::session::key::derive_base_session_key(agent_name, &peer);
        if let Some(session_id) = session_manager
            .get_active_session_id(&peer)
            .await?
        {
            info!(
                "Auto-resuming active session '{}' for peer '{}'",
                session_id, peer_key
            );
            return self
                .resume_specific_session(agent_name, team, channel, channel_id, &session_id)
                .await
                .map(|ctx| (ctx, false));
        }

        // No active session found, create new
        debug!(
            "No active session found for agent '{}', creating new",
            agent_name
        );
        self.create_new_session(agent_name, team, channel, channel_id)
            .await
            .map(|ctx| (ctx, true))
    }

    /// Create a new session
    async fn create_new_session(
        &self,
        agent_name: &str,
        team: Option<&str>,
        channel: ChannelType,
        channel_id: &str,
    ) -> Result<SessionContext> {
        info!("Creating new session for agent '{}'", agent_name);

        let sessions_dir = self.get_sessions_dir(agent_name, team).await?;
        let peer = Peer::User(channel_id.to_string());

        // Use SessionManager for creation - it's the single authority
        let config_dir = self.path_resolver.config_dir();
        let mut session_manager = SessionManager::for_cli(agent_name, team, Some(config_dir));

        // Clear the in-memory cache to ensure we don't reuse an old session
        session_manager.remove_base_session(agent_name, &peer);

        // Create a fresh session explicitly (not get-or-create)
        let options = crate::session::SessionCreateOptions::new()
            .with_trigger("user");
        let handle = session_manager
            .create_session(agent_name, &peer, options)
            .await?;

        // Create channel overlay on the new base session
        let base = handle.base().clone();
        let hybrid = session_manager
            .create_channel_overlay_on_base(base, &peer, channel, channel_id)
            .await?;

        let ctx = SessionContext::new(hybrid).await;

        // Get session ID for logging
        let session_id = {
            let base = ctx.hybrid.base.read().await;
            base.id.clone()
        };

        info!(
            "Created new session '{}' for agent '{}'",
            session_id, agent_name
        );
        Ok(ctx)
    }

    /// Resume a specific session by ID
    async fn resume_specific_session(
        &self,
        agent_name: &str,
        team: Option<&str>,
        channel: ChannelType,
        channel_id: &str,
        session_id: &str,
    ) -> Result<SessionContext> {
        info!(
            "Resuming specific session '{}' for agent '{}'",
            session_id, agent_name
        );

        let peer = Peer::User(channel_id.to_string());
        let config_dir = self.path_resolver.config_dir();
        let mut session_manager = SessionManager::for_cli(agent_name, team, Some(config_dir));

        // FIX: Open the SPECIFIC session by ID (not peer-based lookup)
        let handle = session_manager
            .open_session(session_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Session '{}' not found", session_id))?;

        // Get the base session from the handle
        let base = handle.base().clone();

        // FIX: Create channel overlay on the opened base session
        let hybrid = session_manager
            .create_channel_overlay_on_base(base, &peer, channel, channel_id)
            .await?;

        let ctx = SessionContext::new(hybrid).await;

        info!("Successfully resumed session '{}'", session_id);
        Ok(ctx)
    }

    /// Get sessions directory for an agent
    async fn get_sessions_dir(&self, agent_name: &str, team: Option<&str>) -> Result<PathBuf> {
        let sessions_dir = self.path_resolver.agent_sessions_dir(agent_name, team);

        // Ensure directory exists
        tokio::fs::create_dir_all(&sessions_dir).await?;

        Ok(sessions_dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_resolver() -> (SessionResolver, TempDir) {
        let temp = TempDir::new().unwrap();
        let resolver = PathResolver::with_dirs(
            temp.path().join("config"),
            temp.path().join("data"),
            temp.path().join("cache"),
        );
        (SessionResolver::new(resolver), temp)
    }

    #[test]
    fn test_resolution_strategy() {
        assert_eq!(
            ResolutionStrategy::AutoResume,
            ResolutionStrategy::AutoResume
        );
        assert_eq!(ResolutionStrategy::ForceNew, ResolutionStrategy::ForceNew);
        assert_eq!(ResolutionStrategy::Specific, ResolutionStrategy::Specific);
    }
}
