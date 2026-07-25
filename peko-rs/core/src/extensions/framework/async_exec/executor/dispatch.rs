//! Dispatch context for [`AsyncExecutor::dispatch_tool`] and
//! [`AsyncExecutor::dispatch_tool_with_signal`].
//!
//! F38 introduces a typed `ToolDispatchContext` that consolidates the 11
//! fields the F37 canonical funnel
//! ([`ExtensionCore::execute_tool_via_hook`](crate::extensions::framework::core::ExtensionCore::execute_tool_via_hook))
//! requires. Bundling them means callers don't have to thread 11 named
//! parameters at each call site; the executor owns closure construction
//! internally so the funnel is mandatory.
//!
//! See `f37-gate-bypass-fix.md` for the F37 background and
//! `f38-executor-redesign.md` (forthcoming) for the F38 context.

/// Consolidated dispatch context for `AsyncExecutor::dispatch_tool*`.
///
/// All fields except `tool_name`, `params`, and `parent_session_key`
/// are optional — defaults to `None` / empty `Vec`.
#[derive(Debug, Clone)]
pub struct ToolDispatchContext {
    /// The tool to dispatch (e.g. `"Bash"`, `"Read"`).
    pub tool_name: String,
    /// JSON params passed to the tool.
    pub params: serde_json::Value,
    /// Session key for completion-event routing + `parent_session_key`
    /// stamping on the spawned task file.
    pub parent_session_key: String,

    /// Working directory override. `None` means the tool's default.
    pub workspace: Option<String>,
    /// Agent DID driving the dispatch. Used by reserved-param injection
    /// (e.g. `Read`'s `file_path` defaults).
    pub agent_id: Option<String>,
    /// Session ID driving the dispatch. Used by reserved-param injection.
    pub session_id: Option<String>,
    /// Caller ID for per-user permission checks and audit logging.
    pub caller_id: Option<String>,

    /// Principal ID (the principal's [`String`] representation).
    /// `None` means the system principal — F19 metering will see
    /// `QuotaMeter::unlimited()`.
    pub principal_id: Option<String>,
    /// Human-readable principal name (for Principal-scoped tools like
    /// `CronCreate`).
    pub principal_name: Option<String>,

    /// Per-call capability grants. The capability gate at
    /// `registry.rs:260-277` evaluates tool access against this set.
    /// `None` or empty means fail-closed (no grants).
    pub capabilities: Vec<String>,
    /// Active extension IDs for this principal. When non-empty, the
    /// gate verifies the tool's owning extension is active.
    pub active_extensions: Vec<String>,
}

impl ToolDispatchContext {
    /// Start a builder with `tool_name` + `params` + the most-common
    /// `parent_session_key`.
    #[must_use]
    pub fn builder(
        tool_name: impl Into<String>,
        params: serde_json::Value,
        parent_session_key: impl Into<String>,
    ) -> Self {
        Self {
            tool_name: tool_name.into(),
            params,
            parent_session_key: parent_session_key.into(),
            workspace: None,
            agent_id: None,
            session_id: None,
            caller_id: None,
            principal_id: None,
            principal_name: None,
            capabilities: Vec::new(),
            active_extensions: Vec::new(),
        }
    }

    /// Convenience: pre-fill principal_id + capabilities for the F37
    /// `AsyncSpawnTool` and `cron_engine` snapshot pattern.
    #[must_use]
    pub fn for_principal(mut self, principal_id: String, capabilities: Vec<String>) -> Self {
        self.principal_id = Some(principal_id);
        self.capabilities = capabilities;
        self
    }

    /// Set the workspace override.
    #[must_use]
    pub fn with_workspace(mut self, workspace: impl Into<String>) -> Self {
        self.workspace = Some(workspace.into());
        self
    }

    /// Set the agent DID.
    #[must_use]
    pub fn with_agent_id(mut self, agent_id: impl Into<String>) -> Self {
        self.agent_id = Some(agent_id.into());
        self
    }

    /// Set the session ID.
    #[must_use]
    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    /// Set the caller ID.
    #[must_use]
    pub fn with_caller_id(mut self, caller_id: impl Into<String>) -> Self {
        self.caller_id = Some(caller_id.into());
        self
    }

    /// Set the principal name (display string).
    #[must_use]
    pub fn with_principal_name(mut self, principal_name: impl Into<String>) -> Self {
        self.principal_name = Some(principal_name.into());
        self
    }

    /// Set the principal ID (`String` form). Convenience for tests and
    /// sites that already split principal_id/principal_name into
    /// separate fields; production code uses
    /// [`Self::for_principal`] which sets both at once.
    #[must_use]
    pub fn with_principal_id(mut self, principal_id: impl Into<String>) -> Self {
        self.principal_id = Some(principal_id.into());
        self
    }

    /// Set the active extension IDs.
    #[must_use]
    pub fn with_active_extensions(mut self, ids: Vec<String>) -> Self {
        self.active_extensions = ids;
        self
    }

    /// Build the `task_id` for the spawned task. Convention:
    /// `{tool_name}:{uuid_v4}` so multiple spawns of the same tool are
    /// distinguishable in the registry (matches what `AsyncSpawnTool`
    /// and `cron_engine` do today).
    #[must_use]
    pub fn make_task_id(&self) -> String {
        format!("{}:{}", self.tool_name, uuid::Uuid::new_v4())
    }
}
