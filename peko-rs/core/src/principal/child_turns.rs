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
use crate::principal::peer_dm::{ensure_peer_dm_channel, PeerDmSubscriberHook};
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

/// Result of [`PeerChildTurns::ensure_child_ingress`]: the peer's
/// standing-child session id plus, when a channel port is attached and
/// provisioning succeeded, the peer's DM channel (Phase 11 — the
/// conversation's channel-tier home; `None` keeps the pre-Phase-11
/// degrade posture: standalone/test contexts simply don't post).
#[derive(Debug, Clone)]
pub(crate) struct PeerChildIngress {
    pub child_id: String,
    pub dm_channel: Option<peko_channel::ChannelId>,
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
    /// The principal's own id — the creator (auto-member) of peer DM
    /// channels (Phase 10).
    principal_id: peko_subject::PrincipalId,
    /// Sprint 3 Phase 10: the daemon-global channel port DM channels
    /// are provisioned through. `None` in standalone/test contexts —
    /// provisioning is then skipped (debug log) and session behavior
    /// is unchanged.
    channel_port: Option<Arc<dyn peko_channel::ChannelPort>>,
    /// Phase 10: post-create kickoff hook (the daemon's
    /// `ChannelBindingSupervisor::ensure_subscriber`) so a freshly
    /// provisioned DM channel gets its subscriber without a restart.
    dm_subscriber_hook: Option<PeerDmSubscriberHook>,
    /// Phase 10: per-principal serialization for the DM channel
    /// find-or-create (the manager's `session_creation_lock` — shared
    /// across all `PeerChildTurns` instances built for this
    /// principal). `None` falls back to unsynchronized provisioning
    /// (tests only).
    dm_lock: Option<Arc<tokio::sync::Mutex<()>>>,
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
            principal_id: principal.id.clone(),
            channel_port: None,
            dm_subscriber_hook: None,
            dm_lock: None,
        })
    }

    /// Phase 10: attach the channel port peer DM channels are
    /// provisioned through (`None` keeps provisioning disabled — the
    /// standalone/test default).
    pub(crate) fn with_channel_port(
        mut self,
        port: Option<Arc<dyn peko_channel::ChannelPort>>,
    ) -> Self {
        self.channel_port = port;
        self
    }

    /// Phase 10: attach the post-create subscriber kickoff hook.
    pub(crate) fn with_dm_subscriber_hook(mut self, hook: Option<PeerDmSubscriberHook>) -> Self {
        self.dm_subscriber_hook = hook;
        self
    }

    /// Phase 10: attach the per-principal DM provisioning lock (the
    /// manager's `session_creation_lock`).
    pub(crate) fn with_dm_lock(mut self, lock: Option<Arc<tokio::sync::Mutex<()>>>) -> Self {
        self.dm_lock = lock;
        self
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

    /// Find-or-create the peer's standing child of the trunk and (Phase
    /// 11) surface the peer's DM channel alongside — the ingress
    /// handlers post the inbound message + reply to the returned
    /// channel (`principal::peer_dm::post_peer_dm_inbound` /
    /// `post_peer_dm_reply`). Idempotent per peer (Phase 5 semantics).
    ///
    /// Sprint 3 Phase 10: also find-or-creates the peer's DM channel
    /// (`dm-<slug>`, bound to the child's path) when a channel port is
    /// attached — EVERY external ingress path funnels through here, so
    /// no ingress can provision a child without its DM channel.
    /// Provisioning failures degrade to a warning (the ingress turn
    /// itself is unaffected); a missing port skips provisioning with a
    /// debug log (standalone/test contexts). `dm_channel` is `None` in
    /// both cases (callers skip the posts).
    pub(crate) async fn ensure_child_ingress(&self, peer: &Subject) -> Result<PeerChildIngress> {
        let child_id =
            ensure_peer_child(&self.agent_name, &self.owner, peer, &self.session_manager).await?;
        let Some(port) = self.channel_port.clone() else {
            tracing::debug!(
                peer = %peer,
                "peer DM provisioning skipped: no channel port attached"
            );
            return Ok(PeerChildIngress {
                child_id,
                dm_channel: None,
            });
        };
        // Tests may leave `dm_lock` unset; a call-local mutex gives no
        // cross-call serialization (documented on the field).
        let fallback_lock = tokio::sync::Mutex::new(());
        let lock = self.dm_lock.as_deref().unwrap_or(&fallback_lock);
        match ensure_peer_dm_channel(
            &port,
            &self.principal_id,
            peer,
            &child_id,
            &self.session_manager,
            lock,
        )
        .await
        {
            Ok(provision) => {
                if provision.created {
                    if let Some(hook) = &self.dm_subscriber_hook {
                        hook(self.principal_id.clone(), provision.channel.clone());
                    }
                }
                Ok(PeerChildIngress {
                    child_id,
                    dm_channel: Some(provision.channel),
                })
            }
            Err(e) => {
                tracing::warn!(
                    peer = %peer,
                    child = %child_id,
                    "peer DM channel provisioning failed (ingress unaffected): {e:#}"
                );
                Ok(PeerChildIngress {
                    child_id,
                    dm_channel: None,
                })
            }
        }
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
    use peko_channel::ChannelPort;

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

    // ─── Phase 10: DM channel provisioning on ensure_child_ingress ───

    /// Build a minimal `PeerChildTurns` directly (struct literal —
    /// `build` needs a full `Principal` + resolver, none of which the
    /// provisioning path touches). The executor is never driven here.
    fn bare_turns(
        session_manager: Arc<RwLock<SessionManager>>,
        channel_port: Option<Arc<dyn peko_channel::ChannelPort>>,
        hook: Option<PeerDmSubscriberHook>,
        dm_lock: Option<Arc<tokio::sync::Mutex<()>>>,
    ) -> PeerChildTurns {
        let principal_id = peko_subject::PrincipalId("prin_self".to_string());
        let executor = SubagentExecutor::new(
            Arc::clone(&session_manager),
            "root",
            5,
            principal_id.clone(),
        );
        PeerChildTurns {
            executor,
            session_manager,
            parent_session_key: trunk_session_id(),
            owner: Subject::User("local".to_string()),
            agent_name: "root".to_string(),
            principal_id,
            channel_port,
            dm_subscriber_hook: hook,
            dm_lock,
        }
    }

    async fn dm_fixture() -> (
        tempfile::TempDir,
        Arc<RwLock<SessionManager>>,
        Arc<peko_channel::ChannelStore>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let manager = SessionManager::new().with_sessions_dir_internal(dir.path().join("sessions"));
        let store = Arc::new(peko_channel::ChannelStore::new(
            peko_channel::ChannelConfig {
                runtime_dir: dir.path().join("runtime"),
                shared_dir: None,
            },
        ));
        (dir, Arc::new(RwLock::new(manager)), store)
    }

    #[tokio::test]
    async fn ensure_child_provisions_dm_channel_and_fires_hook_once() {
        let (_dir, manager, store) = dm_fixture().await;
        let port: Arc<dyn peko_channel::ChannelPort> = store.clone();
        let hook_calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let hook_calls2 = Arc::clone(&hook_calls);
        let hook: PeerDmSubscriberHook = Arc::new(move |principal, channel| {
            hook_calls2.lock().unwrap().push((principal, channel));
        });
        let turns = bare_turns(
            manager,
            Some(port),
            Some(hook),
            Some(Arc::new(tokio::sync::Mutex::new(()))),
        );

        let peer = Subject::User("alice".to_string());
        let child_id = turns.ensure_child_ingress(&peer).await.unwrap().child_id;

        // The DM channel exists, named + bound to the peer child.
        let channels = store.list_for_principal(&turns.principal_id).await.unwrap();
        assert_eq!(channels.len(), 1);
        assert_eq!(
            store
                .passive_binding(&channels[0])
                .await
                .unwrap()
                .as_deref(),
            Some("/user-alice")
        );
        assert_eq!(
            store.membership(&channels[0]).await.unwrap().name,
            "dm-user-alice"
        );

        // The kickoff hook fired exactly once with (principal, channel).
        {
            let calls = hook_calls.lock().unwrap();
            assert_eq!(calls.len(), 1, "hook fires only on fresh create");
            assert_eq!(calls[0].0, turns.principal_id);
            assert_eq!(calls[0].1, channels[0]);
        }

        // Second ensure: same child, no new channel, no second hook.
        let again = turns.ensure_child_ingress(&peer).await.unwrap().child_id;
        assert_eq!(again, child_id);
        assert_eq!(
            store
                .list_for_principal(&turns.principal_id)
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(hook_calls.lock().unwrap().len(), 1);
    }

    /// Without a channel port (standalone/test contexts) provisioning
    /// is skipped — session behavior is unchanged.
    #[tokio::test]
    async fn ensure_child_without_port_skips_provisioning() {
        let (_dir, manager, _store) = dm_fixture().await;
        let turns = bare_turns(manager, None, None, None);
        let peer = Subject::User("alice".to_string());
        turns.ensure_child_ingress(&peer).await.unwrap();
        // Nothing to assert on the channel side except "did not
        // panic"; the child still exists.
        let metas = turns
            .session_manager
            .write()
            .await
            .list_all_sessions(false)
            .await
            .unwrap();
        assert_eq!(metas.len(), 1);
        assert_eq!(metas[0].slug.as_deref(), Some("user-alice"));
    }

    /// Concurrent first-contact ensures for the same peer converge on
    /// exactly one DM channel + one hook firing (the per-principal
    /// `dm_lock` serializes find-or-create).
    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_ensure_child_provisions_one_channel() {
        let (_dir, manager, store) = dm_fixture().await;
        let port: Arc<dyn peko_channel::ChannelPort> = store.clone();
        let hook_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let hook_count2 = Arc::clone(&hook_count);
        let hook: PeerDmSubscriberHook = Arc::new(move |_p, _c| {
            hook_count2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        let turns = Arc::new(bare_turns(
            manager,
            Some(port),
            Some(hook),
            Some(Arc::new(tokio::sync::Mutex::new(()))),
        ));

        let peer = Subject::User("alice".to_string());
        let mut handles = Vec::new();
        for _ in 0..6 {
            let turns = Arc::clone(&turns);
            let peer = peer.clone();
            handles.push(tokio::spawn(async move { turns.ensure_child_ingress(&peer).await }));
        }
        for h in handles {
            h.await.unwrap().unwrap();
        }
        assert_eq!(
            store
                .list_for_principal(&turns.principal_id)
                .await
                .unwrap()
                .len(),
            1,
            "concurrent first-contact must create exactly one DM channel"
        );
        assert_eq!(hook_count.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    // ─── Phase 11: ensure_child_ingress surfaces the DM channel ──────

    /// `ensure_child_ingress` returns both the child session id and
    /// the provisioned DM channel; a post pair through the Phase 11
    /// helpers lands with the peer / principal authors.
    #[tokio::test]
    async fn ensure_child_ingress_returns_child_and_dm_channel() {
        use crate::principal::peer_dm::{post_peer_dm_inbound, post_peer_dm_reply};

        let (_dir, manager, store) = dm_fixture().await;
        let port: Arc<dyn peko_channel::ChannelPort> = store.clone();
        let turns = bare_turns(
            manager,
            Some(port.clone()),
            None,
            Some(Arc::new(tokio::sync::Mutex::new(()))),
        );

        let peer = Subject::User("alice".to_string());
        let ingress = turns.ensure_child_ingress(&peer).await.unwrap();
        let dm = ingress.dm_channel.expect("port attached ⇒ DM channel");

        post_peer_dm_inbound(
            &port,
            &turns.principal_id,
            &dm,
            &peer.to_string(),
            "hello",
        )
        .await
        .unwrap();
        post_peer_dm_reply(&port, &turns.principal_id, &dm, "hi alice").await;

        let events = store
            .peek(&dm, &peko_channel::Checkpoint::default())
            .await
            .unwrap();
        let posted: Vec<(String, String)> = events
            .iter()
            .filter_map(|ev| match ev {
                peko_channel::ChannelEvent::Posted { author, text, .. } => {
                    Some((author.clone(), text.clone()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            posted,
            vec![
                ("user:alice".to_string(), "hello".to_string()),
                ("prin_self".to_string(), "hi alice".to_string()),
            ]
        );

        // Idempotent: a second ingress returns the same pair.
        let again = turns.ensure_child_ingress(&peer).await.unwrap();
        assert_eq!(again.child_id, ingress.child_id);
        assert_eq!(again.dm_channel, Some(dm));
    }

    /// Without a channel port, `ensure_child_ingress` reports
    /// `dm_channel: None` (the accepted Phase 11 degrade: the
    /// conversation is not projected anywhere).
    #[tokio::test]
    async fn ensure_child_ingress_without_port_reports_no_channel() {
        let (_dir, manager, _store) = dm_fixture().await;
        let turns = bare_turns(manager, None, None, None);
        let peer = Subject::User("alice".to_string());
        let ingress = turns.ensure_child_ingress(&peer).await.unwrap();
        assert!(ingress.dm_channel.is_none());
        assert!(!ingress.child_id.is_empty());
    }
}
