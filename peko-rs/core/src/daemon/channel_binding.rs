//! Channel passive binding — Phase 4 of the agent-session paradigm
//! sprint (2026-08-15, `docs/architecture/AGENT_SESSION_PARADIGM.md`
//! §3.1 type 1).
//!
//! A channel with a `passive_binding` in its `meta.json` (a session id
//! or `/path` in the creator principal's session tree) is a **DM-tier**
//! channel: an inbound `Posted` event from another member automatically
//! wakes the bound session, the session runs one LLM turn, and the
//! final reply is posted back to the channel as the principal.
//!
//! ## Design decisions
//!
//! - **Turn driving: the subagent resume path.** Turns are driven by
//!   [`SubagentResumeDriver`], a thin wrapper over
//!   `SubagentExecutor::resume_and_execute` — the same path the `Agent`
//!   tool's `resume` action uses. Bound sessions are expected to be
//!   spawned sessions (standing children carry `trigger="spawn"`), which
//!   is exactly what the resume guards require; the session's prior
//!   history loads from its JSONL, so the child continues with full
//!   context. The alternative — a `PrincipalManager`-based receive path
//!   — was rejected: `receive`/`receive_trunk` land turns in root
//!   sessions (`root:{peer}` / `root:self`) and run the *root* agent
//!   prompt, neither of which fits "wake the bound child session".
//! - **No `ChannelKind::Channel`.** That variant was sketched for turn
//!   attribution through `PrincipalManager` routing
//!   (`principal/router.rs`); since turns do NOT flow through the
//!   router, the variant would have no consumer and is deliberately
//!   skipped.
//! - **No chat-log projection.** Channel-origin turns never touch a
//!   separate conversation log (the chat-log crate was retired in
//!   sprint 3 Phase 13). The channel's own append-only event log is
//!   the durable record of both directions (the inbound post and the
//!   posted reply); the bound session's JSONL is the principal's
//!   private working memory. This preserves the channel/session
//!   separation (ADR-044, PEKO.md).
//! - **Author-based turn-ownership partition (Phase 11).** The peer DM
//!   channels are driven by the ingress handlers (`peko send` IPC,
//!   manager `receive*`/A2A/Hub, IPC steer): the handler posts the
//!   inbound message with `author = peer.to_string()` — the Subject
//!   wire form (`user:alice`, `user:local`, `public`,
//!   `principal:<did>`) — and drives the turn itself, then posts the
//!   reply as the principal. The responder must NEVER act on those
//!   posts (that would double-drive the turn), so
//!   [`PassiveBindingResponder::response_trigger`] drops any `Posted`
//!   event whose author parses as a Subject wire form. The responder
//!   owns exactly the raw-principal-id-authored posts: another local
//!   principal posting via `peko channel send` / `ChannelSend`
//!   (existing Phase 4 behavior) and Phase 12 cross-runtime fan-out
//!   posts (a mirrored DM post arrives with author = the peer
//!   principal's source-local id — a raw form, not a Subject wire
//!   form). Remote-mirrored posts whose author IS a Subject wire form
//!   (e.g. `user:*`) are still skipped by this rule — the remote
//!   side's ingress handler owns those turns.
//! - **Root-post-only rule (Phase 12a).** The trigger additionally
//!   requires `parent.is_none()`: every responder reply is posted via
//!   `PostMsg::reply(triggering_line, …)`, so no responder ever reacts
//!   to a reply. Cross-runtime ping-pong (A's mirrored reply wakes B's
//!   responder, whose reply wakes A's, …) is then STRUCTURALLY
//!   impossible — mirror line numbers diverge between runtimes, so
//!   correlation-based dedup can't work and the parent-presence bit is
//!   all the rule needs (`append_remote_event` skips parent
//!   validation, so a dangling remote parent value is fine).
//!   Trade-off, accepted: a threaded human reply (`PostMsg::reply` via
//!   `ChannelSend`) no longer wakes a bound session — root posts only.
//!   Local ingress reply projections (`post_peer_dm_reply`) stay root
//!   posts (self-authored + self-skipped anyway).
//! - **Self-post suppression (anti-loop invariant).** The responder
//!   posts its reply via `ChannelPort::post` as the principal, then
//!   observes its own post on the subscriber's next poll tick.
//!   [`PassiveBindingResponder::response_trigger`] drops any `Posted`
//!   event whose `author` equals the bound principal's id — a responder
//!   NEVER processes its own posts (PEKO.md "Violates channel/session
//!   separation"). Author matching is unambiguous: `ChannelStore::post`
//!   writes `author: sender.to_string()` and the responder compares
//!   against the same `PrincipalId::to_string()` form. Together with
//!   the Subject-wire-form skip and the root-post-only rule above, the
//!   partition is: posts the responder acts on are exactly
//!   raw-principal-id ROOT posts from OTHER principals.
//! - **Run concurrency.** `consider_response` spawns the turn as a
//!   detached task and returns immediately, so the subscriber's cursor
//!   keeps advancing (persisted cursors, Phase 0) instead of blocking
//!   on an LLM turn. Turns for one channel serialize on a per-responder
//!   `tokio::Mutex`, so a second message arriving mid-run queues behind
//!   the first and gets its own turn — never a crash, never a
//!   double-run. Cross-path safety (root agent resuming the same
//!   session via the `Agent` tool while a channel turn is in flight) is
//!   inherited from `resume_and_execute`'s
//!   `has_active_subagent_run_for_child` guard: the driver's executor
//!   deliberately shares the root agent's global registry key
//!   (`default_root_prompt().name`), so both paths see each other's
//!   runs and the loser is refused with `err_run_active` (logged, turn
//!   skipped).
//! - **Failure policy: log-only.** A failed resolution or turn logs a
//!   warning and posts NOTHING — a broken binding must never error-spam
//!   a channel its other members read. (The trunk/cron paths follow the
//!   same log-only idiom.)
//! - **Restart semantics.** Subscriber cursors are persisted (Phase 0)
//!   and loaded at spawn; a daemon restart does not re-fire channel
//!   history through the responder. Caveat: a message whose turn task
//!   was in flight when the daemon died is not retried (the cursor
//!   advanced past it at delivery) — at-most-once delivery, by design.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::RwLock;
use tracing::{debug, warn};

use peko_channel::{
    ChannelCursors, ChannelEvent, ChannelId, ChannelMeter, ChannelPort, ChannelResponder,
    ChannelSubscriber, NoopChannelResponder, PostMsg, RespondCtx, SubscriptionConfig,
};
use peko_observability::Observability;
use peko_session::manager::SessionManager;
use peko_subject::PrincipalId;

use crate::agents::subagent_executor::{ExecutionConfig, SubagentExecutor};
use crate::extensions::framework::async_exec::executor::registry::TaskMetadata;
use crate::extensions::framework::async_exec::executor::AsyncTaskStatus;
use crate::principal::manager::PrincipalManager;
use crate::principal::Principal;

/// Extra slack on top of the turn's own timeout when waiting for a
/// bound-session run to reach a terminal registry state.
const COMPLETION_WAIT_MARGIN_SECS: u64 = 30;

/// Poll interval for the run-completion wait. Mirrors the 50ms poll in
/// `AsyncTaskRegistry::wait_for_completion` (which we deliberately do
/// NOT use — see [`SubagentResumeDriver::drive_turn`]).
const COMPLETION_POLL_MS: u64 = 50;

// ---------------------------------------------------------------------------
// Turn-driver + binding-resolver seams
// ---------------------------------------------------------------------------

/// Drives one LLM turn in an existing session of the principal's tree
/// and returns the final response text. Trait seam so the responder's
/// decision logic is unit-testable without a provider; the production
/// impl is [`SubagentResumeDriver`].
#[async_trait]
pub(crate) trait BoundTurnDriver: Send + Sync + 'static {
    /// Run a turn in `session_id` (a canonical id — the responder
    /// resolves `/`-paths before calling) with `message` as the user
    /// input. Returns the final response text.
    async fn drive_turn(&self, session_id: &str, message: &str) -> anyhow::Result<String>;
}

/// Resolves a channel's passive binding (raw session id or `/path`) to
/// a canonical session id. Trait seam for the same reason as
/// [`BoundTurnDriver`]; the production impl is
/// [`SessionStoreBindingResolver`].
#[async_trait]
pub(crate) trait BindingResolver: Send + Sync + 'static {
    async fn resolve(&self, binding: &str) -> anyhow::Result<String>;
}

// ---------------------------------------------------------------------------
// PassiveBindingResponder
// ---------------------------------------------------------------------------

/// `ChannelResponder` for DM-tier (passively bound) channels. See the
/// module docs for the design; the anti-loop filter is
/// [`Self::response_trigger`].
///
/// Cheap to clone (one `Arc` inside) so `consider_response` can move a
/// handle into the detached turn task.
#[derive(Clone)]
pub(crate) struct PassiveBindingResponder {
    inner: Arc<ResponderInner>,
}

struct ResponderInner {
    channel: ChannelId,
    principal: PrincipalId,
    /// The raw binding string from `meta.json` (id or `/path`).
    binding: String,
    port: Arc<dyn ChannelPort>,
    resolver: Arc<dyn BindingResolver>,
    driver: Arc<dyn BoundTurnDriver>,
    /// Resolved session id cache — the binding is resolved ONCE per
    /// responder (per channel) via `get_or_try_init`, so a failing
    /// resolution retries on the next event but a successful one is
    /// never recomputed.
    resolved_session: tokio::sync::OnceCell<String>,
    /// Serializes turns for this channel: a message arriving mid-run
    /// queues behind the in-flight turn instead of double-running the
    /// bound session.
    turn_lock: tokio::sync::Mutex<()>,
}

impl PassiveBindingResponder {
    pub(crate) fn new(
        channel: ChannelId,
        principal: PrincipalId,
        binding: String,
        port: Arc<dyn ChannelPort>,
        resolver: Arc<dyn BindingResolver>,
        driver: Arc<dyn BoundTurnDriver>,
    ) -> Self {
        Self {
            inner: Arc::new(ResponderInner {
                channel,
                principal,
                binding,
                port,
                resolver,
                driver,
                resolved_session: tokio::sync::OnceCell::new(),
                turn_lock: tokio::sync::Mutex::new(()),
            }),
        }
    }

    /// The anti-loop filter. Returns the message text to act on, or
    /// `None` to drop the event. Only `Posted` ROOT events from OTHER
    /// members trigger a turn — `Created`/`MemberJoined`/`MemberLeft`
    /// are channel bookkeeping, self-authored posts are the loop
    /// vector this filter exists to close, and parent-bearing posts
    /// are replies (every responder reply carries `parent: Some`, so
    /// reacting to one would make cross-runtime ping-pong possible).
    ///
    /// Phase 11 turn-ownership partition: posts whose author is a
    /// Subject wire form (`user:*`, `principal:*`, `public`) are
    /// ingress-handler-owned — the handler already posted them AND
    /// drives the turn, so the responder must skip them (no
    /// double-turn). The responder drives only raw-principal-id root
    /// posts from other principals (local `peko channel send` /
    /// `ChannelSend` cross-principal posts; Phase 12 cross-runtime
    /// fan-out posts).
    fn response_trigger(principal: &PrincipalId, event: &ChannelEvent) -> Option<String> {
        match event {
            ChannelEvent::Posted {
                author,
                text,
                parent,
                ..
            } if *author != principal.to_string()
                && parent.is_none()
                && peko_subject::Subject::from_str(author).is_err() =>
            {
                Some(text.clone())
            }
            _ => None,
        }
    }
}

impl ResponderInner {
    /// One full passive-binding cycle for `text`: resolve the binding
    /// (cached), drive the turn (serialized per channel), post the
    /// reply threaded onto the triggering event (`event_id`). All
    /// failures are log-only — see the module docs.
    async fn run_turn(&self, event_id: String, text: String) {
        let _turn_guard = self.turn_lock.lock().await;

        let session_id = match self
            .resolved_session
            .get_or_try_init(|| self.resolver.resolve(&self.binding))
            .await
        {
            Ok(id) => id.clone(),
            Err(e) => {
                warn!(
                    channel = %self.channel,
                    binding = %self.binding,
                    "channel binding: resolution failed (will retry on next event): {e:#}"
                );
                return;
            }
        };

        match self.driver.drive_turn(&session_id, &text).await {
            Ok(reply) => {
                let reply = reply.trim();
                if reply.is_empty() {
                    debug!(
                        channel = %self.channel,
                        session = %session_id,
                        "channel binding: turn produced an empty reply; posting nothing"
                    );
                    return;
                }
                // Phase 12a: the reply is threaded onto the triggering
                // event so `response_trigger`'s root-post-only rule
                // never reacts to it — here (self-author skip already
                // covers the local echo) and on every remote mirror the
                // reply fans out to.
                if let Err(e) = self
                    .port
                    .post(
                        &self.channel,
                        &self.principal,
                        PostMsg::reply(event_id, reply),
                    )
                    .await
                {
                    warn!(
                        channel = %self.channel,
                        "channel binding: reply post failed: {e}"
                    );
                }
            }
            Err(e) => {
                warn!(
                    channel = %self.channel,
                    session = %session_id,
                    "channel binding: turn failed (posting nothing): {e:#}"
                );
            }
        }
    }
}

#[async_trait]
impl ChannelResponder for PassiveBindingResponder {
    async fn consider_response(&self, ctx: RespondCtx) -> peko_channel::Result<()> {
        let Some(text) = Self::response_trigger(&ctx.principal, &ctx.event) else {
            return Ok(());
        };
        // Detached task: the subscriber's cursor advance + persistence
        // must not block on an LLM turn (see module docs). The turn
        // lock inside `run_turn` serializes concurrent arrivals.
        let inner = Arc::clone(&self.inner);
        tokio::spawn(async move { inner.run_turn(ctx.event_id, text).await });
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// SessionStoreBindingResolver (production)
// ---------------------------------------------------------------------------

/// Production [`BindingResolver`] over the principal's session store.
/// Raw ids pass through; `/`-rooted paths resolve via
/// `peko_session::path::resolve_path` anchored at the principal's owner
/// root (`root:{owner}`) — the tree every standing child hangs under
/// (Phase 2). A dangling owner root (no human has chatted yet) fails
/// path resolution with a clear error; the responder logs and retries
/// on the next event.
pub(crate) struct SessionStoreBindingResolver {
    session_manager: Arc<RwLock<SessionManager>>,
    anchor: String,
}

impl SessionStoreBindingResolver {
    pub(crate) fn new(session_manager: Arc<RwLock<SessionManager>>, anchor: String) -> Self {
        Self {
            session_manager,
            anchor,
        }
    }
}

#[async_trait]
impl BindingResolver for SessionStoreBindingResolver {
    async fn resolve(&self, binding: &str) -> anyhow::Result<String> {
        if !binding.starts_with('/') {
            return Ok(binding.to_string());
        }
        let metas = self
            .session_manager
            .write()
            .await
            .list_all_sessions(false)
            .await?;
        peko_session::path::resolve_path(&metas, &self.anchor, binding)
    }
}

// ---------------------------------------------------------------------------
// SubagentResumeDriver (production)
// ---------------------------------------------------------------------------

/// Production [`BoundTurnDriver`]: drives the turn through
/// `SubagentExecutor::resume_and_execute` — the same machinery the
/// `Agent` tool's `resume` action uses — then waits for the run to
/// reach a terminal registry state and extracts the final text.
///
/// The parent session key is the principal's trunk (`root:self`,
/// Phase 7): a base-session caller, so the ownership guards admit any
/// session in the principal's tree (and while the trunk is dangling,
/// the subtree check still admits its own children — exactly where
/// standing children live).
pub(crate) struct SubagentResumeDriver {
    executor: SubagentExecutor,
    parent_session_key: String,
}

impl SubagentResumeDriver {
    pub(crate) fn new(executor: SubagentExecutor, parent_session_key: String) -> Self {
        Self {
            executor,
            parent_session_key,
        }
    }
}

#[async_trait]
impl BoundTurnDriver for SubagentResumeDriver {
    async fn drive_turn(&self, session_id: &str, message: &str) -> anyhow::Result<String> {
        // No completion announcement: the reply goes to the CHANNEL,
        // not the parent session's inbox. Everything else defaults
        // (cleanup: Keep — bound sessions outlive their runs;
        // timeout 300s; max_depth 1).
        let config = ExecutionConfig {
            announce_completion: false,
            ..ExecutionConfig::default()
        };
        let wait_timeout =
            Duration::from_secs(config.timeout_seconds + COMPLETION_WAIT_MARGIN_SECS);

        let run_id = self
            .executor
            .resume_and_execute(message, session_id, &self.parent_session_key, config, None)
            .await?;

        // Poll for a terminal state with SHORT-LIVED read guards. The
        // registry's own `wait_for_completion` holds its read guard
        // across the whole wait, which would block the completing
        // task's write-guard status update and deadlock until timeout —
        // so we re-acquire per poll instead.
        let registry = Arc::clone(self.executor.registry());
        let deadline = tokio::time::Instant::now() + wait_timeout;
        loop {
            let status = registry.read().await.check_status(&run_id);
            match status {
                Some(AsyncTaskStatus::Completed { .. }) => {
                    let output = {
                        let guard = registry.read().await;
                        guard.get(&run_id).and_then(|entry| match &entry.metadata {
                            TaskMetadata::Subagent(meta) => {
                                meta.subagent_result.as_ref().and_then(|r| r.output.clone())
                            }
                            _ => None,
                        })
                    };
                    return output.ok_or_else(|| {
                        anyhow::anyhow!("bound-session run {run_id} completed without output")
                    });
                }
                Some(AsyncTaskStatus::Failed { error }) => {
                    return Err(anyhow::anyhow!("bound-session turn failed: {error}"));
                }
                Some(AsyncTaskStatus::Cancelled) => {
                    return Err(anyhow::anyhow!("bound-session run {run_id} was cancelled"));
                }
                Some(_) => {
                    if tokio::time::Instant::now() >= deadline {
                        return Err(anyhow::anyhow!(
                            "bound-session run {run_id} timed out after {wait_timeout:?}"
                        ));
                    }
                    tokio::time::sleep(Duration::from_millis(COMPLETION_POLL_MS)).await;
                }
                None => {
                    return Err(anyhow::anyhow!(
                        "bound-session run {run_id} vanished from the registry"
                    ));
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// select_responder — the per-channel wiring decision
// ---------------------------------------------------------------------------

/// Pick the responder for one (principal, channel) pair. Pure decision
/// shared by the boot path and the post-boot hooks, and unit-tested
/// here: a binding + a successfully built driver yields a
/// [`PassiveBindingResponder`]; anything else (unbound channel, or a
/// principal with no resolvable model) keeps the pre-Phase-4
/// [`NoopChannelResponder`] behavior.
pub(crate) fn select_responder(
    channel: ChannelId,
    principal: PrincipalId,
    port: Arc<dyn ChannelPort>,
    binding: Option<String>,
    driver: Option<(Arc<dyn BoundTurnDriver>, Arc<dyn BindingResolver>)>,
) -> Arc<dyn ChannelResponder> {
    match (binding, driver) {
        (Some(binding), Some((turn_driver, resolver))) => Arc::new(PassiveBindingResponder::new(
            channel,
            principal,
            binding,
            port,
            resolver,
            turn_driver,
        )),
        (binding, _) => {
            if let Some(binding) = binding {
                debug!(
                    channel = %channel,
                    binding = %binding,
                    "channel binding: no turn driver available; channel stays active-only"
                );
            }
            Arc::new(NoopChannelResponder)
        }
    }
}

// ---------------------------------------------------------------------------
// ChannelBindingSupervisor
// ---------------------------------------------------------------------------

/// Owns the per-(principal, channel) `ChannelSubscriber` lifespan:
/// boot-time enumeration ([`Self::spawn_all`]) plus the post-boot
/// hooks ([`Self::ensure_subscriber`], called from `ChannelHost`'s
/// `channel_created` / `kickoff_channel_read` impls on `AppState`).
///
/// Also owns the per-principal [`SubagentResumeDriver`] cache — driver
/// construction resolves the principal's provider, so it happens once
/// per principal (first bound channel), not once per message.
///
/// Post-boot approach: hook-driven, not a periodic rescan. The IPC
/// `ChannelCreate` / `ChannelInvite` success arms call `ChannelHost`
/// hooks, and (Phase 12a) the cross-runtime invite bootstrap
/// (`TunnelHost::dm_channel_mirror_bootstrap`) calls
/// [`Self::ensure_subscriber`] right after `join_remote`, so a
/// mirrored channel gets its subscriber immediately instead of at the
/// next boot sweep.
pub(crate) struct ChannelBindingSupervisor {
    port: Arc<dyn ChannelPort>,
    meter: Arc<dyn ChannelMeter>,
    runtime_dir: PathBuf,
    principal_manager: Arc<PrincipalManager>,
    llm_resolver: Arc<peko_providers::LlmResolver>,
    observability: Arc<Observability>,
    /// (principal, channel) pairs that already have a live subscriber.
    /// Short critical sections only — a std Mutex is fine.
    spawned: std::sync::Mutex<HashSet<(PrincipalId, ChannelId)>>,
    /// Per-principal (turn driver, binding resolver) bundles.
    drivers: tokio::sync::Mutex<
        HashMap<PrincipalId, (Arc<SubagentResumeDriver>, Arc<SessionStoreBindingResolver>)>,
    >,
}

impl ChannelBindingSupervisor {
    pub(crate) fn new(
        port: Arc<dyn ChannelPort>,
        meter: Arc<dyn ChannelMeter>,
        runtime_dir: PathBuf,
        principal_manager: Arc<PrincipalManager>,
        llm_resolver: Arc<peko_providers::LlmResolver>,
        observability: Arc<Observability>,
    ) -> Self {
        Self {
            port,
            meter,
            runtime_dir,
            principal_manager,
            llm_resolver,
            observability,
            spawned: std::sync::Mutex::new(HashSet::new()),
            drivers: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Boot path: spawn one subscriber per (loaded principal × channel
    /// the principal is a member of). Replaces the pre-Phase-4
    /// `spawn_channel_subscribers` body; spawn-and-forget so a
    /// subscriber crash doesn't block daemon boot.
    pub(crate) async fn spawn_all(self: &Arc<Self>) -> Vec<tokio::task::JoinHandle<()>> {
        let principals = self.principal_manager.list_all().await;
        let mut handles = Vec::new();
        for principal in principals {
            let channels = match self.port.list_for_principal(&principal.id).await {
                Ok(c) => c,
                Err(e) => {
                    warn!(
                        principal = %principal.id,
                        ?e,
                        "channel subscribers: list_for_principal failed; skipping"
                    );
                    continue;
                }
            };
            for channel in channels {
                if let Some(handle) = self.spawn_one(Arc::clone(&principal), channel).await {
                    handles.push(handle);
                }
            }
        }
        handles
    }

    /// Post-boot hook (create/invite): ensure a subscriber exists for
    /// this pair. Sync wrapper — spawns the async work so the IPC
    /// handler never blocks on it; dedup via `spawned` makes repeat
    /// hooks harmless.
    pub(crate) fn ensure_subscriber(self: &Arc<Self>, principal: PrincipalId, channel: ChannelId) {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            let Some(principal) = this.principal_manager.get(principal.clone()).await else {
                warn!(
                    principal = %principal,
                    channel = %channel,
                    "channel subscribers: post-boot hook for unknown principal; skipping"
                );
                return;
            };
            let _ = this.spawn_one(principal, channel).await;
        });
    }

    /// Test seam: has a subscriber been spawned for this
    /// (principal, channel) pair? `ensure_subscriber` is
    /// fire-and-forget, so tests poll this to observe the spawn.
    #[cfg(test)]
    pub(crate) fn has_subscriber(&self, principal: &PrincipalId, channel: &ChannelId) -> bool {
        self.spawned
            .lock()
            .expect("spawned mutex poisoned")
            .contains(&(principal.clone(), channel.clone()))
    }

    /// Spawn one subscriber unless the pair already has one. Returns
    /// the `JoinHandle` for a fresh spawn, `None` on dedup hit.
    async fn spawn_one(
        self: &Arc<Self>,
        principal: Arc<Principal>,
        channel: ChannelId,
    ) -> Option<tokio::task::JoinHandle<()>> {
        {
            let mut spawned = self.spawned.lock().expect("spawned mutex poisoned");
            if !spawned.insert((principal.id.clone(), channel.clone())) {
                return None;
            }
        }

        let channel_dir = self.runtime_dir.join("channels").join(channel.as_str());
        // Resume from the persisted per-member cursors so a daemon
        // restart doesn't re-observe (and, for bound channels,
        // re-fire) the channel's entire event history. A missing file
        // loads as an empty map (first-ever boot); a corrupt file
        // falls back to fresh cursors.
        let cursors = match ChannelCursors::load(&channel_dir).await {
            Ok(c) => c,
            Err(e) => {
                warn!(
                    channel = %channel,
                    dir = %channel_dir.display(),
                    ?e,
                    "channel subscribers: cursor load failed; starting from fresh cursors"
                );
                ChannelCursors::new()
            }
        };

        let responder = self.responder_for(&principal, &channel).await;
        let sub = ChannelSubscriber::new(
            channel,
            principal.id.clone(),
            channel_dir,
            Arc::clone(&self.port),
            responder,
            Arc::clone(&self.meter),
            cursors,
            SubscriptionConfig::default(),
        );
        Some(sub.spawn())
    }

    /// Read the channel's binding from `meta.json` and pick the
    /// responder. Unreadable meta (or any other binding read failure)
    /// degrades to `Noop` with a warning — the meter-only subscriber
    /// still runs.
    async fn responder_for(
        &self,
        principal: &Arc<Principal>,
        channel: &ChannelId,
    ) -> Arc<dyn ChannelResponder> {
        let binding = match self.port.passive_binding(channel).await {
            Ok(b) => b,
            Err(e) => {
                warn!(
                    channel = %channel,
                    "channel subscribers: passive_binding read failed; treating as unbound: {e}"
                );
                None
            }
        };
        let driver = match binding {
            Some(_) => self.driver_for(principal).await.map(|(turn, resolver)| {
                (
                    Arc::clone(&turn) as Arc<dyn BoundTurnDriver>,
                    Arc::clone(&resolver) as Arc<dyn BindingResolver>,
                )
            }),
            None => None,
        };
        select_responder(
            channel.clone(),
            principal.id.clone(),
            Arc::clone(&self.port),
            binding,
            driver,
        )
    }

    /// Get or build the per-principal driver bundle. `None` (with a
    /// warning) when the principal has no resolvable model — the same
    /// configuration error that breaks `peko send`; bound channels
    /// then stay active-only until the principal config is fixed and
    /// the daemon restarts.
    async fn driver_for(
        &self,
        principal: &Arc<Principal>,
    ) -> Option<(Arc<SubagentResumeDriver>, Arc<SessionStoreBindingResolver>)> {
        if let Some(bundle) = self.drivers.lock().await.get(&principal.id).cloned() {
            return Some(bundle);
        }

        // Sprint 2 Phase 6: construction is shared with the streaming
        // ingress driver (`principal::child_turns`) — one builder for
        // provider resolution, session-manager wiring, registry key,
        // quota attribution, and persona inheritance (the executor now
        // carries the principal's root agent prompt via
        // `.with_agent_config`, closing the blank-prompt gap).
        let turns = match crate::principal::child_turns::PeerChildTurns::build(
            principal,
            &self.llm_resolver,
            Arc::clone(&self.observability),
            // Channel-driven child turns drain the daemon-shared
            // registry too: a CLI/A2A send steered into the same
            // child mid-run is consumed at the next iteration
            // boundary instead of stalling in the inbox.
            Some(self.principal_manager.shared_inbox_registry()),
        )
        .await
        {
            Ok(turns) => turns,
            Err(e) => {
                warn!(
                    principal = %principal.id,
                    "channel binding: cannot build a turn driver (no resolvable model); \
                     bound channels stay active-only: {e:#}"
                );
                return None;
            }
        };

        let owner_root = turns.parent_session_key().to_string();
        let turn_driver = Arc::new(SubagentResumeDriver::new(
            turns.executor().clone(),
            owner_root.clone(),
        ));
        let binding_resolver = Arc::new(SessionStoreBindingResolver::new(
            Arc::clone(turns.session_manager()),
            owner_root,
        ));

        let bundle = (turn_driver, binding_resolver);
        self.drivers
            .lock()
            .await
            .insert(principal.id.clone(), bundle.clone());
        Some(bundle)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use peko_channel::{ChannelConfig, ChannelStore, CreateOpts};
    use peko_session::SessionCreateOptions;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex as StdMutex;

    fn pid(s: &str) -> PrincipalId {
        PrincipalId(s.to_string())
    }

    fn chan() -> ChannelId {
        ChannelId::generate()
    }

    fn posted(author: &str, text: &str) -> ChannelEvent {
        ChannelEvent::Posted {
            channel: chan(),
            author: author.to_string(),
            parent: None,
            text: text.to_string(),
            at: "2026-08-15T00:00:00Z".to_string(),
        }
    }

    fn posted_reply(author: &str, parent: &str, text: &str) -> ChannelEvent {
        ChannelEvent::Posted {
            channel: chan(),
            author: author.to_string(),
            parent: Some(parent.to_string()),
            text: text.to_string(),
            at: "2026-08-15T00:00:00Z".to_string(),
        }
    }

    fn respond_ctx(
        principal: &PrincipalId,
        channel: &ChannelId,
        event_id: &str,
        event: ChannelEvent,
    ) -> RespondCtx {
        RespondCtx {
            channel: channel.clone(),
            principal: principal.clone(),
            event,
            event_id: event_id.to_string(),
            now: std::time::SystemTime::now(),
        }
    }

    // -- stubs ------------------------------------------------------------

    struct StubDriver {
        calls: Arc<StdMutex<Vec<(String, String)>>>,
        reply: String,
        delay: Duration,
        active: Arc<AtomicUsize>,
        max_active: Arc<AtomicUsize>,
    }

    impl StubDriver {
        fn new(reply: &str) -> (Self, Arc<StdMutex<Vec<(String, String)>>>) {
            let calls = Arc::new(StdMutex::new(Vec::new()));
            (
                Self {
                    calls: Arc::clone(&calls),
                    reply: reply.to_string(),
                    delay: Duration::ZERO,
                    active: Arc::new(AtomicUsize::new(0)),
                    max_active: Arc::new(AtomicUsize::new(0)),
                },
                calls,
            )
        }
    }

    #[async_trait]
    impl BoundTurnDriver for StubDriver {
        async fn drive_turn(&self, session_id: &str, message: &str) -> anyhow::Result<String> {
            let now = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(now, Ordering::SeqCst);
            if self.delay > Duration::ZERO {
                tokio::time::sleep(self.delay).await;
            }
            self.calls
                .lock()
                .unwrap()
                .push((session_id.to_string(), message.to_string()));
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(self.reply.clone())
        }
    }

    struct StubResolver {
        id: String,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl BindingResolver for StubResolver {
        async fn resolve(&self, _binding: &str) -> anyhow::Result<String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.id.clone())
        }
    }

    fn test_store(label: &str) -> (Arc<ChannelStore>, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let _ = label;
        let store = ChannelStore::new(ChannelConfig {
            runtime_dir: tmp.path().to_path_buf(),
            shared_dir: None,
        });
        (Arc::new(store), tmp)
    }

    /// Poll `cond` until it holds or the deadline expires.
    async fn eventually<F, Fut>(cond: F) -> bool
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while tokio::time::Instant::now() < deadline {
            if cond().await {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }

    // -- response_trigger (the anti-loop filter) ---------------------------

    #[test]
    fn response_trigger_ignores_non_posted_events() {
        let principal = pid("prin_self");
        let channel = chan();
        for ev in [
            ChannelEvent::Created {
                channel: channel.clone(),
                creator: "prin_other".into(),
                name: "dm".into(),
                at: "2026-08-15T00:00:00Z".into(),
            },
            ChannelEvent::MemberJoined {
                channel: channel.clone(),
                member: "prin_other".into(),
                at: "2026-08-15T00:00:00Z".into(),
            },
            ChannelEvent::MemberLeft {
                channel,
                member: "prin_other".into(),
                at: "2026-08-15T00:00:00Z".into(),
            },
        ] {
            assert_eq!(
                PassiveBindingResponder::response_trigger(&principal, &ev),
                None
            );
        }
    }

    #[test]
    fn response_trigger_ignores_self_authored_posts() {
        let principal = pid("prin_self");
        // The principal's own reply (posted via ChannelPort::post with
        // sender = principal) comes back with author ==
        // principal.to_string() — the anti-loop invariant depends on
        // this exact-match suppression.
        let ev = posted("prin_self", "my own reply");
        assert_eq!(
            PassiveBindingResponder::response_trigger(&principal, &ev),
            None
        );
    }

    #[test]
    fn response_trigger_accepts_other_members_posts() {
        let principal = pid("prin_self");
        let ev = posted("prin_other", "hello agent");
        assert_eq!(
            PassiveBindingResponder::response_trigger(&principal, &ev),
            Some("hello agent".to_string())
        );
    }

    // -- Phase 11: Subject-wire-form authors are ingress-handler-owned ----

    /// Ingress-handler posts (the Phase 11 inbound convention) carry
    /// `author = peer.to_string()` — the Subject wire form. The
    /// responder must skip them all: `user:*`, `principal:*`, and
    /// `public`. Otherwise the peer-DM channel would double-drive
    /// every turn (handler AND responder).
    #[test]
    fn response_trigger_skips_subject_wire_form_authors() {
        let principal = pid("prin_self");
        for author in [
            "user:alice",
            "user:local",
            "principal:did:peko:principal:abc123",
            "public",
        ] {
            let ev = posted(author, "inbound via ingress handler");
            assert_eq!(
                PassiveBindingResponder::response_trigger(&principal, &ev),
                None,
                "Subject-wire-form author {author} must not trigger a responder turn"
            );
        }
    }

    /// The partition boundary: a raw principal-id form that is NOT the
    /// bound principal (including `@runtime`-decorated mirror forms)
    /// still triggers — those are local cross-principal posts and
    /// Phase 12 A2A fan-out posts.
    #[test]
    fn response_trigger_still_accepts_raw_principal_id_forms() {
        let principal = pid("prin_self");
        for author in ["prin_other", "prin_bob@runtime-B"] {
            let ev = posted(author, "cross-principal post");
            assert_eq!(
                PassiveBindingResponder::response_trigger(&principal, &ev),
                Some("cross-principal post".to_string()),
                "raw principal-id author {author} must trigger a responder turn"
            );
        }
    }

    // -- Phase 12a: root-post-only rule ---------------------------------

    /// A parent-bearing post is a reply — and every responder reply
    /// carries `parent: Some` — so the trigger must drop it even when
    /// the author is another principal's raw id. This is the bit that
    /// makes cross-runtime ping-pong structurally impossible: B's
    /// mirrored reply never wakes A's responder.
    #[test]
    fn response_trigger_skips_parent_bearing_posts() {
        let principal = pid("prin_self");
        let ev = posted_reply("prin_other", "7", "a threaded reply");
        assert_eq!(
            PassiveBindingResponder::response_trigger(&principal, &ev),
            None,
            "replies (parent-bearing posts) must never trigger a turn"
        );
    }

    // -- responder end-to-end (stub driver, real ChannelStore) -------------

    async fn bound_responder(
        label_principal: &str,
        driver: StubDriver,
        resolver: StubResolver,
    ) -> (
        PassiveBindingResponder,
        Arc<ChannelStore>,
        ChannelId,
        PrincipalId,
        tempfile::TempDir,
    ) {
        let (store, tmp) = test_store(label_principal);
        let principal = pid(label_principal);
        let channel = store
            .create(&principal, CreateOpts::runtime("dm"))
            .await
            .unwrap();
        let responder = PassiveBindingResponder::new(
            channel.clone(),
            principal.clone(),
            "/bound".to_string(),
            Arc::clone(&store) as Arc<dyn ChannelPort>,
            Arc::new(resolver),
            Arc::new(driver),
        );
        (responder, store, channel, principal, tmp)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn posted_event_drives_turn_and_posts_reply() {
        let (driver, calls) = StubDriver::new("the reply");
        let resolver = StubResolver {
            id: "session-1".into(),
            calls: Arc::new(AtomicUsize::new(0)),
        };
        let (responder, store, channel, principal, _tmp) =
            bound_responder("prin_self", driver, resolver).await;

        // Write the inbound post to the store first so the reply's
        // `parent` (the triggering event's line id) validates against
        // the log — in production the subscriber only ever hands the
        // responder line ids that exist.
        let other = pid("prin_other");
        store.invite(&channel, &principal, &other).await.unwrap();
        let line = store
            .post(&channel, &other, PostMsg::root("hi"))
            .await
            .unwrap();

        responder
            .consider_response(respond_ctx(
                &principal,
                &channel,
                &line,
                posted("prin_other", "hi"),
            ))
            .await
            .unwrap();

        assert!(eventually(|| async { calls.lock().unwrap().len() == 1 }).await);
        assert_eq!(
            calls.lock().unwrap()[0],
            ("session-1".to_string(), "hi".to_string())
        );

        // The reply lands on the channel as the principal, THREADED
        // onto the triggering event (Phase 12a — the root-post-only
        // trigger rule relies on replies carrying `parent`), and the
        // channel's own event log is the only record (no chat-log
        // projection anywhere on this path).
        let store2 = Arc::clone(&store);
        let channel2 = channel.clone();
        let principal2 = principal.clone();
        let line2 = line.clone();
        assert!(
            eventually(|| async {
                store2
                    .peek(&channel2, &peko_channel::Checkpoint::default())
                    .await
                    .unwrap()
                    .iter()
                    .any(|ev| {
                        matches!(ev, ChannelEvent::Posted { author, text, parent, .. }
                    if *author == principal2.to_string()
                        && text == "the reply"
                        && parent.as_deref() == Some(line2.as_str()))
                    })
            })
            .await,
            "reply must land threaded onto the triggering event"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn self_post_is_suppressed_end_to_end() {
        let (driver, calls) = StubDriver::new("reply");
        let resolver = StubResolver {
            id: "session-1".into(),
            calls: Arc::new(AtomicUsize::new(0)),
        };
        let (responder, _store, channel, principal, _tmp) =
            bound_responder("prin_self", driver, resolver).await;

        // A post authored by the bound principal (i.e. our own reply
        // observed on the next poll tick) must not drive a turn.
        responder
            .consider_response(respond_ctx(
                &principal,
                &channel,
                "1",
                posted("prin_self", "my reply"),
            ))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(calls.lock().unwrap().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn binding_resolves_once_and_is_cached() {
        let (driver, calls) = StubDriver::new("r");
        let resolve_calls = Arc::new(AtomicUsize::new(0));
        let resolver = StubResolver {
            id: "session-1".into(),
            calls: Arc::clone(&resolve_calls),
        };
        let (responder, _store, channel, principal, _tmp) =
            bound_responder("prin_self", driver, resolver).await;

        for text in ["one", "two"] {
            responder
                .consider_response(respond_ctx(
                    &principal,
                    &channel,
                    "1",
                    posted("prin_other", text),
                ))
                .await
                .unwrap();
        }
        assert!(eventually(|| async { calls.lock().unwrap().len() == 2 }).await);
        assert_eq!(
            resolve_calls.load(Ordering::SeqCst),
            1,
            "the binding must resolve once per channel, not once per message"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_messages_serialize_turns() {
        let (mut driver, calls) = StubDriver::new("r");
        driver.delay = Duration::from_millis(100);
        let max_active = Arc::clone(&driver.max_active);
        let resolver = StubResolver {
            id: "session-1".into(),
            calls: Arc::new(AtomicUsize::new(0)),
        };
        let (responder, _store, channel, principal, _tmp) =
            bound_responder("prin_self", driver, resolver).await;

        // Two messages arriving in the same tick: both get a turn, but
        // never concurrently (the second queues behind the first).
        responder
            .consider_response(respond_ctx(
                &principal,
                &channel,
                "1",
                posted("prin_other", "m1"),
            ))
            .await
            .unwrap();
        // Deterministic FIFO: wait until m1's turn has actually STARTED
        // (its task holds the turn mutex) before issuing m2 — spawn
        // order alone doesn't guarantee lock-acquisition order.
        assert!(eventually(|| async { calls.lock().unwrap().len() == 1 }).await);
        responder
            .consider_response(respond_ctx(
                &principal,
                &channel,
                "1",
                posted("prin_other", "m2"),
            ))
            .await
            .unwrap();
        assert!(eventually(|| async { calls.lock().unwrap().len() == 2 }).await);
        assert_eq!(max_active.load(Ordering::SeqCst), 1);
        // Order preserved: the queue is FIFO.
        assert_eq!(calls.lock().unwrap()[0].1, "m1");
        assert_eq!(calls.lock().unwrap()[1].1, "m2");
    }

    /// Two-runtime ping-pong simulation (Phase 12a anti-loop
    /// end-to-end). Two stores stand in for two runtimes' views of the
    /// same DM channel; events are hand-forwarded the way the tunnel
    /// fan-out delivers them. A mirrored ROOT post drives exactly one
    /// turn on the receiving side; the reply — posted with
    /// `parent = triggering line` — is then mirrored back and
    /// observed by the originating side's responder, which must drop
    /// it. Zero turns on replies, exactly one turn per root post.
    #[tokio::test(flavor = "multi_thread")]
    async fn cross_runtime_replies_never_trigger_another_turn() {
        // Runtime A: the channel creator's side.
        let (driver_a, calls_a) = StubDriver::new("reply-from-a");
        let resolver_a = StubResolver {
            id: "session-a".into(),
            calls: Arc::new(AtomicUsize::new(0)),
        };
        let (responder_a, store_a, channel_a, principal_a, _tmp_a) =
            bound_responder("prin_a", driver_a, resolver_a).await;
        // Runtime B: the mirror side.
        let (driver_b, calls_b) = StubDriver::new("reply-from-b");
        let resolver_b = StubResolver {
            id: "session-b".into(),
            calls: Arc::new(AtomicUsize::new(0)),
        };
        let (responder_b, store_b, channel_b, principal_b, _tmp_b) =
            bound_responder("prin_b", driver_b, resolver_b).await;

        // A posts a root post (line 1; Created is line 0).
        let line_a = store_a
            .post(&channel_a, &principal_a, PostMsg::root("hello from A"))
            .await
            .unwrap();

        // The fan-out mirrors it to B (the tunnel's inbound append
        // path). The mirrored event keeps the source's author +
        // timestamp verbatim; B's line numbers are its own.
        let mirrored = ChannelEvent::Posted {
            channel: channel_b.clone(),
            author: principal_a.to_string(),
            parent: None,
            text: "hello from A".to_string(),
            at: "2026-08-19T00:00:00Z".to_string(),
        };
        let line_b = store_b
            .append_remote_event(&channel_b, &mirrored)
            .await
            .unwrap();

        // B's responder observes the mirrored root post: exactly one
        // turn, then a threaded reply on B's log.
        responder_b
            .consider_response(respond_ctx(&principal_b, &channel_b, &line_b, mirrored))
            .await
            .unwrap();
        assert!(eventually(|| async { calls_b.lock().unwrap().len() == 1 }).await);

        let store_b2 = Arc::clone(&store_b);
        let channel_b2 = channel_b.clone();
        let principal_b2 = principal_b.clone();
        assert!(
            eventually(|| async {
                store_b2
                    .peek(&channel_b2, &peko_channel::Checkpoint::default())
                    .await
                    .unwrap()
                    .iter()
                    .any(|ev| {
                        matches!(ev, ChannelEvent::Posted { author, parent, .. }
                            if *author == principal_b2.to_string() && parent.is_some())
                    })
            })
            .await,
            "B's reply must land threaded (parent = Some)"
        );

        // B's reply fans back to A. A's responder observes the
        // parent-bearing post and must NOT drive a turn — the
        // ping-pong vector is closed.
        let reply_events = store_b
            .peek_with_ids(&channel_b, &peko_channel::Checkpoint::default())
            .await
            .unwrap();
        let reply = reply_events
            .into_iter()
            .map(|(_, ev)| ev)
            .find(|ev| {
                matches!(ev, ChannelEvent::Posted { author, parent, .. }
                    if *author == principal_b.to_string() && parent.is_some())
            })
            .expect("B's reply event exists");
        let line_a_reply = store_a
            .append_remote_event(&channel_a, &reply)
            .await
            .unwrap();
        responder_a
            .consider_response(respond_ctx(&principal_a, &channel_a, &line_a_reply, reply))
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(
            calls_b.lock().unwrap().len(),
            1,
            "exactly one turn per root post"
        );
        assert!(
            calls_a.lock().unwrap().is_empty(),
            "a mirrored reply must never trigger a turn"
        );
        // Silence the unused binding: A's root line is what B's mirror
        // diverges from (line numbers are runtime-local by design).
        let _ = line_a;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn empty_reply_posts_nothing() {
        let (driver, _calls) = StubDriver::new("   ");
        let resolver = StubResolver {
            id: "session-1".into(),
            calls: Arc::new(AtomicUsize::new(0)),
        };
        let (responder, store, channel, principal, _tmp) =
            bound_responder("prin_self", driver, resolver).await;

        responder
            .consider_response(respond_ctx(
                &principal,
                &channel,
                "1",
                posted("prin_other", "hi"),
            ))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        let events = store
            .peek(&channel, &peko_channel::Checkpoint::default())
            .await
            .unwrap();
        assert!(
            !events
                .iter()
                .any(|ev| matches!(ev, ChannelEvent::Posted { .. })),
            "empty replies must not be posted; got {events:?}"
        );
    }

    // -- select_responder ---------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn select_responder_noop_for_unbound_channel() {
        let (store, _tmp) = test_store("select-unbound");
        let principal = pid("prin_self");
        let channel = store
            .create(&principal, CreateOpts::runtime("group"))
            .await
            .unwrap();

        // Unbound: Noop even if a driver is somehow available.
        let (driver, calls) = StubDriver::new("r");
        let responder = select_responder(
            channel.clone(),
            principal.clone(),
            Arc::clone(&store) as Arc<dyn ChannelPort>,
            None,
            Some((
                Arc::new(driver),
                Arc::new(StubResolver {
                    id: "s".into(),
                    calls: Arc::new(AtomicUsize::new(0)),
                }),
            )),
        );
        responder
            .consider_response(respond_ctx(
                &principal,
                &channel,
                "1",
                posted("prin_other", "hi"),
            ))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(calls.lock().unwrap().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn select_responder_noop_when_driver_unavailable() {
        let (store, _tmp) = test_store("select-nodriver");
        let principal = pid("prin_self");
        let channel = store
            .create(
                &principal,
                CreateOpts::runtime("dm").with_passive_binding("/user-a"),
            )
            .await
            .unwrap();

        // Bound but the principal's model didn't resolve: Noop (the
        // channel behaves as active-only rather than failing per event).
        let responder = select_responder(
            channel.clone(),
            principal.clone(),
            Arc::clone(&store) as Arc<dyn ChannelPort>,
            Some("/user-a".to_string()),
            None,
        );
        responder
            .consider_response(respond_ctx(
                &principal,
                &channel,
                "1",
                posted("prin_other", "hi"),
            ))
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn select_responder_passive_for_bound_channel_with_driver() {
        let (store, _tmp) = test_store("select-bound");
        let principal = pid("prin_self");
        let channel = store
            .create(
                &principal,
                CreateOpts::runtime("dm").with_passive_binding("/user-a"),
            )
            .await
            .unwrap();

        let (driver, calls) = StubDriver::new("r");
        let responder = select_responder(
            channel.clone(),
            principal.clone(),
            Arc::clone(&store) as Arc<dyn ChannelPort>,
            Some("/user-a".to_string()),
            Some((
                Arc::new(driver),
                Arc::new(StubResolver {
                    id: "session-1".into(),
                    calls: Arc::new(AtomicUsize::new(0)),
                }),
            )),
        );
        responder
            .consider_response(respond_ctx(
                &principal,
                &channel,
                "1",
                posted("prin_other", "hi"),
            ))
            .await
            .unwrap();
        assert!(eventually(|| async { !calls.lock().unwrap().is_empty() }).await);
    }

    // -- SessionStoreBindingResolver (real session store) -------------------

    /// Build a session manager over a tempdir with a root session and a
    /// spawned child carrying slug `user-a` — the standing-child shape
    /// from Phase 2.
    async fn store_with_standing_child() -> (Arc<RwLock<SessionManager>>, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let manager = SessionManager::new()
            .with_sessions_dir_internal(tmp.path().join("sessions"))
            .with_agent_name("test-agent")
            .with_user("alice");
        let manager = Arc::new(RwLock::new(manager));
        let peer = peko_auth::Subject::User("alice".to_string());
        {
            let mut mgr = manager.write().await;
            mgr.create_session(
                "test-agent",
                &peer,
                SessionCreateOptions::new()
                    .with_session_id("root:user:alice")
                    .with_trigger("user"),
            )
            .await
            .unwrap();
            mgr.create_session(
                "test-agent",
                &peer,
                SessionCreateOptions::new()
                    .with_session_id("child-1")
                    .with_parent("root:user:alice")
                    .with_trigger("spawn"),
            )
            .await
            .unwrap();
        }
        {
            let mgr = manager.read().await;
            mgr.set_session_slug("child-1", Some("user-a".to_string()))
                .await
                .unwrap();
        }
        (manager, tmp)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resolver_passes_raw_ids_through() {
        let (manager, _tmp) = store_with_standing_child().await;
        let resolver = SessionStoreBindingResolver::new(manager, "root:user:alice".to_string());
        assert_eq!(resolver.resolve("child-1").await.unwrap(), "child-1");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resolver_resolves_slash_paths() {
        let (manager, _tmp) = store_with_standing_child().await;
        let resolver = SessionStoreBindingResolver::new(manager, "root:user:alice".to_string());
        assert_eq!(resolver.resolve("/user-a").await.unwrap(), "child-1");
        let err = resolver.resolve("/nope").await.unwrap_err();
        assert!(err.to_string().contains("user-a"), "{err}");
    }

    // -- SubagentResumeDriver (real resume path, no provider) ---------------

    #[tokio::test(flavor = "multi_thread")]
    async fn driver_runs_a_real_turn_in_the_bound_session() {
        let (manager, _tmp) = store_with_standing_child().await;
        // Unique agent name → private global registry for this test
        // (mirrors the subagent integration tests' counter pattern).
        static CTR: AtomicUsize = AtomicUsize::new(0);
        let agent_name = format!(
            "channel-binding-test-{}",
            CTR.fetch_add(1, Ordering::Relaxed)
        );
        let executor = SubagentExecutor::new(
            Arc::clone(&manager),
            &agent_name,
            5,
            PrincipalId::generate(),
        );
        let driver = SubagentResumeDriver::new(executor, "root:user:alice".to_string());

        // No provider configured: the resume path short-circuits to
        // its stub completion text (which embeds the task message) and
        // never appends to the child session. The assertion therefore
        // pins guard passage + turn execution + output extraction
        // through the registry wait — the message text flowing into
        // the turn — rather than session writes (those only happen
        // once a real provider drives the loop).
        let reply = driver
            .drive_turn("child-1", "hello from the channel")
            .await
            .unwrap();
        assert!(
            reply.contains("no provider configured"),
            "unexpected reply: {reply}"
        );
        assert!(
            reply.contains("hello from the channel"),
            "the channel message must reach the turn body: {reply}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn driver_surfaces_guard_refusals_as_errors() {
        let (manager, _tmp) = store_with_standing_child().await;
        static CTR2: AtomicUsize = AtomicUsize::new(0);
        let agent_name = format!(
            "channel-binding-test-guard-{}",
            CTR2.fetch_add(1, Ordering::Relaxed)
        );
        let executor = SubagentExecutor::new(
            Arc::clone(&manager),
            &agent_name,
            5,
            PrincipalId::generate(),
        );
        let driver = SubagentResumeDriver::new(executor, "root:user:alice".to_string());

        // The root session itself is not a spawned session — the
        // resume guard refuses it (bound sessions must be spawned).
        let err = driver
            .drive_turn("root:user:alice", "hi")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("spawn"), "{err}");
    }

    // -- restart semantics (Phase 0 cursors + anti-loop across ticks) -------

    /// A simulated daemon restart must not re-fire channel history
    /// through the responder: boot 1's subscriber persists its cursor,
    /// boot 2's subscriber loads it and sees only NEW events. The one
    /// new event here is boot 1's own reply post — which the anti-loop
    /// filter suppresses. This is the full PEKO.md "channel/session
    /// separation" loop-closure invariant exercised end to end.
    #[tokio::test(flavor = "multi_thread")]
    async fn restart_does_not_refire_history() {
        let (store, tmp) = test_store("restart");
        let port = Arc::clone(&store) as Arc<dyn ChannelPort>;
        let principal = pid("prin_self");
        let other = pid("prin_other");
        let channel = store
            .create(
                &principal,
                CreateOpts::runtime("dm").with_passive_binding("/user-a"),
            )
            .await
            .unwrap();
        store.invite(&channel, &principal, &other).await.unwrap();
        store
            .post(&channel, &other, PostMsg::root("before-restart"))
            .await
            .unwrap();

        let channel_dir = tmp.path().join("channels").join(channel.as_str());
        let (driver, calls) = StubDriver::new("the reply");
        let mk_responder = |driver: StubDriver| {
            PassiveBindingResponder::new(
                channel.clone(),
                principal.clone(),
                "/user-a".to_string(),
                Arc::clone(&port),
                Arc::new(StubResolver {
                    id: "session-1".into(),
                    calls: Arc::new(AtomicUsize::new(0)),
                }),
                Arc::new(driver),
            )
        };

        // Boot 1: fresh cursors (first-ever boot starts at offset 0 by
        // design), the pre-restart post drives exactly one turn.
        let mut sub1 = ChannelSubscriber::new(
            channel.clone(),
            principal.clone(),
            channel_dir.clone(),
            Arc::clone(&port),
            Arc::new(mk_responder(driver)),
            peko_channel::cost::noop_meter(),
            ChannelCursors::load(&channel_dir).await.unwrap(),
            SubscriptionConfig::default(),
        );
        sub1.tick_once().await.unwrap();
        assert!(eventually(|| async { calls.lock().unwrap().len() == 1 }).await);
        // Let the detached turn task finish so its reply post is in the
        // log before boot 2 ticks.
        let store2 = Arc::clone(&store);
        let channel2 = channel.clone();
        assert!(eventually(|| async {
            store2
                .peek(&channel2, &peko_channel::Checkpoint::default())
                .await
                .unwrap()
                .iter()
                .any(|ev| matches!(ev, ChannelEvent::Posted { text, .. } if text == "the reply"))
        })
        .await);

        // Boot 2 ("daemon restart"): a fresh subscriber over LOADED
        // cursors. History is not re-delivered; the only new event is
        // boot 1's self-authored reply, which must not drive a turn.
        let (driver2, _calls2) = StubDriver::new("unused");
        let mut sub2 = ChannelSubscriber::new(
            channel.clone(),
            principal.clone(),
            channel_dir.clone(),
            Arc::clone(&port),
            Arc::new(mk_responder(driver2)),
            peko_channel::cost::noop_meter(),
            ChannelCursors::load(&channel_dir).await.unwrap(),
            SubscriptionConfig::default(),
        );
        let delivered = sub2.tick_once().await.unwrap();
        assert_eq!(
            delivered.len(),
            1,
            "only boot 1's reply post is new after restart; got {delivered:?}"
        );
        assert!(matches!(
            delivered[0],
            ChannelEvent::Posted { ref author, .. } if *author == principal.to_string()
        ));
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            calls.lock().unwrap().len(),
            1,
            "self-authored reply observed after restart must not drive a turn"
        );
    }
}
