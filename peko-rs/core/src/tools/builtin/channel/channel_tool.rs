//! `Channel` — a principal manages its groups (ADR-082 D4–D6).
//!
//! Five actions: `create` a group, `invite` an actor into one, `leave`
//! it, set its own `wake` policy there, and read a channel's `info`.
//! DMs are opened by `ChannelSend` and never change: every action but
//! `info` refuses one.
//!
//! Authority is split with the dispatcher. The floor refuses `create` /
//! `invite` to untrusted runs, and a trusted run's call waits for the
//! owner (`security::judge::owner_decides`). This tool keeps every
//! non-owner run inside its own conversation's channel and limits
//! invitees to the principal's owner and learned principal contacts.

use std::sync::{Arc, Weak};

use async_trait::async_trait;
use peko_channel::{ChannelId, ChannelPort, ConversationKind, WakePolicy};
use peko_subject::{ActorRef, PrincipalId};
use peko_tools_core::{ActingFor, Tool, ToolContext};
use serde_json::{json, Value};

use crate::extensions::framework::core::ExtensionServices;
use crate::principal::{Principal, PrincipalManager};
use crate::tunnel::CrossRuntimeA2aCtx;

pub const CHANNEL_TOOL_NAME: &str = "Channel";

pub struct ChannelTool {
    port: Arc<dyn ChannelPort>,
    services: Weak<ExtensionServices>,
}

impl ChannelTool {
    pub fn new(port: Arc<dyn ChannelPort>, services: Weak<ExtensionServices>) -> Self {
        Self { port, services }
    }

    fn manager(&self) -> anyhow::Result<Arc<PrincipalManager>> {
        let services = self
            .services
            .upgrade()
            .ok_or_else(|| anyhow::anyhow!("Channel: the runtime is shutting down"))?;
        let ctx: Arc<CrossRuntimeA2aCtx> = services
            .cross_runtime_a2a_ctx()
            .and_then(|ctx| Arc::downcast(ctx).ok())
            .ok_or_else(|| {
                anyhow::anyhow!("Channel: principals are unavailable on this runtime")
            })?;
        Ok(Arc::clone(&ctx.principal_manager))
    }

    async fn principal(&self, id: &PrincipalId) -> anyhow::Result<Arc<Principal>> {
        self.manager()?
            .get(id.clone())
            .await
            .ok_or_else(|| anyhow::anyhow!("Channel: principal {id} is not loaded"))
    }

    fn tool_parameters() -> Value {
        let channel = json!({"type":"string", "minLength":1,
            "description":"Channel id: chan_<id>, group:<slug>, or dm_<id> (info only)."});
        peko_tools_core::schema::action_schema(&[
            (
                "create",
                json!({"type":"object", "properties":{
                    "name":{"type":"string", "minLength":1, "description":"Display name."},
                    "slug":{"type":"string", "minLength":1, "pattern":"^[a-z0-9][a-z0-9-]*$",
                        "description":"Optional id slug: the group becomes group:<slug>."},
                    "members":{"type":"array", "items":{"type":"string", "minLength":1},
                        "description":"Actors to invite (see invite)."}
                }, "required":["name"], "additionalProperties":false}),
            ),
            (
                "invite",
                json!({"type":"object", "properties":{
                    "channel": channel,
                    "actor":{"type":"string", "minLength":1,
                        "description":"kind:local:<id> or kind:hub:<id>: your owner or a principal in Contacts."}
                }, "required":["channel", "actor"], "additionalProperties":false}),
            ),
            (
                "leave",
                json!({"type":"object", "properties":{"channel": channel},
                    "required":["channel"], "additionalProperties":false}),
            ),
            (
                "wake",
                json!({"type":"object", "properties":{
                    "channel": channel,
                    "policy":{"type":"string", "enum":["human", "mention", "off"],
                        "description":"human: every human root post starts a turn; mention: only @<name>; off: nothing does."}
                }, "required":["channel", "policy"], "additionalProperties":false}),
            ),
            (
                "info",
                json!({"type":"object", "properties":{"channel": channel},
                    "required":["channel"], "additionalProperties":false}),
            ),
        ])
    }

    fn validate_params(params: &Value) -> anyhow::Result<()> {
        static SCHEMA: std::sync::OnceLock<crate::tools::action_schema::ActionSchema> =
            std::sync::OnceLock::new();
        SCHEMA
            .get_or_init(|| {
                crate::tools::action_schema::ActionSchema::new(
                    CHANNEL_TOOL_NAME,
                    Self::tool_parameters(),
                )
            })
            .validate(params)
    }

    /// The `channel` argument, which a non-owner run may only name when
    /// it is its own conversation (ADR-078 D4: everything else reads as
    /// missing).
    fn channel_arg(params: &Value, stamp: &ActingFor) -> anyhow::Result<ChannelId> {
        let raw = params["channel"].as_str().unwrap_or_default();
        let channel = ChannelId::parse(raw)
            .ok_or_else(|| anyhow::anyhow!("Channel: invalid channel {raw}"))?;
        anyhow::ensure!(
            stamp.is_owner() || stamp.compartment.channel() == Some(channel.as_str()),
            "Channel: channel {raw} not found"
        );
        Ok(channel)
    }

    async fn require_member(&self, channel: &ChannelId, id: &PrincipalId) -> anyhow::Result<()> {
        let members = self
            .port
            .list_members(channel)
            .await
            .map_err(|e| anyhow::anyhow!("Channel: {channel}: {e}"))?;
        anyhow::ensure!(
            members.contains(&ActorRef::from(id)),
            "Channel: you are not a member of {channel}"
        );
        Ok(())
    }

    async fn require_group(&self, channel: &ChannelId) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.port.conversation_kind(channel).await? == ConversationKind::Group,
            "Channel: {channel} is a DM; a DM keeps its two members and always wakes on \
             their posts. Use create to start a group."
        );
        Ok(())
    }

    /// The canonical actor for `reference`, if `principal` may invite
    /// it: its owner, or a principal it learned (ADR-082 D5).
    async fn invitee(&self, principal: &Principal, reference: &str) -> anyhow::Result<ActorRef> {
        let subject = ActorRef::parse_scoped(reference)
            .map_err(|_| anyhow::anyhow!("Channel: use kind:local:<id> or kind:hub:<id>"))?;
        let key = self.port.key_of(&subject);
        let owner = principal.config.read().await.owner.clone();
        if self.port.key_of(&owner) == key {
            return Ok(self.port.actor_info(&owner).await.subject());
        }
        anyhow::ensure!(
            matches!(subject, ActorRef::Principal(_)),
            "Channel: a principal can add only its owner among people; ask your owner to \
             add {reference}"
        );
        let runtime = self.manager()?.runtime_did().to_string();
        let learned = crate::principal::contacts::ContactBook::of(principal)
            .list()?
            .iter()
            .filter_map(|c| c.subject_on(&runtime))
            .any(|c| self.port.key_of(&c) == key);
        anyhow::ensure!(
            learned,
            "Channel: {reference} is not in your contacts; learn it with Contacts resolve first"
        );
        Ok(self.port.resolve_actor(reference).await?.subject())
    }

    async fn invite_one(
        &self,
        principal: &Principal,
        channel: &ChannelId,
        reference: &str,
    ) -> anyhow::Result<String> {
        let invitee = self.invitee(principal, reference).await?;
        self.port
            .invite(channel, &principal.id, &invitee)
            .await
            .map_err(|e| anyhow::anyhow!("Channel: invite {reference}: {e}"))?;
        if let ActorRef::Principal(_) = &invitee {
            if let Some(local) = self.port.actor_info(&invitee).await.local_id {
                self.manager()?
                    .ensure_channel_subscriber(&PrincipalId(local), channel);
            }
        }
        Ok(self.port.actor_info(&invitee).await.address)
    }

    async fn info(
        &self,
        id: &PrincipalId,
        channel: &ChannelId,
        stamp: &ActingFor,
    ) -> anyhow::Result<Value> {
        self.require_member(channel, id).await?;
        let membership = self.port.membership(channel).await?;
        let binding = self.port.binding_for(channel, id).await?;
        let kind = self.port.conversation_kind(channel).await?;
        let wake = match kind {
            ConversationKind::Dm => WakePolicy::Human,
            ConversationKind::Group => binding
                .as_ref()
                .map_or(WakePolicy::default_for(kind), |b| b.wake),
        };
        // A local creator is stored as its principal id; a mirror's as
        // the source's address.
        let creator = membership
            .creator
            .parse::<ActorRef>()
            .unwrap_or_else(|_| ActorRef::from(&PrincipalId(membership.creator.clone())));
        let creator = self.port.actor_info(&creator).await.address;
        let mut you = json!({"member": true, "wake": wake});
        if stamp.is_owner() {
            you["session"] = json!(binding.map(|b| b.session));
        }
        Ok(json!({
            "channel": channel.as_str(),
            "kind": kind,
            "name": membership.name,
            "creator": creator,
            "participants": self.port.participants(channel).await?,
            "you": you,
        }))
    }
}

#[async_trait]
impl Tool for ChannelTool {
    fn name(&self) -> &'static str {
        CHANNEL_TOOL_NAME
    }

    /// Roster and binding changes run alone (ADR-071 exclusive dispatch).
    fn parallelizable(&self) -> bool {
        false
    }

    fn description(&self) -> String {
        "Manage your group channels. create starts a group (you are its creator and first \
         member) and can invite members at once; invite adds your owner or a principal from \
         Contacts; leave removes you from a group; wake sets what starts a turn for you in a \
         group (human: every human root post, mention: only @<your name> — the default, off: \
         nothing; posts by principals and replies never do); info shows a channel's kind, \
         creator, participants, and your wake policy. DMs are opened by ChannelSend and never \
         change. Only the owner decides create and invite: a run acting for anyone else waits \
         for the owner's approval."
            .into()
    }

    fn parameters(&self) -> Value {
        Self::tool_parameters()
    }

    async fn execute(&self, params: Value) -> anyhow::Result<Value> {
        Self::validate_params(&params)?;
        anyhow::bail!("Channel requires principal context")
    }

    async fn execute_with_context(
        &self,
        params: Value,
        ctx: &ToolContext,
    ) -> anyhow::Result<Value> {
        Self::validate_params(&params)?;
        let id = PrincipalId(
            ctx.principal_id
                .clone()
                .ok_or_else(|| anyhow::anyhow!("Channel requires principal context"))?,
        );
        let stamp = ctx.acting_for();
        match params["action"].as_str() {
            Some("info") => {
                let channel = Self::channel_arg(&params, &stamp)?;
                self.info(&id, &channel, &stamp).await
            }
            Some("create") => {
                let principal = self.principal(&id).await?;
                let name = params["name"].as_str().unwrap_or_default();
                let mut opts = peko_channel::CreateOpts::runtime(name);
                if let Some(slug) = params["slug"].as_str() {
                    opts = opts.with_id(ChannelId::for_group(slug));
                }
                let channel = self
                    .port
                    .create(&id, opts)
                    .await
                    .map_err(|e| anyhow::anyhow!("Channel: create: {e}"))?;
                let manager = self.manager()?;
                manager.ensure_group_binding(&principal, &channel).await?;
                manager.ensure_channel_subscriber(&id, &channel);
                let mut invited = Vec::new();
                for member in params["members"].as_array().into_iter().flatten() {
                    let reference = member.as_str().unwrap_or_default();
                    invited.push(self.invite_one(&principal, &channel, reference).await?);
                }
                Ok(json!({"channel": channel.as_str(), "invited": invited}))
            }
            Some("invite") => {
                let channel = Self::channel_arg(&params, &stamp)?;
                self.require_group(&channel).await?;
                let principal = self.principal(&id).await?;
                let reference = params["actor"].as_str().unwrap_or_default();
                let invited = self.invite_one(&principal, &channel, reference).await?;
                Ok(json!({"channel": channel.as_str(), "invited": [invited]}))
            }
            Some("leave") => {
                let channel = Self::channel_arg(&params, &stamp)?;
                self.require_group(&channel).await?;
                self.require_member(&channel, &id).await?;
                self.port
                    .leave(&channel, &id)
                    .await
                    .map_err(|e| anyhow::anyhow!("Channel: leave: {e}"))?;
                Ok(json!({"channel": channel.as_str(), "left": true}))
            }
            Some("wake") => {
                let channel = Self::channel_arg(&params, &stamp)?;
                self.require_group(&channel).await?;
                self.require_member(&channel, &id).await?;
                let policy = params["policy"]
                    .as_str()
                    .and_then(WakePolicy::parse)
                    .ok_or_else(|| anyhow::anyhow!("Channel: policy is human, mention, or off"))?;
                if self.port.binding_for(&channel, &id).await?.is_none() {
                    let principal = self.principal(&id).await?;
                    self.manager()?
                        .ensure_group_binding(&principal, &channel)
                        .await?;
                }
                let binding = self.port.set_wake(&channel, &id, policy).await?;
                Ok(json!({"channel": channel.as_str(), "wake": binding.wake}))
            }
            other => anyhow::bail!("Channel: unknown action {other:?}"),
        }
    }
}

#[cfg(test)]
#[path = "channel_tool_tests.rs"]
mod tests;
