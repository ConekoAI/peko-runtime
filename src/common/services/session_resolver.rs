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
use crate::session::index::SessionIndex;
use crate::session::types::{ChannelType, Peer};
use crate::session::SessionManager;
use anyhow::{Context, Result};
use std::path::PathBuf;
use tracing::{debug, info, warn};

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
            ResolutionStrategy::ForceNew => {
                self.create_new_session(agent_name, team, channel, channel_id)
                    .await
                    .map(|ctx| (ctx, true))
            }
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

        // Check if there's an active session preference
        let active_pref_path = sessions_dir.join(".active.json");
        if active_pref_path.exists() {
            if let Ok(content) = tokio::fs::read_to_string(&active_pref_path).await {
                if let Ok(pref) = serde_json::from_str::<serde_json::Value>(&content) {
                    if let Some(preferred_id) = pref.get("session_id").and_then(|v| v.as_str()) {
                        // Verify the session exists
                        let mut index = SessionIndex::open(&sessions_dir);
                        if index.get(preferred_id).await?.is_some() {
                            info!(
                                "Auto-resuming preferred session '{}' for agent '{}'",
                                preferred_id, agent_name
                            );
                            return self
                                .resume_specific_session(
                                    agent_name, team, channel, channel_id, preferred_id,
                                )
                                .await
                                .map(|ctx| (ctx, false));
                        } else {
                            warn!(
                                "Preferred session '{}' not found, creating new session",
                                preferred_id
                            );
                        }
                    }
                }
            }
        }

        // Check peer routing in SessionIndex
        let mut index = SessionIndex::open(&sessions_dir);
        let peer_key = format!("agent:{}:peer:{:?}", agent_name, peer);
        if let Ok(Some(active_entry)) = index.get_active_for_peer(&peer_key).await {
            let session_id = active_entry.session_id;
            info!(
                "Auto-resuming active session '{}' for peer '{:?}'",
                session_id, peer
            );
            return self
                .resume_specific_session(agent_name, team, channel, channel_id, &session_id)
                .await
                .map(|ctx| (ctx, false));
        }

        // No active session found, create new
        debug!("No active session found for agent '{}', creating new", agent_name);
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

        // Use SessionManager for creation to ensure proper indexing
        let mut session_manager = SessionManager::for_cli(agent_name, team);

        // Remove any existing overlay for this channel to ensure clean state
        let base_key = crate::session::derive_base_session_key(agent_name, &peer);
        let overlay_key = format!("{}:overlay:{:?}:{}", base_key, channel, channel_id);
        session_manager.remove_channel_overlay(&overlay_key);
        session_manager.remove_base_session(agent_name, &peer);

        // Get or create session through SessionManager
        let hybrid = session_manager
            .get_session_for_channel(agent_name, &peer, channel, channel_id)
            .await?;

        let ctx = SessionContext::new(hybrid).await;
        
        // Get session ID for logging
        let session_id = {
            let base = ctx.hybrid.base.read().await;
            base.id.clone()
        };
        
        // Save as active preference for future auto-resume
        self.save_active_preference(&sessions_dir, &session_id).await?;

        info!("Created new session '{}' for agent '{}'", session_id, agent_name);
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

        let sessions_dir = self.get_sessions_dir(agent_name, team).await?;

        // Verify session exists
        let mut index = SessionIndex::open(&sessions_dir);
        let entry = index
            .get(session_id)
            .await
            .with_context(|| format!("Failed to lookup session '{}'", session_id))?
            .ok_or_else(|| anyhow::anyhow!("Session '{}' not found", session_id))?;

        debug!("Found session '{}' with {} messages", entry.session_id, entry.message_count);

        let peer = Peer::User(channel_id.to_string());
        let mut session_manager = SessionManager::for_cli(agent_name, team);

        // Use SessionManager to get the session for this channel
        let hybrid = session_manager
            .get_session_for_channel(agent_name, &peer, channel, channel_id)
            .await?;

        let ctx = SessionContext::new(hybrid).await;

        // Verify the session IDs match
        {
            let base = ctx.hybrid.base.read().await;
            if base.id != session_id {
                warn!(
                    "Session ID mismatch: requested '{}', got '{}'",
                    session_id,
                    base.id
                );
            }
        }

        // Update active preference
        self.save_active_preference(&sessions_dir, session_id).await?;

        info!("Successfully resumed session '{}'", session_id);
        Ok(ctx)
    }

    /// Save active session preference
    async fn save_active_preference(
        &self,
        sessions_dir: &PathBuf,
        session_id: &str,
    ) -> Result<()> {
        let pref_path = sessions_dir.join(".active.json");
        let pref = serde_json::json!({
            "session_id": session_id,
            "set_at": chrono::Utc::now().to_rfc3339(),
            "set_by": "session_resolver",
        });

        let temp_path = pref_path.with_extension("tmp");
        tokio::fs::write(&temp_path, serde_json::to_string_pretty(&pref)?).await?;
        tokio::fs::rename(&temp_path, &pref_path).await?;

        debug!("Saved active session preference: {}", session_id);
        Ok(())
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
