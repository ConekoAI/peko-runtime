//! `audit` domain request handler (Phase 4 of the ADR-046 trust+audit
//! pivot).
//!
//! Owns the `AuditQuery` IPC variant. The handler reads from the
//! daemon's in-memory audit ring buffer and returns up to `limit`
//! matching events, newest first. Filters: `event_type_prefix` and
//! `principal` (matched against the event's `caller` if it's a
//! `Subject::Principal` whose id matches, OR against
//! `details.principal_name` for events that carry an explicit
//! principal context).
//!
//! **What this handler does NOT do:** it does not read the JSONL
//! file. The JSONL file is durable history that survives across
//! daemon restarts; queries that span multiple sessions
//! (`peko audit tail --since 24h`) read the file directly from the
//! CLI, no IPC needed. The IPC query is for "what happened in
//! this session since I started looking" — fast, no fs.

use std::sync::Arc;

use async_trait::async_trait;
use peko_auth::Subject;
use serde_json::Value;

use crate::ipc::handlers::RequestHandler;
use crate::ipc::packet::{RequestPacket, ResponsePacket};
use crate::ipc::response_sink::ResponseSink;
use crate::ipc::send_response::send_response;
use crate::ipc::server::PeerAddr;
use peko_observability::{AuditEvent, Observability};
use peko_auth::caller::CallerContext;

/// Narrow port the `audit` handler uses to reach daemon state.
///
/// `AppState` is the sole implementor. The handler needs only the
/// observability hub (for `get_audit_log`) — no principal manager,
/// no cron engine, no IPC peer credentials. Keeping the port
/// narrow makes the handler cheap to test in isolation.
pub(crate) trait AuditHost: Send + Sync {
    /// Observability hub whose ring buffer the query reads.
    fn observability(&self) -> Arc<Observability>;
}

/// `audit` domain request handler. Constructed with an
/// `Arc<dyn AuditHost>` (typically `Arc::new(app_state.clone())`
/// from the dispatcher).
pub(crate) struct AuditHandler {
    host: Arc<dyn AuditHost>,
}

impl AuditHandler {
    pub(crate) fn new(host: Arc<dyn AuditHost>) -> Self {
        Self { host }
    }
}

#[async_trait]
impl RequestHandler for AuditHandler {
    fn domain(&self) -> &'static str {
        "audit"
    }

    fn matches(&self, request: &RequestPacket) -> bool {
        matches!(request, RequestPacket::AuditQuery { .. })
    }

    async fn handle(
        &self,
        request: RequestPacket,
        _caller: &CallerContext,
        sink: &dyn ResponseSink,
        _peer: &PeerAddr,
    ) -> anyhow::Result<()> {
        let RequestPacket::AuditQuery {
            request_id,
            limit,
            event_type_prefix,
            principal,
        } = request
        else {
            unreachable!("AuditHandler::matches allowed an unhandled variant");
        };

        // Pull a slightly larger pool than the requested limit
        // because filters may exclude most entries — pulling a few
        // thousand and filtering is bounded and fast (ring buffer
        // is in-memory, max 10k entries). The CLI passes 1k by
        // default; the cap of `limit` (32-bit) is comfortably below
        // the ring buffer ceiling.
        let pool = self
            .host
            .observability()
            .get_audit_log(limit.max(1) as usize)
            .await;
        let filtered: Vec<AuditEvent> = pool
            .into_iter()
            .filter(|e| event_type_prefix.as_deref().is_none_or(|p| e.event_type.starts_with(p)))
            .filter(|e| principal.as_deref().is_none_or(|p| event_matches_principal(e, p)))
            .take(limit.max(1) as usize)
            .collect();

        let response = ResponsePacket::AuditEvents {
            request_id,
            entries: filtered,
        };
        send_response(sink, response).await
    }
}

/// Does `event` match the requested `principal` filter?
///
/// Two matching rules so a single filter string works across event
/// shapes:
/// 1. **Caller match** — if `event.caller` is `Some(Subject::Principal(id))`
///    or `Some(Subject::User(id))`, the id is compared to the filter.
///    (Subject::Public is unauthenticated; never matches a principal
///    filter — that's a feature, not a bug.)
/// 2. **Details match** — `event.details["principal_name"]` is compared.
///    This catches events that were logged with an explicit
///    principal context even when no caller subject was set
///    (e.g. cron-engine system events).
fn event_matches_principal(event: &AuditEvent, principal: &str) -> bool {
    if let Some(caller) = &event.caller {
        match caller {
            Subject::Principal(id) if id.as_str() == principal => return true,
            Subject::User(id) if id == principal => return true,
            _ => {}
        }
    }
    if let Some(Value::String(name)) = event.details.get("principal_name") {
        if name == principal {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use peko_observability::{AuditEvent, AuditSeverity};
    use serde_json::json;

    fn evt(event_type: &str, details: serde_json::Value) -> AuditEvent {
        AuditEvent {
            timestamp: chrono::Utc::now(),
            component: "test".into(),
            event_type: event_type.into(),
            agent_did: None,
            caller: None,
            details,
            severity: AuditSeverity::Info,
        }
    }

    #[test]
    fn matches_via_principal_caller() {
        let mut e = evt("cron.execute", json!({}));
        e.caller = Some(Subject::Principal("alice".into()));
        assert!(event_matches_principal(&e, "alice"));
        assert!(!event_matches_principal(&e, "bob"));
    }

    #[test]
    fn matches_via_user_caller() {
        let mut e = evt("tunnel.proxied", json!({}));
        e.caller = Some(Subject::User("user:alice".into()));
        assert!(event_matches_principal(&e, "user:alice"));
        assert!(!event_matches_principal(&e, "alice"));
    }

    #[test]
    fn matches_via_details_principal_name() {
        let e = evt("principal.config_drift", json!({"principal_name": "alice"}));
        assert!(event_matches_principal(&e, "alice"));
    }

    #[test]
    fn public_caller_does_not_match() {
        // Subject::Public is unauthenticated — a principal filter
        // must not match it; otherwise `--principal foo` would
        // return system events with no real principal.
        let mut e = evt("cron.execute", json!({}));
        e.caller = Some(Subject::Public);
        assert!(!event_matches_principal(&e, "foo"));
    }

    #[test]
    fn no_caller_and_no_details_does_not_match() {
        let e = evt("foo.bar", json!({}));
        assert!(!event_matches_principal(&e, "alice"));
    }
}
