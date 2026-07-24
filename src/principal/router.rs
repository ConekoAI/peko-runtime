use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::Arc;

use crate::common::types::message::LlmMessage;
use crate::observability::Observability;
use peko_session::InboxRegistry;

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

impl fmt::Display for ContextInjectionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Memory => write!(f, "memory"),
            Self::Session => write!(f, "session"),
            Self::File => write!(f, "file"),
            Self::Todo => write!(f, "todo"),
        }
    }
}

/// Format recalled context injections as an ephemeral system-role message
/// for the LLM. Returns an empty vector when there is nothing to recall.
pub fn recalled_context_messages(injections: &[ContextInjection]) -> Vec<LlmMessage> {
    if injections.is_empty() {
        return Vec::new();
    }

    let mut lines = vec!["Prior context:".to_string()];
    for injection in injections {
        lines.push(format!(
            "- [{} {}]: {}",
            injection.kind, injection.id, injection.content
        ));
    }

    vec![LlmMessage::system(lines.join("\n"))]
}

/// Context passed to a `PrincipalRouter`.
pub struct RouterContext {
    pub principal_id: super::PrincipalId,
    pub principal_name: String,
    pub peer: peko_auth::Subject,
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
    /// Per-message configured model override (`peko send --model ...`).
    /// When `Some`, takes precedence over the principal's
    /// `preferred_model_id`. Resolver source becomes
    /// `ResolveSource::ExplicitOverride`.
    pub override_model: Option<String>,
}

// Manual `Debug` impl — `InboxRegistry` (peko-session) contains an
// `Arc<dyn AsyncInboxLike>` so it doesn't derive `Debug`. Custom
// impls of `Debug` for wrappers containing it would otherwise force
// `peko_session` to add a `Debug` impl that allocates a formatter
// name. Same for `Clone` — `InboxRegistry` is wrapped in `Arc` and
// implements `Clone`, so a trivial derived `Clone` works.
impl std::fmt::Debug for RouterContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RouterContext")
            .field("principal_id", &self.principal_id)
            .field("principal_name", &self.principal_name)
            .field("peer", &self.peer)
            .field("message", &self.message)
            .field("channel", &self.channel)
            .field("routing", &self.routing)
            .field("recalled_context", &self.recalled_context)
            .field("available_agents", &self.available_agents)
            .field("capabilities", &self.capabilities)
            .field("intent", &self.intent)
            .field("governance", &self.governance)
            .field("extension_store", &self.extension_store)
            .field("active_extensions", &self.active_extensions)
            .field("inbox_registry", &"<InboxRegistry>")
            .field("session_creation_lock", &"<Mutex>")
            .field("observability", &self.observability)
            .field("override_model", &self.override_model)
            .finish()
    }
}

impl Clone for RouterContext {
    fn clone(&self) -> Self {
        Self {
            principal_id: self.principal_id.clone(),
            principal_name: self.principal_name.clone(),
            peer: self.peer.clone(),
            message: self.message.clone(),
            channel: self.channel.clone(),
            routing: self.routing.clone(),
            recalled_context: self.recalled_context.clone(),
            available_agents: self.available_agents.clone(),
            capabilities: self.capabilities.clone(),
            intent: self.intent.clone(),
            governance: self.governance.clone(),
            extension_store: self.extension_store.clone(),
            active_extensions: self.active_extensions.clone(),
            inbox_registry: Arc::clone(&self.inbox_registry),
            session_creation_lock: Arc::clone(&self.session_creation_lock),
            observability: self.observability.clone(),
            override_model: self.override_model.clone(),
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::types::message::{ContentBlock, MessageRole};

    #[test]
    fn test_recalled_context_messages_empty() {
        let msgs = recalled_context_messages(&[]);
        assert!(msgs.is_empty());
    }

    #[test]
    fn test_recalled_context_messages_single_session() {
        let injections = vec![ContextInjection {
            kind: ContextInjectionKind::Session,
            id: "s1".to_string(),
            content: "previous summary".to_string(),
        }];
        let msgs = recalled_context_messages(&injections);
        assert_eq!(msgs.len(), 1);
        assert!(matches!(msgs[0].role, MessageRole::System));
        let text = match msgs[0].content.first() {
            Some(ContentBlock::Text { text }) => text.as_str(),
            _ => panic!("expected text block"),
        };
        assert!(text.starts_with("Prior context:"));
        assert!(text.contains("- [session s1]: previous summary"));
    }

    #[test]
    fn test_context_injection_kind_display() {
        assert_eq!(ContextInjectionKind::Memory.to_string(), "memory");
        assert_eq!(ContextInjectionKind::Session.to_string(), "session");
        assert_eq!(ContextInjectionKind::File.to_string(), "file");
        assert_eq!(ContextInjectionKind::Todo.to_string(), "todo");
    }
}
