//! Send Peer Tool — message a peer, user or principal (2026-08-08
//! unification of `principal_send` with agent→user notification;
//! sprint 3 Phase 12b re-foundation on channels)
//!
//! The principal branch messages another principal over that pair's
//! **DM channel** (the Phase 10/11 peer-DM machinery), replacing the
//! retired A2A RPC stack (signed `PrincipalToPrincipalRequest`
//! envelopes, pending-response registry, direct transport — all gone
//! in Phase 12b):
//!
//! - **Remote target** (another runtime): the caller's own DM channel
//!   for the peer (`dm-principal-<frag>`) is ensured, the peer's
//!   runtime is invited on first contact (a `TunnelChannelInvite`
//!   fan-out carrying the caller's real DID), and the message is a
//!   root post authored by the caller's raw principal id. The remote
//!   mirror's `PassiveBindingResponder` fires on raw-id root posts,
//!   drives the turn in the target's own peer child, and posts the
//!   reply threaded (`parent` set); the mirror fan-out lands it back
//!   on the caller's channel, where this tool awaits it on the
//!   channel broadcast.
//! - **Local target** (same runtime): one store means no mirror
//!   trick, so the message is posted root on the TARGET's DM channel
//!   for the caller (the caller is invited in first; the target's
//!   responder fires on the raw-id root post), the reply is awaited
//!   there, and the exchange is mirrored onto the caller's own DM
//!   channel for `peko log` continuity (outbound as a self-authored
//!   root, the reply attributed to the target with `parent` set so
//!   the caller's responder skips it).
//!
//! The user branch (`target: "user:<id>"`) is fire-and-forget: the
//! message becomes a labeled note in the user's conversational session
//! via the `PeerMessenger` port (`crate::principal::messenger`), gated
//! to the user who originated the current run. Users never reply
//! synchronously.
//!
//! ## Parameters
//! ```json
//! {
//!   "target": "user:local  |  did:peko:principal:<keyhash>",
//!   "message": "Please review this code",
//!   "label": "optional note label (user branch)"
//! }
//! ```
//!
//! ## Response (principal branch, blocking)
//! ```json
//! {
//!   "success": true,
//!   "kind": "principal",
//!   "response": "Review complete: 3 issues found.",
//!   "session_id": "<caller's standing child session for the peer>"
//! }
//! ```
//!
//! ## Design notes
//!
//! - **Channel continuity replaces session resumption.** The retired
//!   `session_id` argument resumed a session on the target; the
//!   per-peer standing child (which the DM channel is bound to) IS
//!   the continuous conversation now, so there is nothing to name.
//! - **No wire correlation id.** A reply is recognized structurally:
//!   the first parent-bearing post after the caller's root post whose
//!   author isn't the caller (remote branch) / whose author IS the
//!   target's raw id (local branch, same store). Overlapping awaits
//!   to the same target are serialized per tool instance so reply
//!   order matches request order; the remote responder's turn lock
//!   serializes replies on its side.
//! - **Tool name**: `"send_peer"` (renamed from `principal_send` when
//!   the user branch landed).
//!
//! Async execution and timeout are handled by the framework-level
//! `AsyncExecutionRouter` via the reserved `_async` / `_timeout`
//! parameters, same as every other tool.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::principal::Principal;
use crate::tunnel::cross_runtime::CrossRuntimeA2aCtx;
use crate::tunnel::hub_directory::{AgentResolution, DirectoryError, ResolvedExposure};
use peko_auth::Subject;
use peko_channel::{ChannelEvent, ChannelPort, Checkpoint, PostMsg};
use peko_subject::{PrincipalDID, PrincipalId};
use peko_tools_core::Tool;
use std::str::FromStr;
use std::time::Duration;

/// Arguments for the `send_peer` tool.
///
/// `target` is a [`Subject`] string: `user:<id>` delivers a
/// fire-and-forget note to that user's conversational session;
/// `principal:<did>` or a bare `did:peko:…` posts the message to the
/// pair's DM channel and awaits the target principal's reply.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendPeerArgs {
    /// Target peer: `user:<id>`, `principal:<did>`, or a bare
    /// Principal DID (`did:peko:…`).
    pub target: String,
    /// Message content. For principal targets it is posted to the
    /// pair's DM channel; for user targets it becomes the note text.
    pub message: String,
    /// User branch only: label for the delivered note
    /// (`📨 [<label>] <message>`). Defaults to the calling agent's name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Result of a principal-branch `send_peer` execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrincipalSendResult {
    pub success: bool,
    pub response: String,
    /// The caller's standing child session for the peer (the
    /// conversation's session-tier home; the DM channel is bound to
    /// it). Empty on the user branch and on errors.
    pub session_id: String,
    /// Which branch handled the send: `"principal"` (the response is
    /// the target principal's reply, awaited on the DM channel) or
    /// `"user"` (fire-and-forget note; `response` explains the
    /// delivery).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iterations: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Send Peer tool — message a peer, user or principal.
///
/// - `user:<id>`: fire-and-forget note into the user's conversational
///   session via the peer-messenger port (the note appears in their
///   next turn; users never reply synchronously).
/// - `principal:<did>` / bare `did:peko:…`: post a message to the
///   pair's DM channel and await the other principal's reply.
///
/// The tool carries the caller's principal identity (DID) at
/// construction time; the LLM never sets the caller, only the
/// target. This eliminates the "caller masquerades as a user" audit
/// foot-gun the legacy `a2a_send` had when `caller_principal_did`
/// wasn't set.
pub struct SendPeerTool {
    /// The local principal's stable DID. Bound at construction from
    /// the `Agent::principal_id` (resolved via `Principal::did()` at
    /// tool registration).
    caller_principal_did: String,
    /// The local runtime's cross-runtime context. `None` when the
    /// tunnel never started (test harnesses, offline daemons): the
    /// user branch still works — only principal targets need this.
    cross_runtime: Option<Arc<CrossRuntimeA2aCtx>>,
    /// Per-target serialization for the blocking reply await. Without
    /// a wire correlation id, two overlapping awaits on the same DM
    /// channel can't tell replies apart; holding the per-target mutex
    /// for the whole post→await exchange makes reply order match
    /// request order. Keyed by target DID (the caller is fixed per
    /// tool instance).
    await_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl SendPeerTool {
    /// Build a SendPeerTool bound to a specific caller principal.
    #[must_use]
    pub fn new(caller_principal_did: String, cross_runtime: Arc<CrossRuntimeA2aCtx>) -> Self {
        Self {
            caller_principal_did,
            cross_runtime: Some(cross_runtime),
            await_locks: Mutex::new(HashMap::new()),
        }
    }

    /// Build a user-branch-only SendPeerTool (no cross-runtime
    /// context). Principal targets return a structured error.
    #[must_use]
    pub fn new_local_only(caller_principal_did: String) -> Self {
        Self {
            caller_principal_did,
            cross_runtime: None,
            await_locks: Mutex::new(HashMap::new()),
        }
    }

    /// Encode an error into the standard `PrincipalSendResult` JSON
    /// shape.
    fn error_value(&self, err: &str) -> serde_json::Value {
        let result = PrincipalSendResult {
            success: false,
            response: String::new(),
            session_id: String::new(),
            kind: None,
            iterations: None,
            tool_calls: None,
            duration_ms: None,
            error: Some(err.to_string()),
        };
        serde_json::to_value(result).expect("PrincipalSendResult must serialize to JSON")
    }

    /// The per-target await mutex (see the field docs).
    fn await_lock(&self, target_did: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.await_locks
            .lock()
            .expect("send_peer await-locks mutex poisoned")
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
            serde_json::to_value(PrincipalSendResult {
                success: false,
                response: String::new(),
                session_id: String::new(),
                kind: Some("user".to_string()),
                iterations: None,
                tool_calls: None,
                duration_ms: None,
                error: Some(err),
            })
            .expect("PrincipalSendResult must serialize to JSON")
        };

        let originating = match messenger.originating_peer(principal_id, session_id).await {
            Ok(o) => o,
            Err(e) => return user_error(format!("send_peer: peer resolution failed: {e}")),
        };
        match originating {
            Some(o) if &o == target => {}
            Some(o) => {
                return user_error(format!(
                    "send_peer: can only message the user who started this conversation ({o}); \
                     cross-user sends to {target} are not permitted"
                ));
            }
            None => {
                return user_error(
                    "send_peer: could not resolve the user who started this conversation"
                        .to_string(),
                );
            }
        }

        let label = label
            .filter(|l| !l.is_empty())
            .or(agent_label.filter(|l| !l.is_empty()))
            .unwrap_or("agent");
        let note = format!("📨 [{label}] {message}");
        // Caller-side label for the principal's view: tells the
        // principal which agent turn pushed the note into the user's
        // conversational session. Session id is the canonical link
        // back to the engine context (subagent turns carry their
        // parent's session id).
        let caller_label = format!("agent {session_id}");
        match messenger
            .deliver_note(
                principal_id,
                target,
                &note,
                peko_session::events::MessageSource::Agent,
                Some(&caller_label),
            )
            .await
        {
            Ok(true) => serde_json::to_value(PrincipalSendResult {
                success: true,
                response: format!(
                    "Delivered as a note to {target}'s conversational session. Users do not \
                     reply synchronously — it appears in their next turn."
                ),
                session_id: String::new(),
                kind: Some("user".to_string()),
                iterations: None,
                tool_calls: None,
                duration_ms: None,
                error: None,
            })
            .expect("PrincipalSendResult must serialize to JSON"),
            Ok(false) => user_error(format!(
                "send_peer: {target} has no conversational session yet; the note was not delivered"
            )),
            Err(e) => user_error(format!("send_peer: note delivery failed: {e}")),
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
            return Ok(self.error_value("target principal is not loaded on this runtime"));
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
                return Ok(self.error_value(&format!(
                    "send_peer: caller-side peer child provisioning failed: {e}"
                )));
            }
        };
        let Some(own_channel) = caller_ingress.dm_channel.clone() else {
            return Ok(self.error_value(
                "send_peer: peer DM channels are unavailable (no channel port attached \
                 to the principal manager)",
            ));
        };
        let own_line = match ctx
            .channel_port
            .post(&own_channel, &caller.id, PostMsg::root(message))
            .await
        {
            Ok(l) => l,
            Err(e) => {
                return Ok(self.error_value(&format!(
                    "send_peer: caller-side DM channel post failed: {e}"
                )));
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
                return Ok(self.error_value(&format!(
                    "send_peer: target-side peer child provisioning failed: {e}"
                )));
            }
        };
        let Some(target_channel) = target_ingress.dm_channel.clone() else {
            return Ok(self.error_value(
                "send_peer: peer DM channels are unavailable (no channel port attached \
                 to the principal manager)",
            ));
        };
        if let Err(e) = ctx
            .channel_port
            .invite(&target_channel, &target.id, &caller.id)
            .await
        {
            return Ok(self.error_value(&format!(
                "send_peer: could not join the target's DM channel: {e}"
            )));
        }
        let mut rx = ctx.channel_port.subscribe_events(&target_channel).await;
        let line = match ctx
            .channel_port
            .post(&target_channel, &caller.id, PostMsg::root(message))
            .await
        {
            Ok(l) => l,
            Err(e) => {
                return Ok(self.error_value(&format!(
                    "send_peer: DM channel post failed: {e}"
                )));
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
                } if *author == target_raw =>
                {
                    Some(text.clone())
                }
                _ => None,
            },
        )
        .await;

        let Some(text) = reply else {
            return Ok(self.error_value(&format!(
                "local send_peer timed out after {:?} waiting for the reply (target did={}, channel={})",
                ctx.response_timeout,
                target_did,
                target_channel.as_str()
            )));
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
                &caller.id,
                &target.id.0,
                PostMsg::reply(own_line, &text),
            )
            .await
        {
            tracing::warn!(
                channel = %own_channel,
                "send_peer: caller-side reply mirror failed (reply already returned): {e}"
            );
        }

        Ok(serde_json::to_value(PrincipalSendResult {
            success: true,
            response: text,
            session_id: caller_ingress.child_id,
            kind: Some("principal".to_string()),
            iterations: None,
            tool_calls: None,
            duration_ms: None,
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
                return Ok(self.error_value(&format!(
                    "send_peer: peer child provisioning failed: {e}"
                )));
            }
        };
        let Some(channel) = ingress.dm_channel.clone() else {
            return Ok(self.error_value(
                "send_peer: peer DM channels are unavailable (no channel port attached \
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
        let has_remote_member = match ctx.channel_port.local().list_remote_members(&channel).await
        {
            Ok(rows) => rows.iter().any(|rm| rm.runtime_id == resolution.runtime_id),
            Err(e) => {
                return Ok(self.error_value(&format!(
                    "send_peer: DM channel membership read failed: {e}"
                )));
            }
        };
        if !has_remote_member {
            let invitee = PrincipalId(format!("{target_did}@{}", resolution.runtime_id));
            if let Err(e) = ctx
                .channel_port
                .fanout_dm_invite(&channel, &caller.id, &self.caller_principal_did, &invitee)
                .await
            {
                return Ok(self.error_value(&format!(
                    "send_peer: DM channel invite failed: {e}"
                )));
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
            .post(&channel, &caller.id, PostMsg::root(message))
            .await
        {
            Ok(l) => l,
            Err(e) => {
                return Ok(self.error_value(&format!(
                    "send_peer: DM channel post failed: {e}"
                )));
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
            Some(text) => Ok(serde_json::to_value(PrincipalSendResult {
                success: true,
                response: text,
                session_id: ingress.child_id,
                kind: Some("principal".to_string()),
                iterations: None,
                tool_calls: None,
                duration_ms: None,
                error: None,
            })?),
            None => Ok(self.error_value(&format!(
                "remote send_peer timed out after {:?} (target runtime_id={}, channel={})",
                ctx.response_timeout,
                resolution.runtime_id,
                channel.as_str()
            ))),
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
                tracing::warn!(channel = %channel, "send_peer: reply peek failed: {e}");
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

/// Build an `Arc<dyn Tool>` for the `send_peer` extension.
/// Replaces direct `SendPeerTool::new(...)` calls at the
/// registration site so callers don't depend on the concrete type.
#[must_use]
pub fn build_tool(
    caller_principal_did: String,
    cross_runtime: Arc<CrossRuntimeA2aCtx>,
) -> Arc<dyn Tool> {
    Arc::new(SendPeerTool::new(caller_principal_did, cross_runtime))
}

#[async_trait]
impl Tool for SendPeerTool {
    fn name(&self) -> &'static str {
        "send_peer"
    }

    fn description(&self) -> String {
        r#"## Purpose
Send a message to a peer — a human user OR another Principal. The `target` selects the branch:

- `user:<id>` — deliver a fire-and-forget NOTE to that user's conversational session. The note appears in their next turn. Users do NOT reply synchronously; do not wait for an answer. You may only message the user who started this conversation. Any agent in the tree (root or subagent) can use this to surface findings to the user.
- `principal:<did>` or a bare `did:peko:…` — send a message to another Principal and RECEIVE its reply. The conversation lives on the pair's standing DM channel (continuity is automatic — there is no session to name).

## When to Use (principal branch)
- Delegate a task to another Principal you have access to
- Request analysis, review, or specialized work from a peer Principal
- Continue an ongoing exchange with another Principal (the DM channel keeps the history)

## When NOT to Use
- For human-to-agent communication (use the CLI/API instead)
- For spawning subagents of the SAME principal (use the Agent tool instead)

## Parameters
```json
{
  "target": "user:local  |  did:peko:principal:<keyhash>",
  "message": "Please review this code for bugs",
  "label": "optional note label, user branch only"
}
```

## Response
Principal branch: `{ "success": true, "kind": "principal", "response": "<the principal's reply>", "session_id": "…" }`
User branch: `{ "success": true, "kind": "user", "response": "Delivered as a note …" }` — no reply follows."#
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "target": {
                    "type": "string",
                    "description": "Peer to message: `user:<id>` (note to a human — fire-and-forget, no reply; must be the user who started this conversation) or a Principal DID `did:peko:…` (posts to the pair's DM channel and awaits the principal's reply)."
                },
                "message": {
                    "type": "string",
                    "description": "Message content. User branch: becomes the note text. Principal branch: posted to the pair's DM channel for the target principal."
                },
                "label": {
                    "type": "string",
                    "description": "User branch only: short label for the note (shown as 📨 [<label>]). Defaults to your agent name."
                }
            },
            "required": ["target", "message"]
        })
    }

    async fn execute(&self, params: serde_json::Value) -> Result<serde_json::Value> {
        let args: SendPeerArgs =
            serde_json::from_value(params).map_err(|e| anyhow!("Invalid arguments: {e}"))?;

        // Normalize the target. Accepted forms: a bare Principal DID
        // (`did:peko:…`) or the Subject spelling `principal:<did>`.
        // User targets need a ToolContext (peer gating + messenger
        // port) and are only reachable via `execute_with_context`.
        let raw = args.target.trim();
        if raw.is_empty() {
            return Ok(self.error_value(
                "send_peer: target is required (\"user:<id>\" or a Principal DID, e.g. did:peko:…)",
            ));
        }
        if raw.starts_with("user:") || raw == "public" {
            return Ok(self.error_value(
                "send_peer: user targets require a principal runtime context",
            ));
        }
        let target_principal_did = raw.strip_prefix("principal:").unwrap_or(raw);

        // The principal branch needs the cross-runtime context. The
        // tool also registers without one (user branch only), so guard
        // here rather than at registration.
        let Some(ctx) = self.cross_runtime.as_deref() else {
            return Ok(self.error_value(
                "send_peer: principal targets require the cross-runtime context \
                 (the tunnel is not running on this daemon)",
            ));
        };

        // Shared prelude: the caller principal must be loaded — its
        // raw `PrincipalId` authors the DM channel posts.
        let Some(caller) = ctx
            .principal_manager
            .find_by_did(&self.caller_principal_did)
            .await
        else {
            return Ok(self.error_value(
                "send_peer: caller principal is not loaded on this runtime",
            ));
        };

        // Resolve the host runtime via the directory. The directory
        // returns an `AgentResolution { runtime_id, instance_id,
        // agent_did, ... }`. For principals, `agent_did` IS the
        // principal DID (pekohub canonicalizes the response shape
        // across both levels). We surface the directory's structured
        // errors verbatim so the LLM caller sees precise reasons
        // (not_found / forbidden / transport).
        let resolution = match ctx.directory.resolve_by_did(target_principal_did).await {
            Ok(r) => r,
            Err(err) => {
                return Ok(self.error_value(&match err {
                    DirectoryError::NotFound => format!(
                        "target principal not found in hub directory (did={target_principal_did})"
                    ),
                    DirectoryError::Forbidden => format!(
                        "hub directory denied resolution (did={target_principal_did}); cross-runtime \
                         send_peer from anonymous callers can only reach `exposure: \"public\"` \
                         principals until peko-runtime#16 runtime-attested JWT lands"
                    ),
                    other => format!("hub directory lookup failed: {other}"),
                }));
            }
        };

        // Defense in depth: refuse unexposed targets. The hub-side ACL
        // is the primary gate; this is the runtime-side mirror.
        if matches!(resolution.exposure, ResolvedExposure::Unexposed) {
            return Ok(self.error_value(&format!(
                "target principal is unexposed (runtime_id={}, instance_id={})",
                resolution.runtime_id, resolution.instance_id
            )));
        }

        // The hub returns the DID in `agent_did`; for principal-level
        // targets, `target_principal_did` (the input) MUST match it,
        // since the lookup key is the DID itself.
        if resolution.agent_did.is_empty() {
            // Defensive: pre-#34 directory rows may have an empty
            // `agent_did`. The by-did lookup *should* never produce
            // this (the input IS the DID), but if a hub-side
            // regression produces one, refuse to dispatch silently.
            return Ok(self.error_value(
                "hub directory returned an empty target DID; cannot dispatch send_peer \
                 without a stable target identifier",
            ));
        }

        // Same-runtime branch: one store, so the exchange runs on the
        // target's DM channel with a caller-side mirror. Remote
        // branch: the caller's DM channel mirrors to the target's
        // runtime via the channel fan-out.
        if resolution.runtime_id == ctx.caller_runtime_id {
            self.execute_local(ctx, &caller, target_principal_did, &args.message)
                .await
        } else {
            self.execute_remote(ctx, &caller, target_principal_did, &args.message, &resolution)
                .await
        }
    }

    /// Context-aware dispatch: `user:<id>` targets go through the
    /// peer-messenger port (fire-and-forget note, gated to the
    /// originating user of this run); principal targets delegate to
    /// [`Self::execute`].
    async fn execute_with_context(
        &self,
        params: serde_json::Value,
        ctx: &peko_tools_core::exec::ToolContext,
    ) -> Result<serde_json::Value> {
        let args: SendPeerArgs = serde_json::from_value(params.clone())
            .map_err(|e| anyhow!("Invalid arguments: {e}"))?;
        let raw = args.target.trim();
        if raw.starts_with("user:") {
            let target = Subject::from_str(raw)
                .map_err(|e| anyhow!("send_peer: invalid target '{raw}': {e}"))?;
            let principal_id = ctx
                .principal_id
                .clone()
                .ok_or_else(|| anyhow!("send_peer: user targets require a principal context"))?;
            let Some(messenger) = crate::principal::messenger::global_messenger() else {
                return Ok(self.error_value(
                    "send_peer: peer messenger is not installed (daemon not running?)",
                ));
            };
            return Ok(Self::execute_user_target(
                messenger.as_ref(),
                &principal_id,
                ctx.session_id.as_deref().unwrap_or(""),
                ctx.agent_id.as_deref(),
                args.label.as_deref(),
                &target,
                &args.message,
            )
            .await);
        }
        self.execute(params).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::principal::config::{Exposure, TransportPreference};
    use crate::principal::{
        DefaultPrincipalMemoryFactory, DefaultPrincipalRouterFactory, PrincipalConfig,
        PrincipalManager,
    };
    use crate::tunnel::cross_runtime_channel::CrossRuntimeChannelCtx;
    use crate::tunnel::hub_directory::{AgentDirectory, FakeAgentDirectory};
    use crate::tunnel::known_runtimes::KnownRuntimes;
    use crate::tunnel::{TunnelChannelPort, TunnelHandle, TunnelMessage};
    use ed25519_dalek::SigningKey;
    use peko_channel::{ChannelId, ChannelStore};
    use peko_providers::{LlmResolver, MockAdapter};
    use std::time::Duration;
    use tempfile::TempDir;
    use tokio::sync::RwLock;

    // --------------------------------------------------------------
    // Fixture: two principals on one manager + one channel store,
    // mirroring the daemon's wiring (manager's port and the ctx's
    // `TunnelChannelPort` share the underlying store).
    // --------------------------------------------------------------

    struct Fixture {
        _tmp: TempDir,
        manager: Arc<PrincipalManager>,
        tunnel_port: TunnelChannelPort,
        channel_port: Arc<dyn ChannelPort>,
        caller: Arc<Principal>,
        caller_did: String,
        target: Arc<Principal>,
        target_did: String,
    }

    async fn create_test_principal(
        manager: &PrincipalManager,
        workspace: &std::path::Path,
        name: &str,
        owner: Subject,
    ) -> Arc<Principal> {
        let agents_dir = workspace.join(name).join("agents");
        tokio::fs::create_dir_all(&agents_dir).await.unwrap();
        let prompt_path = agents_dir.join("primary.md");
        let prompt_body = format!(
            "---\ndescription: \"Test assistant for {name}\"\n---\n\n\
             You are {name}, a test assistant. Reply concisely.\n"
        );
        tokio::fs::write(&prompt_path, prompt_body).await.unwrap();

        let config = PrincipalConfig {
            name: name.to_string(),
            did: None,
            owner,
            identity: Default::default(),
            intent: Default::default(),
            governance: Default::default(),
            memory: Default::default(),
            routing: Default::default(),
            capabilities: Default::default(),
            exposure: Exposure::Public,
            status: None,
            permissions: Vec::new(),
            preferred_model_id: Some("mock".to_string()),
            transport_preference: TransportPreference::Auto,
            quota: None,
            children: Default::default(),
        };
        manager.create(config).await.unwrap()
    }

    async fn build_fixture() -> Fixture {
        let tmp = TempDir::new().unwrap();
        std::env::set_var("PEKO_HOME", tmp.path());
        peko_identity::init_test_env();

        let path_resolver = crate::common::paths::PathResolver::with_dirs(
            tmp.path().join("config"),
            tmp.path().join("data"),
            tmp.path().join("cache"),
        );
        let tool_runtime =
            crate::engine::tool_runtime::ToolRuntime::with_workspace(path_resolver.clone(), tmp.path())
                .await
                .expect("tool runtime should initialize");
        crate::extensions::framework::core::init_global_core(
            tool_runtime.extension_core().clone(),
        );

        let workspace = tmp.path().join("principals");
        tokio::fs::create_dir_all(&workspace).await.unwrap();

        let catalog_path = tmp.path().join("models.toml");
        let (resolver, _adapter) = LlmResolver::mock(MockAdapter::new(), catalog_path).await;

        let store = Arc::new(ChannelStore::new(peko_channel::ChannelConfig {
            runtime_dir: tmp.path().join("runtime"),
            shared_dir: None,
        }));
        let tunnel_port = TunnelChannelPort::new(store);
        let channel_port: Arc<dyn ChannelPort> = Arc::new(tunnel_port.clone());

        let manager = Arc::new(
            PrincipalManager::with_path_resolver(
                path_resolver,
                Arc::new(DefaultPrincipalMemoryFactory),
                Arc::new(DefaultPrincipalRouterFactory),
                crate::extensions::framework::async_exec::executor::standalone_inbox_registry(),
            )
            .with_resolver(resolver)
            .with_channel_port(channel_port.clone()),
        );

        let caller = create_test_principal(&manager, &workspace, "caller-p", Subject::Public).await;
        let caller_did = caller.config.read().await.did.as_ref().unwrap().0.clone();
        let target = create_test_principal(
            &manager,
            &workspace,
            "target-p",
            Subject::Principal(PrincipalDID(caller_did.clone())),
        )
        .await;
        let target_did = target.config.read().await.did.as_ref().unwrap().0.clone();

        Fixture {
            _tmp: tmp,
            manager,
            tunnel_port,
            channel_port,
            caller,
            caller_did,
            target,
            target_did,
        }
    }

    fn make_ctx(
        f: &Fixture,
        directory: Arc<dyn AgentDirectory>,
        caller_runtime_id: &str,
        response_timeout: Duration,
    ) -> Arc<CrossRuntimeA2aCtx> {
        Arc::new(CrossRuntimeA2aCtx {
            directory,
            caller_runtime_id: caller_runtime_id.to_string(),
            principal_manager: f.manager.clone(),
            channel_port: Arc::new(f.tunnel_port.clone()),
            response_timeout,
        })
    }

    /// Register the target DID in a `FakeAgentDirectory` as living on
    /// a REMOTE runtime.
    fn remote_directory(target_did: &str) -> Arc<FakeAgentDirectory> {
        let directory = Arc::new(FakeAgentDirectory::new());
        directory.register_did(
            target_did,
            AgentResolution {
                runtime_id: "did:key:zTargetRuntime".to_string(),
                instance_id: "inst-target".to_string(),
                agent_did: target_did.to_string(),
                owner_principal: Subject::Public,
                exposure: ResolvedExposure::Public,
                transport_preference:
                    crate::tunnel::known_runtimes::TransportPreference::Auto,
                direct_endpoint: None,
            },
        );
        directory
    }

    /// A channel fan-out ctx backed by a mock tunnel; the receiver
    /// captures every outbound envelope (`TunnelChannelInvite` /
    /// `TunnelChannelEvent`).
    async fn install_channel_ctx(
        f: &Fixture,
        caller_runtime_id: &str,
    ) -> tokio::sync::mpsc::Receiver<TunnelMessage> {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let ctx = Arc::new(CrossRuntimeChannelCtx {
            directory: Arc::new(FakeAgentDirectory::new()),
            signing_key: Arc::new(SigningKey::from_bytes(&[7u8; 32])),
            caller_runtime_id: caller_runtime_id.to_string(),
            tunnel: Arc::new(RwLock::new(Some(TunnelHandle::new(tx)))),
            known_runtimes: Arc::new(RwLock::new(KnownRuntimes::new())),
        });
        // Clone-shared slot: setting it on this handle is visible to
        // the ctx's `Arc<TunnelChannelPort>` built from another clone.
        f.tunnel_port.set_ctx(ctx).await;
        rx
    }

    /// The caller's DM channel for `peer` (base-slug form — the
    /// fixture never collides).
    async fn caller_dm_channel(f: &Fixture, peer: &Subject) -> ChannelId {
        let slug = crate::principal::peer_children::peer_child_slug(peer).unwrap();
        crate::principal::peer_dm::find_peer_dm_channel(
            &f.channel_port,
            &f.caller.id,
            &format!("/{slug}"),
        )
        .await
        .expect("dm lookup")
        .expect("caller DM channel exists")
    }

    /// Poll the channel log until a root `Posted` by `author` with
    /// text `text` appears; returns its line id. Panics after ~5s.
    async fn wait_for_root_post(
        f: &Fixture,
        channel: &ChannelId,
        author: &str,
        text: &str,
    ) -> String {
        for _ in 0..500 {
            let events = f
                .tunnel_port
                .peek_with_ids(channel, &Checkpoint::default())
                .await
                .expect("peek");
            for (line, ev) in events {
                if let ChannelEvent::Posted {
                    author: a,
                    parent: None,
                    text: t,
                    ..
                } = &ev
                {
                    if a == author && t == text {
                        return line;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("root post by {author} with text {text:?} never landed on {channel}");
    }

    /// Poll until the caller's DM channel for `peer` exists.
    async fn wait_for_dm_channel(f: &Fixture, peer: &Subject) -> ChannelId {
        let slug = crate::principal::peer_children::peer_child_slug(peer).unwrap();
        for _ in 0..500 {
            if let Some(channel) = crate::principal::peer_dm::find_peer_dm_channel(
                &f.channel_port,
                &f.caller.id,
                &format!("/{slug}"),
            )
            .await
            .expect("dm lookup")
            {
                return channel;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("caller DM channel for {peer} never appeared");
    }

    /// Simulate the remote side: append the target's reply directly
    /// into the caller's channel log (the fan-back mirror append the
    /// dispatcher would perform), which fires the channel broadcast.
    async fn simulate_remote_reply(f: &Fixture, channel: &ChannelId, text: &str) {
        let ev = ChannelEvent::Posted {
            channel: channel.clone(),
            author: "prin_target_local".to_string(),
            parent: Some("1".to_string()),
            text: text.to_string(),
            at: "2026-08-19T00:00:00Z".to_string(),
        };
        f.tunnel_port
            .append_remote_event(channel, &ev)
            .await
            .expect("remote reply append");
    }

    // --------------------------------------------------------------
    // Arg parsing + result shape
    // --------------------------------------------------------------

    #[test]
    fn test_send_peer_args_parsing() {
        let json = r#"{
            "target": "did:peko:principal:abc",
            "message": "Hello",
            "label": "researcher"
        }"#;
        let args: SendPeerArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.target, "did:peko:principal:abc");
        assert_eq!(args.message, "Hello");
        assert_eq!(args.label.as_deref(), Some("researcher"));
    }

    #[test]
    fn test_send_peer_args_minimal() {
        let json = r#"{
            "target": "did:peko:principal:xyz",
            "message": "Hi"
        }"#;
        let args: SendPeerArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.target, "did:peko:principal:xyz");
        assert_eq!(args.label, None);
    }

    #[test]
    fn test_result_serialization_round_trip() {
        let result = PrincipalSendResult {
            success: true,
            response: "OK".to_string(),
            session_id: "child-session-id".to_string(),
            kind: Some("principal".to_string()),
            iterations: Some(2),
            tool_calls: Some(vec![json!({"name": "Read"})]),
            duration_ms: Some(1234),
            error: None,
        };
        let v = serde_json::to_value(&result).unwrap();
        let back: PrincipalSendResult = serde_json::from_value(v).unwrap();
        assert!(back.success);
        assert_eq!(back.response, "OK");
        assert_eq!(back.session_id, "child-session-id");
        assert_eq!(back.iterations, Some(2));
        assert_eq!(back.tool_calls.as_ref().unwrap().len(), 1);
    }

    // --------------------------------------------------------------
    // Guard rails (pre-dispatch structured errors)
    // --------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn test_empty_target_errors_structured() {
        let f = build_fixture().await;
        let ctx = make_ctx(
            &f,
            Arc::new(FakeAgentDirectory::new()),
            "did:key:zLocalRuntime",
            Duration::from_millis(50),
        );
        let tool = SendPeerTool::new(f.caller_did.clone(), ctx);
        let v = tool.execute(json!({"target": "", "message": "test"})).await.unwrap();
        let r: PrincipalSendResult = serde_json::from_value(v).unwrap();
        assert!(!r.success);
        assert!(r.error.as_deref().unwrap().contains("required"));
    }

    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn test_target_not_found_returns_structured_error() {
        let f = build_fixture().await;
        // Empty directory: every DID is NotFound.
        let ctx = make_ctx(
            &f,
            Arc::new(FakeAgentDirectory::new()),
            "did:key:zLocalRuntime",
            Duration::from_millis(50),
        );
        let tool = SendPeerTool::new(f.caller_did.clone(), ctx);
        let v = tool
            .execute(json!({
                "target": "did:peko:principal:missing",
                "message": "test"
            }))
            .await
            .unwrap();
        let r: PrincipalSendResult = serde_json::from_value(v).unwrap();
        assert!(!r.success);
        assert!(r.error.as_deref().unwrap().contains("not found"));
    }

    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn test_unloaded_caller_errors_structured() {
        let f = build_fixture().await;
        let ctx = make_ctx(
            &f,
            remote_directory(&f.target_did),
            "did:key:zLocalRuntime",
            Duration::from_millis(50),
        );
        // A caller DID no loaded principal owns.
        let tool = SendPeerTool::new("did:peko:principal:ghost".to_string(), ctx);
        let v = tool
            .execute(json!({
                "target": f.target_did.clone(),
                "message": "test"
            }))
            .await
            .unwrap();
        let r: PrincipalSendResult = serde_json::from_value(v).unwrap();
        assert!(!r.success);
        assert!(r
            .error
            .as_deref()
            .unwrap()
            .contains("caller principal is not loaded"));
    }

    // --------------------------------------------------------------
    // Remote branch (cross-runtime): caller-side DM channel + invite
    // + broadcast-awaited reply
    // --------------------------------------------------------------

    /// The happy path: the outbound message lands as a caller-authored
    /// root post on the caller's DM channel, and a mirrored
    /// parent-bearing reply resolves the await.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn remote_branch_resolves_on_mirrored_reply() {
        let f = build_fixture().await;
        let ctx = make_ctx(
            &f,
            remote_directory(&f.target_did),
            "did:key:zLocalRuntime",
            Duration::from_secs(10),
        );
        let tool = Arc::new(SendPeerTool::new(f.caller_did.clone(), ctx));

        let task = {
            let tool = tool.clone();
            let target = f.target_did.clone();
            tokio::spawn(async move {
                tool.execute(json!({"target": target, "message": "hello remote"}))
                    .await
                    .unwrap()
            })
        };

        // Wait for the outbound root post, then simulate the fan-back
        // reply (parent-bearing, authored by the target's raw id).
        let peer = Subject::Principal(PrincipalDID(f.target_did.clone()));
        let channel = wait_for_dm_channel(&f, &peer).await;
        wait_for_root_post(&f, &channel, &f.caller.id.0, "hello remote").await;
        simulate_remote_reply(&f, &channel, "reply from target").await;

        let v = task.await.unwrap();
        let r: PrincipalSendResult = serde_json::from_value(v).unwrap();
        assert!(r.success, "remote send must succeed: {:?}", r.error);
        assert_eq!(r.response, "reply from target");
        assert_eq!(r.kind.as_deref(), Some("principal"));
        assert!(!r.session_id.is_empty(), "session_id is the caller's child");
    }

    /// No reply within `response_timeout` → structured timeout error;
    /// the outbound post remains durably on the channel.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn remote_branch_timeout_is_structured_and_post_is_durable() {
        let f = build_fixture().await;
        let ctx = make_ctx(
            &f,
            remote_directory(&f.target_did),
            "did:key:zLocalRuntime",
            Duration::from_millis(150),
        );
        let tool = SendPeerTool::new(f.caller_did.clone(), ctx);
        let v = tool
            .execute(json!({"target": f.target_did.clone(), "message": "anyone there"}))
            .await
            .unwrap();
        let r: PrincipalSendResult = serde_json::from_value(v).unwrap();
        assert!(!r.success);
        let err = r.error.expect("timeout error");
        assert!(err.contains("timed out"), "got: {err}");
        assert!(err.contains("did:key:zTargetRuntime"), "got: {err}");

        let peer = Subject::Principal(PrincipalDID(f.target_did.clone()));
        let channel = caller_dm_channel(&f, &peer).await;
        let events = f
            .tunnel_port
            .peek_with_ids(&channel, &Checkpoint::default())
            .await
            .unwrap();
        assert!(events.iter().any(|(_, ev)| matches!(
            ev,
            ChannelEvent::Posted { author, parent: None, text, .. }
                if *author == f.caller.id.0 && text == "anyone there"
        )));
    }

    /// First contact fires exactly one DM-aware invite: the
    /// remote-member row is recorded (before any tunnel send) and the
    /// `TunnelChannelInvite` envelope carries the caller's real DID +
    /// the DM binding marker. A second send to the same peer does NOT
    /// re-invite.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn remote_branch_first_contact_invites_with_real_did() {
        let f = build_fixture().await;
        let mut tunnel_rx = install_channel_ctx(&f, "did:key:zLocalRuntime").await;
        let ctx = make_ctx(
            &f,
            remote_directory(&f.target_did),
            "did:key:zLocalRuntime",
            Duration::from_millis(150),
        );
        let tool = SendPeerTool::new(f.caller_did.clone(), ctx);
        let v = tool
            .execute(json!({"target": f.target_did.clone(), "message": "first contact"}))
            .await
            .unwrap();
        let r: PrincipalSendResult = serde_json::from_value(v).unwrap();
        assert!(!r.success, "no responder in this harness → timeout");

        // The remote-member row was recorded for the target runtime.
        let peer = Subject::Principal(PrincipalDID(f.target_did.clone()));
        let channel = caller_dm_channel(&f, &peer).await;
        let remote = f.tunnel_port.local().list_remote_members(&channel).await.unwrap();
        assert_eq!(
            remote,
            vec![peko_channel::port::RemoteMember {
                runtime_id: "did:key:zTargetRuntime".to_string(),
                principal_id: f.target_did.clone(),
            }],
            "first contact must record the remote-member row"
        );

        // Two envelopes left the runtime: the invite (with the real
        // DID + DM marker), then the post's event fan-out.
        let first = tunnel_rx.recv().await.expect("invite envelope");
        match first {
            TunnelMessage::TunnelChannelInvite {
                recipient_runtime_id,
                creator_did,
                passive_binding,
                ..
            } => {
                assert_eq!(recipient_runtime_id, "did:key:zTargetRuntime");
                assert_eq!(creator_did, f.caller_did, "real DID, not the local id");
                assert!(passive_binding.is_some(), "DM marker rides the invite");
            }
            other => panic!("expected TunnelChannelInvite, got {other:?}"),
        }
        let second = tunnel_rx.recv().await.expect("event fan-out");
        assert!(matches!(second, TunnelMessage::TunnelChannelEvent { .. }));

        // Second send: the row exists, so no second invite — the next
        // envelope is the post's event fan-out.
        let v = tool
            .execute(json!({"target": f.target_did.clone(), "message": "second contact"}))
            .await
            .unwrap();
        let r: PrincipalSendResult = serde_json::from_value(v).unwrap();
        assert!(!r.success);
        let third = tunnel_rx.recv().await.expect("event fan-out only");
        assert!(
            matches!(third, TunnelMessage::TunnelChannelEvent { .. }),
            "second contact must not re-invite, got {third:?}"
        );
    }

    /// Overlapping awaits to the same target are serialized per tool
    /// instance: the first call's await completes before the second
    /// call's post lands, so reply order matches request order even
    /// without a wire correlation id.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn remote_branch_overlapping_awaits_serialize() {
        let f = build_fixture().await;
        let ctx = make_ctx(
            &f,
            remote_directory(&f.target_did),
            "did:key:zLocalRuntime",
            Duration::from_secs(10),
        );
        let tool = Arc::new(SendPeerTool::new(f.caller_did.clone(), ctx));

        let t1 = {
            let tool = tool.clone();
            let target = f.target_did.clone();
            tokio::spawn(async move {
                tool.execute(json!({"target": target, "message": "first"}))
                    .await
                    .unwrap()
            })
        };
        // Stagger so t1 reaches the per-target await lock first
        // (tokio's Mutex grants in FIFO request order; the head start
        // makes "who holds it first" deterministic).
        tokio::time::sleep(Duration::from_millis(100)).await;
        let t2 = {
            let tool = tool.clone();
            let target = f.target_did.clone();
            tokio::spawn(async move {
                tool.execute(json!({"target": target, "message": "second"}))
                    .await
                    .unwrap()
            })
        };

        let peer = Subject::Principal(PrincipalDID(f.target_did.clone()));
        let channel = wait_for_dm_channel(&f, &peer).await;

        // First exchange: reply to "first" before "second" is posted.
        wait_for_root_post(&f, &channel, &f.caller.id.0, "first").await;
        simulate_remote_reply(&f, &channel, "reply-one").await;
        let v1 = t1.await.unwrap();
        let r1: PrincipalSendResult = serde_json::from_value(v1).unwrap();
        assert!(r1.success, "first call: {:?}", r1.error);
        assert_eq!(r1.response, "reply-one");

        // The mutex held through the await, so "second" is only posted
        // now. Reply to it; the second call resolves with ITS reply.
        wait_for_root_post(&f, &channel, &f.caller.id.0, "second").await;
        simulate_remote_reply(&f, &channel, "reply-two").await;
        let v2 = t2.await.unwrap();
        let r2: PrincipalSendResult = serde_json::from_value(v2).unwrap();
        assert!(r2.success, "second call: {:?}", r2.error);
        assert_eq!(r2.response, "reply-two");
    }

    // --------------------------------------------------------------
    // Local branch (same runtime): target-channel exchange +
    // caller-channel mirror
    // --------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn local_branch_exchanges_on_target_channel_and_mirrors_back() {
        let f = build_fixture().await;
        // LocalFirst resolution → same runtime → local branch.
        let directory = Arc::new(crate::tunnel::local_directory::LocalFirstAgentDirectory::new(
            "did:key:zLocalRuntime",
            f.manager.clone(),
            Arc::new(FakeAgentDirectory::new()),
        ));
        let ctx = make_ctx(&f, directory, "did:key:zLocalRuntime", Duration::from_secs(10));
        let tool = Arc::new(SendPeerTool::new(f.caller_did.clone(), ctx));

        let task = {
            let tool = tool.clone();
            let target = f.target_did.clone();
            tokio::spawn(async move {
                tool.execute(json!({"target": target, "message": "hello local"}))
                    .await
                    .unwrap()
            })
        };

        // Wait for the caller's root post on the TARGET's DM channel,
        // then play the target's responder: a threaded reply authored
        // by the target's raw id.
        let target_peer = Subject::Principal(PrincipalDID(f.caller_did.clone()));
        let target_slug = crate::principal::peer_children::peer_child_slug(&target_peer).unwrap();
        let mut target_channel = None;
        for _ in 0..500 {
            if let Some(channel) = crate::principal::peer_dm::find_peer_dm_channel(
                &f.channel_port,
                &f.target.id,
                &format!("/{target_slug}"),
            )
            .await
            .expect("dm lookup")
            {
                target_channel = Some(channel);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let target_channel = target_channel.expect("target DM channel never appeared");
        let root_line =
            wait_for_root_post(&f, &target_channel, &f.caller.id.0, "hello local").await;
        f.channel_port
            .post(
                &target_channel,
                &f.target.id,
                PostMsg::reply(root_line.clone(), "local reply"),
            )
            .await
            .expect("target reply post");

        let v = task.await.unwrap();
        let r: PrincipalSendResult = serde_json::from_value(v).unwrap();
        assert!(r.success, "local send must succeed: {:?}", r.error);
        assert_eq!(r.response, "local reply");
        assert!(!r.session_id.is_empty());

        // The target's channel holds the exchange: caller root post,
        // target's threaded reply.
        let target_events = f
            .tunnel_port
            .peek_with_ids(&target_channel, &Checkpoint::default())
            .await
            .unwrap();
        let target_rows: Vec<(String, Option<String>, String)> = target_events
            .iter()
            .filter_map(|(_, ev)| match ev {
                ChannelEvent::Posted {
                    author,
                    parent,
                    text,
                    ..
                } => Some((author.clone(), parent.clone(), text.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(
            target_rows,
            vec![
                (f.caller.id.0.clone(), None, "hello local".to_string()),
                (
                    f.target.id.0.clone(),
                    Some(root_line),
                    "local reply".to_string()
                ),
            ]
        );

        // The caller's own channel mirrors the exchange for `peko log`:
        // self-authored outbound root, then the reply attributed to the
        // target's raw id with `parent` on the outbound line.
        let caller_peer = Subject::Principal(PrincipalDID(f.target_did.clone()));
        let own_channel = caller_dm_channel(&f, &caller_peer).await;
        let own_events = f
            .tunnel_port
            .peek_with_ids(&own_channel, &Checkpoint::default())
            .await
            .unwrap();
        let own_rows: Vec<(String, Option<String>, String)> = own_events
            .iter()
            .filter_map(|(_, ev)| match ev {
                ChannelEvent::Posted {
                    author,
                    parent,
                    text,
                    ..
                } => Some((author.clone(), parent.clone(), text.clone())),
                _ => None,
            })
            .collect();
        let own_root_line = own_events
            .iter()
            .find(|(_, ev)| matches!(ev, ChannelEvent::Posted { parent: None, .. }))
            .map(|(line, _)| line.clone())
            .expect("caller-side outbound root post");
        assert_eq!(
            own_rows,
            vec![
                (f.caller.id.0.clone(), None, "hello local".to_string()),
                (
                    f.target.id.0.clone(),
                    Some(own_root_line),
                    "local reply".to_string()
                ),
            ],
            "caller-side mirror: outbound root + threaded reply attributed to the target"
        );
    }

    // ------------------------------------------------------------------
    // User branch (send_peer → note via PeerMessenger port)
    // ------------------------------------------------------------------

    struct StubMessenger {
        origin: Option<Subject>,
        deliver_ok: bool,
        delivered: Mutex<Vec<(Subject, String)>>,
    }

    impl StubMessenger {
        fn new(origin: Option<Subject>, deliver_ok: bool) -> Self {
            Self {
                origin,
                deliver_ok,
                delivered: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl crate::principal::messenger::PeerMessenger for StubMessenger {
        async fn deliver_note(
            &self,
            _principal_id: &str,
            target: &Subject,
            note: &str,
            _source: peko_session::events::MessageSource,
            _caller_label: Option<&str>,
        ) -> anyhow::Result<bool> {
            self.delivered
                .lock()
                .unwrap()
                .push((target.clone(), note.to_string()));
            Ok(self.deliver_ok)
        }

        async fn originating_peer(
            &self,
            _principal_id: &str,
            _session_id: &str,
        ) -> anyhow::Result<Option<Subject>> {
            Ok(self.origin.clone())
        }
    }

    #[tokio::test]
    async fn user_branch_delivers_note_to_originating_user() {
        let stub = StubMessenger::new(Some(Subject::User("local".to_string())), true);
        let value = SendPeerTool::execute_user_target(
            &stub,
            "prin_test",
            "root:user:local",
            Some("research-helper"),
            None,
            &Subject::User("local".to_string()),
            "the backup failed",
        )
        .await;
        let result: PrincipalSendResult = serde_json::from_value(value).unwrap();
        assert!(result.success, "delivery should succeed: {result:?}");
        assert_eq!(result.kind.as_deref(), Some("user"));
        assert!(
            result.response.contains("do not"),
            "response must set no-reply expectations: {}",
            result.response
        );
        let delivered = stub.delivered.lock().unwrap();
        assert_eq!(delivered.len(), 1);
        assert_eq!(
            delivered[0].1,
            "📨 [research-helper] the backup failed",
            "note is labeled with the calling agent by default"
        );
    }

    #[tokio::test]
    async fn user_branch_rejects_cross_user_target() {
        let stub = StubMessenger::new(Some(Subject::User("local".to_string())), true);
        let value = SendPeerTool::execute_user_target(
            &stub,
            "prin_test",
            "root:user:local",
            Some("root"),
            None,
            &Subject::User("mallory".to_string()),
            "hi",
        )
        .await;
        let result: PrincipalSendResult = serde_json::from_value(value).unwrap();
        assert!(!result.success);
        assert!(
            result.error.as_deref().unwrap_or_default().contains("not permitted"),
            "cross-user send must be rejected: {result:?}"
        );
        assert!(stub.delivered.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn user_branch_errors_when_origin_unresolvable() {
        let stub = StubMessenger::new(None, true);
        let value = SendPeerTool::execute_user_target(
            &stub,
            "prin_test",
            "mystery-session",
            None,
            Some("cron"),
            &Subject::User("local".to_string()),
            "hi",
        )
        .await;
        let result: PrincipalSendResult = serde_json::from_value(value).unwrap();
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("could not resolve"));
        // The explicit label wins when delivery is attempted — not here,
        // so just assert nothing was delivered.
        assert!(stub.delivered.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn user_branch_reports_missing_conversational_session() {
        let stub = StubMessenger::new(Some(Subject::User("local".to_string())), false);
        let value = SendPeerTool::execute_user_target(
            &stub,
            "prin_test",
            "root:user:local",
            Some("root"),
            None,
            &Subject::User("local".to_string()),
            "hi",
        )
        .await;
        let result: PrincipalSendResult = serde_json::from_value(value).unwrap();
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("no conversational session"));
    }
}
