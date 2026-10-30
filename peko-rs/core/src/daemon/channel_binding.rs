//! Channel subscribers and the group-channel wake (ADR-049 Phase 3,
//! ADR-072).
//!
//! Every (principal, channel) membership gets a [`ChannelSubscriber`]
//! that reads new events. What may start a run is narrow, because
//! principals must never wake each other into a loop (ADR-072): **only
//! humans and cron start runs.**
//!
//! - **DM channels** (`kind: dm` in `meta.json`) get
//!   [`NoopChannelResponder`]. A user's DM is driven by the ingress path
//!   that posted it (`peko send`, Hub, A2A, IPC steer —
//!   `PeerChildTurns::drive_turn_streaming`). A post by another principal
//!   starts nothing: the principal reads it with ChannelRead (its turn
//!   context lists unread posts) or waits for one with ChannelRead
//!   `watch`.
//! - **Group channels** (`kind: group` — every `group:<slug>` and bare
//!   `chan_*` group) get a [`GroupWakeResponder`] when the principal has
//!   a resolvable model. What wakes it is the binding's policy (ADR-082
//!   D3), read per post: `mention` (the default) — a human ROOT post
//!   that mentions the principal (`@<name>`, case-insensitive, whole
//!   word); `human` — every human root post; `off` — nothing.
//!   Principal-authored posts never wake anyone — the responder's own
//!   reply is one of them — and replies (`parent: Some`) never wake.
//!   Everything else is read, not pushed.
//! - **Peer-post nudge** (ADR-086). Every responder is wrapped so a post
//!   by another principal, on a DM or in a group, brings the recipient's
//!   attention-gated `keepalive` forward through the cron engine
//!   ([`PeerPostNudge`]). It starts no run itself: the keepalive fires on
//!   the next poll, at least two minutes after its last turn and within
//!   an hourly budget, and the attention check decides whether it runs.
//!
//! ## The group wake
//!
//! The wake runs in the principal's per-(principal, channel) standing
//! child session (`/group-<slug>`, provisioned via
//! [`crate::principal::peer_children::ensure_group_child`]), driven by
//! [`PeerTurnDriver`] — the same `drive_turn_streaming` call the ingress
//! paths make: no time limit, the session's conversation context, and a
//! stop handle in the daemon's streaming-run registry. The responder
//! posts the turn's final text back to the channel as the principal,
//! threaded onto the triggering post. The turn input is
//! `{author}: {text}` so the principal can tell speakers apart.
//!
//! - **Run concurrency.** `consider_response` spawns the turn as a
//!   detached task so the subscriber's cursor keeps advancing; turns
//!   for one channel serialize on a per-responder mutex, so a second
//!   mention mid-run queues behind the first.
//! - **Run-active collision → queued steering.** When another path
//!   already runs a turn in the session (`err_run_active`), the post is
//!   pushed into the session's steering inbox, which the running loop
//!   drains at its next iteration. The responder then waits for that run
//!   to end, drains any steering it left behind, and drives one successor
//!   turn per leftover message, posting each reply as a root post.
//! - **Failures are log-only**: a broken wake must never post errors into
//!   a channel other members read.
//! - **Restart semantics.** Subscriber cursors are persisted and loaded at
//!   spawn, so a restart does not re-fire channel history. A turn in
//!   flight when the daemon dies is not retried.
//!
//! `peko send group:` (the `ChannelPost` IPC handler) only posts; the
//! subscriber's [`GroupWakeResponder`] is the only driver for group
//! posts.

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
    ChannelSubscriber, ConversationKind, NoopChannelResponder, PostMsg, RespondCtx,
    SubscriptionConfig, WakePolicy,
};
use peko_observability::Observability;
use peko_session::manager::SessionManager;
use peko_session::SteeringMessage;
use peko_subject::{ActorRef, PrincipalId};

use crate::daemon::state::StreamingRunHandle;
use crate::principal::child_turns::PeerChildTurns;
use crate::principal::manager::PrincipalManager;
use crate::principal::Principal;
use crate::session::ownership::is_run_active_error;

/// Poll interval while the collision watch waits for another path's
/// run on the bound session to end.
const COMPLETION_POLL_MS: u64 = 50;

/// The daemon's in-flight streaming-run registry (`AppState::streaming_runs`),
/// keyed by session id; `PrincipalStop` cancels runs through it.
pub(crate) type StreamingRuns = Arc<std::sync::Mutex<HashMap<String, StreamingRunHandle>>>;

// ---------------------------------------------------------------------------
// Turn-driver + binding-resolver seams
// ---------------------------------------------------------------------------

/// Drives one LLM turn in an existing session of the principal's tree
/// and returns the final response text. Trait seam so the responder's
/// decision logic is unit-testable without a provider; the production
/// impl is [`PeerTurnDriver`].
///
/// The two collision-seam methods (`steering_inbox`, `has_active_run`)
/// support the run-active collision path: when `drive_turn` refuses
/// with `err_run_active`, the responder converts the post to queued
/// steering via [`Self::steering_inbox`], polls
/// [`Self::has_active_run`] until the in-flight run ends, and drives
/// successor turns for whatever steering was left undrained. They ride this trait (rather than extra fields on
/// `ResponderInner`) because `ResponderInner` already holds the driver
/// and the probe must stay stubbable in tests. The defaults disable
/// the collision path entirely (the wake keeps the log-only skip).
#[async_trait]
pub(crate) trait BoundTurnDriver: Send + Sync + 'static {
    /// Run a turn in `session_id` (a canonical id — the responder
    /// resolves `/`-paths before calling) with `message` as the user
    /// input. `author` is the human whose post started the turn; the
    /// run acts for them (ADR-078). `None` (queued successors and
    /// completion wakes) continues under the session's natural stamp.
    /// Returns the final response text.
    async fn drive_turn(
        &self,
        session_id: &str,
        message: &str,
        author: Option<&ActorRef>,
    ) -> anyhow::Result<String>;

    /// The daemon-shared steering inbox registry this driver's turns
    /// drain, when bound. `None` (the default) disables the run-active
    /// collision conversion — the wake stays log-only.
    fn steering_inbox(&self) -> Option<Arc<peko_session::InboxRegistry>> {
        None
    }

    /// `true` while a run is in flight for `session_id` — the same
    /// check `drive_turn`'s preflight refuses on, keyed by the same
    /// canonical session id. Default: `false`.
    async fn has_active_run(&self, session_id: &str) -> bool {
        let _ = session_id;
        false
    }
}

/// Resolves the session a channel's turns run in (for a group channel,
/// the principal's per-channel child). Trait seam for the same reason as
/// [`BoundTurnDriver`]; the production impl is [`GroupChildResolver`].
#[async_trait]
pub(crate) trait BindingResolver: Send + Sync + 'static {
    async fn resolve(&self, binding: &str) -> anyhow::Result<String>;
}

// ---------------------------------------------------------------------------
// Shared turn machinery
// ---------------------------------------------------------------------------

/// One channel's turn state: the bound session (resolved once), the
/// turn driver, and the per-channel turn lock.
struct ResponderInner {
    channel: ChannelId,
    principal: PrincipalId,
    /// The raw binding string from `meta.json` (id or `/path`).
    binding: String,
    /// `via` attribution stamped on every reply post this responder
    /// makes: the woken session's slug path, derived from the channel
    /// wire id with the same slug rule as `ensure_group_child`.
    /// Attribution only — never an authority claim.
    via: Option<String>,
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

impl ResponderInner {
    /// One full passive-binding cycle for `text`: resolve the binding
    /// (cached), drive the turn (serialized per channel), post the
    /// reply threaded onto the triggering event (`event_id`). Failures
    /// are log-only: a broken wake must never post errors into a channel
    /// other members read.
    async fn run_turn(&self, event_id: String, text: String, author: Option<ActorRef>) {
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

        match self
            .driver
            .drive_turn(&session_id, &text, author.as_ref())
            .await
        {
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
                // reply fans out to. `via` stamps the bound session's
                // slug path (see the `ResponderInner::via` docs).
                let mut msg = PostMsg::reply(event_id, reply);
                msg.via = self.via.clone();
                if let Err(e) = self
                    .port
                    .post(&self.channel, &ActorRef::from(&self.principal), msg)
                    .await
                {
                    warn!(
                        channel = %self.channel,
                        "channel binding: reply post failed: {e}"
                    );
                }
            }
            Err(e) => {
                // Run-active collision: another path (Agent-tool
                // resume, streaming ingress) is driving a run on the
                // bound session. Convert the wake to queued steering
                // instead of dropping it — see `handle_busy_collision`
                // and the module docs' "Run-active collision" bullet.
                // Every other failure keeps the log-only skip.
                if is_run_active_error(&e) {
                    self.handle_busy_collision(&session_id, &text).await;
                } else {
                    warn!(
                        channel = %self.channel,
                        session = %session_id,
                        "channel binding: turn failed (posting nothing): {e:#}"
                    );
                }
            }
        }
    }

    /// Run-active collision recovery: the bound session already has a
    /// run in flight, so the wake could not drive its own turn. Push
    /// the post into the session's daemon-shared steering inbox — the
    /// running loop drains steering as user-role input at its next
    /// iteration boundary, whichever path drives that run — then
    /// watch for leftovers: steering pushed during the active run's
    /// FINAL iteration would sit undrained forever (the IPC ingress
    /// path's "Gap-2" race, see `drain_pending_steering` in
    /// `ipc::handlers::principal`). Still holding `turn_lock`, so
    /// per-channel ordering is preserved.
    ///
    /// Once the active run ends, atomically drain the inbox and drive
    /// one successor turn per leftover steering message, posting each
    /// reply as a ROOT post — NOT threaded: the original event
    /// correlation is lost once the post was converted to steering, so
    /// there is nothing to thread onto. Principal-authored root posts
    /// are self-suppressed by `response_trigger`, so this is
    /// loop-safe. An empty drain means the active run consumed the
    /// steering itself (the common case) and nothing more happens. The
    /// watch has no deadline: runs have no time limit, and stopping the
    /// active run (`peko stop`, `Async stop`) ends it.
    async fn handle_busy_collision(&self, session_id: &str, text: &str) {
        let Some(inbox_registry) = self.driver.steering_inbox() else {
            warn!(
                channel = %self.channel,
                session = %session_id,
                "channel binding: run-active collision but no steering inbox is bound; \
                 dropping the wake (log-only)"
            );
            return;
        };

        // Same call shape as the IPC ingress path
        // (`PrincipalManager`'s serial-queue fallback).
        inbox_registry
            .get_or_create(session_id)
            .await
            .push(SteeringMessage::new(text.to_string()).into())
            .await;
        debug!(
            channel = %self.channel,
            session = %session_id,
            "channel binding: run-active collision converted to queued steering"
        );

        // Leftover watch: poll until the in-flight run ends.
        while self.driver.has_active_run(session_id).await {
            tokio::time::sleep(Duration::from_millis(COMPLETION_POLL_MS)).await;
        }

        // Atomic drain of STEERING only: IPC-queued and collision-queued
        // steering share the inbox; whoever drains it drives the turn, so
        // there is no double-driving. Completion events stay queued
        // (P0-2, 2026-09-27 — the previous `drain_all` silently destroyed
        // them); they get their own successor turns below. An empty drain
        // is the common case — the active run consumed the steering at its
        // own iteration boundary.
        let Some(inbox) = inbox_registry.peek_inbox(session_id).await else {
            return;
        };
        let leftovers = inbox.drain_steering().await;
        for envelope in leftovers {
            match self
                .driver
                .drive_turn(session_id, &envelope.content, None)
                .await
            {
                Ok(reply) => {
                    let reply = reply.trim();
                    if reply.is_empty() {
                        continue;
                    }
                    let mut msg = PostMsg::root(reply);
                    msg.via = self.via.clone();
                    if let Err(e) = self
                        .port
                        .post(&self.channel, &ActorRef::from(&self.principal), msg)
                        .await
                    {
                        warn!(
                            channel = %self.channel,
                            "channel binding: successor-turn reply post failed: {e}"
                        );
                    }
                }
                Err(e) => {
                    // No re-queue: the steering was consumed by the
                    // drain, and an infinite retry loop is worse than
                    // the log-only skip every other failure gets.
                    warn!(
                        channel = %self.channel,
                        session = %session_id,
                        "channel binding: successor turn failed (posting nothing): {e:#}"
                    );
                }
            }
        }

        // §6.1b: completions that raced the predecessor's final
        // iteration (or landed while the collision watch was waiting)
        // are still queued. Drive chained wake turns — each drains the
        // inbox at its first iteration — until it's empty, posting each
        // reply to the channel like a steering successor. Bounded so a
        // turn that keeps producing completions cannot chain forever.
        let mut chained = 0u8;
        const MAX_POST_RUN_WAKE_CHAIN: u8 = 4;
        while chained < MAX_POST_RUN_WAKE_CHAIN {
            if inbox.is_empty().await {
                break;
            }
            chained += 1;
            match self
                .driver
                .drive_turn(
                    session_id,
                    crate::async_exec::executor::wake::WAKE_TURN_MARKER,
                    None,
                )
                .await
            {
                Ok(reply) => {
                    let reply = reply.trim();
                    if reply.is_empty() {
                        continue;
                    }
                    let mut msg = PostMsg::root(reply);
                    msg.via = self.via.clone();
                    if let Err(e) = self
                        .port
                        .post(&self.channel, &ActorRef::from(&self.principal), msg)
                        .await
                    {
                        warn!(
                            channel = %self.channel,
                            "channel binding: completion-wake reply post failed: {e}"
                        );
                    }
                }
                Err(e) => {
                    warn!(
                        channel = %self.channel,
                        session = %session_id,
                        "channel binding: completion-wake turn failed (posting nothing): {e:#}"
                    );
                    break;
                }
            }
        }
    }
}

/// Detached-task spawn: the subscriber's
/// cursor advance + persistence must not block on an LLM turn (see
/// module docs). The turn lock inside `run_turn` serializes concurrent
/// arrivals.
fn spawn_turn(inner: Arc<ResponderInner>, ctx: RespondCtx, input: String) {
    let author = match &ctx.event {
        ChannelEvent::Posted { author, .. } => ActorRef::from_str(author).ok(),
        _ => None,
    };
    tokio::spawn(async move { inner.run_turn(ctx.event_id, input, author).await });
}

// ---------------------------------------------------------------------------
// GroupWakeResponder (ADR-049 Phase 3, D4)
// ---------------------------------------------------------------------------

/// `ChannelResponder` for group-tier (unbound) channels. See the
/// module docs' "ADR-049 Phase 3" section for the design; the trigger
/// is [`Self::response_trigger`].
///
/// The binding slot carries the channel's wire id and the resolver
/// find-or-creates the per-(principal, channel) group child session; the
/// reply is posted threaded onto the triggering post, as the principal.
#[derive(Clone)]
pub(crate) struct GroupWakeResponder {
    inner: Arc<ResponderInner>,
    /// The principal's name: a user post wakes it only when it mentions
    /// `@<name>`.
    name: String,
}

impl GroupWakeResponder {
    pub(crate) fn new(
        channel: ChannelId,
        principal: PrincipalId,
        name: String,
        port: Arc<dyn ChannelPort>,
        resolver: Arc<dyn BindingResolver>,
        driver: Arc<dyn BoundTurnDriver>,
    ) -> Self {
        // The bound child is `/group-<slug>` under the trunk; derive
        // the path with the same slug rule `ensure_group_child`
        // applies, then add the display-side `sess:` scheme prefix so
        // `via` equals what `compute_path` renders for that session
        // (modulo a `-N` suffix on the rare sanitized-slug collision
        // between two channels — attribution only).
        let via = format!(
            "{}/{}",
            peko_session::path::SCHEME_PREFIX,
            crate::principal::peer_children::group_child_base_slug(channel.as_str())
        );
        Self {
            inner: Arc::new(ResponderInner {
                binding: channel.as_str().to_string(),
                channel,
                principal,
                via: Some(via),
                port,
                resolver,
                driver,
                resolved_session: tokio::sync::OnceCell::new(),
                turn_lock: tokio::sync::Mutex::new(()),
            }),
            name,
        }
    }

    /// The wake filter. Returns the turn input to act on, or `None` to
    /// drop the event. Fires ONLY on a `user:*`-authored ROOT post that
    /// mentions this principal (`@<name>`, case-insensitive, whole word):
    ///
    /// - Only humans start runs (ADR-072): principal-authored posts
    ///   (bare ids AND `principal:<did>` forms) never trigger, so
    ///   principals cannot wake each other into a loop; `public` is not
    ///   a user. The principal's own posts are principal-authored, so
    ///   self-suppression falls out of the same rule.
    /// - `parent.is_none()` — replies never wake (every responder reply
    ///   carries `parent: Some`).
    /// - Unmentioned posts don't wake anyone; the principal reads them
    ///   with ChannelRead (its turn context lists unread posts).
    ///
    /// The input is `{author}: {text}` so the woken session can tell
    /// speakers apart in a multi-party channel (see module docs).
    fn response_trigger(name: &str, event: &ChannelEvent) -> Option<String> {
        Self::human_root_post(event)
            .filter(|(_, text)| peko_channel::mentions(text, name))
            .map(|(author, text)| format!("{author}: {text}"))
    }

    /// A human-authored (user or visitor) root post — what a
    /// [`WakePolicy::Human`] binding wakes on. Principal posts and
    /// replies never qualify, whatever the policy (ADR-072).
    fn human_root_post(event: &ChannelEvent) -> Option<(&str, &str)> {
        match event {
            ChannelEvent::Posted {
                author,
                text,
                parent: None,
                ..
            } if matches!(
                ActorRef::from_str(author),
                Ok(ActorRef::User(_) | ActorRef::Visitor(_))
            ) =>
            {
                Some((author.as_str(), text.as_str()))
            }
            _ => None,
        }
    }
}

#[async_trait]
impl ChannelResponder for GroupWakeResponder {
    async fn consider_response(&self, ctx: RespondCtx) -> peko_channel::Result<()> {
        // ADR-082 D3: read the policy per post, so `Channel wake` takes
        // effect without respawning the subscriber.
        let wake = self
            .inner
            .port
            .binding_for(&self.inner.channel, &self.inner.principal)
            .await?
            .map_or(WakePolicy::Mention, |b| b.wake);
        match wake {
            WakePolicy::Off => return Ok(()),
            WakePolicy::Human => {
                if let Some((author, text)) = Self::human_root_post(&ctx.event) {
                    let input = format!("{author}: {text}");
                    spawn_turn(Arc::clone(&self.inner), ctx, input);
                }
                return Ok(());
            }
            WakePolicy::Mention => {}
        }
        let me = self
            .inner
            .port
            .actor_info(&ActorRef::from(&self.inner.principal))
            .await;
        let roster = self.inner.port.participants(&self.inner.channel).await?;
        // Canonical mentions always identify one actor. Short names work only
        // when the complete principal roster is known and the name is unique.
        let short_is_unique =
            peko_channel::short_name_unambiguous(&roster, &self.name, &me.address);
        let input = Self::response_trigger(&me.address, &ctx.event)
            .or_else(|| {
                me.hub_address()
                    .and_then(|h| Self::response_trigger(&h, &ctx.event))
            })
            .or_else(|| {
                short_is_unique
                    .then(|| Self::response_trigger(&self.name, &ctx.event))
                    .flatten()
            });
        let Some(input) = input else {
            return Ok(());
        };
        spawn_turn(Arc::clone(&self.inner), ctx, input);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// GroupChildResolver (ADR-049 Phase 3)
// ---------------------------------------------------------------------------

/// Production [`BindingResolver`] for the group wake: the "binding" is
/// the channel's wire id and resolution find-or-creates the
/// principal's per-(principal, channel) standing child session
/// (`/group-<slug>`) and its binding record via
/// [`crate::principal::peer_children::ensure_group_binding`]. Idempotent
/// per channel — the responder's `OnceCell` caches the first
/// successful resolution, so provisioning happens at most once per
/// (principal, channel) per daemon lifetime.
pub(crate) struct GroupChildResolver {
    port: Arc<dyn ChannelPort>,
    principal: PrincipalId,
    agent_name: String,
    session_manager: Arc<RwLock<SessionManager>>,
}

impl GroupChildResolver {
    pub(crate) fn new(
        port: Arc<dyn ChannelPort>,
        principal: PrincipalId,
        agent_name: String,
        session_manager: Arc<RwLock<SessionManager>>,
    ) -> Self {
        Self {
            port,
            principal,
            agent_name,
            session_manager,
        }
    }
}

#[async_trait]
impl BindingResolver for GroupChildResolver {
    async fn resolve(&self, binding: &str) -> anyhow::Result<String> {
        let channel = ChannelId::parse(binding)
            .ok_or_else(|| anyhow::anyhow!("not a channel id: {binding}"))?;
        crate::principal::peer_children::ensure_group_binding(
            &self.port,
            &self.principal,
            &self.agent_name,
            &channel,
            &self.session_manager,
        )
        .await
    }
}

// ---------------------------------------------------------------------------
// PeerTurnDriver (production)
// ---------------------------------------------------------------------------

/// Production [`BoundTurnDriver`]: the same turn driver every ingress
/// path uses ([`PeerChildTurns::drive_turn_streaming`]), so a turn a
/// channel post starts gets exactly what a `peko send` turn gets — no
/// time limit, the session's conversation context (peer + DM channel),
/// and a cancellation token registered in the daemon's streaming-run
/// registry, so `peko stop` stops it like any other turn.
pub(crate) struct PeerTurnDriver {
    turns: Arc<PeerChildTurns>,
    /// `None` outside the daemon (tests): turns run without a stop
    /// handle.
    runs: Option<StreamingRuns>,
    principal_name: String,
}

impl PeerTurnDriver {
    pub(crate) fn new(
        turns: Arc<PeerChildTurns>,
        runs: Option<StreamingRuns>,
        principal_name: String,
    ) -> Self {
        Self {
            turns,
            runs,
            principal_name,
        }
    }
}

/// Removes this turn's entry from the streaming-run registry when the
/// turn ends — only if the entry is still this turn's (identified by
/// its `interrupt_acked` handle), never another run's.
struct RunRegistration {
    runs: StreamingRuns,
    session_id: String,
    marker: Arc<tokio::sync::Notify>,
}

impl Drop for RunRegistration {
    fn drop(&mut self) {
        let mut runs = self
            .runs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if runs
            .get(&self.session_id)
            .is_some_and(|h| Arc::ptr_eq(&h.interrupt_acked, &self.marker))
        {
            runs.remove(&self.session_id);
        }
    }
}

#[async_trait]
impl BoundTurnDriver for PeerTurnDriver {
    async fn drive_turn(
        &self,
        session_id: &str,
        message: &str,
        author: Option<&ActorRef>,
    ) -> anyhow::Result<String> {
        let cancel = tokio_util::sync::CancellationToken::new();
        // Register a stop handle unless another run already holds the
        // session's entry (that run's preflight refusal follows below).
        let _registration = self.runs.as_ref().and_then(|runs| {
            let marker = Arc::new(tokio::sync::Notify::new());
            let mut guard = runs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if guard.contains_key(session_id) {
                return None;
            }
            guard.insert(
                session_id.to_string(),
                StreamingRunHandle {
                    principal_name: self.principal_name.clone(),
                    peer: None,
                    cancel: cancel.clone(),
                    interrupt_acked: Arc::clone(&marker),
                },
            );
            Some(RunRegistration {
                runs: Arc::clone(runs),
                session_id: session_id.to_string(),
                marker,
            })
        });
        // ADR-078: a group member's mention passes membership, not a
        // permission check, so the member is also the authority.
        let driver = match author {
            Some(author) => Some(self.turns.driver(author, author).await),
            None => None,
        };
        let acting_for = self
            .turns
            .root_stamp(
                driver.as_ref(),
                session_id,
                peko_tools_core::Origin::Ingress { channel: None },
            )
            .await;
        let outcome = self
            .turns
            .drive_turn_streaming(
                session_id,
                message,
                Arc::new(|_event| {}),
                Some(cancel.clone()),
                None,
                acting_for,
            )
            .await;
        match outcome {
            Ok(outcome) => Ok(outcome.final_text),
            Err(_) if cancel.is_cancelled() => Err(anyhow::anyhow!("stopped by user")),
            Err(e) => Err(e),
        }
    }

    /// The daemon-shared steering inbox the turns drain, so
    /// collision-queued steering lands in the inbox the running loop
    /// drains.
    fn steering_inbox(&self) -> Option<Arc<peko_session::InboxRegistry>> {
        self.turns.executor().inbox_registry()
    }

    /// The same registry check the turn's preflight refuses on.
    async fn has_active_run(&self, session_id: &str) -> bool {
        self.turns
            .executor()
            .has_active_run_for_child(session_id)
            .await
    }
}

// ---------------------------------------------------------------------------
// Peer-post nudge (ADR-086)
// ---------------------------------------------------------------------------

/// What a post by another principal pokes (ADR-086): the daemon's cron
/// engine, which brings the recipient's attention keepalive forward. It
/// starts no run itself, so ADR-072 D1 holds.
#[async_trait]
pub(crate) trait PeerPostNudge: Send + Sync {
    async fn peer_posted(&self, principal: &PrincipalId, channel: &ChannelId);
}

/// The nudge target, bound after construction: the supervisor exists
/// before the cron engine does.
pub(crate) type PeerNudgeSlot = Arc<std::sync::OnceLock<Arc<dyn PeerPostNudge>>>;

/// Wraps a channel's responder: a post by another principal nudges the
/// recipient's keepalive, then the inner responder sees the event as
/// before.
struct PeerPostNudger {
    inner: Arc<dyn ChannelResponder>,
    port: Arc<dyn ChannelPort>,
    slot: PeerNudgeSlot,
}

impl PeerPostNudger {
    /// Whether `ctx.event` is a post by a principal other than the one
    /// this subscriber acts for.
    async fn is_peer_post(&self, ctx: &RespondCtx) -> bool {
        let ChannelEvent::Posted { author, .. } = &ctx.event else {
            return false;
        };
        let Ok(author @ ActorRef::Principal(_)) = ActorRef::from_str(author) else {
            return false;
        };
        let me = ActorRef::from(&ctx.principal);
        self.port.actor_info(&author).await.identity_key
            != self.port.actor_info(&me).await.identity_key
    }
}

#[async_trait]
impl ChannelResponder for PeerPostNudger {
    async fn consider_response(&self, ctx: RespondCtx) -> peko_channel::Result<()> {
        if let Some(nudge) = self.slot.get() {
            if self.is_peer_post(&ctx).await {
                nudge.peer_posted(&ctx.principal, &ctx.channel).await;
            }
        }
        self.inner.consider_response(ctx).await
    }
}

/// Wrap `inner` so peer posts reach the nudge in `slot` once it is set.
pub(crate) fn with_peer_nudge(
    inner: Arc<dyn ChannelResponder>,
    port: Arc<dyn ChannelPort>,
    slot: PeerNudgeSlot,
) -> Arc<dyn ChannelResponder> {
    Arc::new(PeerPostNudger { inner, port, slot })
}

// ---------------------------------------------------------------------------
// select_responder — the per-channel wiring decision
// ---------------------------------------------------------------------------

/// The per-principal driver bundle [`select_responder`] picks from:
/// the shared turn driver plus the resolver that find-or-creates the
/// per-(principal, channel) group child, and the principal's name (what
/// a user mentions to wake it).
pub(crate) struct ResponderDrivers {
    pub(crate) turn: Arc<dyn BoundTurnDriver>,
    pub(crate) group_resolver: Arc<dyn BindingResolver>,
    pub(crate) name: String,
}

/// Pick the responder for one (principal, channel) pair. Pure decision
/// shared by the boot path and the post-boot hooks, and unit-tested
/// here:
///
/// - a DM gets [`NoopChannelResponder`]: a user's DM is
///   driven by the ingress path that posted it, and a post by another
///   principal never starts a run (ADR-072) — the principal reads it
///   with ChannelRead;
/// - a group with a driver bundle yields a [`GroupWakeResponder`]
///   (ADR-049 Phase 3 — bare `chan_*` and `group:<slug>` channels
///   alike), which wakes per the binding's policy (ADR-082 D3);
/// - no driver bundle (the principal has no resolvable model) keeps
///   [`NoopChannelResponder`].
pub(crate) fn select_responder(
    channel: ChannelId,
    principal: PrincipalId,
    port: Arc<dyn ChannelPort>,
    kind: ConversationKind,
    drivers: Option<ResponderDrivers>,
) -> Arc<dyn ChannelResponder> {
    match (kind, drivers) {
        (ConversationKind::Group, Some(drivers)) => Arc::new(GroupWakeResponder::new(
            channel,
            principal,
            drivers.name,
            port,
            drivers.group_resolver,
            drivers.turn,
        )),
        _ => Arc::new(NoopChannelResponder),
    }
}

// ---------------------------------------------------------------------------
// ChannelBindingSupervisor
// ---------------------------------------------------------------------------

/// Owns the per-(principal, channel) `ChannelSubscriber` lifespan:
/// boot-time enumeration ([`Self::spawn_all`]) plus the post-boot
/// hooks ([`Self::ensure_subscriber`], called from `ChannelHost`'s
/// `channel_created` / `ensure_invitee_subscriber` impls on `AppState`).
///
/// Also owns the per-principal [`PeerTurnDriver`] cache — driver
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
    /// Per-principal (turn driver, group resolver) bundles.
    drivers:
        tokio::sync::RwLock<HashMap<PrincipalId, (Arc<PeerTurnDriver>, Arc<GroupChildResolver>)>>,
    /// The daemon's streaming-run registry, so responder turns are
    /// stoppable like ingress turns. `None` outside the daemon.
    streaming_runs: Option<StreamingRuns>,
    /// ADR-086: where peer posts nudge the recipient's keepalive. Set by
    /// [`Self::set_peer_nudge`] once the cron engine exists; subscribers
    /// spawned earlier read it at each event.
    peer_nudge: PeerNudgeSlot,
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
            drivers: tokio::sync::RwLock::new(HashMap::new()),
            streaming_runs: None,
            peer_nudge: Arc::default(),
        }
    }

    /// Bind the peer-post nudge (ADR-086). Only the first call binds.
    pub(crate) fn set_peer_nudge(&self, nudge: Arc<dyn PeerPostNudge>) {
        let _ = self.peer_nudge.set(nudge);
    }

    /// Register responder turns in the daemon's streaming-run registry
    /// (`AppState::streaming_runs`) so `peko stop` reaches them.
    #[must_use]
    pub(crate) fn with_streaming_runs(mut self, runs: StreamingRuns) -> Self {
        self.streaming_runs = Some(runs);
        self
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

        let responder = with_peer_nudge(
            self.responder_for(&principal, &channel).await,
            Arc::clone(&self.port),
            Arc::clone(&self.peer_nudge),
        );
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

    /// Read the channel's kind from `meta.json` and pick the responder.
    /// Unreadable meta degrades to `Noop` with a warning — the
    /// meter-only subscriber still runs.
    async fn responder_for(
        &self,
        principal: &Arc<Principal>,
        channel: &ChannelId,
    ) -> Arc<dyn ChannelResponder> {
        let kind = match self.port.conversation_kind(channel).await {
            Ok(kind) => kind,
            Err(e) => {
                warn!(
                    channel = %channel,
                    "channel subscribers: kind read failed; no wake: {e}"
                );
                return Arc::new(NoopChannelResponder);
            }
        };
        // A DM never wakes the principal from here (see
        // `select_responder`), so it needs no driver.
        if kind == ConversationKind::Dm {
            return Arc::new(NoopChannelResponder);
        }
        let name = principal.config.read().await.name.clone();
        let drivers = self
            .driver_for(principal)
            .await
            .map(|(turn, group_resolver)| ResponderDrivers {
                turn: Arc::clone(&turn) as Arc<dyn BoundTurnDriver>,
                group_resolver: Arc::clone(&group_resolver) as Arc<dyn BindingResolver>,
                name,
            });
        select_responder(
            channel.clone(),
            principal.id.clone(),
            Arc::clone(&self.port),
            kind,
            drivers,
        )
    }

    /// Get or build the per-principal driver bundle. `None` (with a
    /// warning) when the principal has no resolvable model — the same
    /// configuration error that breaks `peko send`; bound channels
    /// then stay active-only until the principal config is fixed and
    /// the daemon restarts.
    ///
    /// The fast path is the read lock; the slow path holds the write
    /// lock across construction so two concurrent misses on the same
    /// principal serialize through `insert` and only the first bundle
    /// is cached. The previous `read → construct → write` pattern
    /// could build the bundle twice under concurrent first-contact,
    /// dropping the first executor on the floor mid-construction
    /// (channels then handed events to a different `SubagentExecutor`
    /// than the cached one — silent per-channel cross-talk).
    async fn driver_for(
        &self,
        principal: &Arc<Principal>,
    ) -> Option<(Arc<PeerTurnDriver>, Arc<GroupChildResolver>)> {
        if let Some(bundle) = self.drivers.read().await.get(&principal.id).cloned() {
            return Some(bundle);
        }

        let mut drivers = self.drivers.write().await;
        // Re-check under the write lock: a concurrent slow-path
        // already built and cached the bundle while we waited.
        if let Some(bundle) = drivers.get(&principal.id).cloned() {
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
            self.principal_manager.tooling(),
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

        let principal_name = principal.config.read().await.name.clone();
        let turns = Arc::new(turns);
        let turn_driver = Arc::new(PeerTurnDriver::new(
            Arc::clone(&turns),
            self.streaming_runs.clone(),
            principal_name,
        ));
        // ADR-049 Phase 3: the group-child resolver shares the
        // principal's session manager; provisioning is keyed by the
        // channel at resolve time.
        let group_resolver = Arc::new(GroupChildResolver::new(
            Arc::clone(&self.port),
            principal.id.clone(),
            turns.agent_name().to_string(),
            Arc::clone(turns.session_manager()),
        ));

        let bundle = (turn_driver, group_resolver);
        drivers.insert(principal.id.clone(), bundle.clone());
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

    /// Sprint 6: convert a test-fixture literal to the v5-derived UUID
    /// form the runtime stores in `SessionMetadata.session_id`.
    fn sid(literal: &str) -> String {
        peko_session::SessionId::from(literal).to_string()
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
            via: None,
            msg_id: None,
            reply_to: None,
            attachments: Vec::new(),
        }
    }

    fn posted_reply(author: &str, parent: &str, text: &str) -> ChannelEvent {
        ChannelEvent::Posted {
            channel: chan(),
            author: author.to_string(),
            parent: Some(parent.to_string()),
            text: text.to_string(),
            at: "2026-08-15T00:00:00Z".to_string(),
            via: None,
            msg_id: None,
            reply_to: None,
            attachments: Vec::new(),
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
        async fn drive_turn(
            &self,
            session_id: &str,
            message: &str,
            _author: Option<&ActorRef>,
        ) -> anyhow::Result<String> {
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

    // -- responder end-to-end (stub driver, real ChannelStore) -------------

    /// A group responder for principal `label_principal`, named `self`
    /// (posts wake it with `@self`), over a fresh store.
    async fn bound_responder(
        label_principal: &str,
        driver: impl BoundTurnDriver,
        resolver: StubResolver,
    ) -> (
        GroupWakeResponder,
        Arc<ChannelStore>,
        ChannelId,
        PrincipalId,
        tempfile::TempDir,
    ) {
        let (store, tmp) = test_store(label_principal);
        let principal = pid(label_principal);
        let channel = store
            .create(&principal, CreateOpts::runtime("group"))
            .await
            .unwrap();
        let responder = GroupWakeResponder::new(
            channel.clone(),
            principal.clone(),
            "self".to_string(),
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
        let other = ActorRef::User("alice".into());
        store.invite(&channel, &principal, &other).await.unwrap();
        let line = store
            .post(&channel, &other, PostMsg::root("@self hi"))
            .await
            .unwrap();

        responder
            .consider_response(respond_ctx(
                &principal,
                &channel,
                &line,
                posted("user:alice", "@self hi"),
            ))
            .await
            .unwrap();

        assert!(eventually(|| async { calls.lock().unwrap().len() == 1 }).await);
        assert_eq!(
            calls.lock().unwrap()[0],
            ("session-1".to_string(), "user:alice: @self hi".to_string())
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
                    if *author == format!("principal:local:{}",principal2.0)
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
                    posted("user:alice", &format!("@self {text}")),
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
                posted("user:alice", "@self m1"),
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
                posted("user:alice", "@self m2"),
            ))
            .await
            .unwrap();
        assert!(eventually(|| async { calls.lock().unwrap().len() == 2 }).await);
        assert_eq!(max_active.load(Ordering::SeqCst), 1);
        // Order preserved: the queue is FIFO.
        assert_eq!(calls.lock().unwrap()[0].1, "user:alice: @self m1");
        assert_eq!(calls.lock().unwrap()[1].1, "user:alice: @self m2");
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
                posted("user:alice", "@self hi"),
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

    // -- run-active collision → steering conversion ---------------------

    /// Stub driver for the collision path: the FIRST `drive_turn`
    /// fails with a caller-supplied error (the collision refusal);
    /// later calls succeed with `reply`. `has_active_run` reports
    /// `active_runs > 0` so the test controls when the "colliding
    /// run" ends, and `steering_inbox` hands out a shared registry
    /// the test can observe.
    struct CollisionStubDriver {
        calls: Arc<StdMutex<Vec<(String, String)>>>,
        reply: String,
        inbox: Arc<peko_session::InboxRegistry>,
        active_runs: Arc<AtomicUsize>,
        first_error: StdMutex<Option<anyhow::Error>>,
    }

    impl CollisionStubDriver {
        fn new(reply: &str, first_error: anyhow::Error) -> Self {
            Self {
                calls: Arc::new(StdMutex::new(Vec::new())),
                reply: reply.to_string(),
                inbox: crate::async_exec::executor::standalone_inbox_registry(),
                active_runs: Arc::new(AtomicUsize::new(0)),
                first_error: StdMutex::new(Some(first_error)),
            }
        }
    }

    #[async_trait]
    impl BoundTurnDriver for CollisionStubDriver {
        async fn drive_turn(
            &self,
            session_id: &str,
            message: &str,
            _author: Option<&ActorRef>,
        ) -> anyhow::Result<String> {
            self.calls
                .lock()
                .unwrap()
                .push((session_id.to_string(), message.to_string()));
            if let Some(err) = self.first_error.lock().unwrap().take() {
                return Err(err);
            }
            Ok(self.reply.clone())
        }

        fn steering_inbox(&self) -> Option<Arc<peko_session::InboxRegistry>> {
            Some(Arc::clone(&self.inbox))
        }

        async fn has_active_run(&self, _session_id: &str) -> bool {
            self.active_runs.load(Ordering::SeqCst) > 0
        }
    }

    /// Collision with no active run by the time the watcher polls:
    /// the post converts to steering, the watcher's first poll sees
    /// the run gone, the drain picks the steering back up, and a
    /// successor turn drives it — with the reply posted as a ROOT
    /// post (the original event correlation is lost in the steering
    /// conversion, so there is nothing to thread onto).
    #[tokio::test(flavor = "multi_thread")]
    async fn run_active_collision_converts_to_steering_and_drives_successor() {
        let driver = CollisionStubDriver::new(
            "the successor reply",
            crate::session::ownership::err_run_active("session-1"),
        );
        let calls = Arc::clone(&driver.calls);
        let inbox_registry = Arc::clone(&driver.inbox);
        let resolver = StubResolver {
            id: "session-1".into(),
            calls: Arc::new(AtomicUsize::new(0)),
        };
        let (responder, store, channel, principal, _tmp) =
            bound_responder("prin_self", driver, resolver).await;

        let other = ActorRef::User("alice".into());
        store.invite(&channel, &principal, &other).await.unwrap();
        let line = store
            .post(&channel, &other, PostMsg::root("@self hi"))
            .await
            .unwrap();

        responder
            .consider_response(respond_ctx(
                &principal,
                &channel,
                &line,
                posted("user:alice", "@self hi"),
            ))
            .await
            .unwrap();

        // Two drive_turn calls: the refused wake, then the successor
        // turn for the leftover steering (same session, same text).
        assert!(eventually(|| async { calls.lock().unwrap().len() == 2 }).await);
        assert_eq!(
            calls.lock().unwrap()[0],
            ("session-1".to_string(), "user:alice: @self hi".to_string())
        );
        assert_eq!(
            calls.lock().unwrap()[1],
            ("session-1".to_string(), "user:alice: @self hi".to_string())
        );

        // The steering round-tripped through the inbox: the watcher
        // drained it, so nothing is left queued.
        let inbox = inbox_registry.get_or_create("session-1").await;
        assert!(
            inbox.drain_all().await.is_empty(),
            "the watcher must have drained the collision steering"
        );

        // The successor reply lands as a ROOT post (parent: None),
        // NOT threaded onto the triggering event.
        let store2 = Arc::clone(&store);
        let channel2 = channel.clone();
        let principal2 = principal.clone();
        assert!(
            eventually(|| async {
                store2
                    .peek(&channel2, &peko_channel::Checkpoint::default())
                    .await
                    .unwrap()
                    .iter()
                    .any(|ev| {
                        matches!(ev, ChannelEvent::Posted { author, text, parent, .. }
                    if *author == format!("principal:local:{}",principal2.0)
                        && text == "the successor reply"
                        && parent.is_none())
                    })
            })
            .await,
            "successor reply must land as a ROOT post"
        );
    }

    /// Collision where the ACTIVE RUN drains the steering itself (the
    /// common case): the watcher finds the inbox empty after the run
    /// ends, drives NO successor, and posts nothing.
    #[tokio::test(flavor = "multi_thread")]
    async fn run_active_collision_drained_by_active_run_posts_nothing() {
        let driver = CollisionStubDriver::new(
            "unused",
            crate::session::ownership::err_run_active("session-1"),
        );
        let calls = Arc::clone(&driver.calls);
        let active_runs = Arc::clone(&driver.active_runs);
        // The colliding run is in flight when the wake hits.
        active_runs.store(1, Ordering::SeqCst);
        let drain_registry = Arc::clone(&driver.inbox);
        let resolver = StubResolver {
            id: "session-1".into(),
            calls: Arc::new(AtomicUsize::new(0)),
        };
        let (responder, store, channel, principal, _tmp) =
            bound_responder("prin_self", driver, resolver).await;

        // Simulate the active run's iteration-boundary drain: wait
        // for the steering to land, drain it, then mark the run
        // finished — the watcher's next poll sees the run gone and
        // its drain comes back empty.
        let drain_active = Arc::clone(&active_runs);
        tokio::spawn(async move {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(inbox) = drain_registry.peek_inbox("session-1").await {
                    if !inbox.drain_all().await.is_empty() {
                        break;
                    }
                }
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "collision steering never landed in the inbox"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            drain_active.store(0, Ordering::SeqCst);
        });

        let other = ActorRef::User("alice".into());
        store.invite(&channel, &principal, &other).await.unwrap();
        let line = store
            .post(&channel, &other, PostMsg::root("@self hi"))
            .await
            .unwrap();
        responder
            .consider_response(respond_ctx(
                &principal,
                &channel,
                &line,
                posted("user:alice", "@self hi"),
            ))
            .await
            .unwrap();

        // The refused wake happened, and the simulated run ended.
        assert!(eventually(|| async { calls.lock().unwrap().len() == 1 }).await);
        assert!(eventually(|| async { active_runs.load(Ordering::SeqCst) == 0 }).await);
        // Give the watcher time to run its (empty) drain and exit.
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(
            calls.lock().unwrap().len(),
            1,
            "no successor turn when the active run drained the steering itself"
        );
        let events = store
            .peek(&channel, &peko_channel::Checkpoint::default())
            .await
            .unwrap();
        assert!(
            !events
                .iter()
                .any(|ev| matches!(ev, ChannelEvent::Posted { author, .. }
                if *author == format!("principal:local:{}", principal.0))),
            "nothing may be posted when the steering was already drained; got {events:?}"
        );
    }

    // -- select_responder ---------------------------------------------------

    /// Build a `ResponderDrivers` bundle from stub parts (the group
    /// resolver is a stub too — provisioning is not what these tests
    /// exercise).
    fn stub_drivers(driver: StubDriver, session_id: &str) -> ResponderDrivers {
        ResponderDrivers {
            turn: Arc::new(driver),
            group_resolver: Arc::new(StubResolver {
                id: session_id.into(),
                calls: Arc::new(AtomicUsize::new(0)),
            }),
            name: "self".to_string(),
        }
    }

    /// ADR-049 Phase 3: an unbound channel with a driver bundle gets
    /// the group wake responder — it fires on `user:*` root posts that
    /// mention the principal, never on principal-authored posts or on
    /// posts that don't mention it.
    #[tokio::test(flavor = "multi_thread")]
    async fn select_responder_group_wake_for_unbound_channel_with_driver() {
        let (store, _tmp) = test_store("select-unbound");
        let principal = pid("prin_self");
        let channel = store
            .create(&principal, CreateOpts::runtime("group"))
            .await
            .unwrap();

        let (driver, calls) = StubDriver::new("r");
        let responder = select_responder(
            channel.clone(),
            principal.clone(),
            Arc::clone(&store) as Arc<dyn ChannelPort>,
            ConversationKind::Group,
            Some(stub_drivers(driver, "session-1")),
        );
        // A principal-authored root post must NOT fire (D4 loop safety).
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
        // A user post that doesn't mention the principal must NOT fire.
        responder
            .consider_response(respond_ctx(
                &principal,
                &channel,
                "2",
                posted("user:alice", "hello group"),
            ))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(calls.lock().unwrap().is_empty());
        // A user post that mentions it MUST fire.
        responder
            .consider_response(respond_ctx(
                &principal,
                &channel,
                "3",
                posted("user:local:alice", "@self hello group"),
            ))
            .await
            .unwrap();
        assert!(eventually(|| async { !calls.lock().unwrap().is_empty() }).await);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn select_responder_noop_when_driver_unavailable() {
        let (store, _tmp) = test_store("select-nodriver");
        let principal = pid("prin_self");
        let channel = store
            .ensure_dm(&principal, &ActorRef::User("a".into()), "/user-a")
            .await
            .unwrap();

        // Bound but the principal's model didn't resolve: Noop (the
        // channel behaves as active-only rather than failing per event).
        let responder = select_responder(
            channel.clone(),
            principal.clone(),
            Arc::clone(&store) as Arc<dyn ChannelPort>,
            ConversationKind::Dm,
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

    /// A bound (DM) channel never wakes the principal from the
    /// subscriber, even with a driver: a user's DM is driven by its
    /// ingress path, and another principal's post never starts a run
    /// (ADR-072).
    #[tokio::test(flavor = "multi_thread")]
    async fn select_responder_noop_for_bound_channel_even_with_driver() {
        let (store, _tmp) = test_store("select-bound");
        let principal = pid("prin_self");
        let channel = store
            .ensure_dm(&principal, &ActorRef::User("a".into()), "/user-a")
            .await
            .unwrap();

        let (driver, calls) = StubDriver::new("r");
        let responder = select_responder(
            channel.clone(),
            principal.clone(),
            Arc::clone(&store) as Arc<dyn ChannelPort>,
            ConversationKind::Dm,
            Some(stub_drivers(driver, "session-1")),
        );
        for (line, author) in [("1", "prin_other"), ("2", "user:alice")] {
            responder
                .consider_response(respond_ctx(
                    &principal,
                    &channel,
                    line,
                    posted(author, "@self hi"),
                ))
                .await
                .unwrap();
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(calls.lock().unwrap().is_empty());
    }

    // -- GroupWakeResponder (ADR-049 Phase 3, D4) ----------------------------

    /// The D4 trigger: fires on `user:*` ROOT posts only. Principal
    /// posts (bare id or `principal:<did>`), `public`, threaded
    /// replies, and non-Posted events never fire.
    #[test]
    fn group_trigger_fires_only_on_user_root_posts() {
        let principal = "self";

        // Fires: a user root post that mentions the principal (input
        // carries the speaker prefix).
        let ev = posted("user:local:alice", "@self hello group");
        assert_eq!(
            GroupWakeResponder::response_trigger(principal, &ev),
            Some("user:local:alice: @self hello group".to_string())
        );
        // Never fires: a user post that doesn't mention it.
        let ev = posted("user:alice", "hello group");
        assert_eq!(GroupWakeResponder::response_trigger(principal, &ev), None);

        // Never fires: principal-authored root posts — the D4
        // loop-safety rule (the responder's own replies are
        // principal-authored, so self-suppression falls out too).
        for author in [
            "prin_other",
            "prin_self",
            "prin_bob@runtime-B",
            "principal:did:peko:principal:abc123",
            "public",
        ] {
            let ev = posted(author, "@self a principal speaking");
            assert_eq!(
                GroupWakeResponder::response_trigger(principal, &ev),
                None,
                "author {author} must not trigger a group wake"
            );
        }

        // Never fires: a user-authored REPLY (threaded).
        let ev = posted_reply("user:alice", "3", "@self threaded follow-up");
        assert_eq!(
            GroupWakeResponder::response_trigger(principal, &ev),
            None,
            "replies (parent-bearing posts) must never trigger a group wake"
        );

        // Never fires: channel bookkeeping events.
        let channel = chan();
        for ev in [
            ChannelEvent::Created {
                channel: channel.clone(),
                creator: "prin_other".into(),
                name: "group".into(),
                at: "2026-08-15T00:00:00Z".into(),
            },
            ChannelEvent::MemberJoined {
                channel: channel.clone(),
                member: "user:alice".into(),
                at: "2026-08-15T00:00:00Z".into(),
            },
            ChannelEvent::MemberLeft {
                channel,
                member: "user:alice".into(),
                at: "2026-08-15T00:00:00Z".into(),
            },
        ] {
            assert_eq!(GroupWakeResponder::response_trigger(principal, &ev), None);
        }
    }

    fn group_responder(
        label_principal: &str,
        driver: StubDriver,
        resolver: StubResolver,
        port: Arc<dyn ChannelPort>,
        channel: ChannelId,
    ) -> GroupWakeResponder {
        GroupWakeResponder::new(
            channel,
            pid(label_principal),
            "self".to_string(),
            port,
            Arc::new(resolver),
            Arc::new(driver),
        )
    }

    /// ADR-086: the responder wrapper nudges on another principal's
    /// posts only: not on the principal's own, nor on a human's (those
    /// reach it through ingress or the group wake).
    #[tokio::test(flavor = "multi_thread")]
    async fn peer_nudge_fires_only_for_other_principals_posts() {
        struct Recorder(std::sync::Mutex<Vec<(PrincipalId, ChannelId)>>);
        #[async_trait]
        impl PeerPostNudge for Recorder {
            async fn peer_posted(&self, principal: &PrincipalId, channel: &ChannelId) {
                self.0
                    .lock()
                    .unwrap()
                    .push((principal.clone(), channel.clone()));
            }
        }
        let (store, _tmp) = test_store("peer-nudge");
        let port = Arc::clone(&store) as Arc<dyn ChannelPort>;
        let me = pid("prin_self");
        let peer = ActorRef::Principal("prin_peer".into());
        let channel = store
            .create(&me, CreateOpts::runtime("crew"))
            .await
            .unwrap();
        store.invite(&channel, &me, &peer).await.unwrap();
        store
            .invite(&channel, &me, &ActorRef::User("alice".into()))
            .await
            .unwrap();

        let recorder = Arc::new(Recorder(std::sync::Mutex::default()));
        let slot: PeerNudgeSlot = Arc::default();
        let responder = with_peer_nudge(
            Arc::new(NoopChannelResponder),
            Arc::clone(&port),
            Arc::clone(&slot),
        );
        let deliver = |author: ActorRef| {
            let store = Arc::clone(&store);
            let channel = channel.clone();
            let me = me.clone();
            let responder = Arc::clone(&responder);
            async move {
                let line = store
                    .post(&channel, &author, PostMsg::root("news"))
                    .await
                    .unwrap();
                let (_, event) = store
                    .peek_with_ids(&channel, &peko_channel::Checkpoint::default())
                    .await
                    .unwrap()
                    .into_iter()
                    .rev()
                    .find(|(id, _)| *id == line)
                    .unwrap();
                responder
                    .consider_response(respond_ctx(&me, &channel, &line, event))
                    .await
                    .unwrap();
            }
        };

        // Before the cron engine binds, nothing is nudged.
        deliver(peer.clone()).await;
        let _ = slot.set(Arc::clone(&recorder) as Arc<dyn PeerPostNudge>);
        deliver(ActorRef::from(&me)).await;
        deliver(ActorRef::User("alice".into())).await;
        assert!(recorder.0.lock().unwrap().is_empty());
        deliver(peer).await;
        assert_eq!(*recorder.0.lock().unwrap(), vec![(me, channel)]);
    }

    /// Group wake end-to-end: a user root post on a group channel
    /// drives exactly one turn in the resolved per-(principal,
    /// channel) session, and the reply lands back on the group as the
    /// principal, threaded onto the triggering post.
    #[tokio::test(flavor = "multi_thread")]
    async fn group_wake_user_post_resumes_session_and_posts_reply() {
        let (store, _tmp) = test_store("group-wake");
        let port = Arc::clone(&store) as Arc<dyn ChannelPort>;
        let principal = pid("prin_self");
        let channel = store
            .create(&principal, CreateOpts::runtime("group"))
            .await
            .unwrap();
        store
            .invite(&channel, &principal, &ActorRef::User("alice".into()))
            .await
            .unwrap();

        let (driver, calls) = StubDriver::new("the group reply");
        let responder = group_responder(
            "prin_self",
            driver,
            StubResolver {
                id: "session-1".into(),
                calls: Arc::new(AtomicUsize::new(0)),
            },
            port,
            channel.clone(),
        );

        // The user posts to the group (Phase 2 path: ChannelPost with
        // a `user:<id>` sender — posted by the handler, driven by
        // nobody else, so the subscriber is the ONLY turn driver).
        let line = store
            .post(
                &channel,
                &ActorRef::User("alice".into()),
                PostMsg::root("@self hello group"),
            )
            .await
            .unwrap();
        let events = store
            .peek_with_ids(&channel, &peko_channel::Checkpoint::default())
            .await
            .unwrap();
        let (_, user_event) = events
            .into_iter()
            .find(|(_, ev)| matches!(ev, ChannelEvent::Posted { author, .. } if author == "user:local:alice"))
            .expect("user post is in the log");

        responder
            .consider_response(respond_ctx(&principal, &channel, &line, user_event))
            .await
            .unwrap();

        // One turn, in the resolved session, with the speaker prefix.
        assert!(eventually(|| async { calls.lock().unwrap().len() == 1 }).await);
        assert_eq!(
            calls.lock().unwrap()[0],
            (
                "session-1".to_string(),
                "user:local:alice: @self hello group".to_string()
            )
        );

        // The reply lands on the group as the principal, THREADED
        // onto the triggering user post.
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
                    if *author == format!("principal:local:{}",principal2.0)
                        && text == "the group reply"
                        && parent.as_deref() == Some(line2.as_str()))
                    })
            })
            .await,
            "reply must land on the group threaded onto the user post"
        );
    }

    /// A failed group-wake turn stays log-only: a broken wake must not
    /// post errors into a channel many members read.
    #[tokio::test(flavor = "multi_thread")]
    async fn group_wake_failure_posts_nothing() {
        let (store, _tmp) = test_store("group-fail");
        let port = Arc::clone(&store) as Arc<dyn ChannelPort>;
        let principal = pid("prin_self");
        let channel = store
            .create(&principal, CreateOpts::runtime("group"))
            .await
            .unwrap();
        store
            .invite(&channel, &principal, &ActorRef::User("alice".into()))
            .await
            .unwrap();
        let driver = CollisionStubDriver::new("unused", anyhow::anyhow!("provider exploded"));
        let calls = Arc::clone(&driver.calls);
        let responder = GroupWakeResponder::new(
            channel.clone(),
            principal.clone(),
            "self".to_string(),
            port,
            Arc::new(StubResolver {
                id: "session-1".into(),
                calls: Arc::new(AtomicUsize::new(0)),
            }),
            Arc::new(driver),
        );
        let line = store
            .post(
                &channel,
                &ActorRef::User("alice".into()),
                PostMsg::root("@self hello group"),
            )
            .await
            .unwrap();
        responder
            .consider_response(respond_ctx(
                &principal,
                &channel,
                &line,
                posted("user:local:alice", "@self hello group"),
            ))
            .await
            .unwrap();
        assert!(eventually(|| async { calls.lock().unwrap().len() == 1 }).await);
        tokio::time::sleep(Duration::from_millis(150)).await;
        let events = store
            .peek(&channel, &peko_channel::Checkpoint::default())
            .await
            .unwrap();
        assert!(
            !events
                .iter()
                .any(|ev| matches!(ev, ChannelEvent::Posted { author, .. }
                if *author == format!("principal:local:{}", principal.0))),
            "a group failure posts nothing; got {events:?}"
        );
    }

    /// A principal-authored root post on the group channel drives NO
    /// turn — the D4 loop-safety invariant end-to-end (the group
    /// responder's own reply must not retrigger it either; that case
    /// is the same author shape).
    #[tokio::test(flavor = "multi_thread")]
    async fn group_wake_ignores_principal_posts_end_to_end() {
        let (store, _tmp) = test_store("group-no-wake");
        let port = Arc::clone(&store) as Arc<dyn ChannelPort>;
        let principal = pid("prin_self");
        let other = pid("prin_other");
        let channel = store
            .create(&principal, CreateOpts::runtime("group"))
            .await
            .unwrap();
        store
            .invite(&channel, &principal, &ActorRef::from(&other))
            .await
            .unwrap();

        let (driver, calls) = StubDriver::new("unused");
        let responder = group_responder(
            "prin_self",
            driver,
            StubResolver {
                id: "session-1".into(),
                calls: Arc::new(AtomicUsize::new(0)),
            },
            port,
            channel.clone(),
        );

        let line = store
            .post(
                &channel,
                &ActorRef::from(&other),
                PostMsg::root("agent talking"),
            )
            .await
            .unwrap();
        responder
            .consider_response(respond_ctx(
                &principal,
                &channel,
                &line,
                posted("prin_other", "agent talking"),
            ))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            calls.lock().unwrap().is_empty(),
            "principal-authored posts must never drive a group wake"
        );
    }

    /// The production [`GroupChildResolver`] find-or-creates one
    /// standing child per channel wire id: idempotent per channel,
    /// distinct across channels (the per-(principal, channel) session
    /// keying, D4).
    #[tokio::test(flavor = "multi_thread")]
    async fn group_child_resolver_keys_sessions_per_channel() {
        let tmp = tempfile::tempdir().unwrap();
        let (store, _store_tmp) = test_store("group-resolver");
        let principal = pid("prin_self");
        for id in ["group:eng-standup", "group:ops"] {
            store
                .create(
                    &principal,
                    CreateOpts::runtime(id).with_id(ChannelId::parse(id).unwrap()),
                )
                .await
                .unwrap();
        }
        let manager = SessionManager::new().with_sessions_dir_internal(tmp.path().join("sessions"));
        let resolver = GroupChildResolver::new(
            Arc::clone(&store) as Arc<dyn ChannelPort>,
            principal.clone(),
            "test-agent".to_string(),
            Arc::new(RwLock::new(manager)),
        );

        let eng = resolver.resolve("group:eng-standup").await.unwrap();
        assert_eq!(
            eng,
            resolver.resolve("group:eng-standup").await.unwrap(),
            "same channel must resolve to the same session"
        );
        let ops = resolver.resolve("group:ops").await.unwrap();
        assert_ne!(eng, ops, "distinct channels must get distinct sessions");
        // ADR-082 D2: provisioning records the binding at the default policy.
        let binding = store
            .binding_for(&ChannelId::parse("group:eng-standup").unwrap(), &principal)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(binding.session, eng);
        assert_eq!(binding.wake, WakePolicy::Mention);
    }

    /// ADR-082 D3: the group wake reads the binding's policy per post.
    #[tokio::test(flavor = "multi_thread")]
    async fn group_wake_follows_the_binding_policy() {
        let (store, _tmp) = test_store("wake-policy");
        let principal = pid("prin_self");
        let channel = store
            .create(&principal, CreateOpts::runtime("group"))
            .await
            .unwrap();
        store.bind(&channel, &principal, "session-1").await.unwrap();
        let (driver, calls) = StubDriver::new("r");
        let responder = select_responder(
            channel.clone(),
            principal.clone(),
            Arc::clone(&store) as Arc<dyn ChannelPort>,
            ConversationKind::Group,
            Some(stub_drivers(driver, "session-1")),
        );

        store
            .set_wake(&channel, &principal, WakePolicy::Off)
            .await
            .unwrap();
        responder
            .consider_response(respond_ctx(
                &principal,
                &channel,
                "1",
                posted("user:local:alice", "@self are you there"),
            ))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(calls.lock().unwrap().is_empty(), "off wakes nothing");

        store
            .set_wake(&channel, &principal, WakePolicy::Human)
            .await
            .unwrap();
        responder
            .consider_response(respond_ctx(
                &principal,
                &channel,
                "2",
                posted("prin_other", "a principal speaking"),
            ))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            calls.lock().unwrap().is_empty(),
            "principal posts never wake, whatever the policy"
        );
        responder
            .consider_response(respond_ctx(
                &principal,
                &channel,
                "3",
                posted("user:local:alice", "no mention needed"),
            ))
            .await
            .unwrap();
        assert!(eventually(|| async { !calls.lock().unwrap().is_empty() }).await);
    }

    // -- real session store ---------------------------------------------------

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
        let peer = peko_auth::ActorRef::User("alice".into());
        let anchor = sid("root:user:alice");
        let child_id_str = sid("child-1");
        {
            let mut mgr = manager.write().await;
            mgr.create_session(
                "test-agent",
                &peer,
                SessionCreateOptions::new()
                    .with_session_id(&anchor)
                    .with_trigger("user"),
            )
            .await
            .unwrap();
            mgr.create_session(
                "test-agent",
                &peer,
                SessionCreateOptions::new()
                    .with_session_id(&child_id_str)
                    .with_parent(&anchor)
                    .with_trigger("spawn"),
            )
            .await
            .unwrap();
        }
        {
            let mgr = manager.read().await;
            mgr.set_session_slug(&child_id_str, Some("user-a".to_string()))
                .await
                .unwrap();
        }
        (manager, tmp)
    }

    // -- PeerTurnDriver (real ingress turn path, no provider) ---------------

    fn peer_turn_driver(
        manager: Arc<RwLock<SessionManager>>,
        runs: Option<StreamingRuns>,
    ) -> PeerTurnDriver {
        // Unique agent name per driver (mirrors the subagent integration
        // tests' counter pattern).
        static CTR: AtomicUsize = AtomicUsize::new(0);
        let agent_name = format!(
            "channel-binding-test-{}",
            CTR.fetch_add(1, Ordering::Relaxed)
        );
        let executor = crate::agents::subagent_executor::SubagentExecutor::new(
            Arc::clone(&manager),
            &agent_name,
            PrincipalId::generate(),
            crate::tools::runtime::ToolingRuntime::standalone(),
        );
        let turns = PeerChildTurns::from_parts(executor, manager, sid("root:user:alice"));
        PeerTurnDriver::new(Arc::new(turns), runs, "self".to_string())
    }

    /// The responder's turn is the ingress turn: it runs in the bound
    /// session with the channel's message, and its stop handle is
    /// registered for the turn and removed after it.
    #[tokio::test(flavor = "multi_thread")]
    async fn driver_runs_a_real_turn_in_the_bound_session() {
        let (manager, _tmp) = store_with_standing_child().await;
        let runs: StreamingRuns = Arc::default();
        let driver = peer_turn_driver(manager, Some(Arc::clone(&runs)));

        // No provider configured: the resume path short-circuits to
        // its stub completion text (which embeds the task message). The
        // assertion pins guard passage + turn execution + output
        // extraction — the message text flowing into the turn.
        let reply = driver
            .drive_turn(&sid("child-1"), "hello from the channel", None)
            .await
            .unwrap();
        assert!(
            reply.contains("hello from the channel"),
            "the channel message must reach the turn body: {reply}"
        );
        assert!(runs.lock().unwrap().is_empty(), "stop handle removed");
    }

    /// Another run's stop handle is neither replaced nor removed by a
    /// responder turn on the same session.
    #[tokio::test(flavor = "multi_thread")]
    async fn driver_leaves_another_runs_stop_handle_alone() {
        let (manager, _tmp) = store_with_standing_child().await;
        let runs: StreamingRuns = Arc::default();
        let theirs = tokio_util::sync::CancellationToken::new();
        runs.lock().unwrap().insert(
            sid("child-1"),
            StreamingRunHandle {
                principal_name: "self".into(),
                peer: None,
                cancel: theirs.clone(),
                interrupt_acked: Arc::new(tokio::sync::Notify::new()),
            },
        );
        let driver = peer_turn_driver(manager, Some(Arc::clone(&runs)));
        let _ = driver.drive_turn(&sid("child-1"), "hi", None).await;
        let runs = runs.lock().unwrap();
        let kept = runs.get(&sid("child-1")).expect("their entry survives");
        theirs.cancel();
        assert!(kept.cancel.is_cancelled(), "still their token");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn driver_surfaces_guard_refusals_as_errors() {
        let (manager, _tmp) = store_with_standing_child().await;
        let driver = peer_turn_driver(manager, None);

        // The root session itself is not a spawned session — the
        // resume guard refuses it (bound sessions must be spawned).
        let err = driver
            .drive_turn(&sid("root:user:alice"), "hi", None)
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
        let other = ActorRef::User("alice".into());
        let channel = store
            .create(&principal, CreateOpts::runtime("group"))
            .await
            .unwrap();
        store.invite(&channel, &principal, &other).await.unwrap();
        store
            .post(&channel, &other, PostMsg::root("@self before-restart"))
            .await
            .unwrap();

        let channel_dir = tmp.path().join("channels").join(channel.as_str());
        let (driver, calls) = StubDriver::new("the reply");
        let mk_responder = |driver: StubDriver| {
            GroupWakeResponder::new(
                channel.clone(),
                principal.clone(),
                "self".to_string(),
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
        assert!(
            eventually(|| async {
                store2
                    .peek(&channel2, &peko_channel::Checkpoint::default())
                    .await
                    .unwrap()
                    .iter()
                    .any(
                        |ev| matches!(ev, ChannelEvent::Posted { text, .. } if text == "the reply"),
                    )
            })
            .await
        );

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
            ChannelEvent::Posted { ref author, .. } if *author == format!("principal:local:{}", principal.0)
        ));
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            calls.lock().unwrap().len(),
            1,
            "self-authored reply observed after restart must not drive a turn"
        );
    }
}
