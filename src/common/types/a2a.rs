//! A2A message request/response types and the `AgentMessageService` trait.
//!
//! These types were originally defined in `tunnel::a2a_message_types` and the
//! trait lived in `tunnel::a2a_send_tool`. They were promoted to
//! `common::types::a2a` so `src/extensions/framework/` can hold a trait-object
//! reference to an agent message service without depending on the concrete
//! `agents::StatelessAgentService` or on `tunnel` types (Issue 015/020 boundary
//! Rule 5).
//!
//! `tunnel::a2a_message_types` and `tunnel::a2a_send_tool` keep thin re-exports
//! for backward compatibility with existing callers.

use crate::auth::principal::Principal;
use crate::common::types::message::TokenUsage;

/// Tool call information in response
#[derive(Debug, Clone)]
pub struct ToolCallInfo {
    /// Tool call ID
    pub id: String,
    /// Tool name
    pub name: String,
    /// Tool parameters
    pub parameters: serde_json::Value,
    /// Tool result (if available)
    pub result: Option<String>,
}

/// Message request for high-level message execution
///
/// This type is used by `execute_message()` and `execute_message_streaming()`
/// to provide a unified interface for message sending.
#[derive(Debug, Clone)]
pub struct A2aMessageRequest {
    /// Agent name
    pub agent_name: String,
    /// Team (optional)
    pub team: Option<String>,
    /// Message content
    pub message: String,
    /// Session ID (optional - creates new if not provided)
    pub session_id: Option<String>,
    /// Force new session
    pub new_session: bool,
    /// Timeout in seconds (optional)
    pub timeout_secs: Option<u64>,
    /// Resolved caller identity for session isolation.
    ///
    /// Empty by default — production callers **must** set this explicitly
    /// via [`A2aMessageRequest::with_user`] before handing the request to
    /// the agentic loop. The legacy literal `"default"` was removed
    /// (issue #17) so that no production path can ever attribute a
    /// request to a placeholder user. Tests that don't care about
    /// per-user attribution can leave this empty.
    pub user: String,
    /// Caller agent name for A2A messaging (optional)
    pub caller_agent: Option<String>,
    /// Resolved caller principal for session peer attribution
    /// (issue #24). When set, this takes precedence over
    /// [`A2aMessageRequest::user`] when constructing the session peer.
    pub caller_principal: Option<Principal>,
}

impl A2aMessageRequest {
    /// Create a new message request.
    ///
    /// `user` defaults to the empty string. Production code paths
    /// (tunnel dispatcher, IPC server, CLI frontends) all override this
    /// via [`A2aMessageRequest::with_user`] so the agentic loop and audit
    /// log see a real, resolved caller. Tests can leave it empty.
    pub fn new(agent_name: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            agent_name: agent_name.into(),
            team: None,
            message: message.into(),
            session_id: None,
            new_session: false,
            timeout_secs: None,
            user: String::new(),
            caller_agent: None,
            caller_principal: None,
        }
    }

    /// Set user for session isolation
    pub fn with_user(mut self, user: impl Into<String>) -> Self {
        self.user = user.into();
        self
    }

    /// Set team
    pub fn with_team(mut self, team: impl Into<String>) -> Self {
        self.team = Some(team.into());
        self
    }

    /// Set session ID
    pub fn with_session(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    /// Set session ID from Option (preserves None)
    #[must_use]
    pub fn with_session_opt(mut self, session_id: Option<String>) -> Self {
        self.session_id = session_id;
        self
    }

    /// Set team from Option (preserves None)
    #[must_use]
    pub fn with_team_opt(mut self, team: Option<String>) -> Self {
        self.team = team;
        self
    }

    /// Set new session flag
    #[must_use]
    pub fn with_new_session(mut self, new: bool) -> Self {
        self.new_session = new;
        self
    }

    /// Set timeout
    #[must_use]
    pub fn with_timeout(mut self, secs: u64) -> Self {
        self.timeout_secs = Some(secs);
        self
    }

    /// Set caller agent name for A2A messaging
    #[must_use]
    pub fn with_caller_agent(mut self, caller: impl Into<String>) -> Self {
        self.caller_agent = Some(caller.into());
        self
    }

    /// Set caller agent from Option, filtering out empty strings
    #[must_use]
    pub fn with_caller_agent_opt(mut self, caller: Option<String>) -> Self {
        self.caller_agent = caller.filter(|s| !s.is_empty());
        self
    }

    /// Set the resolved caller principal (issue #24).
    ///
    /// Use this for A2A messaging paths where the caller is an agent,
    /// not a user. The principal is used to construct the session peer
    /// on the receiving agent so the session is keyed under
    /// `agent:{caller}` (not `user:{caller}`).
    #[must_use]
    pub fn with_caller_principal(mut self, principal: Principal) -> Self {
        self.caller_principal = Some(principal);
        self
    }

    /// Set the resolved caller principal from an Option, rejecting
    /// principals that cannot be a session peer (Team / Public).
    #[must_use]
    pub fn with_caller_principal_opt(mut self, principal: Option<Principal>) -> Self {
        self.caller_principal = principal.filter(|p| p.is_session_peer());
        self
    }
}

/// Message sending result
///
/// This is the high-level result type returned by `execute_message()`
#[derive(Debug, Clone)]
pub struct A2aMessageResponse {
    /// Response content
    pub content: String,
    /// Session ID used
    pub session_id: String,
    /// Whether this was a new session
    pub is_new_session: bool,
    /// Token usage
    pub usage: TokenUsage,
    /// Tool calls made
    pub tool_calls: Vec<ToolCallInfo>,
    /// Execution duration in milliseconds
    pub duration_ms: u64,
    /// Number of iterations
    pub iterations: usize,
    /// Whether execution succeeded
    pub success: bool,
    /// Error message (if failed)
    pub error: Option<String>,
}

/// Convenience aliases used by the A2A tool and agent service.
pub use self::{A2aMessageRequest as MessageRequest, A2aMessageResponse as MessageResult};

/// Minimum interface the A2A send tool needs from a peer agent service.
///
/// Lives in `common::types::a2a` (not `agents`) so callers can construct
/// `Arc<dyn AgentMessageService>` without depending on the concrete
/// `StatelessAgentService` type. Implementations convert the common
/// request/response types to whatever internal shape they use.
///
/// Breaking cycle 5 (per `PLAN §2.5`).
#[async_trait::async_trait]
pub trait AgentMessageService: Send + Sync {
    async fn execute_message(
        &self,
        req: A2aMessageRequest,
    ) -> anyhow::Result<A2aMessageResponse>;
}
