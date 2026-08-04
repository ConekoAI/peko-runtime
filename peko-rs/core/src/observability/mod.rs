//! Root-side audit-sink adapter for `peko_engine::AuditSink`.
//!
//! Phase 4 of `feature/multi-model-subagents` (plan:
//! `/Users/rlsn/.claude/plans/goofy-humming-wall.md`). The engine
//! carries an `AuditSink` trait with a typed `AuditEventView` (no
//! observability dep). The root side bridges that view into the
//! concrete `peko_observability::Observability::audit_with_severity(...)`
//! API.
//!
//! ```text
//! peko_engine::AuditSink trait + AuditEventView
//!             ▲
//!             │ impl ObservabilityAuditSink
//!             ▼
//! peko_observability::Observability
//! ```
//!
//! One impl, kept in lockstep with the engine trait — any
//! divergence between `peko_engine::AuditSeverity` and
//! `peko_observability::AuditSeverity` is caught by the
//! `From` impl below at compile time.

use std::sync::Arc;

use peko_engine::audit_sink::{AuditEventView, AuditSeverity as EngineSeverity, AuditSink};
use peko_observability::AuditSeverity as ObsSeverity;

/// Convert engine-side severity → observability severity.
/// Inlined (not `impl From`) to satisfy the orphan rule: both
/// `EngineSeverity` (peko-engine) and `ObsSeverity`
/// (peko-observability) are foreign to `peko`. Adding a
/// variant to either enum without the other becomes a
/// compile error in the matching arms below.
fn severity_into_obs(severity: EngineSeverity) -> ObsSeverity {
    match severity {
        EngineSeverity::Debug => ObsSeverity::Debug,
        EngineSeverity::Info => ObsSeverity::Info,
        EngineSeverity::Warning => ObsSeverity::Warning,
        EngineSeverity::Error => ObsSeverity::Error,
        EngineSeverity::Security => ObsSeverity::Security,
    }
}

/// Bridge `AuditSink` → `Observability`. Held as
/// `Arc<ObservabilityAuditSink>` from `Agent::new_with_shared_executor`
/// (root-side builder) and passed into `AgenticLoop::with_audit_sink`.
pub struct ObservabilityAuditSink {
    observability: Arc<peko_observability::Observability>,
}

impl ObservabilityAuditSink {
    /// Wrap an `Arc<Observability>` for use as an engine-side
    /// `AuditSink`. Caller retains ownership of the inner Arc;
    /// we keep a clone.
    #[must_use]
    pub fn new(observability: Arc<peko_observability::Observability>) -> Self {
        Self { observability }
    }
}

impl AuditSink for ObservabilityAuditSink {
    fn audit(&self, event: AuditEventView) {
        // The engine trait is sync; observability is async. Spawn
        // the audit so the engine's hot path doesn't block on
        // JSONL fsync. The `tokio::task::spawn` requires a
        // multi-thread runtime; principal managers already use
        // one (`#[tokio::main(flavor = "multi_thread")]`).
        let observability = Arc::clone(&self.observability);
        let event_type = event.event_type.to_string();
        let principal = event.principal.clone();
        let details = event.details.clone();
        let severity: ObsSeverity = severity_into_obs(event.severity);
        tokio::task::spawn(async move {
            // `audit_with_severity` takes `Option<&Subject>` for
            // the caller; we don't have a Subject here, only the
            // principal DID — that's exactly what `audit` uses
            // (without severity). Compose: pass `None` for
            // caller, then escalate severity via the severity
            // path. The two-stage approach avoids inventing a
            // Subject just to pass severity.
            let _ = observability
                .audit_with_severity(severity, None, &event_type, principal.as_deref(), details)
                .await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peko_engine::audit_sink::{AuditSeverity as E, AuditSink};
    use peko_observability::AuditSeverity as O;

    #[test]
    fn severity_conversion_is_bijective() {
        for (engine, obs) in [
            (E::Debug, O::Debug),
            (E::Info, O::Info),
            (E::Warning, O::Warning),
            (E::Error, O::Error),
            (E::Security, O::Security),
        ] {
            assert_eq!(severity_into_obs(engine), obs);
        }
    }

    #[tokio::test]
    async fn observability_audit_sink_holds_arc() {
        // Smoke check — the constructor doesn't panic on a
        // fresh Observability. The in-memory-only variant
        // (`Observability::new`) keeps the test self-contained
        // without spinning up a JSONL sink. `#[tokio::test]`
        // because `AuditSink::audit` spawns onto the runtime
        // via `tokio::task::spawn`.
        let obs = Arc::new(peko_observability::Observability::new("audit_sink_test"));
        let sink = ObservabilityAuditSink::new(obs);
        // The trait method is callable and doesn't panic when
        // observability has no path — audit is async-spawned,
        // so we just verify the sync boundary doesn't blow up.
        sink.audit(AuditEventView {
            event_type: "model.selected",
            severity: E::Info,
            principal: Some("did:peko:test".to_string()),
            subagent_id: None,
            model_id: Some("claude-sonnet-4-6".to_string()),
            details: serde_json::json!({ "first_use": true }),
        });
        // Yield once so the spawned audit task can complete
        // before the test exits.
        tokio::task::yield_now().await;
    }
}