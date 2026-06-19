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

/// Arguments for the `a2a_send` tool
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A2aSendArgs {
    /// Target agent name
    pub target_agent: String,
    /// Message content to send
    pub message: String,
    /// Optional session ID to resume
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Optional team for the target agent
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
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
}

impl A2aSendTool {
    /// Create a new A2A send tool
    #[must_use]
    pub fn new(agent_service: Arc<StatelessAgentService>) -> Self {
        Self {
            agent_service,
            caller_agent: None,
        }
    }

    /// Set the caller agent name for message annotation
    #[must_use]
    pub fn with_caller(mut self, caller: impl Into<String>) -> Self {
        self.caller_agent = Some(caller.into());
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

        let request = build_a2a_request(
            &args.target_agent,
            args.message,
            args.session_id,
            args.team,
            caller_agent,
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

/// Pure (no `agent_service` access) request builder, factored out so
/// the validation logic is unit-testable (issue #24).
///
/// `caller_agent` must be non-empty; the caller (`A2aSendTool::build_request`)
/// has already validated this.
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
) -> MessageRequest {
    let caller_principal = Principal::Agent(caller_agent.to_string());
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
        let args: A2aSendArgs = serde_json::from_value(params)
            .map_err(|e| anyhow!("Invalid arguments: {e}"))?;

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
        let err = validate_caller_agent(None)
            .expect_err("missing caller must be rejected");
        assert!(
            err.to_string().contains("caller_agent is not set"),
            "error must mention caller_agent; got: {err}"
        );

        // Empty caller → error (same message — they're both "no caller").
        let err = validate_caller_agent(Some(""))
            .expect_err("empty caller must be rejected");
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
    #[test]
    fn test_build_a2a_request_attaches_caller_principal_as_agent() {
        let req = build_a2a_request(
            "analyzer",
            "review this".to_string(),
            Some("sess-1".to_string()),
            None,
            "helper",
        );

        assert_eq!(
            req.caller_principal,
            Some(Principal::Agent("helper".into())),
            "caller_principal must be Principal::Agent(helper), not a User masquerade"
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
        let req_a = build_a2a_request("target", "hi".into(), None, None, "caller_a");
        let req_b = build_a2a_request("target", "hi".into(), None, None, "caller_b");

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
}
