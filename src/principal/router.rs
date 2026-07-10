use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::observability::Observability;
use crate::session::InboxRegistry;

/// A routing decision emitted by a `PrincipalRouter`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action")]
pub enum RouteDecision {
    /// Respond directly from the router; no sub-agent invocation.
    #[serde(rename = "respond")]
    Respond { response: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextInjection {
    pub kind: ContextInjectionKind,
    pub id: String,
    pub content: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextInjectionKind {
    Memory,
    Session,
    File,
    Todo,
}

/// Context passed to a `PrincipalRouter`.
#[derive(Debug, Clone)]
pub struct RouterContext {
    pub principal_id: super::PrincipalId,
    pub principal_name: String,
    pub peer: crate::auth::Subject,
    /// The raw user message text.
    pub message: String,
    pub channel: ChannelContext,
    pub routing: super::PrincipalRoutingConfig,
    pub recalled_context: Vec<ContextInjection>,
    pub available_agents: Vec<AgentPromptSummary>,
    pub capabilities: super::Capabilities,
    pub intent: super::PrincipalIntentConfig,
    pub governance: super::PrincipalGovernanceConfig,
    /// Per-principal snapshot of all detected extensions/agents and their
    /// authority state.
    pub extension_store: super::ExtensionCatalog,
    /// Set of extension IDs that are currently active for this Principal.
    /// Derived from `extension_store.active_extensions()` and carried here
    /// so routers can thread it into `PrincipalContext` without recomputing.
    pub active_extensions: super::ActiveExtensionSet,
    /// Shared inbox registry so the router can wire the root agent
    /// to the same inbox the Principal boundary pushes steering messages into.
    pub inbox_registry: Arc<InboxRegistry>,
    /// Per-principal lock held during root-agent session creation so concurrent
    /// peers do not race on shared session metadata/index writes.
    pub session_creation_lock: Arc<tokio::sync::Mutex<()>>,
    /// Optional observability hub for audit/metrics. Threaded through to the
    /// root agent and the `Agent` tool so subagent spawns are auditable.
    pub observability: Option<Arc<Observability>>,
}

#[derive(Debug, Clone)]
pub struct AgentPromptSummary {
    /// Canonical agent id used as `subagent_type` when spawning.
    pub id: String,
    /// Human-readable display name from the frontmatter.
    pub name: String,
    pub description: Option<String>,
    /// Whether this agent is currently enabled for the principal.
    pub enabled: bool,
}

#[derive(Debug, Clone)]
pub struct ChannelContext {
    pub kind: ChannelKind,
    pub streaming: bool,
}

#[derive(Debug, Clone, Copy)]
pub enum ChannelKind {
    Cli,
    Http,
    Hub,
    A2a,
    P2p, // principal-to-principal
    Webhook,
    Cron,
    FileWatch,
}

#[async_trait]
pub trait PrincipalRouter: Send + Sync {
    /// Decide how to handle an incoming message.
    async fn route(&self, ctx: RouterContext) -> Result<RouteDecision, RouterError>;

    /// Streaming variant. The router is given an `on_event` callback
    /// that it invokes for each `AgenticEvent` it produces (typically
    /// `AssistantDelta` deltas as the root agent's response
    /// unfolds). The callback must be cheap and non-blocking.
    ///
    /// Default implementation ignores the callback and delegates to
    /// `route()`. Routers that support streaming (the canonical
    /// root-agent router, and any extension that wants live tokens) override
    /// this and forward events to the caller as they happen.
    ///
    /// `cancel` is an optional soft-interrupt token for the in-flight
    /// run. When set, the agentic loop observes it at iteration
    /// boundaries and exits cleanly with `Lifecycle::Interrupted` when
    /// signalled. Routers that don't support streaming (the default
    /// impl) can ignore it — the non-streaming `route()` path doesn't
    /// have iteration boundaries to interrupt at.
    ///
    /// Returns the same final `RouteDecision` that the non-streaming
    /// `route()` would have produced.
    async fn route_streaming(
        &self,
        ctx: RouterContext,
        _on_event: Box<dyn Fn(crate::engine::AgenticEvent) + Send + Sync>,
        cancel: Option<tokio_util::sync::CancellationToken>,
    ) -> Result<RouteDecision, RouterError> {
        let _ = cancel;
        self.route(ctx).await
    }

    /// Phase 4b: bind the local runtime's `runtime_id` (used by
    /// `principal_send` envelopes). Default implementation is a no-op
    /// (the canonical `RootRouter` overrides). Routers that don't
    /// need a runtime id ignore the call.
    fn set_caller_runtime_id(&self, _runtime_id: String) {}
}

#[derive(Debug, thiserror::Error)]
pub enum RouterError {
    #[error("routing decision invalid: {0}")]
    InvalidDecision(String),
    #[error("routing agent failed: {0}")]
    AgentFailed(String),
    #[error("routing loop detected")]
    LoopDetected,
    #[error("permission denied: {0}")]
    PermissionDenied(String),
}
