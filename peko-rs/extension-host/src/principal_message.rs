//! Principal-level message request/response types and the
//! `PrincipalMessageService` trait.
//!
//! The shapes here describe **principal-to-principal** messaging — the
//! canonical wire envelope is the protocol's A2A (Agent-to-Agent) envelope
//! (Google's protocol nomenclature), but the in-process semantic role is
//! `PrincipalMessageRequest`/`PrincipalMessageResponse`: one principal
//! invokes another's root agent.
//!
//! Moved from `src/common/types/principal_message.rs` in Phase 8 commit 2.
//! Lives in the host crate so the extension framework can hold an
//! `Arc<dyn PrincipalMessageService>` without depending on either the
//! concrete `agents::StatelessAgentService` (cycle 5) or root `tunnel`
//! types (Rule 5 of the Issue 015/020 boundary). Root re-exports via
//! `crate::common::types::principal_message::PrincipalMessageService`
//! for backwards compatibility — the orphan rule forces the
//! `StatelessAgentService` impl block to follow the trait into the host
//! crate (host is a leaf, so root does the impl using `peko_extension_host::principal_message`).
//!
//! ## Within-principal vs across-principal note
//!
//! The same execution engine (`StatelessAgentService`, the sole implementor
//! of `PrincipalMessageService`) services both:
//!   - **across-principal**: tunnel-driven `principal_send` traffic
//!     (`src/principal/principal_send_tool.rs`); and
//!   - **within-principal**: synchronous same-runtime agent dispatch (CLI
//!     frontends, IPC `Execute` path).
//!
//! Root-agent → subagent dispatch uses a different shape entirely (the
//! `AgentTool` and `AgentConfig`), not these envelope types.

use peko_message::TokenUsage;
use peko_subject::Subject;

// `ToolCallInfo` lives in `peko_message` (Phase 9b.1 lift) so that
// `peko_engine` can hold `Vec<ToolCallInfo>` on `ChannelOutput`
// without taking a host-crate dep just for a 4-field DTO.
// Re-exported here so every existing
// `peko_extension_host::principal_message::ToolCallInfo` call site
// keeps compiling unchanged.
pub use peko_message::ToolCallInfo;

/// Message request for high-level (principal-level) message execution
///
/// This type is used by `execute_message()` to describe the principal
/// message service contract: one principal invokes another's root agent,
/// carrying the prompt, optional session continuity, caller identity, and
/// timeout. The on-wire A2A envelope carries the same fields.
#[derive(Debug, Clone)]
pub struct PrincipalMessageRequest {
    /// Agent name (within the target principal — typically the root agent)
    pub agent_name: String,
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
    /// via [`PrincipalMessageRequest::with_user`] before handing the
    /// request to the agentic loop. The legacy literal `"default"` was
    /// removed (issue #17) so that no production path can ever attribute a
    /// request to a placeholder user. Tests that don't care about
    /// per-user attribution can leave this empty.
    pub user: String,
    /// Caller agent name for principal-to-principal messaging (optional)
    pub caller_agent: Option<String>,
    /// Resolved caller principal for session peer attribution
    /// (issue #24). When set, this takes precedence over
    /// [`PrincipalMessageRequest::user`] when constructing the session
    /// peer.
    pub caller_principal: Option<Subject>,
}

impl PrincipalMessageRequest {
    /// Create a new message request.
    ///
    /// `user` defaults to the empty string. Production code paths
    /// (tunnel dispatcher, IPC server, CLI frontends) all override this
    /// via [`PrincipalMessageRequest::with_user`] so the agentic loop and
    /// audit log see a real, resolved caller. Tests can leave it empty.
    pub fn new(agent_name: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            agent_name: agent_name.into(),
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

    /// Set caller agent name for principal-to-principal messaging
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
    /// Use this for principal-to-principal messaging paths where the
    /// caller is another principal, not a user. The principal is used to
    /// construct the session peer on the receiving principal so the
    /// session is keyed under `principal:{caller}` (not `user:{caller}`).
    #[must_use]
    pub fn with_caller_principal(mut self, principal: Subject) -> Self {
        self.caller_principal = Some(principal);
        self
    }

    /// Set the resolved caller principal from an Option, rejecting
    /// principals that cannot be a session peer (Team / Public).
    #[must_use]
    pub fn with_caller_principal_opt(mut self, principal: Option<Subject>) -> Self {
        self.caller_principal = principal.filter(|p| p.is_session_peer());
        self
    }
}

/// Message sending result
///
/// This is the high-level result type returned by `execute_message()`
#[derive(Debug, Clone)]
pub struct PrincipalMessageResponse {
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

/// Minimum interface the principal-send tool needs from a peer principal
/// message service.
///
/// Lives in `common::types::principal_message` (not in `agents` or
/// `tunnel`) so callers can construct `Arc<dyn PrincipalMessageService>`
/// without depending on the concrete `StatelessAgentService` type or on
/// tunnel types. The sole implementation in this codebase is
/// `StatelessAgentService`.
///
/// Breaking cycle 5 (per `PLAN §2.5`).
#[async_trait::async_trait]
pub trait PrincipalMessageService: Send + Sync {
    async fn execute_message(
        &self,
        req: PrincipalMessageRequest,
    ) -> anyhow::Result<PrincipalMessageResponse>;
}
