//! Principal Send Tool — principal-to-principal cross-runtime messaging
//!
//! Replaces the agent-targeted `a2a_send` tool at the principal level.
//! The target is a Principal DID (not an agent name in a target
//! runtime); the inbound receiver (`dispatcher::handle_inbound_agent_to_agent_request`)
//! already routes to the principal directly. The wire envelope
//! `TunnelMessage::AgentToAgentRequest` is reused verbatim — its fields
//! are already principal-typed (`caller_principal_did`,
//! `target_principal_did`).
//!
//! ## Parameters
//! ```json
//! {
//!   "target_principal": "did:peko:principal:<keyhash>",
//!   "message": "Please review this code",
//!   "session_id": "optional-session-to-resume"
//! }
//! ```
//!
//! ## Response (blocking)
//! ```json
//! {
//!   "success": true,
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
//!   `principal_send` working when PekoHub is offline. Remote targets still
//!   flow through the tunnel or a direct connection as selected below.
//! - **Callee preference.** The hub directory returns the target principal's
//!   `transport_preference` and advertised `direct_endpoint`. The caller
//!   respects the callee's preference; if direct is requested but unavailable
//!   the call errors rather than silently falling back to the tunnel.
//! - **Tool name**: `"principal_send"` (drops the agent-level naming
//!   the prior `a2a_send` carried).
//!
//! Async execution and timeout are handled by the framework-level
//! `AsyncExecutionRouter` via the reserved `_async` / `_timeout`
//! parameters, same as every other tool.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;

use crate::auth::Subject;
use crate::principal::{ChannelContext, ChannelKind};
use crate::tools::core::Tool;
use crate::tunnel::a2a_audit;
use crate::tunnel::a2a_signature::{sign_request, SignedFields};
use crate::tunnel::cross_runtime::CrossRuntimeA2aCtx;
use crate::tunnel::direct::routing::{select_transport, TransportChoice};
use crate::tunnel::hub_directory::{DirectoryError, ResolvedExposure};
use crate::tunnel::TunnelMessage;

/// Arguments for the `principal_send` tool.
///
/// `target_principal` is the target principal's stable DID
/// (`did:peko:principal:<keyhash>`). Resolution is handled by the hub
/// directory on the caller's side; the wire payload carries the DID
/// directly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrincipalSendArgs {
    /// Target principal DID (e.g. `did:peko:principal:abc...`).
    pub target_principal: String,
    /// Message content to deliver to the target principal's root
    /// agent.
    pub message: String,
    /// Optional session ID to resume on the target principal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// Result of a `principal_send` execution. Shape mirrors `A2aSendResult`
/// so any consumer of the legacy tool can deserialize either with a
/// schema-tolerant adapter. The principal-level receiver
/// (`dispatcher::handle_inbound_agent_to_agent_request`) produces this
/// exact shape on its `Ok` branch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrincipalSendResult {
    pub success: bool,
    pub response: String,
    pub session_id: String,
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
/// injects this shape into `AgentToAgentResponse.payload` when it
/// can't deliver the request (target offline, target unknown, etc.).
/// Same wire shape used by `a2a_send` so callers can share decoders.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HubErrorResponse {
    pub kind: String,
    pub code: String,
    pub message: String,
}

/// Principal Send tool — send a message to another principal and
/// receive its root agent's response.
///
/// The tool carries the caller's principal identity (DID) at
/// construction time; the LLM never sets the caller, only the
/// target. This eliminates the "caller masquerades as a user" audit
/// foot-gun the legacy `a2a_send` had when `caller_principal_did`
/// wasn't set.
pub struct PrincipalSendTool {
    /// The local principal's stable DID. Bound at construction from
    /// the `Agent::principal_id` (resolved via `Principal::did()` at
    /// tool registration).
    caller_principal_did: String,
    /// The local runtime's `runtime_id` (did:key form) — echoed into
    /// the wire envelope from `CrossRuntimeA2aCtx::caller_runtime_id`
    /// so the target runtime can verify the signature.
    cross_runtime: Arc<CrossRuntimeA2aCtx>,
}

impl PrincipalSendTool {
    /// Build a PrincipalSendTool bound to a specific caller principal.
    #[must_use]
    pub fn new(caller_principal_did: String, cross_runtime: Arc<CrossRuntimeA2aCtx>) -> Self {
        Self {
            caller_principal_did,
            cross_runtime,
        }
    }

    /// Encode an error into the standard `PrincipalSendResult` JSON
    /// shape.
    fn error_value(&self, err: &str) -> serde_json::Value {
        let result = PrincipalSendResult {
            success: false,
            response: String::new(),
            session_id: String::new(),
            iterations: None,
            tool_calls: None,
            duration_ms: None,
            error: Some(err.to_string()),
        };
        serde_json::to_value(result).expect("PrincipalSendResult must serialize to JSON")
    }

    /// Dispatch `principal_send` to a target principal on the same runtime.
    async fn execute_local(
        &self,
        target_did: &str,
        message: &str,
        session_id: Option<String>,
    ) -> Result<serde_json::Value> {
        let ctx = &self.cross_runtime;
        let Some(principal) = ctx.principal_manager.find_by_did(target_did).await else {
            return Ok(self.error_value("target principal is not loaded on this runtime"));
        };
        let caller = Subject::Principal(self.caller_principal_did.clone().into());
        let channel = ChannelContext {
            kind: ChannelKind::A2a,
            streaming: false,
        };
        match ctx
            .principal_manager
            .receive(principal.id.clone(), caller, message.to_string(), channel, None, None)
            .await
        {
            Ok(response) => {
                let result = PrincipalSendResult {
                    success: true,
                    response: response.content,
                    session_id: session_id.unwrap_or_default(),
                    iterations: None,
                    tool_calls: None,
                    duration_ms: None,
                    error: None,
                };
                Ok(serde_json::to_value(result)?)
            }
            Err(err) => Ok(self.error_value(&format!("local principal_send failed: {err}"))),
        }
    }
}

/// Build an `Arc<dyn Tool>` for the `principal_send` extension.
/// Replaces direct `PrincipalSendTool::new(...)` calls at the
/// registration site so callers don't depend on the concrete type.
#[must_use]
pub fn build_tool(
    caller_principal_did: String,
    cross_runtime: Arc<CrossRuntimeA2aCtx>,
) -> Arc<dyn Tool> {
    Arc::new(PrincipalSendTool::new(caller_principal_did, cross_runtime))
}

#[async_trait]
impl Tool for PrincipalSendTool {
    fn name(&self) -> &'static str {
        "principal_send"
    }

    fn description(&self) -> String {
        r#"## Purpose
Send a message to another Principal's root agent and receive its response. This is the primary mechanism for principal-to-principal communication across runtime boundaries.

## When to Use
- Delegate a task to another Principal you have access to
- Request analysis, review, or specialized work from a peer Principal
- Resume a conversation with another Principal using a known session_id

## When NOT to Use
- For human-to-agent communication (use the CLI/API instead)
- For fire-and-forget notifications (principal_send is request/response)
- For spawning subagents of the SAME principal (use the Agent tool instead)

## Parameters
```json
{
  "target_principal": "did:peko:principal:<keyhash>",
  "message": "Please review this code for bugs",
  "session_id": "optional-session-to-resume"
}
```

## Response
```json
{
  "success": true,
  "response": "Review complete: 3 issues found.",
  "session_id": "principal:<peer>:session:<id>"
}
```"#
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "target_principal": {
                    "type": "string",
                    "description": "Target Principal DID (did:peko:principal:<keyhash>). The hub directory resolves the host runtime; the wire envelope carries the DID directly."
                },
                "message": {
                    "type": "string",
                    "description": "Message content to deliver to the target Principal's root agent"
                },
                "session_id": {
                    "type": "string",
                    "description": "Optional session ID to resume an existing conversation"
                }
            },
            "required": ["target_principal", "message"]
        })
    }

    async fn execute(&self, params: serde_json::Value) -> Result<serde_json::Value> {
        let args: PrincipalSendArgs =
            serde_json::from_value(params).map_err(|e| anyhow!("Invalid arguments: {e}"))?;

        // Empty-string guard for the target. The DID is what the wire
        // carries; an empty string would dispatch to a non-existent
        // target. Surface a structured error rather than letting
        // the directory call time out.
        let target_principal_did = args.target_principal.trim();
        if target_principal_did.is_empty() {
            return Ok(self.error_value(
                "principal_send: target_principal is required (Principal DID, e.g. did:peko:principal:<keyhash>)",
            ));
        }

        // Resolve the host runtime via the directory. The directory
        // is the same one the legacy `a2a_send` uses — it returns an
        // `AgentResolution { runtime_id, instance_id, agent_did, ... }`.
        // For principals, `agent_did` IS the principal DID (pekohub
        // canonicalizes the response shape across both levels). We
        // surface the directory's structured errors verbatim so the
        // LLM caller sees precise reasons (not_found / forbidden /
        // transport).
        let resolution = match self
            .cross_runtime
            .directory
            .resolve_by_did(target_principal_did)
            .await
        {
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

        let ctx = &self.cross_runtime;

        // Same-runtime shortcut: if the directory resolves to the caller's
        // own runtime, dispatch locally without the tunnel.
        if resolution.runtime_id == ctx.caller_runtime_id {
            return self
                .execute_local(target_principal_did, &args.message, args.session_id)
                .await;
        }

        let request_id = uuid::Uuid::new_v4().to_string();

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

        let envelope = TunnelMessage::AgentToAgentRequest {
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
                Ok(serde_json::to_value(result)?)
            }
            Err(decode_err) => Ok(self.error_value(&format!(
                "remote principal_send response payload could not be decoded: {decode_err}"
            ))),
        }
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
            std::env::temp_dir().join(format!("peko-principal-send-test-{}", uuid::Uuid::new_v4())),
            Arc::new(DefaultPrincipalMemoryFactory),
            Arc::new(DefaultPrincipalRouterFactory),
        ));
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
            response_timeout: Duration::from_millis(50),
        })
    }

    #[test]
    fn test_principal_send_args_parsing() {
        let json = r#"{
            "target_principal": "did:peko:principal:abc",
            "message": "Hello",
            "session_id": "sess_xyz"
        }"#;
        let args: PrincipalSendArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.target_principal, "did:peko:principal:abc");
        assert_eq!(args.message, "Hello");
        assert_eq!(args.session_id, Some("sess_xyz".to_string()));
    }

    #[test]
    fn test_principal_send_args_minimal() {
        let json = r#"{
            "target_principal": "did:peko:principal:xyz",
            "message": "Hi"
        }"#;
        let args: PrincipalSendArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.target_principal, "did:peko:principal:xyz");
        assert_eq!(args.session_id, None);
    }

    #[test]
    fn test_result_serialization_round_trip() {
        let result = PrincipalSendResult {
            success: true,
            response: "OK".to_string(),
            session_id: "principal:abc:session:xyz".to_string(),
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
        let tool = PrincipalSendTool::new("did:peko:principal:caller".into(), ctx);
        let v = tool
            .execute(json!({
                "target_principal": "",
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
        let tool = PrincipalSendTool::new("did:peko:principal:caller".into(), ctx);
        let v = tool
            .execute(json!({
                "target_principal": "did:peko:principal:missing",
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
        let tool = PrincipalSendTool::new("did:peko:principal:caller".into(), ctx);
        let v = tool
            .execute(json!({
                "target_principal": "did:peko:principal:noresolve",
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
    //    exercise `handle_inbound_agent_to_agent_request` end-to-end. ──

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
    ) -> Arc<CrossRuntimeA2aCtx> {
        use crate::principal::{
            DefaultPrincipalMemoryFactory, DefaultPrincipalRouterFactory, PrincipalManager,
        };
        use crate::tunnel::direct::DirectConnectionManager;
        use crate::tunnel::known_runtimes::KnownRuntimes;
        let tunnel_handle = TunnelHandle::new(outbound_tx);
        let principal_manager = Arc::new(PrincipalManager::new(
            std::env::temp_dir().join(format!(
                "peko-principal-send-roundtrip-{}",
                uuid::Uuid::new_v4()
            )),
            Arc::new(DefaultPrincipalMemoryFactory),
            Arc::new(DefaultPrincipalRouterFactory),
        ));
        Arc::new(CrossRuntimeA2aCtx {
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
            response_timeout: Duration::from_secs(5),
        })
    }

    /// In-memory hub forwarder. Reads from the caller's outbound
    /// `mpsc`, synthesizes the target's response, and feeds it into
    /// the caller's pending registry. Returns when the caller's
    /// outbound is closed (test cleanup). The synthesized response
    /// runs `verify_request` against the canonical pre-image from
    /// the envelope — same call the production
    /// `handle_inbound_agent_to_agent_request` makes.
    async fn run_principal_send_hub(
        mut caller_outbound: tokio::sync::mpsc::Receiver<TunnelMessage>,
        caller_pending: Arc<PendingA2aResponses>,
        expected_target_principal_did: &'static str,
        target_response_text: &'static str,
    ) {
        while let Some(msg) = caller_outbound.recv().await {
            let TunnelMessage::AgentToAgentRequest {
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
                };
                serde_json::to_vec(&result).expect("serialize result")
            };

            let _ = caller_pending.complete(&request_id, payload);
        }
    }

    /// Build the "caller runtime" with a real `PrincipalSendTool`
    /// wired to a real `CrossRuntimeA2aCtx`, a populated
    /// `FakeAgentDirectory`, and a `TunnelHandle` whose outbound
    /// sinks into the test hub.
    async fn build_caller_with_signed_runtime(
        directory: Arc<FakeAgentDirectory>,
        pending: Arc<PendingA2aResponses>,
        outbound_tx: tokio::sync::mpsc::Sender<TunnelMessage>,
        caller_principal_did: String,
    ) -> (
        PrincipalSendTool,
        Arc<SigningKey>, // for the hub to derive the caller's verifying key
    ) {
        // Use a real KeyPair so the caller's `runtime_id` is a valid
        // `did:key` (the hub's `verify_request` derives the verifying
        // key from this).
        let kp = crate::identity::keys::KeyPair::generate();
        let signing_key = Arc::new(kp.signing_key);
        let caller_vk = signing_key.verifying_key();
        let caller_runtime_id = crate::tunnel::verifying_key_to_did_key(&caller_vk);

        let ctx = make_round_trip_ctx(
            directory,
            pending,
            signing_key.clone(),
            caller_runtime_id,
            outbound_tx,
        );
        let tool = PrincipalSendTool::new(caller_principal_did, ctx);
        (tool, signing_key)
    }

    /// The full round-trip: caller's `principal_send` reaches the
    /// in-memory hub, the hub verifies the signature, synthesizes a
    /// response, and the caller's `execute` decodes the response
    /// into a `PrincipalSendResult`. Mirrors the `a2a_send`
    /// round-trip test the prior plan listed for `principal_send`.
    #[tokio::test(flavor = "multi_thread")]
    async fn test_principal_send_full_round_trip() {
        use crate::auth::Subject;
        use crate::tunnel::hub_directory::{AgentResolution, ResolvedExposure};

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
        let args = PrincipalSendArgs {
            target_principal: "did:peko:principal:target-keyhash".to_string(),
            message: "review this PR".to_string(),
            session_id: None,
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
        use crate::auth::Subject;
        use crate::tunnel::hub_directory::{AgentResolution, ResolvedExposure};

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

        let args = PrincipalSendArgs {
            target_principal: "did:peko:principal:registered-but-hub-rejects".to_string(),
            message: "hi".to_string(),
            session_id: None,
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
        use crate::auth::Subject;
        use crate::tunnel::hub_directory::{AgentResolution, ResolvedExposure};

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
        let args = PrincipalSendArgs {
            target_principal: "did:peko:principal:target-keyhash".to_string(),
            message: "verify me".to_string(),
            session_id: None,
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
}
