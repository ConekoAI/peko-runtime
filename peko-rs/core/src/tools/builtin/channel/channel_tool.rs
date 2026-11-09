//! `Channel` — a principal manages its groups (ADR-082 D4–D6).
//!
//! Actions: `create` a group, `invite` an actor into one, `leave` it, set
//! its own `wake` policy there, choose the `role` its turns run in a
//! channel (ADR-087), make a member an `admin`, `register` it on PekoHub
//! and make invite `link`s (ADR-092), change its `board` (ADR-101), and
//! read a channel's `info`. DMs are opened
//! by `ChannelSend` and keep their members and wake policy: only `info`
//! and `role` accept one.
//!
//! Authority is split with the dispatcher. The floor refuses `create` /
//! `invite` / `role` to untrusted runs, and a trusted run's call waits
//! for the owner (`security::judge::owner_decides`). This tool keeps every
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
                "role",
                json!({"type":"object", "properties":{
                    "channel": channel,
                    "role":{"type":"string", "minLength":1,
                        "description":"A role id from RoleCatalog. Omit to return to your default conversation role."}
                }, "required":["channel"], "additionalProperties":false}),
            ),
            (
                "admin",
                json!({"type":"object", "properties":{
                    "channel": channel,
                    "actor":{"type":"string", "minLength":1,
                        "description":"A member, as kind:local:<id> or kind:hub:<id>."},
                    "admin":{"type":"boolean",
                        "description":"true makes them an admin (they may add members); false stops it."}
                }, "required":["channel", "actor", "admin"], "additionalProperties":false}),
            ),
            (
                "board",
                json!({"type":"object", "properties":{
                    "channel": channel,
                    "op":{"type":"string", "enum":["add", "claim", "update", "release", "drop"],
                        "description":"add an item; claim one for yourself; update yours (state + note); release one you hold; drop one you added."},
                    "title":{"type":"string", "minLength":1, "maxLength":120,
                        "description":"add: the work, in one line."},
                    "for":{"type":"string", "minLength":1,
                        "description":"add: the member it is asked of (address or name); it wakes them like a mention."},
                    "item":{"type":"string", "minLength":1,
                        "description":"claim/update/release/drop: the item's id from the board (e.g. b3f9a1)."},
                    "state":{"type":"string", "enum":["doing", "blocked", "done"],
                        "description":"update: the item's new state."},
                    "note":{"type":"string", "minLength":1, "maxLength":500,
                        "description":"update: what changed, what it waits on, or the outcome, in a line or two."},
                    "check_at":{"type":"string", "minLength":1,
                        "description":"claim/update: RFC 3339 time to look at it again (10 minutes to 30 days ahead). blocked without one gets 24 hours."}
                }, "required":["channel", "op"], "additionalProperties":false}),
            ),
            (
                "register",
                json!({"type":"object", "properties":{
                    "channel": channel,
                    "visibility":{"type":"string", "enum":["invite_only", "public"],
                        "description":"invite_only (default): people join only with an invite link; public: anyone with the group's ID may join."}
                }, "required":["channel"], "additionalProperties":false}),
            ),
            (
                "link",
                json!({"type":"object", "properties":{
                    "channel": channel,
                    "days":{"type":"integer", "minimum":1, "maximum":30,
                        "description":"How long the link works; default 7."},
                    "revoke":{"type":"boolean",
                        "description":"true stops every link to the group from working instead of making one."}
                }, "required":["channel"], "additionalProperties":false}),
            ),
            (
                "join",
                json!({"type":"object", "properties":{
                    "link":{"type":"string", "minLength":1,
                        "description":"A group's invite link (…/g/<id>?invite=<token>), its token, or a public group's ID."}
                }, "required":["link"], "additionalProperties":false}),
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
    async fn channel_arg(&self, params: &Value, stamp: &ActingFor) -> anyhow::Result<ChannelId> {
        let raw = params["channel"].as_str().unwrap_or_default();
        let channel = ChannelId::parse(raw)
            .ok_or_else(|| anyhow::anyhow!("Channel: invalid channel {raw}"))?;
        // ADR-095 D7: an id kept from before a DM moved still reaches it.
        let channel = self.port.forwarded(&channel).await;
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

    /// ADR-101: make one board change in `channel` as `id`, and return
    /// the item as the board now folds it.
    async fn board(
        &self,
        id: &PrincipalId,
        channel: &ChannelId,
        params: &Value,
        ctx: &ToolContext,
    ) -> anyhow::Result<Value> {
        use peko_protocol::channel::{BoardOp, BoardState};
        let text = |key: &str| params[key].as_str().map(str::to_string);
        let item = || text("item").ok_or_else(|| anyhow::anyhow!("Channel: board: name the item"));
        let op = match params["op"].as_str().unwrap_or_default() {
            "add" => {
                let title = text("title")
                    .ok_or_else(|| anyhow::anyhow!("Channel: board add: give the item a title"))?;
                let for_member = text("for");
                BoardOp::Add { title, for_member }
            }
            "claim" => BoardOp::Claim {
                item: item()?,
                check_at: text("check_at"),
            },
            "update" => {
                let state = match params["state"].as_str() {
                    Some("doing") => BoardState::Doing,
                    Some("blocked") => BoardState::Blocked,
                    Some("done") => BoardState::Done,
                    _ => anyhow::bail!("Channel: board update: state is doing, blocked, or done"),
                };
                BoardOp::Update {
                    item: item()?,
                    state,
                    note: text("note"),
                    check_at: text("check_at"),
                }
            }
            "release" => BoardOp::Release { item: item()? },
            "drop" => BoardOp::Drop { item: item()? },
            other => anyhow::bail!("Channel: board: unknown op {other:?}"),
        };
        let msg_id = peko_channel::new_msg_id();
        let mut msg = peko_channel::PostMsg::root(String::new())
            .with_board(op.clone())
            .with_msg_id(msg_id.clone());
        if let Some(via) = crate::tools::builtin::channel::channel_send::via_for_ctx(ctx).await {
            msg = msg.with_via(via);
        }
        self.port
            .post(channel, &ActorRef::from(id), msg)
            .await
            .map_err(|e| anyhow::anyhow!("Channel: {e}"))?;
        let board = self.port.board(channel).await?.unwrap_or_default();
        let item = match op.item() {
            None => board.items.iter().find(|i| i.id == msg_id),
            Some(reference) => board.find(reference).ok(),
        };
        let mut result = json!({
            "channel": channel.as_str(),
            "op": op.name(),
            "item": item.map(item_json),
            "note": "The group sees this change on the board. Mention it in your reply only if \
                     someone has to act on it.",
        });
        // Finished work often leaves a step nobody holds (2026-11-09: a
        // pushed branch nobody merged).
        if matches!(
            op,
            BoardOp::Update {
                state: BoardState::Done,
                ..
            }
        ) {
            result["note"] = json!(
                "The group sees this change on the board. If finishing it leaves follow-up \
                 work nobody holds (a merge, a release, a check), add an item for it."
            );
        }
        // Two pekos asked at once each add an item before they see the
        // other's: show the second one what is already there.
        if matches!(op, BoardOp::Add { .. } | BoardOp::Claim { .. }) {
            let similar: Vec<Value> = item
                .map(|item| board.similar_to(item).into_iter().map(item_json).collect())
                .unwrap_or_default();
            if !similar.is_empty() {
                result["similar"] = json!(similar);
                result["note"] = json!(
                    "Other open items look like the same work (see similar). If one is, drop \
                     the newer item or leave the work to whoever holds the other, and settle it \
                     in one short post."
                );
            }
        }
        Ok(result)
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
        let role = binding.as_ref().and_then(|b| b.role.clone());
        let mut you = json!({"member": true, "wake": wake, "role": role});
        if stamp.is_owner() {
            you["session"] = json!(binding.map(|b| b.session));
        }
        let mut info = json!({
            "channel": channel.as_str(),
            "kind": kind,
            "name": membership.name,
            "creator": creator,
            "participants": self.port.participants(channel).await?,
            "you": you,
        });
        // ADR-092 D1: where the group lives and who runs it.
        if let Some(group) = self.port.group_info(channel).await? {
            let me = self.port.actor_info(&ActorRef::from(id)).await.address;
            info["you"]["owner"] = json!(group.owner == me);
            info["you"]["admin"] = json!(group.admins.contains(&me));
            info["home"] = json!(group.home.map(|h| h.runtime));
            info["owner"] = json!(group.owner);
            info["admins"] = json!(group.admins);
            // ADR-101: the group's board.
            if let Some(board) = self.port.board(channel).await? {
                let now = chrono::Utc::now();
                info["board"] = json!(board.visible(now).map(item_json).collect::<Vec<_>>());
            }
            // ADR-092 D5: its PekoHub ID, once registered.
            if let Some(hub_id) = group.hub_id {
                info["hubId"] = json!(hub_id);
                info["visibility"] = json!(group.visibility);
            }
        }
        Ok(info)
    }
}

/// One board item as tools and clients see it: its short id.
fn item_json(item: &peko_channel::BoardItem) -> Value {
    let mut v = json!(item);
    v["id"] = json!(item.short_id());
    v
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
         member; made for your owner, it has them in it too) and can invite members at once; invite adds your owner or a principal from \
         Contacts; leave removes you from a group; wake sets what starts a turn for you in a \
         group (human: every human root post, mention: only @<your name> — the default, off: \
         nothing; posts by principals and replies never do); role chooses the role (from \
         RoleCatalog) your turns run in a DM or group, or returns it to your default \
         conversation role when omitted; admin lets a member add members, or stops it (the \
         group's owner only); board changes a group's shared board of work (add an item, \
         claim it before you start, update it instead of posting status, release it, drop \
         one you added; check_at brings an item you hold back to you once); register gives a group an ID on PekoHub (invite_only, the default: people join only \
         with an invite link; public: anyone with the ID may); link makes an invite link to a \
         registered group (default 7 days, at most 30), or with revoke stops all of them; join \
         makes you (not your owner) a member of a group by its invite link or a public \
         group's ID; info \
         shows a channel's kind, creator, participants, a group's home runtime, owner, admins, \
         PekoHub ID and board, and your wake policy and role. A group's members and settings change \
         at its home runtime, by its owner or an admin. DMs are opened by ChannelSend and keep \
         their members and wake policy and have no board. Only the owner decides create, \
         invite, admin, register, link, join, and role: a run acting for anyone else waits for the \
         owner's approval."
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
                let channel = self.channel_arg(&params, &stamp).await?;
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
                // A group made for the owner has them in it: they asked
                // for it, and otherwise could not read or post there
                // (2026-11-08 field test). They can leave.
                if stamp.is_owner() {
                    let owner = principal.config.read().await.owner.clone();
                    let owner = self.port.actor_info(&owner).await.subject();
                    self.port
                        .invite(&channel, &id, &owner)
                        .await
                        .map_err(|e| anyhow::anyhow!("Channel: invite your owner: {e}"))?;
                    invited.push(self.port.actor_info(&owner).await.address);
                }
                for member in params["members"].as_array().into_iter().flatten() {
                    let reference = member.as_str().unwrap_or_default();
                    let invitee = self.invitee(&principal, reference).await?;
                    let address = self.port.actor_info(&invitee).await.address;
                    if invited.contains(&address) {
                        continue;
                    }
                    invited.push(self.invite_one(&principal, &channel, reference).await?);
                }
                Ok(json!({"channel": channel.as_str(), "invited": invited}))
            }
            Some("invite") => {
                let channel = self.channel_arg(&params, &stamp).await?;
                self.require_group(&channel).await?;
                let principal = self.principal(&id).await?;
                let reference = params["actor"].as_str().unwrap_or_default();
                let invited = self.invite_one(&principal, &channel, reference).await?;
                Ok(json!({"channel": channel.as_str(), "invited": [invited]}))
            }
            Some("admin") => {
                let channel = self.channel_arg(&params, &stamp).await?;
                self.require_group(&channel).await?;
                self.require_member(&channel, &id).await?;
                let reference = params["actor"].as_str().unwrap_or_default();
                let actor = self
                    .port
                    .resolve_actor(reference)
                    .await
                    .map_err(|e| anyhow::anyhow!("Channel: admin {reference}: {e}"))?
                    .subject();
                let admin = params["admin"].as_bool().unwrap_or(false);
                self.port
                    .set_group_admin(&channel, &id, &actor, admin)
                    .await
                    .map_err(|e| anyhow::anyhow!("Channel: admin: {e}"))?;
                Ok(json!({
                    "channel": channel.as_str(),
                    "actor": self.port.actor_info(&actor).await.address,
                    "admin": admin,
                }))
            }
            Some("board") => {
                let channel = self.channel_arg(&params, &stamp).await?;
                anyhow::ensure!(
                    self.port.conversation_kind(&channel).await? == ConversationKind::Group,
                    "Channel: boards are for groups; a DM has none"
                );
                self.require_member(&channel, &id).await?;
                self.board(&id, &channel, &params, ctx).await
            }
            Some("register") => {
                let channel = self.channel_arg(&params, &stamp).await?;
                self.require_group(&channel).await?;
                self.require_member(&channel, &id).await?;
                let visibility = match params["visibility"].as_str() {
                    Some("public") => peko_channel::GroupVisibility::Public,
                    _ => peko_channel::GroupVisibility::InviteOnly,
                };
                let hub = self
                    .port
                    .register_group(&channel, &id, visibility)
                    .await
                    .map_err(|e| anyhow::anyhow!("Channel: register: {e}"))?;
                Ok(json!({
                    "channel": channel.as_str(),
                    "id": hub.id,
                    "page": hub.page,
                    "visibility": hub.visibility,
                }))
            }
            Some("link") => {
                let channel = self.channel_arg(&params, &stamp).await?;
                self.require_group(&channel).await?;
                self.require_member(&channel, &id).await?;
                if params["revoke"].as_bool() == Some(true) {
                    let revoked = self
                        .port
                        .revoke_group_links(&channel, &id)
                        .await
                        .map_err(|e| anyhow::anyhow!("Channel: link: {e}"))?;
                    return Ok(json!({"channel": channel.as_str(), "revoked": revoked}));
                }
                let days = params["days"]
                    .as_u64()
                    .and_then(|d| u32::try_from(d).ok())
                    .unwrap_or(crate::tunnel::tunnel_channel_port::INVITE_LINK_DAYS);
                let (link, expires) = self
                    .port
                    .group_invite_link(&channel, &id, days)
                    .await
                    .map_err(|e| anyhow::anyhow!("Channel: link: {e}"))?;
                let expires = chrono::DateTime::from_timestamp(expires, 0)
                    .map(|t| t.to_rfc3339())
                    .unwrap_or_default();
                Ok(json!({"channel": channel.as_str(), "link": link, "expires": expires}))
            }
            Some("join") => {
                // ADR-098 D2: the peko joins, for itself; the home's
                // invitation arrives at its runtime and completes it.
                let link = params["link"].as_str().unwrap_or_default();
                let (channel, name) = self
                    .port
                    .join_group_as(&id, link)
                    .await
                    .map_err(|e| anyhow::anyhow!("Channel: join: {e}"))?;
                let joined = self
                    .port
                    .list_members(&channel)
                    .await
                    .is_ok_and(|m| m.contains(&ActorRef::from(&id)));
                Ok(json!({
                    "channel": channel.as_str(),
                    "name": name,
                    "status": if joined { "joined" } else { "requested" },
                    "note": if joined {
                        "You are a member; post there with ChannelSend."
                    } else {
                        "Asked the group's home; you become a member when it answers (its \
                         computer must be online). Contacts list shows the group then."
                    },
                }))
            }
            Some("leave") => {
                let channel = self.channel_arg(&params, &stamp).await?;
                self.require_group(&channel).await?;
                self.require_member(&channel, &id).await?;
                self.port
                    .leave(&channel, &id)
                    .await
                    .map_err(|e| anyhow::anyhow!("Channel: leave: {e}"))?;
                Ok(json!({"channel": channel.as_str(), "left": true}))
            }
            Some("wake") => {
                let channel = self.channel_arg(&params, &stamp).await?;
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
            Some("role") => {
                let channel = self.channel_arg(&params, &stamp).await?;
                self.require_member(&channel, &id).await?;
                let principal = self.principal(&id).await?;
                let role = params["role"].as_str().map(str::to_string);
                if let Some(role) = &role {
                    let roles_dir = principal.workspace_path.join("roles");
                    crate::principal::agent_prompt::resolve_agent_prompt(role, &roles_dir)
                        .map_err(|_| {
                            anyhow::anyhow!(
                                "Channel: no role {role}; RoleCatalog lists the roles you have"
                            )
                        })?;
                }
                if self.port.binding_for(&channel, &id).await?.is_none() {
                    self.require_group(&channel).await?;
                    self.manager()?
                        .ensure_group_binding(&principal, &channel)
                        .await?;
                }
                let binding = self.port.set_role(&channel, &id, role).await?;
                Ok(json!({"channel": channel.as_str(), "role": binding.role}))
            }
            other => anyhow::bail!("Channel: unknown action {other:?}"),
        }
    }
}

#[cfg(test)]
#[path = "channel_tool_tests.rs"]
mod tests;
