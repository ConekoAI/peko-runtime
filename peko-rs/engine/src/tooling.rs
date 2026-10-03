//! Engine tool execution, prompt rendering, and lifecycle ports.
//! The host implements these ports without an engine → host dependency.

use anyhow::Result;

/// One tool call, fully attributed (ADR-066 D2). Identity fields feed
/// audit, reserved-param injection, and the D9 ownership boundary —
/// never a capability gate (deleted in P2).
#[derive(Debug)]
pub struct ToolCallSpec {
    /// The tool to execute (catalog name).
    pub tool_name: String,
    /// JSON parameters from the LLM.
    pub params: serde_json::Value,
    /// Working directory override (`None` = the tool's default).
    pub workspace: Option<String>,
    /// Agent identity (DID) — reserved-param injection.
    pub agent_id: Option<String>,
    /// Session id — reserved-param injection + session attribution.
    pub session_id: Option<String>,
    /// Resolved caller identity (pekohub sub, API key id, or `local`)
    /// for per-user permission checks and audit logging (issue #17).
    pub caller_id: Option<String>,
    /// Owning principal id — per-principal tool state + quota
    /// attribution.
    pub principal_id: Option<String>,
    /// Human-readable principal name (cron-scoped tools target jobs
    /// by it).
    pub principal_name: Option<String>,
    /// Soft-interrupt abort receiver. When `Some`, the tool layer's
    /// `is_aborted()` check is meaningful; `None` for non-cancellable
    /// dispatches.
    pub abort_signal: Option<tokio::sync::watch::Receiver<bool>>,
}

impl ToolCallSpec {
    /// A bare call: just the tool name + params, everything else unset.
    #[must_use]
    pub fn new(tool_name: impl Into<String>, params: serde_json::Value) -> Self {
        Self {
            tool_name: tool_name.into(),
            params,
            workspace: None,
            agent_id: None,
            session_id: None,
            caller_id: None,
            principal_id: None,
            principal_name: None,
            abort_signal: None,
        }
    }
}

/// Input to [`ToolFunnel::render_prompt_sections`]: the per-turn
/// rendering context the providers resolve against.
#[derive(Debug, Clone)]
pub struct PromptSectionRequest {
    /// Owning principal id.
    pub principal_id: String,
    /// The principal's workspace path (providers scan
    /// `<workspace>/{roles,skills,workflows}` and read
    /// `<workspace>/principal.toml`).
    pub workspace: String,
    /// The running session's id (session-context providers key off it).
    pub session_id: String,
}

/// The rendered per-turn tail sections: `(section_name, text)` pairs.
/// Built-in section names: `identity`, `roles`, `skills`,
/// `workflows`, `session_context`. Workspace-hook `PromptSection`
/// binds arrive under their own names.
#[derive(Debug, Clone, Default)]
pub struct PromptSections {
    /// `(section name, rendered text)` pairs; empty text means the
    /// section is absent this turn.
    pub sections: Vec<(String, String)>,
}

impl PromptSections {
    /// Look up a rendered section by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.sections
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, t)| t.as_str())
    }
}

/// The engine-facing tool-execution seam (ADR-066 D2). Implemented by
/// root's `ToolingRuntime` composition; see
/// `peko-rs/core/src/tools/runtime.rs`.
#[async_trait::async_trait]
pub trait ToolFunnel: Send + Sync + 'static {
    /// Execute one tool call. Returns the `(display, json, success)`
    /// triplet — unavailable tools and tool errors arrive as
    /// `success: false` data, not a transport error.
    async fn execute(&self, call: ToolCallSpec) -> Result<(String, serde_json::Value, bool)>;

    /// The wire catalog for `principal_id` (`tools[]` JSON-schema
    /// array shape). Every registered tool is visible (ADR-066 D1/D5).
    async fn list_tool_definitions(
        &self,
        principal_id: &peko_subject::PrincipalId,
    ) -> Vec<peko_provider_api::ToolDefinition>;

    /// Render the per-turn prompt tail sections (identity, workspace
    /// catalogs, session context, plus workspace-hook `PromptSection`
    /// binds). ADR-052 D2/D6.
    async fn render_prompt_sections(&self, request: &PromptSectionRequest) -> PromptSections;
}

/// The engine-facing lifecycle/bookkeeping seam (ADR-066 D2 split).
///
/// Observe-only Stop/AfterAgent hooks, per-agent session keys, and the
/// parallel-execution catalog probe. Implemented by the root-side runtime
/// using the minimal workspace hook dispatcher (ADR-066 P4).
#[async_trait::async_trait]
pub trait EngineHooks: Send + Sync + 'static {
    /// F33 gate probe: is the named tool parallelizable for
    /// `principal_id`? Returns `true` if the tool isn't registered —
    /// the dispatch will fail anyway, and admitting without serializing
    /// is the right "no-op" fallback.
    async fn is_parallelizable(
        &self,
        tool_name: &str,
        principal_id: &peko_subject::PrincipalId,
    ) -> bool;

    /// Fire the Stop workspace point (observe-only; return value discarded).
    async fn fire_stop_hook(&self, payload: serde_json::Value);

    /// Fire the AfterAgent workspace point (observe-only; return value
    /// discarded).
    async fn fire_after_agent_hook(&self, payload: serde_json::Value);

    /// Set the per-agent session key (issue #68 — concurrent agents
    /// use distinct session keys on the shared runtime). The lifted
    /// `AgenticLoop::run_inner` calls this once at start with
    /// `(self.agent.identity_did(), Some(session_id))`. Passing `None`
    /// clears the entry.
    async fn set_session_key(&self, agent_id: &str, key: Option<String>);
}

/// Combined execution and lifecycle seam.
pub trait ToolingSeam: ToolFunnel + EngineHooks {}

impl<T: ToolFunnel + EngineHooks> ToolingSeam for T {}
