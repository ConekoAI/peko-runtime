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
//! | `principal:<did>`     | Principal  | RPC: auto-provision DM, await reply up to 1 min      |
//! | `user:<id>`           | User       | Fire-and-forget note via peer messenger             |
//! | `group:<slug>`        | Group      | Fire-and-forget post (the LLM has bound a group)    |
//!
//! Symmetric counterpart of `ChannelReadTool`
//! (`tools/builtin/channel/channel_read.rs`). The principal boundary
//! is enforced by the F37 funnel + capability gate; this tool itself
//! dispatches to:
//!
//! - `ChannelPort::post` for Bare / Group / Principal (the
//!   per-target mutex + await-reply + mirror logic from the retired
//!   `send_peer` tool is now in `execute_local` / `execute_remote`).
//! - `crate::principal::messenger::global_messenger()` for the User
//!   branch (fire-and-forget note, gated to the originating user).
//!
//! ## Per-agent registration
//!
//! Unlike the pre-PR global registration, `ChannelSend` is now
//! registered **per-agent** with `caller_principal_did` bound at
//! construction (mirrors the pre-PR `send_peer` shape). This is the
//! only way to know "who is sending?" without bloating `ToolContext`
//! (which intentionally carries no caller-DID — every tool that
//! needs one binds it at registration).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use peko_auth::Subject;
use peko_channel::{ChannelError, ChannelEvent, ChannelPort, Checkpoint, PostMsg};
use peko_protocol::channel::{ChannelId as ProtoChannelId, ChannelKind as ProtoChannelKind};
use peko_session::events::MessageSource;
use peko_subject::{PrincipalDID, PrincipalId};
use peko_tools_core::Tool;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::str::FromStr;

use crate::principal::Principal;
use crate::tunnel::cross_runtime::CrossRuntimeA2aCtx;
use crate::tunnel::hub_directory::{AgentResolution, DirectoryError, ResolvedExposure};

/// Wire name registered with the ExtensionCore.
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
    /// Channel id: any of the four wire forms (`chan_*`,
    /// `principal:<did>`, `user:<id>`, `group:<slug>`).
    pub channel: String,
    /// Message body. Bare / Group: posted as-is. Principal: posted
    /// to the pair's DM channel and the target principal's reply is
    /// awaited. User: becomes the note text (no reply).
    pub text: String,
    /// User branch only: short label for the delivered note. Ignored
    /// on bare / principal / group branches.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Bare / Group branch only: TaskId of the message being replied
    /// to; omit to post a root message. Ignored on principal / user
    /// branches.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
}

/// Result of a `ChannelSend` execution. The principal branch (RPC)
/// populates `response` with the awaited reply; the bare / group
/// branches populate `task_id` with the line number assigned in the
/// channel's JSONL; the user branch populates `response` with a
/// delivery confirmation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelSendResult {
    pub success: bool,
    /// Bare / Group: empty. Principal: the awaited reply text. User:
    /// a delivery confirmation.
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub response: String,
    /// Bare / Group only: the line number the post was assigned in
    /// the channel's JSONL log. Empty on principal / user branches.
    #[serde(skip_serializing_if = "String::is_empty", default)]
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

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
/// Per-agent construction carries the caller's principal DID — the
/// F37 funnel's `ToolContext` does NOT carry the caller DID, so the
/// tool needs it bound at registration time. This eliminates the
/// "caller masquerades as a user" audit foot-gun the legacy `a2a_send`
/// had when `caller_principal_did` was unset.
///
/// The cross-runtime ctx (`Arc<CrossRuntimeA2aCtx>`) is required for
/// the principal branch (same-runtime DM exchange + cross-runtime
/// mirror fan-out). When absent — tunnel down, test harnesses —
/// register with `new_local_only` and principal targets return a
/// structured error.
pub struct ChannelSendTool {
    /// File-backed channel port (the same handle the daemon holds on
    /// `AppState`). Required for every branch.
    port: Arc<dyn ChannelPort>,
    /// The local principal's stable DID. Bound at construction from
    /// the `Agent::principal_id` (resolved via `Principal::did()` at
    /// tool registration).
    caller_principal_did: String,
    /// The local runtime's cross-runtime context. `None` when the
    /// tunnel never started (test harnesses, offline daemons): the
    /// bare / group / user branches still work — only principal
    /// targets need this.
    cross_runtime: Option<Arc<CrossRuntimeA2aCtx>>,
    /// Per-target serialization for the blocking reply await on the
    /// principal branch. Without a wire correlation id, two
    /// overlapping awaits on the same DM channel can't tell replies
    /// apart; holding the per-target mutex for the whole
    /// post→await exchange makes reply order match request order.
    /// Keyed by target DID (the caller is fixed per tool instance).
    await_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl ChannelSendTool {
    /// Build a `ChannelSendTool` bound to a specific caller principal
    /// with full principal / cross-runtime support. Used by the
    /// per-agent registration site when the daemon has the tunnel
    /// running.
    #[must_use]
    pub fn new_with_peer(
        port: Arc<dyn ChannelPort>,
        caller_principal_did: String,
        cross_runtime: Arc<CrossRuntimeA2aCtx>,
    ) -> Self {
        Self {
            port,
            caller_principal_did,
            cross_runtime: Some(cross_runtime),
            await_locks: Mutex::new(HashMap::new()),
        }
    }

    /// Build a `ChannelSendTool` bound to a specific caller principal
    /// WITHOUT the cross-runtime context. The bare / group / user
    /// branches work; principal targets return a structured error.
    /// Used by the per-agent registration site when the tunnel is
    /// not running on this daemon.
    #[must_use]
    pub fn new_local_only(port: Arc<dyn ChannelPort>, caller_principal_did: String) -> Self {
        Self {
            port,
            caller_principal_did,
            cross_runtime: None,
            await_locks: Mutex::new(HashMap::new()),
        }
    }

    /// Encode an error into the standard `ChannelSendResult` JSON
    /// shape. Mirrors the pre-PR `SendPeerTool::error_value`.
    fn error_value(&self, kind: &str, err: &str) -> serde_json::Value {
        let result = ChannelSendResult {
            success: false,
            response: String::new(),
            task_id: String::new(),
            session_id: String::new(),
            kind: Some(kind.to_string()),
            channel: String::new(),
            error: Some(err.to_string()),
        };
        serde_json::to_value(result).expect("ChannelSendResult must serialize to JSON")
    }

    /// The per-target await mutex (see the field docs).
    fn await_lock(&self, target_did: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.await_locks
            .lock()
            .expect("ChannelSend await-locks mutex poisoned")
            .entry(target_did.to_string())
            .or_default()
            .clone()
    }

    /// User branch: append a labeled note to a user's conversational
    /// session via the peer-messenger port. Fire-and-forget — users do
    /// not reply synchronously; the note appears in their next turn.
    ///
    /// Gate (v1): the target must be the user who originated this run
    /// (derived from `session_id` via the messenger's parentage walk).
    /// Cross-user sends have no grant model yet and are rejected.
    ///
    /// Free-standing over `&dyn PeerMessenger` so unit tests can
    /// substitute a stub without touching the global registry.
    async fn execute_user_target(
        messenger: &dyn crate::principal::messenger::PeerMessenger,
        principal_id: &str,
        session_id: &str,
        agent_label: Option<&str>,
        label: Option<&str>,
        target: &Subject,
        message: &str,
    ) -> serde_json::Value {
        let user_error = |err: String| {
            serde_json::to_value(ChannelSendResult {
                success: false,
                response: String::new(),
                task_id: String::new(),
                session_id: String::new(),
                kind: Some("user".to_string()),
                channel: target.to_string(),
                error: Some(err),
            })
            .expect("ChannelSendResult must serialize to JSON")
        };

        let originating = match messenger.originating_peer(principal_id, session_id).await {
            Ok(o) => o,
            Err(e) => return user_error(format!("ChannelSend: peer resolution failed: {e}")),
        };
        match originating {
            Some(o) if &o == target => {}
            Some(o) => {
                return user_error(format!(
                    "ChannelSend: can only message the user who started this conversation ({o}); \
                     cross-user sends to {target} are not permitted"
                ));
            }
            None => {
                return user_error(
                    "ChannelSend: could not resolve the user who started this conversation"
                        .to_string(),
                );
            }
        }

        let label = label
            .filter(|l| !l.is_empty())
            .or(agent_label.filter(|l| !l.is_empty()))
            .unwrap_or("agent");
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
                response: format!(
                    "Delivered as a note to {target}'s conversational session. Users do not \
                     reply synchronously — it appears in their next turn."
                ),
                task_id: String::new(),
                session_id: String::new(),
                kind: Some("user".to_string()),
                channel: target.to_string(),
                error: None,
            })
            .expect("ChannelSendResult must serialize to JSON"),
            Ok(false) => user_error(format!(
                "ChannelSend: {target} has no conversational session yet; the note was not delivered"
            )),
            Err(e) => user_error(format!("ChannelSend: note delivery failed: {e}")),
        }
    }

    /// Local principal branch: the target lives on THIS runtime. One
    /// channel store serves both principals, so the cross-runtime
    /// mirror trick is unavailable — the exchange runs on the
    /// TARGET's DM channel for the caller (where the target's
    /// responder fires on the caller's raw-id root post), and is
    /// mirrored onto the caller's own DM channel for `peko log`
    /// continuity.
    async fn execute_local(
        &self,
        ctx: &CrossRuntimeA2aCtx,
        caller: &Arc<Principal>,
        target_did: &str,
        message: &str,
    ) -> Result<serde_json::Value> {
        let Some(target) = ctx.principal_manager.find_by_did(target_did).await else {
            return Ok(self.error_value(
                "principal",
                "target principal is not loaded on this runtime",
            ));
        };
        let await_lock = self.await_lock(target_did);
        let _guard = await_lock.lock().await;

        // 1. Caller-side DM channel: the durable outbound record for
        //    the caller's `peko log` (self-authored root — the
        //    caller's own responder self-skips it).
        let caller_peer = Subject::Principal(PrincipalDID(target_did.to_string()));
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
        let own_line = match ctx
            .channel_port
            .post(&own_channel, &Subject::from(&caller.id), PostMsg::root(message))
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

        // 2. Target side: ensure the TARGET's DM channel for the
        //    caller peer, invite the caller in (idempotent; the
        //    target is the creator/member so the store's inviter
        //    check passes), subscribe BEFORE posting, then post the
        //    root. Author = the caller's raw id → the target's
        //    responder fires; the caller has no subscriber on the
        //    target's channel, so there is no double-drive.
        let target_peer = Subject::Principal(PrincipalDID(self.caller_principal_did.clone()));
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
        let mut rx = ctx.channel_port.subscribe_events(&target_channel).await;
        let line = match ctx
            .channel_port
            .post(&target_channel, &Subject::from(&caller.id), PostMsg::root(message))
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

        // 3. Await the reply on the target's channel. Same store →
        //    exact matching: a parent-bearing post authored by the
        //    target's raw id after our root line.
        let target_raw = target.id.0.clone();
        let reply = await_reply(
            &ctx.channel_port,
            &target_channel,
            &mut rx,
            &line,
            ctx.response_timeout,
            move |ev| match ev {
                ChannelEvent::Posted {
                    parent: Some(_),
                    author,
                    text,
                    ..
                } if *author == target_raw => Some(text.clone()),
                _ => None,
            },
        )
        .await;

        let Some(text) = reply else {
            return Ok(self.error_value(
                "principal",
                &format!(
                    "local ChannelSend timed out after {:?} waiting for the reply (target did={}, channel={})",
                    ctx.response_timeout,
                    target_did,
                    target_channel.as_str()
                ),
            ));
        };

        // 4. Mirror the reply onto the caller's own DM channel so
        //    `peko log` reads the full exchange: attributed to the
        //    target's raw id, parented on the outbound line. The
        //    parent bit keeps the caller's responder from reacting;
        //    `own_line` exists in that log so the parent validates.
        //    Best-effort: the reply has already been produced.
        if let Err(e) = ctx
            .channel_port
            .post_attributed(
                &own_channel,
                &Subject::from(&caller.id),
                &target.id.0,
                PostMsg::reply(own_line, &text),
            )
            .await
        {
            tracing::warn!(
                channel = %own_channel,
                "ChannelSend: caller-side reply mirror failed (reply already returned): {e}"
            );
        }

        Ok(serde_json::to_value(ChannelSendResult {
            success: true,
            response: text,
            task_id: String::new(),
            session_id: caller_ingress.child_id,
            kind: Some("principal".to_string()),
            channel: format!("principal:{target_did}"),
            error: None,
        })?)
    }

    /// Remote principal branch: the target lives on another runtime.
    /// Runs entirely on the CALLER's DM channel for the peer: invite
    /// the target's runtime on first contact, root-post the message,
    /// and await the mirrored reply on the channel broadcast.
    async fn execute_remote(
        &self,
        ctx: &CrossRuntimeA2aCtx,
        caller: &Arc<Principal>,
        target_did: &str,
        message: &str,
        resolution: &AgentResolution,
    ) -> Result<serde_json::Value> {
        let await_lock = self.await_lock(target_did);
        let _guard = await_lock.lock().await;

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
            let invitee = PrincipalId(format!("{target_did}@{}", resolution.runtime_id));
            if let Err(e) = ctx
                .channel_port
                .fanout_dm_invite(&channel, &caller.id, &self.caller_principal_did, &invitee)
                .await
            {
                return Ok(self.error_value(
                    "principal",
                    &format!("ChannelSend: DM channel invite failed: {e}"),
                ));
            }
        }

        // 3. Subscribe BEFORE posting — the per-channel broadcast only
        //    fires for appends after subscription.
        let mut rx = ctx.channel_port.subscribe_events(&channel).await;

        // 4. Root post, author = the caller's raw id: self-skipped by
        //    the caller's own responder, fires the remote one once the
        //    fan-out lands on the target's mirror.
        let line = match ctx
            .channel_port
            .post(&channel, &Subject::from(&caller.id), PostMsg::root(message))
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

        // 5. Await the mirrored reply: the first parent-bearing post
        //    after our root line whose author is neither the caller
        //    (raw id or DID forms) nor a `user:*`/`public` form — on a
        //    1:1 DM channel that is the peer's reply. (`parent` can't
        //    be matched exactly: mirror line numbers diverge between
        //    runtimes.)
        let caller_raw = caller.id.0.clone();
        let caller_did = self.caller_principal_did.clone();
        let reply = await_reply(
            &ctx.channel_port,
            &channel,
            &mut rx,
            &line,
            ctx.response_timeout,
            move |ev| match ev {
                ChannelEvent::Posted {
                    parent: Some(_),
                    author,
                    text,
                    ..
                } if *author != caller_raw
                    && *author != caller_did
                    && !author.starts_with("user:")
                    && author != "public" =>
                {
                    Some(text.clone())
                }
                _ => None,
            },
        )
        .await;

        match reply {
            Some(text) => Ok(serde_json::to_value(ChannelSendResult {
                success: true,
                response: text,
                task_id: String::new(),
                session_id: ingress.child_id,
                kind: Some("principal".to_string()),
                channel: format!("principal:{target_did}"),
                error: None,
            })?),
            None => Ok(self.error_value(
                "principal",
                &format!(
                    "remote ChannelSend timed out after {:?} (target runtime_id={}, channel={})",
                    ctx.response_timeout,
                    resolution.runtime_id,
                    channel.as_str()
                ),
            )),
        }
    }
}

/// Await the first channel event strictly after `after_line` that
/// `matches` accepts, returning its text. Wakes on the channel's
/// per-channel broadcast (subscription must precede the post that
/// prompts the reply) and re-reads via `peek_with_ids` on every wake —
/// so a `Lagged` receiver self-repairs, and a reply that landed
/// between post and first wake is still seen. `None` on timeout or a
/// closed broadcast.
async fn await_reply(
    port: &Arc<crate::tunnel::TunnelChannelPort>,
    channel: &peko_channel::ChannelId,
    rx: &mut tokio::sync::broadcast::Receiver<ChannelEvent>,
    after_line: &str,
    timeout: Duration,
    matches: impl Fn(&ChannelEvent) -> Option<String>,
) -> Option<String> {
    let since = Checkpoint(after_line.to_string());
    let sleep = tokio::time::sleep(timeout);
    tokio::pin!(sleep);
    loop {
        match port.peek_with_ids(channel, &since).await {
            Ok(events) => {
                for (_id, ev) in events {
                    if let Some(text) = matches(&ev) {
                        return Some(text);
                    }
                }
            }
            Err(e) => {
                tracing::warn!(channel = %channel, "ChannelSend: reply peek failed: {e}");
                return None;
            }
        }
        tokio::select! {
            () = &mut sleep => return None,
            recv = rx.recv() => {
                match recv {
                    // Any append (or a lag gap) → loop and re-peek.
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
                }
            }
        }
    }
}

#[async_trait]
impl Tool for ChannelSendTool {
    fn name(&self) -> &'static str {
        CHANNEL_SEND_TOOL_NAME
    }

    fn description(&self) -> String {
        r#"## Purpose
Post a message to a peko channel. The `channel` id selects the branch:

- `chan_<8 base36>` (Bare) — plain fire-and-forget post. The channel id must match one the calling principal is a member of.
- `principal:<did>` (Principal) — send a message to another Principal and RECEIVE its reply. The conversation lives on the pair's standing DM channel (continuity is automatic — there is no session to name).
- `user:<id>` (User) — deliver a fire-and-forget NOTE to that user's conversational session. The note appears in their next turn. Users do NOT reply synchronously; do not wait for an answer. You may only message the user who started this conversation.
- `group:<slug>` (Group) — plain fire-and-forget post to a group channel.

## When to Use (Principal branch)
- Delegate a task to another Principal you have access to
- Request analysis, review, or specialized work from a peer Principal
- Continue an ongoing exchange with another Principal (the DM channel keeps the history)

## When NOT to Use
- For human-to-agent communication outside the originating user (use the CLI/API instead)
- For spawning subagents of the SAME principal (use the Agent tool instead)

## Parameters
```json
{
  "channel": "chan_a1b2c3d4 | principal:did:peko:… | user:<id> | group:<slug>",
  "text":    "Please review this code for bugs",
  "parent":  "<TaskId> (Bare / Group branch only)",
  "label":   "<note label> (User branch only)"
}
```

## Response
Bare / Group: `{ "success": true, "task_id": "<line>", "channel": "<wire form>" }`.
Principal:    `{ "success": true, "kind": "principal", "response": "<the principal's reply>", "session_id": "…", "channel": "principal:<did>" }`.
User:         `{ "success": true, "kind": "user", "response": "Delivered as a note …" }` — no reply follows."#
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "channel": {
                    "type": "string",
                    "description": "Channel id. Wire form selects the dispatch: `chan_<8 base36>` (Bare post), `principal:<did>` (RPC, await reply), `user:<id>` (fire-and-forget note), `group:<slug>` (fire-and-forget group post)."
                },
                "text": {
                    "type": "string",
                    "description": "Message body. Bare / Group: posted as-is. Principal: posted to the pair's DM channel and the target principal's reply is awaited. User: becomes the note text."
                },
                "parent": {
                    "type": "string",
                    "description": "Bare / Group branch only: TaskId of the message being replied to; omit to post a root message."
                },
                "label": {
                    "type": "string",
                    "description": "User branch only: short label for the note (shown as 📨 [<label>]). Defaults to your agent name."
                }
            },
            "required": ["channel", "text"]
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

    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
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
        let channel_str = params
            .get("channel")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("ChannelSend requires 'channel' (string)"))?;
        let channel_id = ProtoChannelId::parse(channel_str).ok_or_else(|| {
            anyhow::anyhow!(
                "ChannelSend: '{channel_str}' is not a valid channel id \
                 (expected 'chan_<8 base36>', 'principal:<did>', 'user:<id>', or 'group:<slug>')"
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

        // Pull the principal id out of the ToolContext. The F37 funnel
        // supplies this; bare `execute` callers (none in production)
        // get a hard error.
        let principal_str = ctx
            .principal_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("ChannelSend requires a principal context"))?;

        // Sprint 4 dispatch: branch on `kind()` to select the right
        // semantic. The wire form IS the policy — the LLM picks by
        // choosing the prefix.
        match channel_id.kind() {
            ProtoChannelKind::Bare | ProtoChannelKind::Group => {
                self.execute_bare(&channel_id, text, parent, &principal_str)
                    .await
            }
            ProtoChannelKind::Principal => {
                self.execute_principal_branch(&channel_id, text, ctx).await
            }
            ProtoChannelKind::User => {
                self.execute_user_branch(&channel_id, text, label, ctx)
                    .await
            }
        }
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
    ) -> anyhow::Result<serde_json::Value> {
        let kind = match channel_id.kind() {
            ProtoChannelKind::Bare => "bare",
            ProtoChannelKind::Group => "group",
            _ => unreachable!("execute_bare is only called for Bare / Group kinds"),
        };
        let msg = match parent {
            Some(p) => PostMsg::reply(p, text.to_string()),
            None => PostMsg::root(text.to_string()),
        };
        let sender = Subject::from(&PrincipalId(principal_str.to_string()));
        match self.port.post(channel_id, &sender, msg).await {
            Ok(task_id) => Ok(serde_json::to_value(ChannelSendResult {
                success: true,
                response: String::new(),
                task_id,
                session_id: String::new(),
                kind: Some(kind.to_string()),
                channel: channel_id.as_str().to_string(),
                error: None,
            })
            .expect("ChannelSendResult must serialize to JSON")),
            Err(ChannelError::NotMember) => Ok(serde_json::to_value(ChannelSendResult {
                success: false,
                response: String::new(),
                task_id: String::new(),
                session_id: String::new(),
                kind: Some(kind.to_string()),
                channel: channel_id.as_str().to_string(),
                error: Some("caller is not a member of this channel".to_string()),
            })
            .expect("ChannelSendResult must serialize to JSON")),
            Err(ChannelError::NotFound(_)) => Ok(serde_json::to_value(ChannelSendResult {
                success: false,
                response: String::new(),
                task_id: String::new(),
                session_id: String::new(),
                kind: Some(kind.to_string()),
                channel: channel_id.as_str().to_string(),
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
    ) -> anyhow::Result<serde_json::Value> {
        let raw = channel_id.as_str();
        let target_principal_did = raw.strip_prefix("principal:").ok_or_else(|| {
            anyhow::anyhow!("internal: principal branch requires 'principal:' prefix")
        })?;
        if target_principal_did.is_empty() {
            return Ok(self.error_value("principal", "ChannelSend: target DID is empty"));
        }
        let Some(cross_runtime) = self.cross_runtime.as_deref() else {
            return Ok(self.error_value(
                "principal",
                "ChannelSend: principal targets require the cross-runtime context \
                 (the tunnel is not running on this daemon)",
            ));
        };
        let Some(caller) = cross_runtime
            .principal_manager
            .find_by_did(&self.caller_principal_did)
            .await
        else {
            return Ok(self.error_value(
                "principal",
                "ChannelSend: caller principal is not loaded on this runtime",
            ));
        };
        let resolution = match cross_runtime
            .directory
            .resolve_by_did(target_principal_did)
            .await
        {
            Ok(r) => r,
            Err(err) => {
                return Ok(self.error_value("principal", &match err {
                    DirectoryError::NotFound => format!(
                        "target principal not found in hub directory (did={target_principal_did})"
                    ),
                    DirectoryError::Forbidden => format!(
                        "hub directory denied resolution (did={target_principal_did}); cross-runtime \
                         ChannelSend from anonymous callers can only reach `exposure: \"public\"` \
                         principals until peko-runtime#16 runtime-attested JWT lands"
                    ),
                    other => format!("hub directory lookup failed: {other}"),
                }));
            }
        };
        if matches!(resolution.exposure, ResolvedExposure::Unexposed) {
            return Ok(self.error_value(
                "principal",
                &format!(
                    "target principal is unexposed (runtime_id={}, instance_id={})",
                    resolution.runtime_id, resolution.instance_id
                ),
            ));
        }
        if resolution.agent_did.is_empty() {
            return Ok(self.error_value(
                "principal",
                "hub directory returned an empty target DID; cannot dispatch ChannelSend \
                 without a stable target identifier",
            ));
        }
        if resolution.runtime_id == cross_runtime.caller_runtime_id {
            self.execute_local(cross_runtime, &caller, target_principal_did, text)
                .await
        } else {
            self.execute_remote(
                cross_runtime,
                &caller,
                target_principal_did,
                text,
                &resolution,
            )
            .await
        }
    }

    /// User branch: fire-and-forget note via the peer-messenger port.
    /// The originating-user gate (only the user who started this run
    /// may be addressed) lives inside `execute_user_target`.
    async fn execute_user_branch(
        &self,
        channel_id: &ProtoChannelId,
        text: &str,
        label: Option<String>,
        ctx: &peko_tools_core::exec::ToolContext,
    ) -> anyhow::Result<serde_json::Value> {
        let target = Subject::from_str(channel_id.as_str())
            .map_err(|e| anyhow::anyhow!("ChannelSend: invalid user target '{channel_id}': {e}"))?;
        let principal_str = ctx.principal_id.clone().ok_or_else(|| {
            anyhow::anyhow!("ChannelSend: user targets require a principal context")
        })?;
        let Some(messenger) = crate::principal::messenger::global_messenger() else {
            return Ok(self.error_value(
                "user",
                "ChannelSend: peer messenger is not installed (daemon not running?)",
            ));
        };
        Ok(Self::execute_user_target(
            messenger.as_ref(),
            &principal_str,
            ctx.session_id.as_deref().unwrap_or(""),
            ctx.agent_id.as_deref(),
            label.as_deref(),
            &target,
            text,
        )
        .await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peko_channel::{ChannelError, ChannelEvent, ChannelId, Checkpoint, CreateOpts};
    use peko_subject::{PrincipalId, Subject};
    use peko_tools_core::ToolContext;
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use tokio::sync::Mutex as AsyncMutex;

    /// In-memory `ChannelPort` for unit tests. Local to this tool's
    /// tests so the tool can be tested without depending on a real
    /// `ChannelStore` (file-backed) — that integration path is covered
    /// by `peko-channel`'s own `tests/integration.rs`.
    #[derive(Default)]
    struct TestChannelPort {
        events: Mutex<HashMap<ChannelId, Vec<ChannelEvent>>>,
        members: AsyncMutex<HashMap<ChannelId, Vec<Subject>>>,
        next_line: AsyncMutex<HashMap<ChannelId, u64>>,
    }

    impl TestChannelPort {
        fn is_member(&self, channel: &ChannelId, subject: &Subject) -> bool {
            // Sync lookup against the snapshot taken under the async
            // lock — fine for tests, since TestChannelPort is only
            // exercised from a single-threaded tokio runtime here.
            if let Ok(g) = self.members.try_lock() {
                g.get(channel)
                    .map(|m| m.iter().any(|p| p == subject))
                    .unwrap_or(false)
            } else {
                false
            }
        }
    }

    #[async_trait]
    impl ChannelPort for TestChannelPort {
        async fn create(
            &self,
            _creator: &PrincipalId,
            _opts: CreateOpts,
        ) -> peko_channel::Result<ChannelId> {
            Err(ChannelError::Adapter(
                "create not implemented in test".into(),
            ))
        }

        async fn invite(
            &self,
            _channel: &ChannelId,
            _inviter: &PrincipalId,
            _invitee: &Subject,
        ) -> peko_channel::Result<()> {
            Ok(())
        }

        async fn post(
            &self,
            channel: &ChannelId,
            sender: &Subject,
            msg: PostMsg,
        ) -> peko_channel::Result<String> {
            if !self.is_member(channel, sender) {
                return Err(ChannelError::NotMember);
            }
            let mut lines = self.next_line.lock().await;
            let line = lines.get(channel).copied().unwrap_or(0);
            lines.insert(channel.clone(), line + 1);
            drop(lines);
            let ev = ChannelEvent::Posted {
                channel: channel.clone(),
                author: peko_channel::subject_wire_form(sender),
                parent: msg.parent,
                text: msg.text,
                at: "2026-08-06T12:00:00Z".into(),
            };
            let mut events = self.events.lock().unwrap();
            events.entry(channel.clone()).or_default().push(ev);
            Ok(line.to_string())
        }

        async fn peek(
            &self,
            channel: &ChannelId,
            _since: &Checkpoint,
        ) -> peko_channel::Result<Vec<ChannelEvent>> {
            let events = self.events.lock().unwrap();
            Ok(events.get(channel).cloned().unwrap_or_default())
        }

        async fn peek_with_ids(
            &self,
            channel: &ChannelId,
            since: &Checkpoint,
        ) -> peko_channel::Result<Vec<(String, ChannelEvent)>> {
            let events = self.peek(channel, since).await?;
            // Test fixture: assign line numbers by event index; the
            // subscription loop only needs strictly-increasing ids.
            Ok(events
                .into_iter()
                .enumerate()
                .map(|(i, ev)| ((i + 1).to_string(), ev))
                .collect())
        }

        async fn leave(
            &self,
            _channel: &ChannelId,
            _principal: &PrincipalId,
        ) -> peko_channel::Result<()> {
            Ok(())
        }

        async fn list_members(
            &self,
            channel: &ChannelId,
        ) -> peko_channel::Result<Vec<Subject>> {
            let g = self.members.lock().await;
            Ok(g.get(channel).cloned().unwrap_or_default())
        }

        async fn list_for_principal(
            &self,
            _principal: &PrincipalId,
        ) -> peko_channel::Result<Vec<ChannelId>> {
            Ok(Vec::new())
        }

        async fn pin_to_shared(
            &self,
            _channel: &ChannelId,
        ) -> peko_channel::Result<std::path::PathBuf> {
            Err(ChannelError::Adapter(
                "pin_to_shared not implemented in test".into(),
            ))
        }
    }

    fn ctx_with(principal: &PrincipalId) -> ToolContext {
        ToolContext::for_hook_run("run", "tc", CHANNEL_SEND_TOOL_NAME)
            .with_principal_id(principal.0.clone())
    }

    fn chan_id() -> ChannelId {
        ChannelId::generate()
    }

    #[tokio::test]
    async fn send_returns_task_id_for_root_post() {
        let port = Arc::new(TestChannelPort::default());
        let channel = chan_id();
        let alice = PrincipalId::generate();
        {
            let mut members = port.members.lock().await;
            members.insert(channel.clone(), vec![Subject::from(&alice)]);
        }

        let tool = ChannelSendTool::new_local_only(port.clone(), alice.0.clone());
        let got = tool
            .execute_with_context(
                json!({ "channel": channel.as_str(), "text": "hello world" }),
                &ctx_with(&alice),
            )
            .await
            .unwrap();
        assert_eq!(got["channel"], channel.as_str());
        assert_eq!(got["kind"], "bare");
        assert_eq!(got["task_id"], "0", "first post in a channel is line 0");
    }

    #[tokio::test]
    async fn send_advances_task_id_per_post() {
        let port = Arc::new(TestChannelPort::default());
        let channel = chan_id();
        let alice = PrincipalId::generate();
        {
            let mut members = port.members.lock().await;
            members.insert(channel.clone(), vec![Subject::from(&alice)]);
        }

        let tool = ChannelSendTool::new_local_only(port.clone(), alice.0.clone());
        let first = tool
            .execute_with_context(
                json!({ "channel": channel.as_str(), "text": "first" }),
                &ctx_with(&alice),
            )
            .await
            .unwrap();
        let second = tool
            .execute_with_context(
                json!({ "channel": channel.as_str(), "text": "second" }),
                &ctx_with(&alice),
            )
            .await
            .unwrap();
        assert_eq!(first["task_id"], "0");
        assert_eq!(second["task_id"], "1");
    }

    #[tokio::test]
    async fn send_accepts_parent_for_reply() {
        let port = Arc::new(TestChannelPort::default());
        let channel = chan_id();
        let alice = PrincipalId::generate();
        {
            let mut members = port.members.lock().await;
            members.insert(channel.clone(), vec![Subject::from(&alice)]);
        }

        let tool = ChannelSendTool::new_local_only(port.clone(), alice.0.clone());
        let root = tool
            .execute_with_context(
                json!({ "channel": channel.as_str(), "text": "root" }),
                &ctx_with(&alice),
            )
            .await
            .unwrap();
        let reply = tool
            .execute_with_context(
                json!({
                    "channel": channel.as_str(),
                    "text": "reply",
                    "parent": root["task_id"].as_str().unwrap(),
                }),
                &ctx_with(&alice),
            )
            .await
            .unwrap();
        assert_eq!(reply["task_id"], "1");
        // Confirm the reply actually wired the parent into the event
        // log.
        let events = port.events.lock().unwrap();
        let reply_event = events
            .get(&channel)
            .and_then(|v| v.last())
            .expect("reply event");
        match reply_event {
            ChannelEvent::Posted { parent, text, .. } => {
                assert_eq!(parent.as_deref(), Some("0"));
                assert_eq!(text, "reply");
            }
            other => panic!("expected Posted event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn send_soft_errors_on_non_member() {
        let port = Arc::new(TestChannelPort::default());
        let channel = chan_id();
        let alice = PrincipalId::generate();
        let bob = PrincipalId::generate();
        {
            let mut members = port.members.lock().await;
            members.insert(channel.clone(), vec![Subject::from(&bob)]);
        }
        let tool = ChannelSendTool::new_local_only(port.clone(), alice.0.clone());

        let got = tool
            .execute_with_context(
                json!({ "channel": channel.as_str(), "text": "hi" }),
                &ctx_with(&alice),
            )
            .await
            .unwrap();
        assert_eq!(got["error"], "caller is not a member of this channel");
        assert_eq!(got["channel"], channel.as_str());
    }

    #[tokio::test]
    async fn send_rejects_empty_text() {
        let port = Arc::new(TestChannelPort::default());
        let channel = chan_id();
        let alice = PrincipalId::generate();
        {
            let mut members = port.members.lock().await;
            members.insert(channel.clone(), vec![Subject::from(&alice)]);
        }
        let tool = ChannelSendTool::new_local_only(port.clone(), alice.0.clone());

        let res = tool
            .execute_with_context(
                json!({ "channel": channel.as_str(), "text": "" }),
                &ctx_with(&alice),
            )
            .await;
        assert!(res.is_err(), "empty text must be rejected as Err");
    }

    #[tokio::test]
    async fn send_rejects_invalid_channel_id() {
        let port = Arc::new(TestChannelPort::default());
        let alice = PrincipalId::generate();
        let tool = ChannelSendTool::new_local_only(port.clone(), alice.0.clone());

        let res = tool
            .execute_with_context(
                json!({ "channel": "not-a-chan-id", "text": "hi" }),
                &ctx_with(&alice),
            )
            .await;
        assert!(res.is_err(), "invalid channel id must propagate as Err");
    }

    #[tokio::test]
    async fn send_requires_principal_context() {
        let port = Arc::new(TestChannelPort::default());
        let alice = PrincipalId::generate();
        let tool = ChannelSendTool::new_local_only(port, alice.0);
        // Bare `execute` — no principal context.
        let res = tool
            .execute(json!({ "channel": "chan_a1b2c3d4", "text": "hi" }))
            .await;
        assert!(
            res.is_err(),
            "bare execute must surface principal-missing error"
        );
    }

    // -----------------------------------------------------------------
    // Sprint 4 dispatch tests — four ChannelKind branches
    // -----------------------------------------------------------------

    /// Group-prefix channel ids share the bare-post path: the LLM has
    /// bound a `group:<slug>` channel and the tool just posts a JSONL
    /// line. No await, no mirror, no messenger.
    #[tokio::test]
    async fn group_dispatch_routes_through_bare_post() {
        let port = Arc::new(TestChannelPort::default());
        let channel = ChannelId::for_group("eng-standup");
        let alice = PrincipalId::generate();
        {
            let mut members = port.members.lock().await;
            members.insert(channel.clone(), vec![Subject::from(&alice)]);
        }
        let tool = ChannelSendTool::new_local_only(port.clone(), alice.0.clone());

        let got = tool
            .execute_with_context(
                json!({ "channel": "group:eng-standup", "text": "standup starts in 5" }),
                &ctx_with(&alice),
            )
            .await
            .unwrap();
        assert_eq!(got["success"], true);
        assert_eq!(got["kind"], "group");
        assert_eq!(got["channel"], "group:eng-standup");
        assert_eq!(got["task_id"], "0");
    }

    /// The principal branch requires the cross-runtime context. Without
    /// it, the tool must produce a structured `error` (not panic) —
    /// this is the offline-tunnel shape tested by
    /// `principal_send_offline`.
    #[tokio::test]
    async fn principal_branch_offline_returns_structured_error() {
        let port = Arc::new(TestChannelPort::default());
        let alice = PrincipalId::generate();
        // new_local_only → cross_runtime is None.
        let tool = ChannelSendTool::new_local_only(port.clone(), alice.0.clone());

        let got = tool
            .execute_with_context(
                json!({
                    "channel": "principal:did:key:zBob",
                    "text": "hi bob",
                }),
                &ctx_with(&alice),
            )
            .await
            .unwrap();
        assert_eq!(got["success"], false);
        assert_eq!(got["kind"], "principal");
        assert!(
            got["error"]
                .as_str()
                .unwrap_or("")
                .contains("cross-runtime context"),
            "principal branch must explain the missing cross-runtime ctx: {:?}",
            got["error"]
        );
    }

    /// The principal branch with cross_runtime bound but no caller
    /// principal loaded returns a structured error.
    #[tokio::test]
    async fn principal_branch_missing_caller_returns_error() {
        // No principal manager state — we just exercise the dispatch
        // shape; the missing caller path is the easiest one to reach
        // deterministically without a full harness.
        let port = Arc::new(TestChannelPort::default());
        let alice = PrincipalId::generate();
        let tool = ChannelSendTool::new_local_only(port.clone(), alice.0.clone());

        let got = tool
            .execute_with_context(
                json!({
                    "channel": "principal:did:key:zBob",
                    "text": "hi",
                }),
                &ctx_with(&alice),
            )
            .await
            .unwrap();
        assert_eq!(got["success"], false);
        assert_eq!(got["kind"], "principal");
        assert!(got["error"].is_string(), "expected an error message");
    }

    /// The user branch requires the global peer messenger. Without it
    /// (test harness), the tool must produce a structured `error` —
    /// not a panic.
    #[tokio::test]
    async fn user_branch_without_messenger_returns_structured_error() {
        let port = Arc::new(TestChannelPort::default());
        let alice = PrincipalId::generate();
        let tool = ChannelSendTool::new_local_only(port.clone(), alice.0.clone());

        let got = tool
            .execute_with_context(
                json!({ "channel": "user:alice", "text": "hi" }),
                &ctx_with(&alice),
            )
            .await
            .unwrap();
        assert_eq!(got["success"], false);
        assert_eq!(got["kind"], "user");
        assert!(
            got["error"]
                .as_str()
                .unwrap_or("")
                .contains("peer messenger"),
            "user branch must explain the missing messenger: {:?}",
            got["error"]
        );
    }

    /// Invalid channel id forms (unrecognized prefixes) propagate as
    /// `Err` rather than the structured `ChannelSendResult` — that's
    /// an argument-validation failure, not a tool-success-with-error.
    #[tokio::test]
    async fn unknown_prefix_propagates_as_err() {
        let port = Arc::new(TestChannelPort::default());
        let alice = PrincipalId::generate();
        let tool = ChannelSendTool::new_local_only(port, alice.0.clone());

        let res = tool
            .execute_with_context(
                json!({ "channel": "stream:abc", "text": "hi" }),
                &ctx_with(&alice),
            )
            .await;
        assert!(res.is_err(), "unknown prefix must propagate as Err");
    }

    /// `kind()` dispatch matches the wire form. This is the
    /// single-source-of-truth for the dispatch table; every test
    /// above leans on it.
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
}
