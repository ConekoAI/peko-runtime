//! Peer-child turn driving (agent-session paradigm, sprint 2 Phase 6).
//!
//! Sprint 2 re-routes all external ingress (peko send CLI, tunnel A2A,
//! Hub webchat) into per-peer standing children of the trunk (Phase 5
//! provisioning: [`crate::principal::peer_children`]). This module owns
//! the per-principal construction those ingress paths share in Phase 7:
//!
//! - [`PeerChildTurns::build`] is the ONE builder for peer-child turn
//!   executors. The channel passive-binding driver
//!   (`daemon::channel_binding::ChannelBindingSupervisor::driver_for`)
//!   and the Phase 7 streaming ingress paths both go through it, so
//!   provider resolution, session-manager wiring, registry key, quota
//!   attribution, and — critically — **persona inheritance** cannot
//!   drift between the two.
//! - [`PeerChildTurns::drive_turn_streaming`] drives one streaming turn
//!   in a peer's standing child via
//!   `SubagentExecutor::resume_streaming`, emitting the same
//!   `AgenticEvent` stream shape the IPC `principal_send` drain loop
//!   consumes.
//!
//! ## Persona inheritance
//!
//! Peer children run the principal's persona. The builder resolves the
//! principal's root agent prompt exactly as the router factory does
//! ([`DefaultPrincipalRouterFactory::resolve_root_agent_prompt`]) and
//! carries it on the executor's `AgentConfig` snapshot — closing the
//! pre-Phase-6 gap where the channel driver's executor had no
//! `.with_agent_config` and child turns fell back to a blank default
//! prompt.
//!
//! ## Registry key (cross-guard)
//!
//! The executor's agent name keys the GLOBAL async task registry
//! (`get_or_create_registry_for_agent`). The builder uses the default
//! root prompt's name — the same key the root agent's own executor
//! uses — so `has_active_subagent_run_for_child` sees Agent-tool runs,
//! channel-driven turns, and streaming ingress turns on the same child
//! and refuses the second (no double-run of one session JSONL).

use std::sync::Arc;

use anyhow::Result;
use tokio::sync::RwLock;

use peko_auth::Subject;
use peko_observability::Observability;
use peko_session::manager::SessionManager;

use crate::agents::agent_config::AgentConfig;
use crate::agents::subagent_executor::{
    AgenticEventSink, ExecutionConfig, StreamingResumeOutcome, SubagentExecutor,
};
use crate::principal::agent_runner::build_agent_config;
use crate::principal::config::PrincipalConfig;
use crate::principal::factory::DefaultPrincipalRouterFactory;
use crate::principal::peer_children::ensure_peer_child;
use crate::principal::routers::root::{default_root_prompt, trunk_session_id};
use crate::principal::Principal;

/// Resolve the principal's root agent prompt and build the
/// `AgentConfig` snapshot peer-child turns inherit (the persona).
///
/// Resolution order is the router factory's: `principal.toml`
/// `routing.root_prompt` file → `<workspace>/agents/root.md` (or
/// `agents/root/AGENT.md`) → compiled-in default. Factored out of
/// [`PeerChildTurns::build`] so the persona mapping is unit-testable
/// without a full `Principal`.
pub(crate) fn peer_child_agent_config(
    config: &PrincipalConfig,
    workspace_path: &std::path::Path,
) -> AgentConfig {
    let agents_dir = workspace_path.join("agents");
    let prompt = DefaultPrincipalRouterFactory::resolve_root_agent_prompt(config, &agents_dir);
    build_agent_config(
        &prompt,
        &config.capabilities,
        &[],
        config.preferred_model_id.clone(),
    )
}

/// Per-principal peer-child turn bundle: the shared
/// [`SubagentExecutor`] (persona-carrying), the session manager the
/// peer children live in, and the ownership anchor for the resume
/// guard stack.
///
/// Cheap to share: every field is an `Arc` or an owned `String`.
pub(crate) struct PeerChildTurns {
    executor: SubagentExecutor,
    session_manager: Arc<RwLock<SessionManager>>,
    /// The caller session key the resume guards anchor at: the
    /// principal's trunk session (`root:self`). The trunk is a
    /// base-session caller, so the ownership guards admit any session
    /// in the principal's tree (and while the trunk is still dangling —
    /// no self-turn has run — the subtree check still admits its own
    /// children, exactly where peer children live).
    parent_session_key: String,
    /// The principal's configured owner subject — decides the
    /// `privileged` flag in [`ensure_peer_child`] (the owner's child
    /// `/local-user` gets whole-store reach).
    owner: Subject,
    /// The root agent's prompt name, stamped as the agent name on
    /// provisioned peer-child sessions.
    agent_name: String,
}

impl PeerChildTurns {
    /// Build the bundle for one principal. Fails when the principal
    /// has no resolvable model — the same configuration error that
    /// breaks `peko send` (callers degrade to their no-driver
    /// behavior: bound channels stay active-only, ingress errors out).
    ///
    /// `inbox_registry` is the daemon-shared `InboxRegistry` (Phase 7):
    /// bound onto the executor so child runs drain steering queued by
    /// the `PrincipalManager` / IPC serial-queue fallback (keyed by the
    /// child session id). `None` keeps the per-call standalone drain
    /// (tests and non-daemon contexts).
    pub(crate) async fn build(
        principal: &Arc<Principal>,
        llm_resolver: &peko_providers::LlmResolver,
        observability: Arc<Observability>,
        inbox_registry: Option<Arc<peko_session::InboxRegistry>>,
    ) -> Result<Self> {
        let (name, owner, capabilities, agent_config, preferred_model_id) = {
            let config = principal.config.read().await;
            (
                config.name.clone(),
                config.owner.clone(),
                config.capabilities.clone(),
                // Persona inheritance — see module docs.
                peer_child_agent_config(&config, &principal.workspace_path),
                config.preferred_model_id.clone(),
            )
        };
        let agent_name = agent_config.name.clone();

        // Resolve the principal's pinned model through the daemon's
        // shared resolver (`AgentPreference` precedence). A per-call
        // override (`peko send --model`) does NOT re-resolve here —
        // it rides `ExecutionConfig::model_override` at drive time
        // (`Provider::with_model_id` on a clone + pre-flight
        // `SpecGate::check`), so the cached bundle stays
        // override-neutral.
        let (provider, _choice) = llm_resolver
            .build(peko_providers::resolver::ResolveRequest {
                agent_model: preferred_model_id.as_deref(),
                ..Default::default()
            })
            .await?;

        // Session manager mirrors `agent_runner`'s root-agent
        // construction: same sessions dir, root agent prompt name,
        // owner as the session peer.
        let session_manager = Arc::new(RwLock::new(
            SessionManager::new()
                .with_sessions_dir_internal(principal.memory.sessions_dir())
                .with_agent_name(&agent_name)
                .with_peer_principal(owner.clone())
                .with_user(&owner.to_string()),
        ));

        // `SubagentExecutor::new`'s agent name keys the GLOBAL async
        // task registry — see "Registry key" in the module docs.
        //
        // `max_concurrent` is 64, not the subagent-default 5: this
        // executor drives PEER INGRESS turns, where distinct peers
        // legitimately run in parallel (per-child serialization is the
        // `InboxRegistry` run permit + the registry's
        // `has_active_subagent_run_for_child` cross-check). The shared
        // "root"-keyed registry counts the trunk's own subagent spawns
        // here too, so the cap is a runaway guard, not a throttle.
        let registry_key = default_root_prompt().name;
        let executor = SubagentExecutor::new(
            Arc::clone(&session_manager),
            registry_key,
            64,
            principal.id.clone(),
        )
        .with_principal_name(name)
        .with_principal_workspace(principal.workspace_path.clone())
        .with_principal_capabilities(Some(Arc::new(capabilities)))
        .with_principal_plan_port(Arc::clone(&principal.plan_port))
        .with_observability(Some(observability))
        // Phase 7: child runs drain the daemon-shared registry so the
        // ingress serial-queue fallback's steering pushes reach the
        // live turn at its next iteration boundary.
        .with_inbox_registry(inbox_registry)
        // Charge peer-child turns against the principal's meter, like
        // any other subagent run (F39).
        .with_quota_meter(Some(Arc::clone(&principal.quota_meter)))
        .with_provider(provider)
        // Persona inheritance — see module docs.
        .with_agent_config(agent_config);
        executor.set_caller_principal_did(principal.did().await.0);

        Ok(Self {
            executor,
            session_manager,
            parent_session_key: trunk_session_id(),
            owner,
            agent_name,
        })
    }

    /// The shared executor (channel driver wraps this in its
    /// final-only `SubagentResumeDriver`).
    pub(crate) fn executor(&self) -> &SubagentExecutor {
        &self.executor
    }

    /// The session manager the principal's peer children live in.
    pub(crate) fn session_manager(&self) -> &Arc<RwLock<SessionManager>> {
        &self.session_manager
    }

    /// The ownership anchor for the resume guard stack (the trunk,
    /// `root:self`).
    pub(crate) fn parent_session_key(&self) -> &str {
        &self.parent_session_key
    }

    /// Find-or-create the peer's standing child of the trunk; returns
    /// the child session id. Idempotent per peer (Phase 5 semantics).
    pub(crate) async fn ensure_child(&self, peer: &Subject) -> Result<String> {
        ensure_peer_child(&self.agent_name, &self.owner, peer, &self.session_manager).await
    }

    /// Drive one streaming turn in the peer's standing child session.
    ///
    /// Blocks until the run reaches a terminal state and returns the
    /// final text + token usage. Every `AgenticEvent` flows to
    /// `on_event` in the IPC drain-loop shape; `cancel` is observed by
    /// the child loop at iteration boundaries. The full resume guard
    /// stack applies (spawn-trigger / subtree / archived / active-run
    /// cross-check / depth / cost pre-flight) — a concurrent turn on
    /// the same child from ANY path sharing the registry key is
    /// refused with `err_run_active`.
    ///
    /// `override_model` is the per-message configured-model override
    /// (`peko send --model`): threaded into `ExecutionConfig`’s
    /// `model_override` so the run's pre-flight `SpecGate` check and
    /// provider override behave exactly like a parent-picked subagent
    /// model. `None` keeps the principal's pinned model.
    ///
    /// No completion announcement and no cleanup: the reply goes to
    /// the caller, and standing children outlive their runs. The turn
    /// has no wall-clock timeout (`timeout_seconds: 0`) — parity with
    /// the retired root-agent ingress path; cancellation is via
    /// `cancel` (the IPC `PrincipalSendControl::Interrupt` token).
    pub(crate) async fn drive_turn_streaming(
        &self,
        session_id: &str,
        message: &str,
        on_event: AgenticEventSink,
        cancel: Option<tokio_util::sync::CancellationToken>,
        override_model: Option<String>,
    ) -> Result<StreamingResumeOutcome> {
        let config = ExecutionConfig {
            announce_completion: false,
            timeout_seconds: 0,
            model_override: override_model,
            ..ExecutionConfig::default()
        };
        self.executor
            .resume_streaming(
                message,
                session_id,
                &self.parent_session_key,
                config,
                on_event,
                cancel,
            )
            .await
    }

    /// Blocking variant of [`Self::drive_turn_streaming`] for the
    /// one-shot `PrincipalManager::receive` path and steering
    /// successor runs: events are dropped, no cancel token.
    pub(crate) async fn drive_turn(
        &self,
        session_id: &str,
        message: &str,
        override_model: Option<String>,
    ) -> Result<StreamingResumeOutcome> {
        self.drive_turn_streaming(session_id, message, Arc::new(|_| {}), None, override_model)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::principal::config::{
        PrincipalGovernanceConfig, PrincipalIdentityConfig, PrincipalIntentConfig,
        PrincipalMemoryConfig, PrincipalRoutingConfig,
    };

    /// Minimal principal config for persona tests — everything
    /// defaulted except `name` and `routing`.
    fn test_config(name: &str, routing: PrincipalRoutingConfig) -> PrincipalConfig {
        PrincipalConfig {
            name: name.to_string(),
            did: None,
            owner: Subject::User("test-owner".to_string()),
            identity: PrincipalIdentityConfig::default(),
            intent: PrincipalIntentConfig::default(),
            governance: PrincipalGovernanceConfig::default(),
            memory: PrincipalMemoryConfig::default(),
            routing,
            capabilities: peko_extension_api::Capabilities::starter_bundle(),
            exposure: peko_auth::Exposure::Private,
            status: None,
            permissions: vec![],
            preferred_model_id: Some("mock".to_string()),
            transport_preference: Default::default(),
            quota: None,
            children: Default::default(),
        }
    }

    /// Persona: a workspace `agents/root.md` prompt body lands on the
    /// child agent config's `prompt`.
    #[test]
    fn persona_comes_from_workspace_root_prompt() {
        let tmp = tempfile::tempdir().unwrap();
        let agents_dir = tmp.path().join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(
            agents_dir.join("root.md"),
            "---\ndescription: \"Persona fixture\"\n---\n\nYou are PERSONA_MARKER, the principal's voice.\n",
        )
        .unwrap();

        let config = test_config("persona-test", PrincipalRoutingConfig::default());
        let agent_config = peer_child_agent_config(&config, tmp.path());
        assert_eq!(agent_config.name, "root");
        let prompt = agent_config.prompt.expect("persona prompt must be set");
        assert!(
            prompt.contains("PERSONA_MARKER"),
            "child agent config must carry the principal's root prompt body; got: {prompt}"
        );
    }

    /// Persona fallback: with no workspace file and no `root_prompt`
    /// override, the compiled-in default root prompt is inherited.
    #[test]
    fn persona_falls_back_to_compiled_default() {
        let tmp = tempfile::tempdir().unwrap();
        let config = test_config("persona-test", PrincipalRoutingConfig::default());
        let agent_config = peer_child_agent_config(&config, tmp.path());
        let prompt = agent_config.prompt.expect("persona prompt must be set");
        assert_eq!(
            prompt,
            default_root_prompt().body,
            "no workspace file ⇒ compiled-in default root prompt"
        );
    }

    /// Persona: an explicit `routing.root_prompt` file wins over both
    /// the workspace file and the compiled-in default.
    #[test]
    fn persona_explicit_override_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let override_path = tmp.path().join("custom.md");
        std::fs::write(
            &override_path,
            "---\ndescription: \"Override\"\n---\n\nOVERRIDE_MARKER persona.\n",
        )
        .unwrap();
        let agents_dir = tmp.path().join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(agents_dir.join("root.md"), "workspace persona\n").unwrap();

        let config = test_config(
            "persona-test",
            PrincipalRoutingConfig {
                root_prompt: Some(override_path),
                ..Default::default()
            },
        );
        let agent_config = peer_child_agent_config(&config, tmp.path());
        let prompt = agent_config.prompt.expect("persona prompt must be set");
        assert!(
            prompt.contains("OVERRIDE_MARKER"),
            "explicit root_prompt override must win; got: {prompt}"
        );
    }
}
