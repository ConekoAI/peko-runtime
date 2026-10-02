//! Payloads for the six workspace observer points (ADR-066).
use peko_session::SessionSnapshot;

#[derive(Debug)]
pub enum HookResult {
    Continue(HookOutput),
    PassThrough,
    Handled,
    Replace(HookOutput),
    Error(anyhow::Error),
}

#[derive(Debug, Clone, Default)]
pub enum HookOutput {
    #[default]
    Unit,
    Text(String),
    Json(serde_json::Value),
    Vec(Vec<HookOutput>),
}

impl HookOutput {
    #[must_use]
    pub fn as_text(&self) -> Option<&str> {
        if let Self::Text(text) = self {
            Some(text)
        } else {
            None
        }
    }
    #[must_use]
    pub fn as_json(&self) -> Option<&serde_json::Value> {
        if let Self::Json(value) = self {
            Some(value)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Default)]
pub enum HookInput {
    #[default]
    Unit,
    ToolCall {
        tool_name: String,
        params: serde_json::Value,
        /// Workspace directory for tool execution (optional)
        workspace: Option<String>,
        /// Agent identifier for reserved parameter injection (optional)
        agent_id: Option<String>,
        /// Session identifier for reserved parameter injection (optional)
        session_id: Option<String>,
        /// Resolved caller identity (pekohub sub, API key id, or `local`)
        /// — populated on tunneled requests so per-user permission
        /// checks (issue #17) and audit logging can attribute the call
        /// to a real user. `None` for local CLI invocations.
        caller_id: Option<String>,
        /// Owning principal scope for observation and attribution.
        principal_id: Option<String>,
        /// Human-readable Principal name. Cron-scoped tools use this to
        /// create and filter jobs for the current Principal.
        principal_name: Option<String>,
        /// Soft-interrupt signal carried from the engine tool call.
        abort_signal: Option<tokio::sync::watch::Receiver<bool>>,
    },
    SessionState(SessionSnapshot),
    Json(serde_json::Value),
}

#[derive(Debug, Clone, Default)]
pub struct ToolRuntimeContext {
    pub agent_id: Option<String>,
    pub session_id: Option<String>,
    pub peer_id: Option<String>,
    pub workspace: Option<String>,
    pub run_id: Option<String>,
    pub principal_id: Option<String>,
    pub principal_name: Option<String>,
    /// Soft-interrupt abort signal receiver. Plumbed from the engine's
    /// `CancellationToken` (PR #128) via
    /// [`bridge_from_cancellation_token`] in `peko_tools_core::exec`.
    /// When `Some`, the tool layer's `is_aborted()` check is meaningful
    /// in production; `None` for hooks fired outside a tool execution
    /// (prompt-build, async status checks) and for legacy callers that
    /// haven't been migrated to thread a token through.
    pub abort_signal: Option<tokio::sync::watch::Receiver<bool>>,
}

impl ToolRuntimeContext {
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_agent_id(mut self, agent_id: impl Into<String>) -> Self {
        self.agent_id = Some(agent_id.into());
        self
    }

    #[must_use]
    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    #[must_use]
    pub fn with_peer_id(mut self, peer_id: impl Into<String>) -> Self {
        self.peer_id = Some(peer_id.into());
        self
    }

    #[must_use]
    pub fn with_workspace(mut self, workspace: impl Into<String>) -> Self {
        self.workspace = Some(workspace.into());
        self
    }

    #[must_use]
    pub fn with_run_id(mut self, run_id: impl Into<String>) -> Self {
        self.run_id = Some(run_id.into());
        self
    }

    #[must_use]
    pub fn with_principal_id(mut self, principal_id: impl Into<String>) -> Self {
        self.principal_id = Some(principal_id.into());
        self
    }

    #[must_use]
    pub fn with_principal_name(mut self, principal_name: impl Into<String>) -> Self {
        self.principal_name = Some(principal_name.into());
        self
    }

    /// Bridge the engine's `CancellationToken` into the tool layer by
    /// supplying the `watch::Receiver<bool>` half of an `AbortSignal`.
    #[must_use]
    pub fn with_abort_signal(mut self, abort_signal: tokio::sync::watch::Receiver<bool>) -> Self {
        self.abort_signal = Some(abort_signal);
        self
    }
}
