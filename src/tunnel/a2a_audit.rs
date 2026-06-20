//! Cross-runtime A2A audit event construction. Issue #29 Slice D.
//!
//! The `A2aSentEvent` / `A2aReceivedEvent` structs in
//! `src/session/events.rs` are the canonical audit trail for
//! cross-runtime a2a — they carry the four DIDs the issue body
//! requires (`caller_did`, `runtime_id_caller`, `target_did`,
//! `runtime_id_target`) plus the `request_id` correlation. They
//! were declared in the original ADR-023 design but no production
//! code constructs them today.
//!
//! This module fills that gap. It does NOT touch a future event
//! bus publication path (that's a follow-up). It:
//!
//! 1. Constructs `A2aSentEvent` / `A2aReceivedEvent` instances
//!    with the cross-runtime fields populated at the right moments
//!    (outbound send, inbound receive, inbound response send).
//! 2. Emits a structured `tracing::info!` log line carrying the
//!    same fields so audit consumers can see the events today
//!    (via the existing tracing pipeline) and a future event-bus
//!    publication can be slotted in without changing the call
//!    sites.
//!
//! Each public function takes the necessary fields explicitly —
//! no `&AgentConfig` or `&TunnelMessage` references — so the
//! call sites stay readable and the test surface doesn't need to
//! fake the full envelope.

use serde_json::json;
use tracing::info;

use crate::session::events::{
    A2aMessageType, A2aReceivedEvent, A2aSentEvent, EventEnvelope,
};

/// Construct an `A2aSentEvent` for the outbound side — emitted
/// after `A2aSendTool::execute_remote` has successfully sent
/// the `AgentToAgentRequest` over the tunnel. The event is the
/// call-side audit row for "agent X on runtime R1 sent a message
/// to agent Y on runtime R2".
///
/// `session_id` is the *local* session the outbound `a2a_send`
/// call originated from (correlates the audit row with the
/// caller's session log). It's not stored on the event itself
/// because `EventEnvelope` only carries `{id, ts}`; the value is
/// passed through to the `tracing::info!` line so audit consumers
/// reading the log can join the rows back together.
#[allow(clippy::too_many_arguments)]
pub fn build_a2a_sent_outbound(
    session_id: &str,
    request_id: &str,
    caller_runtime_id: &str,
    caller_agent_did: &str,
    target_runtime_id: &str,
    target_agent_did: &str,
    message: &str,
) -> A2aSentEvent {
    A2aSentEvent {
        envelope: EventEnvelope::new(),
        message_type: A2aMessageType::Task,
        // The legacy same-runtime fields are empty for the
        // cross-runtime path — the cross-runtime fields below
        // are the authoritative identifiers.
        topic: String::new(),
        to: String::new(),
        payload: json!({
            "session_id": session_id,
            "message_preview": preview(message),
        }),
        caller_did: Some(caller_agent_did.to_string()),
        runtime_id_caller: Some(caller_runtime_id.to_string()),
        target_did: Some(target_agent_did.to_string()),
        runtime_id_target: Some(target_runtime_id.to_string()),
        request_id: Some(request_id.to_string()),
    }
}

/// Construct an `A2aReceivedEvent` for the inbound side — emitted
/// in `dispatcher::handle_inbound_agent_to_agent_request` after
/// the inbound `AgentToAgentRequest` has been received. The event
/// is the receiver-side audit row for "agent Y on runtime R2
/// received a message from agent X on runtime R1".
#[allow(clippy::too_many_arguments)]
pub fn build_a2a_received_inbound(
    session_id: &str,
    request_id: &str,
    caller_runtime_id: &str,
    caller_agent_did: &str,
    target_runtime_id: &str,
    target_agent_did: &str,
    message: &str,
) -> A2aReceivedEvent {
    A2aReceivedEvent {
        envelope: EventEnvelope::new(),
        message_type: A2aMessageType::Task,
        topic: String::new(),
        from: String::new(),
        payload: json!({
            "session_id": session_id,
            "message_preview": preview(message),
        }),
        caller_did: Some(caller_agent_did.to_string()),
        runtime_id_caller: Some(caller_runtime_id.to_string()),
        target_did: Some(target_agent_did.to_string()),
        runtime_id_target: Some(target_runtime_id.to_string()),
        request_id: Some(request_id.to_string()),
    }
}

/// Construct an `A2aSentEvent` for the response side — emitted in
/// `dispatcher::handle_inbound_agent_to_agent_request` after the
/// target runtime has dispatched and is sending back an
/// `AgentToAgentResponse` to the caller. Symmetric to
/// `build_a2a_sent_outbound` but on the *receiving* runtime, and
/// with the local agent as the "caller" of the response.
#[allow(clippy::too_many_arguments)]
pub fn build_a2a_sent_response(
    session_id: &str,
    request_id: &str,
    caller_runtime_id: &str,
    caller_agent_did: &str,
    target_runtime_id: &str,
    target_agent_did: &str,
    response_preview: &str,
) -> A2aSentEvent {
    A2aSentEvent {
        envelope: EventEnvelope::new(),
        message_type: A2aMessageType::TaskResult,
        topic: String::new(),
        to: String::new(),
        payload: json!({
            "session_id": session_id,
            "response_preview": preview(response_preview),
        }),
        // The local agent is the "caller" of the response
        // (it's initiating the response send), and the original
        // caller is the "target" of the response.
        caller_did: Some(target_agent_did.to_string()),
        runtime_id_caller: Some(target_runtime_id.to_string()),
        target_did: Some(caller_agent_did.to_string()),
        runtime_id_target: Some(caller_runtime_id.to_string()),
        request_id: Some(request_id.to_string()),
    }
}

/// Construct an `A2aReceivedEvent` for the response side — emitted
/// in `A2aSendTool::execute_remote` after the
/// `AgentToAgentResponse` has been received. Symmetric to
/// `build_a2a_received_inbound` but on the *calling* runtime.
#[allow(clippy::too_many_arguments)]
pub fn build_a2a_received_response(
    session_id: &str,
    request_id: &str,
    caller_runtime_id: &str,
    caller_agent_did: &str,
    target_runtime_id: &str,
    target_agent_did: &str,
    response_preview: &str,
) -> A2aReceivedEvent {
    A2aReceivedEvent {
        envelope: EventEnvelope::new(),
        message_type: A2aMessageType::TaskResult,
        topic: String::new(),
        from: String::new(),
        payload: json!({
            "session_id": session_id,
            "response_preview": preview(response_preview),
        }),
        // The local agent is the "target" of the response
        // (the response is coming back to it). The remote
        // agent is the "caller" (it initiated the response).
        caller_did: Some(target_agent_did.to_string()),
        runtime_id_caller: Some(target_runtime_id.to_string()),
        target_did: Some(caller_agent_did.to_string()),
        runtime_id_target: Some(caller_runtime_id.to_string()),
        request_id: Some(request_id.to_string()),
    }
}

/// Emit a structured `tracing::info!` log line carrying the same
/// fields the event will eventually publish through the event
/// bus. Today's audit consumers read the log; tomorrow's read
/// the bus. Same shape both ways.
pub fn emit_a2a_sent(event: &A2aSentEvent) {
    let session_id = event
        .payload
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    info!(
        event_kind = "a2a.sent",
        session_id = %session_id,
        caller_did = ?event.caller_did,
        runtime_id_caller = ?event.runtime_id_caller,
        target_did = ?event.target_did,
        runtime_id_target = ?event.runtime_id_target,
        request_id = ?event.request_id,
        message_type = ?event.message_type,
        "a2a message sent"
    );
}

pub fn emit_a2a_received(event: &A2aReceivedEvent) {
    let session_id = event
        .payload
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    info!(
        event_kind = "a2a.received",
        session_id = %session_id,
        caller_did = ?event.caller_did,
        runtime_id_caller = ?event.runtime_id_caller,
        target_did = ?event.target_did,
        runtime_id_target = ?event.runtime_id_target,
        request_id = ?event.request_id,
        message_type = ?event.message_type,
        "a2a message received"
    );
}

/// Truncate a free-form message for the audit-log payload. Audit
/// logs are meant to be searchable, not verbatim-message
/// repositories; a 200-char preview keeps the log line scannable
/// while still capturing the first hint of the message.
fn preview(s: &str) -> String {
    const MAX: usize = 200;
    if s.len() <= MAX {
        s.to_string()
    } else {
        let mut end = MAX;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &s[..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `build_a2a_sent_outbound` populates the four
    /// cross-runtime fields plus `request_id`, and leaves the
    /// legacy `topic`/`to` fields empty (back-compat with the
    /// same-runtime path that uses them).
    #[test]
    fn test_build_a2a_sent_outbound_populates_cross_runtime_fields() {
        let event = build_a2a_sent_outbound(
            "session-1",
            "req-1",
            "did:key:zCallerRuntime",
            "did:peko:agent:caller",
            "did:key:zTargetRuntime",
            "did:peko:agent:target",
            "review this PR",
        );
        assert_eq!(event.caller_did.as_deref(), Some("did:peko:agent:caller"));
        assert_eq!(event.runtime_id_caller.as_deref(), Some("did:key:zCallerRuntime"));
        assert_eq!(event.target_did.as_deref(), Some("did:peko:agent:target"));
        assert_eq!(event.runtime_id_target.as_deref(), Some("did:key:zTargetRuntime"));
        assert_eq!(event.request_id.as_deref(), Some("req-1"));
        assert!(event.topic.is_empty());
        assert!(event.to.is_empty());
        // Message preview is captured (truncated if too long).
        let payload_str = event.payload.to_string();
        assert!(payload_str.contains("review this PR"));
        assert!(payload_str.contains("session-1"));
    }

    /// `build_a2a_received_inbound` mirrors the sent shape with
    /// the "from"/"topic" legacy fields empty.
    #[test]
    fn test_build_a2a_received_inbound_populates_cross_runtime_fields() {
        let event = build_a2a_received_inbound(
            "session-2",
            "req-2",
            "did:key:zCallerRuntime",
            "did:peko:agent:caller",
            "did:key:zTargetRuntime",
            "did:peko:agent:target",
            "hi",
        );
        assert_eq!(event.caller_did.as_deref(), Some("did:peko:agent:caller"));
        assert_eq!(event.target_did.as_deref(), Some("did:peko:agent:target"));
        assert_eq!(event.request_id.as_deref(), Some("req-2"));
        assert!(event.topic.is_empty());
        assert!(event.from.is_empty());
    }

    /// Response-side events swap the "caller" and "target" fields
    /// — from the local runtime's perspective, the local agent
    /// is the *caller* of the response, and the original caller
    /// is the *target*. The DID forms stay consistent (caller_did
    /// = who initiated the local op), but the target_* fields
    /// flip.
    #[test]
    fn test_response_event_swaps_caller_and_target() {
        let sent = build_a2a_sent_response(
            "session-3",
            "req-3",
            "did:key:zCallerRuntime", // original caller
            "did:peko:agent:caller",
            "did:key:zTargetRuntime", // local runtime (the responder)
            "did:peko:agent:target",
            "ok",
        );
        // The local agent is the "caller" of the response (it's
        // initiating the response send).
        assert_eq!(sent.caller_did.as_deref(), Some("did:peko:agent:target"));
        assert_eq!(sent.runtime_id_caller.as_deref(), Some("did:key:zTargetRuntime"));
        // The original caller is the "target" of the response.
        assert_eq!(sent.target_did.as_deref(), Some("did:peko:agent:caller"));
        assert_eq!(sent.runtime_id_target.as_deref(), Some("did:key:zCallerRuntime"));
    }

    /// Long messages are truncated in the audit payload with an
    /// ellipsis. Keeps the log line scannable.
    #[test]
    fn test_message_preview_truncates() {
        let long = "x".repeat(500);
        let event = build_a2a_sent_outbound(
            "s",
            "r",
            "did:key:a",
            "did:peko:agent:a",
            "did:key:b",
            "did:peko:agent:b",
            &long,
        );
        let payload_str = event.payload.to_string();
        assert!(payload_str.contains("…"), "long message must be truncated with ellipsis");
        assert!(
            !payload_str.contains(&"x".repeat(300)),
            "truncated preview must not include the full message body"
        );
    }

    /// Short messages fit through unchanged.
    #[test]
    fn test_message_preview_passes_short_through() {
        let event = build_a2a_sent_outbound(
            "s",
            "r",
            "did:key:a",
            "did:peko:agent:a",
            "did:key:b",
            "did:peko:agent:b",
            "short",
        );
        let payload_str = event.payload.to_string();
        assert!(payload_str.contains("short"));
        assert!(!payload_str.contains("…"));
    }

    /// The emit functions don't panic on any of the event
    /// shapes. Catches a regression where a `?` or a missing
    /// field sneaks into the construction.
    #[test]
    fn test_emit_functions_dont_panic() {
        let sent = build_a2a_sent_outbound("s", "r", "a", "a", "b", "b", "hi");
        emit_a2a_sent(&sent);
        let recv = build_a2a_received_inbound("s", "r", "a", "a", "b", "b", "hi");
        emit_a2a_received(&recv);
    }
}
