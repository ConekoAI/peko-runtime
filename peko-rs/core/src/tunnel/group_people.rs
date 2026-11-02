//! People as members of groups (ADR-092 D2). A child module of
//! `tunnel_channel_port` so it shares the port's store, outbox and
//! tunnel context.
//!
//! - The home invites a person by Hub account
//!   ([`TunnelChannelPort::invite_person`]): the roster records them and
//!   PekoHub delivers a `tunnel_group_invitation` to each of their
//!   runtimes.
//! - Each of those runtimes keeps it as a pending invitation until the
//!   person answers ([`TunnelChannelPort::answer_group_invitation`]),
//!   which joins the group here and tells the home (`tunnel_group_join`).
//! - The home records the answering runtime as the person's route and
//!   sends the roster to every route ([`TunnelChannelPort::sync_group_roster`]);
//!   a roster addressed to the person keeps their copy current
//!   ([`TunnelChannelPort::on_person_roster`]).

use super::{OutboxItem, TunnelChannelPort};
use crate::tunnel::cross_runtime_channel::CrossRuntimeChannelCtx;
use crate::tunnel::{
    did_key_to_verifying_key, sign_history, verify_group_invitation, verify_group_join,
    GroupInvitationPayload, GroupJoinPayload, TunnelMessage, GROUP_INVITATION_VERSION,
    GROUP_JOIN_VERSION,
};
use peko_channel::{ChannelId, ChannelPort, ConversationKind, GroupInvitation, Registration};
use peko_protocol::channel::InitialMember;
use peko_subject::{ActorRef, PrincipalId};
use std::sync::Arc;
use tracing::{debug, warn};

/// `user:hub:<account>`.
fn person_address(account: &str) -> String {
    format!("user:hub:{account}")
}

/// The Hub account of a person on the tunnel, if `address` is one.
fn hub_person(address: &str) -> Option<(ActorRef, String)> {
    match address.parse::<ActorRef>() {
        Ok(person @ ActorRef::User(_))
            if person.is_hub_route() && person.canonical_address() == address =>
        {
            let account = person.route()?.id().to_string();
            Some((person, account))
        }
        _ => None,
    }
}

impl TunnelChannelPort {
    /// The account this runtime's local user is registered under.
    async fn local_account(&self) -> Option<String> {
        self.local
            .actor_info(&ActorRef::User("local".into()))
            .await
            .hub_id
    }

    /// Whether `actor` is a person reached through other runtimes (not
    /// this runtime's local user).
    pub(super) async fn is_remote_person(&self, actor: &ActorRef) -> bool {
        matches!(actor, ActorRef::User(_))
            && actor.is_hub_route()
            && self.local.actor_info(actor).await.local_id.as_deref() != Some("local")
    }

    // ── The home ────────────────────────────────────────────────────────

    /// Invite a person to a group at its home (ADR-092 D2): the roster
    /// records them, and their runtimes get an invitation to answer.
    /// The caller already checked the roster change.
    pub(super) async fn invite_person(
        &self,
        ctx: &Arc<CrossRuntimeChannelCtx>,
        channel: &ChannelId,
        inviter: &PrincipalId,
        person: &ActorRef,
    ) -> peko_channel::Result<()> {
        let account = person
            .route()
            .filter(|r| r.is_hub())
            .map(|r| r.id().to_string())
            .ok_or_else(|| {
                peko_channel::ChannelError::Adapter("A person is invited by Hub account".into())
            })?;
        self.local
            .register_actor(
                person,
                Some(&account),
                None,
                None,
                None,
                Registration::Registered,
            )
            .await?;
        self.local.invite(channel, inviter, person).await?;
        self.send_queued(
            ctx,
            &person_address(&account),
            OutboxItem::GroupInvitation {
                channel: channel.clone(),
                account,
            },
        )
        .await
        .map_err(peko_channel::ChannelError::Adapter)
    }

    /// Build and sign the invitation for `account` from the group's
    /// current roster.
    pub(super) async fn group_invitation_envelope(
        &self,
        ctx: &Arc<CrossRuntimeChannelCtx>,
        request_id: &str,
        channel: &ChannelId,
        account: &str,
    ) -> Result<TunnelMessage, String> {
        let person = person_address(account);
        let name = self
            .local
            .channel_name(channel)
            .await
            .map_err(|e| format!("read group name ({channel}): {e}"))?;
        let owner = self
            .local
            .group_owner(channel)
            .await
            .map_err(|e| format!("read group owner ({channel}): {e}"))?;
        let inviter = self
            .local
            .resolve_actor(&owner)
            .await
            .ok()
            .and_then(|a| a.hub_address())
            .unwrap_or(owner);
        let members = self.roster_rows(ctx, channel, &person, &person).await?;
        let members_bytes =
            serde_json::to_vec(&members).map_err(|e| format!("serialize roster: {e}"))?;
        let iat = chrono::Utc::now().timestamp();
        let payload = GroupInvitationPayload {
            v: GROUP_INVITATION_VERSION.into(),
            request_id: request_id.into(),
            source_runtime_id: ctx.caller_runtime_id.clone(),
            recipient_account: person.clone(),
            channel_id: channel.as_str().into(),
            name: name.clone(),
            inviter: inviter.clone(),
            initial_members_b64u: GroupInvitationPayload::members_b64u_of(&members_bytes),
            iat,
            exp: iat + 300,
        };
        let signature = sign_history(&ctx.signing_key, &payload);
        Ok(TunnelMessage::TunnelGroupInvitation {
            request_id: request_id.into(),
            source_runtime_id: payload.source_runtime_id,
            recipient_account: person,
            channel_id: payload.channel_id,
            name,
            inviter,
            initial_members: members,
            signature,
        })
    }

    /// A person answered, from `source_runtime_id` (ADR-092 D2). PekoHub
    /// relayed it only from a runtime registered to their account. On
    /// accept, that runtime becomes their route and every member gets
    /// the roster; on decline, they leave the roster.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn on_group_join(
        &self,
        request_id: String,
        source_runtime_id: String,
        recipient_runtime_id: String,
        source_principal_did: String,
        channel_id: String,
        accept: bool,
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
            .and_then(|key| verify_group_join(&key, &signature));
        let agrees = verified.as_ref().is_ok_and(|p| {
            p.request_id == request_id
                && p.source_runtime_id == source_runtime_id
                && p.recipient_runtime_id == recipient_runtime_id
                && p.source_principal_did == source_principal_did
                && p.channel_id == channel_id
                && p.accept == accept
        });
        if !agrees {
            warn!(request_id, "group join failed verification; dropped");
            return;
        }
        let Some((person, account)) = hub_person(&source_principal_did) else {
            return;
        };
        let channel = ChannelId(channel_id);
        if let Err(e) = self
            .apply_group_join(
                &ctx,
                &channel,
                &person,
                &account,
                &source_runtime_id,
                accept,
            )
            .await
        {
            debug!(%channel, "group join not applied: {e}");
            return;
        }
        self.send_receipt(&source_runtime_id, &channel, vec![request_id])
            .await;
    }

    async fn apply_group_join(
        &self,
        ctx: &Arc<CrossRuntimeChannelCtx>,
        channel: &ChannelId,
        person: &ActorRef,
        account: &str,
        route: &str,
        accept: bool,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.local.conversation_kind(channel).await? == ConversationKind::Group,
            "not a group"
        );
        let home = self.local.group_home(channel).await?;
        anyhow::ensure!(
            home.is_none_or(|h| h.runtime == ctx.caller_runtime_id),
            "this runtime is not the group's home"
        );
        let address = self.local.actor_info(person).await.address;
        let mut invited = false;
        for member in self.local.list_members(channel).await? {
            if self.local.actor_info(&member).await.address == address {
                invited = true;
            }
        }
        anyhow::ensure!(invited, "{address} was not invited");
        self.outbox
            .forget_for(&person_address(account), channel)
            .await;
        if accept {
            if !self
                .local
                .is_remote_member(channel, route, &person_address(account))
                .await?
            {
                self.local
                    .add_remote_member(channel, route, &person_address(account))
                    .await?;
            }
            self.sync_group_roster(ctx, channel).await
        } else {
            self.local.leave_actor(channel, person).await?;
            self.report_group_roster(ctx, channel).await;
            Ok(())
        }
    }

    /// Send the group's roster to every route: each remote principal's
    /// runtime, and each runtime a person joined from (ADR-092 D2). Signed
    /// by the group's owner when it is a peko hosted here.
    pub(super) async fn sync_group_roster(
        &self,
        ctx: &Arc<CrossRuntimeChannelCtx>,
        channel: &ChannelId,
    ) -> anyhow::Result<()> {
        let owner = self.local.group_owner(channel).await?;
        let owner = self.local.resolve_actor(&owner).await?;
        let Some(local_id) = owner.local_id.filter(|_| owner.kind == "principal") else {
            anyhow::bail!("the group's owner is not a peko hosted here");
        };
        let inviter = PrincipalId(local_id);
        let creator_did = ctx
            .principal_keys
            .did_for_principal(&inviter)
            .await
            .ok_or_else(|| anyhow::anyhow!("the owner's signing key is unavailable"))?;
        for member in self.local.list_remote_members(channel).await? {
            self.send_queued(
                ctx,
                &member.runtime_id,
                OutboxItem::Invite {
                    channel: channel.clone(),
                    inviter: inviter.clone(),
                    creator_did: creator_did.clone(),
                    invitee: PrincipalId(member.principal_id),
                },
            )
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        }
        // ADR-092 D6: PekoHub hears who is in a registered group.
        self.report_group_roster(ctx, channel).await;
        Ok(())
    }

    // ── The person's runtime ────────────────────────────────────────────

    /// An invitation arrived for this runtime's person (ADR-092 D2): keep
    /// it for their answer. A group they are already in takes the roster.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn on_group_invitation(
        &self,
        request_id: String,
        source_runtime_id: String,
        recipient_account: String,
        channel_id: String,
        name: String,
        inviter: String,
        initial_members: Vec<InitialMember>,
        signature: String,
    ) {
        let Some(account) = self.local_account().await else {
            return;
        };
        if recipient_account != person_address(&account) {
            return;
        }
        let members_bytes = serde_json::to_vec(&initial_members).unwrap_or_default();
        let verified = did_key_to_verifying_key(&source_runtime_id)
            .map_err(|e| anyhow::anyhow!("{e}"))
            .and_then(|key| verify_group_invitation(&key, &signature));
        let agrees = verified.as_ref().is_ok_and(|p| {
            p.request_id == request_id
                && p.source_runtime_id == source_runtime_id
                && p.recipient_account == recipient_account
                && p.channel_id == channel_id
                && p.name == name
                && p.inviter == inviter
                && p.initial_members_b64u == GroupInvitationPayload::members_b64u_of(&members_bytes)
        });
        if !agrees {
            warn!(request_id, "group invitation failed verification; dropped");
            return;
        }
        let Some(channel) = ChannelId::parse(&channel_id) else {
            return;
        };
        if self
            .local
            .remote_dm_account(&channel)
            .await
            .ok()
            .flatten()
            .as_deref()
            == Some(account.as_str())
        {
            // Already in it: the home resends until an answer reaches it.
            return;
        }
        let invitation = GroupInvitation {
            channel,
            name,
            inviter,
            home_runtime: source_runtime_id,
            account,
            initial_members,
            received_at: chrono::Utc::now(),
            requested: false,
        };
        if let Err(e) = self.local.save_invitation(&invitation).await {
            warn!("could not keep a group invitation: {e}");
        }
    }

    /// The groups this runtime's person joined from other runtimes, with
    /// their names and homes.
    ///
    /// # Errors
    ///
    /// The runtime is not registered with PekoHub.
    pub async fn person_groups(&self) -> anyhow::Result<Vec<(ChannelId, String, Option<String>)>> {
        let account = self
            .local_account()
            .await
            .ok_or_else(|| anyhow::anyhow!("Register this runtime with PekoHub first"))?;
        Ok(self.local.person_groups(&account).await?)
    }

    /// The invitations waiting for this runtime's person.
    ///
    /// # Errors
    ///
    /// The runtime is not registered with PekoHub.
    pub async fn group_invitations(&self) -> anyhow::Result<Vec<GroupInvitation>> {
        let account = self
            .local_account()
            .await
            .ok_or_else(|| anyhow::anyhow!("Register this runtime with PekoHub first"))?;
        Ok(self
            .local
            .invitations(&account)
            .await?
            .into_iter()
            .filter(|i| !i.requested)
            .collect())
    }

    /// The person answers an invitation (ADR-092 D2): accepting joins the
    /// group here and tells its home; declining tells the home and keeps
    /// nothing.
    ///
    /// # Errors
    ///
    /// No invitation to `channel` waits, or the runtime is not registered.
    pub async fn answer_group_invitation(
        &self,
        channel: &ChannelId,
        accept: bool,
    ) -> anyhow::Result<()> {
        let account = self
            .local_account()
            .await
            .ok_or_else(|| anyhow::anyhow!("Register this runtime with PekoHub first"))?;
        let ctx = self
            .ctx
            .read()
            .await
            .clone()
            .ok_or_else(|| anyhow::anyhow!("This runtime has no PekoHub connection"))?;
        let invitation = self
            .local
            .take_invitation(channel)
            .await?
            .filter(|i| i.account == account && !i.requested)
            .ok_or_else(|| anyhow::anyhow!("No invitation to {channel} is waiting"))?;
        if accept {
            self.join_as_person(&invitation.channel, &invitation, &account)
                .await?;
        }
        self.send_queued(
            &ctx,
            &invitation.home_runtime,
            OutboxItem::GroupJoin {
                channel: channel.clone(),
                accept,
            },
        )
        .await
        .map_err(|e| anyhow::anyhow!(e))
    }

    /// Join (or refresh) a group as this runtime's person from a roster the
    /// home signed: record each member, then the copy.
    async fn join_as_person(
        &self,
        channel: &ChannelId,
        invitation: &GroupInvitation,
        account: &str,
    ) -> anyhow::Result<()> {
        let me = person_address(account);
        for member in &invitation.initial_members {
            if member.principal_did == me {
                continue;
            }
            self.record_member(member).await?;
        }
        let creator = self
            .local
            .resolve_actor(&invitation.inviter)
            .await
            .map(|a| a.address)
            .unwrap_or_else(|_| invitation.inviter.clone());
        let members: Vec<InitialMember> = invitation
            .initial_members
            .iter()
            .filter(|m| m.principal_did != me)
            .cloned()
            .collect();
        self.local
            .join_remote_person(
                channel,
                &invitation.name,
                &creator,
                &members,
                account,
                &invitation.home_runtime,
            )
            .await?;
        Ok(())
    }

    /// A member of a roster the home signed, in this runtime's registry.
    async fn record_member(&self, member: &InitialMember) -> anyhow::Result<()> {
        if let Some((person, account)) = hub_person(&member.principal_did) {
            self.local
                .register_actor(
                    &person,
                    Some(&account),
                    None,
                    None,
                    member.name.as_deref(),
                    Registration::Registered,
                )
                .await?;
            return Ok(());
        }
        let Some(hub_id) = member.hub_id.as_deref() else {
            return Ok(());
        };
        self.local
            .register_actor(
                &ActorRef::Principal(peko_subject::Route::hub(hub_id.to_string())),
                Some(hub_id),
                member
                    .principal_did
                    .starts_with("did:key:")
                    .then_some(member.principal_did.as_str()),
                member.identity_key.as_deref(),
                member.name.as_deref(),
                Registration::Private,
            )
            .await?;
        Ok(())
    }

    /// A roster from the group's home addressed to this runtime's person
    /// (ADR-092 D2). A group they are in takes it. A group whose
    /// invitation waits here takes it once the roster shows the person
    /// joined from another of their runtimes. Anything else is ignored:
    /// no one adds a person to a group they did not accept.
    ///
    /// Returns whether it was applied.
    pub(crate) async fn on_person_roster(
        &self,
        channel: &ChannelId,
        name: &str,
        creator: &str,
        members: &[InitialMember],
        source_runtime_id: &str,
    ) -> anyhow::Result<bool> {
        let Some(account) = self.local_account().await else {
            return Ok(false);
        };
        let me = person_address(&account);
        let joined_here = self
            .local
            .remote_dm_account(channel)
            .await
            .ok()
            .flatten()
            .as_deref()
            == Some(account.as_str());
        if !joined_here {
            // An invitation waiting here takes the roster once it shows them
            // joined from another of their runtimes; a request they made
            // here (by link, ID, or as a new device, ADR-092 D4/D5) takes
            // it as the home's yes.
            let Some(waiting) = self
                .local
                .invitations(&account)
                .await?
                .into_iter()
                .find(|i| &i.channel == channel && i.home_runtime == source_runtime_id)
            else {
                return Ok(false);
            };
            let joined_elsewhere = members
                .iter()
                .any(|m| m.principal_did == me && m.runtime_id.is_some());
            if !(waiting.requested || joined_elsewhere) {
                return Ok(false);
            }
            let _ = self.local.take_invitation(channel).await;
        }
        let invitation = GroupInvitation {
            channel: channel.clone(),
            name: name.into(),
            inviter: creator.into(),
            home_runtime: source_runtime_id.into(),
            account: account.clone(),
            initial_members: members.to_vec(),
            received_at: chrono::Utc::now(),
            requested: false,
        };
        self.join_as_person(channel, &invitation, &account).await?;
        Ok(true)
    }

    /// Build and sign the person's answer to `channel`'s home.
    pub(super) async fn group_join_envelope(
        &self,
        ctx: &Arc<CrossRuntimeChannelCtx>,
        request_id: &str,
        channel: &ChannelId,
        home_runtime: &str,
        accept: bool,
    ) -> Result<TunnelMessage, String> {
        let account = self
            .local_account()
            .await
            .ok_or_else(|| "Human sender has no Hub identity".to_string())?;
        let iat = chrono::Utc::now().timestamp();
        let payload = GroupJoinPayload {
            v: GROUP_JOIN_VERSION.into(),
            request_id: request_id.into(),
            source_runtime_id: ctx.caller_runtime_id.clone(),
            recipient_runtime_id: home_runtime.into(),
            source_principal_did: person_address(&account),
            channel_id: channel.as_str().into(),
            accept,
            iat,
            exp: iat + 300,
        };
        let signature = sign_history(&ctx.signing_key, &payload);
        Ok(TunnelMessage::TunnelGroupJoin {
            request_id: request_id.into(),
            source_runtime_id: payload.source_runtime_id,
            recipient_runtime_id: payload.recipient_runtime_id,
            source_principal_did: payload.source_principal_did,
            channel_id: payload.channel_id,
            accept,
            signature,
        })
    }
}
