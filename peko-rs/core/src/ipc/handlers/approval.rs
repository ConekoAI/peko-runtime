//! `approval` domain request handler (ADR-045 PR #4 step 2).
//!
//! Owns the user→daemon decision IPC variant
//! [`RequestPacket::ApprovalDecision`]. The handler routes the
//! decision through [`ApprovalEngine::decide_and_execute`], which:
//! 1. Stamps the queue with the user's choice (Approved | Denied).
//! 2. (On Grant) executes the privileged op via the engine's
//!    `ApprovalExecutionHost` port.
//! 3. Returns the per-op result to the caller.
//!
//! ## Boundary rules (F6)
//!
//! - Dependency inversion: this module defines [`ApprovalHost`]; the
//!   producer (`daemon::state::AppState`) implements it.
//! - This module must not import any other `ipc::handlers::*` module.
//!
//! ## Subject stamping
//!
//! The queue persists `by: Subject` alongside the decision timestamp.
//! The handler derives this from the caller's IPC auth context via
//! [`CallerContext::subject`], so the user can never spoof the
//! "who decided" attribution on the wire.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use tracing::{info, warn};

use crate::daemon::approval_engine::ApprovalEngine;
use crate::daemon::approval_queue::Decision;
use crate::ipc::handlers::RequestHandler;
use crate::ipc::packet::{
    ApprovalDecisionPayload, ApprovalStatusPayload, RequestPacket, ResponsePacket,
};
use crate::ipc::response_sink::ResponseSink;
use crate::ipc::send_response::send_response;
use crate::ipc::server::PeerAddr;
use peko_auth::caller::CallerContext;

/// Narrow port the `approval` handler uses to reach daemon state.
///
/// `AppState` is the sole implementor. The handler is intentionally
/// minimal — the engine already encapsulates queue + host, so this
/// trait only needs to surface the engine accessor.
pub(crate) trait ApprovalHost: Send + Sync {
    /// The lazy-init engine. `None` while `AppState::build_internal`
    /// is still wiring — the handler surfaces a clear "engine not
    /// ready" error rather than panicking on unwrap.
    fn approval_engine(&self) -> Option<Arc<ApprovalEngine>>;
}

/// `approval` domain request handler. Constructed with an
/// `Arc<dyn ApprovalHost>` (typically `Arc::new(app_state.clone())`
/// from the dispatcher).
pub(crate) struct ApprovalHandler {
    host: Arc<dyn ApprovalHost>,
}

impl ApprovalHandler {
    pub(crate) fn new(host: Arc<dyn ApprovalHost>) -> Self {
        Self { host }
    }
}

#[async_trait]
impl RequestHandler for ApprovalHandler {
    fn domain(&self) -> &'static str {
        "approval"
    }

    fn matches(&self, request: &RequestPacket) -> bool {
        matches!(request, RequestPacket::ApprovalDecision { .. })
    }

    async fn handle(
        &self,
        request: RequestPacket,
        caller: &CallerContext,
        sink: &dyn ResponseSink,
        _peer: &PeerAddr,
    ) -> anyhow::Result<()> {
        match request {
            RequestPacket::ApprovalDecision {
                request_id,
                id,
                decision,
            } => {
                let subject = caller.subject();

                let engine = match self.host.approval_engine() {
                    Some(e) => e,
                    None => {
                        let response = ResponsePacket::ApprovalError {
                            request_id,
                            id,
                            message: "approval engine not ready (daemon still initializing)"
                                .to_string(),
                        };
                        return send_response(sink, response).await;
                    }
                };

                // Convert wire payload → engine decision.
                let engine_decision = match decision {
                    ApprovalDecisionPayload::Grant => Decision::Grant,
                    ApprovalDecisionPayload::Deny { reason } => Decision::Deny { reason },
                };

                match engine
                    .decide_and_execute(id, engine_decision, subject.clone())
                    .await
                {
                    Ok(outcome) => {
                        // Audit log: every approved grant is a privileged
                        // capability change. Surface who/why at INFO.
                        info!(
                            request_id,
                            principal_id = %outcome.request.principal_id,
                            decision = if outcome.op_result.is_null() { "denied" } else { "granted" },
                            op = outcome.request.op.label(),
                            by = subject.subject_id(),
                            "approval decided"
                        );

                        let status = match &outcome.request.status {
                            crate::daemon::approval_queue::ApprovalStatus::Approved {
                                decided_at_secs,
                                by,
                            } => ApprovalStatusPayload::Approved {
                                decided_at_secs: *decided_at_secs,
                                by: subject_to_json(by),
                            },
                            crate::daemon::approval_queue::ApprovalStatus::Denied {
                                decided_at_secs,
                                by,
                                reason,
                            } => ApprovalStatusPayload::Denied {
                                decided_at_secs: *decided_at_secs,
                                by: subject_to_json(by),
                                reason: reason.clone(),
                            },
                            // `decide_with` always transitions out of
                            // Pending; this arm is unreachable in normal
                            // operation.
                            crate::daemon::approval_queue::ApprovalStatus::Pending => {
                                warn!(
                                    request_id,
                                    "decide_and_execute returned Pending status"
                                );
                                ApprovalStatusPayload::Pending
                            }
                        };

                        let response = ResponsePacket::ApprovalDecided {
                            request_id,
                            id,
                            status,
                            op_result: outcome.op_result,
                        };
                        send_response(sink, response).await
                    }
                    Err(e) => {
                        warn!(
                            request_id,
                            id = %id,
                            error = %e,
                            "approval decision failed"
                        );
                        let response = ResponsePacket::ApprovalError {
                            request_id,
                            id,
                            message: e.to_string(),
                        };
                        send_response(sink, response).await
                    }
                }
            }

            // `matches()` returned true, so the exhaustive list above
            // covers every owned variant. This arm is unreachable.
            _ => unreachable!("ApprovalHandler::matches allowed an unhandled variant"),
        }
    }
}

/// Encode a `peko_subject::Subject` into a wire-shaped JSON object.
///
/// The IPC layer doesn't depend on `peko_subject` directly — the
/// `ApprovalStatusPayload::by` field is `serde_json::Value`. We
/// reconstruct the canonical shape (`{"kind": "...", "id": "..."}` or
/// `{"kind": "public"}`) so the CLI can pretty-print it without a
/// peko-subject import on its side.
fn subject_to_json(subject: &peko_subject::Subject) -> serde_json::Value {
    match subject {
        peko_subject::Subject::User(id) => json!({ "kind": "user", "id": id }),
        peko_subject::Subject::Principal(did) => {
            json!({ "kind": "principal", "id": did.as_str() })
        }
        peko_subject::Subject::Public => json!({ "kind": "public" }),
    }
}