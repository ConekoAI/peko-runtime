//! A2A Send Tool — Minimal agent-to-agent messaging
//!
//! Implements ADR-023: delegates to StatelessAgentService, reusing the same
//! execution path as `peko send` and the HTTP API.
//!
//! ## Parameters
//! ```json
//! {
//!   "target_agent": "analyzer",
//!   "message": "Review this code for bugs",
//!   "session_id": "optional-session-to-resume",
//!   "team": "optional-team"
//! }
//! ```
//!
//! ## Response (blocking)
//! ```json
//! {
//!   "success": true,
//!   "response": "I found 3 issues...",
//!   "session_id": "agent:analyzer:session:xyz",
//!   "iterations": 2,
//!   "tool_calls": [...]
//! }
//! ```
//!
//! Async execution (`_async: true`) and timeout (`_timeout: N`) are handled
//! by the framework-level `AsyncExecutionRouter` via reserved parameters.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;

use crate::agent::stateless_service::{MessageRequest, StatelessAgentService};
use crate::auth::principal::Principal;
use crate::tools::core::Tool;

/// Where an `a2a_send` call is routed. Issue #29 (Slice A — wire shape).
///
/// Pre-#29 the only routable target was an agent name on the **same**
/// runtime as the caller, threaded through the legacy
/// `A2aSendArgs::target_agent` field. With #29 the call site can be
/// explicit about cross-runtime addressing without breaking that
/// legacy field — `target_agent` stays accepted, and an explicit
/// `target: TargetSpec` overrides it when present.
///
/// Slice A only **parses** `TargetSpec` and round-trips it on the wire;
/// the `Remote*` variants are not yet dispatched (they error out of
/// `A2aSendTool::build_request` with a Slice B pointer). The outbound
/// resolver, signer, and tunnel path are Slice B; the receiver
/// attribution + dispatch is Slice C.
///
/// The JSON tag is `kind` (`local` / `remote_by_did` / `remote_by_handle`)
/// to mirror the discriminant on the receiving runtime and on the
/// hub-side `resolveAgentTarget` helper described in pekohub#14.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TargetSpec {
    /// Send to an agent on the **local** runtime, addressed by its
    /// local name. This is the current `target_agent` behavior, reified
    /// into an explicit variant so cross-runtime callers can express
    /// "no, really, the same runtime" without ambiguity.
    Local {
        /// Local agent name.
        name: String,
    },
    /// Send to an agent on a **remote** runtime, addressed by its
    /// stable DID (issue #28 form: `did:peko:agent:<keyhash>`). The
    /// `runtime_id_hint` lets the caller short-circuit the PekoHub
    /// directory lookup (pekohub#14) when it already knows the
    /// target's `runtime_id` from a previous resolution.
    #[serde(rename_all = "snake_case")]
    RemoteByDid {
        /// Target agent DID (`did:peko:agent:...`).
        did: String,
        /// Optional cached `runtime_id` of the host runtime. Slice B
        /// uses it to skip the directory lookup; when absent, Slice B
        /// resolves via `GET /v1/agents/by-did/:did` (pekohub#14).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        runtime_id_hint: Option<String>,
    },
    /// Send to an agent on a **remote** runtime, addressed by a
    /// human-readable `{owner, agent_name}` handle. The runtime
    /// resolves the handle via PekoHub's `GET /v1/agents/by-handle/:owner/:agent_name`
    /// endpoint (pekohub#14).
    #[serde(rename_all = "snake_case")]
    RemoteByHandle {
        /// User namespace (for `User` owners) or team handle
        /// (for `Team` owners — gated on pekohub#8).
        owner: String,
        /// Agent name within that owner's namespace.
        agent_name: String,
    },
}

impl TargetSpec {
    /// Whether this target requires a cross-runtime hop. `false` for
    /// `Local`, `true` for either `Remote*` variant. Slice A short-
    /// circuits on this in `A2aSendTool::build_request` to avoid
    /// silently dispatching cross-runtime calls to the local agent
    /// table before the outbound path lands in Slice B.
    #[must_use]
    pub const fn is_remote(&self) -> bool {
        !matches!(self, Self::Local { .. })
    }
}

/// Arguments for the `a2a_send` tool
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A2aSendArgs {
    /// Target agent name (legacy, pre-#29). Equivalent to
    /// `target = Local { name: target_agent }`. Required for
    /// back-compat with pre-#29 callers and the current LLM-facing
    /// tool description; ignored when `target` is explicitly set.
    pub target_agent: String,
    /// Issue #29: explicit target spec. When present, takes precedence
    /// over `target_agent`. The `Local` variant behaves identically to
    /// the legacy `target_agent` path (just spelled explicitly); the
    /// `Remote*` variants are routed cross-runtime over the tunnel
    /// (Slices B/C).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<TargetSpec>,
    /// Message content to send
    pub message: String,
    /// Optional session ID to resume
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Optional team for the target agent
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
}

impl A2aSendArgs {
    /// Resolve the effective `TargetSpec` for this call, honoring the
    /// legacy `target_agent` path when no explicit `target` is set.
    ///
    /// This is the single normalization point — anything downstream
    /// of `build_request` matches on a `TargetSpec`, never on the
    /// (`target`, `target_agent`) pair.
    #[must_use]
    pub fn effective_target(&self) -> TargetSpec {
        self.target.clone().unwrap_or_else(|| TargetSpec::Local {
            name: self.target_agent.clone(),
        })
    }
}

/// Result of an `a2a_send` execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A2aSendResult {
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

/// A2A Send tool — send a message to another agent and receive its response
///
/// This tool delegates to StatelessAgentService, reusing the exact same
/// execution path as `peko send` and the HTTP API.
pub struct A2aSendTool {
    agent_service: Arc<StatelessAgentService>,
    /// Optional caller agent name for annotation
    caller_agent: Option<String>,
    /// Optional caller agent DID (issue #28). When set, this is what
    /// gets projected to `Principal::Agent(...)` on the wire so the
    /// receiving agent's session is keyed by a stable, runtime-independent
    /// identifier. Falls back to `caller_agent` (the local name) when
    /// unset — this is the legacy behavior and is fine for single-runtime
    /// use but ambiguous across runtimes.
    caller_agent_did: Option<String>,
}

impl A2aSendTool {
    /// Create a new A2A send tool
    #[must_use]
    pub fn new(agent_service: Arc<StatelessAgentService>) -> Self {
        Self {
            agent_service,
            caller_agent: None,
            caller_agent_did: None,
        }
    }

    /// Set the caller agent name for message annotation
    #[must_use]
    pub fn with_caller(mut self, caller: impl Into<String>) -> Self {
        self.caller_agent = Some(caller.into());
        self
    }

    /// Set the caller agent DID (issue #28). Prefer this over
    /// `with_caller` when registering the tool: the DID is what flows
    /// through to `Principal::Agent` on the wire, the name is just for
    /// the human-readable annotation. `caller` is also set as a
    /// back-compat fallback for the (rare) case where the DID is missing
    /// — see `build_request` for the resolution order.
    #[must_use]
    pub fn with_caller_did(mut self, caller: impl Into<String>, did: impl Into<String>) -> Self {
        self.caller_agent = Some(caller.into());
        self.caller_agent_did = Some(did.into());
        self
    }

    /// Build the message with optional caller annotation
    #[allow(dead_code)]
    fn build_message(&self, message: &str) -> String {
        match &self.caller_agent {
            Some(caller) => format!("[Message from agent: {caller}]\n\n{message}"),
            None => message.to_string(),
        }
    }

    /// Build the `MessageRequest` for an A2A send, attributing the
    /// receiving agent's session to `Principal::Agent(caller_agent_id)`
    /// (issue #24).
    ///
    /// This is split out from `execute` so the validation logic is
    /// unit-testable without spinning up a real `StatelessAgentService`.
    ///
    /// # Errors
    /// Returns `Err` if `caller_agent` is missing or empty. The
    /// pre-ADR-039 behavior was to fall back to the literal
    /// `"default"` user, which corrupted audit trails and broke the
    /// cross-kind permission grant path. We refuse instead.
    pub(crate) fn build_request(&self, args: A2aSendArgs) -> Result<MessageRequest> {
        // Issue #24: a2a_send must attribute the receiving agent's
        // session to a `Principal::Agent(caller_agent_id)`, not
        // masquerade as a `Principal::User(caller_agent_id)`. The
        // masquerade was correct before ADR-039 (no Agent principal
        // existed); after ADR-039 it lies to the audit log, breaks
        // cross-kind permission grants, and mis-classifies the
        // per-extension chokepoint.
        //
        // We require a known caller_agent. a2a_send is agent-to-agent,
        // so a missing caller indicates a misconfigured tool
        // registration (no `with_caller()` set on the `A2aSendTool`
        // builder). Refuse rather than fall back to a fake user.
        let caller_agent = validate_caller_agent(self.caller_agent.as_deref())?;

        // Issue #28: prefer the caller DID for the wire-side
        // `Principal::Agent` so cross-runtime references stay
        // unambiguous. Fall back to the caller name (legacy) when no
        // DID is set — fine within a single runtime, ambiguous across
        // runtimes by design. The `caller_agent` annotation is always
        // the human-readable name regardless of which one is used.
        //
        // Review of #34: route through `Principal::agent_wire_id` so
        // the DID/name resolution and the empty-string guard live in
        // exactly one place. Previously the a2a_send path had its
        // own `.filter(|d| !d.is_empty())` clause that disagreed
        // subtly with `AgentConfig::wire_agent_id`.
        let wire_caller_id =
            Principal::agent_wire_id(self.caller_agent_did.as_deref(), caller_agent);

        // Issue #29 (Slice A): normalize the (target_agent, target)
        // pair to a single `TargetSpec` and short-circuit the
        // unimplemented remote variants via the same free-function
        // seam the existing tests use for `validate_caller_agent` and
        // `build_a2a_request`. Slice B will replace the short-circuit
        // with the real outbound resolver + signer + tunnel hop.
        let target = args.effective_target();
        let local_name = resolve_local_target(&target)?.to_string();

        let request = build_a2a_request(
            &local_name,
            args.message,
            args.session_id,
            args.team,
            caller_agent,
            &wire_caller_id,
        );
        Ok(request)
    }
}

/// Validate the `caller_agent` field for issue #24.
///
/// Returns the non-empty caller_agent string if valid, or an `Err`
/// suitable for surfacing to the LLM caller. Exposed as `pub(crate)`
/// so unit tests can assert the actual production predicate instead
/// of duplicating it (review #3).
///
/// The empty-string check matches the pre-fix behavior; whitespace is
/// preserved verbatim (a `Principal::Agent("   ")` is a misconfigured
/// caller, but it's not a missing one — the agent operator will see
/// it in the audit log immediately).
pub(crate) fn validate_caller_agent(caller: Option<&str>) -> Result<&str> {
    caller.filter(|s| !s.is_empty()).ok_or_else(|| {
        anyhow!(
            "a2a_send: caller_agent is not set; this tool must be \
             constructed with A2aSendTool::with_caller(...) so the \
             receiving agent's session is attributed to the \
             calling agent (issue #24)."
        )
    })
}

/// Resolve a `TargetSpec` to a local agent name, or short-circuit with
/// a Slice-B-pointer error for the remote variants. Issue #29 Slice A.
///
/// Exposed as `pub(crate)` so unit tests can pin the contract that the
/// remote-rejection path returns a structured error mentioning Slice B
/// (so anyone tracing the error back through a log or CI report can
/// find the work that lifts the limitation).
///
/// The Slice B replacement will branch on this same `TargetSpec` and
/// route remote variants through the outbound resolver + tunnel
/// dispatcher; the local path will stay verbatim. Keeping the seam
/// pinned today makes the Slice B diff small and review-friendly.
pub(crate) fn resolve_local_target(target: &TargetSpec) -> Result<&str> {
    match target {
        TargetSpec::Local { name } => Ok(name),
        TargetSpec::RemoteByDid { .. } | TargetSpec::RemoteByHandle { .. } => Err(anyhow!(
            "a2a_send: cross-runtime target dispatch is not yet \
             implemented. peko-runtime#29 Slice A landed the wire \
             shape ({target:?}); Slice B adds the outbound resolver, \
             signer, and tunnel hop. Use TargetSpec::Local (or the \
             legacy target_agent field) until Slice B lands."
        )),
    }
}

/// Pure (no `agent_service` access) request builder, factored out so
/// the validation logic is unit-testable (issue #24).
///
/// `caller_agent` must be non-empty; the caller (`A2aSendTool::build_request`)
/// has already validated this.
///
/// `wire_caller_id` is the value projected into `Principal::Agent` on
/// the wire — typically the agent's DID (issue #28) but can be the name
/// as a legacy fallback. `caller_agent` is preserved verbatim on
/// `MessageRequest::caller_agent` for the human-readable annotation.
///
/// **Issue #24 review concern #1:** `user` is left as the empty string
/// for a2a_send (not populated with `caller_agent`). This forces every
/// downstream code path that still reads `MessageRequest::user` to
/// encounter a falsy value and migrate to `caller_principal` instead
/// of silently seeing the agent name masquerade as a user id (which
/// is exactly the audit-trail footgun the issue is built on).
#[allow(clippy::too_many_arguments)]
fn build_a2a_request(
    target_agent: &str,
    message: String,
    session_id: Option<String>,
    team: Option<String>,
    caller_agent: &str,
    wire_caller_id: &str,
) -> MessageRequest {
    let caller_principal = Principal::Agent(wire_caller_id.to_string());
    // The `user` field is INTENTIONALLY left as the empty string for
    // a2a_send (issue #24 review #1). Any reader of
    // `MessageRequest::user` for a2a-originated calls must migrate to
    // `caller_principal`. The audit log path uses `caller_principal`
    // as its single source of truth, so the empty string here is
    // safe — it just means "no human user is associated with this
    // call," which is the correct semantic.
    MessageRequest::new(target_agent, message)
        .with_session_opt(session_id)
        .with_team_opt(team)
        .with_user("")
        .with_caller_agent_opt(Some(caller_agent.to_string()))
        .with_caller_principal(caller_principal)
}

#[async_trait]
impl Tool for A2aSendTool {
    fn name(&self) -> &'static str {
        "a2a_send"
    }

    fn description(&self) -> String {
        r#"## Purpose
Send a message to another agent and receive its response. This is the primary mechanism for agent-to-agent (A2A) communication.

## When to Use
- Delegate a subtask to another agent
- Request analysis, review, or specialized work from a peer agent
- Resume a conversation with another agent using a known session_id

## When NOT to Use
- For human-to-agent communication (use the CLI/API instead)
- For fire-and-forget notifications (A2A send is request/response)

## Parameters
```json
{
  "target_agent": "analyzer",
  "message": "Review this code for bugs",
  "session_id": "optional-session-to-resume",
  "team": "optional-team"
}
```

## Response
```json
{
  "success": true,
  "response": "I found 3 issues...",
  "session_id": "agent:analyzer:session:xyz",
  "iterations": 2
}
```"#
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "target_agent": {
                    "type": "string",
                    "description": "Name of the target agent to send the message to"
                },
                "message": {
                    "type": "string",
                    "description": "Message content to send to the target agent"
                },
                "session_id": {
                    "type": "string",
                    "description": "Optional session ID to resume an existing conversation"
                },
                "team": {
                    "type": "string",
                    "description": "Optional team name for the target agent"
                }
            },
            "required": ["target_agent", "message"]
        })
    }

    async fn execute(&self, params: serde_json::Value) -> Result<serde_json::Value> {
        let args: A2aSendArgs =
            serde_json::from_value(params).map_err(|e| anyhow!("Invalid arguments: {e}"))?;

        // Issue #24: build the request with the principal-aware path
        // (no more user-masquerade). Any caller misconfiguration is
        // surfaced here as a structured error.
        let request = self.build_request(args)?;

        let result = self.agent_service.execute_message(request).await;

        match result {
            Ok(msg_result) => {
                let tool_calls: Vec<serde_json::Value> = msg_result
                    .tool_calls
                    .iter()
                    .map(|tc| {
                        json!({
                            "id": tc.id,
                            "name": tc.name,
                            "parameters": tc.parameters,
                            "result": tc.result
                        })
                    })
                    .collect();

                let response = A2aSendResult {
                    success: msg_result.success,
                    response: msg_result.content,
                    session_id: msg_result.session_id,
                    iterations: Some(msg_result.iterations),
                    tool_calls: if tool_calls.is_empty() {
                        None
                    } else {
                        Some(tool_calls)
                    },
                    duration_ms: Some(msg_result.duration_ms),
                    error: msg_result.error,
                };

                Ok(serde_json::to_value(response)?)
            }
            Err(e) => {
                let response = A2aSendResult {
                    success: false,
                    response: String::new(),
                    session_id: String::new(),
                    iterations: None,
                    tool_calls: None,
                    duration_ms: None,
                    error: Some(e.to_string()),
                };
                Ok(serde_json::to_value(response)?)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_a2a_send_args_parsing() {
        let json = r#"{
            "target_agent": "analyzer",
            "message": "Review this code",
            "session_id": "sess_123"
        }"#;

        let args: A2aSendArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.target_agent, "analyzer");
        assert_eq!(args.message, "Review this code");
        assert_eq!(args.session_id, Some("sess_123".to_string()));
        assert_eq!(args.team, None);
    }

    #[test]
    fn test_a2a_send_args_minimal() {
        let json = r#"{
            "target_agent": "helper",
            "message": "Hello"
        }"#;

        let args: A2aSendArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.target_agent, "helper");
        assert_eq!(args.message, "Hello");
        assert_eq!(args.session_id, None);
    }

    #[test]
    fn test_caller_annotation_format() {
        // Verify the caller annotation format used by StatelessAgentService::execute_inner.
        // The service prepends this format when caller_agent is set on ExecutionRequest.
        let caller = "researcher";
        let msg = "Do this task";
        let annotated = format!("[Message from agent: {caller}]\n\n{msg}");
        assert!(annotated.contains("researcher"));
        assert!(annotated.ends_with(msg));
        assert!(annotated.starts_with("[Message from agent:"));
    }

    #[test]
    fn test_a2a_send_result_serialization() {
        let result = A2aSendResult {
            success: true,
            response: "All good".to_string(),
            session_id: "agent:test:session:abc".to_string(),
            iterations: Some(1),
            tool_calls: None,
            duration_ms: Some(1500),
            error: None,
        };

        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["success"], true);
        assert_eq!(json["response"], "All good");
        assert_eq!(json["session_id"], "agent:test:session:abc");
        assert!(json.get("tool_calls").is_none());
    }

    // -- Issue #24: a2a_send masquerade removal -----------------------

    /// `validate_caller_agent` is the production predicate used by
    /// `A2aSendTool::build_request` (issue #24). Test the actual
    /// function, not a copy — if the production predicate drifts,
    /// this test must catch it (review #3).
    #[test]
    fn test_validate_caller_agent_rejects_missing_and_empty() {
        // Missing caller → error.
        let err = validate_caller_agent(None).expect_err("missing caller must be rejected");
        assert!(
            err.to_string().contains("caller_agent is not set"),
            "error must mention caller_agent; got: {err}"
        );

        // Empty caller → error (same message — they're both "no caller").
        let err = validate_caller_agent(Some("")).expect_err("empty caller must be rejected");
        assert!(
            err.to_string().contains("caller_agent is not set"),
            "error must mention caller_agent; got: {err}"
        );

        // Whitespace is NOT empty — preserved verbatim. This is
        // deliberate: a `Principal::Agent("   ")` is a
        // misconfigured caller, not a missing one. The agent
        // operator sees it in the audit log immediately rather
        // than being silently coerced to `User("default")`.
        assert_eq!(validate_caller_agent(Some("   ")).unwrap(), "   ");

        // Normal caller → passes through.
        assert_eq!(validate_caller_agent(Some("helper")).unwrap(), "helper");
    }

    /// The pure `build_a2a_request` helper attaches
    /// `caller_principal = Principal::Agent(caller)` and never
    /// `Principal::User(caller)`. This is the core fix for issue
    /// #24 — the receiving agent's session is keyed under
    /// `agent:{caller}`, not `user:{caller}`.
    ///
    /// Review of #34 (non-blocking): `caller_agent` (the
    /// human-readable name) and `wire_caller_id` (the value
    /// projected to `Principal::Agent`) are intentionally
    /// distinct here so the test exercises both the
    /// caller-annotation code path AND the wire-identifier code
    /// path on the same request. Pre-fix, both args were `"helper"`
    /// and the test didn't actually distinguish them.
    #[test]
    fn test_build_a2a_request_attaches_caller_principal_as_agent() {
        let req = build_a2a_request(
            "analyzer",
            "review this".to_string(),
            Some("sess-1".to_string()),
            None,
            "helper",
            "did:peko:local:abc123",
        );

        assert_eq!(
            req.caller_principal,
            Some(Principal::Agent("did:peko:local:abc123".into())),
            "caller_principal must be Principal::Agent(<DID>), not a User masquerade"
        );
        // Belt-and-suspenders: confirm we're not falling back to the
        // legacy user path by accident.
        assert_ne!(
            req.caller_principal,
            Some(Principal::User("helper".into())),
            "must not masquerade caller_agent as Principal::User (issue #24)"
        );
        // Issue #24 review #1: `user` must be empty so downstream
        // readers can't accidentally treat the caller as a human user.
        assert_eq!(
            req.user, "",
            "a2a_send must leave MessageRequest::user empty (review #1); \
             downstream code must read caller_principal instead"
        );
        // caller_agent annotation stays as the human-readable name
        // even when the wire id is the DID (issue #28).
        assert_eq!(req.caller_agent.as_deref(), Some("helper"));
        assert_eq!(req.session_id.as_deref(), Some("sess-1"));
        assert_eq!(req.agent_name, "analyzer");
        assert_eq!(req.message, "review this");
        assert_eq!(req.team, None);
    }

    /// Two distinct callers produce two distinct principals — the
    /// session-key isolation invariant the issue's tests rely on.
    #[test]
    fn test_build_a2a_request_distinguishes_callers() {
        let req_a = build_a2a_request("target", "hi".into(), None, None, "caller_a", "caller_a");
        let req_b = build_a2a_request("target", "hi".into(), None, None, "caller_b", "caller_b");

        assert_eq!(
            req_a.caller_principal,
            Some(Principal::Agent("caller_a".into()))
        );
        assert_eq!(
            req_b.caller_principal,
            Some(Principal::Agent("caller_b".into()))
        );
        assert_ne!(
            req_a.caller_principal, req_b.caller_principal,
            "different callers must produce different principals so the \
             session keys stay isolated"
        );
    }

    /// Issue #28: when a DID is provided as `wire_caller_id`, the
    /// `Principal::Agent` on the wire must be the DID (not the local
    /// name) so cross-runtime references are unambiguous. The
    /// `caller_agent` annotation stays as the human-readable name.
    #[test]
    fn test_build_a2a_request_prefers_did_for_wire_principal() {
        let req = build_a2a_request(
            "analyzer",
            "review this".to_string(),
            None,
            None,
            "helper",
            "did:peko:local:abc123",
        );
        assert_eq!(
            req.caller_principal,
            Some(Principal::Agent("did:peko:local:abc123".into())),
            "caller_principal must be the DID when provided (issue #28)"
        );
        assert_eq!(
            req.caller_agent.as_deref(),
            Some("helper"),
            "caller_agent annotation must remain the human-readable name"
        );
    }

    // -- Issue #29 (Slice A): TargetSpec wire shape --------------------

    /// Legacy `A2aSendArgs` JSON (no `target` field) must still
    /// parse. Slice A is additive — the wire-compatible default for
    /// `target` is `None`, which `effective_target()` projects to
    /// `TargetSpec::Local { name: target_agent }`. Existing LLM
    /// tool-call producers and persisted call records (e.g. audit
    /// trails, fixtures) keep working without re-emission.
    #[test]
    fn test_a2a_send_args_back_compat_no_target() {
        let json = r#"{
            "target_agent": "analyzer",
            "message": "review this"
        }"#;
        let args: A2aSendArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.target_agent, "analyzer");
        assert!(args.target.is_none(), "legacy callers omit `target`");
        assert_eq!(
            args.effective_target(),
            TargetSpec::Local {
                name: "analyzer".to_string(),
            },
            "the legacy path projects to TargetSpec::Local"
        );
    }

    /// When an explicit `target` is provided, it takes precedence
    /// over the legacy `target_agent`. The `target_agent` is still
    /// required (back-compat with the LLM tool description) but is
    /// effectively a hint until Slice B exposes `target` to the LLM
    /// schema.
    #[test]
    fn test_a2a_send_args_explicit_local_target_overrides_legacy() {
        let json = r#"{
            "target_agent": "ignored-legacy-name",
            "target": { "kind": "local", "name": "preferred" },
            "message": "hello"
        }"#;
        let args: A2aSendArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.target_agent, "ignored-legacy-name");
        assert_eq!(
            args.effective_target(),
            TargetSpec::Local {
                name: "preferred".to_string(),
            },
            "explicit `target` overrides the legacy `target_agent`"
        );
    }

    /// `TargetSpec::RemoteByDid` round-trips through JSON with the
    /// expected `kind` tag and snake_case field names. The
    /// `runtime_id_hint` is optional and omitted from the wire form
    /// when absent (the hub directory lookup is the fallback path).
    #[test]
    fn test_target_spec_remote_by_did_roundtrip() {
        let spec = TargetSpec::RemoteByDid {
            did: "did:peko:agent:abcd1234".to_string(),
            runtime_id_hint: Some("did:key:zRuntime".to_string()),
        };
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(json["kind"], "remote_by_did");
        assert_eq!(json["did"], "did:peko:agent:abcd1234");
        assert_eq!(json["runtime_id_hint"], "did:key:zRuntime");

        let back: TargetSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, spec);

        // Hint-less form is also valid and omits the field.
        let spec_no_hint = TargetSpec::RemoteByDid {
            did: "did:peko:agent:abcd1234".to_string(),
            runtime_id_hint: None,
        };
        let json_no_hint = serde_json::to_value(&spec_no_hint).unwrap();
        assert!(
            json_no_hint.get("runtime_id_hint").is_none(),
            "runtime_id_hint must be omitted when None (hub-side resolves \
             via pekohub#14 directory lookup); got: {json_no_hint}"
        );
    }

    /// `TargetSpec::RemoteByHandle` round-trips with `owner` +
    /// `agent_name` — the human-friendly form that pekohub#14's
    /// `/v1/agents/by-handle/:owner/:agent_name` endpoint resolves.
    #[test]
    fn test_target_spec_remote_by_handle_roundtrip() {
        let spec = TargetSpec::RemoteByHandle {
            owner: "alice".to_string(),
            agent_name: "analyzer".to_string(),
        };
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(json["kind"], "remote_by_handle");
        assert_eq!(json["owner"], "alice");
        assert_eq!(json["agent_name"], "analyzer");

        let back: TargetSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, spec);
    }

    /// `TargetSpec::Local` round-trips with just `name`. Useful for
    /// the (uncommon but legal) case where a caller wants to be
    /// explicit about same-runtime addressing.
    #[test]
    fn test_target_spec_local_roundtrip() {
        let spec = TargetSpec::Local {
            name: "helper".to_string(),
        };
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(json["kind"], "local");
        assert_eq!(json["name"], "helper");

        let back: TargetSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, spec);
    }

    /// `is_remote()` discriminates the cross-runtime variants. The
    /// `build_request` short-circuit and the future Slice B
    /// dispatcher both branch on this predicate, so it gets its own
    /// guard.
    #[test]
    fn test_target_spec_is_remote_discriminator() {
        assert!(!TargetSpec::Local {
            name: "x".into(),
        }
        .is_remote());
        assert!(TargetSpec::RemoteByDid {
            did: "did:peko:agent:x".into(),
            runtime_id_hint: None,
        }
        .is_remote());
        assert!(TargetSpec::RemoteByHandle {
            owner: "u".into(),
            agent_name: "a".into(),
        }
        .is_remote());
    }

    /// `resolve_local_target` returns the local name verbatim for
    /// `TargetSpec::Local`. This pins the "Local is just an alias for
    /// the legacy target_agent path" invariant Slice A depends on —
    /// the Slice B diff will keep the local arm verbatim and add a
    /// new arm for the remote variants.
    #[test]
    fn test_resolve_local_target_passes_local_through() {
        let target = TargetSpec::Local {
            name: "helper".to_string(),
        };
        let name = resolve_local_target(&target).expect("Local must resolve");
        assert_eq!(name, "helper");
    }

    /// `resolve_local_target` short-circuits on the two `Remote*`
    /// variants with a structured error mentioning Slice B. The
    /// error string is part of the contract — when someone hits
    /// this in a log or test report, they need to find the issue
    /// and slice that lifts the limitation. If Slice B changes the
    /// message text, this test must be updated in the same diff.
    #[test]
    fn test_resolve_local_target_rejects_remote_with_slice_b_pointer() {
        let did_target = TargetSpec::RemoteByDid {
            did: "did:peko:agent:remote-xyz".to_string(),
            runtime_id_hint: None,
        };
        let err = resolve_local_target(&did_target)
            .expect_err("RemoteByDid must short-circuit until Slice B");
        let msg = err.to_string();
        assert!(
            msg.contains("cross-runtime target dispatch is not yet implemented"),
            "error must name the unimplemented condition; got: {msg}"
        );
        assert!(
            msg.contains("Slice B"),
            "error must point at Slice B so callers can find the work; got: {msg}"
        );
        assert!(
            msg.contains("did:peko:agent:remote-xyz"),
            "error must surface the target so it appears in audit/log traces; got: {msg}"
        );

        let handle_target = TargetSpec::RemoteByHandle {
            owner: "alice".to_string(),
            agent_name: "analyzer".to_string(),
        };
        let err = resolve_local_target(&handle_target)
            .expect_err("RemoteByHandle must short-circuit until Slice B");
        let msg = err.to_string();
        assert!(
            msg.contains("Slice B"),
            "RemoteByHandle error must also point at Slice B; got: {msg}"
        );
    }
}
