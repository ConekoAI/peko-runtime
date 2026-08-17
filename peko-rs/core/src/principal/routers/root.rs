//! Root-agent-based Principal router — TRUNK-ONLY since Phase 7.
//!
//! The `RootRouter` runs a normal agent prompt (the built-in root
//! agent, or a user-supplied override) in the principal's trunk
//! session `root:self`. The root agent does the actual orchestration:
//! it inspects principal memory/sessions, chooses specialist agents
//! from the catalog, and delegates via the existing `Agent` tool and
//! async task tools.
//!
//! From the Principal boundary's point of view, the root agent simply
//! returns a `RouteDecision::Respond` containing the agent's final
//! answer.
//!
//! Phase 7 (sprint 2, 2026-08-17): the per-peer root sessions
//! (`root:{peer}` / `root:cron:{peer}`) are RETIRED. Peer ingress
//! (`Cli`/`Http`/`Hub`/`A2a`/`P2p`/`Webhook`/`FileWatch`) never reaches
//! this router — `PrincipalManager::receive`/`receive_streaming` and
//! the IPC `principal_send` handler route it into per-peer standing
//! children of the trunk (`principal::peer_children` provisioning +
//! `principal::child_turns` turn driving). `Cron` traffic maps onto
//! the trunk (the cron `Send` default target IS the trunk). A peer
//! channel reaching [`RootRouter::route`]/[`RootRouter::route_streaming`]
//! is a routing bug and fails loudly.

use std::path::PathBuf;
use std::sync::{Arc, RwLock as StdRwLock};

use async_trait::async_trait;

use crate::principal::agent_prompt::{parse_agent_prompt, AgentPrompt};
use crate::principal::agent_runner::{run_root_agent_prompt, run_root_agent_prompt_streaming};
use crate::principal::context::PrincipalContext;
use crate::principal::memory::PrincipalMemory;
use crate::principal::router::{
    recalled_context_messages, AgentPromptSummary, PrincipalRouter, RouteDecision, RouterContext,
    RouterError,
};
use peko_engine::AgenticEvent;
// F19: removed `use peko_quota::QuotaMeter;` — the router no
// longer carries a quota meter field.
use peko_providers::LlmResolver;

/// Load the compiled-in root agent prompt.
pub fn default_root_prompt() -> AgentPrompt {
    let content = include_str!("../../resources/agents/root/AGENT.md");
    parse_agent_prompt("root", PathBuf::from("builtin:root"), content)
}

/// The principal's trunk session id: `"root:self"` (Phase 3,
/// 2026-08-15).
///
/// Paradigm role: the trunk is the principal's forever-continuous SELF
/// session — the `/` of its session tree. Cron `Send` jobs fire turns
/// into it (the default target since Phase 7), keeping the principal
/// an active actor (supervising children, organizing memory) rather
/// than a passive request handler. The trunk is keyed by NO peer:
/// every self-turn, from whatever automation source, continues the
/// same session. The per-peer `root:{peer}` / `root:cron:{peer}`
/// sessions are retired (Phase 7): peer ingress lives in per-peer
/// standing children of the trunk.
///
/// The literal deliberately keeps the `root:` prefix so the trunk
/// stays under the root-family guard (`session/session_runtime_impl.rs`
/// refuses delete/archive/move on `root:self`) and never misparses in
/// the messenger's `peer_from_session_key`.
#[must_use]
pub fn trunk_session_id() -> String {
    "root:self".to_string()
}

/// The session id the trunk router runs a turn in, adjusted for the
/// channel.
///
/// Phase 7: the `RootRouter` is trunk-only. `Trunk` and `Cron` both
/// map to the trunk (`root:self`) — cron `Send`'s default target IS
/// the trunk. Peer channels are unreachable here (peer ingress is
/// routed into per-peer standing children by `PrincipalManager`
/// before any router call); reaching this function with one is a
/// routing bug and panics loudly rather than silently misrouting.
#[must_use]
pub fn root_session_id_for_channel(kind: &crate::principal::router::ChannelKind) -> String {
    match kind {
        crate::principal::router::ChannelKind::Cron
        | crate::principal::router::ChannelKind::Trunk => trunk_session_id(),
        other => panic!(
            "peer channel {other:?} reached the retired per-peer root routing — \
             Phase 7 routes peer ingress into per-peer standing children of the trunk \
             (PrincipalManager::receive* / the IPC principal_send handler)"
        ),
    }
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
    /// Per-Principal configured model preference from `principal.toml`.
    /// When `Some`, it is used for any LLM call routed through this
    /// Principal's root agent. When `None`, the resolver will error
    /// unless a per-message override is supplied.
    principal_model_id: Option<String>,
    /// Caller principal DID for outbound `principal_send` envelopes.
    /// Resolved at factory creation time from
    /// `PrincipalConfig::did` / `Principal::did().await`; copied into
    /// every `PrincipalContext` produced by `build_context`.
    principal_caller_did: Option<String>,
    /// Local runtime id (`did:key` form) for outbound
    /// `principal_send` envelopes. Set by the daemon-state bootstrap
    /// post-`start_tunnel` via [`Self::set_caller_runtime_id`].
    /// When `None`, `send_peer` is not registered.
    caller_runtime_id: StdRwLock<Option<String>>,
    /// Per-Principal plan DAG port (PR #2 wiring). Copied into every
    /// `PrincipalContext` produced by `build_context` so the seven
    /// `Plan*` tools can be wired into the principal's agents.
    plan_port: Arc<dyn peko_plan::PlanPort>,
    // F19: removed `quota_meter` field. The engine loop fetches the
    // principal's meter directly from `Principal.quota_meter` at run
    // entrypoint and opens `QuotaScope::with` around the run. No need
    // to thread it through the router.
}

impl RootRouter {
    /// Create a new root router for the given Principal workspace.
    ///
    /// `principal_model_id` is the value from
    /// `PrincipalConfig::preferred_model_id`. Pass `None` only when the
    /// caller intends to supply a per-message override; otherwise
    /// resolution will fail with "no model configured".
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
        principal_model_id: Option<String>,
        principal_caller_did: Option<String>,
        plan_port: Arc<dyn peko_plan::PlanPort>,
    ) -> Self {
        Self {
            memory,
            resolver,
            root_prompt,
            workspace_path,
            principal_model_id,
            principal_caller_did,
            caller_runtime_id: StdRwLock::new(None),
            plan_port,
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
            self.principal_model_id.clone(),
            // Per-message configured model override from `RouterContext`
            // so the resolver classifies the resolution as
            // `ResolveSource::ExplicitOverride` when `peko send --model`
            // is used.
            ctx.override_model.clone(),
            ctx.principal_id.clone(),
            // PR #2 wiring: per-Principal plan DAG port.
            Arc::clone(&self.plan_port),
        );
        principal_ctx.set_root_prompt(self.root_prompt.clone());
        // Phase 4b: bind caller identity so `send_peer` is
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
        // Bug A (2026-08-01 v2): bind the principal's quota meter so
        // `agent_runner` can charge the per-cycle counter on every
        // LLM call. `QuotaMeter::unlimited()` is what `Principal`
        // holds when no quota is configured, so passing through
        // unconditionally is safe (no quota ⇒ unlimited ⇒ no charge).
        if let Some(ref meter) = ctx.quota_meter {
            if principal_ctx.set_quota_meter(Arc::clone(meter)).is_err() {
                tracing::debug!("RootRouter::build_context: quota_meter already set");
            }
        }
        // F20 (deferred): peer_meter — bound the same way when the
        // dispatcher populates `RouterContext::peer_meter`.
        if let Some(ref meter) = ctx.peer_meter {
            if principal_ctx.set_peer_meter(Arc::clone(meter)).is_err() {
                tracing::debug!("RootRouter::build_context: peer_meter already set");
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
        // Phase 7: trunk-only. Peer channels route into per-peer
        // standing children before any router call; reaching here with
        // one is a routing bug — fail loudly.
        if !matches!(
            ctx.channel.kind,
            crate::principal::router::ChannelKind::Trunk
                | crate::principal::router::ChannelKind::Cron
        ) {
            return Err(RouterError::AgentFailed(format!(
                "channel {:?} reached the trunk-only RootRouter — peer ingress must route \
                 through per-peer standing children (PrincipalManager::receive*)",
                ctx.channel.kind
            )));
        }
        let peer = ctx.peer.clone();
        let session_id = root_session_id_for_channel(&ctx.channel.kind);
        let available_agents: Vec<AgentPromptSummary> = ctx.available_agents.clone();
        let user_text = ctx.message.clone();
        let pre_user_messages = recalled_context_messages(&ctx.recalled_context);
        let principal_ctx = self.build_context(&ctx);

        let response = run_root_agent_prompt(
            &self.root_prompt,
            peer,
            user_text,
            pre_user_messages,
            session_id,
            available_agents,
            &principal_ctx,
        )
        .await
        // Use `{e:?}` (Debug) rather than `{e}` (Display) so the full
        // anyhow chain — including the `Caused by:` chain — reaches
        // the CLI. `Display` (`e.to_string()`) joins all contexts
        // with ": " but does NOT render the underlying source
        // separately, which collapses a chain like
        // `[root agent execution failed → No provider configured]`
        // down to a single segment when an empty `Display` is in the
        // middle. Debug keeps the multi-line structure intact.
        .map_err(|e| RouterError::AgentFailed(format!("{e:?}")))?;

        // Phase 7: no session-recall artifact write here. The retired
        // per-peer write (session id `root:{peer}`) is replaced by
        // `PrincipalManager::record_peer_recall` on the peer-child
        // paths; trunk continuity lives in the trunk JSONL itself.
        Ok(RouteDecision::Respond { response })
    }

    async fn route_streaming(
        &self,
        ctx: RouterContext,
        on_event: Box<dyn Fn(AgenticEvent) + Send + Sync>,
        cancel: Option<tokio_util::sync::CancellationToken>,
    ) -> Result<RouteDecision, RouterError> {
        // Phase 7: trunk-only — see `route` above.
        if !matches!(
            ctx.channel.kind,
            crate::principal::router::ChannelKind::Trunk
                | crate::principal::router::ChannelKind::Cron
        ) {
            return Err(RouterError::AgentFailed(format!(
                "channel {:?} reached the trunk-only RootRouter — peer ingress must route \
                 through per-peer standing children (PrincipalManager::receive*)",
                ctx.channel.kind
            )));
        }
        let peer = ctx.peer.clone();
        let session_id = root_session_id_for_channel(&ctx.channel.kind);
        let available_agents: Vec<AgentPromptSummary> = ctx.available_agents.clone();
        let user_text = ctx.message.clone();
        let pre_user_messages = recalled_context_messages(&ctx.recalled_context);
        let principal_ctx = self.build_context(&ctx);

        let response = run_root_agent_prompt_streaming(
            &self.root_prompt,
            peer,
            user_text,
            pre_user_messages,
            session_id,
            available_agents,
            &principal_ctx,
            on_event,
            cancel,
        )
        .await
        // Use `{e:?}` (Debug) rather than `{e}` (Display) so the full
        // anyhow chain — including the `Caused by:` chain — reaches
        // the CLI. See the matching note in `route()` above.
        .map_err(|e| RouterError::AgentFailed(format!("{e:?}")))?;

        // Phase 7: no session-recall artifact write — see `route`.
        Ok(RouteDecision::Respond { response })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn trunk_session_id_is_root_prefixed_constant() {
        // The `root:` prefix is load-bearing: it puts the trunk under
        // the root-family guard (the `root:self` refusal on
        // delete/archive/move in the session tool surface).
        assert_eq!(trunk_session_id(), "root:self");
        assert!(trunk_session_id().starts_with("root:"));
    }

    #[test]
    fn trunk_and_cron_channels_map_to_trunk_session() {
        use crate::principal::router::ChannelKind;
        // Phase 7: cron traffic maps onto the trunk — the per-peer
        // `root:cron:{owner}` session is retired.
        assert_eq!(
            root_session_id_for_channel(&ChannelKind::Trunk),
            "root:self"
        );
        assert_eq!(root_session_id_for_channel(&ChannelKind::Cron), "root:self");
    }

    #[test]
    #[should_panic(expected = "retired per-peer root routing")]
    fn peer_channel_panics_loudly() {
        // Phase 7: peer ingress never reaches the RootRouter — a peer
        // channel here is a routing bug and must fail loudly.
        let _ = root_session_id_for_channel(&crate::principal::router::ChannelKind::Cli);
    }
}
