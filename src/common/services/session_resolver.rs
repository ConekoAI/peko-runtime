//! Session Resolver
//!
//! Provides unified session resolution logic for both CLI and HTTP API.
//! This module is now a thin wrapper around SessionManager, which is the
//! single authority for all session operations.
//!
//! DEPRECATED: This module is kept for backward compatibility.
//! New code should use SessionManager directly.

use crate::common::paths::PathResolver;
use crate::session::context::SessionContext;
use crate::session::types::ChannelType;
use crate::session::{ResolvedSession, SessionManager};
use anyhow::Result;

/// Session resolution strategy
///
/// DEPRECATED: Use `session::ResolutionStrategy` directly.
pub use crate::session::ResolutionStrategy;

/// Unified session resolver
///
/// This is a thin wrapper around SessionManager for backward compatibility.
/// SessionManager is the single authority for session operations.
///
/// DEPRECATED: Use SessionManager directly.
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
    /// Delegates to SessionManager, which is the single authority.
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
        // Create SessionManager with proper path resolution
        let mut session_manager =
            SessionManager::for_cli(self.path_resolver.clone(), agent_name, team);

        // Delegate to SessionManager (single authority)
        let resolved = session_manager
            .resolve_session(agent_name, team, channel, channel_id, session_id, force_new)
            .await?;

        Ok((resolved.context, resolved.is_new))
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
