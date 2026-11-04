//! People DM people (ADR-095 D6). A child module of `tunnel_channel_port`
//! so it shares the port's store, tunnel context, and history.
//!
//! - **Opening** ([`TunnelChannelPort::open_person_dm`]): the DM is an
//!   ordinary `dm` between `user:hub:<a>` and `user:hub:<b>` at their pair
//!   id, kept on `<a>`'s runtime, and an invite goes to `<b>`'s account.
//!   Until a route of `<b>` exists, `<a>`'s posts stay in `<a>`'s copy and
//!   the invite is resent on `tunnel_ready` and `peer_runtime_online`.
//! - **A message request** ([`TunnelChannelPort::on_person_dm_invite`]):
//!   each of `<b>`'s runtimes keeps it and says nothing back.
//! - **Accepting** ([`TunnelChannelPort::answer_dm_request`]): the DM is
//!   created here, the asker's runtime becomes a route and hears of this
//!   one (`tunnel_dm_route`), and `<a>`'s posts come over as history.
//! - **Routes** ([`TunnelChannelPort::on_dm_route`]): an announcement
//!   carries every route its sender knows; a receiver records what it
//!   lacks and announces itself to each runtime it just learned of, so
//!   every route of both people comes to know every other.
//! - **Blocking** drops a person's requests, posts, routes, and history
//!   requests on this runtime; the list travels with the account DM list
//!   (D5) to the person's other runtimes.

use super::TunnelChannelPort;
use crate::tunnel::{
    did_key_to_verifying_key, sign_history, verify_dm_route, verify_person_dm_invite, DmRoute,
    DmRoutePayload, PersonDmInvitePayload, TunnelMessage, DM_ROUTE_VERSION,
    PERSON_DM_INVITE_VERSION,
};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use peko_channel::{ChannelId, ChannelPort, DmRequest, Registration, RemoteMember};
use peko_subject::{ActorRef, Route};
use std::collections::HashSet;
use std::sync::Arc;
use tracing::{debug, warn};

/// A person's DM as [`TunnelChannelPort::open_person_dm`] opened it.
#[derive(Debug, Clone)]
pub struct PersonDm {
    pub channel: ChannelId,
    /// The other person, `user:hub:<account>`.
    pub peer: String,
    /// Whether this call created the DM here.
    pub created: bool,
}

/// What a person may do with a DM request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmRequestAnswer {
    Accept,
    Decline,
    /// Decline, and refuse everything from that person from now on.
    Block,
}

fn person(account: &str) -> String {
    format!("user:hub:{account}")
}

/// The Hub account of `address` when it is a person on the Hub.
fn person_account(address: &str) -> Option<String> {
    match address.parse::<ActorRef>() {
        Ok(person @ ActorRef::User(_))
            if person.is_hub_route() && person.canonical_address() == address =>
        {
            person.route().map(|r| r.id().to_string())
        }
        _ => None,
    }
}

/// The pair id of a DM between two people.
#[must_use]
pub fn person_dm_channel_id(a: &str, b: &str) -> ChannelId {
    peko_channel::dm_channel_id(a, b)
}

fn routes_b64u(routes: &[DmRoute]) -> String {
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(routes).unwrap_or_default())
}

impl TunnelChannelPort {
    /// This runtime's person's Hub account, when it is registered.
    async fn my_account(&self) -> Option<String> {
        self.local
            .actor_info(&ActorRef::User("local".into()))
            .await
            .hub_id
    }

    /// Whether `channel` is one of `account`'s DMs with another person,
    /// and with whom.
    pub(crate) async fn person_dm_peer(
        &self,
        account: &str,
        channel: &ChannelId,
    ) -> Option<String> {
        self.local
            .person_dms(account)
            .await
            .ok()?
            .into_iter()
            .find(|(c, _)| c == channel)
            .map(|(_, peer)| peer)
    }

    /// Whether this runtime's person blocked `address` (a person on the
    /// Hub). Anything else is never blocked.
    pub(crate) async fn blocks(&self, address: &str) -> bool {
        match self.my_account().await {
            Some(account) => self.local.is_blocked(&account, address).await,
            None => false,
        }
    }

    /// The people this runtime's person blocked, with the name each is
    /// known by here (from their request, or a DM), for the block list.
    pub async fn blocked_people_named(
        &self,
        account: &str,
    ) -> peko_channel::Result<(Vec<String>, std::collections::BTreeMap<String, String>)> {
        let people = self.local.blocked_people(account).await?;
        let mut names = std::collections::BTreeMap::new();
        for person in &people {
            if let Ok(subject) = person.parse::<ActorRef>() {
                if let Some(name) = self.local.actor_info(&subject).await.name {
                    names.insert(person.clone(), name);
                }
            }
        }
        Ok((people, names))
    }

    /// Record `actor` (a person on the Hub) the way first contact would.
    pub(crate) async fn know_person(
        &self,
        address: &str,
        name: Option<&str>,
    ) -> anyhow::Result<ActorRef> {
        let account = person_account(address)
            .ok_or_else(|| anyhow::anyhow!("{address} is not a person on PekoHub"))?;
        let subject = ActorRef::User(Route::hub(account.clone()));
        if self.local.actor_info(&subject).await.identity_key == subject.canonical_address() {
            self.local
                .register_actor(
                    &subject,
                    Some(&account),
                    None,
                    None,
                    name,
                    Registration::Registered,
                )
                .await?;
        }
        Ok(subject)
    }

    /// Open, or find, this runtime's person's DM with the person whose
    /// PekoHub account is `peer_account` (ADR-095 D6), and ask them when
    /// none of their runtimes is a route yet.
    ///
    /// # Errors
    ///
    /// This runtime is not registered; `peer_account` is not an account,
    /// or is the person's own; the person blocked them.
    pub async fn open_person_dm(
        &self,
        peer_account: &str,
        name: Option<&str>,
    ) -> anyhow::Result<PersonDm> {
        anyhow::ensure!(
            uuid::Uuid::parse_str(peer_account).is_ok(),
            "Expected a person's PekoHub account ID (a UUID), got {peer_account:?}"
        );
        let account = self.my_account().await.ok_or_else(|| {
            anyhow::anyhow!("Register this runtime with PekoHub first (peko runtime register)")
        })?;
        anyhow::ensure!(peer_account != account, "That is your own account");
        let peer = person(peer_account);
        anyhow::ensure!(
            !self.local.is_blocked(&account, &peer).await,
            "You blocked this person; unblock them first"
        );
        let subject = self.know_person(&peer, name).await?;
        let channel = person_dm_channel_id(&person(&account), &peer);
        let created = self
            .local
            .ensure_human_dm(
                &ActorRef::User("local".into()),
                &subject,
                &channel,
                &format!("dm-{}", name.unwrap_or(peer_account)),
                &account,
            )
            .await?;
        // Answered elsewhere: a request for the same pair is moot.
        let _ = self.local.take_dm_request(&channel).await;
        if self.peer_routes(&channel, &peer).await.is_empty() {
            self.send_person_dm_invite(&channel, &account, &peer).await;
        } else {
            self.announce_route(&channel, None).await;
        }
        Ok(PersonDm {
            channel,
            peer,
            created,
        })
    }

    /// The runtimes recorded as routes for `peer` in `channel`.
    async fn peer_routes(&self, channel: &ChannelId, peer: &str) -> Vec<String> {
        self.local
            .list_remote_members(channel)
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|m| m.principal_id == peer)
            .map(|m| m.runtime_id)
            .collect()
    }

    /// Ask `peer`'s account to DM; PekoHub delivers it to their runtimes.
    async fn send_person_dm_invite(&self, channel: &ChannelId, account: &str, peer: &str) {
        let Some(ctx) = self.ctx.read().await.clone() else {
            return;
        };
        let Some(handle) = ctx.tunnel.read().await.clone() else {
            return;
        };
        let name = self.local.person_display_name().await;
        let iat = chrono::Utc::now().timestamp();
        let payload = PersonDmInvitePayload {
            v: PERSON_DM_INVITE_VERSION.into(),
            request_id: uuid::Uuid::new_v4().to_string(),
            source_runtime_id: ctx.caller_runtime_id.clone(),
            recipient_account: peer.to_string(),
            source_principal_did: person(account),
            channel_id: channel.as_str().to_string(),
            name,
            iat,
            exp: iat + 300,
        };
        let signature = sign_history(&ctx.signing_key, &payload);
        if let Err(e) = handle.send(TunnelMessage::TunnelPersonDmInvite {
            request_id: payload.request_id,
            source_runtime_id: payload.source_runtime_id,
            recipient_account: payload.recipient_account,
            source_principal_did: payload.source_principal_did,
            channel_id: payload.channel_id,
            name: payload.name,
            signature,
        }) {
            debug!(%channel, "person DM invite not sent: {e}");
        }
    }

    /// Ask again for every DM with a person none of whose runtimes is a
    /// route yet (on `tunnel_ready`, and when PekoHub reports a runtime
    /// online).
    pub(crate) async fn resend_person_dm_invites(&self) {
        let Some(account) = self.my_account().await else {
            return;
        };
        for (channel, peer) in self.local.person_dms(&account).await.unwrap_or_default() {
            if self.peer_routes(&channel, &peer).await.is_empty()
                && !self.local.is_blocked(&account, &peer).await
            {
                self.send_person_dm_invite(&channel, &account, &peer).await;
            }
        }
    }

    /// A person asks this runtime's person to DM. Kept as a request; on a
    /// DM already accepted here the asker's runtime becomes a route.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn on_person_dm_invite(
        &self,
        request_id: String,
        source_runtime_id: String,
        recipient_account: String,
        source_principal_did: String,
        channel_id: String,
        name: Option<String>,
        signature: String,
    ) {
        let (Some(account), Some(ctx)) = (self.my_account().await, self.ctx.read().await.clone())
        else {
            return;
        };
        if recipient_account != person(&account) {
            return;
        }
        let verified = did_key_to_verifying_key(&source_runtime_id)
            .map_err(|e| anyhow::anyhow!("{e}"))
            .and_then(|key| verify_person_dm_invite(&key, &signature));
        let agrees = verified.as_ref().is_ok_and(|p| {
            p.request_id == request_id
                && p.source_runtime_id == source_runtime_id
                && p.recipient_account == recipient_account
                && p.source_principal_did == source_principal_did
                && p.channel_id == channel_id
                && p.name == name
        });
        if !agrees {
            warn!(request_id, "person DM invite failed verification; dropped");
            return;
        }
        let Some(asker) = person_account(&source_principal_did) else {
            return;
        };
        let channel = person_dm_channel_id(&source_principal_did, &recipient_account);
        if channel.as_str() != channel_id
            || asker == account
            || source_runtime_id == ctx.caller_runtime_id
            || self.local.is_blocked(&account, &source_principal_did).await
        {
            return;
        }
        // Accepted before (here, or on another device whose list reached
        // this one): the asker's runtime is a route.
        if self.person_dm_peer(&account, &channel).await.is_some() {
            let _ = self
                .local
                .add_remote_member(&channel, &source_runtime_id, &source_principal_did)
                .await;
            self.announce_route(&channel, Some(&source_runtime_id))
                .await;
            return;
        }
        let request = DmRequest {
            channel,
            from: source_principal_did,
            name,
            source_runtime: source_runtime_id,
            account,
            received_at: chrono::Utc::now(),
        };
        if let Err(e) = self.local.save_dm_request(&request).await {
            warn!(channel = %request.channel, "DM request not kept: {e}");
        }
    }

    /// The person answers a DM request (ADR-095 D6).
    ///
    /// # Errors
    ///
    /// This runtime is not registered, or no request waits for `channel`.
    pub async fn answer_dm_request(
        self: &Arc<Self>,
        channel: &ChannelId,
        answer: DmRequestAnswer,
    ) -> anyhow::Result<Option<PersonDm>> {
        let account = self
            .my_account()
            .await
            .ok_or_else(|| anyhow::anyhow!("Register this runtime with PekoHub first"))?;
        let request = self
            .local
            .take_dm_request(channel)
            .await?
            .filter(|r| r.account == account)
            .ok_or_else(|| anyhow::anyhow!("No message request waits for {channel}"))?;
        match answer {
            DmRequestAnswer::Decline => Ok(None),
            DmRequestAnswer::Block => {
                // Remember who they were, so the block list can name them.
                let _ = self
                    .know_person(&request.from, request.name.as_deref())
                    .await;
                self.local
                    .set_blocked(&account, std::slice::from_ref(&request.from), true)
                    .await?;
                Ok(None)
            }
            DmRequestAnswer::Accept => {
                let subject = self
                    .know_person(&request.from, request.name.as_deref())
                    .await?;
                let label = request
                    .name
                    .clone()
                    .unwrap_or_else(|| person_account(&request.from).unwrap_or_default());
                let created = self
                    .local
                    .ensure_human_dm(
                        &ActorRef::User("local".into()),
                        &subject,
                        channel,
                        &format!("dm-{label}"),
                        &account,
                    )
                    .await?;
                self.local
                    .add_remote_member(channel, &request.source_runtime, &request.from)
                    .await?;
                self.announce_route(channel, Some(&request.source_runtime))
                    .await;
                let port = Arc::clone(self);
                let caught = channel.clone();
                tokio::spawn(async move {
                    if let Err(e) = port.sync_history(&caught).await {
                        debug!(channel = %caught, "accepted DM not caught up yet: {e}");
                    }
                });
                Ok(Some(PersonDm {
                    channel: channel.clone(),
                    peer: request.from,
                    created,
                }))
            }
        }
    }

    /// Every route this runtime knows for a DM between two people, itself
    /// included.
    async fn known_routes(&self, channel: &ChannelId, account: &str) -> Vec<DmRoute> {
        let Some(ctx) = self.ctx.read().await.clone() else {
            return Vec::new();
        };
        let mut routes = vec![DmRoute {
            runtime_id: ctx.caller_runtime_id.clone(),
            person: person(account),
        }];
        for member in self
            .local
            .list_remote_members(channel)
            .await
            .unwrap_or_default()
        {
            if member.principal_id.starts_with("user:hub:") {
                routes.push(DmRoute {
                    runtime_id: member.runtime_id,
                    person: member.principal_id,
                });
            }
        }
        routes
    }

    /// Tell `to` (or every known route) that this runtime is a route for
    /// its person in `channel`, with every route it knows.
    pub(crate) async fn announce_route(&self, channel: &ChannelId, to: Option<&str>) {
        let (Some(account), Some(ctx)) = (self.my_account().await, self.ctx.read().await.clone())
        else {
            return;
        };
        let Some(handle) = ctx.tunnel.read().await.clone() else {
            return;
        };
        let routes = self.known_routes(channel, &account).await;
        let targets: Vec<String> = match to {
            Some(runtime) => vec![runtime.to_string()],
            None => routes
                .iter()
                .map(|r| r.runtime_id.clone())
                .filter(|r| *r != ctx.caller_runtime_id)
                .collect::<HashSet<_>>()
                .into_iter()
                .collect(),
        };
        for target in targets {
            let iat = chrono::Utc::now().timestamp();
            let payload = DmRoutePayload {
                v: DM_ROUTE_VERSION.into(),
                request_id: uuid::Uuid::new_v4().to_string(),
                source_runtime_id: ctx.caller_runtime_id.clone(),
                recipient_runtime_id: target.clone(),
                source_principal_did: person(&account),
                channel_id: channel.as_str().to_string(),
                routes_b64u: routes_b64u(&routes),
                iat,
                exp: iat + 300,
            };
            let signature = sign_history(&ctx.signing_key, &payload);
            if let Err(e) = handle.send(TunnelMessage::TunnelDmRoute {
                request_id: payload.request_id,
                source_runtime_id: payload.source_runtime_id,
                recipient_runtime_id: target,
                source_principal_did: payload.source_principal_did,
                channel_id: payload.channel_id,
                routes: routes.clone(),
                signature,
            }) {
                debug!(%channel, "DM route not announced: {e}");
            }
        }
    }

    /// Another runtime is a route in one of this person's DMs with a
    /// person: record the routes this one lacks, and announce itself to
    /// each runtime it just learned of (and to the sender, if it did not
    /// list this one).
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn on_dm_route(
        self: &Arc<Self>,
        request_id: String,
        source_runtime_id: String,
        recipient_runtime_id: String,
        source_principal_did: String,
        channel_id: String,
        routes: Vec<DmRoute>,
        signature: String,
    ) {
        let (Some(account), Some(ctx)) = (self.my_account().await, self.ctx.read().await.clone())
        else {
            return;
        };
        if recipient_runtime_id != ctx.caller_runtime_id {
            return;
        }
        let verified = did_key_to_verifying_key(&source_runtime_id)
            .map_err(|e| anyhow::anyhow!("{e}"))
            .and_then(|key| verify_dm_route(&key, &signature));
        let agrees = verified.as_ref().is_ok_and(|p| {
            p.request_id == request_id
                && p.source_runtime_id == source_runtime_id
                && p.recipient_runtime_id == recipient_runtime_id
                && p.source_principal_did == source_principal_did
                && p.channel_id == channel_id
                && p.routes_b64u == routes_b64u(&routes)
        });
        if !agrees {
            warn!(request_id, "DM route failed verification; dropped");
            return;
        }
        let Some(channel) = ChannelId::parse(&channel_id) else {
            return;
        };
        // Only a DM this person has with a person (an open request is
        // not one), and only the two of them.
        let Some(peer) = self.person_dm_peer(&account, &channel).await else {
            return;
        };
        let me = person(&account);
        if (source_principal_did != peer && source_principal_did != me)
            || self.local.is_blocked(&account, &peer).await
        {
            return;
        }
        // The sender speaks for itself; what it lists of others is a
        // lead, recorded only for the DM's two people.
        let mut learned = Vec::new();
        let mut rows = vec![DmRoute {
            runtime_id: source_runtime_id.clone(),
            person: source_principal_did.clone(),
        }];
        rows.extend(routes.iter().cloned());
        let known: HashSet<(String, String)> = self
            .local
            .list_remote_members(&channel)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|m| (m.runtime_id, m.principal_id))
            .collect();
        for row in rows {
            if row.runtime_id == ctx.caller_runtime_id
                || (row.person != peer && row.person != me)
                || known.contains(&(row.runtime_id.clone(), row.person.clone()))
            {
                continue;
            }
            if self
                .local
                .add_remote_member(&channel, &row.runtime_id, &row.person)
                .await
                .is_ok()
            {
                learned.push(row.runtime_id);
            }
        }
        let listed_me = routes.iter().any(|r| r.runtime_id == ctx.caller_runtime_id);
        let mut tell: HashSet<String> = learned.into_iter().collect();
        if !listed_me {
            tell.insert(source_runtime_id);
        }
        for runtime in tell {
            self.announce_route(&channel, Some(&runtime)).await;
        }
    }

    /// The person's DMs with other people as the desktop lists them:
    /// who, what they are called, and whether they have accepted yet.
    pub async fn person_dm_summaries(
        &self,
        account: &str,
    ) -> Vec<crate::ipc::packet::PersonDmSummary> {
        let mut out = Vec::new();
        for (channel, peer) in self.local.person_dms(account).await.unwrap_or_default() {
            let name = self
                .local
                .actor_info(&peer.parse::<ActorRef>().unwrap_or(ActorRef::Public))
                .await
                .name;
            let waiting = self.peer_routes(&channel, &peer).await.is_empty();
            out.push(crate::ipc::packet::PersonDmSummary {
                channel,
                peer,
                name,
                waiting,
            });
        }
        out
    }

    /// The person's DMs with other people and their routes, for the
    /// account DM list (D5).
    pub(crate) async fn person_dm_list(
        &self,
        account: &str,
    ) -> Vec<crate::tunnel::AccountPersonDm> {
        let mut out = Vec::new();
        for (channel, peer) in self.local.person_dms(account).await.unwrap_or_default() {
            let peer_name = self
                .local
                .actor_info(&peer.parse::<ActorRef>().unwrap_or(ActorRef::Public))
                .await
                .name;
            out.push(crate::tunnel::AccountPersonDm {
                channel_id: channel.as_str().to_string(),
                peer,
                peer_name,
                routes: self.known_routes(&channel, account).await,
            });
        }
        out
    }

    /// Open a DM with a person that another of this person's runtimes
    /// holds (D5): record its routes, announce this runtime to them, and
    /// catch up.
    pub(crate) async fn adopt_person_dm(
        self: &Arc<Self>,
        account: &str,
        dm: crate::tunnel::AccountPersonDm,
    ) -> anyhow::Result<()> {
        let channel =
            ChannelId::parse(&dm.channel_id).ok_or_else(|| anyhow::anyhow!("not a channel id"))?;
        anyhow::ensure!(
            channel == person_dm_channel_id(&person(account), &dm.peer)
                && person_account(&dm.peer).is_some_and(|a| a != account),
            "not this person's DM with that person"
        );
        let _ = self.local.take_dm_request(&channel).await;
        if self.local.is_blocked(account, &dm.peer).await {
            return Ok(());
        }
        let subject = self.know_person(&dm.peer, dm.peer_name.as_deref()).await?;
        self.local
            .ensure_human_dm(
                &ActorRef::User("local".into()),
                &subject,
                &channel,
                &format!(
                    "dm-{}",
                    dm.peer_name
                        .clone()
                        .unwrap_or_else(|| person_account(&dm.peer).unwrap_or_default())
                ),
                account,
            )
            .await?;
        let me = person(account);
        let here = self
            .ctx
            .read()
            .await
            .as_ref()
            .map(|c| c.caller_runtime_id.clone())
            .unwrap_or_default();
        let rows: Vec<RemoteMember> = dm
            .routes
            .iter()
            .filter(|r| r.runtime_id != here && (r.person == dm.peer || r.person == me))
            .map(|r| RemoteMember {
                runtime_id: r.runtime_id.clone(),
                principal_id: r.person.clone(),
            })
            .collect();
        for row in &rows {
            self.local
                .add_remote_member(&channel, &row.runtime_id, &row.principal_id)
                .await?;
        }
        self.announce_route(&channel, None).await;
        let port = Arc::clone(self);
        tokio::spawn(async move {
            if let Err(e) = port.sync_history(&channel).await {
                debug!(%channel, "DM from another device not caught up yet: {e}");
            }
        });
        Ok(())
    }
}
