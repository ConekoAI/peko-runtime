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
//! Peer children run the principal's persona by default. The builder
//! resolves the principal's root agent prompt exactly as the router
//! factory does
//! ([`DefaultPrincipalRouterFactory::resolve_root_agent_prompt`]) and
//! carries it on the executor's `AgentConfig` snapshot — closing the
//! pre-Phase-6 gap where the channel driver's executor had no
//! `.with_agent_config` and child turns fell back to a blank default
//! prompt. ADR-052 D3 makes the persona an explicit policy: when
//! `routing.peer_agent` names a workspace role file
//! (`roles/<name>.md` / `roles/<name>/ROLE.md`), that role's
//! prompt body replaces the root persona for all peer-facing turns;
//! unresolvable roles warn and fall back to the root persona.
//!
//! ## Registry key (cross-guard)
//!
//! Every executor of a principal registers runs in that principal's task
//! registry (`ToolingRuntime::task_registry_for`), whatever its agent
//! name, so `has_active_subagent_run_for_child` sees Agent-tool runs,
//! channel-driven turns, and streaming ingress turns on the same child
//! and refuses the second (no double-run of one session JSONL).

use std::sync::Arc;

use anyhow::Result;
use tokio::sync::RwLock;

use peko_auth::ActorRef;
use peko_observability::Observability;
use peko_session::manager::SessionManager;

use crate::agents::agent_config::AgentConfig;
use crate::agents::subagent_executor::{
    AgenticEventSink, ExecutionConfig, StreamingResumeOutcome, SubagentExecutor,
};
use crate::principal::agent_runner::build_agent_config;
use crate::principal::config::PrincipalConfig;
use crate::principal::factory::DefaultPrincipalRouterFactory;
use crate::principal::peer_dm::{ensure_peer_dm_channel, post_peer_dm_reply, PeerDmSubscriberHook};
use crate::principal::routers::root::{default_root_prompt, trunk_session_id};
use crate::principal::Principal;

/// Resolve the agent prompt peer-child turns run and build the
/// `AgentConfig` snapshot carrying it.
///
/// Default is **persona inheritance**: resolution order is the router
/// factory's — `principal.toml` `routing.root_prompt` file →
/// `<workspace>/roles/root.md` (or `roles/root/ROLE.md`) →
/// compiled-in default. When `routing.peer_agent` names a workspace
/// role (ADR-052 D3), that role's prompt BODY replaces the root
/// prompt's body; any resolution failure logs a warning and falls
/// back to the root persona so a bad config value never breaks peer
/// ingress. Only the body swaps — the `AgentConfig` keeps the root
/// prompt's name, preserving the registry-key / session-stamp
/// invariants of [`PeerChildTurns::build`].
///
/// Factored out of [`PeerChildTurns::build`] so the persona mapping
/// is unit-testable without a full `Principal`.
pub(crate) fn peer_child_agent_config(
    config: &PrincipalConfig,
    workspace_path: &std::path::Path,
) -> AgentConfig {
    let roles_dir = workspace_path.join("roles");
    let mut prompt = DefaultPrincipalRouterFactory::resolve_root_agent_prompt(config, &roles_dir);
    // ADR-052 D3: opt-in T1 role for peer-facing turns.
    if let Some(role) = config.routing.peer_agent.as_deref() {
        match crate::principal::agent_prompt::resolve_agent_prompt(role, &roles_dir) {
            Ok(role_prompt) => prompt.body = role_prompt.body,
            Err(e) => tracing::warn!(
                "Failed to resolve routing.peer_agent role '{role}': {e:#}. \
                 Falling back to the root persona."
            ),
        }
    }
    build_agent_config(&prompt, config.preferred_model_id.clone())
}

/// The trust source of test bundles built from parts: everyone is the
/// owner.
#[cfg(test)]
struct TestOwnerTrust;

#[cfg(test)]
#[async_trait::async_trait]
impl crate::security::TrustSource for TestOwnerTrust {
    async fn trust_of(
        &self,
        _actor: &ActorRef,
        _keys: &(dyn peko_subject::ActorKeys + Sync),
    ) -> peko_tools_core::Trust {
        peko_tools_core::Trust::Owner
    }
    async fn owner(&self) -> ActorRef {
        ActorRef::User("local".into())
    }
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

/// The session manager a principal's peer children live in. Mirrors
/// `agent_runner`'s root-agent construction: same sessions dir, root
/// agent prompt name, owner as the session peer. Factored out of
/// [`PeerChildTurns::build`] so the Phase 12a cross-runtime DM mirror
/// bootstrap (`TunnelHost::dm_channel_mirror_bootstrap`) can do a
/// child-only ensure without building the full turn bundle (which
/// requires a resolvable model).
pub(crate) fn peer_child_session_manager(
    principal: &Principal,
    agent_name: &str,
    owner: &ActorRef,
) -> Arc<RwLock<SessionManager>> {
    Arc::new(RwLock::new(
        SessionManager::new()
            .with_sessions_dir_internal(principal.memory.sessions_dir())
            .with_agent_name(agent_name)
            .with_peer_principal(owner.clone())
            .with_user(&owner.to_string()),
    ))
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
    /// ADR-078: who each actor is to this principal. `None` only for
    /// test bundles built from parts.
    trust: Option<Arc<dyn crate::security::TrustSource>>,
    /// Activity record for `Idle` schedules: human-started turns mark
    /// the principal active when they start and end. `None` outside
    /// the daemon.
    idle_detector: Option<Arc<peko_cron::IdleDetector>>,
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
        tooling: Arc<crate::tools::runtime::ToolingRuntime>,
    ) -> Result<Self> {
        let (name, owner, agent_config, preferred_model_id) = {
            let config = principal.config.read().await;
            tooling
                .agent_runs()
                .for_principal(&principal.id)
                .set_limit(config.governance.max_running_agents);
            (
                config.name.clone(),
                config.owner.clone(),
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
        // construction (see `peer_child_session_manager`).
        let session_manager = peer_child_session_manager(principal, &agent_name, &owner);

        // `SubagentExecutor::new`'s agent name keys the GLOBAL async
        // task registry — see "Registry key" in the module docs.
        //
        // Peer ingress and recursive spawns share principal-wide admission.
        let registry_key = default_root_prompt().name;
        let async_inbox = inbox_registry
            .clone()
            .unwrap_or_else(crate::async_exec::executor::standalone_inbox_registry);
        let executor = SubagentExecutor::new(
            Arc::clone(&session_manager),
            registry_key,
            principal.id.clone(),
            Arc::clone(&tooling),
        )
        .with_principal_name(name)
        .with_principal_workspace(principal.workspace_path.clone())
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
        let trust: Arc<dyn crate::security::TrustSource> =
            Arc::new(crate::principal::trust::PrincipalTrust::new(principal));
        // ADR-078: stamps read conversation rosters through the channel
        // port. Without it every conversation stamped untrusted and acted
        // for the channel, so a group whose only human is the owner ran
        // below owner on every caller that skipped `with_channel_port`
        // (channel-binding wakes, completion wakes, cron origin turns).
        let executor = executor.with_stamps(Some(crate::security::Stamps::new(
            principal.id.clone(),
            Arc::clone(&trust),
            tooling.services().channel_port(),
        )));

        // Tool-bag parity with the root-agent path
        // (`PrincipalContext::core` + `agent_runner`): the peer-ingress
        // path builds no `PrincipalContext`, so without this the peer
        // turn ran with built-ins only — no `Skill` tool, no workspace
        // MCP tools, no `{{roles}}`/`{{skills}}` prompt
        // sections, and no `RoleCatalog` (the tool the root prompt
        // advertises). Both installs are idempotent; the tool-bag
        // install honors the core's once-gate.
        let core = crate::principal::context::ensure_principal_tool_bag(
            tooling,
            &principal.workspace_path,
            &principal.id,
        )
        .await;

        let caller_did = principal.did().await.0;
        crate::tools::installation::install_principal_services(
            &core,
            &principal.id,
            crate::tools::installation::PrincipalBindings {
                sessions_dir: Some(principal.memory.sessions_dir().clone()),
                plan: Some(Arc::clone(&principal.plan_port)),
                model_catalog: Some(Arc::clone(llm_resolver.catalog())),
                caller_did: Some(&caller_did),
            },
        )
        .await?;
        crate::tools::installation::install_async(&core, &principal.id, async_inbox).await?;

        Ok(Self {
            executor,
            session_manager,
            parent_session_key: trunk_session_id(),
            agent_name,
            principal_id: principal.id.clone(),
            channel_port: None,
            dm_subscriber_hook: None,
            dm_lock: None,
            idle_detector: None,
            trust: Some(trust),
        })
    }

    /// A bundle over an existing executor and session store, for tests
    /// that drive turns without a principal or provider.
    #[cfg(test)]
    pub(crate) fn from_parts(
        executor: SubagentExecutor,
        session_manager: Arc<RwLock<SessionManager>>,
        parent_session_key: String,
    ) -> Self {
        // Test bundles treat every actor as the owner: they exercise turn
        // plumbing, not the trust floor.
        let trust: Arc<dyn crate::security::TrustSource> = Arc::new(TestOwnerTrust);
        let principal_id = executor.principal_id().clone();
        let executor = executor.with_stamps(Some(crate::security::Stamps::new(
            principal_id.clone(),
            Arc::clone(&trust),
            None,
        )));
        Self {
            principal_id,
            executor,
            session_manager,
            parent_session_key,
            agent_name: "root".to_string(),
            channel_port: None,
            dm_subscriber_hook: None,
            dm_lock: None,
            idle_detector: None,
            trust: Some(trust),
        }
    }

    /// Phase 10: attach the channel port peer DM channels are
    /// provisioned through (`None` keeps provisioning disabled — the
    /// standalone/test default).
    pub(crate) fn with_channel_port(
        mut self,
        port: Option<Arc<dyn peko_channel::ChannelPort>>,
    ) -> Self {
        // ADR-078: stamps read conversation rosters through the port.
        // `None` keeps the port `build` already gave them.
        if let (Some(trust), Some(port)) = (&self.trust, &port) {
            self.executor = self.executor.with_stamps(Some(crate::security::Stamps::new(
                self.principal_id.clone(),
                Arc::clone(trust),
                Some(Arc::clone(port)),
            )));
        }
        self.channel_port = port;
        self
    }

    /// Whom `author` is, as a run driver, given the subject permissions
    /// were checked against (ADR-057 D2).
    pub(crate) async fn driver(
        &self,
        author: &ActorRef,
        authority: &ActorRef,
    ) -> crate::security::Driver {
        match self.executor.stamps() {
            Some(stamps) => stamps.driver(author, authority).await,
            None => crate::security::Driver {
                actor: peko_channel::ActorInfo::from_subject(author, None).address,
                name: None,
                trust: peko_tools_core::Trust::Untrusted,
            },
        }
    }

    /// The stamp a run root gives a turn in `session_id` (ADR-078 D1).
    /// With a driver, the run acts for it, capped at the session's
    /// ceiling; without one (a completion wake), the session continues
    /// its own work under its natural stamp.
    pub(crate) async fn root_stamp(
        &self,
        driver: Option<&crate::security::Driver>,
        session_id: &str,
        origin: peko_tools_core::Origin,
    ) -> peko_tools_core::ActingFor {
        let metas = self
            .session_manager
            .write()
            .await
            .list_all_sessions(false)
            .await
            .unwrap_or_default();
        let compartment =
            crate::security::compartment_of(&metas, peko_session::SessionId::from(session_id));
        let origin = match origin {
            peko_tools_core::Origin::Ingress { channel: None } => {
                peko_tools_core::Origin::Ingress {
                    channel: compartment.channel().map(str::to_string),
                }
            }
            other => other,
        };
        match (self.executor.stamps(), driver) {
            (Some(stamps), Some(driver)) => stamps.enter(driver, compartment, origin).await,
            (Some(stamps), None) => stamps.natural(compartment, origin).await,
            (None, Some(driver)) => peko_tools_core::ActingFor {
                actor: driver.actor.clone(),
                name: driver.name.clone(),
                trust: driver.trust,
                compartment,
                origin,
            },
            (None, None) => peko_tools_core::ActingFor::unattributed(),
        }
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

    /// Record human-started turns as principal activity
    /// (`peko_cron::idle`).
    pub(crate) fn with_idle_detector(
        mut self,
        detector: Option<Arc<peko_cron::IdleDetector>>,
    ) -> Self {
        self.idle_detector = detector;
        self
    }

    /// Mark the principal active when a turn was started by a person
    /// (ADR-078 `Origin::Ingress`); cron turns and completion wakes are
    /// not activity.
    async fn note_activity(&self, acting_for: &peko_tools_core::ActingFor) {
        if let (Some(detector), peko_tools_core::Origin::Ingress { .. }) =
            (&self.idle_detector, &acting_for.origin)
        {
            detector.record_activity(&self.principal_id.0).await;
        }
    }

    /// The shared executor (the channel driver reads its steering
    /// inbox and active-run check from it).
    pub(crate) fn executor(&self) -> &SubagentExecutor {
        &self.executor
    }

    /// The session manager the principal's peer children live in.
    pub(crate) fn session_manager(&self) -> &Arc<RwLock<SessionManager>> {
        &self.session_manager
    }

    /// The root agent's prompt name, stamped on created sessions
    /// (ADR-049 Phase 3: group child provisioning).
    pub(crate) fn agent_name(&self) -> &str {
        &self.agent_name
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
    pub(crate) async fn ensure_child_ingress(&self, peer: &ActorRef) -> Result<PeerChildIngress> {
        let port = self
            .channel_port
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Conversation ingress requires a channel store"))?;
        let known = port.actor_info(peer).await;
        let peer = if known.identity_key != known.address {
            known.subject()
        } else {
            let hub = peer.route().filter(|r| r.is_hub()).map(|r| r.id());
            port.register_actor(
                peer,
                hub,
                None,
                None,
                None,
                if hub.is_some() {
                    peko_channel::Registration::Registered
                } else {
                    peko_channel::Registration::LocalOnly
                },
            )
            .await?
            .subject()
        };
        let slug = crate::principal::peer_children::peer_child_slug(&peer)?;
        let initial_lock = self
            .dm_lock
            .clone()
            .unwrap_or_else(|| Arc::new(tokio::sync::Mutex::new(())));
        let initial_guard = initial_lock.lock().await;
        let me = port.actor_info(&ActorRef::from(&self.principal_id)).await;
        let other = port.actor_info(&peer).await;
        let expected = peko_channel::dm_channel_id(&me.identity_key, &other.identity_key);
        let created = !port
            .list_for_principal(&self.principal_id)
            .await?
            .contains(&expected);
        let channel = port
            .ensure_dm(&self.principal_id, &peer, &format!("/{slug}"))
            .await?;
        drop(initial_guard);
        let child_id = crate::principal::peer_children::ensure_channel_child(
            &self.agent_name,
            channel.as_str(),
            &slug,
            &peer.to_string(),
            &self.session_manager,
        )
        .await?;

        // Tests may leave `dm_lock` unset; a call-local mutex gives no
        // cross-call serialization (documented on the field).
        let fallback_lock = tokio::sync::Mutex::new(());
        let lock = self.dm_lock.as_deref().unwrap_or(&fallback_lock);
        match ensure_peer_dm_channel(
            &port,
            &self.principal_id,
            &peer,
            &child_id,
            &self.session_manager,
            lock,
        )
        .await
        {
            Ok(provision) => {
                if created || provision.created {
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

    /// The conversation channel a peer turn's session is bound to
    /// (ADR-073: the binding is solely `channel_id`). `None` for an
    /// unknown or unbound session.
    async fn conversation_channel(&self, session_id: &str) -> Option<String> {
        let metas = self
            .session_manager
            .write()
            .await
            .list_all_sessions(false)
            .await
            .unwrap_or_default();
        metas
            .iter()
            .find(|m| m.session_id.to_string() == session_id)
            .and_then(|meta| meta.channel_id.clone())
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
    /// `cancel` (the IPC `PrincipalStop` token).
    ///
    /// `acting_for` is the run root's stamp (ADR-078): the ingress
    /// driver, the cron job's creator, or the session's own natural
    /// stamp for a completion wake. The executor re-caps it against the
    /// session before the run starts.
    ///
    /// A turn a person started counts as principal activity at its start
    /// and its end (`peko_cron::idle`), so an `Idle` schedule never fires
    /// mid-conversation.
    pub(crate) async fn drive_turn_streaming(
        &self,
        session_id: &str,
        message: &str,
        on_event: AgenticEventSink,
        cancel: Option<tokio_util::sync::CancellationToken>,
        override_model: Option<String>,
        acting_for: peko_tools_core::ActingFor,
    ) -> Result<StreamingResumeOutcome> {
        self.note_activity(&acting_for).await;
        let stamp = acting_for.clone();
        let outcome = self
            .drive_turn(
                session_id,
                message,
                on_event,
                cancel,
                override_model,
                acting_for,
            )
            .await;
        self.note_activity(&stamp).await;
        outcome
    }

    async fn drive_turn(
        &self,
        session_id: &str,
        message: &str,
        on_event: AgenticEventSink,
        cancel: Option<tokio_util::sync::CancellationToken>,
        override_model: Option<String>,
        acting_for: peko_tools_core::ActingFor,
    ) -> Result<StreamingResumeOutcome> {
        let conversation_channel = self.conversation_channel(session_id).await;
        // ADR-078 D7: when the policy reviews replies, a reply to anyone
        // but the owner is held back from live streaming until reviewed.
        let gate = self.executor.tooling().dispatcher().policy_gate();
        let review_reply = !acting_for.is_owner()
            && gate.is_some()
            && self
                .executor
                .principal_workspace()
                .as_deref()
                .and_then(crate::security::policy::load)
                .is_some_and(|policy| policy.review_replies);
        let reviewed_stamp = acting_for.clone();
        let sink = on_event;
        let on_event: AgenticEventSink = if review_reply {
            let inner = Arc::clone(&sink);
            Arc::new(move |event| {
                if !matches!(
                    event,
                    peko_engine::AgenticEvent::AssistantDelta { .. }
                        | peko_engine::AgenticEvent::AssistantText { .. }
                ) {
                    inner(event);
                }
            })
        } else {
            Arc::clone(&sink)
        };
        let config = ExecutionConfig {
            timeout_seconds: 0,
            model_override: override_model,
            // Peer ingress is a CONVERSATION, not a delegated task:
            // no subagent framing, the session's prior history is
            // loaded, and the peer-facing agent may itself delegate
            // using the same principal-wide run admission.
            conversation: true,
            conversation_channel,
            acting_for: Some(acting_for),
            ..ExecutionConfig::default()
        };
        let mut outcome = self
            .executor
            .resume_streaming(
                message,
                session_id,
                &self.parent_session_key,
                config,
                on_event,
                cancel,
            )
            .await?;
        if let (true, Some(gate)) = (review_reply, gate) {
            if let crate::security::judge::Verdict::Deny(reason) = gate
                .review_reply(&reviewed_stamp, &self.principal_id.0, &outcome.final_text)
                .await
            {
                tracing::info!(
                    actor = %reviewed_stamp.actor,
                    "reply withheld by the owner's policy: {reason}"
                );
                outcome.final_text =
                    "I can't share that here. The owner's policy doesn't allow it.".to_string();
            }
            sink(peko_engine::AgenticEvent::AssistantText {
                run_id: outcome.run_id.clone(),
                text: outcome.final_text.clone(),
                sequence: 0,
                is_interstitial: false,
            });
        }
        Ok(outcome)
    }
}

/// [`PeerTurnSurface`] over the principal's session metadata + channel
/// port (2026-09-07).
///
/// Installed on executors that can reach a peer-bound session from a
/// NON-ingress path — the root agent's `SubagentExecutor` (so the Agent
/// tool can target `/local-user`-style peer sessions) and the cron
/// cold-start builder. The lookup mirrors
/// [`PeerChildTurns::conversation_channel`] but additionally requires
/// the session to be STANDING and to have a DM channel: plain spawned
/// subagent sessions carry `peer_type: "principal"` + a `spawn_*` peer
/// id and must not be mistaken for peer-bound sessions.
pub(crate) struct PeerTurnSurfaceImpl {
    session_manager: Arc<RwLock<SessionManager>>,
    channel_port: Arc<dyn peko_channel::ChannelPort>,
    principal_id: peko_subject::PrincipalId,
}

impl PeerTurnSurfaceImpl {
    pub(crate) fn new(
        session_manager: Arc<RwLock<SessionManager>>,
        channel_port: Arc<dyn peko_channel::ChannelPort>,
        principal_id: peko_subject::PrincipalId,
    ) -> Self {
        Self {
            session_manager,
            channel_port,
            principal_id,
        }
    }
}

#[async_trait::async_trait]
impl crate::agents::subagent_executor::PeerTurnSurface for PeerTurnSurfaceImpl {
    async fn conversation_channel(&self, session_id: &str) -> Option<String> {
        let metas = self
            .session_manager
            .write()
            .await
            .list_all_sessions(false)
            .await
            .ok()?;
        let meta = metas
            .iter()
            .find(|m| m.session_id.to_string() == session_id)?;
        let channel = meta.channel_id.as_ref()?;
        let id = peko_channel::ChannelId::parse(channel)?;
        if self
            .channel_port
            .list_members(&id)
            .await
            .ok()?
            .contains(&ActorRef::from(&self.principal_id))
        {
            Some(channel.clone())
        } else {
            None
        }
    }

    async fn post_reply(&self, channel_id: &str, text: &str) {
        match peko_channel::ChannelId::parse(channel_id) {
            Some(channel) => {
                post_peer_dm_reply(&self.channel_port, &self.principal_id, &channel, text).await;
            }
            None => {
                tracing::warn!("peer turn delivery: unparseable channel id '{channel_id}'");
            }
        }
    }
}

/// Prompt-section handler rendering the principal's peer sessions and
/// their DM channels into the `{{session_context}}` section
/// (2026-09-08).
///
/// The model can then address any peer directly — `Agent` with
/// `path="/user-bob"` or `ChannelSend` to the listed channel — instead
/// of guessing the `user-<id>` slug convention. Registered once on the
/// daemon-global core (per-principal state is resolved per invoke from
/// the hook context's workspace + principal id, like the workspace
/// `{{roles}}` / `{{skills}}` catalog handlers). Renders nothing when
/// the principal has no peer sessions yet.
///
/// The agent's OWN position in the session tree is rendered by the
/// sibling [`SelfPositionSessionContextHandler`] (ADR-052 D5), whose
/// output is aggregated above this handler's peer list by the hook
/// registry (higher priority first).
#[derive(Debug)]
pub(crate) struct PeersSessionContextHandler;

/// Resolve the principal's sessions directory from the hook context's
/// workspace path (`<workspace>` → principal name →
/// `PathResolver::principal_layout`). Shared by the two
/// `SessionContextBuild` handlers in this module and by the
/// `ChannelSend` tool's `via` attribution
/// (`tools::builtin::channel::channel_send`).
pub(crate) fn sessions_dir_for_workspace(workspace: &str) -> Option<std::path::PathBuf> {
    let name = std::path::Path::new(workspace)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())?;
    let sessions_dir = crate::common::paths::PathResolver::new()
        .principal_layout(&name)
        .local
        .root
        .join("sessions");
    sessions_dir.exists().then_some(sessions_dir)
}

/// Render the agent's own position in the session tree (ADR-052 D5 —
/// T2 agent context): its slug path plus a role line derived from
/// the session metadata. `None` when the session has no metadata in
/// the store (e.g. a bare unit-test run).
///
/// Role lines:
/// - trunk (`parent_session_id == None`) → "root session"; the trunk
///   carries no slug and is addressable as `/` from inside its own
///   tree (`peko_session::path`).
/// - standing peer child → the peer it serves.
/// - anything else → "spawned child session".
fn render_self_position(
    metas: &[peko_session::metadata::SessionMetadata],
    session_id: peko_session::id::SessionId,
) -> Option<String> {
    let meta = metas.iter().find(|m| m.session_id == session_id)?;
    if meta.parent_session_id.is_none() {
        return Some("your position: `sess:/` — root session (the principal's trunk)".to_string());
    }
    let path = peko_session::path::compute_path(metas, session_id);
    let role = match crate::principal::conversation::binding(meta) {
        Some(crate::principal::conversation::Conversation::Channel(channel)) => {
            format!("conversation session (channel: {channel})")
        }
        None => "spawned child session".to_string(),
    };
    Some(format!("your position: `{path}` — {role}"))
}

/// Prompt-section handler rendering the agent's OWN position in the
/// session tree into the `{{session_context}}` section (ADR-052 D5 —
/// T2 agent context, 2026-09-11).
///
/// Until now the section only listed OTHER peers' `/slug` paths (see
/// [`PeersSessionContextHandler`]); the agent had no rendering of
/// where it sits itself. The hook registry aggregates multiple
/// `SessionContextBuild` handlers by concatenating their text outputs,
/// and this handler's priority (110) keeps the self-position line
/// above the peer list. Renders nothing when the session id isn't in
/// the store or the workspace doesn't resolve.
#[derive(Debug)]
pub(crate) struct SelfPositionSessionContextHandler;

#[async_trait::async_trait]
impl crate::tools::prompt_sections::PromptSectionProvider for SelfPositionSessionContextHandler {
    fn section(&self) -> &'static str {
        "session_context"
    }

    fn priority(&self) -> i32 {
        // Above `PeersSessionContextHandler` (100): the aggregator
        // renders higher-priority providers first, so the
        // self-position line lands above the peer list.
        110
    }

    async fn render(
        &self,
        input: &crate::tools::prompt_sections::PromptSectionInput,
    ) -> Option<String> {
        let session_id = peko_session::id::SessionId::parse(&input.session_id)?;
        let workspace = input.workspace.to_string_lossy().to_string();
        if workspace.is_empty() {
            return None;
        }
        let sessions_dir = sessions_dir_for_workspace(&workspace)?;
        let metas = peko_session::manager::SessionManager::new()
            .with_sessions_dir_internal(sessions_dir)
            .list_all_sessions(false)
            .await
            .unwrap_or_default();

        render_self_position(&metas, session_id)
    }
}

#[async_trait::async_trait]
impl crate::tools::prompt_sections::PromptSectionProvider for PeersSessionContextHandler {
    fn section(&self) -> &'static str {
        "session_context"
    }

    async fn render(
        &self,
        input: &crate::tools::prompt_sections::PromptSectionInput,
    ) -> Option<String> {
        let workspace = input.workspace.to_string_lossy().to_string();
        let principal_id = input.principal_id.clone();
        if workspace.is_empty() || principal_id.is_empty() {
            return None;
        }
        let port = input.channel_port.clone()?;
        let principal_id = peko_subject::PrincipalId(principal_id);

        let me = port.actor_info(&ActorRef::from(&principal_id)).await;
        let mut lines = vec![format!(
            "your actor: {} — Hub ID: {} — registration: {}",
            me.address,
            me.hub_address().unwrap_or_else(|| "none".into()),
            me.registration
        )];
        // ADR-078 D4: only owner runs see the principal's other
        // conversations; any other run (or an unattributed render) sees
        // its own conversation alone.
        let stamp = input
            .acting_for
            .clone()
            .unwrap_or_else(peko_tools_core::ActingFor::unattributed);
        for channel in port
            .list_for_principal(&principal_id)
            .await
            .unwrap_or_default()
        {
            if !stamp.is_owner() && stamp.compartment.channel() != Some(channel.as_str()) {
                continue;
            }
            let roster = port.participants(&channel).await.unwrap_or_default();
            let names = roster
                .into_iter()
                .map(|p| {
                    format!(
                        "{} — Hub: {} — name: {} — registration: {}{}",
                        p.address,
                        p.hub_address().unwrap_or_else(|| "none".into()),
                        p.name.as_deref().unwrap_or("unknown"),
                        p.registration,
                        p.mention
                            .as_ref()
                            .map(|m| format!(" — mention: {m}"))
                            .unwrap_or_default()
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!("- channel `{channel}` — participants: {names}"));
        }
        if lines.is_empty() {
            return None;
        }
        Some(format!("known contacts and conversations (Contacts lists/resolves actors; ChannelRead returns current participants):\n{}",
            crate::tools::prompt_sections::render_capped_lines(&lines, 8192, "More contacts: use Contacts with action=list.")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::principal::config::{
        PrincipalGovernanceConfig, PrincipalIdentityConfig, PrincipalIntentConfig,
        PrincipalRoutingConfig,
    };
    use peko_channel::ChannelPort;

    /// ADR-078 D4: the "known contacts and conversations" section shows
    /// a non-owner run only its own conversation; the owner sees all.
    #[tokio::test]
    async fn roster_section_shows_non_owner_runs_only_their_conversation() {
        use crate::tools::prompt_sections::{PromptSectionInput, PromptSectionProvider};
        let tmp = tempfile::tempdir().unwrap();
        let port: Arc<dyn ChannelPort> = Arc::new(peko_channel::ChannelStore::new(
            peko_channel::ChannelConfig {
                runtime_dir: tmp.path().into(),
                shared_dir: None,
            },
        ));
        let id = peko_subject::PrincipalId("prin_a".into());
        let guest = ActorRef::User("hub:guest".into());
        let friend = ActorRef::User("hub:friend".into());
        let guest_dm = port.ensure_dm(&id, &guest, "/guest").await.unwrap();
        let friend_dm = port.ensure_dm(&id, &friend, "/friend").await.unwrap();
        let render = |acting_for| {
            let port = port.clone();
            let id = id.clone();
            async move {
                PeersSessionContextHandler
                    .render(&PromptSectionInput {
                        principal_id: id.0.clone(),
                        workspace: "/workspace/a".into(),
                        session_id: String::new(),
                        channel_port: Some(port),
                        acting_for,
                    })
                    .await
                    .unwrap()
            }
        };

        let guest_stamp = peko_tools_core::ActingFor {
            actor: "user:hub:guest".into(),
            name: None,
            trust: peko_tools_core::Trust::Untrusted,
            compartment: peko_tools_core::Compartment::Conversation {
                channel: guest_dm.as_str().into(),
            },
            origin: peko_tools_core::Origin::Ingress { channel: None },
        };
        let out = render(Some(guest_stamp)).await;
        assert!(out.contains(guest_dm.as_str()), "{out}");
        assert!(!out.contains(friend_dm.as_str()), "{out}");
        assert!(!out.contains("user:hub:friend"), "{out}");

        let out = render(None).await;
        assert!(!out.contains(guest_dm.as_str()), "{out}");
        assert!(!out.contains(friend_dm.as_str()), "{out}");

        let out = render(Some(peko_tools_core::ActingFor::runtime())).await;
        assert!(out.contains(guest_dm.as_str()), "{out}");
        assert!(out.contains(friend_dm.as_str()), "{out}");
    }

    #[tokio::test]
    async fn resumed_group_surface_has_a_channel_and_no_invented_user() {
        use crate::agents::subagent_executor::PeerTurnSurface;
        let tmp = tempfile::tempdir().unwrap();
        let manager = Arc::new(RwLock::new(
            peko_session::SessionManager::new()
                .with_sessions_dir_internal(tmp.path().join("sessions")),
        ));
        let port: Arc<dyn ChannelPort> = Arc::new(peko_channel::ChannelStore::new(
            peko_channel::ChannelConfig {
                runtime_dir: tmp.path().into(),
                shared_dir: None,
            },
        ));
        let id = peko_subject::PrincipalId("principal-key".into());
        let channel = peko_channel::ChannelId::for_group("eng");
        port.create(
            &id,
            peko_channel::CreateOpts::runtime("eng").with_id(channel),
        )
        .await
        .unwrap();
        let session =
            crate::principal::peer_children::ensure_group_child("root", "group:eng", &manager)
                .await
                .unwrap();
        let surface = PeerTurnSurfaceImpl::new(manager.clone(), port.clone(), id);
        assert_eq!(
            surface.conversation_channel(&session).await,
            Some("group:eng".into())
        );
        let outsider =
            PeerTurnSurfaceImpl::new(manager, port, peko_subject::PrincipalId("other".into()));
        assert_eq!(outsider.conversation_channel(&session).await, None);
    }

    /// ADR-078 D2: a conversation's ceiling is its least-trusted member
    /// other than the principal. A driver built by `build` alone (the
    /// channel-binding wake, completion wakes, cron origin turns) must
    /// read the roster too. Before, it had no channel port, so an
    /// owner-only group stamped untrusted and its reads of the
    /// principal's own notes went to the policy reviewer (peko-benchmark
    /// `docs/RESPONSIBILITY_MASTER_RERUN2_MIMO_2026-10-20.md`, pairs 3, 5).
    #[tokio::test(flavor = "multi_thread")]
    async fn built_turns_read_conversation_rosters_for_stamps() {
        use peko_channel::CreateOpts;
        use peko_tools_core::{Compartment, Trust};
        let tmp = tempfile::tempdir().unwrap();
        let (llm, _adapter) = peko_providers::resolver::LlmResolver::mock(
            peko_providers::mock::MockAdapter::new(),
            tmp.path().join("models.toml"),
        )
        .await;
        let manager = crate::principal::PrincipalManager::with_path_resolver(
            crate::common::paths::PathResolver::with_dirs(
                tmp.path().join("config"),
                tmp.path().join("data"),
                tmp.path().join("cache"),
            ),
            Arc::new(crate::principal::DefaultPrincipalMemoryFactory),
            Arc::new(crate::principal::DefaultPrincipalRouterFactory),
            crate::async_exec::executor::standalone_inbox_registry(),
        )
        .with_resolver(llm.clone());
        let principal = manager
            .create(test_config("roster", PrincipalRoutingConfig::default()))
            .await
            .unwrap();
        let port: Arc<dyn ChannelPort> = Arc::new(peko_channel::ChannelStore::new(
            peko_channel::ChannelConfig {
                runtime_dir: tmp.path().join("channels"),
                shared_dir: None,
            },
        ));
        let tooling = crate::tools::runtime::ToolingRuntime::standalone();
        tooling.services().set_channel_port(Arc::clone(&port));
        let turns = PeerChildTurns::build(
            &principal,
            &llm,
            Arc::new(Observability::new("test")),
            None,
            tooling,
        )
        .await
        .unwrap();
        let stamps = turns.executor.stamps().expect("build installs stamps");
        let ceiling = |channel: &peko_channel::ChannelId| Compartment::Conversation {
            channel: channel.as_str().into(),
        };

        let owner = ActorRef::User("test-owner".into());
        let review = port
            .create(&principal.id, CreateOpts::runtime("review"))
            .await
            .unwrap();
        port.invite(&review, &principal.id, &owner).await.unwrap();
        assert_eq!(stamps.ceiling(&ceiling(&review)).await, Trust::Owner);

        let mixed = port
            .create(&principal.id, CreateOpts::runtime("mixed"))
            .await
            .unwrap();
        port.invite(&mixed, &principal.id, &owner).await.unwrap();
        port.invite(
            &mixed,
            &principal.id,
            &ActorRef::User("hub:stranger".into()),
        )
        .await
        .unwrap();
        assert_eq!(stamps.ceiling(&ceiling(&mixed)).await, Trust::Untrusted);
    }

    /// A conversation turn's reply goes to its caller, which already has
    /// it. Before, every such run also pushed a completion event into the
    /// trunk's inbox and fired a wake. The wake failed, because the
    /// trunk is the driver's own caller ("cannot re-attach to session"),
    /// so the trunk read every reply again at its next turn.
    #[tokio::test(flavor = "multi_thread")]
    async fn conversation_turns_deliver_no_completion_to_the_trunk() {
        let tmp = tempfile::tempdir().unwrap();
        let (llm, adapter) = peko_providers::resolver::LlmResolver::mock(
            peko_providers::mock::MockAdapter::new(),
            tmp.path().join("models.toml"),
        )
        .await;
        let manager = crate::principal::PrincipalManager::with_path_resolver(
            crate::common::paths::PathResolver::with_dirs(
                tmp.path().join("config"),
                tmp.path().join("data"),
                tmp.path().join("cache"),
            ),
            Arc::new(crate::principal::DefaultPrincipalMemoryFactory),
            Arc::new(crate::principal::DefaultPrincipalRouterFactory),
            crate::async_exec::executor::standalone_inbox_registry(),
        )
        .with_resolver(llm.clone());
        let principal = manager
            .create(test_config(
                "quiet-trunk",
                PrincipalRoutingConfig::default(),
            ))
            .await
            .unwrap();
        let port: Arc<dyn ChannelPort> = Arc::new(peko_channel::ChannelStore::new(
            peko_channel::ChannelConfig {
                runtime_dir: tmp.path().join("channels"),
                shared_dir: None,
            },
        ));
        let tooling = crate::tools::runtime::ToolingRuntime::standalone();
        tooling.services().set_channel_port(Arc::clone(&port));
        let inboxes = manager.shared_inbox_registry();
        let turns = PeerChildTurns::build(
            &principal,
            &llm,
            Arc::new(Observability::new("test")),
            Some(Arc::clone(&inboxes)),
            tooling,
        )
        .await
        .unwrap()
        .with_channel_port(Some(Arc::clone(&port)));

        let child = turns
            .ensure_child_ingress(&ActorRef::User("test-owner".into()))
            .await
            .unwrap()
            .child_id;
        adapter.queue_text("hello back");
        let stamp = turns
            .root_stamp(
                None,
                &child,
                peko_tools_core::Origin::Ingress { channel: None },
            )
            .await;
        let outcome = turns
            .drive_turn_streaming(&child, "hello", Arc::new(|_| {}), None, None, stamp)
            .await
            .unwrap();
        assert_eq!(outcome.final_text, "hello back");

        // Delivery runs on the executor's task after the waiter wakes;
        // give it time to land if it is going to.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let trunk_key = peko_session::inbox_key(&principal.id.0, &trunk_session_id());
        let queued = match inboxes.peek_inbox(&trunk_key).await {
            Some(inbox) => inbox.drain_all().await.len(),
            None => 0,
        };
        assert_eq!(queued, 0, "the trunk's inbox got the conversation reply");
    }

    /// Minimal principal config for persona tests — everything
    /// defaulted except `name` and `routing`.
    fn test_config(name: &str, routing: PrincipalRoutingConfig) -> PrincipalConfig {
        PrincipalConfig {
            actor_key: None,
            name: name.to_string(),
            id: None,
            did: None,
            owner: ActorRef::User("test-owner".into()),
            identity: PrincipalIdentityConfig::default(),
            intent: PrincipalIntentConfig::default(),
            governance: PrincipalGovernanceConfig::default(),
            routing,
            exposure: peko_auth::Exposure::Private,
            status: None,
            boot_state: None,
            permissions: vec![],
            preferred_model_id: Some("mock".to_string()),
            quota: None,
            children: Default::default(),
        }
    }

    /// Persona: a workspace `roles/root.md` prompt body lands on the
    /// child agent config's `prompt`.
    #[test]
    fn persona_comes_from_workspace_root_prompt() {
        let tmp = tempfile::tempdir().unwrap();
        let roles_dir = tmp.path().join("roles");
        std::fs::create_dir_all(&roles_dir).unwrap();
        std::fs::write(
            roles_dir.join("root.md"),
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

    /// Persona: with no workspace `root.md` and no `root_prompt`
    /// override, resolution FAILS loudly (ADR-064 follow-up: authored
    /// prompts are a default start, not a hidden default value —
    /// `manager.create` and the boot pass stamp `roles/root.md`).
    #[test]
    #[should_panic(expected = "No root role prompt found")]
    fn persona_without_root_role_file_fails_loudly() {
        let tmp = tempfile::tempdir().unwrap();
        let config = test_config("persona-test", PrincipalRoutingConfig::default());
        let _ = peer_child_agent_config(&config, tmp.path());
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
        let roles_dir = tmp.path().join("roles");
        std::fs::create_dir_all(&roles_dir).unwrap();
        std::fs::write(roles_dir.join("root.md"), "workspace persona\n").unwrap();

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

    // ─── ADR-052 D3: opt-in T1 role via `routing.peer_agent` ───

    /// `peer_agent` set + role file present → the child config's
    /// prompt is the role body; the config NAME stays the root
    /// prompt's (the executor registry key + session-stamp
    /// invariants rely on it).
    #[test]
    fn peer_agent_role_body_replaces_persona_prompt() {
        let tmp = tempfile::tempdir().unwrap();
        let roles_dir = tmp.path().join("roles");
        std::fs::create_dir_all(&roles_dir).unwrap();
        std::fs::write(
            roles_dir.join("channel-comm.md"),
            "---\ndescription: \"Channel voice\"\n---\n\nYou are CHANNEL_ROLE_MARKER, the peer-facing voice.\n",
        )
        .unwrap();
        std::fs::write(roles_dir.join("root.md"), "PERSONA_MARKER persona\n").unwrap();

        let config = test_config(
            "persona-test",
            PrincipalRoutingConfig {
                peer_agent: Some("channel-comm".to_string()),
                ..Default::default()
            },
        );
        let agent_config = peer_child_agent_config(&config, tmp.path());
        assert_eq!(
            agent_config.name, "root",
            "role swap must not change the agent name (registry-key invariant)"
        );
        let prompt = agent_config.prompt.expect("persona prompt must be set");
        assert!(
            prompt.contains("CHANNEL_ROLE_MARKER"),
            "peer_agent role body must replace the persona prompt; got: {prompt}"
        );
        assert!(
            !prompt.contains("PERSONA_MARKER"),
            "root persona body must not leak into a role-bound turn; got: {prompt}"
        );
    }

    /// Directory layout (`roles/<name>/ROLE.md`) resolves too —
    /// same lookup order as the Agent tool.
    #[test]
    fn peer_agent_role_resolves_directory_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let roles_dir = tmp.path().join("roles");
        let role_dir = roles_dir.join("channel-comm");
        std::fs::create_dir_all(&role_dir).unwrap();
        std::fs::write(role_dir.join("ROLE.md"), "DIR_LAYOUT_ROLE_MARKER\n").unwrap();
        // The trunk's root role must exist (no render-time fallback).
        std::fs::write(roles_dir.join("root.md"), "ROOT_PERSONA\n").unwrap();

        let config = test_config(
            "persona-test",
            PrincipalRoutingConfig {
                peer_agent: Some("channel-comm".to_string()),
                ..Default::default()
            },
        );
        let agent_config = peer_child_agent_config(&config, tmp.path());
        let prompt = agent_config.prompt.expect("persona prompt must be set");
        assert!(
            prompt.contains("DIR_LAYOUT_ROLE_MARKER"),
            "directory-layout role file must resolve; got: {prompt}"
        );
    }

    /// `peer_agent` set + role file MISSING → warn + fall back to the
    /// root persona; peer ingress must never break on a bad value.
    #[test]
    fn peer_agent_missing_role_falls_back_to_root_persona() {
        let tmp = tempfile::tempdir().unwrap();
        let roles_dir = tmp.path().join("roles");
        std::fs::create_dir_all(&roles_dir).unwrap();
        std::fs::write(roles_dir.join("root.md"), "PERSONA_MARKER persona\n").unwrap();

        let config = test_config(
            "persona-test",
            PrincipalRoutingConfig {
                peer_agent: Some("does-not-exist".to_string()),
                ..Default::default()
            },
        );
        let agent_config = peer_child_agent_config(&config, tmp.path());
        assert_eq!(agent_config.name, "root");
        let prompt = agent_config.prompt.expect("persona prompt must be set");
        assert!(
            prompt.contains("PERSONA_MARKER"),
            "unresolvable peer_agent must fall back to the root persona; got: {prompt}"
        );
    }

    /// `peer_agent: None` → today's persona inheritance, unchanged.
    #[test]
    fn peer_agent_none_inherits_root_persona() {
        let tmp = tempfile::tempdir().unwrap();
        let roles_dir = tmp.path().join("roles");
        std::fs::create_dir_all(&roles_dir).unwrap();
        std::fs::write(roles_dir.join("root.md"), "PERSONA_MARKER persona\n").unwrap();
        std::fs::write(roles_dir.join("channel-comm.md"), "role body\n").unwrap();

        let config = test_config(
            "persona-test",
            PrincipalRoutingConfig {
                peer_agent: None,
                ..Default::default()
            },
        );
        let agent_config = peer_child_agent_config(&config, tmp.path());
        let prompt = agent_config.prompt.expect("persona prompt must be set");
        assert!(
            prompt.contains("PERSONA_MARKER"),
            "peer_agent None must inherit the root persona; got: {prompt}"
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
            principal_id.clone(),
            crate::tools::runtime::ToolingRuntime::standalone(),
        );
        PeerChildTurns {
            executor,
            session_manager,
            parent_session_key: trunk_session_id(),
            agent_name: "root".to_string(),
            principal_id,
            channel_port,
            dm_subscriber_hook: hook,
            dm_lock,
            trust: None,
            idle_detector: None,
        }
    }

    /// Only turns a person started mark the principal active: cron
    /// turns and completion wakes leave the idle window alone.
    #[tokio::test]
    async fn only_ingress_turns_count_as_activity() {
        let (_dir, session_manager, _store) = dm_fixture().await;
        let detector = Arc::new(peko_cron::IdleDetector::new());
        let turns = bare_turns(session_manager, None, None, None)
            .with_idle_detector(Some(Arc::clone(&detector)));
        let start = detector.last_activity("prin_self").await;
        let stamp = |origin| peko_tools_core::ActingFor {
            origin,
            ..peko_tools_core::ActingFor::runtime()
        };

        for origin in [
            peko_tools_core::Origin::Cron {
                job_id: "keepalive".into(),
            },
            peko_tools_core::Origin::Wake,
            peko_tools_core::Origin::Runtime,
        ] {
            turns.note_activity(&stamp(origin)).await;
        }
        assert_eq!(detector.last_activity("prin_self").await, start);

        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        turns
            .note_activity(&stamp(peko_tools_core::Origin::Ingress { channel: None }))
            .await;
        assert!(detector.last_activity("prin_self").await > start);
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

        let peer = ActorRef::User("alice".into());
        let child_id = turns.ensure_child_ingress(&peer).await.unwrap().child_id;

        // The DM channel exists, named + bound to the peer child.
        let channels = store.list_for_principal(&turns.principal_id).await.unwrap();
        assert_eq!(channels.len(), 1);
        assert_eq!(
            store
                .binding_for(&channels[0], &turns.principal_id)
                .await
                .unwrap()
                .map(|b| b.session)
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
        let error = turns
            .ensure_child_ingress(&ActorRef::User("alice".into()))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("requires a channel store"));
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

        let peer = ActorRef::User("alice".into());
        let mut handles = Vec::new();
        for _ in 0..6 {
            let turns = Arc::clone(&turns);
            let peer = peer.clone();
            handles.push(tokio::spawn(async move {
                turns.ensure_child_ingress(&peer).await
            }));
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

        let peer = ActorRef::User("alice".into());
        let ingress = turns.ensure_child_ingress(&peer).await.unwrap();
        let dm = ingress.dm_channel.expect("port attached ⇒ DM channel");

        post_peer_dm_inbound(&port, &turns.principal_id, &dm, &peer.to_string(), "hello")
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
                ("user:local:alice".to_string(), "hello".to_string()),
                (
                    "principal:local:prin_self".to_string(),
                    "hi alice".to_string()
                ),
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
        let error = turns
            .ensure_child_ingress(&ActorRef::User("alice".into()))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("requires a channel store"));
    }

    /// Conversation-prompt context: the peer's subject + DM channel
    /// id resolve from the child session's metadata + the channel
    /// port's passive binding; a no-port context yields `None` for
    /// the channel (the section simply omits the line).
    #[tokio::test]
    async fn conversation_channel_resolves_the_dm_channel() {
        let (_dir, manager, store) = dm_fixture().await;
        let port: Arc<dyn peko_channel::ChannelPort> = store.clone();
        let turns = bare_turns(
            manager,
            Some(port),
            None,
            Some(Arc::new(tokio::sync::Mutex::new(()))),
        );

        let peer = ActorRef::User("alice".into());
        let ingress = turns.ensure_child_ingress(&peer).await.unwrap();
        let dm = ingress.dm_channel.expect("port attached ⇒ DM channel");

        let channel = turns.conversation_channel(&ingress.child_id).await;
        assert_eq!(channel.as_deref(), Some(dm.to_string().as_str()));

        // Unknown session id ⇒ no context.
        assert!(turns
            .conversation_channel("no-such-session")
            .await
            .is_none());
    }

    /// No channel port (standalone/test contexts): ingress needs a
    /// channel store to bind the conversation.
    #[tokio::test]
    async fn ingress_without_port_is_refused() {
        let (_dir, manager, _store) = dm_fixture().await;
        let turns = bare_turns(manager, None, None, None);
        let error = turns
            .ensure_child_ingress(&ActorRef::User("alice".into()))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("requires a channel store"));
    }

    // ─── ADR-052 D5: self-position rendering ───────────────────────

    fn position_meta(
        id: peko_session::id::SessionId,
        parent: Option<peko_session::id::SessionId>,
        slug: Option<&str>,
    ) -> peko_session::metadata::SessionMetadata {
        peko_session::metadata::SessionMetadata {
            session_id: id,
            agent_name: "root".to_string(),
            created_at: 0,
            updated_at: 0,
            message_count: 0,
            turn_count: 0,
            last_total_tokens: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            model_context_limit: None,
            transcript_file: format!("{id}.jsonl"),
            title: None,
            parent_session_id: parent,
            trigger: "user".to_string(),
            channel_id: None,
            peer_type: None,
            peer_id: None,
            slug: slug.map(String::from),
            page_limit: None,
            pruned_pages: 0,
            compaction_count: 0,
            last_compaction_at: None,
            consecutive_auto_compactions: 0,
            consecutive_compaction_failures: 0,
            compartment: None,
        }
    }

    /// trunk (no slug, no parent) ── local-user (standing peer child)
    ///                             └─ memory ── task-b (spawned child)
    fn position_tree() -> (
        peko_session::id::SessionId,
        peko_session::id::SessionId,
        peko_session::id::SessionId,
        Vec<peko_session::metadata::SessionMetadata>,
    ) {
        let trunk =
            peko_session::id::SessionId::parse("00000000-0000-0000-0000-000000000001").unwrap();
        let local_user =
            peko_session::id::SessionId::parse("00000000-0000-0000-0000-000000000002").unwrap();
        let memory =
            peko_session::id::SessionId::parse("00000000-0000-0000-0000-000000000003").unwrap();
        let task =
            peko_session::id::SessionId::parse("00000000-0000-0000-0000-000000000004").unwrap();
        let mut peer_child = position_meta(local_user, Some(trunk), Some("local-user"));
        peer_child.trigger = "spawn".into();
        peer_child.peer_type = Some("user".to_string());
        peer_child.peer_id = Some("local".to_string());
        let metas = vec![
            position_meta(trunk, None, None),
            peer_child,
            position_meta(memory, Some(trunk), Some("memory")),
            position_meta(task, Some(memory), Some("task-b")),
        ];
        (trunk, local_user, task, metas)
    }

    #[test]
    fn self_position_trunk_renders_root_session() {
        let (trunk, _, _, metas) = position_tree();
        let rendered = render_self_position(&metas, trunk).expect("trunk has a position");
        assert_eq!(
            rendered, "your position: `sess:/` — root session (the principal's trunk)",
            "got: {rendered}"
        );
    }

    #[test]
    fn self_position_peer_child_renders_path_and_peer() {
        let (_, local_user, _, metas) = position_tree();
        let rendered = render_self_position(&metas, local_user).expect("peer child has a position");
        assert_eq!(
            rendered, "your position: `sess:/local-user` — spawned child session",
            "got: {rendered}"
        );
    }

    #[test]
    fn self_position_spawned_child_renders_nested_path() {
        let (_, _, task, metas) = position_tree();
        let rendered = render_self_position(&metas, task).expect("spawned child has a position");
        assert_eq!(
            rendered, "your position: `sess:/memory/task-b` — spawned child session",
            "got: {rendered}"
        );
    }

    #[test]
    fn self_position_unknown_session_renders_nothing() {
        let (_, _, _, metas) = position_tree();
        let ghost = peko_session::id::SessionId::new();
        assert_eq!(render_self_position(&metas, ghost), None);
    }
}
