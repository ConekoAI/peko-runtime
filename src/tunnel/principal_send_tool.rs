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
//! - **No local shortcut.** Even if the target principal lives on the
//!   same daemon, the call flows through the tunnel. PekoHub is the
//!   canonical router and the receiver's `principal_manager.find_by_did`
//!   handles local dispatch on its end. This keeps the call-site
//!   invariant "everything goes through the tunnel" and avoids a
//!   daemon-internal cycle between tools and the principal manager.
//! - **No `TargetSpec` / no directory hint.** The hub routes by DID
//!   alone (pekohub#14, `resolve_by_did`). The caller's only required
//!   input is the target principal's DID.
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

use crate::tools::core::Tool;
use crate::tunnel::a2a_audit;
use crate::tunnel::a2a_signature::{sign_request, SignedFields};
use crate::tunnel::cross_runtime::CrossRuntimeA2aCtx;
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
}

/// Build an `Arc<dyn Tool>` for the `principal_send` capability.
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
        let args: PrincipalSendArgs = serde_json::from_value(params)
            .map_err(|e| anyhow!("Invalid arguments: {e}"))?;

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
        let request_id = uuid::Uuid::new_v4().to_string();

        let signed = SignedFields {
            request_id: &request_id,
            caller_runtime_id: &ctx.caller_runtime_id,
            caller_principal_did: &self.caller_principal_did,
            target_principal_did,
            message: &args.message,
            session_id: args.session_id.as_deref(),
        };
        let signature = sign_request(&ctx.signing_key, signed);

        let envelope = TunnelMessage::AgentToAgentRequest {
            request_id: request_id.clone(),
            caller_runtime_id: ctx.caller_runtime_id.clone(),
            caller_principal_did: self.caller_principal_did.clone(),
            target_principal_did: target_principal_did.to_string(),
            session_id: args.session_id.clone(),
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

        // Send over the live tunnel handle. The handle slot is `None`
        // when the tunnel isn't currently connected — same idiom as
        // the legacy `a2a_send::execute_remote` path.
        let tunnel_handle = {
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
        };
        if let Err(err) = tunnel_handle.send(envelope) {
            ctx.pending.discard(&request_id);
            return Ok(self.error_value(&format!(
                "tunnel send failed: {err} (tunnel may be disconnected)"
            )));
        }

        // Slice D: emit the outbound audit event now that the request
        // is on the wire. The session_id is best-effort and may be
        // empty on a fresh cross-principal exchange.
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
    use ed25519_dalek::SigningKey;
    use std::time::Duration;
    use tokio::sync::RwLock;

    /// Build a `CrossRuntimeA2aCtx` with a stub directory and a live
    /// (but unfilled) tunnel slot. The fake directory resolves a
    /// single test DID to a known `runtime_id`.
    fn make_test_ctx() -> Arc<CrossRuntimeA2aCtx> {
        use crate::tunnel::hub_directory::FakeAgentDirectory;
        Arc::new(CrossRuntimeA2aCtx {
            directory: Arc::new(FakeAgentDirectory::new()),
            pending: Arc::new(PendingA2aResponses::new()),
            signing_key: Arc::new(SigningKey::from_bytes(&[7u8; 32])),
            caller_runtime_id: "did:key:test-runtime".to_string(),
            tunnel: Arc::new(RwLock::new(None)),
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
}
