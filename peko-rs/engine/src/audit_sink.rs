//! `AuditSink` — engine-facing port for emitting audit events.
//!
//! Phase 4 of `feature/multi-model-subagents` (plan:
//! `/Users/rlsn/.claude/plans/goofy-humming-wall.md`). `peko-engine`
//! does **not** depend on `peko-observability` (confirmed in
//! `peko-rs/engine/Cargo.toml`), so this module carries a typed
//! `AuditEventView` struct + a local `AuditSeverity` mirror
//! instead of importing `peko_observability::AuditEvent`. The
//! root-side adapter at
//! `peko-rs/core/src/observability/audit_sink_impl.rs` is the
//! single integration point: it converts `AuditEventView` to
//! `peko_observability::Observability::audit_with_severity(...)`.
//!
//! ## Design
//!
//! ```text
//! ┌─────────────────────┐         ┌──────────────────────────┐
//! │ AgenticLoop         │ audit() │ ObservabilityAuditSink   │
//! │ (peko-engine)       │────────▶│ (peko / root impl)       │
//! │                     │         │   Arc<Observability>     │
//! └─────────────────────┘         └────────┬─────────────────┘
//!                                            │ audit_with_severity()
//!                                            ▼
//!                                 ┌──────────────────────────┐
//!                                 │ peko_observability       │
//!                                 └──────────────────────────┘
//! ```
//!
//! Engine callers carry an `Option<Arc<dyn AuditSink>>` so tests
//! and embedded use can omit it without restructuring the loop.
//!
//! ## Event types
//!
//! Convention (lowercase + dot-separated):
//!
//! - `"model.selected"` — every successful LLM call. Severity
//!   `Warning` for the first use of a (principal, model) pair,
//!   `Info` thereafter. Powers `peko audit tail`'s first-use
//!   warning UX.
//! - Future event types can layer on the same trait without
//!   touching call sites — `peko audit tail|list` already
//!   renders arbitrary `event_type` strings (see
//!   `peko-rs/cli/src/commands/audit.rs:370-410`).

use serde_json::Value;

/// Engine-side severity mirror. `peko-engine` doesn't depend on
/// `peko-observability`, so we carry a 1:1 copy of
/// `peko_observability::AuditSeverity`. The root impl at
/// `peko-rs/core/src/observability/audit_sink_impl.rs` converts
/// via `From` / explicit mapping — keep this enum in lockstep
/// with the observability crate; any drift is a bug.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditSeverity {
    Debug,
    Info,
    Warning,
    Error,
    Security,
}

/// Engine-facing audit view. `event_type` is the canonical
/// `"verb.noun"` form (`"model.selected"`,
/// `"spawn.rejected"`, etc.); `principal` and `model_id` are
/// best-effort (some events may not have either); `details`
/// carries the structured payload as a free-form JSON value so
/// the trait doesn't need to enumerate every event shape.
#[derive(Debug, Clone)]
pub struct AuditEventView {
    /// Canonical event identifier, e.g. `"model.selected"`.
    pub event_type: &'static str,
    /// Severity — drives the audit row's icon in
    /// `peko audit tail` and any downstream alerting.
    pub severity: AuditSeverity,
    /// Principal runtime id (DID) when applicable, `None` for
    /// engine-internal events.
    pub principal: Option<String>,
    /// Subagent run id when the event is per-spawn, `None` for
    /// root-agent events.
    pub subagent_id: Option<String>,
    /// Resolved model id when the event is per-model, `None`
    /// for non-LLM events.
    pub model_id: Option<String>,
    /// Free-form JSON details (call cost, first_use flag,
    /// error message, etc.).
    pub details: Value,
}

/// Engine-facing audit sink. `Send + Sync` so the loop can hold
/// it in `Arc` and emit from `async` contexts. Implementations
/// are expected to be cheap — the engine emits one event per
/// successful LLM call, so this is hot path.
pub trait AuditSink: Send + Sync {
    /// Emit one event. Implementations should not block; long
    /// persistence paths should be `tokio::task::spawn_blocking`
    /// internally.
    fn audit(&self, event: AuditEventView);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Local sink impl that captures events for tests.
    #[derive(Default)]
    struct CapturingSink {
        events: std::sync::Mutex<Vec<AuditEventView>>,
    }

    impl AuditSink for CapturingSink {
        fn audit(&self, event: AuditEventView) {
            self.events.lock().unwrap().push(event);
        }
    }

    #[test]
    fn audit_sink_receives_events() {
        let sink = CapturingSink::default();
        sink.audit(AuditEventView {
            event_type: "model.selected",
            severity: AuditSeverity::Info,
            principal: Some("did:peko:alice".to_string()),
            subagent_id: None,
            model_id: Some("claude-sonnet-4-6".to_string()),
            details: json!({ "first_use": true }),
        });
        let events = sink.events.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "model.selected");
        assert_eq!(events[0].severity, AuditSeverity::Info);
        assert_eq!(
            events[0].model_id.as_deref(),
            Some("claude-sonnet-4-6")
        );
        assert_eq!(events[0].details["first_use"], true);
    }

    #[test]
    fn audit_severity_has_five_levels() {
        // Lockstep check with `peko_observability::AuditSeverity`.
        // Drift in either direction is a bug.
        let all = [
            AuditSeverity::Debug,
            AuditSeverity::Info,
            AuditSeverity::Warning,
            AuditSeverity::Error,
            AuditSeverity::Security,
        ];
        assert_eq!(all.len(), 5);
    }
}