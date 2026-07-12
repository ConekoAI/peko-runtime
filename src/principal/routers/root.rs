//! Root-agent-based Principal router.
//!
//! The `RootRouter` runs a normal agent prompt (the built-in root
//! agent, or a user-supplied override) in a peer-scoped session. The
//! root agent does the actual orchestration: it inspects principal
//! memory/sessions, chooses specialist agents from the catalog, and
//! delegates via the existing `Agent` tool and async task tools.
//!
//! From the Principal boundary's point of view, the root agent simply
//! returns a `RouteDecision::Respond` containing the agent's final
//! answer.

use std::path::PathBuf;
use std::sync::{Arc, RwLock as StdRwLock};

use async_trait::async_trait;

use crate::engine::AgenticEvent;
use crate::principal::agent_prompt::{parse_agent_prompt, AgentPrompt};
use crate::principal::agent_runner::{run_root_agent_prompt, run_root_agent_prompt_streaming};
use crate::principal::context::PrincipalContext;
use crate::principal::memory::{PrincipalMemory, SessionArtifact};
use crate::principal::router::{
    AgentPromptSummary, PrincipalRouter, RouteDecision, RouterContext, RouterError,
};
// F19: removed `use crate::quota::QuotaMeter;` — the router no
// longer carries a quota meter field.
use crate::providers::LlmResolver;

/// Load the compiled-in root agent prompt.
pub fn default_root_prompt() -> AgentPrompt {
    let content = include_str!("../../resources/agents/root/AGENT.md");
    parse_agent_prompt("root", PathBuf::from("builtin:root"), content)
}

/// Stable root-agent session id for a peer.
#[must_use]
pub fn root_session_id(peer: &crate::auth::Subject) -> String {
    format!("root:{peer}")
}

/// A Principal router powered by a root-agent agentic loop.
///
/// Holds a cached `PrincipalContext` for the principal's lifetime; the
/// shared per-principal `ExtensionCore` lives on the context and is
/// reused across messages.
pub struct RootRouter {
    memory: Arc<dyn PrincipalMemory>,
    resolver: Option<Arc<LlmResolver>>,
    root_prompt: AgentPrompt,
    workspace_path: PathBuf,
    /// Per-Principal provider preference from `principal.toml`. When
    /// `Some`, it overrides the global catalog default for any LLM call
    /// routed through this Principal's root agent. When `None`, the
    /// catalog default wins.
    principal_provider_id: Option<String>,
    principal_model_id: Option<String>,
    /// Caller principal DID for outbound `principal_send` envelopes.
    /// Resolved at factory creation time from
    /// `PrincipalConfig::did` / `Principal::did().await`; copied into
    /// every `PrincipalContext` produced by `build_context`.
    principal_caller_did: Option<String>,
    /// Local runtime id (`did:key` form) for outbound
    /// `principal_send` envelopes. Set by the daemon-state bootstrap
    /// post-`start_tunnel` via [`Self::set_caller_runtime_id`].
    /// When `None`, `principal_send` is not registered.
    caller_runtime_id: StdRwLock<Option<String>>,
    // F19: removed `quota_meter` field. The engine loop fetches the
    // principal's meter directly from `Principal.quota_meter` at run
    // entrypoint and opens `QuotaScope::with` around the run. No need
    // to thread it through the router.
}

impl RootRouter {
    /// Create a new root router for the given Principal workspace.
    ///
    /// `principal_provider_id` / `principal_model_id` are the values from
    /// `PrincipalConfig::preferred_provider_id` / `preferred_model_id`.
    /// Pass `None, None` to inherit the global catalog default.
    ///
    /// `principal_caller_did` is the principal's stable DID used as
    /// `caller_principal_did` on the wire for `principal_send`.
    /// `caller_runtime_id` is set later via
    /// [`Self::set_caller_runtime_id`] (the bootstrap can't supply it
    /// until `start_tunnel` runs).
    #[must_use]
    pub fn new(
        memory: Arc<dyn PrincipalMemory>,
        resolver: Option<Arc<LlmResolver>>,
        root_prompt: AgentPrompt,
        workspace_path: PathBuf,
        principal_provider_id: Option<String>,
        principal_model_id: Option<String>,
        principal_caller_did: Option<String>,
    ) -> Self {
        Self {
            memory,
            resolver,
            root_prompt,
            workspace_path,
            principal_provider_id,
            principal_model_id,
            principal_caller_did,
            caller_runtime_id: StdRwLock::new(None),
        }
    }

    /// Bind the local runtime's `runtime_id` so `principal_send` can
    /// be registered on this Principal's agents. Called by the
    /// daemon-state bootstrap after `start_tunnel` succeeds; takes
    /// effect on the next `PrincipalContext` produced by
    /// `build_context`. (Existing contexts in flight won't see it
    /// until they re-build — same lazy semantics as
    /// `set_caller_principal_did`.)
    pub fn set_caller_runtime_id(&self, runtime_id: String) {
        if let Ok(mut guard) = self.caller_runtime_id.write() {
            *guard = Some(runtime_id);
        }
    }

    /// Read the local runtime id (if bound). Returns a clone so
    /// callers can use it outside the lock.
    #[must_use]
    pub fn caller_runtime_id(&self) -> Option<String> {
        self.caller_runtime_id.read().ok().and_then(|g| g.clone())
    }

    /// Build a `PrincipalContext` from the router's already-resolved
    /// state plus the per-call `RouterContext` (which carries the
    /// per-message pieces: inbox registry, session-creation lock, the
    /// current allowed extensions snapshot, and the principal's runtime id).
    fn build_context(&self, ctx: &RouterContext) -> PrincipalContext {
        let principal_ctx = PrincipalContext::new(
            self.workspace_path.clone(),
            Arc::clone(&self.memory),
            Arc::clone(&ctx.inbox_registry),
            Arc::clone(&ctx.session_creation_lock),
            Arc::new(ctx.capabilities.clone()),
            self.resolver.clone(),
            (
                self.principal_provider_id.clone(),
                self.principal_model_id.clone(),
            ),
            ctx.principal_id.clone(),
        );
        principal_ctx.set_root_prompt(self.root_prompt.clone());
        // Phase 4b: bind caller identity so `principal_send` is
        // registered on the principal's agents. The DID is the
        // principal's stable identifier (set in the factory from
        // `Principal::did()` / `config.did`); the runtime_id may be
        // set later by the daemon-state bootstrap post-`start_tunnel`.
        if let Some(ref did) = self.principal_caller_did {
            if let Err(e) = principal_ctx.set_caller_principal_did(did.clone()) {
                tracing::debug!("RootRouter::build_context: {e}");
            }
        }
        if let Some(ref runtime_id) = self.caller_runtime_id.read().ok().and_then(|g| g.clone()) {
            if let Err(e) = principal_ctx.set_caller_runtime_id((*runtime_id).clone()) {
                tracing::debug!("RootRouter::build_context: {e}");
            }
        }
        if let Err(e) = principal_ctx.set_active_extensions(ctx.active_extensions.clone()) {
            tracing::debug!("RootRouter::build_context: active_extensions already set");
            let _ = e;
        }
        if let Some(ref obs) = ctx.observability {
            if principal_ctx.set_observability(Arc::clone(obs)).is_err() {
                tracing::debug!("RootRouter::build_context: observability already set");
            }
        }
        principal_ctx
    }
}

#[async_trait]
impl PrincipalRouter for RootRouter {
    fn set_caller_runtime_id(&self, runtime_id: String) {
        RootRouter::set_caller_runtime_id(self, runtime_id);
    }
    async fn route(&self, ctx: RouterContext) -> Result<RouteDecision, RouterError> {
        let peer = ctx.peer.clone();
        let session_id = root_session_id(&peer);
        let available_agents: Vec<AgentPromptSummary> = ctx.available_agents.clone();
        let message = build_root_message(&ctx);
        let principal_ctx = self.build_context(&ctx);

        let response = run_root_agent_prompt(
            &self.root_prompt,
            peer.clone(),
            message,
            session_id.clone(),
            available_agents,
            &principal_ctx,
        )
        .await
        .map_err(|e| RouterError::AgentFailed(e.to_string()))?;

        // Record the root-agent session artifact so future messages
        // from this peer can recall it as prior context.
        let artifact = SessionArtifact {
            session_id,
            peer,
            title: Some("root".to_string()),
            updated_at: chrono::Utc::now(),
            summary: Some(response.clone()),
        };
        if let Err(e) = self.memory.record_session(artifact).await {
            tracing::warn!("failed to record root-agent session artifact: {e}");
        }

        Ok(RouteDecision::Respond { response })
    }

    async fn route_streaming(
        &self,
        ctx: RouterContext,
        on_event: Box<dyn Fn(AgenticEvent) + Send + Sync>,
        cancel: Option<tokio_util::sync::CancellationToken>,
    ) -> Result<RouteDecision, RouterError> {
        let peer = ctx.peer.clone();
        let session_id = root_session_id(&peer);
        let available_agents: Vec<AgentPromptSummary> = ctx.available_agents.clone();
        let message = build_root_message(&ctx);
        let principal_ctx = self.build_context(&ctx);

        let response = run_root_agent_prompt_streaming(
            &self.root_prompt,
            peer.clone(),
            message,
            session_id.clone(),
            available_agents,
            &principal_ctx,
            on_event,
            cancel,
        )
        .await
        .map_err(|e| RouterError::AgentFailed(e.to_string()))?;

        // Record the root-agent session artifact so future messages
        // from this peer can recall it as prior context.
        let artifact = SessionArtifact {
            session_id,
            peer,
            title: Some("root".to_string()),
            updated_at: chrono::Utc::now(),
            summary: Some(response.clone()),
        };
        if let Err(e) = self.memory.record_session(artifact).await {
            tracing::warn!("failed to record root-agent session artifact: {e}");
        }

        Ok(RouteDecision::Respond { response })
    }
}

fn build_root_message(ctx: &RouterContext) -> String {
    let mut parts = Vec::new();

    if !ctx.recalled_context.is_empty() {
        parts.push("Prior context:".to_string());
        for injection in &ctx.recalled_context {
            parts.push(format!(
                "- [{} {}]: {}",
                format!("{:?}", injection.kind).to_lowercase(),
                injection.id,
                injection.content
            ));
        }
    }

    parts.push(format!("User ({}) says:\n{}", ctx.peer, ctx.message));

    parts.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Subject;
    use crate::principal::config::{
        PrincipalGovernanceConfig, PrincipalIntentConfig, PrincipalRoutingConfig,
    };
    use crate::principal::router::{ChannelContext, ChannelKind, ContextInjectionKind};
    use crate::principal::{Capabilities, ExtensionCatalog};
    use crate::session::InboxRegistry;

    #[test]
    fn test_default_root_prompt_loads() {
        let prompt = default_root_prompt();
        assert_eq!(prompt.name, "root");
        assert!(
            prompt.body.contains("agent_catalog"),
            "root agent prompt should mention agent_catalog"
        );
    }

    #[test]
    fn test_build_root_message_includes_context() {
        let ctx = RouterContext {
            principal_id: crate::subject::PrincipalId::generate(),
            principal_name: "test".to_string(),
            peer: Subject::User("alice".to_string()),
            message: "hello".to_string(),
            channel: ChannelContext {
                kind: ChannelKind::Cli,
                streaming: false,
            },
            routing: PrincipalRoutingConfig::default(),
            recalled_context: vec![crate::principal::router::ContextInjection {
                kind: ContextInjectionKind::Session,
                id: "s1".to_string(),
                content: "previous summary".to_string(),
            }],
            available_agents: vec![AgentPromptSummary {
                id: "primary".to_string(),
                name: "primary".to_string(),
                description: Some("Generalist".to_string()),
                enabled: true,
            }],
            capabilities: Capabilities::default(),
            intent: PrincipalIntentConfig::default(),
            governance: PrincipalGovernanceConfig::default(),
            extension_store: ExtensionCatalog::default(),
            active_extensions: crate::principal::ActiveExtensionSet::empty(),
            inbox_registry: Arc::new(InboxRegistry::new()),
            session_creation_lock: Arc::new(tokio::sync::Mutex::new(())),
            observability: None,
        };

        let message = build_root_message(&ctx);
        assert!(message.contains("User (user:alice) says:"));
        assert!(message.contains("previous summary"));
    }
}
