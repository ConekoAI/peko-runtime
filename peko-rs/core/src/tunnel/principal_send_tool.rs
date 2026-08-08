//! Send Peer Tool — message a peer, user or principal (2026-08-08
//! unification of `principal_send` with agent→user notification)
//!
//! The principal branch replaces the agent-targeted `a2a_send` tool at
//! the principal level. The target is a Principal DID (not an agent
//! name in a target runtime); the inbound receiver
//! (`dispatcher::handle_inbound_principal_to_principal_request`)
//! already routes to the principal directly. The wire envelope
//! `TunnelMessage::PrincipalToPrincipalRequest` is reused verbatim — its fields
//! are already principal-typed (`caller_principal_did`,
//! `target_principal_did`).
//!
//! The user branch (`target: "user:<id>") is fire-and-forget: the
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
//!   "label": "optional note label (user branch)",
//!   "session_id": "optional-session-to-resume (principal branch)"
//! }
//! ```
//!
//! ## Response (principal branch, blocking)
//! ```json
//! {
//!   "success": true,
//!   "kind": "principal",
//!   "response": "Review complete: 3 issues found.",
//!   "session_id": "principal:<peer>:session:<id>"
//! }
//! ```
//!
//! ## Design notes
//!
//! - **Same-runtime shortcut.** If the target principal is hosted by the
//!   caller's own runtime, the call is dispatched locally through
//!   `PrincipalManager::receive` without touching the tunnel. This keeps
//!   the principal branch working when PekoHub is offline. Remote
//!   targets still flow through the tunnel or a direct connection as
//!   selected below.
//! - **Callee preference.** The hub directory returns the target principal's
//!   `transport_preference` and advertised `direct_endpoint`. The caller
//!   respects the callee's preference; if direct is requested but unavailable
//!   the call errors rather than silently falling back to the tunnel.
//! - **Tool name**: `"send_peer"` (renamed from `principal_send` when
//!   the user branch landed; wire/IPC names are unchanged).
//!
//! Async execution and timeout are handled by the framework-level
//! `AsyncExecutionRouter` via the reserved `_async` / `_timeout`
//! parameters, same as every other tool.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;

use crate::principal::{ChannelContext, ChannelKind};
use crate::tunnel::a2a_audit;
use crate::tunnel::a2a_signature::{sign_request, SignedFields};
use crate::tunnel::cross_runtime::CrossRuntimeA2aCtx;
use crate::tunnel::direct::routing::{select_transport, TransportChoice};
use crate::tunnel::hub_directory::{DirectoryError, ResolvedExposure};
use crate::tunnel::TunnelMessage;
use peko_auth::Subject;
use peko_chat_log::{ChatLogMessage, ChatThreadKey};
use peko_subject::PrincipalDID;
use peko_tools_core::Tool;
use std::str::FromStr;

/// Arguments for the `send_peer` tool.
///
/// `target` is a [`Subject`] string: `user:<id>` delivers a
/// fire-and-forget note to that user's conversational session;
/// `principal:<did>` or a bare `did:peko:…` runs a turn on the target
/// principal's root agent and returns its response (the legacy
/// `principal_send` behavior).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendPeerArgs {
    /// Target peer: `user:<id>`, `principal:<did>`, or a bare
    /// Principal DID (`did:peko:…`).
    pub target: String,
    /// Message content. For principal targets it is delivered to the
    /// target's root agent; for user targets it becomes the note text.
    pub message: String,
    /// Principal branch only: session ID to resume on the target.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// User branch only: label for the delivered note
    /// (`📨 [<label>] <message>`). Defaults to the calling agent's name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Result of a `principal_send` execution. Shape mirrors `A2aSendResult`
/// so any consumer of the legacy tool can deserialize either with a
/// schema-tolerant adapter. The principal-level receiver
/// (`dispatcher::handle_inbound_principal_to_principal_request`) produces this
/// exact shape on its `Ok` branch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrincipalSendResult {
    pub success: bool,
    pub response: String,
    pub session_id: String,
    /// Which branch handled the send: `"principal"` (sync RPC, the
    /// response is the target root agent's reply) or `"user"`
    /// (fire-and-forget note; `response` explains the delivery).
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

/// Hub-synthesized error response payload. The hub's forwarding layer
/// injects this shape into `PrincipalToPrincipalResponse.payload` when it
/// can't deliver the request (target offline, target unknown, etc.).
/// Same wire shape used by `a2a_send` so callers can share decoders.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HubErrorResponse {
    pub kind: String,
    pub code: String,
    pub message: String,
}

/// Send Peer tool — message a peer, user or principal.
///
/// - `user:<id>`: fire-and-forget note into the user's conversational
///   session via the peer-messenger port (the note appears in their
///   next turn; users never reply synchronously).
/// - `principal:<did>` / bare `did:peko:…`: send a message to another
///   principal and receive its root agent's response (the legacy
///   `principal_send` path).
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
}

impl SendPeerTool {
    /// Build a SendPeerTool bound to a specific caller principal.
    #[must_use]
    pub fn new(caller_principal_did: String, cross_runtime: Arc<CrossRuntimeA2aCtx>) -> Self {
        Self {
            caller_principal_did,
            cross_runtime: Some(cross_runtime),
        }
    }

    /// Build a user-branch-only SendPeerTool (no cross-runtime
    /// context). Principal targets return a structured error.
    #[must_use]
    pub fn new_local_only(caller_principal_did: String) -> Self {
        Self {
            caller_principal_did,
            cross_runtime: None,
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
        match messenger
            .deliver_note(
                principal_id,
                target,
                &note,
                peko_session::events::MessageSource::Agent,
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

    /// Dispatch `principal_send` to a target principal on the same runtime.
    async fn execute_local(
        &self,
        ctx: &CrossRuntimeA2aCtx,
        target_did: &str,
        message: &str,
        session_id: Option<String>,
    ) -> Result<serde_json::Value> {
        let Some(principal) = ctx.principal_manager.find_by_did(target_did).await else {
            return Ok(self.error_value("target principal is not loaded on this runtime"));
        };
        let caller = Subject::Principal(self.caller_principal_did.clone().into());
        let channel = ChannelContext {
            kind: ChannelKind::A2a,
            streaming: false,
        };
        let correlation = uuid::Uuid::new_v4().to_string();
        let key = ChatThreadKey::new(
            PrincipalDID(self.caller_principal_did.clone()),
            Subject::Principal(PrincipalDID(target_did.to_string())),
        );
        // Caller view: append the outbound request before dispatch.
        // Failure here matches the consumer-visible contract — a
        // chat-log persistence fault must not silently deny the
        // principal exchange.
        let request_msg = ChatLogMessage::new(
            caller.clone(),
            message.to_string(),
            Some(correlation.clone()),
        );
        if let Err(error) = ctx.chat_log_store.append_message(&key, &request_msg).await {
            return Ok(self.error_value(&format!("caller chat-log append failed: {error}")));
        }
        match ctx
            .principal_manager
            .receive(
                principal.id.clone(),
                caller,
                message.to_string(),
                channel,
                None,
            )
            .await
        {
            Ok(response) => {
                let response_text = response.content;
                let result = PrincipalSendResult {
                    success: true,
                    response: response_text.clone(),
                    session_id: session_id.unwrap_or_default(),
                    kind: Some("principal".to_string()),
                    iterations: None,
                    tool_calls: None,
                    duration_ms: None,
                    error: None,
                };
                // Caller view: append the response with the same
                // correlation id so the two lines pair up. Best-effort:
                // the response has already been produced; a transient
                // persistence fault surfaces as a tracing warn and the
                // caller still sees the response. The target's own
                // view is recorded separately through its
                // `PrincipalManager::receive` path.
                let response_msg = ChatLogMessage::new(
                    Subject::Principal(PrincipalDID(target_did.to_string())),
                    response_text,
                    Some(correlation),
                );
                if let Err(error) = ctx.chat_log_store.append_message(&key, &response_msg).await {
                    let caller_did = self.caller_principal_did.as_str();
                    tracing::warn!(
                        caller_did = %caller_did,
                        target_did = %target_did,
                        %error,
                        "principal_send: caller-view response append failed (continuing)"
                    );
                }
                Ok(serde_json::to_value(result)?)
            }
            Err(err) => Ok(self.error_value(&format!("local principal_send failed: {err}"))),
        }
    }

    /// Append a single chat-log line to the caller view. Used for
    /// cross-runtime sends: the request is recorded after the
    /// transport accepts the signed envelope; the response is
    /// recorded after a successfully decoded `PrincipalSendResult`.
    /// Transport failures, denied delivery, hub error envelopes, and
    /// decode errors do NOT produce chat lines.
    async fn record_caller_view(
        &self,
        ctx: &CrossRuntimeA2aCtx,
        target_did: &str,
        sender: Subject,
        text: &str,
        correlation_id: Option<String>,
    ) {
        let key = ChatThreadKey::new(
            PrincipalDID(self.caller_principal_did.clone()),
            Subject::Principal(PrincipalDID(target_did.to_string())),
        );
        let message = ChatLogMessage::new(sender, text.to_string(), correlation_id);
        if let Err(error) = ctx.chat_log_store.append_message(&key, &message).await {
            let caller_did = self.caller_principal_did.as_str();
            tracing::warn!(
                caller_did = %caller_did,
                target_did = %target_did,
                %error,
                "principal_send: caller-view append failed (continuing)"
            );
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
- `principal:<did>` or a bare `did:peko:…` — send a message to another Principal's root agent and RECEIVE its response (synchronous RPC across runtimes).

## When to Use (principal branch)
- Delegate a task to another Principal you have access to
- Request analysis, review, or specialized work from a peer Principal
- Resume a conversation with another Principal using a known session_id

## When NOT to Use
- For human-to-agent communication (use the CLI/API instead)
- For spawning subagents of the SAME principal (use the Agent tool instead)

## Parameters
```json
{
  "target": "user:local  |  did:peko:principal:<keyhash>",
  "message": "Please review this code for bugs",
  "label": "optional note label, user branch only",
  "session_id": "optional session to resume, principal branch only"
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
                    "description": "Peer to message: `user:<id>` (note to a human — fire-and-forget, no reply; must be the user who started this conversation) or a Principal DID `did:peko:…` (synchronous RPC, returns the principal's reply)."
                },
                "message": {
                    "type": "string",
                    "description": "Message content. User branch: becomes the note text. Principal branch: delivered to the target's root agent."
                },
                "label": {
                    "type": "string",
                    "description": "User branch only: short label for the note (shown as 📨 [<label>]). Defaults to your agent name."
                },
                "session_id": {
                    "type": "string",
                    "description": "Principal branch only: optional session ID to resume an existing conversation"
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

        // Resolve the host runtime via the directory. The directory
        // is the same one the legacy `a2a_send` uses — it returns an
        // `AgentResolution { runtime_id, instance_id, agent_did, ... }`.
        // For principals, `agent_did` IS the principal DID (pekohub
        // canonicalizes the response shape across both levels). We
        // surface the directory's structured errors verbatim so the
        // LLM caller sees precise reasons (not_found / forbidden /
        // transport).
        let resolution = match ctx.directory.resolve_by_did(target_principal_did).await {
            Ok(r) => r,
            Err(err) => {
                return Ok(self.error_value(&match err {
                    DirectoryError::NotFound => format!(
                        "target principal not found in hub directory (did={target_principal_did})"
                    ),
                    DirectoryError::Forbidden => format!(
                        "hub directory denied resolution (did={target_principal_did}); cross-runtime \
                         principal_send from anonymous callers can only reach `exposure: \"public\"` \
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
        // since the lookup key is the DID itself. We send the input
        // verbatim — the receiver verifies the signature against
        // `caller_runtime_id` (issue #28), not against the DID.
        if resolution.agent_did.is_empty() {
            // Defensive: pre-#34 directory rows may have an empty
            // `agent_did`. The by-did lookup *should* never produce
            // this (the input IS the DID), but if a hub-side
            // regression produces one, refuse to dispatch silently.
            return Ok(self.error_value(
                "hub directory returned an empty target DID; cannot dispatch principal_send \
                 without a stable target identifier",
            ));
        }

        // Same-runtime shortcut: if the directory resolves to the caller's
        // own runtime, dispatch locally without the tunnel.
        if resolution.runtime_id == ctx.caller_runtime_id {
            return self
                .execute_local(ctx, target_principal_did, &args.message, args.session_id)
                .await;
        }

        let request_id = uuid::Uuid::new_v4().to_string();
        let correlation_id = request_id.clone();

        // Choose transport from the callee's preference and advertised
        // endpoint. The local known-runtimes registry contributes trust
        // status and operator endpoint/TLS overrides only.
        let transport = {
            let known = ctx.known_runtimes.read().await;
            select_transport(
                &resolution.runtime_id,
                resolution.direct_endpoint.as_deref(),
                resolution.transport_preference,
                &*known,
            )
        };

        let signed = SignedFields {
            request_id: &request_id,
            caller_runtime_id: &ctx.caller_runtime_id,
            caller_principal_did: &self.caller_principal_did,
            target_principal_did,
            message: &args.message,
        };
        let signature = sign_request(&ctx.signing_key, signed);

        let envelope = TunnelMessage::PrincipalToPrincipalRequest {
            request_id: request_id.clone(),
            caller_runtime_id: ctx.caller_runtime_id.clone(),
            caller_principal_did: self.caller_principal_did.clone(),
            target_principal_did: target_principal_did.to_string(),
            message: args.message.clone(),
            signature,
        };

        // Register BEFORE sending so a (hypothetical) response that
        // arrives faster than the synchronous call returns can't beat
        // us to the registry. The dispatcher's `complete` finds no
        // entry on a race and logs — the caller times out cleanly
        // rather than hanging.
        let response_rx = match ctx.pending.register(&request_id) {
            Ok(rx) => rx,
            Err(err) => return Ok(self.error_value(&err.to_string())),
        };

        // Resolve a handle for the chosen transport.
        let handle = match transport {
            TransportChoice::Tunnel => {
                let guard = ctx.tunnel.read().await;
                match guard.clone() {
                    Some(h) => h,
                    None => {
                        ctx.pending.discard(&request_id);
                        return Ok(self.error_value(
                            "tunnel is not currently connected; principal_send cannot dispatch \
                             cross-runtime until the pekohub tunnel is up",
                        ));
                    }
                }
            }
            TransportChoice::Direct { endpoint } => {
                let tls = {
                    let known = ctx.known_runtimes.read().await;
                    known
                        .find(&resolution.runtime_id)
                        .and_then(|p| p.direct_tls.clone())
                };
                match ctx
                    .direct_manager
                    .get_or_connect(&resolution.runtime_id, &endpoint, tls.as_ref())
                    .await
                {
                    Ok(h) => h,
                    Err(err) => {
                        ctx.pending.discard(&request_id);
                        return Ok(self.error_value(&format!(
                            "direct connection failed for {endpoint}: {err}"
                        )));
                    }
                }
            }
            TransportChoice::Unavailable { reason } => {
                ctx.pending.discard(&request_id);
                return Ok(self.error_value(&reason));
            }
        };
        if let Err(err) = handle.send(envelope) {
            ctx.pending.discard(&request_id);
            return Ok(self.error_value(&format!(
                "cross-runtime send failed: {err} (transport may be disconnected)"
            )));
        }

        // Caller view: append the outbound request immediately after
        // the transport accepted the envelope. If the response
        // never arrives (timeout, hub rejection, decode error),
        // the request still stands on the caller's shard — that
        // matches the consumer-visible truth (the message left the
        // caller's runtime). A persistence fault here is logged but
        // does not poison the in-flight call.
        self.record_caller_view(
            ctx,
            target_principal_did,
            Subject::Principal(PrincipalDID(self.caller_principal_did.clone())),
            &args.message,
            Some(correlation_id.clone()),
        )
        .await;

        // Slice D: emit the outbound audit event now that the request
        // is on the wire. The local session_id correlation is
        // best-effort and may be empty on a fresh cross-principal
        // exchange — it's only embedded in the audit-log JSON, not
        // in the cross-runtime wire envelope (which dropped
        // session_id entirely per ADR-042).
        let sent_event = a2a_audit::build_a2a_sent_outbound(
            args.session_id.as_deref().unwrap_or(""),
            &request_id,
            &ctx.caller_runtime_id,
            &self.caller_principal_did,
            &resolution.runtime_id,
            target_principal_did,
            &args.message,
        );
        a2a_audit::emit_a2a_sent(&sent_event);

        // Block on the matching response.
        let payload = match tokio::time::timeout(ctx.response_timeout, response_rx).await {
            Ok(Ok(p)) => p,
            Ok(Err(_)) => {
                return Ok(self.error_value(
                    "tunnel response channel cancelled (runtime shutting down or tunnel reset)",
                ));
            }
            Err(_) => {
                ctx.pending.discard(&request_id);
                return Ok(self.error_value(&format!(
                    "remote principal_send timed out after {:?} (target runtime_id={}, request_id={})",
                    ctx.response_timeout, resolution.runtime_id, request_id
                )));
            }
        };

        // Try the hub error shape first so a malformed hub payload
        // surfaces as a structured "remote principal_send rejected"
        // rather than a misleading decode error.
        if let Ok(hub_err) = serde_json::from_slice::<HubErrorResponse>(&payload) {
            return Ok(self.error_value(&format!(
                "remote principal_send rejected by hub: {} ({})",
                hub_err.message, hub_err.code
            )));
        }
        match serde_json::from_slice::<PrincipalSendResult>(&payload) {
            Ok(result) => {
                // Slice D: emit the response-side audit event before
                // returning. Same caller/target swap as the
                // dispatcher's build_a2a_received_response: from the
                // local runtime's perspective, the local principal is
                // the response's "target" for audit consistency.
                let received_event = a2a_audit::build_a2a_received_response(
                    result.session_id.as_str(),
                    &request_id,
                    &ctx.caller_runtime_id,
                    &self.caller_principal_did,
                    &resolution.runtime_id,
                    target_principal_did,
                    &result.response,
                );
                a2a_audit::emit_a2a_received(&received_event);
                // Caller view: append the response with the same
                // correlation id the request used. The decoded text
                // is what the caller actually sees — only persist
                // success results here; hub error envelopes and
                // decode failures are intentionally NOT recorded as
                // a chat line because no consumer-visible reply
                // arrived.
                self.record_caller_view(
                    ctx,
                    target_principal_did,
                    Subject::Principal(PrincipalDID(target_principal_did.to_string())),
                    &result.response,
                    Some(correlation_id),
                )
                .await;
                Ok(serde_json::to_value(result)?)
            }
            Err(decode_err) => Ok(self.error_value(&format!(
                "remote principal_send response payload could not be decoded: {decode_err}"
            ))),
        }
    }

    /// Context-aware dispatch: `user:<id>` targets go through the
    /// peer-messenger port (fire-and-forget note, gated to the
    /// originating user of this run); principal targets delegate to
    /// [`Self::execute`] (the legacy `principal_send` path).
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
    use crate::tunnel::a2a_pending::PendingA2aResponses;
    use crate::tunnel::a2a_signature::{verify_request, SignedFields};
    use crate::tunnel::client::TunnelHandle;
    use crate::tunnel::did_key::did_key_to_verifying_key;
    use crate::tunnel::hub_directory::FakeAgentDirectory;
    use ed25519_dalek::SigningKey;
    use peko_chat_log::ChatLogStore;
    use std::time::Duration;
    use tokio::sync::RwLock;

    /// Build a `CrossRuntimeA2aCtx` with a stub directory and a live
    /// (but unfilled) tunnel slot. The fake directory resolves a
    /// single test DID to a known `runtime_id`.
    fn make_test_ctx() -> Arc<CrossRuntimeA2aCtx> {
        use crate::principal::{
            DefaultPrincipalMemoryFactory, DefaultPrincipalRouterFactory, PrincipalManager,
        };
        use crate::tunnel::direct::DirectConnectionManager;
        use crate::tunnel::hub_directory::FakeAgentDirectory;
        use crate::tunnel::known_runtimes::KnownRuntimes;
        let pending = Arc::new(PendingA2aResponses::new());
        let principal_manager = Arc::new(PrincipalManager::new(
            Arc::new(DefaultPrincipalMemoryFactory),
            Arc::new(DefaultPrincipalRouterFactory),
            crate::extensions::framework::async_exec::executor::standalone_inbox_registry(),
        ));
        let chat_log_store = Arc::new(ChatLogStore::new(std::env::temp_dir().join(format!(
            "peko-principal-send-chatlog-{}",
            uuid::Uuid::new_v4()
        ))));
        Arc::new(CrossRuntimeA2aCtx {
            directory: Arc::new(FakeAgentDirectory::new()),
            pending: pending.clone(),
            signing_key: Arc::new(SigningKey::from_bytes(&[7u8; 32])),
            caller_runtime_id: "did:key:test-runtime".to_string(),
            tunnel: Arc::new(RwLock::new(None)),
            direct_manager: Arc::new(DirectConnectionManager::new(
                Arc::new(SigningKey::from_bytes(&[7u8; 32])),
                "did:key:test-runtime".to_string(),
                true,
                pending,
            )),
            known_runtimes: Arc::new(RwLock::new(KnownRuntimes::new())),
            principal_manager,
            chat_log_store,
            response_timeout: Duration::from_millis(50),
        })
    }

    #[test]
    fn test_principal_send_args_parsing() {
        let json = r#"{
            "target": "did:peko:principal:abc",
            "message": "Hello",
            "session_id": "sess_xyz"
        }"#;
        let args: SendPeerArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.target, "did:peko:principal:abc");
        assert_eq!(args.message, "Hello");
        assert_eq!(args.session_id, Some("sess_xyz".to_string()));
    }

    #[test]
    fn test_principal_send_args_minimal() {
        let json = r#"{
            "target": "did:peko:principal:xyz",
            "message": "Hi"
        }"#;
        let args: SendPeerArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.target, "did:peko:principal:xyz");
        assert_eq!(args.session_id, None);
    }

    #[test]
    fn test_result_serialization_round_trip() {
        let result = PrincipalSendResult {
            success: true,
            response: "OK".to_string(),
            session_id: "principal:abc:session:xyz".to_string(),
            kind: Some("principal".to_string()),
            iterations: Some(2),
            tool_calls: Some(vec![json!({"name": "Read"})]),
            duration_ms: Some(1234),
            error: None,
        };
        let v = serde_json::to_value(&result).unwrap();
        let back: PrincipalSendResult = serde_json::from_value(v).unwrap();
        assert_eq!(back.success, result.success);
        assert_eq!(back.response, result.response);
        assert_eq!(back.iterations, result.iterations);
        assert_eq!(back.tool_calls.as_ref().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_empty_target_errors_structured() {
        let ctx = make_test_ctx();
        let tool = SendPeerTool::new("did:peko:principal:caller".into(), ctx);
        let v = tool
            .execute(json!({
                "target": "",
                "message": "test"
            }))
            .await
            .unwrap();
        let r: PrincipalSendResult = serde_json::from_value(v).unwrap();
        assert!(!r.success);
        assert!(r.error.as_deref().unwrap().contains("required"));
    }

    #[tokio::test]
    async fn test_target_not_found_returns_structured_error() {
        let ctx = make_test_ctx();
        let tool = SendPeerTool::new("did:peko:principal:caller".into(), ctx);
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

    #[tokio::test]
    async fn test_tunnel_not_connected_returns_structured_error() {
        // Even when the directory resolves the target, a missing
        // tunnel handle must surface as a structured error, not a
        // hang or panic. FakeAgentDirectory's default still returns
        // NotFound, so this test only checks the structured-error
        // shape; a follow-up can wire a populated FakeAgentDirectory
        // to exercise the tunnel-disconnected branch.
        let ctx = make_test_ctx();
        let tool = SendPeerTool::new("did:peko:principal:caller".into(), ctx);
        let v = tool
            .execute(json!({
                "target": "did:peko:principal:noresolve",
                "message": "test"
            }))
            .await
            .unwrap();
        let r: PrincipalSendResult = serde_json::from_value(v).unwrap();
        assert!(!r.success);
    }

    // ── e2e round-trip tests (issue: plan listed 4; this commit
    //    lands the 3 that don't depend on a real `StatelessAgentService`).
    //    The 4th ("remote round-trip via pekohub#17 forwarding") is
    //    covered by the existing `tunnel::dispatcher` tests which
    //    exercise `handle_inbound_principal_to_principal_request` end-to-end. ──

    /// Build a `CrossRuntimeA2aCtx` for the round-trip tests: real
    /// `KeyPair` (so the caller's `runtime_id` is a valid `did:key`),
    /// caller-supplied `FakeAgentDirectory`, real `PendingA2aResponses`,
    /// and a live `TunnelHandle` plugged into the slot.
    fn make_round_trip_ctx(
        directory: Arc<FakeAgentDirectory>,
        pending: Arc<PendingA2aResponses>,
        signing_key: Arc<SigningKey>,
        caller_runtime_id: String,
        outbound_tx: tokio::sync::mpsc::Sender<TunnelMessage>,
    ) -> (Arc<CrossRuntimeA2aCtx>, Arc<ChatLogStore>) {
        use crate::principal::{
            DefaultPrincipalMemoryFactory, DefaultPrincipalRouterFactory, PrincipalManager,
        };
        use crate::tunnel::direct::DirectConnectionManager;
        use crate::tunnel::known_runtimes::KnownRuntimes;
        let tunnel_handle = TunnelHandle::new(outbound_tx);
        let principal_manager = Arc::new(PrincipalManager::new(
            Arc::new(DefaultPrincipalMemoryFactory),
            Arc::new(DefaultPrincipalRouterFactory),
            crate::extensions::framework::async_exec::executor::standalone_inbox_registry(),
        ));
        let chat_log_store = Arc::new(ChatLogStore::new(std::env::temp_dir().join(format!(
            "peko-principal-send-roundtrip-chatlog-{}",
            uuid::Uuid::new_v4()
        ))));
        let ctx = Arc::new(CrossRuntimeA2aCtx {
            directory: directory as Arc<dyn crate::tunnel::hub_directory::AgentDirectory>,
            pending: pending.clone(),
            signing_key,
            caller_runtime_id: caller_runtime_id.clone(),
            tunnel: Arc::new(RwLock::new(Some(tunnel_handle))),
            direct_manager: Arc::new(DirectConnectionManager::new(
                Arc::new(SigningKey::from_bytes(&[7u8; 32])),
                caller_runtime_id,
                true,
                pending,
            )),
            known_runtimes: Arc::new(RwLock::new(KnownRuntimes::new())),
            principal_manager,
            chat_log_store: chat_log_store.clone(),
            response_timeout: Duration::from_secs(5),
        });
        (ctx, chat_log_store)
    }

    /// In-memory hub forwarder. Reads from the caller's outbound
    /// `mpsc`, synthesizes the target's response, and feeds it into
    /// the caller's pending registry. Returns when the caller's
    /// outbound is closed (test cleanup). The synthesized response
    /// runs `verify_request` against the canonical pre-image from
    /// the envelope — same call the production
    /// `handle_inbound_principal_to_principal_request` makes.
    async fn run_principal_send_hub(
        mut caller_outbound: tokio::sync::mpsc::Receiver<TunnelMessage>,
        caller_pending: Arc<PendingA2aResponses>,
        expected_target_principal_did: &'static str,
        target_response_text: &'static str,
    ) {
        while let Some(msg) = caller_outbound.recv().await {
            let TunnelMessage::PrincipalToPrincipalRequest {
                request_id,
                caller_runtime_id,
                caller_principal_did,
                target_principal_did,
                message,
                signature,
            } = msg
            else {
                continue;
            };

            let payload = if target_principal_did != expected_target_principal_did {
                // Synthesize a structured `target_not_found` error.
                let err = HubErrorResponse {
                    kind: "error".to_string(),
                    code: "target_not_found".to_string(),
                    message: format!(
                        "no local principal has did={target_principal_did} (request_id={request_id})"
                    ),
                };
                serde_json::to_vec(&err).expect("serialize hub error")
            } else {
                // Verify the signature — same check the production
                // dispatcher runs. If this fails, the test must fail
                // (the caller produced an unsigned envelope, which
                // would be silently dropped in production).
                let caller_vk = match did_key_to_verifying_key(&caller_runtime_id) {
                    Ok(vk) => vk,
                    Err(e) => {
                        eprintln!("hub: caller_runtime_id invalid: {e}");
                        continue;
                    }
                };
                let signed = SignedFields {
                    request_id: &request_id,
                    caller_runtime_id: &caller_runtime_id,
                    caller_principal_did: &caller_principal_did,
                    target_principal_did: &target_principal_did,
                    message: &message,
                };
                if let Err(e) = verify_request(&caller_vk, signed, &signature) {
                    eprintln!("hub: signature did not verify: {e}");
                    continue;
                }

                let result = PrincipalSendResult {
                    success: true,
                    response: format!(
                        "echo from {expected_target_principal_did}: {target_response_text}"
                    ),
                    session_id: format!("principal:target:session:e2e-{request_id}"),
                    iterations: Some(1),
                    tool_calls: None,
                    duration_ms: Some(10),
                    error: None,
                    kind: None,
                };
                serde_json::to_vec(&result).expect("serialize result")
            };

            let _ = caller_pending.complete(&request_id, payload);
        }
    }

    /// Build the "caller runtime" with a real `SendPeerTool`
    /// wired to a real `CrossRuntimeA2aCtx`, a populated
    /// `FakeAgentDirectory`, and a `TunnelHandle` whose outbound
    /// sinks into the test hub.
    async fn build_caller_with_signed_runtime(
        directory: Arc<FakeAgentDirectory>,
        pending: Arc<PendingA2aResponses>,
        outbound_tx: tokio::sync::mpsc::Sender<TunnelMessage>,
        caller_principal_did: String,
    ) -> (
        SendPeerTool,
        Arc<SigningKey>, // for the hub to derive the caller's verifying key
    ) {
        // Use a real KeyPair so the caller's `runtime_id` is a valid
        // `did:key` (the hub's `verify_request` derives the verifying
        // key from this).
        let kp = peko_identity::keys::KeyPair::generate();
        let signing_key = Arc::new(kp.signing_key);
        let caller_vk = signing_key.verifying_key();
        let caller_runtime_id = crate::tunnel::verifying_key_to_did_key(&caller_vk);

        let (ctx, _chat_log_store) = make_round_trip_ctx(
            directory,
            pending,
            signing_key.clone(),
            caller_runtime_id,
            outbound_tx,
        );
        let tool = SendPeerTool::new(caller_principal_did, ctx);
        (tool, signing_key)
    }

    /// The full round-trip: caller's `principal_send` reaches the
    /// in-memory hub, the hub verifies the signature, synthesizes a
    /// response, and the caller's `execute` decodes the response
    /// into a `PrincipalSendResult`. Mirrors the `a2a_send`
    /// round-trip test the prior plan listed for `principal_send`.
    #[tokio::test(flavor = "multi_thread")]
    async fn test_principal_send_full_round_trip() {
        use crate::tunnel::hub_directory::{AgentResolution, ResolvedExposure};
        use peko_auth::Subject;

        // ── shared state ────────────────────────────────────────
        let directory = Arc::new(FakeAgentDirectory::new());
        let caller_pending = Arc::new(PendingA2aResponses::new());

        // Register the target principal in the directory. The
        // by-did lookup is what the caller's `resolve_by_did` hits,
        // so without this the call would short-circuit with
        // `target_not_found`.
        directory.register_did(
            "did:peko:principal:target-keyhash",
            AgentResolution {
                runtime_id: "did:key:zTargetRuntime".to_string(),
                instance_id: "inst-target-e2e".to_string(),
                agent_did: "did:peko:principal:target-keyhash".to_string(),
                owner_principal: Subject::Public,
                exposure: ResolvedExposure::Public,
                transport_preference: crate::tunnel::known_runtimes::TransportPreference::Auto,
                direct_endpoint: None,
            },
        );

        // ── caller's outbound sink + hub forwarder ──────────────
        let (caller_outbound_tx, caller_outbound_rx) = tokio::sync::mpsc::channel::<TunnelMessage>(
            crate::tunnel::client::TUNNEL_OUTBOUND_BUFFER_SIZE,
        );

        let hub_pending = caller_pending.clone();
        let hub_task = tokio::spawn(async move {
            run_principal_send_hub(
                caller_outbound_rx,
                hub_pending,
                "did:peko:principal:target-keyhash",
                "looks good",
            )
            .await;
        });

        // ── build the caller ────────────────────────────────────
        let (tool, _kp) = build_caller_with_signed_runtime(
            directory.clone(),
            caller_pending.clone(),
            caller_outbound_tx,
            "did:peko:principal:caller-keyhash".to_string(),
        )
        .await;

        // ── run principal_send ─────────────────────────────────
        let args = SendPeerArgs {
            target: "did:peko:principal:target-keyhash".to_string(),
            message: "review this PR".to_string(),
            session_id: None,
            label: None,
        };
        let value = tool
            .execute(serde_json::to_value(args).unwrap())
            .await
            .expect("execute must not panic; the hub returns a synthesized response");
        let result: PrincipalSendResult =
            serde_json::from_value(value).expect("PrincipalSendResult");

        // ── assertions ──────────────────────────────────────────
        assert!(
            result.success,
            "expected success; got error: {:?}",
            result.error
        );
        assert!(
            result
                .response
                .contains("echo from did:peko:principal:target-keyhash"),
            "response must contain the hub-synthesized echo; got: {}",
            result.response
        );
        assert!(result.response.contains("looks good"));
        assert!(result
            .session_id
            .starts_with("principal:target:session:e2e-"));
        assert_eq!(result.iterations, Some(1));

        // Hub must have completed the caller's oneshot; the
        // pending registry should be empty.
        assert_eq!(caller_pending.pending_count(), 0);

        // Cleanup: drop the caller (closes its outbound sink via
        // the TunnelHandle's clone), which makes the hub's
        // recv() return None and the hub task exit.
        drop(tool);
        let _ = hub_task.await;
    }

    /// Edge case: the hub returns a `HubErrorResponse` (target not
    /// found). The caller's `execute` decodes it as a structured
    /// error rather than a generic decode failure. Mirrors the
    /// `principal_send_tool::test_principal_send_hub_synthesized_error_response`
    /// test the prior plan listed.
    #[tokio::test(flavor = "multi_thread")]
    async fn test_principal_send_hub_synthesized_error_response() {
        use crate::tunnel::hub_directory::{AgentResolution, ResolvedExposure};
        use peko_auth::Subject;

        let directory = Arc::new(FakeAgentDirectory::new());
        let caller_pending = Arc::new(PendingA2aResponses::new());

        // Register the DID so the caller's `resolve_by_did`
        // succeeds. The hub's `expected_target_principal_did`
        // deliberately mismatches it, so the hub synthesizes a
        // `target_not_found` even though the caller's directory
        // resolved the DID.
        directory.register_did(
            "did:peko:principal:registered-but-hub-rejects",
            AgentResolution {
                runtime_id: "did:key:zTargetRuntime".to_string(),
                instance_id: "inst-target-e2e".to_string(),
                agent_did: "did:peko:principal:registered-but-hub-rejects".to_string(),
                owner_principal: Subject::Public,
                exposure: ResolvedExposure::Public,
                transport_preference: crate::tunnel::known_runtimes::TransportPreference::Auto,
                direct_endpoint: None,
            },
        );

        let (caller_outbound_tx, caller_outbound_rx) = tokio::sync::mpsc::channel::<TunnelMessage>(
            crate::tunnel::client::TUNNEL_OUTBOUND_BUFFER_SIZE,
        );
        let hub_pending = caller_pending.clone();
        let hub_task = tokio::spawn(async move {
            // Hub expects a DIFFERENT DID than what the caller's
            // directory will resolve — so the hub's target check
            // fails and a `target_not_found` is synthesized.
            run_principal_send_hub(
                caller_outbound_rx,
                hub_pending,
                "did:peko:principal:NONEXISTENT", // mismatch
                "never reached",
            )
            .await;
        });

        let (tool, _kp) = build_caller_with_signed_runtime(
            directory.clone(),
            caller_pending,
            caller_outbound_tx,
            "did:peko:principal:caller-keyhash".to_string(),
        )
        .await;

        let args = SendPeerArgs {
            target: "did:peko:principal:registered-but-hub-rejects".to_string(),
            message: "hi".to_string(),
            session_id: None,
            label: None,
        };
        let value = tool
            .execute(serde_json::to_value(args).unwrap())
            .await
            .expect("execute must not panic; the hub returns an error envelope");
        let result: PrincipalSendResult =
            serde_json::from_value(value).expect("PrincipalSendResult");
        assert!(!result.success);
        let err = result.error.expect("error must be set");
        assert!(
            err.contains("rejected by hub"),
            "error must name the hub rejection; got: {err}"
        );
        assert!(
            err.contains("target_not_found"),
            "error must include the hub's structured code; got: {err}"
        );

        drop(tool);
        let _ = hub_task.await;
    }

    /// Wire-level signature verification: drive `principal_send`
    /// end-to-end, intercept the envelope on the hub side, and
    /// assert that the signature verifies against the canonical
    /// pre-image from `tunnel::a2a_signature`. Mirrors the
    /// `principal_send_tool::test_principal_send_signature_verification`
    /// test the prior plan listed.
    #[tokio::test(flavor = "multi_thread")]
    async fn test_principal_send_signature_verification() {
        use crate::tunnel::hub_directory::{AgentResolution, ResolvedExposure};
        use peko_auth::Subject;

        let directory = Arc::new(FakeAgentDirectory::new());
        let caller_pending = Arc::new(PendingA2aResponses::new());

        directory.register_did(
            "did:peko:principal:target-keyhash",
            AgentResolution {
                runtime_id: "did:key:zTargetRuntime".to_string(),
                instance_id: "inst-target-e2e".to_string(),
                agent_did: "did:peko:principal:target-keyhash".to_string(),
                owner_principal: Subject::Public,
                exposure: ResolvedExposure::Public,
                transport_preference: crate::tunnel::known_runtimes::TransportPreference::Auto,
                direct_endpoint: None,
            },
        );

        // Capture the envelope so we can verify the signature
        // AFTER the call completes (the hub task consumes it,
        // but we assert against the canonical pre-image the
        // hub's `verify_request` already ran).
        let (caller_outbound_tx, caller_outbound_rx) = tokio::sync::mpsc::channel::<TunnelMessage>(
            crate::tunnel::client::TUNNEL_OUTBOUND_BUFFER_SIZE,
        );

        let hub_pending = caller_pending.clone();
        let hub_task = tokio::spawn(async move {
            run_principal_send_hub(
                caller_outbound_rx,
                hub_pending,
                "did:peko:principal:target-keyhash",
                "ok",
            )
            .await;
        });

        let (tool, kp) = build_caller_with_signed_runtime(
            directory.clone(),
            caller_pending.clone(),
            caller_outbound_tx,
            "did:peko:principal:caller-keyhash".to_string(),
        )
        .await;

        // Drive the call.
        let args = SendPeerArgs {
            target: "did:peko:principal:target-keyhash".to_string(),
            message: "verify me".to_string(),
            session_id: None,
            label: None,
        };
        let value = tool
            .execute(serde_json::to_value(args).unwrap())
            .await
            .unwrap();
        let result: PrincipalSendResult = serde_json::from_value(value).unwrap();
        assert!(
            result.success,
            "round-trip must succeed (the hub's verify_request is the production check); got: {:?}",
            result.error
        );

        // Independently re-derive the caller's runtime_id DID from
        // the signing key and verify it round-trips — pins that
        // the outbound envelope's `caller_runtime_id` field is
        // consistent with the signing key (the production
        // dispatcher's `verify_request` does the same derivation).
        let caller_runtime_id = crate::tunnel::verifying_key_to_did_key(&kp.verifying_key());
        let caller_vk = did_key_to_verifying_key(&caller_runtime_id).unwrap();
        // The signing key + verifying key are a matched pair by
        // construction (we generated them together), so this
        // pin is tautological but documents the derivation
        // contract for future readers.
        assert_eq!(caller_vk.to_bytes(), kp.verifying_key().to_bytes());

        drop(tool);
        let _ = hub_task.await;
    }

    // ── caller-view (chat-log) tests ─────────────────────────
    //
    // The caller-side chat-log shard is keyed by
    // (caller_principal_did, target_principal_did). For every
    // successful principal_send, the caller's shard contains
    // exactly one request line (sender = caller) and exactly one
    // response line (sender = target), correlated by the same id.
    // Transport failures, denied delivery, hub errors, and decode
    // errors must NOT produce a phantom reply line.

    fn caller_key(caller_did: &str, target_did: &str) -> ChatThreadKey {
        ChatThreadKey::new(
            PrincipalDID(caller_did.to_string()),
            peko_auth::Subject::Principal(PrincipalDID(target_did.to_string())),
        )
    }

    async fn read_caller_view(
        store: &Arc<ChatLogStore>,
        key: &ChatThreadKey,
    ) -> Vec<ChatLogMessage> {
        store
            .read_page(key, None, 100, None)
            .await
            .unwrap()
            .messages
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_caller_view_appends_request_and_response_on_round_trip() {
        use crate::tunnel::hub_directory::{AgentResolution, ResolvedExposure};
        use peko_auth::Subject;

        let directory = Arc::new(FakeAgentDirectory::new());
        let caller_pending = Arc::new(PendingA2aResponses::new());

        directory.register_did(
            "did:peko:principal:target-keyhash",
            AgentResolution {
                runtime_id: "did:key:zTargetRuntime".to_string(),
                instance_id: "inst-target-e2e".to_string(),
                agent_did: "did:peko:principal:target-keyhash".to_string(),
                owner_principal: Subject::Public,
                exposure: ResolvedExposure::Public,
                transport_preference: crate::tunnel::known_runtimes::TransportPreference::Auto,
                direct_endpoint: None,
            },
        );

        let (caller_outbound_tx, caller_outbound_rx) = tokio::sync::mpsc::channel::<TunnelMessage>(
            crate::tunnel::client::TUNNEL_OUTBOUND_BUFFER_SIZE,
        );

        let hub_pending = caller_pending.clone();
        let hub_task = tokio::spawn(async move {
            run_principal_send_hub(
                caller_outbound_rx,
                hub_pending,
                "did:peko:principal:target-keyhash",
                "looks good",
            )
            .await;
        });

        let caller_did = "did:peko:principal:caller-keyhash".to_string();
        let target_did = "did:peko:principal:target-keyhash".to_string();
        let (ctx, store) = {
            // Reuse the existing builder but we need its tuple
            // form, so we replicate the wiring here with a fresh
            // signing key.
            let kp = peko_identity::keys::KeyPair::generate();
            let signing_key = Arc::new(kp.signing_key);
            let caller_runtime_id =
                crate::tunnel::verifying_key_to_did_key(&signing_key.verifying_key());
            make_round_trip_ctx(
                directory.clone(),
                caller_pending.clone(),
                signing_key,
                caller_runtime_id,
                caller_outbound_tx,
            )
        };
        let tool = SendPeerTool::new(caller_did.clone(), ctx);

        let args = SendPeerArgs {
            target: target_did.clone(),
            message: "review this PR".to_string(),
            session_id: None,
            label: None,
        };
        let value = tool
            .execute(serde_json::to_value(args).unwrap())
            .await
            .unwrap();
        let result: PrincipalSendResult = serde_json::from_value(value).unwrap();
        assert!(
            result.success,
            "round-trip should succeed: {:?}",
            result.error
        );

        let key = caller_key(&caller_did, &target_did);
        let view = read_caller_view(&store, &key).await;
        assert_eq!(
            view.len(),
            2,
            "caller view should hold one request + one response; got: {:?}",
            view.iter()
                .map(|m| (&m.sender, &m.text))
                .collect::<Vec<_>>()
        );
        assert!(matches!(view[0].sender, Subject::Principal(ref d) if d.0 == caller_did));
        assert_eq!(view[0].text, "review this PR");
        assert!(matches!(view[1].sender, Subject::Principal(ref d) if d.0 == target_did));
        assert!(
            view[1]
                .text
                .contains("echo from did:peko:principal:target-keyhash"),
            "response text should match hub echo: {}",
            view[1].text
        );
        // Correlation ids must match between request and response
        // so a future paging consumer can pair them.
        let req_corr = view[0].correlation_id.clone();
        let res_corr = view[1].correlation_id.clone();
        assert!(
            req_corr.is_some(),
            "request line should carry correlation id"
        );
        assert_eq!(
            res_corr, req_corr,
            "request and response should share the same correlation id"
        );

        drop(tool);
        let _ = hub_task.await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_caller_view_records_only_request_when_response_decodes_fail() {
        use crate::tunnel::hub_directory::{AgentResolution, ResolvedExposure};
        use peko_auth::Subject;

        let directory = Arc::new(FakeAgentDirectory::new());
        let caller_pending = Arc::new(PendingA2aResponses::new());

        directory.register_did(
            "did:peko:principal:target-keyhash",
            AgentResolution {
                runtime_id: "did:key:zTargetRuntime".to_string(),
                instance_id: "inst-target-e2e".to_string(),
                agent_did: "did:peko:principal:target-keyhash".to_string(),
                owner_principal: Subject::Public,
                exposure: ResolvedExposure::Public,
                transport_preference: crate::tunnel::known_runtimes::TransportPreference::Auto,
                direct_endpoint: None,
            },
        );

        let (caller_outbound_tx, mut caller_outbound_rx) =
            tokio::sync::mpsc::channel::<TunnelMessage>(
                crate::tunnel::client::TUNNEL_OUTBOUND_BUFFER_SIZE,
            );

        // Hub returns a valid envelope, but the bytes inside will
        // fail to decode as PrincipalSendResult — so the caller
        // must surface an error and NOT persist a phantom reply.
        let hub_pending = caller_pending.clone();
        let hub_task = tokio::spawn(async move {
            while let Some(msg) = caller_outbound_rx.recv().await {
                if let TunnelMessage::PrincipalToPrincipalRequest { request_id, .. } = msg {
                    let _ = hub_pending.complete(&request_id, b"<<not valid json>>".to_vec());
                }
            }
        });

        let caller_did = "did:peko:principal:caller-keyhash".to_string();
        let target_did = "did:peko:principal:target-keyhash".to_string();
        let (ctx, store) = {
            let kp = peko_identity::keys::KeyPair::generate();
            let signing_key = Arc::new(kp.signing_key);
            let caller_runtime_id =
                crate::tunnel::verifying_key_to_did_key(&signing_key.verifying_key());
            make_round_trip_ctx(
                directory.clone(),
                caller_pending.clone(),
                signing_key,
                caller_runtime_id,
                caller_outbound_tx,
            )
        };
        let tool = SendPeerTool::new(caller_did.clone(), ctx);

        let args = SendPeerArgs {
            target: target_did.clone(),
            message: "review this PR".to_string(),
            session_id: None,
            label: None,
        };
        let value = tool
            .execute(serde_json::to_value(args).unwrap())
            .await
            .unwrap();
        let result: PrincipalSendResult = serde_json::from_value(value).unwrap();
        assert!(!result.success, "decode failure must surface as error");

        let key = caller_key(&caller_did, &target_did);
        let view = read_caller_view(&store, &key).await;
        assert_eq!(
            view.len(),
            1,
            "caller view must contain only the request when the response could not be decoded"
        );
        assert_eq!(view[0].text, "review this PR");

        drop(tool);
        let _ = hub_task.await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_caller_view_records_only_request_on_hub_error() {
        use crate::tunnel::hub_directory::{AgentResolution, ResolvedExposure};
        use peko_auth::Subject;

        let directory = Arc::new(FakeAgentDirectory::new());
        let caller_pending = Arc::new(PendingA2aResponses::new());

        directory.register_did(
            "did:peko:principal:registered-but-hub-rejects",
            AgentResolution {
                runtime_id: "did:key:zTargetRuntime".to_string(),
                instance_id: "inst-target-e2e".to_string(),
                agent_did: "did:peko:principal:registered-but-hub-rejects".to_string(),
                owner_principal: Subject::Public,
                exposure: ResolvedExposure::Public,
                transport_preference: crate::tunnel::known_runtimes::TransportPreference::Auto,
                direct_endpoint: None,
            },
        );

        let (caller_outbound_tx, caller_outbound_rx) = tokio::sync::mpsc::channel::<TunnelMessage>(
            crate::tunnel::client::TUNNEL_OUTBOUND_BUFFER_SIZE,
        );
        let hub_pending = caller_pending.clone();
        let hub_task = tokio::spawn(async move {
            run_principal_send_hub(
                caller_outbound_rx,
                hub_pending,
                "did:peko:principal:NONEXISTENT", // hub rejects
                "never reached",
            )
            .await;
        });

        let caller_did = "did:peko:principal:caller-keyhash".to_string();
        let target_did = "did:peko:principal:registered-but-hub-rejects".to_string();
        let (ctx, store) = {
            let kp = peko_identity::keys::KeyPair::generate();
            let signing_key = Arc::new(kp.signing_key);
            let caller_runtime_id =
                crate::tunnel::verifying_key_to_did_key(&signing_key.verifying_key());
            make_round_trip_ctx(
                directory.clone(),
                caller_pending.clone(),
                signing_key,
                caller_runtime_id,
                caller_outbound_tx,
            )
        };
        let tool = SendPeerTool::new(caller_did.clone(), ctx);

        let args = SendPeerArgs {
            target: target_did.clone(),
            message: "hi".to_string(),
            session_id: None,
            label: None,
        };
        let value = tool
            .execute(serde_json::to_value(args).unwrap())
            .await
            .unwrap();
        let result: PrincipalSendResult = serde_json::from_value(value).unwrap();
        assert!(!result.success, "hub rejection must surface as error");

        let key = caller_key(&caller_did, &target_did);
        let view = read_caller_view(&store, &key).await;
        assert_eq!(
            view.len(),
            1,
            "hub rejection must NOT add a phantom response line"
        );
        assert_eq!(view[0].text, "hi");

        drop(tool);
        let _ = hub_task.await;
    }

    // ------------------------------------------------------------------
    // User branch (send_peer → note via PeerMessenger port)
    // ------------------------------------------------------------------

    struct StubMessenger {
        origin: Option<Subject>,
        deliver_ok: bool,
        delivered: std::sync::Mutex<Vec<(Subject, String)>>,
    }

    impl StubMessenger {
        fn new(origin: Option<Subject>, deliver_ok: bool) -> Self {
            Self {
                origin,
                deliver_ok,
                delivered: std::sync::Mutex::new(Vec::new()),
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
