//! `ChannelSend` — `peko_channel_send` tool impl.
//!
//! Sprint 4 unification: this tool replaces both the pre-PR bare
//! `ChannelSend` AND the `send_peer` tool. The LLM-facing shape is a
//! single tool with one parameter (`channel`) whose wire form
//! selects the dispatch:
//!
//! | Wire form             | Kind       | Tool semantics                                     |
//! |-----------------------|------------|-----------------------------------------------------|
//! | `chan_<8 base36>`     | Bare       | Plain fire-and-forget post (backward compat)        |
//! | `principal:local:<id>` / `principal:hub:<id>`     | Principal  | Post on the pair's DM channel and return (ADR-072)  |
//! | `user:<id>`           | User       | Fire-and-forget note via peer messenger             |
//! | `group:<slug>`        | Group      | Fire-and-forget post (the LLM has bound a group)    |
//!
//! Symmetric counterpart of `ChannelReadTool`
//! (`tools/builtin/channel/channel_read.rs`). The principal boundary
//! is enforced at the `ChannelPort` call site; this tool itself
//! dispatches to:
//!
//! - `ChannelPort::post` for Bare / Group / Principal (`execute_local`
//!   posts on both principals' DM channels; `execute_remote` posts on
//!   the caller's and lets the runtime outbox deliver it).
//!
//! No branch waits for an answer: posting never wakes another principal
//! (ADR-072). An answer is a later post the caller reads with
//! `ChannelRead` (optionally `watch: true`).
//! - `crate::principal::messenger::global_messenger()` for the User
//!   branch (fire-and-forget note, gated to the originating user).
//!
//! ## Principal-scoped registration
//!
//! The caller DID stays bound to the principal. Production
//! tools resolve the current tunnel context from shared runtime services
//! per invocation, including tunnel connections made after installation.

use std::sync::{Arc, Weak};

use anyhow::Result;
use async_trait::async_trait;
use peko_auth::Subject;
use peko_channel::{ChannelError, ChannelPort, PostMsg};
use peko_protocol::channel::{ChannelId as ProtoChannelId, ChannelKind as ProtoChannelKind};
use peko_session::events::MessageSource;
use peko_subject::{PrincipalDID, PrincipalId};
use peko_tools_core::Tool;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::extensions::framework::core::ExtensionServices;
use crate::principal::Principal;
use crate::tunnel::cross_runtime::CrossRuntimeA2aCtx;
use crate::tunnel::hub_directory::{AgentResolution, ResolvedExposure};

/// Wire name registered with the ToolingRuntime.
pub const CHANNEL_SEND_TOOL_NAME: &str = "ChannelSend";

// ---------------------------------------------------------------------------
// ChannelSendArgs / ChannelSendResult — wire shapes (sprint 4 rename)
// ---------------------------------------------------------------------------

/// Arguments for the `ChannelSend` tool.
///
/// `channel` is a [`ChannelId`] wire form that selects the dispatch
/// branch (see module docs). `text` is the message body. `parent`
/// is the TaskId of the message being replied to (`None` for root).
///
/// The user branch carries an optional `label` for the delivered
/// note (`📨 [<label>] <text>`); defaults to the calling agent's
/// name. Ignored on bare / principal / group branches.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelSendArgs {
    /// Actor reference (`principal:<ref>`, `user:<id>`, `visitor:<id>`) or
    /// conversation ID (`chan_*`, `dm_*`, `group:<slug>`).
    #[serde(rename = "to")]
    pub channel: String,
    /// Message body. Bare / Group: posted as-is. Principal: posted
    /// to the pair's DM channel. User: becomes the note text.
    pub text: String,
    /// Human-note branch only: short label for the delivered note.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Bare / Group branch only: TaskId of the message being replied
    /// to; omit to post a root message. Actor references reject this field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
}

/// Result of a `ChannelSend` execution. The principal branch returns
/// at once with the post's `id` on the caller's DM channel (an answer
/// is a later post there, read with `ChannelRead`); the bare / group
/// branches populate `task_id` (wire name `id`, the same event id
/// ChannelRead reports) with the line number assigned in the channel's
/// JSONL; the user branch populates `response` with a delivery
/// confirmation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelSendResult {
    pub success: bool,
    #[serde(rename = "to", skip_serializing_if = "Option::is_none", default)]
    pub target: Option<String>,
    /// User: a delivery confirmation. Empty otherwise.
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub response: String,
    /// The line the post was assigned in the channel's log — its event
    /// `id` (Principal: on the caller's DM channel, `dm_channel`). Empty
    /// on the user branch.
    #[serde(rename = "id", skip_serializing_if = "String::is_empty", default)]
    pub task_id: String,
    /// Principal branch only: the caller's standing child session
    /// for the peer. Empty on bare / group / user branches.
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub session_id: String,
    /// Which dispatch branch handled the send: `"bare"` | `"principal"`
    /// | `"user"` | `"group"`. `None` on errors.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Echo of the resolved channel id (post-`ChannelId::parse`).
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub channel: String,
    /// Principal branch: the caller's DM channel with the target, where
    /// the request is recorded and the reply will appear.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub dm_channel: Option<String>,
    /// Principal branch: `delivered` (the target has the message) or
    /// `queued` (another runtime has not confirmed it yet; it is resent
    /// until it does).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub delivery: Option<String>,
    /// Principal branch: how the reply comes back.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// How long a send to another runtime waits for its delivery receipt
/// before reporting the message as queued.
const DELIVERY_CONFIRM_WAIT: std::time::Duration = std::time::Duration::from_secs(3);

/// What a principal-branch send tells the caller about answers.
const REPLY_NOTE: &str = "Posted. Posting does not wake the other principal; it sees the \
     message the next time it reads the channel. Its answer, if any, arrives on dm_channel: \
     read it later with ChannelRead, or call ChannelRead with watch: true to wait for the next \
     post there.";

// ---------------------------------------------------------------------------
// ChannelSendTool
// ---------------------------------------------------------------------------

/// `ChannelSend` tool — message a peer, user, or group channel.
///
/// Replaces the pre-PR `ChannelSend` (bare post) AND the `send_peer`
/// tool (principal / user branch) with one LLM-facing surface. The
/// LLM picks the dispatch branch by choosing the channel id's wire
/// form (see module docs).
///
/// Principal-scoped construction carries the caller's principal DID — the
/// F37 funnel's `ToolContext` does NOT carry the caller DID, so the
/// tool needs it bound at registration time. This eliminates the
/// "caller masquerades as a user" audit foot-gun the legacy `a2a_send`
/// had when `caller_principal_did` was unset.
///
/// The addressing context is installed before tunnel setup. Its local-first
/// directory resolves offline actors; tunnel setup adds signed Hub resolution
/// and remote transport. Bare test adapters can omit it and reject actor sends.
pub struct ChannelSendTool {
    /// File-backed channel port (the same handle the daemon holds on
    /// `AppState`). Required for every branch.
    port: Arc<dyn ChannelPort>,
    /// The local principal's stable DID. Bound at construction from
    /// principal (resolved via `Principal::did()` at installation).
    caller_principal_did: String,
    /// The local runtime's cross-runtime context. `None` when the
    /// tunnel never started (test harnesses, offline daemons): the
    /// bare / group / user branches still work — only principal
    /// targets need this.
    cross_runtime: Option<Arc<CrossRuntimeA2aCtx>>,
    /// Production bindings read the current tunnel service per invocation.
    services: Option<Weak<ExtensionServices>>,
}

impl ChannelSendTool {
    /// Build a `ChannelSendTool` bound to a specific caller principal
    /// WITHOUT the cross-runtime context. The bare / group / user
    /// branches work; principal targets return a structured error.
    /// Runtime installation adds a live service binding to this adapter.
    #[must_use]
    pub fn new_local_only(port: Arc<dyn ChannelPort>, caller_principal_did: String) -> Self {
        Self {
            port,
            caller_principal_did,
            cross_runtime: None,
            services: None,
        }
    }

    pub(crate) fn with_services(mut self, services: Weak<ExtensionServices>) -> Self {
        self.services = Some(services);
        self
    }

    fn cross_runtime_context(&self) -> Option<Arc<CrossRuntimeA2aCtx>> {
        match &self.services {
            Some(services) => services
                .upgrade()?
                .cross_runtime_a2a_ctx()
                .and_then(|ctx| Arc::downcast::<CrossRuntimeA2aCtx>(ctx).ok()),
            None => self.cross_runtime.clone(),
        }
    }

    /// Encode an error into the standard `ChannelSendResult` JSON
    /// shape. Mirrors the pre-PR `SendPeerTool::error_value`.
    fn error_value(&self, kind: &str, err: &str) -> serde_json::Value {
        let result = ChannelSendResult {
            success: false,
            target: None,
            response: String::new(),
            task_id: String::new(),
            session_id: String::new(),
            kind: Some(kind.to_string()),
            channel: String::new(),
            dm_channel: None,
            delivery: None,
            note: None,
            error: Some(err.to_string()),
        };
        serde_json::to_value(result).expect("ChannelSendResult must serialize to JSON")
    }

    /// User branch: append a labeled note to a user's conversational
    /// session via the peer-messenger port. Fire-and-forget — users do
    /// not reply synchronously; the note appears in their next turn.
    ///
    /// ADR-078: attribution comes from the run's stamp. A note an owner
    /// run sends, or one a run sends back to the actor it serves, keeps
    /// the agent's label. A note a non-owner run sends to anyone else
    /// carries a runtime-written `via <actor> (<trust>)` label the model
    /// cannot override, so the recipient knows on whose behalf it came.
    #[allow(clippy::too_many_arguments)]
    async fn execute_user_target(
        messenger: &dyn crate::principal::messenger::PeerMessenger,
        principal_id: &str,
        session_id: &str,
        agent_label: Option<&str>,
        label: Option<&str>,
        target: &Subject,
        message: &str,
        acting_for: &peko_tools_core::ActingFor,
    ) -> serde_json::Value {
        let user_error = |err: String| {
            serde_json::to_value(ChannelSendResult {
                success: false,
                target: Some(target.to_string()),
                response: String::new(),
                task_id: String::new(),
                session_id: String::new(),
                kind: Some(target.kind().to_string()),
                channel: String::new(),
                dm_channel: None,
                delivery: None,
                note: None,
                error: Some(err),
            })
            .expect("ChannelSendResult must serialize to JSON")
        };

        let to_actor =
            peko_channel::ActorInfo::from_subject(target, None).address == acting_for.actor;
        let attributed = (!acting_for.is_owner() && !to_actor)
            .then(|| format!("via {} ({})", acting_for.actor, acting_for.trust.as_str()));
        let label = match &attributed {
            Some(attributed) => attributed.as_str(),
            None => label
                .filter(|l| !l.is_empty())
                .or(agent_label.filter(|l| !l.is_empty()))
                .unwrap_or("agent"),
        };
        let note = format!("📨 [{label}] {message}");
        let caller_label = format!("agent {session_id}");
        match messenger
            .deliver_note(
                principal_id,
                target,
                &note,
                MessageSource::Agent,
                Some(&caller_label),
            )
            .await
        {
            Ok(true) => serde_json::to_value(ChannelSendResult {
                success: true,
            target: Some(target.to_string()),
                response: format!(
                    "Delivered as a note to {target}'s conversational session. Users do not \
                     reply synchronously — it appears in their next turn."
                ),
                task_id: String::new(),
                session_id: String::new(),
                kind: Some(target.kind().to_string()),
                channel: String::new(),
                dm_channel: None,
                delivery: None,
                note: None,
                error: None,
            })
            .expect("ChannelSendResult must serialize to JSON"),
            Ok(false) => user_error(format!(
                "ChannelSend: {target} has no conversational session yet; the note was not delivered"
            )),
            Err(e) => user_error(format!("ChannelSend: note delivery failed: {e}")),
        }
    }

    /// Local principal branch: both principals share one pair DM with
    /// independent session bindings. Posting starts no run (ADR-072).
    async fn execute_local(
        &self,
        ctx: &CrossRuntimeA2aCtx,
        caller: &Arc<Principal>,
        resolution: &AgentResolution,
        message: &str,
        via: Option<String>,
    ) -> Result<serde_json::Value> {
        let target_did = resolution.agent_did.as_str();
        let target = match ctx
            .principal_manager
            .get(PrincipalId(resolution.local_id.clone()))
            .await
        {
            Some(target) => Some(target),
            None => ctx.principal_manager.find_by_did(target_did).await,
        };
        let Some(target) = target else {
            return Ok(self.error_value(
                "principal",
                "target principal is not loaded on this runtime",
            ));
        };
        anyhow::ensure!(
            target.did().await.0 == target_did,
            "Directory instance does not match the resolved principal identity"
        );

        {
            let config = target.config.read().await;
            if matches!(config.exposure, peko_auth::Exposure::Private) {
                peko_auth::ownership::check_permission(
                    &peko_auth::ownership::principal_resource(&*config),
                    peko_auth::ownership::Permission::Chat,
                    &Subject::from(&caller.id),
                    &peko_channel::PortKeys(ctx.channel_port.as_ref()),
                )
                .map_err(|e| anyhow::anyhow!("DM delivery denied: {e}"))?;
            }
        }

        // Provision the caller's session binding and pair conversation.
        let caller_peer = Subject::from(&target.id);
        let caller_ingress = match ctx
            .principal_manager
            .ensure_peer_child_ingress(caller, &caller_peer)
            .await
        {
            Ok(i) => i,
            Err(e) => {
                return Ok(self.error_value(
                    "principal",
                    &format!("ChannelSend: caller-side peer child provisioning failed: {e}"),
                ));
            }
        };
        let Some(own_channel) = caller_ingress.dm_channel.clone() else {
            return Ok(self.error_value(
                "principal",
                "ChannelSend: peer DM channels are unavailable (no channel port attached \
                 to the principal manager)",
            ));
        };
        // The post has one stable identity, independent of its session binding.
        let msg_id = peko_channel::new_msg_id();
        let mut own_msg = PostMsg::root(message).with_msg_id(msg_id.clone());
        own_msg.via = via.clone();
        let own_line = match ctx
            .channel_port
            .post(&own_channel, &Subject::from(&caller.id), own_msg)
            .await
        {
            Ok(l) => l,
            Err(e) => {
                return Ok(self.error_value(
                    "principal",
                    &format!("ChannelSend: caller-side DM channel post failed: {e}"),
                ));
            }
        };

        // Add the target's independent binding and local membership to the
        // same pair channel. Adapters exposing separate projections still
        // receive the same message ID rather than unrelated duplicate posts.
        let target_peer = Subject::from(&caller.id);
        let target_ingress = match ctx
            .principal_manager
            .ensure_peer_child_ingress(&target, &target_peer)
            .await
        {
            Ok(i) => i,
            Err(e) => {
                return Ok(self.error_value(
                    "principal",
                    &format!("ChannelSend: target-side peer child provisioning failed: {e}"),
                ));
            }
        };
        let Some(target_channel) = target_ingress.dm_channel.clone() else {
            return Ok(self.error_value(
                "principal",
                "ChannelSend: peer DM channels are unavailable (no channel port attached \
                 to the principal manager)",
            ));
        };
        if let Err(e) = ctx
            .channel_port
            .invite(&target_channel, &target.id, &Subject::from(&caller.id))
            .await
        {
            return Ok(self.error_value(
                "principal",
                &format!("ChannelSend: could not join the target's DM channel: {e}"),
            ));
        }
        let mut target_msg = PostMsg::root(message).with_msg_id(msg_id);
        target_msg.via = via;
        if target_channel != own_channel {
            if let Err(e) = ctx
                .channel_port
                .post(&target_channel, &Subject::from(&caller.id), target_msg)
                .await
            {
                return Ok(self.error_value(
                    "principal",
                    &format!("ChannelSend: DM channel post failed: {e}"),
                ));
            }
        }

        // Return at once. Posting never starts a run in the target
        //    (ADR-072); it reads the message on its DM channel, and its
        //    answer is a post the caller reads on its own.
        Ok(serde_json::to_value(ChannelSendResult {
            success: true,
            target: Some(
                ctx.channel_port
                    .actor_info(&Subject::Principal(target_did.into()))
                    .await
                    .address,
            ),
            response: String::new(),
            task_id: own_line,
            session_id: caller_ingress.child_id,
            kind: Some("principal".to_string()),
            channel: own_channel.to_string(),
            dm_channel: Some(own_channel.as_str().to_string()),
            delivery: Some("delivered".to_string()),
            note: Some(REPLY_NOTE.to_string()),
            error: None,
        })?)
    }

    /// Remote principal branch: the target lives on another runtime.
    /// Runs entirely on the CALLER's DM channel for the peer: invite
    /// the target's runtime on first contact, root-post the message
    /// (fanned out through the runtime outbox), and wait briefly for the
    /// target runtime's delivery receipt.
    #[allow(clippy::too_many_arguments)]
    async fn execute_remote(
        &self,
        ctx: &CrossRuntimeA2aCtx,
        caller: &Arc<Principal>,
        target_did: &str,
        message: &str,
        resolution: &AgentResolution,
        via: Option<String>,
    ) -> Result<serde_json::Value> {
        // 1. Caller-side standing child + DM channel (Phase 10/11
        //    machinery): the channel is the conversation's home on
        //    this runtime, and the remote runtime mirrors it via the
        //    invite/fan-out below.
        let peer = Subject::Principal(PrincipalDID(target_did.to_string()));
        let ingress = match ctx
            .principal_manager
            .ensure_peer_child_ingress(caller, &peer)
            .await
        {
            Ok(i) => i,
            Err(e) => {
                return Ok(self.error_value(
                    "principal",
                    &format!("ChannelSend: peer child provisioning failed: {e}"),
                ));
            }
        };
        let Some(channel) = ingress.dm_channel.clone() else {
            return Ok(self.error_value(
                "principal",
                "ChannelSend: peer DM channels are unavailable (no channel port attached \
                 to the principal manager)",
            ));
        };

        // 2. First contact: no remote-member row for the target's
        //    runtime yet → invite. `fanout_dm_invite` is called
        //    DIRECTLY (not via the `ChannelPort::invite` trait path,
        //    which has no DID resolver and would degrade `creator_did`
        //    to the caller's source-local id) so the receiver can name
        //    its peer child for the caller's real DID. The remote-member
        //    row is recorded before the tunnel-handle check, so an
        //    offline tunnel still leaves the routing state durable
        //    (the post below then stays local-only and the await times
        //    out cleanly).
        let has_remote_member = match ctx.channel_port.local().list_remote_members(&channel).await {
            Ok(rows) => rows.iter().any(|rm| rm.runtime_id == resolution.runtime_id),
            Err(e) => {
                return Ok(self.error_value(
                    "principal",
                    &format!("ChannelSend: DM channel membership read failed: {e}"),
                ));
            }
        };
        if !has_remote_member {
            let invitee = PrincipalId(target_did.into());
            if let Err(e) = ctx
                .channel_port
                .fanout_dm_invite(
                    &channel,
                    &caller.id,
                    &self.caller_principal_did,
                    &invitee,
                    &resolution.runtime_id,
                )
                .await
            {
                return Ok(self.error_value(
                    "principal",
                    &format!("ChannelSend: DM channel invite failed: {e}"),
                ));
            }
        }

        // 3. Root post, author = the caller's raw id, fanned out to the
        //    target's mirror. `via` stamps the calling session's slug
        //    path (attribution only).
        let msg_id = peko_channel::new_msg_id();
        let mut msg = PostMsg::root(message).with_msg_id(msg_id.clone());
        msg.via = via;
        let line = match ctx
            .channel_port
            .post(&channel, &Subject::from(&caller.id), msg)
            .await
        {
            Ok(l) => l,
            Err(e) => {
                return Ok(self.error_value(
                    "principal",
                    &format!("ChannelSend: DM channel post failed: {e}"),
                ));
            }
        };

        // 4. A short wait for the target runtime's receipt, so the
        //    caller learns whether the message arrived. Unconfirmed
        //    messages stay in the outbox and are resent until they are
        //    (`tunnel::channel_outbox`).
        let delivered = ctx
            .channel_port
            .wait_delivered(&msg_id, DELIVERY_CONFIRM_WAIT)
            .await;
        let (delivery, note) = if delivered {
            ("delivered", REPLY_NOTE.to_string())
        } else {
            (
                "queued",
                format!(
                    "Not confirmed by the target's runtime yet (it may be offline). The message \
                     is kept and resent until it is delivered. {REPLY_NOTE}"
                ),
            )
        };
        Ok(serde_json::to_value(ChannelSendResult {
            success: true,
            target: Some(
                ctx.channel_port
                    .actor_info(&Subject::Principal(target_did.into()))
                    .await
                    .address,
            ),
            response: String::new(),
            task_id: line,
            session_id: ingress.child_id,
            kind: Some("principal".to_string()),
            channel: channel.to_string(),
            dm_channel: Some(channel.as_str().to_string()),
            delivery: Some(delivery.to_string()),
            note: Some(note),
            error: None,
        })?)
    }
}

/// Resolve the calling session's slug path (e.g. `/user-a/reminder`)
/// for `via` attribution on the posts this call makes. Best-effort:
/// no session id / workspace on the `ToolContext`, an unresolvable
/// sessions dir, an unreadable store, or a session absent from it all
/// yield `None` — attribution is audit metadata and must never fail a
/// send.
async fn via_for_ctx(ctx: &peko_tools_core::exec::ToolContext) -> Option<String> {
    let session_id = peko_session::id::SessionId::parse(ctx.session_id.as_deref()?)?;
    let workspace = ctx.workspace.as_deref().filter(|w| !w.is_empty())?;
    let sessions_dir = crate::principal::child_turns::sessions_dir_for_workspace(workspace)?;
    let metas = peko_session::manager::SessionManager::new()
        .with_sessions_dir_internal(sessions_dir)
        .list_all_sessions(false)
        .await
        .ok()?;
    // A session missing from the store would render as `sess:/<raw id>` —
    // not a slug path — so treat absence as unattributed.
    metas
        .iter()
        .any(|m| m.session_id == session_id)
        .then(|| peko_session::path::compute_path(&metas, session_id))
}

#[async_trait]
impl Tool for ChannelSendTool {
    fn name(&self) -> &'static str {
        CHANNEL_SEND_TOOL_NAME
    }

    fn description(&self) -> String {
        r#"Post a message. Every send returns at once. Posting never starts a run in another principal: it sees your message the next time it reads the channel. The `to` address picks where the message goes (use Contacts to list and resolve actors and channels):

- `principal:local:<id>` / `principal:hub:<id>` — a message to another principal, on your shared DM channel (`dm_channel` in the result). Its answer, if any, is a post on that channel: read it later with ChannelRead, or call ChannelRead with `watch: true` to wait for the next post.
- `user:local:<id>` / `user:hub:<id>` / `visitor:local:<id>` / `visitor:hub:<id>` — a note to a user's conversation; it appears in their next turn, and their answer arrives in their conversation with you.
- `dm_<hash>` / `chan_<id>` / `group:<slug>` — a post to a channel you are a member of. `parent` threads it onto an earlier message. In a group, mentioning `@<name>` does not wake that principal either — only users' mentions do.

Each post carries your session path as `via`. Not for running work in your own principal's sessions — use the Agent tool.

## Result
- Principal: `{success, kind: "principal", id, dm_channel, delivery, note}` — `id` is the message on `dm_channel`; `delivery` is `delivered`, or `queued` when the target's runtime has not confirmed it yet (it may be offline; the message is resent until it arrives).
- User: `{success, kind: "user", response}`.
- Channel / group: `{success, id, channel}` — the same `id` ChannelRead reports, usable as `parent`.
A send that cannot go out (not a member, unknown channel or principal) is an error."#
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "to": { "type": "string", "description": "Actor address (principal:local:<id>, principal:hub:<id>, user:local:<id>, user:hub:<id>, visitor:local:<id>, visitor:hub:<id>) or conversation ID (dm_<hash>, chan_<id>, group:<slug>). Resolve unfamiliar principals with Contacts." },
                "text": {
                    "type": "string",
                    "description": "Message body."
                },
                "parent": {
                    "type": "string",
                    "description": "Channel posts only: id of the message being replied to (an event id from ChannelRead or a previous send); omit to post a new message."
                },
                "label": {
                    "type": "string",
                    "description": "Notes to users only: short label for the note (shown as 📨 [<label>]); defaults to your agent name."
                }
            },
            "required": ["to", "text"]
        })
    }

    fn parallelizable(&self) -> bool {
        // Posts are NOT parallel-safe for the same channel: each post
        // appends a JSONL line, and the line-number TaskId assigned
        // depends on the order in which writes hit disk. Two parallel
        // posts in the same channel can race the lock and force one
        // caller to retry (or, worse, observe an ordering the channel
        // reader won't reconstruct). Different channels are fine.
        false
    }

    async fn execute(&self, _params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        // Bare / Group / Principal can in principle reach this path
        // (none of them need the ToolContext — they get everything
        // from the bound fields + `params`), but in production the F37
        // funnel routes through `execute_with_context` so the
        // principal_id is sourced from the principal snapshot, not the
        // LLM. Surface a clear error if `execute` is hit directly.
        Err(anyhow::anyhow!(
            "ChannelSend requires a ToolContext (use execute_with_context)"
        ))
    }

    async fn execute_with_context(
        &self,
        params: serde_json::Value,
        ctx: &peko_tools_core::exec::ToolContext,
    ) -> anyhow::Result<serde_json::Value> {
        // Parse + validate arguments.
        anyhow::ensure!(
            params.get("channel").is_none(),
            "ChannelSend: the retired 'channel' alias is unsupported; use 'to'"
        );
        let channel_str = params
            .get("to")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("ChannelSend requires 'to' (string)"))?;
        let channel_id = ProtoChannelId::parse(channel_str).ok_or_else(|| {
            anyhow::anyhow!(
                "ChannelSend: '{channel_str}' is not a valid channel id \
                 (expected 'chan_<8 base36>', 'dm_<64 hex>', 'principal:<ref>', \
                 'user:local:<id>', 'user:hub:<id>', 'visitor:local:<id>', 'visitor:hub:<id>', or 'group:<slug>')"
            )
        })?;
        let text = params
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("ChannelSend requires 'text' (string)"))?;
        if text.is_empty() {
            return Err(anyhow::anyhow!("ChannelSend requires non-empty 'text'"));
        }
        let parent = params
            .get("parent")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let label = params
            .get("label")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        anyhow::ensure!(
            parent.is_none()
                || matches!(
                    channel_id.kind(),
                    ProtoChannelKind::Bare | ProtoChannelKind::Group
                ),
            "Thread replies using the conversation channel ID, not an actor address"
        );
        anyhow::ensure!(
            label.is_none() || channel_id.kind() == ProtoChannelKind::User,
            "Labels are only supported for user/visitor notes"
        );

        // Pull the principal id out of the ToolContext. The F37 funnel
        // supplies this; bare `execute` callers (none in production)
        // get a hard error.
        let principal_str = ctx
            .principal_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("ChannelSend requires a principal context"))?;

        // `via` attribution: the calling session's slug path, stamped
        // onto every post this call makes (audit metadata only — never
        // an authority claim). Best-effort; `None` when unresolvable.
        let via = via_for_ctx(ctx).await;

        // Sprint 4 dispatch: branch on `kind()` to select the right
        // semantic. The wire form IS the policy — the LLM picks by
        // choosing the prefix.
        let mut result = match channel_id.kind() {
            ProtoChannelKind::Bare | ProtoChannelKind::Group => {
                self.execute_bare(&channel_id, text, parent, &principal_str, via)
                    .await
            }
            ProtoChannelKind::Principal => {
                self.execute_principal_branch(&channel_id, text, ctx, via)
                    .await
            }
            ProtoChannelKind::User => {
                self.execute_user_branch(&channel_id, text, label, ctx)
                    .await
            }
        }?;
        if let Some(out) = result.as_object_mut() {
            out.entry("to".to_string())
                .or_insert_with(|| json!(channel_str));
        }
        // A send that did not go through is a tool error, not a
        // successful call carrying `success: false`.
        if result.get("success") == Some(&serde_json::Value::Bool(false)) {
            let reason = result
                .get("error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("send failed");
            anyhow::bail!("ChannelSend to {channel_str} failed: {reason}");
        }
        Ok(result)
    }
}

impl ChannelSendTool {
    /// Bare / Group branch: fire-and-forget post. `principal_str`
    /// comes from `ToolContext::principal_id`; membership is enforced
    /// inside [`ChannelPort::post`] (the F37 funnel does not
    /// pre-check). Returns the standard `ChannelSendResult` JSON
    /// shape with `task_id` populated.
    async fn execute_bare(
        &self,
        channel_id: &ProtoChannelId,
        text: &str,
        parent: Option<String>,
        principal_str: &str,
        via: Option<String>,
    ) -> anyhow::Result<serde_json::Value> {
        let kind = match channel_id.kind() {
            ProtoChannelKind::Bare => "bare",
            ProtoChannelKind::Group => "group",
            _ => unreachable!("execute_bare is only called for Bare / Group kinds"),
        };
        let mut msg = match parent {
            Some(p) => PostMsg::reply(p, text.to_string()),
            None => PostMsg::root(text.to_string()),
        };
        msg.via = via;
        let sender = Subject::from(&PrincipalId(principal_str.to_string()));
        match self.port.post(channel_id, &sender, msg).await {
            Ok(task_id) => Ok(serde_json::to_value(ChannelSendResult {
                success: true,
                target: None,
                response: String::new(),
                task_id,
                session_id: String::new(),
                kind: Some(kind.to_string()),
                channel: channel_id.as_str().to_string(),
                dm_channel: None,
                delivery: None,
                note: None,
                error: None,
            })
            .expect("ChannelSendResult must serialize to JSON")),
            Err(ChannelError::NotMember) => Ok(serde_json::to_value(ChannelSendResult {
                success: false,
                target: None,
                response: String::new(),
                task_id: String::new(),
                session_id: String::new(),
                kind: Some(kind.to_string()),
                channel: channel_id.as_str().to_string(),
                dm_channel: None,
                delivery: None,
                note: None,
                error: Some("caller is not a member of this channel".to_string()),
            })
            .expect("ChannelSendResult must serialize to JSON")),
            Err(ChannelError::NotFound(_)) => Ok(serde_json::to_value(ChannelSendResult {
                success: false,
                target: None,
                response: String::new(),
                task_id: String::new(),
                session_id: String::new(),
                kind: Some(kind.to_string()),
                channel: channel_id.as_str().to_string(),
                dm_channel: None,
                delivery: None,
                note: None,
                error: Some("channel not found".to_string()),
            })
            .expect("ChannelSendResult must serialize to JSON")),
            Err(e) => Err(anyhow::anyhow!("ChannelSend post: {e}")),
        }
    }

    /// Principal branch: pull the DID out of the wire form, ensure the
    /// cross-runtime context is bound, then dispatch to
    /// [`Self::execute_local`] or [`Self::execute_remote`] based on
    /// whether the target lives on this runtime.
    async fn execute_principal_branch(
        &self,
        channel_id: &ProtoChannelId,
        text: &str,
        ctx: &peko_tools_core::exec::ToolContext,
        via: Option<String>,
    ) -> anyhow::Result<serde_json::Value> {
        let raw = channel_id.as_str();
        let reference = raw.strip_prefix("principal:").ok_or_else(|| {
            anyhow::anyhow!("internal: principal branch requires 'principal:' prefix")
        })?;
        let cross_runtime = self.cross_runtime_context();
        let Some(cross_runtime) = cross_runtime.as_deref() else {
            return Ok(self.error_value(
                "principal",
                "ChannelSend: principal targets require the cross-runtime context \
                 (the tunnel is not running on this daemon)",
            ));
        };
        let Some(caller) = cross_runtime
            .principal_manager
            .get(PrincipalId(ctx.principal_id.clone().ok_or_else(|| {
                anyhow::anyhow!("ChannelSend requires a principal context")
            })?))
            .await
        else {
            return Ok(self.error_value(
                "principal",
                "ChannelSend: caller principal is not loaded on this runtime",
            ));
        };
        anyhow::ensure!(
            caller.did().await.0 == self.caller_principal_did,
            "ChannelSend caller identity does not match its principal context"
        );
        let (scope, identifier) = reference
            .split_once(':')
            .ok_or_else(|| anyhow::anyhow!("Use principal:local:<id> or principal:hub:<id>"))?;
        let resolution = match scope {
            "local" => {
                let actor = cross_runtime.channel_port.resolve_actor(raw).await?;
                let local_id = actor
                    .local_id
                    .ok_or_else(|| anyhow::anyhow!("Actor is not local"))?;
                anyhow::ensure!(
                    cross_runtime
                        .principal_manager
                        .get(PrincipalId(local_id.clone()))
                        .await
                        .is_some(),
                    "Local principal is not hosted by this runtime"
                );
                cross_runtime
                    .directory
                    .resolve_by_handle("", &local_id)
                    .await?
            }
            "hub" => {
                let result = cross_runtime
                    .directory
                    .resolve_by_hub_id_as(identifier, &caller.did().await.0)
                    .await?;
                anyhow::ensure!(
                    result.hub_id == identifier,
                    "Directory substituted a different Hub identity"
                );
                result
            }
            _ => anyhow::bail!("Use principal:local:<id> or principal:hub:<id>"),
        };
        anyhow::ensure!(
            !resolution.agent_did.is_empty(),
            "Directory returned an empty target signing DID"
        );
        anyhow::ensure!(
            !resolution.identity_key.is_empty(),
            "Directory returned no immutable actor key"
        );
        if resolution.runtime_id != cross_runtime.caller_runtime_id {
            anyhow::ensure!(
                resolution.exposure != ResolvedExposure::LocalOnly,
                "Target is local_only"
            );
        }
        let subject = cross_runtime.actor_subject(&resolution).await?;
        cross_runtime
            .channel_port
            .register_actor(
                &subject,
                (!resolution.hub_id.is_empty()).then_some(resolution.hub_id.as_str()),
                Some(&resolution.agent_did),
                Some(&resolution.identity_key),
                resolution.name.as_deref(),
                if resolution.hub_id.is_empty() {
                    "local_only"
                } else if resolution.exposure == ResolvedExposure::Public {
                    "public"
                } else {
                    "private"
                },
            )
            .await?;
        let target_principal_did = resolution.agent_did.as_str();
        if resolution.runtime_id != cross_runtime.caller_runtime_id {
            anyhow::ensure!(
                cross_runtime
                    .principal_manager
                    .hub_id_of(&caller)
                    .await
                    .is_some(),
                "Register the sending principal before using Hub transport"
            );
        }
        if resolution.runtime_id == cross_runtime.caller_runtime_id {
            self.execute_local(cross_runtime, &caller, &resolution, text, via)
                .await
        } else {
            self.execute_remote(
                cross_runtime,
                &caller,
                target_principal_did,
                text,
                &resolution,
                via,
            )
            .await
        }
    }

    /// User branch: fire-and-forget note via the peer-messenger port.
    /// Cross-user sends are permitted (2026-09-08); attribution is
    /// derived from the originating peer inside `execute_user_target`.
    async fn execute_user_branch(
        &self,
        channel_id: &ProtoChannelId,
        text: &str,
        label: Option<String>,
        ctx: &peko_tools_core::exec::ToolContext,
    ) -> anyhow::Result<serde_json::Value> {
        let principal_str = ctx.principal_id.clone().ok_or_else(|| {
            anyhow::anyhow!("ChannelSend: user targets require a principal context")
        })?;
        let Some(messenger) = crate::principal::messenger::global_messenger() else {
            return Ok(self.error_value(
                "user",
                "ChannelSend: peer messenger is not installed (daemon not running?)",
            ));
        };
        let actor = self.port.resolve_actor(channel_id.as_str()).await?;
        let target = actor.subject();
        Ok(Self::execute_user_target(
            messenger.as_ref(),
            &principal_str,
            ctx.session_id.as_deref().unwrap_or(""),
            ctx.agent_id.as_deref(),
            label.as_deref(),
            &target,
            text,
            &ctx.acting_for(),
        )
        .await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peko_channel::{
        ChannelConfig, ChannelEvent, ChannelId, ChannelStore, Checkpoint, CreateOpts,
    };
    use peko_subject::{PrincipalId, Subject};
    use peko_tools_core::ToolContext;
    use serde_json::json;
    use std::sync::Mutex;

    /// A real file-backed channel store and one caller bound to the tool.
    struct Fixture {
        _dir: tempfile::TempDir,
        port: Arc<ChannelStore>,
        alice: PrincipalId,
        tool: ChannelSendTool,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let port = Arc::new(ChannelStore::new(ChannelConfig {
                runtime_dir: dir.path().to_path_buf(),
                shared_dir: None,
            }));
            let alice = PrincipalId::generate();
            let tool = ChannelSendTool::new_local_only(port.clone(), alice.0.clone());
            Self {
                _dir: dir,
                port,
                alice,
                tool,
            }
        }

        /// A channel created by `creator` (its only member).
        async fn channel_of(&self, creator: &PrincipalId, id: Option<ChannelId>) -> ChannelId {
            self.port
                .create(
                    creator,
                    CreateOpts {
                        id,
                        ..CreateOpts::default()
                    },
                )
                .await
                .unwrap()
        }

        async fn send(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
            self.tool
                .execute_with_context(params, &ctx_with(&self.alice))
                .await
        }

        async fn posts(&self, channel: &ChannelId) -> Vec<(Option<String>, String)> {
            self.port
                .peek(channel, &Checkpoint::zero())
                .await
                .unwrap()
                .into_iter()
                .filter_map(|ev| match ev {
                    ChannelEvent::Posted { parent, text, .. } => Some((parent, text)),
                    _ => None,
                })
                .collect()
        }
    }

    fn ctx_with(principal: &PrincipalId) -> ToolContext {
        ToolContext::for_hook_run("run", "tc", CHANNEL_SEND_TOOL_NAME)
            .with_principal_id(principal.0.clone())
    }

    #[tokio::test]
    async fn bare_posts_return_increasing_task_ids_and_land_in_the_log() {
        let fx = Fixture::new();
        let channel = fx.channel_of(&fx.alice, None).await;

        let first = fx
            .send(json!({ "to": channel.as_str(), "text": "first" }))
            .await
            .unwrap();
        let second = fx
            .send(json!({ "to": channel.as_str(), "text": "second" }))
            .await
            .unwrap();
        assert_eq!(first["success"], true, "{first}");
        assert_eq!(first["kind"], "bare");
        assert_eq!(first["channel"], channel.as_str());
        let (a, b) = (
            first["id"].as_str().unwrap().parse::<u64>().unwrap(),
            second["id"].as_str().unwrap().parse::<u64>().unwrap(),
        );
        assert!(b > a, "task ids must advance: {a} then {b}");
        assert_eq!(
            fx.posts(&channel).await,
            vec![(None, "first".to_string()), (None, "second".to_string())]
        );
    }

    #[tokio::test]
    async fn parent_makes_the_post_a_reply() {
        let fx = Fixture::new();
        let channel = fx.channel_of(&fx.alice, None).await;
        let root = fx
            .send(json!({ "to": channel.as_str(), "text": "root" }))
            .await
            .unwrap();
        let root_id = root["id"].as_str().unwrap().to_string();
        fx.send(json!({ "to": channel.as_str(), "text": "reply", "parent": root_id }))
            .await
            .unwrap();
        assert_eq!(
            fx.posts(&channel).await.last().unwrap(),
            &(Some(root_id), "reply".to_string())
        );
    }

    #[tokio::test]
    async fn non_member_and_missing_channel_are_errors() {
        let fx = Fixture::new();
        let bobs = fx.channel_of(&PrincipalId::generate(), None).await;

        let got = fx
            .send(json!({ "to": bobs.as_str(), "text": "hi" }))
            .await
            .unwrap_err();
        assert_eq!(
            got.to_string(),
            format!(
                "ChannelSend to {} failed: caller is not a member of this channel",
                bobs.as_str()
            )
        );
        assert!(fx.posts(&bobs).await.is_empty(), "nothing was posted");

        let got = fx
            .send(json!({ "to": "chan_zzzzzzzz", "text": "hi" }))
            .await
            .unwrap_err();
        assert_eq!(
            got.to_string(),
            "ChannelSend to chan_zzzzzzzz failed: channel not found"
        );
    }

    #[tokio::test]
    async fn group_ids_share_the_bare_post_path() {
        let fx = Fixture::new();
        let channel = fx
            .channel_of(&fx.alice, Some(ChannelId::for_group("eng-standup")))
            .await;
        let got = fx
            .send(json!({ "to": "group:eng-standup", "text": "standup in 5" }))
            .await
            .unwrap();
        assert_eq!(got["success"], true, "{got}");
        assert_eq!(got["kind"], "group");
        assert_eq!(got["channel"], "group:eng-standup");
        assert_eq!(
            fx.posts(&channel).await,
            vec![(None, "standup in 5".to_string())]
        );
    }

    /// Argument-validation failures are `Err`, not a tool result with
    /// `success: false`.
    #[tokio::test]
    async fn malformed_calls_are_rejected() {
        let fx = Fixture::new();
        for (params, expected) in [
            (json!({ "text": "hi" }), "requires 'to'"),
            (json!({ "to": 7, "text": "hi" }), "requires 'to'"),
            (json!({ "to": "chan_a1b2c3d4" }), "requires 'text'"),
            (
                json!({ "to": "chan_a1b2c3d4", "text": "" }),
                "non-empty 'text'",
            ),
            (
                json!({ "to": "not-a-chan-id", "text": "hi" }),
                "not a valid channel id",
            ),
            (
                json!({ "to": "stream:abc", "text": "hi" }),
                "not a valid channel id",
            ),
            (
                json!({ "to": "principal:", "text": "hi" }),
                "not a valid channel id",
            ),
        ] {
            let error = fx.send(params.clone()).await.unwrap_err().to_string();
            assert!(error.contains(expected), "{params}: {error}");
        }

        let no_principal = ToolContext::for_hook_run("run", "tc", CHANNEL_SEND_TOOL_NAME);
        let error = fx
            .tool
            .execute_with_context(
                json!({ "to": "chan_a1b2c3d4", "text": "hi" }),
                &no_principal,
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("principal context"), "{error}");

        let error = fx
            .tool
            .execute(json!({ "to": "chan_a1b2c3d4", "text": "hi" }))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("ToolContext"), "{error}");
    }

    /// Without the cross-runtime context (no tunnel on this daemon), the
    /// principal branch returns a structured error instead of panicking.
    #[tokio::test]
    async fn principal_branch_without_cross_runtime_context_is_an_error() {
        let fx = Fixture::new();
        let got = fx
            .send(json!({ "to": "principal:did:key:zBob", "text": "hi bob" }))
            .await
            .unwrap_err()
            .to_string();
        assert!(got.contains("cross-runtime context"), "{got}");
    }

    /// The user branch requires the global peer messenger. Without it
    /// the tool returns a structured error, not a panic.
    #[tokio::test]
    async fn user_branch_without_messenger_is_an_error() {
        let fx = Fixture::new();
        let got = fx
            .send(json!({ "to": "user:local:alice", "text": "hi" }))
            .await
            .unwrap_err()
            .to_string();
        assert!(got.contains("peer messenger"), "{got}");
    }

    /// `kind()` dispatch matches the wire form.
    #[test]
    fn channel_kind_dispatch_table() {
        let bare = ProtoChannelId::parse("chan_a1b2c3d4").unwrap();
        assert_eq!(bare.kind(), ProtoChannelKind::Bare);

        let principal = ProtoChannelId::parse("principal:did:key:zAlice").unwrap();
        assert_eq!(principal.kind(), ProtoChannelKind::Principal);

        let user = ProtoChannelId::parse("user:alice").unwrap();
        assert_eq!(user.kind(), ProtoChannelKind::User);

        let group = ProtoChannelId::parse("group:eng-standup").unwrap();
        assert_eq!(group.kind(), ProtoChannelKind::Group);
    }
    // ---- user branch: cross-user sends + attribution (2026-09-08) ----

    #[derive(Default)]
    struct StubMessenger {
        notes: Mutex<Vec<(String, String)>>,
        /// `Some(false)`: the target has no session; `None`: delivery errors.
        delivered: Option<bool>,
    }

    impl StubMessenger {
        fn delivering() -> Self {
            Self {
                delivered: Some(true),
                ..Default::default()
            }
        }
    }

    #[async_trait::async_trait]
    impl crate::principal::messenger::PeerMessenger for StubMessenger {
        async fn deliver_note(
            &self,
            _principal_id: &str,
            target: &Subject,
            note: &str,
            _source: MessageSource,
            _caller_label: Option<&str>,
        ) -> anyhow::Result<bool> {
            self.notes
                .lock()
                .unwrap()
                .push((target.to_string(), note.to_string()));
            self.delivered
                .ok_or_else(|| anyhow::anyhow!("messenger offline"))
        }
    }

    fn stamp_for(actor: &str, trust: peko_tools_core::Trust) -> peko_tools_core::ActingFor {
        peko_tools_core::ActingFor {
            actor: actor.into(),
            name: None,
            trust,
            compartment: peko_tools_core::Compartment::Conversation {
                channel: "dm_x".into(),
            },
            origin: peko_tools_core::Origin::Ingress { channel: None },
        }
    }

    /// ADR-078: a non-owner run's note to someone else carries the
    /// runtime's attribution, which the model's label cannot replace.
    #[tokio::test]
    async fn notes_for_someone_else_carry_the_runtime_attribution() {
        let messenger = StubMessenger::delivering();
        let target = Subject::User("hub:bob".to_string());
        let out = ChannelSendTool::execute_user_target(
            &messenger,
            "prin_x",
            "sess-1",
            None,
            Some("from the owner"),
            &target,
            "hello bob",
            &stamp_for("user:hub:alice", peko_tools_core::Trust::Untrusted),
        )
        .await;
        assert_eq!(out["success"], true, "{out}");
        let notes = messenger.notes.lock().unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].0, "user:hub:bob");
        assert!(
            notes[0].1.contains("[via user:hub:alice (untrusted)]"),
            "note must carry the runtime attribution: {}",
            notes[0].1
        );
    }

    /// A note back to the actor the run serves keeps the agent's label.
    #[tokio::test]
    async fn notes_to_the_served_actor_keep_the_agent_label() {
        let messenger = StubMessenger::delivering();
        let target = Subject::User("hub:bob".to_string());
        let out = ChannelSendTool::execute_user_target(
            &messenger,
            "prin_x",
            "sess-1",
            None,
            None,
            &target,
            "hi",
            &stamp_for("user:hub:bob", peko_tools_core::Trust::Untrusted),
        )
        .await;
        assert_eq!(out["success"], true, "{out}");
        let notes = messenger.notes.lock().unwrap();
        assert!(notes[0].1.contains("[agent]"), "note: {}", notes[0].1);
    }

    /// Empty labels are skipped: an explicit label wins, then the agent's
    /// label, then the default.
    #[tokio::test]
    async fn empty_labels_fall_through_to_the_next_choice() {
        let target = Subject::User("bob".to_string());
        for (agent_label, label, shown) in [
            (Some("worker"), Some(""), "[worker]"),
            (Some(""), Some(""), "[agent]"),
            (Some("worker"), Some("deploy"), "[deploy]"),
        ] {
            let messenger = StubMessenger::delivering();
            ChannelSendTool::execute_user_target(
                &messenger,
                "prin_x",
                "sess-1",
                agent_label,
                label,
                &target,
                "hi",
                &peko_tools_core::ActingFor::runtime(),
            )
            .await;
            let notes = messenger.notes.lock().unwrap();
            assert!(notes[0].1.contains(shown), "{shown}: {}", notes[0].1);
        }
    }

    /// A note that cannot be delivered is a structured failure naming
    /// the reason; nothing claims success.
    #[tokio::test]
    async fn undelivered_notes_are_structured_failures() {
        let target = Subject::User("bob".to_string());
        for (delivered, expected) in [
            (Some(false), "no conversational session yet"),
            (None, "note delivery failed: messenger offline"),
        ] {
            let messenger = StubMessenger {
                delivered,
                ..Default::default()
            };
            let out = ChannelSendTool::execute_user_target(
                &messenger,
                "prin_x",
                "sess-1",
                None,
                None,
                &target,
                "hi",
                &peko_tools_core::ActingFor::runtime(),
            )
            .await;
            assert_eq!(out["success"], false, "{out}");
            assert_eq!(out["kind"], "user");
            assert_eq!(out["to"], "user:bob");
            assert!(out["error"].as_str().unwrap().contains(expected), "{out}");
        }
    }
}
