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
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

use crate::agents::stateless_service::{MessageRequest, StatelessAgentService};
use crate::auth::principal::Principal;
use crate::tools::core::Tool;
use crate::tunnel::a2a_audit;
use crate::tunnel::a2a_signature::{sign_request, SignedFields};
use crate::tunnel::cross_runtime::CrossRuntimeA2aCtx;
use crate::tunnel::hub_directory::{AgentDirectory, DirectoryError, ResolvedExposure};
use crate::tunnel::{
    A2aWaitError, PendingA2aResponses, TunnelHandle, TunnelMessage,
};

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
    /// Optional cross-runtime context (issue #29 Slice B). When set,
    /// `Remote*` `TargetSpec` variants dispatch over the tunnel via
    /// the hub directory; when unset they error with a structured
    /// "cross-runtime not configured" message. The ctx lives in
    /// `tunnel::cross_runtime` (re-exported here) so both the
    /// daemon-state bootstrap side and the consumer side reference
    /// the same type without an `extension` ↔ `tools` cycle.
    cross_runtime: Option<Arc<CrossRuntimeA2aCtx>>,
}

impl A2aSendTool {
    /// Create a new A2A send tool
    #[must_use]
    pub fn new(agent_service: Arc<StatelessAgentService>) -> Self {
        Self {
            agent_service,
            caller_agent: None,
            caller_agent_did: None,
            cross_runtime: None,
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

    /// Enable cross-runtime dispatch (issue #29 Slice B). When set, a
    /// `TargetSpec::RemoteByDid` or `TargetSpec::RemoteByHandle` is
    /// resolved via the hub directory, signed with the runtime's
    /// `PekoHubCredential`, and sent over the tunnel; the call blocks
    /// on the matching `AgentToAgentResponse` (correlation by
    /// `request_id` in `ctx.pending`).
    ///
    /// Until the bootstrap follow-up (Slice B') wires the ctx through
    /// `ExtensionServices`, production builds construct `A2aSendTool`
    /// without ever calling this, so `Remote*` targets continue to
    /// return the Slice B short-circuit error. Tests can call this
    /// directly with `FakeAgentDirectory` etc.
    #[must_use]
    pub fn with_cross_runtime(mut self, ctx: Arc<CrossRuntimeA2aCtx>) -> Self {
        self.cross_runtime = Some(ctx);
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

/// Resolve a `TargetSpec` to a local agent name. Issue #29 Slice A
/// introduced this helper as the short-circuit for not-yet-implemented
/// remote variants; Slice B then promoted remote dispatch to its own
/// code path on `A2aSendTool::execute_remote`. This helper now serves
/// the defense-in-depth role: `build_request` (the local-only request
/// builder) calls it to refuse any `Remote*` that escaped the
/// `execute` branch — that would be an internal bug, but better to
/// surface it loudly than dispatch a cross-runtime spec to the local
/// agent table.
///
/// Exposed as `pub(crate)` so the existing unit test pins the
/// contract for refactors that might one day collapse the two
/// execute paths.
pub(crate) fn resolve_local_target(target: &TargetSpec) -> Result<&str> {
    match target {
        TargetSpec::Local { name } => Ok(name),
        TargetSpec::RemoteByDid { .. } | TargetSpec::RemoteByHandle { .. } => Err(anyhow!(
            "a2a_send internal bug: `resolve_local_target` reached with a remote \
             TargetSpec ({target:?}). Cross-runtime dispatch flows through \
             `A2aSendTool::execute_remote` (peko-runtime#29 Slice B); reaching this \
             helper indicates `execute()` did not branch on `is_remote()` correctly."
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

        // Issue #29: split local vs remote at the top so the cross-
        // runtime path's error handling stays out of the local path's
        // hot loop. The local path is unchanged from Slice A.
        match args.effective_target() {
            TargetSpec::Local { .. } => self.execute_local(args).await,
            TargetSpec::RemoteByDid { .. } | TargetSpec::RemoteByHandle { .. } => {
                self.execute_remote(args).await
            }
        }
    }
}

impl A2aSendTool {
    /// Local-runtime dispatch — the pre-#29 path, kept verbatim.
    /// Issue #24's `Principal::Agent` attribution still applies via
    /// `build_request`.
    async fn execute_local(&self, args: A2aSendArgs) -> Result<serde_json::Value> {
        let request = self.build_request(args)?;
        let result = self.agent_service.execute_message(request).await;
        Ok(message_result_to_a2a_value(result))
    }

    /// Cross-runtime dispatch — issue #29 Slice B. Resolves the
    /// target via the hub directory (or the caller-supplied hint),
    /// signs the envelope with the runtime's `PekoHubCredential`,
    /// sends it over the tunnel, and blocks on the matching
    /// `AgentToAgentResponse`. Failures at any step convert into the
    /// same `A2aSendResult` shape the local path uses, so the LLM
    /// caller sees one consistent surface.
    async fn execute_remote(&self, args: A2aSendArgs) -> Result<serde_json::Value> {
        // Validate caller identity up front. The cross-runtime path
        // requires both a `caller_agent` (for annotation) and a
        // `caller_agent_did` (for the wire-side `Principal::Agent`
        // attribution) — issue #24 + issue #28 together. Without a
        // DID we'd be sending a name-based caller to a remote runtime
        // that has no way to disambiguate it from a local agent of
        // the same name, which is exactly the foot-gun the issue
        // exists to fix.
        //
        // Both early-exits wrap into a structured `A2aSendResult`
        // envelope rather than `Err` so the LLM sees the same shape
        // on every cross-runtime failure (missing DID, missing ctx,
        // directory miss, hub rejection, response timeout, decode
        // failure). The local path's `Err` arm is wrapped into the
        // same shape in `message_result_to_a2a_value`; this is the
        // parallel arm for the remote path.
        let caller_agent = match validate_caller_agent(self.caller_agent.as_deref()) {
            Ok(s) => s,
            Err(err) => return Ok(remote_error_value(&err.to_string())),
        };
        let caller_agent_did = match self
            .caller_agent_did
            .as_deref()
            .filter(|s| !s.is_empty())
        {
            Some(s) => s,
            None => {
                return Ok(remote_error_value(
                    "a2a_send: cross-runtime dispatch requires the caller agent's DID \
                     (issue #28). Construct the tool with \
                     `A2aSendTool::with_caller_did(name, did)` so the target runtime \
                     can attribute the call under `Principal::Agent(<did>)`.",
                ));
            }
        };

        let Some(ctx) = self.cross_runtime.as_ref() else {
            return Ok(remote_error_value(
                "a2a_send: cross-runtime dispatch is not configured on this runtime. \
                 The bootstrap-time follow-up to peko-runtime#29 Slice B (the daemon-state \
                 wiring that injects `CrossRuntimeA2aCtx` into the per-agent tool) has not \
                 landed; `Remote*` targets cannot dispatch until it does. Use `Local` (or \
                 the legacy `target_agent` field) until then.",
            ));
        };

        let target = args.effective_target();
        let resolution = match resolve_remote_target(ctx.directory.as_ref(), &target).await {
            Ok(r) => r,
            Err(err) => return Ok(remote_error_value(&err)),
        };

        // Defense in depth: an unexposed target should never have been
        // surfaced by the directory, but if a stale row leaks one,
        // refuse to dispatch. The hub-side ACL is the primary gate;
        // this is the runtime-side mirror.
        if matches!(resolution.exposure, ResolvedExposure::Unexposed) {
            return Ok(remote_error_value(&format!(
                "target agent is unexposed (runtime_id={}, instance_id={})",
                resolution.runtime_id, resolution.instance_id
            )));
        }

        let target_agent_did = if resolution.agent_did.is_empty() {
            // The hub returns an empty `agent_did` only on the
            // by-handle path for pre-#34 rows. We refuse to dispatch:
            // without a DID the target runtime has no stable
            // identifier to dispatch on (the local name is not on
            // the wire). Better to error here than to silently send
            // a request the target can't route.
            return Ok(remote_error_value(
                "target runtime predates peko-runtime#34 (no agent_did); \
                 cross-runtime dispatch by-handle is not supported for legacy targets",
            ));
        } else {
            resolution.agent_did.as_str()
        };

        let request_id = uuid::Uuid::new_v4().to_string();
        let session_id = args.session_id.as_deref();
        let team = args.team.as_deref();
        let signed = SignedFields {
            request_id: &request_id,
            caller_runtime_id: &ctx.caller_runtime_id,
            caller_agent_did,
            target_agent_did,
            message: &args.message,
            session_id,
            team,
        };
        let signature = sign_request(&ctx.signing_key, signed);

        let envelope = TunnelMessage::AgentToAgentRequest {
            request_id: request_id.clone(),
            caller_runtime_id: ctx.caller_runtime_id.clone(),
            caller_agent_did: caller_agent_did.to_string(),
            target_agent_did: target_agent_did.to_string(),
            session_id: args.session_id.clone(),
            message: args.message.clone(),
            team: args.team.clone(),
            signature,
        };

        // Register BEFORE sending so the response can't beat us to the
        // pending registry. If the response somehow does arrive first
        // (impossible on a single tunnel today, but enforced anyway),
        // the dispatcher's `complete` finds no entry and logs — the
        // caller times out cleanly rather than hanging.
        let response_rx = match ctx.pending.register(&request_id) {
            Ok(rx) => rx,
            Err(err) => return Ok(remote_error_value(&err.to_string())),
        };

        // Send over the live tunnel handle. The handle slot is
        // `None` when the tunnel isn't currently connected (e.g. the
        // daemon just started and the WebSocket hasn't completed
        // yet, or the most recent reconnect attempt is still in
        // backoff). Both are "temporarily unavailable" conditions
        // that the LLM caller might reasonably want to retry.
        let tunnel_handle = {
            let guard = ctx.tunnel.read().await;
            match guard.clone() {
                Some(h) => h,
                None => {
                    // Drop the pending entry so a future request
                    // doesn't collide on the request_id.
                    ctx.pending.discard(&request_id);
                    return Ok(remote_error_value(
                        "tunnel is not currently connected; a2a_send cannot dispatch \
                         cross-runtime until the pekohub tunnel is up",
                    ));
                }
            }
        };
        if let Err(err) = tunnel_handle.send(envelope) {
            // Send failure means the tunnel channel is closed (e.g.
            // the dispatcher task ended). Drop the pending entry
            // so it doesn't leak past the failure.
            ctx.pending.discard(&request_id);
            return Ok(remote_error_value(&format!(
                "tunnel send failed: {err} (tunnel may be disconnected)"
            )));
        }

        // Slice D: emit the outbound audit event now that the
        // request is on the wire. The session_id is best-effort:
        // we don't have it here (the call comes from the tool
        // layer above the session manager), so the audit row
        // records the empty session. A future PR can plumb the
        // session id through to the tool.
        let sent_event = a2a_audit::build_a2a_sent_outbound(
            "", // session_id — see comment above
            &request_id,
            &ctx.caller_runtime_id,
            caller_agent_did,
            &resolution.runtime_id,
            target_agent_did,
            &args.message,
        );
        a2a_audit::emit_a2a_sent(&sent_event);

        // Now block on the matching response. The dispatcher's
        // `AgentToAgentResponse` arm (Slice B' wires this) decodes the
        // inbound envelope and calls `ctx.pending.complete(...)`.
        let payload = match tokio::time::timeout(ctx.response_timeout, response_rx).await {
            Ok(Ok(p)) => p,
            Ok(Err(_)) => {
                // The oneshot was dropped (cancel_all_for_disconnect
                // or the dispatcher panicked). Translate to a clear
                // error rather than leaving the LLM caller wondering.
                return Ok(remote_error_value(
                    "tunnel response channel cancelled (runtime shutting down or tunnel reset)",
                ));
            }
            Err(_) => {
                // Timeout — drop the pending entry so a late response
                // doesn't complete a vanished receiver.
                ctx.pending.discard(&request_id);
                return Ok(remote_error_value(&format!(
                    "remote a2a timed out after {:?} (target runtime_id={}, request_id={})",
                    ctx.response_timeout, resolution.runtime_id, request_id
                )));
            }
        };

        // Decode the response payload. The wire shape is dual:
        //
        //   1. **Target-runtime success/failure** — the target's Slice
        //      C produces a serialized `A2aSendResult` here. This is
        //      the common case.
        //
        //   2. **Hub-synthesized error** — when pekohub's forwarding
        //      layer (pekohub#16, shipped via pekohub#17) can't deliver
        //      the request (target offline, target unknown, hub-side
        //      ACL denied, response TTL expired, peer disconnected
        //      mid-flight), it injects a structured error envelope
        //      `{ kind: "error", code, message }` into the response
        //      payload so the caller sees a precise reason instead of
        //      a hang or a generic 500.
        //
        // We try the hub error shape first because decoding it against
        // `A2aSendResult` would fail (no `success` / `response` /
        // `session_id` fields) and surface a misleading "could not
        // decode" error. A malformed payload (neither shape decodes)
        // gets a generic decode error.
        if let Ok(hub_err) = serde_json::from_slice::<HubA2AErrorResponse>(&payload) {
            return Ok(remote_error_value(&format!(
                "remote a2a rejected by hub: {} ({})",
                hub_err.message, hub_err.code
            )));
        }
        match serde_json::from_slice::<A2aSendResult>(&payload) {
            Ok(result) => {
                // Slice D: emit the response-side audit event
                // (the "received" half of the round-trip on the
                // caller side). Uses the same caller/target swap
                // as the dispatcher's `build_a2a_sent_response`:
                // from the local runtime's perspective, the local
                // agent is the "target" of the response.
                let received_event = a2a_audit::build_a2a_received_response(
                    "", // session_id — see earlier comment
                    &request_id,
                    &ctx.caller_runtime_id,
                    caller_agent_did,
                    &resolution.runtime_id,
                    target_agent_did,
                    &result.response,
                );
                a2a_audit::emit_a2a_received(&received_event);
                Ok(serde_json::to_value(result)?)
            }
            Err(decode_err) => Ok(remote_error_value(&format!(
                "remote a2a response payload could not be decoded: {decode_err}"
            ))),
        }
        // The `caller_agent` value is intentionally unused on this
        // path right now — it's the human-readable annotation the
        // local execute_local path prepends to the message. The
        // target runtime decides whether to add its own annotation
        // based on the wire-side `caller_agent_did`; surfacing the
        // local name to the wire would leak runtime-specific naming
        // and is not part of the issue #29 contract.
        //
        // Keep the binding so a future "annotation in the message
        // body" feature has a clean call site, but the let-pattern
        // makes the unused-var warning go away.
        .map(|v| {
            let _ = caller_agent;
            v
        })
    }
}

/// Resolve a `TargetSpec::Remote*` via the directory, honoring the
/// `runtime_id_hint` short-circuit. Slice B factors this out so the
/// `execute_remote` body stays linear and unit tests can exercise the
/// directory branching without spinning up the tool / tunnel / signing
/// stack.
///
/// The hint path constructs a synthetic resolution: when the caller
/// already knows the `runtime_id`, the only piece the directory adds
/// is the `agent_did`. For `RemoteByDid` the DID *is* the input, so
/// the hint elides the round-trip entirely.
async fn resolve_remote_target(
    directory: &dyn AgentDirectory,
    target: &TargetSpec,
) -> Result<crate::tunnel::hub_directory::AgentResolution, String> {
    use crate::tunnel::hub_directory::AgentResolution;
    match target {
        TargetSpec::Local { .. } => {
            // resolve_remote_target is only called for Remote* — the
            // top-level branch in `execute` enforces that. This arm
            // exists to make the match exhaustive; reaching it is a
            // logic bug.
            Err("internal error: resolve_remote_target called with TargetSpec::Local".to_string())
        }
        TargetSpec::RemoteByDid {
            did,
            runtime_id_hint,
        } => {
            if let Some(runtime_id) = runtime_id_hint {
                // Hint short-circuit: skip the hub round-trip. The
                // synthetic resolution is enough to dispatch — we have
                // a runtime_id and the DID; the owner principal and
                // exposure are unknown so we fill in safe defaults
                // (`Public` exposure mirrors the canChat-permissive
                // path, and `Public` principal is the "we don't know"
                // sentinel). Slice B' tightens this if needed.
                return Ok(AgentResolution {
                    runtime_id: runtime_id.clone(),
                    instance_id: String::new(),
                    agent_did: did.clone(),
                    owner_principal: Principal::Public,
                    exposure: ResolvedExposure::Public,
                });
            }
            directory.resolve_by_did(did).await.map_err(|e| match e {
                DirectoryError::NotFound => format!(
                    "remote agent not found in hub directory (did={did})"
                ),
                DirectoryError::Forbidden => format!(
                    "hub directory denied resolution (did={did}); cross-runtime a2a from \
                     anonymous callers can only reach `exposure: \"public\"` agents \
                     until peko-runtime#16 runtime-attested JWT lands"
                ),
                other => format!("hub directory lookup failed: {other}"),
            })
        }
        TargetSpec::RemoteByHandle { owner, agent_name } => directory
            .resolve_by_handle(owner, agent_name)
            .await
            .map_err(|e| match e {
                DirectoryError::NotFound => {
                    format!("remote agent not found in hub directory ({owner}/{agent_name})")
                }
                DirectoryError::Forbidden => format!(
                    "hub directory denied resolution ({owner}/{agent_name}); cross-runtime a2a from \
                     anonymous callers can only reach `exposure: \"public\"` agents \
                     until peko-runtime#16 runtime-attested JWT lands"
                ),
                other => format!("hub directory lookup failed: {other}"),
            }),
    }
}

/// Build an `A2aSendResult` JSON value from a remote-path error
/// string. Matches the shape produced on the local path's `Err`
/// arm so the LLM sees one consistent envelope.
fn remote_error_value(err: &str) -> serde_json::Value {
    let response = A2aSendResult {
        success: false,
        response: String::new(),
        session_id: String::new(),
        iterations: None,
        tool_calls: None,
        duration_ms: None,
        error: Some(err.to_string()),
    };
    serde_json::to_value(response).expect("A2aSendResult must serialize to JSON")
}

/// Hub-synthesized error response payload. PekoHub's forwarding layer
/// (pekohub#16, shipped via pekohub#17 PR — see
/// `tunnel-manager.ts::sendA2AErrorResponse`) injects this shape
/// into the `AgentToAgentResponse.payload` when it can't deliver the
/// request or the target never replies within the TTL.
///
/// The runtime caller decodes this first (before the regular
/// `A2aSendResult` shape) and surfaces the structured `code` + `message`
/// to the LLM. The `code` is one of: `target_not_found`,
/// `target_offline`, `forbidden`, `timeout`, `internal_error`.
///
/// `Serialize` is also derived so the target runtime's inbound
/// dispatcher (Slice C, see `dispatcher.rs::send_hub_error`) can
/// produce a `HubA2AErrorResponse` in the rare case it has to
/// reject an inbound request — keeping the same wire shape on both
/// sides simplifies the caller's decode.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HubA2AErrorResponse {
    pub kind: String,
    pub code: String,
    pub message: String,
}

/// Local-path `MessageResult` → `A2aSendResult` JSON value. Slice B
/// factors this out of `execute()` so both `execute_local` (Slice A
/// behavior) and any future code path that needs the same shape can
/// share the conversion.
fn message_result_to_a2a_value(
    result: Result<crate::agents::stateless_service::MessageResult>,
) -> serde_json::Value {
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
            serde_json::to_value(response).expect("A2aSendResult must serialize")
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
            serde_json::to_value(response).expect("A2aSendResult must serialize")
        }
    }
}

// Suppress the unused-import warning on `A2aWaitError` — it's the
// public surface of `PendingA2aResponses::register_and_wait` which
// `execute_remote` doesn't call directly (it uses
// `tokio::time::timeout` over the raw `oneshot::Receiver` so the
// pending-discard cleanup stays in one place). Re-exporting through
// this module makes the trait reachable from the unit tests below
// without a `use crate::tunnel::a2a_pending::...` mouthful.
#[allow(dead_code)]
type _UnusedA2aWaitError = A2aWaitError;

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
    /// variants — the defense-in-depth check that `build_request`
    /// relies on. After Slice B, the production path branches on
    /// `is_remote()` BEFORE calling `build_request`, so this error
    /// is "internal bug" territory; the test pins the bug-trip
    /// message so a future refactor that accidentally routes remote
    /// targets through `build_request` gets a loud, descriptive
    /// failure.
    #[test]
    fn test_resolve_local_target_rejects_remote_with_internal_bug_pointer() {
        let did_target = TargetSpec::RemoteByDid {
            did: "did:peko:agent:remote-xyz".to_string(),
            runtime_id_hint: None,
        };
        let err = resolve_local_target(&did_target)
            .expect_err("RemoteByDid must short-circuit at the local-only seam");
        let msg = err.to_string();
        assert!(
            msg.contains("internal bug"),
            "error must name the bug condition; got: {msg}"
        );
        assert!(
            msg.contains("execute_remote"),
            "error must point at the right code path (Slice B's execute_remote); got: {msg}"
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
            .expect_err("RemoteByHandle must short-circuit at the local-only seam");
        let msg = err.to_string();
        assert!(
            msg.contains("execute_remote"),
            "RemoteByHandle error must also point at execute_remote; got: {msg}"
        );
    }

    // -- Issue #29 (Slice B): execute_remote ----------------------------

    /// `HubA2AErrorResponse` decodes from the exact JSON shape
    /// pekohub's `sendA2AErrorResponse` synthesizes (see
    /// `tunnel-manager.ts::sendA2AErrorResponse` in pekohub#17).
    /// Catches the case where a future pekohub rename drops a field
    /// and the runtime caller starts mis-decoding hub-synthesized
    /// errors as "response payload could not be decoded".
    #[test]
    fn test_hub_a2a_error_response_decodes_pekohub_shape() {
        let body = r#"{
            "kind": "error",
            "code": "target_not_found",
            "message": "no instance with agent_did = did:peko:agent:nope"
        }"#;
        let decoded: HubA2AErrorResponse = serde_json::from_str(body).unwrap();
        assert_eq!(decoded.kind, "error");
        assert_eq!(decoded.code, "target_not_found");
        assert!(decoded.message.contains("no instance with agent_did"));
    }

    /// Build a real `Arc<StatelessAgentService>` for the tool. The
    /// remote dispatch path never invokes the service (the `Local`
    /// branch is what calls `agent_service.execute_message`), but the
    /// tool's `agent_service` field needs a valid `Arc` for
    /// construction. Uses the same TempDir pattern as the rest of
    /// `agent::stateless_service` tests.
    async fn build_test_service() -> Arc<StatelessAgentService> {
        use crate::agents::stateless_service::StatelessAgentService;
        use crate::common::paths::PathResolver;
        use crate::common::services::config_authority::ConfigAuthorityImpl;

        let temp_dir = tempfile::TempDir::new().expect("tempdir");
        let path_resolver = PathResolver::with_dirs(
            temp_dir.path().join("config"),
            temp_dir.path().join("data"),
            temp_dir.path().join("cache"),
        );
        let config_service = Arc::new(ConfigAuthorityImpl::new(path_resolver.clone()));
        Arc::new(
            StatelessAgentService::new(config_service, path_resolver)
                .await
                .expect("test StatelessAgentService must construct"),
        )
    }

    /// Build a `CrossRuntimeA2aCtx` for tests. Returns the ctx, the
    /// tunnel `mpsc::UnboundedReceiver` (so the test can inspect what
    /// the tool sent), and the `FakeAgentDirectory` (so the test can
    /// register the responses the tool should consume).
    fn build_test_ctx(
        service_timeout: Duration,
    ) -> (
        Arc<CrossRuntimeA2aCtx>,
        tokio::sync::mpsc::UnboundedReceiver<TunnelMessage>,
        std::sync::Arc<crate::tunnel::hub_directory::FakeAgentDirectory>,
    ) {
        use crate::identity::keys::KeyPair;
        use crate::tunnel::hub_directory::FakeAgentDirectory;
        use crate::tunnel::PendingA2aResponses;

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let tunnel = Arc::new(RwLock::new(Some(TunnelHandle::new(tx)))); // #[cfg(test)] ctor
        let fake_dir = std::sync::Arc::new(FakeAgentDirectory::new());
        let pending = std::sync::Arc::new(PendingA2aResponses::new());
        let kp = KeyPair::generate();
        let ctx = Arc::new(CrossRuntimeA2aCtx {
            directory: fake_dir.clone(),
            pending: pending.clone(),
            signing_key: Arc::new(kp.signing_key),
            caller_runtime_id: "did:key:zCallerRuntime".to_string(),
            tunnel,
            response_timeout: service_timeout,
        });
        (ctx, rx, fake_dir)
    }

    /// Sample `AgentResolution` for the happy-path tests. Mirrors
    /// the JSON shape the pekohub directory returns.
    fn sample_remote_resolution() -> crate::tunnel::hub_directory::AgentResolution {
        use crate::tunnel::hub_directory::{AgentResolution, ResolvedExposure};
        AgentResolution {
            runtime_id: "did:key:zTargetRuntime".to_string(),
            instance_id: "inst-target-123".to_string(),
            agent_did: "did:peko:agent:target-keyhash".to_string(),
            owner_principal: Principal::User("alice".to_string()),
            exposure: ResolvedExposure::Public,
        }
    }

    /// Drain a single `AgentToAgentRequest` from the tunnel send sink
    /// and assert on its wire shape. Returns the parsed envelope.
    fn assert_one_request(
        rx: &mut tokio::sync::mpsc::UnboundedReceiver<TunnelMessage>,
    ) -> TunnelMessage {
        let msg = rx.try_recv().expect("expected one request on the tunnel");
        // Each test sends exactly one request.
        assert!(
            rx.try_recv().is_err(),
            "expected exactly one request, got more"
        );
        msg
    }

    /// The cross-runtime path requires the `cross_runtime` ctx to be
    /// set. Without it, `execute_remote` errors with a clear
    /// "not configured" message rather than panicking.
    #[tokio::test]
    async fn test_execute_remote_without_ctx_errors_cleanly() {
        let service = build_test_service().await;
        let tool = A2aSendTool::new(service).with_caller_did(
            "caller",
            "did:peko:agent:caller-keyhash",
        );
        // Intentionally NOT calling `with_cross_runtime`.
        let args = A2aSendArgs {
            target_agent: "ignored".to_string(),
            target: Some(TargetSpec::RemoteByDid {
                did: "did:peko:agent:remote".to_string(),
                runtime_id_hint: None,
            }),
            message: "hi".to_string(),
            session_id: None,
            team: None,
        };
        let value = tool
            .execute(serde_json::to_value(args).unwrap())
            .await
            .expect("execute must not panic; returns an A2aSendResult error envelope");
        let result: A2aSendResult =
            serde_json::from_value(value).expect("execute must return an A2aSendResult");
        assert!(!result.success);
        let err = result.error.expect("error message must be set");
        assert!(
            err.contains("cross-runtime dispatch is not configured"),
            "error must name the condition; got: {err}"
        );
    }

    /// The cross-runtime path requires a `caller_agent_did` so the
    /// target runtime can attribute the receiving session under
    /// `Principal::Agent(<caller_did>)` (issue #28). Without a DID,
    /// `execute_remote` errors rather than dispatching a name-only
    /// attribution that would be ambiguous across runtimes.
    #[tokio::test]
    async fn test_execute_remote_without_caller_did_errors_cleanly() {
        let service = build_test_service().await;
        let (ctx, _rx, _dir) = build_test_ctx(Duration::from_secs(1));
        let tool = A2aSendTool::new(service)
            .with_caller("caller-name-only")
            .with_cross_runtime(ctx);
        let args = A2aSendArgs {
            target_agent: "ignored".to_string(),
            target: Some(TargetSpec::RemoteByDid {
                did: "did:peko:agent:remote".to_string(),
                runtime_id_hint: None,
            }),
            message: "hi".to_string(),
            session_id: None,
            team: None,
        };
        let value = tool
            .execute(serde_json::to_value(args).unwrap())
            .await
            .expect("execute must not panic");
        let result: A2aSendResult = serde_json::from_value(value).unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(
            err.contains("requires the caller agent's DID"),
            "error must name the condition; got: {err}"
        );
    }

    /// Directory miss (404) is surfaced as a structured
    /// `A2aSendResult` with the hub's `NotFound` error message
    /// rather than a panic or a hang.
    #[tokio::test]
    async fn test_execute_remote_directory_not_found_surfaces_structured_error() {
        use crate::tunnel::hub_directory::{DirectoryErrorKind, FakeAgentDirectory};

        let service = build_test_service().await;
        let (ctx, _rx, dir) = build_test_ctx(Duration::from_secs(1));
        dir.register_did_err(
            "did:peko:agent:unknown",
            DirectoryErrorKind::NotFound,
        );
        let tool = A2aSendTool::new(service)
            .with_caller_did("caller", "did:peko:agent:caller-keyhash")
            .with_cross_runtime(ctx);
        let args = A2aSendArgs {
            target_agent: "ignored".to_string(),
            target: Some(TargetSpec::RemoteByDid {
                did: "did:peko:agent:unknown".to_string(),
                runtime_id_hint: None,
            }),
            message: "hi".to_string(),
            session_id: None,
            team: None,
        };
        let value = tool.execute(serde_json::to_value(args).unwrap()).await.unwrap();
        let result: A2aSendResult = serde_json::from_value(value).unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(
            err.contains("not found"),
            "error must name the directory miss; got: {err}"
        );
        assert!(
            err.contains("did:peko:agent:unknown"),
            "error must surface the target DID; got: {err}"
        );
    }

    /// Directory denial (403) is surfaced with a clear explanation
    /// pointing at the runtime-attested-JWT follow-up
    /// (peko-runtime#16) so a future caller knows why a private
    /// target can't be resolved from this runtime.
    #[tokio::test]
    async fn test_execute_remote_directory_forbidden_surfaces_acl_message() {
        use crate::tunnel::hub_directory::DirectoryErrorKind;

        let service = build_test_service().await;
        let (ctx, _rx, dir) = build_test_ctx(Duration::from_secs(1));
        // The target is a RemoteByHandle; register Forbidden against
        // the (owner, agent_name) tuple (FakeAgentDirectory's
        // separate maps for did vs handle).
        dir.register_handle_err(
            "alice",
            "private-agent",
            DirectoryErrorKind::Forbidden,
        );
        let tool = A2aSendTool::new(service)
            .with_caller_did("caller", "did:peko:agent:caller-keyhash")
            .with_cross_runtime(ctx);
        let args = A2aSendArgs {
            target_agent: "ignored".to_string(),
            target: Some(TargetSpec::RemoteByHandle {
                owner: "alice".to_string(),
                agent_name: "private-agent".to_string(),
            }),
            message: "hi".to_string(),
            session_id: None,
            team: None,
        };
        let value = tool.execute(serde_json::to_value(args).unwrap()).await.unwrap();
        let result: A2aSendResult = serde_json::from_value(value).unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(
            err.contains("denied resolution"),
            "error must name the ACL condition; got: {err}"
        );
        assert!(
            err.contains("peko-runtime#16"),
            "error must point at the runtime-attested-JWT follow-up; got: {err}"
        );
    }

    /// Happy path: directory hit, target runtime simulates
    /// dispatching the request, sends back an `A2aSendResult` over
    /// the pending registry. The caller's `execute` returns the
    /// decoded result.
    ///
    /// The "simulated dispatcher" is a spawned task that drains the
    /// tunnel send sink for the `AgentToAgentRequest` and immediately
    /// fires the matching `AgentToAgentResponse` through the pending
    /// registry — exactly what Slice B' (dispatcher AgentToAgentResponse
    /// arm) will do in production.
    #[tokio::test]
    async fn test_execute_remote_happy_path_returns_target_result() {
        let service = build_test_service().await;
        let (ctx, mut rx, dir) = build_test_ctx(Duration::from_secs(1));
        dir.register_did("did:peko:agent:target-keyhash", sample_remote_resolution());
        let target_response = A2aSendResult {
            success: true,
            response: "Reviewed — looks good".to_string(),
            session_id: "agent:analyzer:session:abc".to_string(),
            iterations: Some(2),
            tool_calls: None,
            duration_ms: Some(1500),
            error: None,
        };
        let target_response_bytes = serde_json::to_vec(&target_response).unwrap();

        // Simulated dispatcher: receive the request, fire the response
        // back via the pending registry. The dispatcher's production
        // counterpart (Slice B') is what does this; here we exercise
        // the caller's contract.
        let pending = ctx.pending.clone();
        let dispatcher_join = tokio::spawn(async move {
            let msg = rx.recv().await.expect("tunnel sink must see the request");
            let TunnelMessage::AgentToAgentRequest { request_id, .. } = msg else {
                panic!("dispatcher: expected AgentToAgentRequest, got: {msg:?}");
            };
            // Tiny yield so the caller's `tokio::time::timeout(rx)` is
            // already awaiting on the oneshot before we complete.
            tokio::task::yield_now().await;
            pending.complete(&request_id, target_response_bytes);
        });

        let tool = A2aSendTool::new(service)
            .with_caller_did("caller", "did:peko:agent:caller-keyhash")
            .with_cross_runtime(ctx);
        let args = A2aSendArgs {
            target_agent: "ignored".to_string(),
            target: Some(TargetSpec::RemoteByDid {
                did: "did:peko:agent:target-keyhash".to_string(),
                runtime_id_hint: None,
            }),
            message: "review this PR".to_string(),
            session_id: None,
            team: None,
        };
        let value = tool.execute(serde_json::to_value(args).unwrap()).await.unwrap();
        let result: A2aSendResult = serde_json::from_value(value).unwrap();
        assert!(result.success, "expected success; got error: {:?}", result.error);
        assert_eq!(result.response, "Reviewed — looks good");
        assert_eq!(result.session_id, "agent:analyzer:session:abc");
        assert_eq!(result.iterations, Some(2));

        dispatcher_join.await.expect("simulated dispatcher must not panic");
    }

    /// When pekohub synthesizes a structured error response (target
    /// offline, ACL denied, etc.) the caller decodes the
    /// `HubA2AErrorResponse` shape and surfaces the message verbatim
    /// — NOT a generic "could not decode" error. Catches the
    /// failure mode where the dual-shape decoder regresses to only
    /// trying `A2aSendResult`.
    #[tokio::test]
    async fn test_execute_remote_decodes_hub_synthesized_error() {
        let service = build_test_service().await;
        let (ctx, mut rx, dir) = build_test_ctx(Duration::from_secs(1));
        dir.register_did(
            "did:peko:agent:target-keyhash",
            sample_remote_resolution(),
        );

        let hub_error = serde_json::to_vec(&serde_json::json!({
            "kind": "error",
            "code": "target_offline",
            "message": "no tunnel for runtime did:key:zTargetRuntime",
        }))
        .unwrap();

        let pending = ctx.pending.clone();
        let dispatcher_join = tokio::spawn(async move {
            let msg = rx.recv().await.unwrap();
            let TunnelMessage::AgentToAgentRequest { request_id, .. } = msg else {
                panic!("dispatcher: expected AgentToAgentRequest, got: {msg:?}");
            };
            tokio::task::yield_now().await;
            pending.complete(&request_id, hub_error);
        });

        let tool = A2aSendTool::new(service)
            .with_caller_did("caller", "did:peko:agent:caller-keyhash")
            .with_cross_runtime(ctx);
        let args = A2aSendArgs {
            target_agent: "ignored".to_string(),
            target: Some(TargetSpec::RemoteByDid {
                did: "did:peko:agent:target-keyhash".to_string(),
                runtime_id_hint: None,
            }),
            message: "hi".to_string(),
            session_id: None,
            team: None,
        };
        let value = tool.execute(serde_json::to_value(args).unwrap()).await.unwrap();
        let result: A2aSendResult = serde_json::from_value(value).unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(
            err.contains("target_offline"),
            "error must include the hub code; got: {err}"
        );
        assert!(
            err.contains("no tunnel for runtime"),
            "error must include the hub message; got: {err}"
        );
        assert!(
            !err.contains("could not be decoded"),
            "error must not be the 'decode failure' fallback when the hub synthesized an error; got: {err}"
        );

        dispatcher_join.await.unwrap();
    }

    /// Response timeout (target never replies) surfaces as a clear
    /// error with the request_id and target runtime_id in the
    /// message, not a generic "execute timed out" string.
    #[tokio::test]
    async fn test_execute_remote_response_timeout_surfaces_structured_error() {
        let service = build_test_service().await;
        // 50ms timeout so the test runs in <100ms.
        let (ctx, _rx, dir) = build_test_ctx(Duration::from_millis(50));
        dir.register_did("did:peko:agent:target-keyhash", sample_remote_resolution());

        let tool = A2aSendTool::new(service)
            .with_caller_did("caller", "did:peko:agent:caller-keyhash")
            .with_cross_runtime(ctx);
        let args = A2aSendArgs {
            target_agent: "ignored".to_string(),
            target: Some(TargetSpec::RemoteByDid {
                did: "did:peko:agent:target-keyhash".to_string(),
                runtime_id_hint: None,
            }),
            message: "hi".to_string(),
            session_id: None,
            team: None,
        };
        let start = std::time::Instant::now();
        let value = tool.execute(serde_json::to_value(args).unwrap()).await.unwrap();
        let elapsed = start.elapsed();
        let result: A2aSendResult = serde_json::from_value(value).unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(
            err.contains("timed out"),
            "error must name the timeout; got: {err}"
        );
        assert!(
            err.contains("did:key:zTargetRuntime"),
            "error must include the target runtime_id; got: {err}"
        );
        assert!(
            elapsed < Duration::from_millis(500),
            "timeout must surface near the configured 50ms; got {elapsed:?}"
        );
    }

    /// When the tunnel slot is empty (e.g. daemon just started and
    /// the WebSocket hasn't connected yet, or the most recent
    /// reconnect attempt is in backoff), the outbound path errors
    /// with a "tunnel not connected" message rather than blocking
    /// on a non-existent handle.
    #[tokio::test]
    async fn test_execute_remote_tunnel_not_connected_errors_cleanly() {
        let service = build_test_service().await;
        let (ctx, _rx, dir) = build_test_ctx(Duration::from_secs(1));
        dir.register_did("did:peko:agent:target-keyhash", sample_remote_resolution());
        // Replace the tunnel slot with an empty one to simulate a
        // disconnected tunnel. (build_test_ctx's default has Some.)
        *ctx.tunnel.write().await = None;

        let tool = A2aSendTool::new(service)
            .with_caller_did("caller", "did:peko:agent:caller-keyhash")
            .with_cross_runtime(ctx);
        let args = A2aSendArgs {
            target_agent: "ignored".to_string(),
            target: Some(TargetSpec::RemoteByDid {
                did: "did:peko:agent:target-keyhash".to_string(),
                runtime_id_hint: None,
            }),
            message: "hi".to_string(),
            session_id: None,
            team: None,
        };
        let value = tool.execute(serde_json::to_value(args).unwrap()).await.unwrap();
        let result: A2aSendResult = serde_json::from_value(value).unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(
            err.contains("tunnel is not currently connected"),
            "error must name the condition; got: {err}"
        );
    }

    /// `runtime_id_hint` on `RemoteByDid` short-circuits the hub
    /// lookup (issue #29 acceptance criterion). The directory
    /// receives no calls; the dispatch uses the hint directly.
    #[tokio::test]
    async fn test_execute_remote_uses_runtime_id_hint_without_directory_lookup() {
        let service = build_test_service().await;
        let (ctx, mut rx, dir) = build_test_ctx(Duration::from_secs(1));
        // Register a 404 for the DID — if the path looked it up,
        // we'd get NotFound rather than the happy-path result.
        dir.register_did_err(
            "did:peko:agent:target-keyhash",
            crate::tunnel::hub_directory::DirectoryErrorKind::NotFound,
        );

        let target_response = A2aSendResult {
            success: true,
            response: "ok from hint path".to_string(),
            session_id: "agent:target:session:hint".to_string(),
            iterations: Some(1),
            tool_calls: None,
            duration_ms: None,
            error: None,
        };
        let target_response_bytes = serde_json::to_vec(&target_response).unwrap();
        let pending = ctx.pending.clone();
        let dispatcher_join = tokio::spawn(async move {
            let msg = rx.recv().await.unwrap();
            let TunnelMessage::AgentToAgentRequest { request_id, .. } = msg else {
                panic!("dispatcher: expected AgentToAgentRequest, got: {msg:?}");
            };
            tokio::task::yield_now().await;
            pending.complete(&request_id, target_response_bytes);
        });

        let tool = A2aSendTool::new(service)
            .with_caller_did("caller", "did:peko:agent:caller-keyhash")
            .with_cross_runtime(ctx);
        let args = A2aSendArgs {
            target_agent: "ignored".to_string(),
            target: Some(TargetSpec::RemoteByDid {
                did: "did:peko:agent:target-keyhash".to_string(),
                runtime_id_hint: Some("did:key:zHintedTarget".to_string()),
            }),
            message: "hi".to_string(),
            session_id: None,
            team: None,
        };
        let value = tool.execute(serde_json::to_value(args).unwrap()).await.unwrap();
        let result: A2aSendResult = serde_json::from_value(value).unwrap();
        assert!(result.success);
        assert_eq!(result.response, "ok from hint path");
        assert_eq!(result.session_id, "agent:target:session:hint");
        dispatcher_join.await.unwrap();
    }
}
