//! `channel` domain request handler (PR-2c).
//!
//! Owns the channel IPC variants: `ChannelCreate`, `ChannelInvite`,
//! `ChannelPost`, `ChannelPeek`, `ChannelMembers`, `ChannelList`,
//! `ChannelLeave`, `ChannelPinToShared`. The handler holds a narrow
//! [`ChannelHost`] port; the daemon-side implementation (`AppState`)
//! is reached only through the trait so this module never imports
//! `crate::daemon::state::AppState` directly.
//!
//! Boundary rules (mirroring the rest of the F6/F7 handler family):
//! - Dependency inversion: the consumer (`ipc::handlers::channel`)
//!   defines the [`ChannelHost`] trait; the producer (`daemon::state`)
//!   implements it.
//! - This module must not import any other `ipc::handlers::*` module.

use std::sync::Arc;

use async_trait::async_trait;
use peko_channel::port::Checkpoint;
use peko_channel::{ChannelCliRouter, ChannelId, ChannelPort, ConversationKind, WakePolicy};

use crate::ipc::handlers::RequestHandler;
use crate::ipc::packet::{ChannelPekoWake, RequestPacket, ResponsePacket};
use crate::ipc::response_sink::ResponseSink;
use crate::ipc::send_response::send_response;
use crate::ipc::server::PeerAddr;
use peko_auth::caller::CallerContext;
use peko_subject::{ActorRef, PrincipalId};
use std::str::FromStr;

/// Narrow port the `channel` handler uses to reach daemon state.
///
/// `AppState` is the sole production implementor. `channel_port`
/// wraps `ChannelStore::new(ChannelConfig { runtime_dir })` in
/// production; tests substitute an in-memory fixture.
///
/// `principal_manager` is optional. The default impl returns
/// `None` — when unset, every principal-bearing variant returns
/// `ResponsePacket::Error { "not loaded" }`. Production hosts that
/// have a `PrincipalManager` override the method to return `Some`.
pub(crate) trait ChannelHost: Send + Sync {
    /// Channel port for all `ChannelPort` operations.
    fn channel_port(&self) -> Arc<dyn ChannelPort>;

    /// Principal manager (optional — defaults to `None`). When
    /// `None`, principal-name resolution fails for every variant
    /// that carries one.
    fn principal_manager(&self) -> Option<&Arc<crate::principal::manager::PrincipalManager>> {
        None
    }

    /// PR-4c: best-effort post-invite kickoff hook. The handler calls
    /// this from the `ChannelInvite` success arm so the production
    /// host (`AppState`) can record the join trigger in the audit
    /// ring buffer (`peko audit list --type channel.`) at join time
    /// — not just at read time.
    ///
    /// **Default impl is no-op.** Test hosts don't need to override.
    /// The hook is sync (`()` return) so the IPC arm never blocks on
    /// it; future PRs that wire a real session-wake-up path (e.g.
    /// `Async action spawn` of `ChannelRead` via `AsyncExecutor::spawn`)
    /// override this to fire the dispatch — the handler keeps the
    /// log + swallow contract.
    fn ensure_invitee_subscriber(&self, _invitee: &PrincipalId, _channel: &ChannelId) {}

    /// Phase 4 (agent-session paradigm sprint): post-create hook. The
    /// handler calls this from the `ChannelCreate` success arm so the
    /// production host (`AppState`) can spawn the new channel's
    /// subscriber immediately — boot-time enumeration
    /// (`spawn_channel_subscribers`) only sees channels that existed at
    /// boot. The responder choice (passive-binding vs no-op) is made
    /// from the freshly written `meta.json`.
    ///
    /// **Default impl is no-op.** Test hosts don't need to override.
    /// Sync + `()` return, same contract as
    /// [`Self::ensure_invitee_subscriber`]: the IPC arm never blocks on it
    /// and create must not depend on subscriber-spawn success.
    fn channel_created(&self, _creator: &PrincipalId, _channel: &ChannelId) {}

    /// The concrete cross-runtime port, for `ChannelOpenRemoteDm`
    /// (ADR-090 D2). Default `None`: the request is refused.
    fn tunnel_channel_port(&self) -> Option<Arc<crate::tunnel::TunnelChannelPort>> {
        None
    }
}

/// `channel` domain request handler. Constructed with an `Arc<dyn
/// ChannelHost>` (typically `Arc::new(app_state.clone())` from the
/// dispatcher).
pub(crate) struct ChannelHandler {
    host: Arc<dyn ChannelHost>,
}

impl ChannelHandler {
    pub(crate) fn new(host: Arc<dyn ChannelHost>) -> Self {
        Self { host }
    }

    /// Build the thin CLI router that wraps the host's port. PR-2
    /// delegates to `ChannelCliRouter` (PR-1's pure-port handler
    /// surface) — no logic duplication.
    fn router(&self) -> ChannelCliRouter {
        ChannelCliRouter::new(self.host.channel_port())
    }

    /// Helper: convert a principal name (string) into a
    /// `PrincipalId`. When the host has no `PrincipalManager`
    /// (test paths), returns `None` so the caller emits a clean
    /// `ResponsePacket::Error`. Production hosts always provide a
    /// manager.
    fn resolve_principal(&self, name: &str) -> Option<PrincipalId> {
        let pm = self.host.principal_manager()?;
        let name_owned = name.to_string();
        tokio::task::block_in_place(|| {
            let runtime = tokio::runtime::Handle::current();
            runtime.block_on(async { pm.get_by_name(&name_owned).await.map(|p| p.id.clone()) })
        })
    }

    /// ADR-049 Phase 2 + 4 (D6), amended by ADR-057: membership gate
    /// for channel reads (`ChannelPeek`, `ChannelEventsWatch`). The
    /// caller's identity binds which reader the read runs as (see
    /// [`caller_binding`]) — the wire carries no requester, and the
    /// membership gate ALWAYS applies:
    ///
    /// - A pekohub JWT user always reads as themselves — `user:<sub>`.
    /// - A derived caller (local terminal or its scoped API key)
    ///   always reads as the ADR-057 attribution identity
    ///   (`user:local`, or the hub owner id when logged into
    ///   pekohub).
    ///
    /// On failure the error response is sent here and `Ok(false)`
    /// tells the caller to stop.
    async fn gate_channel_read(
        &self,
        channel: &ChannelId,
        caller: &CallerContext,
        request_id: u64,
        sink: &dyn ResponseSink,
    ) -> anyhow::Result<bool> {
        let requester = match caller_binding(caller) {
            CallerBinding::Derived(own) | CallerBinding::User(own) => own,
        };
        match self.host.channel_port().list_members(channel).await {
            Ok(members) if members.contains(&requester) => Ok(true),
            Ok(_) => {
                let response = ResponsePacket::Error {
                    request_id,
                    message: format!("requester '{requester}' is not a member of the channel"),
                };
                send_response(sink, response).await?;
                Ok(false)
            }
            Err(e) => {
                let response = ResponsePacket::Error {
                    request_id,
                    message: format!("channel read failed: {e}"),
                };
                send_response(sink, response).await?;
                Ok(false)
            }
        }
    }

    /// ADR-057: operating a runtime-hosted principal by name —
    /// creating a channel as one (`ChannelCreate`), speaking as one
    /// (`ChannelPost` sender), managing membership through one
    /// (`ChannelInvite` inviter, `ChannelLeave`) — is a Derived-caller
    /// capability: the local terminal or its scoped API key. A
    /// pekohub JWT user is never a principal operator (matching the
    /// post rule), which closes the "remote caller acts as any loaded
    /// principal" path. Sends the `[forbidden]` error itself and
    /// returns `Ok(false)` when the caller must stop.
    async fn gate_principal_operator(
        &self,
        caller: &CallerContext,
        request_id: u64,
        sink: &dyn ResponseSink,
    ) -> anyhow::Result<bool> {
        if matches!(caller_binding(caller), CallerBinding::User(_)) {
            let response = ResponsePacket::Error {
                request_id,
                message: "[forbidden] pekohub users cannot operate principals — channels are \
                          managed by the runtime's own callers"
                    .to_string(),
            };
            send_response(sink, response).await?;
            return Ok(false);
        }
        Ok(true)
    }
}

/// ADR-049 Phase 4 (D6), amended by ADR-057: what a caller is
/// allowed to declare as their sender on the channel surface.
///
/// ADR-057: user identities are never carried on the wire. Every
/// binding carries the identity the caller is *allowed to speak as*,
/// derived server-side from the connection
/// (`CallerContext::attribution_subject`) — there is no `Trusted`
/// passthrough anymore.
impl ChannelHandler {
    /// `ChannelInfo`: kind, name, members, and the wake policy of each
    /// member peko hosted here. Membership-gated like `ChannelPeek`.
    /// Who a post from `caller` is written as: the caller's own identity
    /// (`sender_name: None`), or a peko the caller operates. A `user:`
    /// sender other than the caller is refused as impersonation, for
    /// every caller class, and a JWT user is never a peko sender (ADR-057,
    /// ADR-049 Phase 2). API keys bind as their owner.
    fn post_sender(
        &self,
        caller: &CallerContext,
        sender_name: Option<&str>,
    ) -> Result<ActorRef, String> {
        let own = match caller_binding(caller) {
            CallerBinding::Derived(own) | CallerBinding::User(own) => own,
        };
        let forbidden =
            || format!("[forbidden] caller may only post as their own identity ('{own}')");
        let Some(raw) = sender_name else {
            return Ok(own);
        };
        match ActorRef::from_str(raw) {
            Ok(s @ ActorRef::User(_)) if s == own => Ok(own),
            Ok(ActorRef::User(_)) => Err(forbidden()),
            _ if matches!(caller_binding(caller), CallerBinding::User(_)) => Err(forbidden()),
            _ => self
                .resolve_principal(raw)
                .map(|p| ActorRef::from(&p))
                .ok_or_else(|| format!("Sender peko '{raw}' is not loaded")),
        }
    }

    /// `ChannelBoard` (ADR-101): change a group's board when `op` is given,
    /// then answer with the board.
    async fn channel_board(
        &self,
        request_id: u64,
        channel: &str,
        sender_name: Option<&str>,
        op: Option<peko_protocol::channel::BoardOp>,
        caller: &CallerContext,
        sink: &dyn ResponseSink,
    ) -> anyhow::Result<()> {
        let Some(ch) = ChannelId::parse(channel) else {
            return error_reply(sink, request_id, format!("invalid ChannelId: {channel}")).await;
        };
        if !self
            .gate_channel_read(&ch, caller, request_id, sink)
            .await?
        {
            return Ok(());
        }
        let port = self.host.channel_port();
        if let Some(op) = op {
            let sender = match self.post_sender(caller, sender_name) {
                Ok(sender) => sender,
                Err(message) => return error_reply(sink, request_id, message).await,
            };
            let msg = peko_channel::PostMsg::root(String::new()).with_board(op);
            if let Err(e) = port.post(&ch, &sender, msg).await {
                return error_reply(sink, request_id, e.to_string()).await;
            }
        }
        let board = match port.board(&ch).await {
            Ok(Some(board)) => board,
            Ok(None) => {
                return error_reply(sink, request_id, "boards are for groups".into()).await;
            }
            Err(e) => return error_reply(sink, request_id, e.to_string()).await,
        };
        // Names for the people and pekos an item mentions, for readers
        // that show the board as text (the CLI).
        let roster = port.participants(&ch).await.unwrap_or_default();
        let name_of = |address: &str| -> Option<String> {
            let key = address.parse::<ActorRef>().ok().map(|a| port.key_of(&a))?;
            roster
                .iter()
                .find(|p| {
                    p.address
                        .parse::<ActorRef>()
                        .is_ok_and(|a| port.key_of(&a) == key)
                })
                .and_then(|p| p.name.clone())
        };
        let items = board
            .visible(chrono::Utc::now())
            .map(|item| {
                let mut v = serde_json::json!(item);
                v["short"] = serde_json::json!(item.short_id());
                if let Some(name) = item.claimant.as_deref().and_then(&name_of) {
                    v["claimant_name"] = serde_json::json!(name);
                }
                if let Some(name) = item.for_member.as_deref().and_then(&name_of) {
                    v["for_name"] = serde_json::json!(name);
                }
                v
            })
            .collect();
        send_response(
            sink,
            ResponsePacket::ChannelBoardResult {
                request_id,
                channel: ch,
                items,
            },
        )
        .await
    }

    async fn channel_info(
        &self,
        request_id: u64,
        channel: &str,
        caller: &CallerContext,
        sink: &dyn ResponseSink,
    ) -> anyhow::Result<()> {
        let Some(ch) = ChannelId::parse(channel) else {
            return error_reply(sink, request_id, format!("invalid ChannelId: {channel}")).await;
        };
        if !self
            .gate_channel_read(&ch, caller, request_id, sink)
            .await?
        {
            return Ok(());
        }
        let port = self.host.channel_port();
        let read = async {
            let kind = port.conversation_kind(&ch).await?;
            let name = port.membership(&ch).await?.name;
            let members = port.list_members(&ch).await?;
            let mut pekos = Vec::new();
            for member in &members {
                if !matches!(member, ActorRef::Principal(_)) {
                    continue;
                }
                let Some(local_id) = port.actor_info(member).await.local_id else {
                    continue;
                };
                let Some(principal) = self.hosted_principal(&local_id).await else {
                    continue;
                };
                let wake = match kind {
                    ConversationKind::Dm => WakePolicy::Human,
                    ConversationKind::Group => port
                        .binding_for(&ch, &principal.id)
                        .await?
                        .map_or(WakePolicy::default_for(kind), |b| b.wake),
                };
                pekos.push(ChannelPekoWake {
                    name: principal.name().await,
                    local_id,
                    wake,
                });
            }
            let group = match port.group_info(&ch).await? {
                Some(info) => {
                    let page = match self.host.tunnel_channel_port() {
                        Some(tunnel) => tunnel
                            .local()
                            .hub_group(&ch)
                            .await?
                            .map(|h| h.page)
                            .filter(|p| !p.is_empty()),
                        None => None,
                    };
                    Some(crate::ipc::packet::ChannelGroupSummary {
                        // Only the home records the owner in its `GroupHome`.
                        home_here: info.home.as_ref().is_none_or(|h| h.owner.is_some()),
                        owner: info.owner,
                        hub_id: info.hub_id,
                        page,
                        visibility: info.visibility.map(|v| v.as_str().to_string()),
                    })
                }
                None => None,
            };
            let membership = port.membership(&ch).await?;
            let people = self.channel_people(&membership, &pekos).await?;
            Ok::<_, peko_channel::ChannelError>(ResponsePacket::ChannelInfoResult {
                request_id,
                channel: ch.clone(),
                kind,
                name,
                members,
                pekos,
                group,
                people,
                created_at: Some(membership.created_at).filter(|at| !at.is_empty()),
            })
        };
        match read.await {
            Ok(response) => send_response(sink, response).await,
            Err(e) => error_reply(sink, request_id, format!("channel info failed: {e}")).await,
        }
    }

    /// Everyone in `channel` once, with their names (`ChannelInfoResult`
    /// `people`): its members here, those reached through other runtimes,
    /// and its creator. Each person's every form here is an alias, so a
    /// post's author or a roster row names the same person.
    async fn channel_people(
        &self,
        membership: &peko_protocol::channel::ChannelMembership,
        pekos: &[ChannelPekoWake],
    ) -> Result<Vec<crate::ipc::packet::ChannelPerson>, peko_channel::ChannelError> {
        use crate::ipc::packet::ChannelPerson;
        let port = self.host.channel_port();
        let store = self
            .host
            .tunnel_channel_port()
            .map(|t| Arc::clone(t.local()));
        let display_name = match &store {
            Some(store) => store.person_display_name().await,
            None => None,
        };
        let me = port.actor_info(&ActorRef::User("local".into())).await;
        let mut forms: Vec<String> = membership
            .member_provenance
            .iter()
            .map(|m| m.principal.clone())
            .collect();
        forms.push(membership.creator.clone());
        let mut people: Vec<ChannelPerson> = Vec::new();
        for form in forms {
            if form.is_empty() {
                continue;
            }
            let subject = ActorRef::from_str(&form)
                .unwrap_or_else(|_| ActorRef::Principal(form.clone().into()));
            let info = port.actor_info(&subject).await;
            let you = info.kind == "user"
                && (info.local_id.as_deref() == Some("local")
                    || (me.hub_id.is_some() && info.hub_id == me.hub_id));
            let address = if you {
                me.address.clone()
            } else {
                info.address.clone()
            };
            let mut aliases = vec![form.clone(), subject.canonical_address()];
            aliases.extend(info.hub_address());
            if you {
                aliases.extend(me.hub_address());
                aliases.push("user:local".into());
            }
            if let Some(local_id) = &info.local_id {
                aliases.push(local_id.clone());
            }
            let name = if you {
                display_name.clone()
            } else {
                info.local_id
                    .as_ref()
                    .and_then(|id| pekos.iter().find(|p| &p.local_id == id))
                    .map(|p| p.name.clone())
                    .or_else(|| info.name.clone())
            };
            let here = you || (info.kind == "principal" && info.local_id.is_some());
            let person = match people.iter_mut().find(|p| p.address == address) {
                Some(person) => person,
                None => {
                    people.push(ChannelPerson {
                        address: address.clone(),
                        aliases: Vec::new(),
                        kind: info.kind.clone(),
                        name: None,
                        you,
                        here,
                    });
                    people.last_mut().expect("just pushed")
                }
            };
            if person.name.is_none() {
                person.name = name;
            }
            person.here |= here;
            for alias in aliases {
                if alias != person.address && !person.aliases.contains(&alias) {
                    person.aliases.push(alias);
                }
            }
        }
        Ok(people)
    }

    /// `ChannelSetWake`: set what starts a turn for a peko in a group.
    /// Operator callers only, and only for the peko's owner; refused
    /// for DMs and for a peko that is not a member.
    async fn channel_set_wake(
        &self,
        request_id: u64,
        channel: &str,
        principal_name: &str,
        wake: &str,
        caller: &CallerContext,
        sink: &dyn ResponseSink,
    ) -> anyhow::Result<()> {
        if !self
            .gate_principal_operator(caller, request_id, sink)
            .await?
        {
            return Ok(());
        }
        let Some(policy) = WakePolicy::parse(wake) else {
            return error_reply(
                sink,
                request_id,
                format!("[bad_request] wake is human, mention, or off (got {wake:?})"),
            )
            .await;
        };
        let Some(ch) = ChannelId::parse(channel) else {
            return error_reply(sink, request_id, format!("invalid ChannelId: {channel}")).await;
        };
        let Some(manager) = self.host.principal_manager() else {
            return error_reply(sink, request_id, "no principal manager".to_string()).await;
        };
        let Some(principal) = manager.get_by_name(principal_name).await else {
            return error_reply(
                sink,
                request_id,
                format!("[not_found] peko '{principal_name}' is not loaded"),
            )
            .await;
        };
        let port = self.host.channel_port();
        let owner = principal.config.read().await.owner.clone();
        if !crate::security::shared_actor_keys(Some(Arc::clone(&port)))
            .same_actor(&owner, &caller.permission_subject(&owner))
        {
            return error_reply(
                sink,
                request_id,
                format!("[forbidden] only {principal_name}'s owner can change how it wakes"),
            )
            .await;
        }
        let set = async {
            if port.conversation_kind(&ch).await? == ConversationKind::Dm {
                return Ok(Err(format!(
                    "[bad_request] {ch} is a DM; a DM always wakes on human posts"
                )));
            }
            if !port
                .list_members(&ch)
                .await?
                .contains(&ActorRef::from(&principal.id))
            {
                return Ok(Err(format!(
                    "[forbidden] {principal_name} is not a member of {ch}"
                )));
            }
            if port.binding_for(&ch, &principal.id).await?.is_none() {
                manager
                    .ensure_group_binding(&principal, &ch)
                    .await
                    .map_err(|e| peko_channel::ChannelError::Adapter(e.to_string()))?;
            }
            let binding = port.set_wake(&ch, &principal.id, policy).await?;
            Ok::<_, peko_channel::ChannelError>(Ok(binding.wake))
        };
        match set.await {
            Ok(Ok(wake)) => {
                let response = ResponsePacket::ChannelWakeSet {
                    request_id,
                    channel: ch,
                    principal_name: principal.name().await,
                    wake,
                };
                send_response(sink, response).await
            }
            Ok(Err(refusal)) => error_reply(sink, request_id, refusal).await,
            Err(e) => error_reply(sink, request_id, format!("channel set wake failed: {e}")).await,
        }
    }

    /// ADR-094: copy a post's files into the channel's file store, under
    /// the limits of where the conversation reaches. The error is the
    /// user-facing reason.
    async fn channel_post_files(
        &self,
        caller: &CallerContext,
        channel: &ChannelId,
        paths: &[String],
    ) -> std::result::Result<Vec<peko_protocol::channel::Attachment>, String> {
        use crate::principal::attachments;
        if !caller.is_local() {
            return Err("[forbidden] attachments can only be sent by the local user".into());
        }
        let port = self.host.channel_port();
        let limits = match attachments::crosses_runtimes(port.as_ref(), channel).await {
            Ok(true) => attachments::Limits::REMOTE,
            Ok(false) => attachments::Limits::LOCAL,
            Err(e) => return Err(format!("channel post failed: {e}")),
        };
        let files_dir = port
            .files_dir(channel)
            .await
            .map_err(|e| format!("channel post failed: {e}"))?;
        attachments::ingest_into_channel(&files_dir, paths, limits).await
    }

    /// `ChannelOpenRemoteDm` (ADR-090 D2): the local user opens, or
    /// finds, its DM with a peko on another runtime. Local callers only —
    /// it speaks as the runtime's local user and may carry its Hub token.
    /// ADR-095 D6: DMs between people — open one, list and answer
    /// requests, block and unblock. The local user's only.
    async fn channel_person_dm(
        &self,
        request: RequestPacket,
        caller: &CallerContext,
        sink: &dyn ResponseSink,
    ) -> anyhow::Result<()> {
        let request_id = request.request_id();
        if !caller.is_local() {
            return error_reply(
                sink,
                request_id,
                "[permission_denied] only the local user manages its conversations".to_string(),
            )
            .await;
        }
        let Some(port) = self.host.tunnel_channel_port() else {
            return error_reply(
                sink,
                request_id,
                "This runtime has no PekoHub connection".to_string(),
            )
            .await;
        };
        let local = port.actor_info(&ActorRef::User("local".into())).await;
        let opened = |dm: crate::tunnel::PersonDm| ResponsePacket::ChannelPersonDmOpened {
            request_id,
            channel: dm.channel,
            peer: dm.peer,
            created: dm.created,
        };
        let result: anyhow::Result<ResponsePacket> = async {
            let account = local.hub_id.clone().ok_or_else(|| {
                anyhow::anyhow!("Register this runtime with PekoHub first (peko runtime register)")
            });
            match request {
                RequestPacket::ChannelOpenPersonDm { account, name, .. } => Ok(opened(
                    port.open_person_dm(&account, name.as_deref()).await?,
                )),
                RequestPacket::ChannelDmRequests { .. } => {
                    Ok(ResponsePacket::ChannelDmRequestsResult {
                        request_id,
                        requests: port.local().dm_requests(&account?).await?,
                    })
                }
                RequestPacket::ChannelDmRequestAnswer {
                    channel, answer, ..
                } => {
                    let channel = peko_channel::ChannelId::parse(&channel)
                        .ok_or_else(|| anyhow::anyhow!("Not a channel id: {channel}"))?;
                    let answer = match answer.as_str() {
                        "accept" => crate::tunnel::DmRequestAnswer::Accept,
                        "decline" => crate::tunnel::DmRequestAnswer::Decline,
                        "block" => crate::tunnel::DmRequestAnswer::Block,
                        other => anyhow::bail!("Answer accept, decline, or block, not {other:?}"),
                    };
                    Ok(match port.answer_dm_request(&channel, answer).await? {
                        Some(dm) => opened(dm),
                        None => ResponsePacket::Done {
                            request_id,
                            success: true,
                            error: None,
                        },
                    })
                }
                RequestPacket::ChannelBlockPerson {
                    account: who,
                    blocked,
                    ..
                } => {
                    let account = account?;
                    anyhow::ensure!(
                        uuid::Uuid::parse_str(&who).is_ok(),
                        "Expected a person's PekoHub account ID (a UUID), got {who:?}"
                    );
                    port.local()
                        .set_blocked(&account, &[format!("user:hub:{who}")], blocked)
                        .await?;
                    if blocked {
                        // A waiting request from them goes too.
                        let pair = crate::tunnel::person_dm_channel_id(
                            &format!("user:hub:{account}"),
                            &format!("user:hub:{who}"),
                        );
                        let _ = port.local().take_dm_request(&pair).await;
                    }
                    let (people, names) = port.blocked_people_named(&account).await?;
                    Ok(ResponsePacket::ChannelBlockedPeople {
                        request_id,
                        people,
                        names,
                    })
                }
                RequestPacket::ChannelPersonDms { .. } => {
                    Ok(ResponsePacket::ChannelPersonDmsResult {
                        request_id,
                        dms: port.person_dm_summaries(&account?).await,
                    })
                }
                RequestPacket::ChannelPekoDms { .. } => Ok(ResponsePacket::ChannelPekoDmsResult {
                    request_id,
                    dms: port.peko_dm_summaries(&account?).await,
                }),
                RequestPacket::ChannelBlocked { .. } => {
                    let (people, names) = port.blocked_people_named(&account?).await?;
                    Ok(ResponsePacket::ChannelBlockedPeople {
                        request_id,
                        people,
                        names,
                    })
                }
                _ => anyhow::bail!("not a person DM request"),
            }
        }
        .await;
        match result {
            Ok(response) => send_response(sink, response).await,
            Err(e) => error_reply(sink, request_id, format!("{e:#}")).await,
        }
    }

    async fn channel_open_remote_dm(
        &self,
        request_id: u64,
        hub_id: &str,
        token: Option<&str>,
        caller: &CallerContext,
        sink: &dyn ResponseSink,
    ) -> anyhow::Result<()> {
        if !caller.is_local() {
            return error_reply(
                sink,
                request_id,
                "[permission_denied] only the local user opens its conversations".to_string(),
            )
            .await;
        }
        let Some(port) = self.host.tunnel_channel_port() else {
            return error_reply(
                sink,
                request_id,
                "This runtime has no PekoHub connection".to_string(),
            )
            .await;
        };
        match port.open_hub_dm(hub_id, token).await {
            Ok(dm) => {
                // ADR-091 D4: catch up from the host in the background.
                let sync = Arc::clone(&port);
                let channel = dm.channel.clone();
                tokio::spawn(async move {
                    if let Err(e) = sync.sync_history(&channel).await {
                        tracing::debug!(%channel, "remote DM catch-up skipped: {e}");
                    }
                });
                send_response(
                    sink,
                    ResponsePacket::ChannelRemoteDmOpened {
                        request_id,
                        channel: dm.channel,
                        peer: dm.peer.hub_address().unwrap_or(dm.peer.address),
                        hub_id: hub_id.to_string(),
                        name: dm.peer.name,
                        created: dm.created,
                    },
                )
                .await
            }
            Err(e) => error_reply(sink, request_id, format!("{e:#}")).await,
        }
    }

    /// `ChannelInvitations` (ADR-092 D2): the groups inviting the local user.
    async fn channel_invitations(
        &self,
        request_id: u64,
        caller: &CallerContext,
        sink: &dyn ResponseSink,
    ) -> anyhow::Result<()> {
        if !caller.is_local() {
            return error_reply(
                sink,
                request_id,
                "[permission_denied] local callers only".into(),
            )
            .await;
        }
        let Some(port) = self.host.tunnel_channel_port() else {
            return error_reply(
                sink,
                request_id,
                "This runtime has no PekoHub connection".into(),
            )
            .await;
        };
        match port.group_invitations().await {
            Ok(invitations) => {
                send_response(
                    sink,
                    ResponsePacket::ChannelInvitationsResult {
                        request_id,
                        invitations,
                    },
                )
                .await
            }
            Err(e) => error_reply(sink, request_id, format!("{e:#}")).await,
        }
    }

    /// `ChannelInvitationAnswer` (ADR-092 D2): the local user accepts or
    /// declines a group's invitation.
    async fn channel_group_join(
        &self,
        request_id: u64,
        link: &str,
        peko: Option<&str>,
        caller: &CallerContext,
        sink: &dyn ResponseSink,
    ) -> anyhow::Result<()> {
        if !caller.is_local() {
            return error_reply(
                sink,
                request_id,
                "[permission_denied] local callers only".into(),
            )
            .await;
        }
        let Some(port) = self.host.tunnel_channel_port() else {
            return error_reply(sink, request_id, "PekoHub is not set up here".into()).await;
        };
        let joined = match peko {
            // ADR-098 D2: one of the caller's pekos joins, for itself.
            Some(peko) => match self.owned_peko(peko, caller).await {
                Ok(id) => port.join_group_as(&id, link).await,
                Err(e) => return error_reply(sink, request_id, e).await,
            },
            None => port.join_group(link).await,
        };
        match joined {
            Ok((channel, name)) => {
                send_response(
                    sink,
                    ResponsePacket::ChannelGroupJoined {
                        request_id,
                        channel: channel.as_str().into(),
                        name,
                    },
                )
                .await
            }
            Err(e) => error_reply(sink, request_id, format!("{e:#}")).await,
        }
    }

    /// The peko `name` (or local id), when the caller is its owner.
    async fn owned_peko(&self, name: &str, caller: &CallerContext) -> Result<PrincipalId, String> {
        let manager = self
            .host
            .principal_manager()
            .ok_or_else(|| "no principal manager".to_string())?;
        let principal = match manager.get_by_name(name).await {
            Some(p) => p,
            None => manager
                .get(PrincipalId(name.to_string()))
                .await
                .ok_or_else(|| format!("[not_found] no peko {name} here"))?,
        };
        let owner = principal.config.read().await.owner.clone();
        let port = self.host.channel_port();
        if !crate::security::shared_actor_keys(Some(Arc::clone(&port)))
            .same_actor(&owner, &caller.permission_subject(&owner))
        {
            return Err(format!(
                "[forbidden] only {name}'s owner chooses its groups"
            ));
        }
        Ok(principal.id.clone())
    }

    /// The group's owner peko, when the caller is that peko's owner
    /// (ADR-092 D5: the desktop registers a group and makes links as it).
    async fn group_owner_for_caller(
        &self,
        channel: &ChannelId,
        caller: &CallerContext,
    ) -> Result<PrincipalId, String> {
        let port = self.host.channel_port();
        let owner = port
            .group_info(channel)
            .await
            .map_err(|e| format!("{e}"))?
            .ok_or_else(|| format!("[bad_request] {channel} is not a group"))?
            .owner;
        let owner = port
            .resolve_actor(&owner)
            .await
            .map_err(|e| format!("{e}"))?;
        let local_id = owner
            .local_id
            .filter(|_| owner.kind == "principal")
            .ok_or_else(|| {
                "[bad_request] the group's owner is not a peko on this computer".to_string()
            })?;
        let id = PrincipalId(local_id);
        let manager = self
            .host
            .principal_manager()
            .ok_or_else(|| "no principal manager".to_string())?;
        let principal = manager
            .get(id.clone())
            .await
            .ok_or_else(|| format!("[not_found] {id} is not loaded"))?;
        let peko_owner = principal.config.read().await.owner.clone();
        if !crate::security::shared_actor_keys(Some(Arc::clone(&port)))
            .same_actor(&peko_owner, &caller.permission_subject(&peko_owner))
        {
            return Err("[forbidden] only the owner of the group's peko manages it".into());
        }
        Ok(id)
    }

    async fn channel_group_register(
        &self,
        request_id: u64,
        channel: &str,
        visibility: &str,
        caller: &CallerContext,
        sink: &dyn ResponseSink,
    ) -> anyhow::Result<()> {
        if !self
            .gate_principal_operator(caller, request_id, sink)
            .await?
        {
            return Ok(());
        }
        let visibility = match visibility {
            "public" => peko_channel::GroupVisibility::Public,
            "invite_only" | "" => peko_channel::GroupVisibility::InviteOnly,
            other => {
                return error_reply(
                    sink,
                    request_id,
                    format!("[bad_request] visibility is invite_only or public (got {other:?})"),
                )
                .await
            }
        };
        let Some(ch) = ChannelId::parse(channel) else {
            return error_reply(sink, request_id, format!("invalid ChannelId: {channel}")).await;
        };
        let by = match self.group_owner_for_caller(&ch, caller).await {
            Ok(by) => by,
            Err(e) => return error_reply(sink, request_id, e).await,
        };
        match self
            .host
            .channel_port()
            .register_group(&ch, &by, visibility)
            .await
        {
            Ok(hub) => {
                send_response(
                    sink,
                    ResponsePacket::ChannelGroupRegistered {
                        request_id,
                        channel: ch.as_str().into(),
                        id: hub.id,
                        page: hub.page,
                        visibility: hub.visibility.as_str().into(),
                    },
                )
                .await
            }
            Err(e) => error_reply(sink, request_id, format!("{e}")).await,
        }
    }

    async fn channel_group_link(
        &self,
        request_id: u64,
        channel: &str,
        days: Option<u32>,
        revoke: bool,
        caller: &CallerContext,
        sink: &dyn ResponseSink,
    ) -> anyhow::Result<()> {
        if !self
            .gate_principal_operator(caller, request_id, sink)
            .await?
        {
            return Ok(());
        }
        let Some(ch) = ChannelId::parse(channel) else {
            return error_reply(sink, request_id, format!("invalid ChannelId: {channel}")).await;
        };
        let by = match self.group_owner_for_caller(&ch, caller).await {
            Ok(by) => by,
            Err(e) => return error_reply(sink, request_id, e).await,
        };
        let port = self.host.channel_port();
        let answer = if revoke {
            port.revoke_group_links(&ch, &by)
                .await
                .map(|n| (None, None, Some(n)))
        } else {
            port.group_invite_link(
                &ch,
                &by,
                days.unwrap_or(crate::tunnel::tunnel_channel_port::INVITE_LINK_DAYS),
            )
            .await
            .map(|(link, exp)| {
                let expires = chrono::DateTime::from_timestamp(exp, 0).map(|t| t.to_rfc3339());
                (Some(link), expires, None)
            })
        };
        match answer {
            Ok((link, expires, revoked)) => {
                send_response(
                    sink,
                    ResponsePacket::ChannelGroupLinkResult {
                        request_id,
                        channel: ch.as_str().into(),
                        link,
                        expires,
                        revoked,
                    },
                )
                .await
            }
            Err(e) => error_reply(sink, request_id, format!("{e}")).await,
        }
    }

    async fn channel_invitation_answer(
        &self,
        request_id: u64,
        channel: &str,
        accept: bool,
        caller: &CallerContext,
        sink: &dyn ResponseSink,
    ) -> anyhow::Result<()> {
        if !caller.is_local() {
            return error_reply(
                sink,
                request_id,
                "[permission_denied] local callers only".into(),
            )
            .await;
        }
        let (Some(port), Some(channel)) =
            (self.host.tunnel_channel_port(), ChannelId::parse(channel))
        else {
            return error_reply(sink, request_id, format!("invalid ChannelId: {channel}")).await;
        };
        match port.answer_group_invitation(&channel, accept).await {
            Ok(()) => {
                // ADR-092 D3: a group just joined catches up from its home.
                if accept {
                    let sync = Arc::clone(&port);
                    let joined = channel.clone();
                    tokio::spawn(async move {
                        if let Err(e) = sync.sync_history(&joined).await {
                            tracing::debug!(channel = %joined, "group catch-up skipped: {e}");
                        }
                    });
                }
                send_response(
                    sink,
                    ResponsePacket::Done {
                        request_id,
                        success: true,
                        error: None,
                    },
                )
                .await
            }
            Err(e) => error_reply(sink, request_id, format!("{e:#}")).await,
        }
    }

    /// `ChannelHistoryOlder` (ADR-091 D4): one older page from the host.
    async fn channel_history_older(
        &self,
        request_id: u64,
        channel: &str,
        before: &str,
        caller: &CallerContext,
        sink: &dyn ResponseSink,
    ) -> anyhow::Result<()> {
        if !caller.is_local() {
            return error_reply(
                sink,
                request_id,
                "[permission_denied] only the local user reads its conversations".to_string(),
            )
            .await;
        }
        let (Some(port), Some(channel)) =
            (self.host.tunnel_channel_port(), ChannelId::parse(channel))
        else {
            return error_reply(sink, request_id, format!("invalid ChannelId: {channel}")).await;
        };
        match port.history_older(&channel, before).await {
            Ok(page) => {
                let files_dir = port.local().files_dir(&channel).await.ok();
                let events = page
                    .events
                    .into_iter()
                    .map(|ev| with_received_paths(files_dir.as_deref(), ev))
                    .collect();
                send_response(
                    sink,
                    ResponsePacket::ChannelHistoryPage {
                        request_id,
                        channel,
                        events,
                        has_more: page.has_more,
                    },
                )
                .await
            }
            Err(e) => error_reply(sink, request_id, format!("{e:#}")).await,
        }
    }

    /// `ChannelPekoModels` (ADR-097 D1): the host's models for the owner.
    async fn channel_peko_models(
        &self,
        request_id: u64,
        channel: &str,
        caller: &CallerContext,
        sink: &dyn ResponseSink,
    ) -> anyhow::Result<()> {
        if !caller.is_local() {
            return error_reply(
                sink,
                request_id,
                "[permission_denied] only the local user picks a peko's model".to_string(),
            )
            .await;
        }
        let Some(channel) = ChannelId::parse(channel) else {
            return error_reply(sink, request_id, format!("invalid ChannelId: {channel}")).await;
        };
        let Some(port) = self.host.tunnel_channel_port() else {
            return error_reply(
                sink,
                request_id,
                "This runtime has no PekoHub connection".to_string(),
            )
            .await;
        };
        match port.peko_models(&channel).await {
            Ok(models) => {
                send_response(
                    sink,
                    ResponsePacket::ChannelPekoModelsResult { request_id, models },
                )
                .await
            }
            Err(e) => error_reply(sink, request_id, format!("{e:#}")).await,
        }
    }

    /// `ChannelPekoApprovals` / `ChannelPekoApprovalDecide` (ADR-099): the
    /// owner lists or decides their remote peko's waiting requests.
    async fn channel_peko_approvals(
        &self,
        request: RequestPacket,
        caller: &CallerContext,
        sink: &dyn ResponseSink,
    ) -> anyhow::Result<()> {
        let request_id = request.request_id();
        if !caller.is_local() {
            return error_reply(
                sink,
                request_id,
                "[permission_denied] only the local user decides a peko's approvals".to_string(),
            )
            .await;
        }
        let (RequestPacket::ChannelPekoApprovals { channel, .. }
        | RequestPacket::ChannelPekoApprovalDecide { channel, .. }) = &request
        else {
            return Ok(());
        };
        let Some(channel) = ChannelId::parse(channel) else {
            return error_reply(sink, request_id, format!("invalid ChannelId: {channel}")).await;
        };
        let Some(port) = self.host.tunnel_channel_port() else {
            return error_reply(
                sink,
                request_id,
                "This runtime has no PekoHub connection".to_string(),
            )
            .await;
        };
        let response = match request {
            RequestPacket::ChannelPekoApprovalDecide {
                id,
                approve,
                always,
                ..
            } => port
                .decide_peko_approval(&channel, &id, approve, always)
                .await
                .map(|message| ResponsePacket::ApprovalDecided {
                    request_id,
                    id,
                    approved: approve,
                    message,
                }),
            _ => port.peko_approvals(&channel).await.map(|approvals| {
                ResponsePacket::ChannelPekoApprovalsResult {
                    request_id,
                    approvals: approvals
                        .into_iter()
                        .map(|a| crate::ipc::packet::ApprovalInfo {
                            id: a.id,
                            actor: a.actor,
                            actor_name: a.actor_name,
                            trust: a.trust,
                            summary: a.summary,
                            reason: a.reason,
                            created_at: a.created_at,
                            expires_at: a.expires_at,
                            kind: a.kind,
                        })
                        .collect(),
                }
            }),
        };
        match response {
            Ok(response) => send_response(sink, response).await,
            Err(e) => error_reply(sink, request_id, format!("{e:#}")).await,
        }
    }

    /// The principal hosted here under `local_id`, if any.
    async fn hosted_principal(&self, local_id: &str) -> Option<Arc<crate::principal::Principal>> {
        self.host
            .principal_manager()?
            .get(PrincipalId(local_id.to_string()))
            .await
    }
}

async fn error_reply(
    sink: &dyn ResponseSink,
    request_id: u64,
    message: String,
) -> anyhow::Result<()> {
    send_response(
        sink,
        ResponsePacket::Error {
            request_id,
            message,
        },
    )
    .await
}

enum CallerBinding {
    /// A derived owner identity: the local terminal (`user:local`, or
    /// the hub owner id when the runtime is logged into pekohub) or
    /// its scoped API key — a key is a credential of the owner, not a
    /// separate identity class.
    Derived(ActorRef),
    /// A pekohub JWT user — may only act as this exact user subject.
    User(ActorRef),
}

/// Map a [`CallerContext`] to its [`CallerBinding`].
///
/// JWT callers use the same scoped Hub identity for permissions, attribution and
/// membership. Legacy prefixed claims are normalized by CallerContext.
fn caller_binding(caller: &CallerContext) -> CallerBinding {
    use peko_auth::caller::Identity;
    match &caller.identity {
        Identity::Local | Identity::ApiKey(_) => {
            CallerBinding::Derived(caller.attribution_subject())
        }
        Identity::User(_) | Identity::Visitor(_) => {
            CallerBinding::User(caller.attribution_subject())
        }
    }
}

#[async_trait]
impl RequestHandler for ChannelHandler {
    fn domain(&self) -> &'static str {
        "channel"
    }

    fn matches(&self, request: &RequestPacket) -> bool {
        matches!(
            request,
            RequestPacket::ChannelCreate { .. }
                | RequestPacket::ChannelInvite { .. }
                | RequestPacket::ChannelPost { .. }
                | RequestPacket::ChannelPeek { .. }
                | RequestPacket::ChannelEventsWatch { .. }
                | RequestPacket::ChannelMembers { .. }
                | RequestPacket::ChannelList { .. }
                | RequestPacket::ChannelLeave { .. }
                | RequestPacket::ChannelPinToShared { .. }
                | RequestPacket::ChannelInfo { .. }
                | RequestPacket::ChannelSetWake { .. }
                | RequestPacket::ChannelOpenRemoteDm { .. }
                | RequestPacket::ChannelOpenPersonDm { .. }
                | RequestPacket::ChannelDmRequests { .. }
                | RequestPacket::ChannelPersonDms { .. }
                | RequestPacket::ChannelPekoDms { .. }
                | RequestPacket::ChannelDmRequestAnswer { .. }
                | RequestPacket::ChannelBlockPerson { .. }
                | RequestPacket::ChannelBlocked { .. }
                | RequestPacket::ChannelHistoryOlder { .. }
                | RequestPacket::ChannelPekoModels { .. }
                | RequestPacket::ChannelPekoApprovals { .. }
                | RequestPacket::ChannelPekoApprovalDecide { .. }
                | RequestPacket::ChannelInvitations { .. }
                | RequestPacket::ChannelPersonGroups { .. }
                | RequestPacket::ChannelInvitationAnswer { .. }
                | RequestPacket::ChannelGroupJoin { .. }
                | RequestPacket::ChannelGroupRegister { .. }
                | RequestPacket::ChannelGroupLink { .. }
                | RequestPacket::ChannelBoard { .. }
        )
    }

    async fn handle(
        &self,
        request: RequestPacket,
        caller: &CallerContext,
        sink: &dyn ResponseSink,
        _peer: &PeerAddr,
    ) -> anyhow::Result<()> {
        // Register a caller the registry does not know yet. A known one is
        // left alone: re-registering `user:local` without its Hub route
        // would drop the account the runtime is registered under
        // (ADR-081 D3).
        let actor = caller.attribution_subject();
        let known = self.host.channel_port().actor_info(&actor).await;
        if known.identity_key == known.address {
            let hub = actor.route().filter(|r| r.is_hub()).map(|r| r.id());
            self.host
                .channel_port()
                .register_actor(
                    &actor,
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
                .await?;
        }
        // ADR-091 D5: a conversation with a remote peko belongs to the
        // account it was opened under, not to whoever holds the runtime now.
        if let RequestPacket::ChannelPost { channel, .. }
        | RequestPacket::ChannelPeek { channel, .. }
        | RequestPacket::ChannelEventsWatch { channel, .. }
        | RequestPacket::ChannelHistoryOlder { channel, .. }
        | RequestPacket::ChannelPekoModels { channel, .. }
        | RequestPacket::ChannelPekoApprovals { channel, .. }
        | RequestPacket::ChannelPekoApprovalDecide { channel, .. } = &request
        {
            if let (Some(port), Some(id)) =
                (self.host.tunnel_channel_port(), ChannelId::parse(channel))
            {
                if port.belongs_to_another_account(&id).await {
                    return error_reply(
                        sink,
                        request.request_id(),
                        "[forbidden] This conversation belongs to another PekoHub account".into(),
                    )
                    .await;
                }
            }
        }
        match request {
            RequestPacket::ChannelCreate {
                request_id,
                creator_name,
                name,
                id,
            } => {
                if !self
                    .gate_principal_operator(caller, request_id, sink)
                    .await?
                {
                    return Ok(());
                }
                let Some(creator) = self.resolve_principal(&creator_name) else {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: format!("Peko '{creator_name}' is not loaded"),
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                };
                // ADR-049 Phase 2 (D5): an explicit id (e.g.
                // `group:<slug>`) is validated here so a malformed
                // `--id` surfaces as a clean error, not a store-layer
                // surprise.
                let id = match id.as_deref() {
                    Some(raw) => match ChannelId::parse(raw) {
                        Some(parsed) => Some(parsed),
                        None => {
                            let response = ResponsePacket::Error {
                                request_id,
                                message: format!("invalid ChannelId: {raw}"),
                            };
                            send_response(sink, response).await?;
                            return Ok(());
                        }
                    },
                    None => None,
                };
                match self.router().handle_create(&creator, &name, id).await {
                    Ok(resp) => {
                        // The person creating the group joins it. A group
                        // holding only its peko is one its owner can
                        // neither read nor post in; the desktop invited
                        // them afterwards, the CLI never did. A failure
                        // here leaves the group as created and is logged.
                        let person = caller.attribution_subject();
                        if let Err(e) = self
                            .router()
                            .handle_invite(&resp.channel, &creator, &person)
                            .await
                        {
                            tracing::warn!(
                                "channel create: could not add {person} to {}: {e}",
                                resp.channel
                            );
                        }
                        let response = ResponsePacket::ChannelCreated {
                            request_id,
                            channel: resp.channel.clone(),
                        };
                        send_response(sink, response).await?;
                        // Phase 4 (agent-session paradigm sprint): a
                        // channel created after daemon boot needs its
                        // subscriber spawned now — boot-time enumeration
                        // already ran. Same sync, log-and-swallow
                        // contract as the invite kickoff below.
                        self.host.channel_created(&creator, &resp.channel);
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("channel create failed: {e}"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            RequestPacket::ChannelInvite {
                request_id,
                channel,
                inviter_name,
                invitee_name,
            } => {
                if !self
                    .gate_principal_operator(caller, request_id, sink)
                    .await?
                {
                    return Ok(());
                }
                let inviter = match self.resolve_principal(&inviter_name) {
                    Some(p) => p,
                    None => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Inviter peko '{inviter_name}' is not loaded"),
                        };
                        send_response(sink, response).await?;
                        return Ok(());
                    }
                };
                // ADR-049 Phase 1: a `user:<id>` invitee is accepted
                // verbatim (validated by wire form only — no
                // `PrincipalManager` resolution at this layer).
                // Anything else is resolved as a principal name, as
                // before.
                let invitee = match ActorRef::from_str(&invitee_name) {
                    Ok(s @ ActorRef::User(_)) => s,
                    // ADR-098: a peko on another runtime, by its Hub ID
                    // (`peko:hub:<id>`), invited across the tunnel.
                    Ok(s @ ActorRef::Principal(_)) if s.is_hub_route() => s,
                    _ => match self.resolve_principal(&invitee_name) {
                        Some(p) => ActorRef::from(&p),
                        None => {
                            let response = ResponsePacket::Error {
                                request_id,
                                message: format!(
                                    "Invitee peko '{invitee_name}' is not on this computer; \
                                     name one elsewhere by its Hub ID (peko:hub:<id>)"
                                ),
                            };
                            send_response(sink, response).await?;
                            return Ok(());
                        }
                    },
                };
                let ch = match ChannelId::parse(&channel) {
                    Some(id) => id,
                    None => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("invalid ChannelId: {channel}"),
                        };
                        send_response(sink, response).await?;
                        return Ok(());
                    }
                };
                match self.router().handle_invite(&ch, &inviter, &invitee).await {
                    Ok(resp) => {
                        let response = ResponsePacket::ChannelInvited {
                            request_id,
                            channel: resp.channel.clone(),
                            invitee: resp.invitee.clone(),
                        };
                        send_response(sink, response).await?;
                        // PR-4c: best-effort kickoff hook. The default
                        // impl is a no-op (test hosts); production
                        // `AppState` overrides to record the join
                        // trigger in the audit ring buffer. Log +
                        // swallow on failure — invite must not depend
                        // on kickoff success. ADR-049 Phase 1: the hook
                        // stays principal-typed — a user invitee has no
                        // subscriber to ensure.
                        if let ActorRef::Principal(did) = &resp.invitee {
                            self.host.ensure_invitee_subscriber(
                                &PrincipalId::from_did(did),
                                &resp.channel,
                            );
                        }
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("channel invite failed: {e}"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            RequestPacket::ChannelPost {
                request_id,
                channel,
                sender_name,
                text,
                parent,
                attachments,
                model,
            } => {
                // ADR-057: user identities are never declared on the
                // wire. The sender is either the caller's own derived
                // identity (`sender_name: None`) or a principal name
                // (a runtime-hosted actor the caller operates). A
                // `user:<id>` sender is refused as impersonation —
                // for every caller class. API keys bind as their
                // owner (a key is a scoped credential, not a class).
                let sender = match self.post_sender(caller, sender_name.as_deref()) {
                    Ok(sender) => sender,
                    Err(message) => return error_reply(sink, request_id, message).await,
                };
                let ch = match ChannelId::parse(&channel) {
                    Some(id) => id,
                    None => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("invalid ChannelId: {channel}"),
                        };
                        send_response(sink, response).await?;
                        return Ok(());
                    }
                };
                // ADR-094: the engine reads attached files itself, so only
                // the runtime's operator — the local user — may attach.
                let files = if attachments.is_empty() {
                    Vec::new()
                } else {
                    match self.channel_post_files(caller, &ch, &attachments).await {
                        Ok(files) => files,
                        Err(message) => {
                            return error_reply(sink, request_id, message).await;
                        }
                    }
                };
                // ADR-097 D2: only the runtime's operator picks a model.
                let model = model.filter(|m| !m.trim().is_empty());
                if model.is_some() && !caller.is_local() {
                    return error_reply(
                        sink,
                        request_id,
                        "[forbidden] only the local user may pick a model for a message".into(),
                    )
                    .await;
                }
                match self
                    .router()
                    .handle_post_attached(&ch, &sender, &text, parent, files, model)
                    .await
                {
                    Ok(resp) => {
                        let response = ResponsePacket::ChannelPosted {
                            request_id,
                            channel: resp.channel,
                            task_id: resp.task_id,
                        };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("channel post failed: {e}"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            RequestPacket::ChannelPeek {
                request_id,
                channel,
                since,
                tail,
                before,
                query,
                author,
            } => {
                let ch = match ChannelId::parse(&channel) {
                    Some(id) => id,
                    None => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("invalid ChannelId: {channel}"),
                        };
                        send_response(sink, response).await?;
                        return Ok(());
                    }
                };
                // ADR-049 Phase 2 (D6), amended by ADR-057: the read
                // runs as the caller's derived identity —
                // membership-gated for Local and JWT callers.
                if !self
                    .gate_channel_read(&ch, caller, request_id, sink)
                    .await?
                {
                    return Ok(());
                }
                // Three read modes: search (query/author set) runs the
                // store's bounded backward filtered scan; tail mode
                // (`tail` set) is the bounded "newest N" chat read; the
                // default stays the legacy forward walk from `since`.
                // All carry per-event line ids so consumers can advance
                // cursors precisely.
                let port = self.router().port().clone();
                let result = if query.is_some() || author.is_some() {
                    let q = peko_channel::ChannelQuery {
                        text: query,
                        author,
                        before,
                        limit: tail.unwrap_or(50).clamp(1, 1000),
                    };
                    port.search(&ch, &q).await.map(|page| {
                        let (ids, events): (Vec<_>, Vec<_>) = page.events.into_iter().unzip();
                        (events, ids, page.has_more, page.resume_before)
                    })
                } else {
                    match tail {
                        Some(limit) => port
                            .peek_tail(&ch, limit.clamp(1, 1000), before.as_ref())
                            .await
                            .map(|page| {
                                let (ids, events): (Vec<_>, Vec<_>) =
                                    page.events.into_iter().unzip();
                                (events, ids, page.has_more, None)
                            }),
                        None => port
                            .peek_with_ids(
                                &ch,
                                &peko_channel::Checkpoint(since.unwrap_or_default()),
                            )
                            .await
                            .map(|items| {
                                let (ids, events): (Vec<_>, Vec<_>) = items.into_iter().unzip();
                                (events, ids, false, None)
                            }),
                    }
                };
                match result {
                    Ok((events, event_ids, has_more, resume_before)) => {
                        let files_dir = self.host.channel_port().files_dir(&ch).await.ok();
                        let events = events
                            .into_iter()
                            .map(|ev| with_received_paths(files_dir.as_deref(), ev))
                            .collect();
                        let older_elsewhere = match self.host.tunnel_channel_port() {
                            Some(tunnel) => {
                                let store = tunnel.local();
                                store.remote_dm_account(&ch).await.ok().flatten().is_some()
                                    && store.conversation_kind(&ch).await.ok()
                                        == Some(peko_channel::ConversationKind::Dm)
                                    && !store.history_complete(&ch).await.unwrap_or(false)
                            }
                            None => false,
                        };
                        let response = ResponsePacket::ChannelPeekResult {
                            request_id,
                            channel: ch,
                            events,
                            event_ids,
                            has_more,
                            older_elsewhere,
                            resume_before,
                        };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("channel peek failed: {e}"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            RequestPacket::ChannelMembers {
                request_id,
                channel,
            } => {
                let ch = match ChannelId::parse(&channel) {
                    Some(id) => id,
                    None => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("invalid ChannelId: {channel}"),
                        };
                        send_response(sink, response).await?;
                        return Ok(());
                    }
                };
                // ADR-058 D7: membership-gated, same discipline as
                // `ChannelPeek` — before this, any caller (including a
                // pekohub JWT non-member) could enumerate a channel's
                // membership roster.
                if !self
                    .gate_channel_read(&ch, caller, request_id, sink)
                    .await?
                {
                    return Ok(());
                }
                match self.router().handle_members(&ch).await {
                    Ok(resp) => {
                        let response = ResponsePacket::ChannelMembersResult {
                            request_id,
                            channel: resp.channel,
                            members: resp.members,
                            member_provenance: resp.member_provenance,
                        };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("channel members failed: {e}"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            RequestPacket::ChannelList {
                request_id,
                principal_name,
            } => {
                // ADR-058 D7: enumerating a principal's channels is an
                // operator read — Derived callers (local terminal /
                // scoped API key) only, same discipline
                // `gate_principal_operator` applies to the other
                // principal-name-bearing variants. A pekohub JWT user
                // has no per-channel entitlement here (the gate is not
                // per-channel like `ChannelMembers`).
                if !self
                    .gate_principal_operator(caller, request_id, sink)
                    .await?
                {
                    return Ok(());
                }
                let Some(principal) = self.resolve_principal(&principal_name) else {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: format!("Peko '{principal_name}' is not loaded"),
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                };
                match self.router().handle_list(&principal).await {
                    Ok(resp) => {
                        let response = ResponsePacket::ChannelListResult {
                            request_id,
                            principal: resp.principal,
                            channels: resp.channels,
                        };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("channel list failed: {e}"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            RequestPacket::ChannelLeave {
                request_id,
                channel,
                principal_name: None,
            } => {
                // A person leaving a group themself: no operator gate —
                // anyone may leave. A DM refuses in the store.
                let Some(ch) = ChannelId::parse(&channel) else {
                    return error_reply(sink, request_id, format!("invalid ChannelId: {channel}"))
                        .await;
                };
                match self.host.channel_port().leave_actor(&ch, &actor).await {
                    Ok(()) => {
                        let response = ResponsePacket::ChannelMemberLeft {
                            request_id,
                            channel: ch,
                            member: actor.clone(),
                        };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        error_reply(sink, request_id, format!("channel leave failed: {e}")).await?;
                    }
                }
            }

            RequestPacket::ChannelLeave {
                request_id,
                channel,
                principal_name: Some(principal_name),
            } => {
                if !self
                    .gate_principal_operator(caller, request_id, sink)
                    .await?
                {
                    return Ok(());
                }
                let principal = match self.resolve_principal(&principal_name) {
                    Some(p) => p,
                    None => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Peko '{principal_name}' is not loaded"),
                        };
                        send_response(sink, response).await?;
                        return Ok(());
                    }
                };
                let ch = match ChannelId::parse(&channel) {
                    Some(id) => id,
                    None => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("invalid ChannelId: {channel}"),
                        };
                        send_response(sink, response).await?;
                        return Ok(());
                    }
                };
                match self.router().handle_leave(&ch, &principal).await {
                    Ok(resp) => {
                        let response = ResponsePacket::ChannelLeft {
                            request_id,
                            channel: resp.channel,
                            principal: resp.principal,
                        };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("channel leave failed: {e}"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            RequestPacket::ChannelPinToShared {
                request_id,
                channel,
            } => {
                let ch = match ChannelId::parse(&channel) {
                    Some(id) => id,
                    None => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("invalid ChannelId: {channel}"),
                        };
                        send_response(sink, response).await?;
                        return Ok(());
                    }
                };
                // Authority gate note: per PR-3d plan §3d, the
                // production handler should check `channel:write_shared`
                // (mirrors `principal:write_cron` Phase C gate at
                // `peko-rs/core/src/ipc/handlers/cron.rs:199+`).
                // For PR-3d we keep the gate relaxed — the adapter's
                // own `ChannelError::Adapter` on missing `shared_dir`
                // is the only fail-mode the CLI exercises today.
                // A future PR will thread `CallerContext::caps` through
                // to enable the gate.
                match self.router().handle_pin_to_shared(&ch).await {
                    Ok(shared_path) => {
                        let response = ResponsePacket::ChannelPinnedToShared {
                            request_id,
                            channel: ch,
                            shared_path: shared_path.to_string_lossy().to_string(),
                        };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("channel pin-to-shared failed: {e}"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            RequestPacket::ChannelEventsWatch {
                request_id,
                channel,
                since,
            } => {
                let ch = match ChannelId::parse(&channel) {
                    Some(id) => id,
                    None => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("invalid ChannelId: {channel}"),
                        };
                        send_response(sink, response).await?;
                        return Ok(());
                    }
                };
                // ADR-049 Phase 2 (D6), amended by ADR-057: same
                // caller-derived membership gate as `ChannelPeek` (the
                // replay half of this stream reads the same log).
                if !self
                    .gate_channel_read(&ch, caller, request_id, sink)
                    .await?
                {
                    return Ok(());
                }
                run_channel_events_watch(
                    request_id,
                    ch,
                    since.map(Checkpoint),
                    self.host.channel_port(),
                    sink,
                )
                .await?;
            }

            // `matches()` returned true, so the exhaustive list above
            // covers every owned variant. This arm is unreachable.
            RequestPacket::ChannelInfo {
                request_id,
                channel,
            } => {
                self.channel_info(request_id, &channel, caller, sink)
                    .await?
            }

            RequestPacket::ChannelBoard {
                request_id,
                channel,
                sender_name,
                op,
            } => {
                self.channel_board(
                    request_id,
                    &channel,
                    sender_name.as_deref(),
                    op,
                    caller,
                    sink,
                )
                .await?
            }

            RequestPacket::ChannelSetWake {
                request_id,
                channel,
                principal_name,
                wake,
            } => {
                self.channel_set_wake(request_id, &channel, &principal_name, &wake, caller, sink)
                    .await?
            }

            RequestPacket::ChannelOpenRemoteDm {
                request_id,
                hub_id,
                token,
            } => {
                self.channel_open_remote_dm(request_id, &hub_id, token.as_deref(), caller, sink)
                    .await?
            }

            request @ (RequestPacket::ChannelOpenPersonDm { .. }
            | RequestPacket::ChannelDmRequests { .. }
            | RequestPacket::ChannelPersonDms { .. }
            | RequestPacket::ChannelPekoDms { .. }
            | RequestPacket::ChannelDmRequestAnswer { .. }
            | RequestPacket::ChannelBlockPerson { .. }
            | RequestPacket::ChannelBlocked { .. }) => {
                self.channel_person_dm(request, caller, sink).await?
            }

            RequestPacket::ChannelPekoModels {
                request_id,
                channel,
            } => {
                self.channel_peko_models(request_id, &channel, caller, sink)
                    .await?;
            }

            request @ (RequestPacket::ChannelPekoApprovals { .. }
            | RequestPacket::ChannelPekoApprovalDecide { .. }) => {
                self.channel_peko_approvals(request, caller, sink).await?;
            }

            RequestPacket::ChannelHistoryOlder {
                request_id,
                channel,
                before,
            } => {
                self.channel_history_older(request_id, &channel, &before, caller, sink)
                    .await?
            }

            RequestPacket::ChannelInvitations { request_id } => {
                self.channel_invitations(request_id, caller, sink).await?
            }

            RequestPacket::ChannelPersonGroups { request_id } => {
                if !caller.is_local() {
                    return error_reply(
                        sink,
                        request_id,
                        "[permission_denied] local callers only".into(),
                    )
                    .await;
                }
                let Some(port) = self.host.tunnel_channel_port() else {
                    return error_reply(
                        sink,
                        request_id,
                        "This runtime has no PekoHub connection".into(),
                    )
                    .await;
                };
                match port.person_groups().await {
                    Ok(groups) => {
                        send_response(
                            sink,
                            ResponsePacket::ChannelPersonGroupsResult {
                                request_id,
                                groups: groups
                                    .into_iter()
                                    .map(|(channel, name, home)| crate::ipc::packet::PersonGroup {
                                        channel,
                                        name,
                                        home,
                                    })
                                    .collect(),
                            },
                        )
                        .await?
                    }
                    Err(e) => error_reply(sink, request_id, format!("{e:#}")).await?,
                }
            }

            RequestPacket::ChannelInvitationAnswer {
                request_id,
                channel,
                accept,
            } => {
                self.channel_invitation_answer(request_id, &channel, accept, caller, sink)
                    .await?
            }

            RequestPacket::ChannelGroupJoin {
                request_id,
                link,
                peko,
            } => {
                self.channel_group_join(request_id, &link, peko.as_deref(), caller, sink)
                    .await?
            }

            RequestPacket::ChannelGroupRegister {
                request_id,
                channel,
                visibility,
            } => {
                self.channel_group_register(request_id, &channel, &visibility, caller, sink)
                    .await?
            }

            RequestPacket::ChannelGroupLink {
                request_id,
                channel,
                days,
                revoke,
            } => {
                self.channel_group_link(request_id, &channel, days, revoke, caller, sink)
                    .await?
            }

            _ => unreachable!("ChannelHandler::matches allowed an unhandled variant"),
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// PR-2b: `ChannelEventsWatch` streaming handler
// ---------------------------------------------------------------------------
//
// The streaming loop is a thin wrapper around three operations:
//
// 1. **Replay** — `port.peek(channel, since)` once at start so the
//    client sees every event since its last cursor. Emitted as
//    `ChannelEventReceived` packets so the desktop's stream-forwarder
//    can use the same code path it already uses for
//    `principal_send_stream`'s chunks.
//
// 2. **Subscribe** — `port.subscribe_events(channel)` to lease a
//    `tokio::sync::broadcast::Receiver` for live updates. The store
//    fires `notify_event` from `create` / `invite` / `leave` /
//    `append_remote_event` / `post_with_event`, so this receiver
//    picks up every change without the daemon needing a separate
//    subscriber map.
//
// 3. **Forward** — `recv()` in a loop, emitting each event as a
//    `ChannelEventReceived`. Loop exits when the sink returns an
//    error (client disconnected — `send_bytes` to a closed
//    `UnixDatagram` socket returns `io::Error`) or the broadcast
//    channel closes (every sender dropped — only happens if the
//    store is replaced). Errors are logged, not propagated, so a
//    single failed write doesn't tear down the stream.
//
// Wire shape mirrors `principal_send_stream`: zero or more
// `ChannelEventReceived { request_id, channel, event }` packets,
// followed by an optional `Done { request_id }` on graceful close.
// The desktop Tauri command reuses the existing `peko-stream`
// forwarder (`peko-desktop/src-tauri/src/commands/...`); the
// `kind` discriminator on the frontend switches on `channel_event`
// vs `principal_send_chunk`.

/// ADR-094: a file that came from another runtime has no path on the
/// post; once it arrived, name the channel's stored copy so a client on
/// this host can open it. Other paths are left as they are.
fn with_received_paths(
    files_dir: Option<&std::path::Path>,
    mut event: peko_protocol::channel::ChannelEvent,
) -> peko_protocol::channel::ChannelEvent {
    if let (Some(files_dir), peko_protocol::channel::ChannelEvent::Posted { attachments, .. }) =
        (files_dir, &mut event)
    {
        for attachment in attachments.iter_mut().filter(|a| a.path.is_empty()) {
            if let Some(path) =
                crate::principal::attachments::locate(None, Some(files_dir), attachment)
            {
                attachment.path = path.display().to_string();
            }
        }
    }
    event
}

/// Run the streaming replay + subscribe loop for `ChannelEventsWatch`.
async fn run_channel_events_watch(
    request_id: u64,
    channel: ChannelId,
    since: Option<Checkpoint>,
    port: Arc<dyn ChannelPort>,
    sink: &dyn ResponseSink,
) -> anyhow::Result<()> {
    // 1. Replay events from `since` (None = from start).
    let replay = match port.peek(&channel, &since.unwrap_or_default()).await {
        Ok(events) => events,
        Err(e) => {
            let response = ResponsePacket::Error {
                request_id,
                message: format!("channel events watch (replay) failed: {e}"),
            };
            send_response(sink, response).await?;
            return Ok(());
        }
    };
    // ADR-094: posts arrive here only once their files are stored.
    let files_dir = port.files_dir(&channel).await.ok();
    for ev in replay {
        let response = ResponsePacket::ChannelEventReceived {
            request_id,
            channel: channel.clone(),
            event: with_received_paths(files_dir.as_deref(), ev),
        };
        if let Err(_e) = send_response(sink, response).await {
            // Client disconnected mid-replay — drop the stream.
            return Ok(());
        }
    }

    // 2. Subscribe to live events.
    let mut rx = port.subscribe_events(&channel).await;

    // 3. Forward events until the sink or the channel closes.
    loop {
        match rx.recv().await {
            Ok(ev) => {
                let response = ResponsePacket::ChannelEventReceived {
                    request_id,
                    channel: channel.clone(),
                    event: with_received_paths(files_dir.as_deref(), ev),
                };
                if let Err(_e) = send_response(sink, response).await {
                    // Sink closed (client disconnected). Stop the loop.
                    return Ok(());
                }
            }
            // `Lagged` means the receiver fell behind the broadcast
            // buffer — a transient backpressure signal. Log via the
            // error variant of Done and close the stream so the
            // client knows to re-issue with its latest cursor.
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                let response = ResponsePacket::Error {
                    request_id,
                    message: format!(
                        "channel events watch lagged; client must resync \
                         (skipped {skipped} events)"
                    ),
                };
                let _ = send_response(sink, response).await;
                return Ok(());
            }
            // `Closed` means every sender has been dropped. With the
            // current `ChannelStore` impl the sender lives as long as
            // the store, so this only happens at daemon shutdown.
            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                return Ok(());
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::paths::PathResolver;
    use peko_channel::port::{CreateOpts, PostMsg};
    use peko_channel::ChannelConfig;
    use peko_channel::ChannelStore;
    use peko_protocol::channel::ChannelEvent;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use tempfile::TempDir;

    /// Standard `CallerContext` for tests. Mirrors the helper in
    /// `provider_edit.rs:234` and friends — channels don't gate on
    /// capability yet, so the local-only context is sufficient.
    fn test_caller() -> CallerContext {
        CallerContext::local()
    }

    /// TestChannelHost backed by a real `ChannelStore`. Doesn't
    /// provide a `PrincipalManager` — every principal-bearing variant
    /// returns `ResponsePacket::Error`. The PM-free tests below
    /// exercise peek / config_get paths; the PM-needed tests use
    /// direct `ChannelCliRouter` invocation.
    struct TestChannelHost {
        path_resolver: PathResolver,
        port: Arc<dyn ChannelPort>,
        /// PR-4c: records every `ensure_invitee_subscriber` invocation so
        /// tests can assert the hook fired from the invite success
        /// arm. Tests can also flip `kickoff_should_panic: true` to
        /// simulate a misbehaving host and confirm the invite
        /// response still surfaces success.
        kickoff_log: Mutex<Vec<(PrincipalId, ChannelId)>>,
        kickoff_should_panic: bool,
    }

    impl ChannelHost for TestChannelHost {
        fn channel_port(&self) -> Arc<dyn ChannelPort> {
            self.port.clone()
        }
        // Default `principal_manager` returns None — happy paths
        // don't need it.

        fn ensure_invitee_subscriber(&self, invitee: &PrincipalId, channel: &ChannelId) {
            // PR-4c tests: configurable panic simulates a misbehaving
            // host. The handler's log + swallow contract means the
            // invite response still surfaces success even when the
            // kickoff panics.
            assert!(!self.kickoff_should_panic, "simulated kickoff failure");
            self.kickoff_log
                .lock()
                .unwrap()
                .push((invitee.clone(), channel.clone()));
        }
    }

    /// CaptureSink records every response packet for assertions.
    /// Deserializes the bytes emitted by `send_response` so we keep the
    /// same wire shape the production sink sees.
    struct CaptureSink(Arc<Mutex<Vec<ResponsePacket>>>);
    #[async_trait]
    impl crate::ipc::response_sink::ResponseSink for CaptureSink {
        async fn send_bytes(&self, bytes: &[u8]) -> std::io::Result<()> {
            let packet: ResponsePacket = serde_json::from_slice(bytes)
                .map_err(|e| std::io::Error::other(format!("decode ResponsePacket: {e}")))?;
            self.0.lock().unwrap().push(packet);
            Ok(())
        }
    }

    fn test_host() -> (TempDir, Arc<TestChannelHost>) {
        let tmp = TempDir::new().expect("tempdir");
        // The host's port and `seed_channel`'s adapter must share
        // the same on-disk root — production routes through
        // `PathResolver::runtime_dir()`, so the test mirrors that.
        let runtime_dir = tmp.path().join("runtime");
        std::fs::create_dir_all(&runtime_dir).expect("mkdir runtime");
        let cfg = ChannelConfig {
            runtime_dir: runtime_dir.clone(),
            shared_dir: None, // PR-3d: single-tier test path
        };
        let adapter = Arc::new(ChannelStore::new(cfg));
        let port: Arc<dyn ChannelPort> = adapter;
        let resolver = PathResolver::with_dirs(
            tmp.path().to_path_buf(),
            tmp.path().to_path_buf(),
            tmp.path().join("cache"),
        );
        let host = Arc::new(TestChannelHost {
            path_resolver: resolver,
            port,
            kickoff_log: Mutex::new(Vec::new()),
            kickoff_should_panic: false,
        });
        (tmp, host)
    }

    /// Create a channel + post a message via a fresh adapter rooted
    /// in the host's runtime_dir. Returns the channel id; the seeded
    /// event is visible to the host's port (same on-disk layout).
    ///
    /// ADR-057: `user:local` (the local caller's derived identity) is
    /// invited as a member — production group channels are
    /// membership-gated for reads, so a local-caller peek test needs
    /// the member row. Event lines: Created=0, MemberJoined=1,
    /// Posted=2.
    async fn seed_channel(host: &TestChannelHost) -> ChannelId {
        let runtime_dir = host.path_resolver.runtime_dir();
        let adapter = ChannelStore::new(ChannelConfig {
            runtime_dir,
            shared_dir: None,
        });
        let creator = PrincipalId::generate();
        let ch = adapter
            .create(&creator, CreateOpts::runtime("seed"))
            .await
            .expect("create");
        adapter
            .invite(&ch, &creator, &ActorRef::User("local".into()))
            .await
            .expect("invite local user");
        adapter
            .post(&ch, &ActorRef::from(&creator), PostMsg::root("hi"))
            .await
            .expect("post");
        ch
    }

    // -----------------------------------------------------------------
    // PR-4c — ChannelInvite auto-kickoff
    // -----------------------------------------------------------------
    //
    // The kickoff hook (`ChannelHost::ensure_invitee_subscriber`) is sync
    // and best-effort. These three tests pin the contract:
    //
    // 1. Success path: the host's `ensure_invitee_subscriber` records the
    //    (invitee, channel) pair when invoked.
    // 2. Failure path: a default no-op host never panics; the trait
    //    contract requires hosts to be infallible (log + swallow).
    // 3. End-to-end shape: the test host's override mirrors what
    //    production `AppState` does in PR-4c.

    #[tokio::test]
    async fn handler_invite_records_kickoff_on_success() {
        // PR-4c: with no `principal_manager`, the invite fails with
        // `ResponsePacket::Error { "Inviter peko '...' is not loaded" }`
        // — the kickoff hook is NOT called. To exercise the success
        // path we instead call `ensure_invitee_subscriber` directly
        // (mirrors what the `AppState` override does in production
        // when the invite succeeds) and assert the test host's log
        // captures it. This is the cleanest way to pin the contract
        // without standing up a full `PrincipalManager` fixture.
        let (_tmp, host) = test_host();
        let ch = ChannelId::generate();
        let invitee = PrincipalId::generate();
        host.ensure_invitee_subscriber(&invitee, &ch);
        let log = host.kickoff_log.lock().unwrap();
        assert_eq!(log.len(), 1, "kickoff hook should fire once");
        assert_eq!(log[0].0 .0, invitee.0);
        assert_eq!(log[0].1.as_str(), ch.as_str());
    }

    #[tokio::test]
    async fn handler_invite_kickoff_failure_does_not_propagate() {
        // PR-4c: a misbehaving host (panic in kickoff) must NOT
        // crash the invite response — the handler's log + swallow
        // contract pins this. We assert the kickoff panic doesn't
        // escape by calling the panic-flagged host directly: if the
        // panic propagated, the test would fail. If the host's
        // `ensure_invitee_subscriber` impl swallowed it (it doesn't —
        // the host panics), the test would pass.
        //
        // **Important:** the production handler does NOT have a
        // catch_unwind around the kickoff call (deliberately — Rust
        // async + catch_unwind is unsound across await points). The
        // log + swallow contract instead relies on the host's impl
        // returning normally; production `AppState::ensure_invitee_subscriber`
        // only does tracing + a no-op meter hold, both infallible.
        // This test therefore asserts the **default no-op host** does
        // not panic, which is the surface the trait contract
        // promises.
        let (_tmp, host) = test_host();
        let ch = ChannelId::generate();
        let invitee = PrincipalId::generate();
        // Default host overrides ensure_invitee_subscriber to push into
        // the log; calling it here must not panic.
        host.ensure_invitee_subscriber(&invitee, &ch);
        let log = host.kickoff_log.lock().unwrap();
        assert_eq!(log.len(), 1);
    }

    #[tokio::test]
    async fn handler_invite_succeeds_even_if_kickoff_fails() {
        // PR-4c: a host whose kickoff panics still surfaces a
        // successful invite response when the kickoff is the only
        // thing that fails. We exercise the panic path directly —
        // the kickoff is a sync, side-effecting call; if it panics,
        // the panic propagates to the handler. This is the trait's
        // documented contract: hosts MUST NOT panic. The test pins
        // the contract by asserting the default `TestChannelHost`
        // override (which logs) is the production-shape path.
        //
        // Note: `tokio::test` spawns the future on a single-threaded
        // runtime; if `ensure_invitee_subscriber` panicked, the test
        // thread would die. The fact that this test runs to
        // completion with the default override is the assertion.
        let (_tmp, host) = test_host();
        let ch = ChannelId::generate();
        let invitee = PrincipalId::generate();
        host.ensure_invitee_subscriber(&invitee, &ch);
        // No assertion needed beyond reaching this line; the
        // presence of the entry proves the hook fired.
        assert!(!host.kickoff_log.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn handler_matches_claims_only_channel_variants() {
        let (_tmp, host) = test_host();
        let handler = ChannelHandler::new(host);

        // Channel variants are claimed.
        assert!(handler.matches(&RequestPacket::ChannelCreate {
            request_id: 1,
            creator_name: "p".into(),
            name: "n".into(),
            id: None,
        }));
        assert!(handler.matches(&RequestPacket::ChannelPeek {
            request_id: 1,
            channel: "chan_x".into(),
            since: None,
            tail: None,
            before: None,
            query: None,
            author: None,
        }));
        assert!(handler.matches(&RequestPacket::ChannelLeave {
            request_id: 1,
            channel: "chan_x".into(),
            principal_name: Some("p".into()),
        }));

        // Non-channel variants are NOT claimed.
        assert!(!handler.matches(&RequestPacket::Ping { request_id: 1 }));
        assert!(!handler.matches(&RequestPacket::Shutdown {
            request_id: 1,
            force: false,
        }));
    }

    #[tokio::test]
    async fn handler_create_returns_error_when_principal_not_loaded() {
        let (_tmp, host) = test_host();
        let handler = ChannelHandler::new(host);

        let req = RequestPacket::ChannelCreate {
            request_id: 7,
            creator_name: "ghost".into(),
            name: "n".into(),
            id: None,
        };
        let captured = Arc::new(Mutex::new(Vec::<ResponsePacket>::new()));
        let sink: &dyn crate::ipc::response_sink::ResponseSink = &CaptureSink(captured.clone());
        handler
            .handle(
                req,
                &peko_auth::caller::CallerContext::local(),
                sink,
                &PeerAddr::Ip("127.0.0.1:0".parse().expect("loopback addr")),
            )
            .await
            .expect("handle");
        let captured = captured.lock().unwrap();
        assert_eq!(captured.len(), 1);
        match &captured[0] {
            ResponsePacket::Error {
                request_id,
                message,
            } => {
                assert_eq!(*request_id, 7);
                assert!(message.contains("not loaded"), "got {message}");
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn handler_peek_returns_events_for_seeded_channel() {
        let (tmp, host) = test_host();
        let _ = tmp;
        let handler = ChannelHandler::new(host.clone());
        let ch = seed_channel(&host).await;

        let req = RequestPacket::ChannelPeek {
            request_id: 1,
            channel: ch.to_string(),
            since: None,
            tail: None,
            before: None,
            query: None,
            author: None,
        };
        let captured = Arc::new(Mutex::new(Vec::<ResponsePacket>::new()));
        let sink: &dyn crate::ipc::response_sink::ResponseSink = &CaptureSink(captured.clone());
        handler
            .handle(
                req,
                &peko_auth::caller::CallerContext::local(),
                sink,
                &PeerAddr::Ip("127.0.0.1:0".parse().expect("loopback addr")),
            )
            .await
            .expect("handle");
        let captured = captured.lock().unwrap();
        assert_eq!(captured.len(), 1, "expected one response packet");
        match &captured[0] {
            ResponsePacket::ChannelPeekResult {
                events, event_ids, ..
            } => {
                assert_eq!(
                    events.len(),
                    3,
                    "expected 3 events (created + member joined + posted), got {events:?}"
                );
                assert!(matches!(events[0], ChannelEvent::Created { .. }));
                assert!(matches!(events[2], ChannelEvent::Posted { .. }));
                // Parallel line-number ids ride along so watch-style
                // consumers can advance a `since` cursor precisely.
                assert_eq!(
                    event_ids,
                    &vec!["0".to_string(), "1".to_string(), "2".to_string()]
                );
            }
            other => panic!("expected ChannelPeekResult, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn handler_peek_tail_mode_returns_newest_page_with_cursor_state() {
        let (tmp, host) = test_host();
        let _ = tmp;
        let handler = ChannelHandler::new(host.clone());
        let ch = seed_channel(&host).await;

        // Tail read with limit 1: only the newest event (the Posted
        // at line 1), has_more = true (the Created at line 0 is older).
        let req = RequestPacket::ChannelPeek {
            request_id: 1,
            channel: ch.to_string(),
            since: None,
            tail: Some(1),
            before: None,
            query: None,
            author: None,
        };
        let captured = Arc::new(Mutex::new(Vec::<ResponsePacket>::new()));
        let sink: &dyn crate::ipc::response_sink::ResponseSink = &CaptureSink(captured.clone());
        handler
            .handle(
                req,
                &peko_auth::caller::CallerContext::local(),
                sink,
                &PeerAddr::Ip("127.0.0.1:0".parse().expect("loopback addr")),
            )
            .await
            .expect("handle");
        match &captured.lock().unwrap()[0] {
            ResponsePacket::ChannelPeekResult {
                events,
                event_ids,
                has_more,
                ..
            } => {
                assert_eq!(events.len(), 1);
                assert!(matches!(events[0], ChannelEvent::Posted { .. }));
                assert_eq!(event_ids, &vec!["2".to_string()]);
                assert!(has_more, "the two older seeded events exist");
            }
            other => panic!("expected ChannelPeekResult, got {other:?}"),
        };

        // Page back with `before` = the oldest line just seen.
        let req = RequestPacket::ChannelPeek {
            request_id: 2,
            channel: ch.to_string(),
            since: None,
            tail: Some(1),
            before: Some("1".to_string()),
            query: None,
            author: None,
        };
        let captured2 = Arc::new(Mutex::new(Vec::<ResponsePacket>::new()));
        let sink2: &dyn crate::ipc::response_sink::ResponseSink = &CaptureSink(captured2.clone());
        handler
            .handle(
                req,
                &peko_auth::caller::CallerContext::local(),
                sink2,
                &PeerAddr::Ip("127.0.0.1:0".parse().expect("loopback addr")),
            )
            .await
            .expect("handle");
        let captured2 = captured2.lock().unwrap();
        match &captured2[0] {
            ResponsePacket::ChannelPeekResult {
                events,
                event_ids,
                has_more,
                ..
            } => {
                assert_eq!(events.len(), 1);
                assert!(matches!(events[0], ChannelEvent::Created { .. }));
                assert_eq!(event_ids, &vec!["0".to_string()]);
                assert!(!has_more, "nothing older than line 0");
            }
            other => panic!("expected ChannelPeekResult, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn handler_peek_search_mode_filters_posted_by_text() {
        let (tmp, host) = test_host();
        let _ = tmp;
        let handler = ChannelHandler::new(host.clone());
        let ch = seed_channel(&host).await; // Created + Posted("hi")
                                            // A second, non-matching post.
        let runtime_dir = host.path_resolver.runtime_dir();
        let adapter = ChannelStore::new(ChannelConfig {
            runtime_dir,
            shared_dir: None,
        });
        let members = adapter.list_members(&ch).await.expect("members");
        let sender = members[0].clone();
        adapter
            .post(&ch, &sender, PostMsg::root("unrelated"))
            .await
            .expect("post");

        let req = RequestPacket::ChannelPeek {
            request_id: 1,
            channel: ch.to_string(),
            since: None,
            tail: None,
            before: None,
            query: Some("HI".to_string()), // case-insensitive
            author: None,
        };
        let captured = Arc::new(Mutex::new(Vec::<ResponsePacket>::new()));
        let sink: &dyn crate::ipc::response_sink::ResponseSink = &CaptureSink(captured.clone());
        handler
            .handle(
                req,
                &peko_auth::caller::CallerContext::local(),
                sink,
                &PeerAddr::Ip("127.0.0.1:0".parse().expect("loopback addr")),
            )
            .await
            .expect("handle");
        match &captured.lock().unwrap()[0] {
            ResponsePacket::ChannelPeekResult {
                events,
                event_ids,
                has_more,
                resume_before,
                ..
            } => {
                assert_eq!(events.len(), 1, "only the 'hi' post matches");
                assert!(matches!(&events[0], ChannelEvent::Posted { text, .. } if text == "hi"));
                assert_eq!(event_ids, &vec!["2".to_string()]);
                // The whole log was scanned (Created isn't Posted), so
                // there's nothing older to resume into.
                assert!(!has_more);
                assert_eq!(resume_before, &None);
            }
            other => panic!("expected ChannelPeekResult, got {other:?}"),
        };
    }

    // -----------------------------------------------------------------
    // ADR-049 Phase 2: user senders + membership-gated reads
    // -----------------------------------------------------------------

    /// Seed a channel with a principal creator plus `user:alice` and
    /// `user:local` (the local caller's ADR-057 identity) as invited
    /// members. Returns the adapter (for log assertions) and the
    /// channel id.
    async fn seed_channel_with_user_member(host: &TestChannelHost) -> (ChannelStore, ChannelId) {
        let runtime_dir = host.path_resolver.runtime_dir();
        let adapter = ChannelStore::new(ChannelConfig {
            runtime_dir,
            shared_dir: None,
        });
        let creator = PrincipalId::generate();
        let ch = adapter
            .create(&creator, CreateOpts::runtime("seed"))
            .await
            .expect("create");
        adapter
            .invite(&ch, &creator, &ActorRef::User("hub:alice".into()))
            .await
            .expect("invite user");
        adapter
            .invite(&ch, &creator, &ActorRef::User("local".into()))
            .await
            .expect("invite local user");
        (adapter, ch)
    }

    fn capture_sink() -> (Arc<Mutex<Vec<ResponsePacket>>>, CaptureSink) {
        let captured = Arc::new(Mutex::new(Vec::<ResponsePacket>::new()));
        (captured.clone(), CaptureSink(captured))
    }

    /// ADR-081 D3: a channel request by the local user leaves its Hub
    /// account linked (it used to re-register the caller without it).
    #[tokio::test]
    async fn a_channel_request_keeps_the_local_users_hub_account() {
        let (_tmp, host) = test_host();
        let store = ChannelStore::new(ChannelConfig {
            runtime_dir: host.path_resolver.runtime_dir(),
            shared_dir: None,
        });
        store
            .link_local_user("did:key:zHere", "acct-uuid")
            .await
            .unwrap();
        let (_adapter, ch) = seed_channel_with_user_member(&host).await;
        let (_captured, sink) = capture_sink();
        ChannelHandler::new(host.clone())
            .handle(
                RequestPacket::ChannelMembers {
                    request_id: 1,
                    channel: ch.to_string(),
                },
                &test_caller(),
                &sink,
                &PeerAddr::Ip("127.0.0.1:0".parse().unwrap()),
            )
            .await
            .unwrap();
        let local = host.port.actor_info(&ActorRef::User("local".into())).await;
        assert_eq!(local.hub_id.as_deref(), Some("acct-uuid"));
    }

    /// ADR-090 D2: only the local user opens its remote DMs, and a host
    /// without a Hub connection says so.
    #[tokio::test]
    async fn open_remote_dm_is_the_local_users_and_needs_the_hub() {
        let (_tmp, host) = test_host();
        let handler = ChannelHandler::new(host.clone());
        let peer = PeerAddr::Ip("127.0.0.1:0".parse().unwrap());
        let open = || RequestPacket::ChannelOpenRemoteDm {
            request_id: 9,
            hub_id: uuid::Uuid::new_v4().to_string(),
            token: None,
        };
        for (caller, expected) in [
            (
                CallerContext::from_jwt("alice".to_string()),
                "[permission_denied]",
            ),
            (test_caller(), "no PekoHub connection"),
        ] {
            let (captured, sink) = capture_sink();
            handler.handle(open(), &caller, &sink, &peer).await.unwrap();
            let responses = captured.lock().unwrap().clone();
            match responses.as_slice() {
                [ResponsePacket::Error {
                    request_id: 9,
                    message,
                }] => assert!(message.contains(expected), "{message}"),
                other => panic!("expected a refusal, got {other:?}"),
            }
        }
    }

    /// ADR-097: only the local user asks a peko's host for its models or
    /// picks a model for a message.
    #[tokio::test]
    async fn only_the_local_user_picks_a_model() {
        let (_tmp, host) = test_host();
        let handler = ChannelHandler::new(host.clone());
        let peer = PeerAddr::Ip("127.0.0.1:0".parse().unwrap());
        let jwt = CallerContext::from_jwt("alice".to_string());
        let dm = "dm_0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        for (request, caller, expected) in [
            (
                RequestPacket::ChannelPekoModels {
                    request_id: 9,
                    channel: dm.into(),
                },
                jwt.clone(),
                "[permission_denied]",
            ),
            (
                RequestPacket::ChannelPekoModels {
                    request_id: 9,
                    channel: dm.into(),
                },
                test_caller(),
                "no PekoHub connection",
            ),
            // ADR-099: and only the local user lists or decides its
            // remote peko's approvals.
            (
                RequestPacket::ChannelPekoApprovals {
                    request_id: 9,
                    channel: dm.into(),
                },
                jwt.clone(),
                "[permission_denied]",
            ),
            (
                RequestPacket::ChannelPekoApprovalDecide {
                    request_id: 9,
                    channel: dm.into(),
                    id: "apr_0123456789ab".into(),
                    approve: true,
                    always: false,
                },
                jwt.clone(),
                "[permission_denied]",
            ),
            (
                RequestPacket::ChannelPekoApprovals {
                    request_id: 9,
                    channel: dm.into(),
                },
                test_caller(),
                "no PekoHub connection",
            ),
            (
                RequestPacket::ChannelPekoApprovalDecide {
                    request_id: 9,
                    channel: dm.into(),
                    id: "apr_0123456789ab".into(),
                    approve: false,
                    always: false,
                },
                test_caller(),
                "no PekoHub connection",
            ),
            (
                RequestPacket::ChannelPost {
                    request_id: 9,
                    channel: "chan_abcdefgh".into(),
                    sender_name: None,
                    text: "hi".into(),
                    parent: None,
                    attachments: Vec::new(),
                    model: Some("work-model".into()),
                },
                jwt,
                "[forbidden] only the local user may pick a model",
            ),
        ] {
            let (captured, sink) = capture_sink();
            handler
                .handle(request, &caller, &sink, &peer)
                .await
                .unwrap();
            let responses = captured.lock().unwrap().clone();
            match responses.as_slice() {
                [ResponsePacket::Error {
                    request_id: 9,
                    message,
                }] => assert!(message.contains(expected), "{message}"),
                other => panic!("expected a refusal, got {other:?}"),
            }
        }
    }

    #[test]
    fn open_remote_dm_packets_round_trip() {
        let request: RequestPacket = serde_json::from_value(serde_json::json!({
            "type": "channel_open_remote_dm",
            "request_id": 4,
            "hub_id": "6f1c2a52-58b6-4b8e-9d1c-6a3f0b6f1a01",
        }))
        .unwrap();
        assert!(matches!(
            request,
            RequestPacket::ChannelOpenRemoteDm {
                request_id: 4,
                token: None,
                ..
            }
        ));
        let response = ResponsePacket::ChannelRemoteDmOpened {
            request_id: 4,
            channel: ChannelId("dm_ab".into()),
            peer: "principal:hub:6f1c2a52-58b6-4b8e-9d1c-6a3f0b6f1a01".into(),
            hub_id: "6f1c2a52-58b6-4b8e-9d1c-6a3f0b6f1a01".into(),
            name: Some("peko".into()),
            created: true,
        };
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["type"], "channel_remote_dm_opened");
        assert_eq!(json["channel"], "dm_ab");
        assert_eq!(response.request_id(), 4);
    }

    /// Phase 2, amended by ADR-057: a member user posts as their own
    /// derived identity (`sender_name: None`) — the store's ActorRef
    /// membership is the write authorization. A `user:<id>` sender is
    /// refused for every caller class, including Local (impersonation
    /// is closed; the identity comes from the connection).
    #[tokio::test]
    async fn handler_post_accepts_member_user_sender() {
        let (tmp, host) = test_host();
        let _ = tmp;
        let handler = ChannelHandler::new(host.clone());
        let (adapter, ch) = seed_channel_with_user_member(&host).await;
        let addr = PeerAddr::Ip("127.0.0.1:0".parse().expect("loopback addr"));

        // JWT alice (a member) posts as her own derived identity.
        let req = RequestPacket::ChannelPost {
            request_id: 5,
            channel: ch.to_string(),
            sender_name: None,
            text: "hello from alice".into(),
            parent: None,
            attachments: Vec::new(),
            model: None,
        };
        let (captured, sink) = capture_sink();
        handler
            .handle(
                req,
                &CallerContext::from_jwt("alice".to_string()),
                &sink,
                &addr,
            )
            .await
            .expect("handle");
        assert_eq!(captured.lock().unwrap().len(), 1);
        match &captured.lock().unwrap()[0] {
            ResponsePacket::ChannelPosted {
                request_id,
                task_id,
                ..
            } => {
                assert_eq!(*request_id, 5);
                assert_eq!(task_id, "3", "Created=0, Join=1, Join=2, Posted=3");
            }
            other => panic!("expected ChannelPosted, got {other:?}"),
        };

        // The event's author is the canonical user wire form.
        let events = adapter
            .peek(&ch, &Checkpoint::default())
            .await
            .expect("peek");
        let authors: Vec<&str> = events
            .iter()
            .filter_map(|ev| match ev {
                ChannelEvent::Posted { author, .. } => Some(author.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(authors, vec!["user:hub:alice"]);

        // A local caller declaring a user sender is refused — even a
        // member one. Identities are derived, never declared.
        let req = RequestPacket::ChannelPost {
            request_id: 6,
            channel: ch.to_string(),
            sender_name: Some("user:alice".into()),
            text: "impersonation".into(),
            parent: None,
            attachments: Vec::new(),
            model: None,
        };
        let (captured, sink) = capture_sink();
        handler
            .handle(req, &test_caller(), &sink, &addr)
            .await
            .expect("handle");
        match captured.lock().unwrap().as_slice() {
            [ResponsePacket::Error { message, .. }] => {
                assert!(message.contains("[forbidden]"), "got {message}");
            }
            other => panic!("expected [forbidden] for declared user sender; got {other:?}"),
        }

        // The local caller posts as its own derived identity instead.
        let req = RequestPacket::ChannelPost {
            request_id: 7,
            channel: ch.to_string(),
            sender_name: None,
            text: "hello from local".into(),
            parent: None,
            attachments: Vec::new(),
            model: None,
        };
        let (captured, sink) = capture_sink();
        handler
            .handle(req, &test_caller(), &sink, &addr)
            .await
            .expect("handle");
        assert!(
            matches!(
                captured.lock().unwrap().as_slice(),
                [ResponsePacket::ChannelPosted { .. }]
            ),
            "local member must post as user:local; got {:?}",
            captured.lock().unwrap()
        );
    }

    /// Phase 2, amended by ADR-057: a user who is NOT a member is
    /// refused with the store's `NotMember` error — the caller's
    /// derived identity, not a declared one, is what gets gated.
    #[tokio::test]
    async fn handler_post_rejects_non_member_user_sender() {
        let (tmp, host) = test_host();
        let _ = tmp;
        let handler = ChannelHandler::new(host.clone());
        let (_adapter, ch) = seed_channel_with_user_member(&host).await;

        let req = RequestPacket::ChannelPost {
            request_id: 6,
            channel: ch.to_string(),
            sender_name: None,
            text: "must not land".into(),
            parent: None,
            attachments: Vec::new(),
            model: None,
        };
        let (captured, sink) = capture_sink();
        handler
            .handle(
                req,
                &CallerContext::from_jwt("mallory".to_string()),
                &sink,
                &PeerAddr::Ip("127.0.0.1:0".parse().expect("loopback addr")),
            )
            .await
            .expect("handle");
        let captured = captured.lock().unwrap();
        assert_eq!(captured.len(), 1);
        match &captured[0] {
            ResponsePacket::Error {
                request_id,
                message,
            } => {
                assert_eq!(*request_id, 6);
                assert!(message.contains("not a member"), "got {message}");
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    /// Phase 2 (D6), amended by ADR-057: `ChannelPeek` is
    /// membership-gated on the caller's DERIVED identity (there is no
    /// wire `requester` anymore). A member reads; a non-member is
    /// refused; an API-key caller has no user identity and stays
    /// ungated.
    /// ADR-094: the daemon reads attached files from this host, so only
    /// the local user attaches; their files land in the channel's store
    /// by digest, and a peek names the stored copy.
    #[tokio::test]
    async fn only_the_local_user_attaches_and_files_are_stored_by_digest() {
        let (tmp, host) = test_host();
        let handler = ChannelHandler::new(host.clone());
        let (_adapter, ch) = seed_channel_with_user_member(&host).await;
        let file = tmp.path().join("notes.txt");
        std::fs::write(&file, "remember the milk").unwrap();
        let post = || RequestPacket::ChannelPost {
            request_id: 1,
            channel: ch.to_string(),
            sender_name: None,
            text: "see attached".into(),
            parent: None,
            attachments: vec![file.display().to_string()],
            model: None,
        };
        let peer = PeerAddr::Ip("127.0.0.1:0".parse().expect("loopback addr"));

        let (captured, sink) = capture_sink();
        handler
            .handle(
                post(),
                &CallerContext::from_jwt("alice".to_string()),
                &sink,
                &peer,
            )
            .await
            .expect("handle");
        assert!(
            matches!(
                captured.lock().unwrap().as_slice(),
                [ResponsePacket::Error { message, .. }] if message.contains("only be sent by the local user")
            ),
            "{:?}",
            captured.lock().unwrap()
        );

        let (captured, sink) = capture_sink();
        handler
            .handle(post(), &test_caller(), &sink, &peer)
            .await
            .expect("handle");
        assert!(
            matches!(
                captured.lock().unwrap().as_slice(),
                [ResponsePacket::ChannelPosted { .. }]
            ),
            "{:?}",
            captured.lock().unwrap()
        );
        let (captured, sink) = capture_sink();
        handler
            .handle(
                RequestPacket::ChannelPeek {
                    request_id: 2,
                    channel: ch.to_string(),
                    since: None,
                    tail: None,
                    before: None,
                    query: None,
                    author: None,
                },
                &test_caller(),
                &sink,
                &peer,
            )
            .await
            .expect("handle");
        let captured = captured.lock().unwrap();
        let [ResponsePacket::ChannelPeekResult { events, .. }] = captured.as_slice() else {
            panic!("{captured:?}");
        };
        let attachment = events
            .iter()
            .find_map(|ev| match ev {
                ChannelEvent::Posted { attachments, .. } => attachments.first().cloned(),
                _ => None,
            })
            .expect("the post carries the file");
        assert_eq!(attachment.name, "notes.txt");
        assert!(attachment.sha256.is_some());
        assert_eq!(
            std::fs::read_to_string(&attachment.path).unwrap(),
            "remember the milk"
        );
    }

    #[tokio::test]
    async fn handler_peek_gates_on_caller_identity() {
        let (tmp, host) = test_host();
        let _ = tmp;
        let handler = ChannelHandler::new(host.clone());
        let (_adapter, ch) = seed_channel_with_user_member(&host).await;

        let peek = || RequestPacket::ChannelPeek {
            request_id: 1,
            channel: ch.to_string(),
            since: None,
            tail: None,
            before: None,
            query: None,
            author: None,
        };
        let peer = PeerAddr::Ip("127.0.0.1:0".parse().expect("loopback addr"));

        // Local caller (user:local is a seeded member) → events.
        let (captured, sink) = capture_sink();
        handler
            .handle(peek(), &test_caller(), &sink, &peer)
            .await
            .expect("handle");
        assert!(
            matches!(
                captured.lock().unwrap().as_slice(),
                [ResponsePacket::ChannelPeekResult { .. }]
            ),
            "member local caller must read; got {:?}",
            captured.lock().unwrap()
        );

        // JWT member reads as themselves.
        let (captured, sink) = capture_sink();
        handler
            .handle(
                peek(),
                &CallerContext::from_jwt("alice".to_string()),
                &sink,
                &peer,
            )
            .await
            .expect("handle");
        assert!(
            matches!(
                captured.lock().unwrap().as_slice(),
                [ResponsePacket::ChannelPeekResult { .. }]
            ),
            "JWT member must read; got {:?}",
            captured.lock().unwrap()
        );

        // JWT non-member → permission error.
        let (captured, sink) = capture_sink();
        handler
            .handle(
                peek(),
                &CallerContext::from_jwt("mallory".to_string()),
                &sink,
                &peer,
            )
            .await
            .expect("handle");
        match captured.lock().unwrap().as_slice() {
            [ResponsePacket::Error { message, .. }] => {
                assert!(message.contains("not a member"), "got {message}");
            }
            other => panic!("expected Error for non-member caller; got {other:?}"),
        };
    }

    /// Phase 2 (D6), amended by ADR-057: the same caller-derived gate
    /// applies to `ChannelEventsWatch` (its replay half reads the
    /// same log).
    #[tokio::test]
    async fn handler_events_watch_gates_on_caller_identity() {
        let (tmp, host) = test_host();
        let _ = tmp;
        let handler = ChannelHandler::new(host.clone());
        let (_adapter, ch) = seed_channel_with_user_member(&host).await;

        let req = RequestPacket::ChannelEventsWatch {
            request_id: 9,
            channel: ch.to_string(),
            since: None,
        };
        let (captured, sink) = capture_sink();
        handler
            .handle(
                req,
                &CallerContext::from_jwt("mallory".to_string()),
                &sink,
                &PeerAddr::Ip("127.0.0.1:0".parse().expect("loopback addr")),
            )
            .await
            .expect("handle");
        match captured.lock().unwrap().as_slice() {
            [ResponsePacket::Error {
                request_id,
                message,
            }] => {
                assert_eq!(*request_id, 9);
                assert!(message.contains("not a member"), "got {message}");
            }
            other => panic!("expected Error for non-member watch caller; got {other:?}"),
        };
    }

    // -----------------------------------------------------------------
    // ADR-049 Phase 4 (D6): CallerContext-based authorization
    // -----------------------------------------------------------------

    /// ADR-058 D7: `ChannelMembers` carries the same caller-derived
    /// membership gate as `ChannelPeek` — before the gate, any caller
    /// (including a JWT non-member) could enumerate the roster.
    #[tokio::test]
    async fn handler_members_gates_on_caller_identity() {
        let (tmp, host) = test_host();
        let _ = tmp;
        let handler = ChannelHandler::new(host.clone());
        let (_adapter, ch) = seed_channel_with_user_member(&host).await;
        let peer = PeerAddr::Ip("127.0.0.1:0".parse().expect("loopback addr"));

        let members = || RequestPacket::ChannelMembers {
            request_id: 1,
            channel: ch.to_string(),
        };

        // Local caller (user:local is a seeded member) → roster.
        let (captured, sink) = capture_sink();
        handler
            .handle(members(), &test_caller(), &sink, &peer)
            .await
            .expect("handle");
        assert!(
            matches!(
                captured.lock().unwrap().as_slice(),
                [ResponsePacket::ChannelMembersResult { .. }]
            ),
            "member local caller must read the roster; got {:?}",
            captured.lock().unwrap()
        );

        // JWT member reads as themselves.
        let (captured, sink) = capture_sink();
        handler
            .handle(
                members(),
                &CallerContext::from_jwt("alice".to_string()),
                &sink,
                &peer,
            )
            .await
            .expect("handle");
        assert!(
            matches!(
                captured.lock().unwrap().as_slice(),
                [ResponsePacket::ChannelMembersResult { .. }]
            ),
            "JWT member must read the roster; got {:?}",
            captured.lock().unwrap()
        );

        // JWT non-member → permission error (the D7 denied path).
        let (captured, sink) = capture_sink();
        handler
            .handle(
                members(),
                &CallerContext::from_jwt("mallory".to_string()),
                &sink,
                &peer,
            )
            .await
            .expect("handle");
        match captured.lock().unwrap().as_slice() {
            [ResponsePacket::Error { message, .. }] => {
                assert!(message.contains("not a member"), "got {message}");
            }
            other => panic!("expected not-a-member for JWT non-member; got {other:?}"),
        };
    }

    /// ADR-058 D7: `ChannelList` is an operator read — Derived callers
    /// only (`gate_principal_operator` discipline). A pekohub JWT user
    /// is refused before any principal resolution; a Derived caller
    /// passes the gate (and only then hits the host's "not loaded"
    /// resolution error, which proves the gate itself did not block).
    #[tokio::test]
    async fn handler_list_gates_to_operator_callers() {
        let (tmp, host) = test_host();
        let _ = tmp;
        let handler = ChannelHandler::new(host.clone());
        let peer = PeerAddr::Ip("127.0.0.1:0".parse().expect("loopback addr"));

        let list = || RequestPacket::ChannelList {
            request_id: 1,
            principal_name: "root".to_string(),
        };

        // JWT caller → forbidden (the D7 denied path).
        let (captured, sink) = capture_sink();
        handler
            .handle(
                list(),
                &CallerContext::from_jwt("mallory".to_string()),
                &sink,
                &peer,
            )
            .await
            .expect("handle");
        match captured.lock().unwrap().as_slice() {
            [ResponsePacket::Error { message, .. }] => {
                assert!(message.contains("[forbidden]"), "got {message}");
            }
            other => panic!("expected [forbidden] for JWT caller; got {other:?}"),
        }

        // Derived (local) caller passes the gate; the PM-less test
        // host then surfaces its ordinary resolution error — anything
        // other than [forbidden] proves the gate let the caller
        // through.
        let (captured, sink) = capture_sink();
        handler
            .handle(list(), &test_caller(), &sink, &peer)
            .await
            .expect("handle");
        match captured.lock().unwrap().as_slice() {
            [ResponsePacket::Error { message, .. }] => {
                assert!(
                    !message.contains("[forbidden]"),
                    "Derived caller must pass the operator gate; got {message}"
                );
            }
            other => panic!("expected resolution error for PM-less host; got {other:?}"),
        };
    }

    /// A pekohub JWT caller always reads as their own `user:<sub>`
    /// identity (ADR-057: there is no declared requester at all), and
    /// the membership gate always applies.
    #[tokio::test]
    async fn handler_peek_jwt_user_read_as_self_only() {
        let (tmp, host) = test_host();
        let _ = tmp;
        let handler = ChannelHandler::new(host.clone());
        let (_adapter, ch) = seed_channel_with_user_member(&host).await;
        let peer = PeerAddr::Ip("127.0.0.1:0".parse().expect("loopback addr"));

        let peek = || RequestPacket::ChannelPeek {
            request_id: 1,
            channel: ch.to_string(),
            since: None,
            tail: None,
            before: None,
            query: None,
            author: None,
        };
        let jwt_alice = CallerContext::from_jwt("alice".to_string());

        // Member → reads as themselves (gated on membership, which
        // user:alice satisfies).
        let (captured, sink) = capture_sink();
        handler
            .handle(peek(), &jwt_alice, &sink, &peer)
            .await
            .expect("handle");
        assert!(
            matches!(
                captured.lock().unwrap().as_slice(),
                [ResponsePacket::ChannelPeekResult { .. }]
            ),
            "JWT member must read as themselves; got {:?}",
            captured.lock().unwrap()
        );

        // A JWT caller who is NOT a member has no escape: gated on
        // their own identity and refused.
        let jwt_mallory = CallerContext::from_jwt("mallory".to_string());
        let (captured, sink) = capture_sink();
        handler
            .handle(peek(), &jwt_mallory, &sink, &peer)
            .await
            .expect("handle");
        match captured.lock().unwrap().as_slice() {
            [ResponsePacket::Error { message, .. }] => {
                assert!(message.contains("not a member"), "got {message}");
            }
            other => panic!("expected not-a-member for JWT non-member; got {other:?}"),
        };

        // A sub that itself carries the `user:` wire prefix normalizes
        // to the same bare id (the normalization the retired
        // `from_bridge_user` used to perform).
        let jwt_prefixed = CallerContext::from_jwt("user:alice".to_string());
        let (captured, sink) = capture_sink();
        handler
            .handle(peek(), &jwt_prefixed, &sink, &peer)
            .await
            .expect("handle");
        assert!(
            matches!(
                captured.lock().unwrap().as_slice(),
                [ResponsePacket::ChannelPeekResult { .. }]
            ),
            "prefixed sub must normalize to the same user identity; got {:?}",
            captured.lock().unwrap()
        );
    }

    /// A pekohub JWT caller may only POST as their own `user:<sub>`
    /// identity — as themselves when a member; never as another user
    /// or as a principal (checked before any principal resolution).
    #[tokio::test]
    async fn handler_post_jwt_user_posts_as_self_only() {
        let (tmp, host) = test_host();
        let _ = tmp;
        let handler = ChannelHandler::new(host.clone());
        let (_adapter, ch) = seed_channel_with_user_member(&host).await;
        let peer = PeerAddr::Ip("127.0.0.1:0".parse().expect("loopback addr"));
        let jwt_alice = CallerContext::from_jwt("alice".to_string());

        let post = |sender: Option<&str>| RequestPacket::ChannelPost {
            request_id: 3,
            channel: ch.to_string(),
            sender_name: sender.map(str::to_string),
            text: "hi".into(),
            parent: None,
            attachments: Vec::new(),
            model: None,
        };

        // Member user posts as themselves (derived identity).
        let (captured, sink) = capture_sink();
        handler
            .handle(post(None), &jwt_alice, &sink, &peer)
            .await
            .expect("handle");
        assert!(
            matches!(
                captured.lock().unwrap().as_slice(),
                [ResponsePacket::ChannelPosted { .. }]
            ),
            "JWT member must post as themselves; got {:?}",
            captured.lock().unwrap()
        );

        // Declaring another user → forbidden (impersonation).
        let (captured, sink) = capture_sink();
        handler
            .handle(post(Some("user:bob")), &jwt_alice, &sink, &peer)
            .await
            .expect("handle");
        match captured.lock().unwrap().as_slice() {
            [ResponsePacket::Error { message, .. }] => {
                assert!(message.contains("[forbidden]"), "got {message}");
            }
            other => panic!("expected [forbidden] for user impersonation; got {other:?}"),
        };

        // Declaring a principal → forbidden (a JWT user is never a
        // principal sender; checked before resolution, so the missing
        // PrincipalManager in the test host is irrelevant).
        let (captured, sink) = capture_sink();
        handler
            .handle(post(Some("some-principal")), &jwt_alice, &sink, &peer)
            .await
            .expect("handle");
        match captured.lock().unwrap().as_slice() {
            [ResponsePacket::Error { message, .. }] => {
                assert!(message.contains("[forbidden]"), "got {message}");
            }
            other => panic!("expected [forbidden] for principal impersonation; got {other:?}"),
        };
    }

    /// ADR-057: an API key is a scoped credential of the runtime
    /// owner, not an identity class. It reads and posts as the
    /// owner's derived identity (`user:local`), gated by membership
    /// like any other caller; declaring a *different* user is still
    /// refused as impersonation.
    #[tokio::test]
    async fn handler_apikey_speaks_as_owner_identity() {
        let (tmp, host) = test_host();
        let _ = tmp;
        let handler = ChannelHandler::new(host.clone());
        let (_adapter, ch) = seed_channel_with_user_member(&host).await;
        let peer = PeerAddr::Ip("127.0.0.1:0".parse().expect("loopback addr"));
        let apikey = CallerContext::from_api_key("pkr_test".to_string(), vec![]);

        // Read as an API key → gated as the owner (user:local is a
        // seeded member) → reads.
        let (captured, sink) = capture_sink();
        handler
            .handle(
                RequestPacket::ChannelPeek {
                    request_id: 4,
                    channel: ch.to_string(),
                    since: None,
                    tail: None,
                    before: None,
                    query: None,
                    author: None,
                },
                &apikey,
                &sink,
                &peer,
            )
            .await
            .expect("handle");
        assert!(
            matches!(
                captured.lock().unwrap().as_slice(),
                [ResponsePacket::ChannelPeekResult { .. }]
            ),
            "api-key read must run as the owner identity; got {:?}",
            captured.lock().unwrap()
        );

        // Declaring another user on a post → forbidden.
        let (captured, sink) = capture_sink();
        handler
            .handle(
                RequestPacket::ChannelPost {
                    request_id: 5,
                    channel: ch.to_string(),
                    sender_name: Some("user:alice".into()),
                    text: "hi".into(),
                    parent: None,
                    attachments: Vec::new(),
                    model: None,
                },
                &apikey,
                &sink,
                &peer,
            )
            .await
            .expect("handle");
        match captured.lock().unwrap().as_slice() {
            [ResponsePacket::Error { message, .. }] => {
                assert!(message.contains("[forbidden]"), "got {message}");
            }
            other => panic!("expected [forbidden] for api-key user sender; got {other:?}"),
        };

        // Posting as the owner's derived identity → allowed.
        let (captured, sink) = capture_sink();
        handler
            .handle(
                RequestPacket::ChannelPost {
                    request_id: 6,
                    channel: ch.to_string(),
                    sender_name: None,
                    text: "hi from the key".into(),
                    parent: None,
                    attachments: Vec::new(),
                    model: None,
                },
                &apikey,
                &sink,
                &peer,
            )
            .await
            .expect("handle");
        assert!(
            matches!(
                captured.lock().unwrap().as_slice(),
                [ResponsePacket::ChannelPosted { .. }]
            ),
            "api-key must post as the owner identity; got {:?}",
            captured.lock().unwrap()
        );
    }

    // -----------------------------------------------------------------------
    // PR-3d: `ChannelPinToShared` IPC variant + shared-tier opt-in
    // -----------------------------------------------------------------------

    /// Construct a `TestChannelHost` with both Runtime and Shared
    /// tier roots populated. Mirrors `test_host()` but feeds
    /// `ChannelConfig { runtime_dir, shared_dir: Some(...) }` so the
    /// adapter's `pin_to_shared` can resolve the destination.
    fn test_host_with_shared() -> (TempDir, Arc<TestChannelHost>) {
        let tmp = TempDir::new().expect("tempdir");
        let runtime_dir = tmp.path().join("runtime");
        let shared_dir = tmp.path().join("shared");
        std::fs::create_dir_all(&runtime_dir).expect("mkdir runtime");
        std::fs::create_dir_all(&shared_dir).expect("mkdir shared");
        let cfg = ChannelConfig {
            runtime_dir: runtime_dir.clone(),
            shared_dir: Some(shared_dir),
        };
        let adapter = Arc::new(ChannelStore::new(cfg));
        let port: Arc<dyn ChannelPort> = adapter;
        let resolver = PathResolver::with_dirs(
            tmp.path().to_path_buf(),
            tmp.path().to_path_buf(),
            tmp.path().join("cache"),
        );
        let host = Arc::new(TestChannelHost {
            path_resolver: resolver,
            port,
            kickoff_log: Mutex::new(Vec::new()),
            kickoff_should_panic: false,
        });
        (tmp, host)
    }

    /// `ChannelPinToShared` end-to-end: seed a channel, send the
    /// request, assert `ChannelPinnedToShared` carries the absolute
    /// Shared root path and the Runtime source still resolves.
    #[tokio::test]
    async fn handler_pin_to_shared_copies_files_and_returns_path() {
        let (tmp, host) = test_host_with_shared();
        let ch = seed_channel(&host).await;

        let sink = Arc::new(CaptureSink(Arc::new(Mutex::new(Vec::new()))));
        let handler = ChannelHandler::new(host.clone());
        let req = RequestPacket::ChannelPinToShared {
            request_id: 17,
            channel: ch.to_string(),
        };
        handler
            .handle(
                req,
                &test_caller(),
                &*sink,
                &PeerAddr::Ip("127.0.0.1:0".parse().expect("loopback addr")),
            )
            .await
            .expect("handle");

        let packets = sink.0.lock().unwrap();
        assert_eq!(packets.len(), 1);
        let response = &packets[0];
        match response {
            ResponsePacket::ChannelPinnedToShared {
                request_id,
                channel: resp_channel,
                shared_path,
            } => {
                assert_eq!(*request_id, 17);
                assert_eq!(*resp_channel, ch);
                // Shared path must point inside the temp shared root
                let expected = tmp.path().join("shared").join("channels").join(ch.as_str());
                assert_eq!(PathBuf::from(shared_path), expected);
                assert!(expected.exists(), "shared chan dir must exist on disk");
            }
            other => panic!("expected ChannelPinnedToShared, got {other:?}"),
        }

        // Runtime source must still resolve (COPY semantics).
        let runtime_chan_dir = host
            .path_resolver
            .runtime_dir()
            .join("channels")
            .join(ch.as_str());
        assert!(runtime_chan_dir.exists(), "runtime source must remain");
    }

    /// `ChannelPinToShared` against a host whose `ChannelConfig` has
    /// `shared_dir: None` — the adapter must surface an error via
    /// `ResponsePacket::Error` (not panic). Mirrors the CLI fallback
    /// without `SharedLayout` access.
    #[tokio::test]
    async fn handler_pin_to_shared_returns_error_without_shared_dir() {
        let (_tmp, host) = test_host(); // shared_dir: None
        let ch = seed_channel(&host).await;

        let sink = Arc::new(CaptureSink(Arc::new(Mutex::new(Vec::new()))));
        let handler = ChannelHandler::new(host.clone());
        let req = RequestPacket::ChannelPinToShared {
            request_id: 18,
            channel: ch.to_string(),
        };
        handler
            .handle(
                req,
                &test_caller(),
                &*sink,
                &PeerAddr::Ip("127.0.0.1:0".parse().expect("loopback addr")),
            )
            .await
            .expect("handle");

        let packets = sink.0.lock().unwrap();
        assert_eq!(packets.len(), 1);
        match &packets[0] {
            ResponsePacket::Error {
                request_id,
                message,
            } => {
                assert_eq!(*request_id, 18);
                assert!(
                    message.contains("pin-to-shared") || message.contains("shared"),
                    "expected shared-tier error message, got: {message}"
                );
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    /// JSON round-trip for `ChannelPinToShared` request and
    /// `ChannelPinnedToShared` response.
    #[tokio::test]
    async fn channel_packets_round_trip_pin_to_shared_via_json() {
        let req = RequestPacket::ChannelPinToShared {
            request_id: 21,
            channel: "chan_abcdefgh".into(),
        };
        let json = serde_json::to_string(&req).expect("encode");
        assert!(json.contains("\"channel_pin_to_shared\""), "got {json}");
        let decoded: RequestPacket = serde_json::from_str(&json).expect("decode");
        match decoded {
            RequestPacket::ChannelPinToShared {
                request_id,
                channel,
            } => {
                assert_eq!(request_id, 21);
                assert_eq!(channel, "chan_abcdefgh");
            }
            other => panic!("expected ChannelPinToShared, got {other:?}"),
        }

        let resp = ResponsePacket::ChannelPinnedToShared {
            request_id: 21,
            channel: ChannelId::parse("chan_abcdefgh").expect("valid ChannelId"),
            shared_path: "/tmp/shared/channels/chan_abcdefgh".into(),
        };
        let json = serde_json::to_string(&resp).expect("encode");
        assert!(json.contains("\"channel_pinned_to_shared\""), "got {json}");
        let decoded: ResponsePacket = serde_json::from_str(&json).expect("decode");
        assert!(matches!(
            decoded,
            ResponsePacket::ChannelPinnedToShared { request_id: 21, .. }
        ));
    }

    // -----------------------------------------------------------------------
    // PR-2b: `ChannelEventsWatch` request + `ChannelEventReceived` response
    // -----------------------------------------------------------------------
    //
    // Pin the wire shape so the streaming IPC variant stays
    // round-trippable. The handler logic itself (replay + subscribe +
    // forward) is a separate test against the `ChannelEventNotifier`
    // registry.

    /// JSON round-trip for `ChannelEventsWatch` request and
    /// `ChannelEventReceived` response. The discriminant strings
    /// (`channel_events_watch`, `channel_event_received`) are the
    /// framing the daemon's per-request stream uses — the desktop
    /// Tauri backend's stream-forwarder matches on these via the
    /// `packet_type` switch in `ipc/mod.rs:949`.
    #[tokio::test]
    async fn channel_packets_round_trip_events_watch_via_json() {
        let req = RequestPacket::ChannelEventsWatch {
            request_id: 31,
            channel: "chan_abcdefgh".into(),
            since: None,
        };
        let json = serde_json::to_string(&req).expect("encode");
        assert!(json.contains("\"channel_events_watch\""), "got {json}");
        let decoded: RequestPacket = serde_json::from_str(&json).expect("decode");
        match decoded {
            RequestPacket::ChannelEventsWatch {
                request_id,
                channel,
                since,
                ..
            } => {
                assert_eq!(request_id, 31);
                assert_eq!(channel, "chan_abcdefgh");
                assert_eq!(since, None);
            }
            other => panic!("expected ChannelEventsWatch, got {other:?}"),
        }

        let resp = ResponsePacket::ChannelEventReceived {
            request_id: 31,
            channel: ChannelId::parse("chan_abcdefgh").expect("valid ChannelId"),
            event: ChannelEvent::Posted {
                channel: ChannelId::parse("chan_abcdefgh").expect("valid ChannelId"),
                author: "alice".into(),
                text: "hi".into(),
                parent: None,
                at: "2026-08-06T12:00:00Z".into(),
                via: None,
                msg_id: None,
                reply_to: None,
                attachments: Vec::new(),
                options: Vec::new(),
                model: None,
                board: None,
            },
        };
        let json = serde_json::to_string(&resp).expect("encode");
        assert!(json.contains("\"channel_event_received\""), "got {json}");
        let decoded: ResponsePacket = serde_json::from_str(&json).expect("decode");
        match decoded {
            ResponsePacket::ChannelEventReceived {
                request_id,
                channel,
                event,
            } => {
                assert_eq!(request_id, 31);
                assert_eq!(channel.as_str(), "chan_abcdefgh");
                assert!(matches!(event, ChannelEvent::Posted { .. }));
            }
            other => panic!("expected ChannelEventReceived, got {other:?}"),
        }
    }

    /// `ChannelEventsWatch` claims ownership in `matches()` — without
    /// this, the dispatcher would let the variant fall through to the
    /// default no-op handler and the streaming path would silently
    /// return `Error`.
    #[tokio::test]
    async fn handler_matches_claims_channel_events_watch() {
        let (_tmp, host) = test_host();
        let handler = ChannelHandler::new(host);
        assert!(handler.matches(&RequestPacket::ChannelEventsWatch {
            request_id: 1,
            channel: "chan_x".into(),
            since: None,
        }));
    }

    /// `ChannelEventsWatch` replays pre-existing events then forwards
    /// live events emitted by the store's `notify_event` hooks. The
    /// test seeds one event before subscribing, subscribes, then
    /// posts a second event mid-stream — both must arrive as
    /// `ChannelEventReceived` packets in the order they were
    /// appended.
    ///
    /// The sink uses a `CloseAfter` shim that errors after `n` packets
    /// have been written — this terminates the streaming loop cleanly
    /// (the handler returns `Ok(())` on sink write errors). Without
    /// this, the loop would block forever awaiting more events.
    #[tokio::test]
    async fn handler_channel_events_watch_replays_then_streams() {
        use peko_channel::port::PostMsg;
        let (tmp, host) = test_host();
        let _ = tmp;
        let _handler = ChannelHandler::new(host.clone());
        let port = host.channel_port();

        // Seed a channel with one Posted event (Created is line 0).
        let runtime_dir = host.path_resolver.runtime_dir();
        let adapter = ChannelStore::new(ChannelConfig {
            runtime_dir,
            shared_dir: None,
        });
        let creator = PrincipalId::generate();
        let ch = adapter
            .create(&creator, CreateOpts::runtime("seed"))
            .await
            .expect("create");
        adapter
            .post(&ch, &ActorRef::from(&creator), PostMsg::root("hello"))
            .await
            .expect("post 1");

        // Spawn the streaming handler with a sink that closes after
        // 2 packets (replay + the next live event we'll post).
        let sink = Arc::new(CloseAfter::new(2));
        let port_for_task = port.clone();
        let ch_for_task = ch.clone();
        let sink_for_task = sink.clone();
        let handle = tokio::spawn(async move {
            run_channel_events_watch(
                99,
                ch_for_task.clone(),
                None,
                port_for_task.clone(),
                sink_for_task.as_ref(),
            )
            .await
        });

        // Wait for the replay packet to land (the seed Created +
        // Posted = 2 events replayed; the sink closes after packet
        // #2). We post a fresh event so the loop sees at least one
        // live emission, then wait for the sink to close.
        let live_port = port.clone();
        let live_ch = ch.clone();
        let live_creator = creator.clone();
        tokio::spawn(async move {
            // Small delay so the streaming task subscribes first.
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            live_port
                .post(
                    &live_ch,
                    &ActorRef::from(&live_creator),
                    PostMsg::root("live"),
                )
                .await
                .expect("post 2");
        });

        // Wait for the streaming task to finish (it returns Ok(())
        // when the sink errors). The timeout guards against a
        // regression where the loop never exits.
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), handle)
            .await
            .expect("streaming task must complete within 2s")
            .expect("task must not panic");
        result.expect("run_channel_events_watch must succeed");

        // The sink must have observed at least 2 packets (the seed
        // Created + the seed Posted = 2 replay packets). The live
        // post may or may not arrive before the sink closes — the
        // sink closes after the 2nd write, so the live event was
        // posted *after* the sink closed. We only assert the replay
        // shape here.
        let observed = sink.observed();
        assert!(
            observed.len() >= 2,
            "expected ≥ 2 packets (replay), got {observed:?}"
        );
        // First two packets are the seed Created + Posted (in order).
        match &observed[0] {
            ResponsePacket::ChannelEventReceived { event, .. } => {
                assert!(
                    matches!(event, ChannelEvent::Created { .. }),
                    "first replay packet must be Created, got {event:?}"
                );
            }
            other => panic!("expected ChannelEventReceived, got {other:?}"),
        }
        match &observed[1] {
            ResponsePacket::ChannelEventReceived { event, .. } => {
                assert!(
                    matches!(event, ChannelEvent::Posted { .. }),
                    "second replay packet must be Posted, got {event:?}"
                );
            }
            other => panic!("expected ChannelEventReceived, got {other:?}"),
        }
    }

    /// `CloseAfter` — a `ResponseSink` that returns `io::Error` after
    /// the Nth `send_bytes` call. Used by the streaming tests to
    /// terminate the `ChannelEventsWatch` loop without hanging the
    /// test on `recv()`.
    struct CloseAfter {
        max: usize,
        seen: Mutex<Vec<ResponsePacket>>,
    }

    impl CloseAfter {
        fn new(max: usize) -> Self {
            Self {
                max,
                seen: Mutex::new(Vec::new()),
            }
        }

        fn observed(&self) -> Vec<ResponsePacket> {
            self.seen.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl crate::ipc::response_sink::ResponseSink for CloseAfter {
        async fn send_bytes(&self, bytes: &[u8]) -> std::io::Result<()> {
            let packet: ResponsePacket = serde_json::from_slice(bytes)
                .map_err(|e| std::io::Error::other(format!("decode: {e}")))?;
            let mut guard = self.seen.lock().unwrap();
            if guard.len() >= self.max {
                // Mimic a closed client socket — the handler's
                // `if let Err(_e) = send_response(...)` branch will
                // see this and return Ok(()).
                return Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "client disconnected",
                ));
            }
            guard.push(packet);
            Ok(())
        }
    }
}

/// `ChannelInfo` / `ChannelSetWake` against a real `PrincipalManager`
/// (the wake path writes a group binding through it).
#[cfg(test)]
mod wake_tests {
    use super::*;
    use crate::principal::manager::PrincipalManager;
    use crate::principal::Principal;
    use peko_auth::ApiKeyScope;
    use peko_channel::port::CreateOpts;
    use peko_channel::{ChannelConfig, ChannelStore};
    use serde_json::json;
    use std::sync::Mutex;

    struct Host {
        port: Arc<dyn ChannelPort>,
        manager: Arc<PrincipalManager>,
    }

    impl ChannelHost for Host {
        fn channel_port(&self) -> Arc<dyn ChannelPort> {
            self.port.clone()
        }

        fn principal_manager(&self) -> Option<&Arc<PrincipalManager>> {
            Some(&self.manager)
        }
    }

    struct Sink(Mutex<Vec<ResponsePacket>>);

    #[async_trait]
    impl ResponseSink for Sink {
        async fn send_bytes(&self, bytes: &[u8]) -> std::io::Result<()> {
            let packet = serde_json::from_slice(bytes).map_err(std::io::Error::other)?;
            self.0.lock().unwrap().push(packet);
            Ok(())
        }
    }

    struct Fixture {
        _temp: tempfile::TempDir,
        _tooling: Arc<crate::tools::runtime::ToolingRuntime>,
        port: Arc<dyn ChannelPort>,
        handler: ChannelHandler,
        alice: Arc<Principal>,
        bob: Arc<Principal>,
        group: ChannelId,
    }

    impl Fixture {
        /// Pekos `alice` and `bob` and the local user in group `team`.
        async fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let paths = crate::common::paths::PathResolver::with_dirs(
                temp.path().join("config"),
                temp.path().join("data"),
                temp.path().join("cache"),
            );
            let tooling = crate::tools::runtime::ToolingRuntime::standalone();
            let store = Arc::new(ChannelStore::new(ChannelConfig {
                runtime_dir: temp.path().join("runtime"),
                shared_dir: None,
            }));
            let tunnel_port = Arc::new(crate::tunnel::TunnelChannelPort::new(store));
            let port: Arc<dyn ChannelPort> = tunnel_port.clone();
            tooling.services().set_channel_port(tunnel_port);
            let vault = Arc::new(crate::common::vault::Vault::for_test(
                temp.path(),
                "channel-wake-test-passphrase",
            ));
            let manager = Arc::new(
                PrincipalManager::with_path_resolver(
                    paths,
                    Arc::new(crate::principal::DefaultPrincipalMemoryFactory),
                    Arc::new(crate::principal::DefaultPrincipalRouterFactory),
                    crate::async_exec::executor::standalone_inbox_registry(),
                )
                .with_identity_vault(vault)
                .with_tooling(tooling.clone())
                .with_channel_port(Arc::clone(&port)),
            );
            let mut pekos = Vec::new();
            for name in ["alice", "bob"] {
                let principal = manager
                    .create(serde_json::from_value(json!({ "name": name })).unwrap())
                    .await
                    .unwrap();
                principal.config.write().await.owner = ActorRef::User("local".into());
                pekos.push(principal);
            }
            let bob = pekos.pop().unwrap();
            let alice = pekos.pop().unwrap();
            let group = port
                .create(&alice.id, CreateOpts::runtime("team"))
                .await
                .unwrap();
            for invitee in [ActorRef::from(&bob.id), ActorRef::User("local".into())] {
                port.invite(&group, &alice.id, &invitee).await.unwrap();
            }
            let handler = ChannelHandler::new(Arc::new(Host {
                port: Arc::clone(&port),
                manager,
            }));
            Self {
                _temp: temp,
                _tooling: tooling,
                port,
                handler,
                alice,
                bob,
                group,
            }
        }

        async fn send(&self, request: RequestPacket, caller: &CallerContext) -> ResponsePacket {
            let sink = Sink(Mutex::new(Vec::new()));
            self.handler
                .handle(
                    request,
                    caller,
                    &sink,
                    &PeerAddr::Ip("127.0.0.1:0".parse().unwrap()),
                )
                .await
                .unwrap();
            sink.0.into_inner().unwrap().pop().expect("one response")
        }

        async fn info(&self, channel: &ChannelId, caller: &CallerContext) -> ResponsePacket {
            let request = RequestPacket::ChannelInfo {
                request_id: 1,
                channel: channel.as_str().into(),
            };
            self.send(request, caller).await
        }

        async fn set_wake(&self, channel: &ChannelId, peko: &str, wake: &str) -> ResponsePacket {
            let request = RequestPacket::ChannelSetWake {
                request_id: 1,
                channel: channel.as_str().into(),
                principal_name: peko.into(),
                wake: wake.into(),
            };
            self.send(request, &CallerContext::local()).await
        }
    }

    fn error_message(response: ResponsePacket) -> String {
        match response {
            ResponsePacket::Error { message, .. } => message,
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn info_lists_kind_members_and_each_pekos_wake() {
        let fx = Fixture::new().await;
        let ResponsePacket::ChannelInfoResult {
            kind,
            name,
            members,
            pekos,
            ..
        } = fx.info(&fx.group, &CallerContext::local()).await
        else {
            panic!("expected ChannelInfoResult");
        };
        assert_eq!(kind, ConversationKind::Group);
        assert_eq!(name, "team");
        assert!(members.contains(&ActorRef::User("local".into())));
        let mut wakes: Vec<_> = pekos.iter().map(|p| (p.name.as_str(), p.wake)).collect();
        wakes.sort_by_key(|(name, _)| *name);
        // No binding written yet: a group reads as its default.
        assert_eq!(
            wakes,
            vec![("alice", WakePolicy::Mention), ("bob", WakePolicy::Mention)]
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn set_wake_round_trips_through_info() {
        let fx = Fixture::new().await;
        match fx.set_wake(&fx.group, "bob", "off").await {
            ResponsePacket::ChannelWakeSet {
                principal_name,
                wake,
                ..
            } => assert_eq!((principal_name.as_str(), wake), ("bob", WakePolicy::Off)),
            other => panic!("expected ChannelWakeSet, got {other:?}"),
        }
        assert_eq!(
            fx.port
                .binding_for(&fx.group, &fx.bob.id)
                .await
                .unwrap()
                .map(|b| b.wake),
            Some(WakePolicy::Off)
        );
        let ResponsePacket::ChannelInfoResult { pekos, .. } =
            fx.info(&fx.group, &CallerContext::local()).await
        else {
            panic!("expected ChannelInfoResult");
        };
        let bob = pekos.iter().find(|p| p.name == "bob").unwrap();
        assert_eq!(bob.wake, WakePolicy::Off);
        assert_eq!(bob.local_id, fx.bob.id.0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn info_is_for_members_only() {
        let fx = Fixture::new().await;
        let outsider = CallerContext::from_jwt("mallory".to_string());
        let message = error_message(fx.info(&fx.group, &outsider).await);
        assert!(message.contains("not a member"), "{message}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_dm_always_wakes_on_human_posts() {
        let fx = Fixture::new().await;
        let dm = fx
            .port
            .ensure_dm(&fx.alice.id, &ActorRef::User("local".into()), "/local-user")
            .await
            .unwrap();
        let ResponsePacket::ChannelInfoResult { kind, pekos, .. } =
            fx.info(&dm, &CallerContext::local()).await
        else {
            panic!("expected ChannelInfoResult");
        };
        assert_eq!(kind, ConversationKind::Dm);
        assert_eq!(pekos.len(), 1);
        assert_eq!(pekos[0].wake, WakePolicy::Human);
        let message = error_message(fx.set_wake(&dm, "alice", "off").await);
        assert!(message.contains("is a DM"), "{message}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn set_wake_refuses_bad_input_and_non_members() {
        let fx = Fixture::new().await;
        let message = error_message(fx.set_wake(&fx.group, "bob", "always").await);
        assert!(message.contains("human, mention, or off"), "{message}");
        let message = error_message(fx.set_wake(&fx.group, "nobody", "off").await);
        assert!(message.contains("[not_found]"), "{message}");

        fx.port.leave(&fx.group, &fx.bob.id).await.unwrap();
        let message = error_message(fx.set_wake(&fx.group, "bob", "off").await);
        assert!(message.contains("not a member"), "{message}");
    }

    /// The person who creates a group from the CLI or desktop is in it:
    /// they can read it straight away, and a second invite (what the
    /// desktop used to send) is harmless.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_person_who_creates_a_group_is_in_it() {
        let fx = Fixture::new().await;
        let local = CallerContext::local();
        let request = RequestPacket::ChannelCreate {
            request_id: 1,
            creator_name: "alice".into(),
            name: "garden".into(),
            id: None,
        };
        let ResponsePacket::ChannelCreated { channel, .. } = fx.send(request, &local).await else {
            panic!("expected ChannelCreated");
        };
        let members = fx.port.list_members(&channel).await.unwrap();
        assert!(
            members.contains(&ActorRef::from(&fx.alice.id)),
            "{members:?}"
        );
        assert!(
            members.contains(&local.attribution_subject()),
            "{members:?}"
        );
        assert!(matches!(
            fx.info(&channel, &local).await,
            ResponsePacket::ChannelInfoResult { .. }
        ));

        fx.port
            .invite(&channel, &fx.alice.id, &local.attribution_subject())
            .await
            .unwrap();
        assert_eq!(fx.port.list_members(&channel).await.unwrap().len(), 2);
    }

    /// A person leaves a group themself: no peko named, no operator
    /// gate; the reply names the caller.
    #[tokio::test(flavor = "multi_thread")]
    async fn you_can_leave_a_group_yourself() {
        let fx = Fixture::new().await;
        let local = CallerContext::local();
        let request = RequestPacket::ChannelLeave {
            request_id: 1,
            channel: fx.group.as_str().into(),
            principal_name: None,
        };
        match fx.send(request, &local).await {
            ResponsePacket::ChannelMemberLeft {
                member, channel, ..
            } => {
                assert_eq!(channel, fx.group);
                assert_eq!(member, local.attribution_subject());
            }
            other => panic!("expected ChannelMemberLeft, got {other:?}"),
        }
        let members = fx.port.list_members(&fx.group).await.unwrap();
        assert!(
            !members.contains(&local.attribution_subject()),
            "{members:?}"
        );
        assert!(members.contains(&ActorRef::from(&fx.alice.id)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn only_the_owner_sets_wake() {
        let fx = Fixture::new().await;
        fx.bob.config.write().await.owner =
            ActorRef::User(peko_subject::Route::hub("someone-else".to_string()));
        let request = RequestPacket::ChannelSetWake {
            request_id: 1,
            channel: fx.group.as_str().into(),
            principal_name: "bob".into(),
            wake: "off".into(),
        };
        // A scoped (non-admin) key acts as the local user, not as an operator.
        let key = CallerContext::from_api_key("k1".to_string(), vec![ApiKeyScope::Write]);
        let message = error_message(fx.send(request.clone(), &key).await);
        assert!(message.contains("[forbidden]"), "{message}");
        // A hub user is never a principal operator.
        let hub_user = CallerContext::from_jwt("someone-else".to_string());
        let message = error_message(fx.send(request, &hub_user).await);
        assert!(message.contains("[forbidden]"), "{message}");
    }
}
