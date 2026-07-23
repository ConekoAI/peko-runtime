//! Session context for agent execution
//!
//! Provides a lightweight DTO for session routing metadata.
//! All session operations go through `SessionHandle` obtained from `SessionManager`.

use super::types::ChannelType;
use peko_auth::Subject;

/// Lightweight context for session-aware agent execution — pure DTO, no operations.
///
/// This holds routing metadata for a resolved session. For actual session operations
/// (adding messages, loading history, etc.), use the `SessionHandle` from `ResolvedSession`.
#[derive(Debug, Clone)]
pub struct SessionContext {
    /// Session ID (UUID)
    pub session_id: String,
    /// Agent name
    pub agent_name: String,
    /// Base session key
    pub session_key: String,
    /// Full session key (including overlay if present)
    pub full_session_key: String,
    /// The peer this session belongs to
    pub peer: Subject,
    /// Channel type (if applicable)
    pub channel_type: Option<ChannelType>,
    /// Whether this session is for a subagent/spawn
    pub is_subagent: bool,
    /// Whether this is an isolated spawn
    pub is_isolated: bool,
}

impl SessionContext {
    /// Create a new session context from components
    pub fn new(
        session_id: impl Into<String>,
        agent_name: impl Into<String>,
        session_key: impl Into<String>,
        full_session_key: impl Into<String>,
        peer: Subject,
        channel_type: Option<ChannelType>,
        is_subagent: bool,
        is_isolated: bool,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            agent_name: agent_name.into(),
            session_key: session_key.into(),
            full_session_key: full_session_key.into(),
            peer,
            channel_type,
            is_subagent,
            is_isolated,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_context_new() {
        let ctx = SessionContext::new(
            "sess_123",
            "test_agent",
            "agent:test_agent:peer:user:alice",
            "agent:test_agent:peer:user:alice",
            Subject::User("alice".to_string()),
            Some(ChannelType::Cli),
            false,
            false,
        );
        assert_eq!(ctx.session_id, "sess_123");
        assert_eq!(ctx.agent_name, "test_agent");
        assert_eq!(ctx.channel_type, Some(ChannelType::Cli));
        assert!(!ctx.is_subagent);
        assert!(!ctx.is_isolated);
    }
}
