//! Groups on PekoHub (ADR-092 D4–D6). A child module of
//! `tunnel_channel_port` so it shares the port's store, outbox and
//! tunnel context.
//!
//! - The home registers a group ([`TunnelChannelPort::register_group`]):
//!   PekoHub gives it a bare-number ID and a page, and from then on hears
//!   which accounts are in it ([`TunnelChannelPort::report_group_roster`]).
//! - The home mints invite links ([`TunnelChannelPort::group_invite_link`]):
//!   a token signed by the home runtime, whose nonce the home keeps so it
//!   can revoke it.
//! - A person joins by link or, for a public group, by ID
//!   ([`TunnelChannelPort::join_group`]): PekoHub says where the group
//!   lives, the person's runtime asks the home
//!   (`tunnel_group_join_request`), and the home decides
//!   ([`TunnelChannelPort::on_group_join_request`]).
//! - A person's new device finds their groups
//!   ([`TunnelChannelPort::sync_account_groups`]) and asks each home to
//!   add it as a route.

use super::{OutboxItem, TunnelChannelPort};
use crate::tunnel::cross_runtime_channel::CrossRuntimeChannelCtx;
use crate::tunnel::{
    did_key_to_verifying_key, sign_history, verify_group_join_request, AccountGroup,
    GroupInviteToken, GroupJoinRequestPayload, TunnelMessage, GROUP_INVITE_TOKEN_VERSION,
    GROUP_JOIN_REQUEST_VERSION,
};
use peko_channel::{
    ChannelId, ChannelPort, ConversationKind, GroupInvitation, GroupVisibility, HubGroup,
    InviteLink, Registration,
};
use peko_subject::{ActorRef, PrincipalId};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{debug, warn};

/// Requests to PekoHub waiting for its answer, by request id.
pub(super) type HubPending =
    Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<TunnelMessage>>>>;

/// How long a request to PekoHub waits for its answer.
const HUB_WAIT: Duration = Duration::from_secs(15);
/// How long an invite link works unless asked otherwise, and at most.
pub const INVITE_LINK_DAYS: u32 = 7;
pub const INVITE_LINK_MAX_DAYS: u32 = 30;

/// `user:hub:<account>`.
fn person_address(account: &str) -> String {
    format!("user:hub:{account}")
}

/// A group's ID and invite token from what a person pasted: a link
/// (`…/g/<id>?invite=<token>`), a bare token, or a bare ID.
fn parse_group_ref(input: &str) -> Option<(String, Option<String>)> {
    let input = input.trim();
    if !input.is_empty() && input.len() <= 20 && input.bytes().all(|b| b.is_ascii_digit()) {
        return Some((input.to_string(), None));
    }
    let invite = input
        .split_once("invite=")
        .map(|(_, rest)| rest.split(['&', '#']).next().unwrap_or_default())
        .unwrap_or(input);
    let token = GroupInviteToken::read_unverified(invite).ok()?;
    Some((token.g, Some(invite.to_string())))
}

impl TunnelChannelPort {
    /// The account this runtime's local user is registered under.
    async fn hub_account(&self) -> Option<String> {
        self.local
            .actor_info(&ActorRef::User("local".into()))
            .await
            .hub_id
    }

    async fn hub_ctx(&self) -> anyhow::Result<Arc<CrossRuntimeChannelCtx>> {
        self.ctx
            .read()
            .await
            .clone()
            .ok_or_else(|| anyhow::anyhow!("This runtime has no PekoHub connection"))
    }

    /// Send `message` to PekoHub and wait for the answer to `request_id`.
    async fn ask_hub(
        &self,
        request_id: &str,
        message: TunnelMessage,
    ) -> anyhow::Result<TunnelMessage> {
        let ctx = self.hub_ctx().await?;
        let handle = ctx
            .tunnel
            .read()
            .await
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Not connected to PekoHub"))?;
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.hub_pending
            .lock()
            .expect("hub pending")
            .insert(request_id.to_string(), reply);
        if let Err(e) = handle.send(message) {
            self.hub_pending
                .lock()
                .expect("hub pending")
                .remove(request_id);
            anyhow::bail!("Not connected to PekoHub: {e}");
        }
        match tokio::time::timeout(HUB_WAIT, answer).await {
            Ok(Ok(message)) => Ok(message),
            _ => {
                self.hub_pending
                    .lock()
                    .expect("hub pending")
                    .remove(request_id);
                anyhow::bail!("PekoHub did not answer; try again")
            }
        }
    }

    /// PekoHub answered request `request_id`. `false` when nothing waits
    /// for it.
    pub(crate) fn on_hub_answer(&self, request_id: &str, message: TunnelMessage) -> bool {
        let waiting = self
            .hub_pending
            .lock()
            .expect("hub pending")
            .remove(request_id);
        waiting.is_some_and(|reply| reply.send(message).is_ok())
    }

    /// Whether this runtime is `channel`'s home (or the group has none
    /// recorded, which only a group created here lacks).
    async fn is_home_of(
        &self,
        ctx: &CrossRuntimeChannelCtx,
        channel: &ChannelId,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.local.conversation_kind(channel).await? == ConversationKind::Group,
            "{channel} is not a group"
        );
        if let Some(home) = self.local.group_home(channel).await? {
            anyhow::ensure!(
                home.runtime == ctx.caller_runtime_id,
                "A group is registered by its home runtime ({})",
                home.runtime
            );
        }
        Ok(())
    }

    // ── The home ────────────────────────────────────────────────────────

    /// Register `channel` with PekoHub, or change its visibility (ADR-092
    /// D5). The caller already checked that the asker may manage it.
    pub(super) async fn register_group_at_home(
        &self,
        channel: &ChannelId,
        visibility: GroupVisibility,
    ) -> anyhow::Result<HubGroup> {
        let ctx = self.hub_ctx().await?;
        self.is_home_of(&ctx, channel).await?;
        let owner = self.local.group_owner(channel).await?;
        let owner_address = self
            .local
            .resolve_actor(&owner)
            .await
            .ok()
            .and_then(|a| a.hub_address())
            .ok_or_else(|| {
                anyhow::anyhow!("The group's owner ({owner}) must be registered with PekoHub first")
            })?;
        let name = self.local.channel_name(channel).await?;
        let request_id = uuid::Uuid::new_v4().to_string();
        let answer = self
            .ask_hub(
                &request_id,
                TunnelMessage::GroupRegister {
                    request_id: request_id.clone(),
                    channel_id: channel.as_str().into(),
                    name,
                    owner_address,
                    visibility: visibility.as_str().into(),
                },
            )
            .await?;
        let (id, page) = match answer {
            TunnelMessage::GroupRegistered {
                group_id, page_url, ..
            } => (group_id, page_url),
            TunnelMessage::GroupRegisterRejected { message, .. } => {
                anyhow::bail!("PekoHub refused: {message}")
            }
            other => anyhow::bail!("unexpected answer from PekoHub: {other:?}"),
        };
        let hub = self
            .local
            .update_hub_group(channel, |old| HubGroup {
                id,
                page,
                visibility,
                links: old.map(|o| o.links).unwrap_or_default(),
            })
            .await?;
        self.report_group_roster(&ctx, channel).await;
        Ok(hub)
    }

    /// Mint an invite link to a registered group, good for `days` (ADR-092
    /// D5). The caller already checked that the asker may manage it.
    pub(super) async fn invite_link_at_home(
        &self,
        channel: &ChannelId,
        days: u32,
    ) -> anyhow::Result<(String, i64)> {
        let ctx = self.hub_ctx().await?;
        self.is_home_of(&ctx, channel).await?;
        let hub = self.local.hub_group(channel).await?.ok_or_else(|| {
            anyhow::anyhow!("Register {channel} with PekoHub first (Channel register)")
        })?;
        let days = days.clamp(1, INVITE_LINK_MAX_DAYS);
        let now = chrono::Utc::now().timestamp();
        let exp = now + i64::from(days) * 86_400;
        let token = GroupInviteToken {
            v: GROUP_INVITE_TOKEN_VERSION.into(),
            g: hub.id.clone(),
            c: channel.as_str().into(),
            h: ctx.caller_runtime_id.clone(),
            exp,
            n: uuid::Uuid::new_v4().to_string(),
        };
        let compact = sign_history(&ctx.signing_key, &token);
        let page = if hub.page.is_empty() {
            format!("https://pekohub.ai/g/{}", hub.id)
        } else {
            hub.page.clone()
        };
        self.local
            .update_hub_group(channel, |old| {
                let mut hub = old.unwrap_or(hub);
                hub.links.retain(|l| l.exp > now);
                hub.links.push(InviteLink {
                    nonce: token.n.clone(),
                    exp,
                    revoked: false,
                });
                hub
            })
            .await?;
        Ok((format!("{page}?invite={compact}"), exp))
    }

    /// Stop every invite link to `channel` from working. Returns how many
    /// were live.
    pub(super) async fn revoke_links_at_home(&self, channel: &ChannelId) -> anyhow::Result<usize> {
        let ctx = self.hub_ctx().await?;
        self.is_home_of(&ctx, channel).await?;
        let now = chrono::Utc::now().timestamp();
        let mut live = 0;
        if self.local.hub_group(channel).await?.is_some() {
            self.local
                .update_hub_group(channel, |old| {
                    let mut hub = old.expect("registered");
                    hub.links.retain(|l| l.exp > now);
                    for link in &mut hub.links {
                        live += usize::from(!link.revoked);
                        link.revoked = true;
                    }
                    hub
                })
                .await?;
        }
        Ok(live)
    }

    /// Tell PekoHub which accounts are in a registered group (ADR-092
    /// D4/D6): persons who joined (from a route), and this runtime's own
    /// person when a member. Invited but not yet joined is not in.
    pub(super) async fn report_group_roster(
        &self,
        ctx: &CrossRuntimeChannelCtx,
        channel: &ChannelId,
    ) {
        let Ok(Some(hub)) = self.local.hub_group(channel).await else {
            return;
        };
        let mut accounts = Vec::new();
        if let Ok(remote) = self.local.list_remote_members(channel).await {
            for member in remote {
                if let Some(account) = member.principal_id.strip_prefix("user:hub:") {
                    if !accounts.iter().any(|a| a == account) {
                        accounts.push(account.to_string());
                    }
                }
            }
        }
        let me = ActorRef::User("local".into());
        if let (Some(account), Ok(members)) = (
            self.hub_account().await,
            self.local.list_members(channel).await,
        ) {
            if members.contains(&me) && !accounts.contains(&account) {
                accounts.push(account);
            }
        }
        let Some(handle) = ctx.tunnel.read().await.clone() else {
            return;
        };
        if let Err(e) = handle.send(TunnelMessage::GroupRoster {
            group_id: hub.id,
            accounts,
        }) {
            debug!(%channel, "group roster not reported: {e}");
        }
    }

    /// A person asks to join, from `source_runtime_id` (ADR-092 D5).
    /// PekoHub relayed it only from a runtime registered to their account.
    /// The home admits them with a live invite link it minted, or to a
    /// public group without one; that runtime becomes their route.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn on_group_join_request(
        &self,
        request_id: String,
        source_runtime_id: String,
        recipient_runtime_id: String,
        source_principal_did: String,
        channel_id: String,
        invite: Option<String>,
        signature: String,
    ) {
        let Some(ctx) = self.ctx.read().await.clone() else {
            return;
        };
        if recipient_runtime_id != ctx.caller_runtime_id {
            return;
        }
        let verified = did_key_to_verifying_key(&source_runtime_id)
            .map_err(|e| anyhow::anyhow!("{e}"))
            .and_then(|key| verify_group_join_request(&key, &signature));
        let agrees = verified.as_ref().is_ok_and(|p| {
            p.request_id == request_id
                && p.source_runtime_id == source_runtime_id
                && p.recipient_runtime_id == recipient_runtime_id
                && p.source_principal_did == source_principal_did
                && p.channel_id == channel_id
                && p.invite == invite
        });
        if !agrees {
            warn!(
                request_id,
                "group join request failed verification; dropped"
            );
            return;
        }
        let Some(channel) = ChannelId::parse(&channel_id) else {
            return;
        };
        match self
            .admit(
                &ctx,
                &channel,
                &source_principal_did,
                &source_runtime_id,
                invite.as_deref(),
            )
            .await
        {
            Ok(()) => {}
            Err(e) => debug!(%channel, "group join request refused: {e}"),
        }
        // Confirmed either way: a refusal is final until they ask again.
        self.send_receipt(&source_runtime_id, &channel, vec![request_id])
            .await;
    }

    async fn admit(
        &self,
        ctx: &Arc<CrossRuntimeChannelCtx>,
        channel: &ChannelId,
        person_address_: &str,
        route: &str,
        invite: Option<&str>,
    ) -> anyhow::Result<()> {
        let person = match person_address_.parse::<ActorRef>() {
            Ok(person @ ActorRef::User(_))
                if person.is_hub_route() && person.canonical_address() == person_address_ =>
            {
                person
            }
            _ => anyhow::bail!("only a person on PekoHub asks to join"),
        };
        let account = person
            .route()
            .map(|r| r.id().to_string())
            .ok_or_else(|| anyhow::anyhow!("no account"))?;
        self.is_home_of(ctx, channel).await?;
        let hub = self
            .local
            .hub_group(channel)
            .await?
            .ok_or_else(|| anyhow::anyhow!("not registered"))?;
        let address = self.local.actor_info(&person).await.address;
        let mut member = false;
        for m in self.local.list_members(channel).await? {
            if self.local.actor_info(&m).await.address == address {
                member = true;
            }
        }
        // A member (invited, or joined from another device, D4) needs no
        // link; anyone else needs a live one, or a public group.
        let admitted = member
            || match invite {
                Some(compact) => {
                    let home_key = did_key_to_verifying_key(&ctx.caller_runtime_id)
                        .map_err(|e| anyhow::anyhow!("{e}"))?;
                    let token = GroupInviteToken::verify(&home_key, compact)?;
                    let now = chrono::Utc::now().timestamp();
                    token.g == hub.id
                        && token.c == channel.as_str()
                        && token.h == ctx.caller_runtime_id
                        && token.exp > now
                        && hub
                            .links
                            .iter()
                            .any(|l| l.nonce == token.n && !l.revoked && l.exp > now)
                }
                None => hub.visibility == GroupVisibility::Public,
            };
        anyhow::ensure!(admitted, "no live invite, and the group is not public");

        if !member {
            self.local
                .register_actor(
                    &person,
                    Some(&account),
                    None,
                    None,
                    None,
                    Registration::Registered,
                )
                .await?;
            let owner = self.local.group_owner(channel).await?;
            let owner = self.local.resolve_actor(&owner).await?;
            let inviter = owner
                .local_id
                .filter(|_| owner.kind == "principal")
                .map(PrincipalId)
                .ok_or_else(|| anyhow::anyhow!("the group's owner is not a peko hosted here"))?;
            self.local.invite(channel, &inviter, &person).await?;
        }
        if !self
            .local
            .is_remote_member(channel, route, &person_address(&account))
            .await?
        {
            self.local
                .add_remote_member(channel, route, &person_address(&account))
                .await?;
        }
        self.sync_group_roster(ctx, channel).await
    }

    // ── The person's runtime ────────────────────────────────────────────

    /// Ask PekoHub what it shows of group `group_id`.
    async fn look_up_group(
        &self,
        group_id: &str,
        invite: Option<&str>,
    ) -> anyhow::Result<Option<AccountGroup>> {
        let request_id = uuid::Uuid::new_v4().to_string();
        match self
            .ask_hub(
                &request_id,
                TunnelMessage::GroupLookupRequest {
                    request_id: request_id.clone(),
                    group_id: group_id.into(),
                    invite: invite.map(str::to_string),
                },
            )
            .await?
        {
            TunnelMessage::GroupLookup { group, .. } => Ok(group),
            other => anyhow::bail!("unexpected answer from PekoHub: {other:?}"),
        }
    }

    /// Join a group by its invite link, or a public one by its ID
    /// (ADR-092 D5): ask its home, and keep a request that the home's
    /// roster completes. Returns the group's channel and name.
    ///
    /// # Errors
    ///
    /// The runtime is not registered, the input is neither, PekoHub
    /// shows no such group to this person, or its home is this runtime.
    pub async fn join_group(&self, input: &str) -> anyhow::Result<(ChannelId, String)> {
        let account = self
            .hub_account()
            .await
            .ok_or_else(|| anyhow::anyhow!("Register this runtime with PekoHub first"))?;
        let ctx = self.hub_ctx().await?;
        let (group_id, invite) = parse_group_ref(input).ok_or_else(|| {
            anyhow::anyhow!("Paste a group's invite link, or the ID of a public group")
        })?;
        let group = self
            .look_up_group(&group_id, invite.as_deref())
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "No group {group_id} is open to you: an invite-only group needs a live invite link"
                )
            })?;
        let channel = ChannelId::parse(&group.channel_id)
            .ok_or_else(|| anyhow::anyhow!("PekoHub sent an invalid channel"))?;
        anyhow::ensure!(
            group.home_runtime != ctx.caller_runtime_id,
            "{} is kept on this computer; it is already here",
            group.name
        );
        if self
            .local
            .remote_dm_account(&channel)
            .await
            .ok()
            .flatten()
            .as_deref()
            == Some(account.as_str())
        {
            return Ok((channel, group.name));
        }
        self.request_group(&ctx, &channel, &group, &account, invite)
            .await?;
        Ok((channel, group.name))
    }

    /// Keep a request for `group` and send it to its home: with `invite`
    /// as a join request, or (a member's new device, D4) as an accepted
    /// answer.
    async fn request_group(
        &self,
        ctx: &Arc<CrossRuntimeChannelCtx>,
        channel: &ChannelId,
        group: &AccountGroup,
        account: &str,
        invite: Option<String>,
    ) -> anyhow::Result<()> {
        self.local
            .save_invitation(&GroupInvitation {
                channel: channel.clone(),
                name: group.name.clone(),
                inviter: String::new(),
                home_runtime: group.home_runtime.clone(),
                account: account.to_string(),
                initial_members: Vec::new(),
                received_at: chrono::Utc::now(),
                requested: true,
            })
            .await?;
        self.send_queued(
            ctx,
            &group.home_runtime,
            OutboxItem::GroupJoinRequest {
                channel: channel.clone(),
                invite,
            },
        )
        .await
        .map_err(|e| anyhow::anyhow!(e))
    }

    /// A person's groups follow them to a new device (ADR-092 D4): ask
    /// PekoHub for the account's groups and request each one not here.
    /// Called when the tunnel is ready.
    pub async fn sync_account_groups(&self) {
        let (Some(account), Ok(ctx)) = (self.hub_account().await, self.hub_ctx().await) else {
            return;
        };
        let request_id = uuid::Uuid::new_v4().to_string();
        let groups = match self
            .ask_hub(
                &request_id,
                TunnelMessage::AccountGroupsRequest {
                    request_id: request_id.clone(),
                },
            )
            .await
        {
            Ok(TunnelMessage::AccountGroups { groups, .. }) => groups,
            Ok(_) => return,
            Err(e) => {
                debug!("account groups not synced: {e}");
                return;
            }
        };
        let waiting = self.local.invitations(&account).await.unwrap_or_default();
        for group in groups {
            let Some(channel) = ChannelId::parse(&group.channel_id) else {
                continue;
            };
            if group.home_runtime == ctx.caller_runtime_id
                || waiting.iter().any(|i| i.channel == channel)
                || self.local.conversation_kind(&channel).await.is_ok()
            {
                continue;
            }
            if let Err(e) = self
                .request_group(&ctx, &channel, &group, &account, None)
                .await
            {
                warn!(%channel, "could not ask for a group of this account: {e}");
            }
        }
    }

    /// Build and sign the person's request to `channel`'s home.
    pub(super) async fn group_join_request_envelope(
        &self,
        ctx: &Arc<CrossRuntimeChannelCtx>,
        request_id: &str,
        channel: &ChannelId,
        home_runtime: &str,
        invite: Option<String>,
    ) -> Result<TunnelMessage, String> {
        let account = self
            .hub_account()
            .await
            .ok_or_else(|| "Human sender has no Hub identity".to_string())?;
        let iat = chrono::Utc::now().timestamp();
        let payload = GroupJoinRequestPayload {
            v: GROUP_JOIN_REQUEST_VERSION.into(),
            request_id: request_id.into(),
            source_runtime_id: ctx.caller_runtime_id.clone(),
            recipient_runtime_id: home_runtime.into(),
            source_principal_did: person_address(&account),
            channel_id: channel.as_str().into(),
            invite,
            iat,
            exp: iat + 300,
        };
        let signature = sign_history(&ctx.signing_key, &payload);
        Ok(TunnelMessage::TunnelGroupJoinRequest {
            request_id: request_id.into(),
            source_runtime_id: payload.source_runtime_id,
            recipient_runtime_id: payload.recipient_runtime_id,
            source_principal_did: payload.source_principal_did,
            channel_id: payload.channel_id,
            invite: payload.invite,
            signature,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_group_is_named_by_link_token_or_id() {
        let key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let token = sign_history(
            &key,
            &GroupInviteToken {
                v: GROUP_INVITE_TOKEN_VERSION.into(),
                g: "1234567890".into(),
                c: "chan_book".into(),
                h: "did:key:z6Mkhome".into(),
                exp: 1,
                n: "n".into(),
            },
        );
        assert_eq!(
            parse_group_ref(&format!("https://pekohub.ai/g/1234567890?invite={token}")),
            Some(("1234567890".into(), Some(token.clone())))
        );
        assert_eq!(
            parse_group_ref(&token),
            Some(("1234567890".into(), Some(token.clone())))
        );
        assert_eq!(
            parse_group_ref(" 1234567890 "),
            Some(("1234567890".into(), None))
        );
        assert_eq!(parse_group_ref("book club"), None);
        assert_eq!(
            parse_group_ref("https://pekohub.ai/g/123?invite=junk"),
            None
        );
    }
}
