//! Observability - Audit, metrics, and tracing for Pekobot
//!
//! Provides visibility into:
//! - What agents are doing (audit log)
//! - Performance metrics (counters, timers)
//! - Execution traces (request flows)

pub mod async_tool_metrics;
pub mod audit;
pub mod metrics;
pub mod performance;
pub mod tracer;

pub use async_tool_metrics::{
    AsyncToolExecutionMetrics, AsyncToolMetricsCollector, TaskExecutionMetrics, ToolSpecificMetrics,
};
pub use audit::{AuditEvent, AuditLogger, AuditSeverity};
pub use metrics::MetricsCollector;
pub use performance::{
    start_timer, stop_timer, LatencyStats, MetricsExport, PerformanceGuard, PerformanceMetrics,
    GLOBAL_METRICS,
};
pub use tracer::{TraceSpan, Tracer};

use crate::auth::Principal;
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Unified observability hub
pub struct Observability {
    /// Audit logger
    audit: Arc<RwLock<AuditLogger>>,
    /// Metrics collector
    metrics: Arc<RwLock<MetricsCollector>>,
    /// Distributed tracer
    tracer: Arc<RwLock<Tracer>>,
    /// Component name
    component: String,
}

impl Observability {
    /// Create new observability hub
    pub fn new(component: impl Into<String>) -> Self {
        let component = component.into();
        Self {
            audit: Arc::new(RwLock::new(AuditLogger::new())),
            metrics: Arc::new(RwLock::new(MetricsCollector::new())),
            tracer: Arc::new(RwLock::new(Tracer::new())),
            component,
        }
    }

    /// Log an audit event
    pub async fn audit(
        &self,
        event_type: &str,
        agent_did: Option<&str>,
        details: serde_json::Value,
    ) -> Result<()> {
        self.log_audit(event_type, agent_did, None, details, AuditSeverity::Info)
            .await
    }

    /// Log an audit event tagged with the resolved caller identity
    /// (issues #17 + #26). Prefer this over `audit` on any event that
    /// flows through a request path so the audit trail is attributable
    /// to a real subject. The caller is a typed `Principal` (ADR-039)
    /// — `User` / `Agent` / `Team` / `Public` — so per-user, per-key,
    /// and per-agent audit queries can index on the kind tag instead of
    /// string-parsing the legacy `user:{sub}` convention.
    pub async fn audit_with_caller(
        &self,
        caller: Option<&Principal>,
        event_type: &str,
        agent_did: Option<&str>,
        details: serde_json::Value,
    ) -> Result<()> {
        self.log_audit(event_type, agent_did, caller, details, AuditSeverity::Info)
            .await
    }

    /// Log security-sensitive event
    pub async fn audit_security(
        &self,
        event_type: &str,
        agent_did: Option<&str>,
        details: serde_json::Value,
    ) -> Result<()> {
        self.log_audit(event_type, agent_did, None, details, AuditSeverity::Security)
            .await
    }

    /// Log a security-sensitive event tagged with the resolved caller
    /// identity (issues #17 + #26). Prefer this over `audit_security`
    /// on any event that flows through a request path — security
    /// events are the ones operators query by user when investigating
    /// an incident, so the caller attribution matters more here than
    /// for `Info`-level events. Use `Principal::Public` when the
    /// event is genuinely unauthenticated (no caller context
    /// available) rather than passing `None`.
    pub async fn audit_security_with_caller(
        &self,
        caller: Option<&Principal>,
        event_type: &str,
        agent_did: Option<&str>,
        details: serde_json::Value,
    ) -> Result<()> {
        self.log_audit(event_type, agent_did, caller, details, AuditSeverity::Security)
            .await
    }

    /// Internal: log a fully-specified audit event.
    async fn log_audit(
        &self,
        event_type: &str,
        agent_did: Option<&str>,
        caller: Option<&Principal>,
        details: serde_json::Value,
        severity: AuditSeverity,
    ) -> Result<()> {
        let mut audit = self.audit.write().await;
        audit
            .log(AuditEvent {
                timestamp: chrono::Utc::now(),
                component: self.component.clone(),
                event_type: event_type.to_string(),
                agent_did: agent_did.map(std::string::ToString::to_string),
                caller: caller.cloned(),
                details,
                severity,
            })
            .await
    }

    /// Increment a counter metric
    pub async fn count(&self, name: &str, value: u64) {
        let mut metrics = self.metrics.write().await;
        metrics.counter(name, value);
    }

    /// Record a timing
    pub async fn timing(&self, name: &str, duration_ms: u64) {
        let mut metrics = self.metrics.write().await;
        metrics.histogram(name, duration_ms);
    }

    /// Start a trace span
    pub async fn start_span(&self, name: &str, parent_id: Option<&str>) -> TraceSpan {
        let tracer = self.tracer.read().await;
        tracer.start_span(name, parent_id)
    }

    /// Get audit log entries
    pub async fn get_audit_log(&self, limit: usize) -> Vec<AuditEvent> {
        let audit = self.audit.read().await;
        audit.get_entries(limit).await
    }

    /// Get metrics snapshot
    pub async fn get_metrics(&self) -> serde_json::Value {
        let metrics = self.metrics.read().await;
        metrics.snapshot().await
    }

    /// Health check
    pub async fn health_check(&self) -> HealthStatus {
        HealthStatus::Healthy
    }
}

/// Health status
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthStatus {
    Healthy,
    Degraded,
    Unhealthy,
}

impl HealthStatus {
    #[must_use]
    pub fn is_healthy(&self) -> bool {
        matches!(self, HealthStatus::Healthy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `audit_with_caller` must stamp the resolved caller as a typed
    /// `Principal` on the emitted event so the audit trail is
    /// attributable to a real subject (issues #17 + #26).
    #[tokio::test]
    async fn audit_with_caller_records_caller_principal() {
        use crate::auth::Principal;
        let obs = Observability::new("tunnel");
        let caller = Principal::User("user:user-42".to_string());
        obs.audit_with_caller(
            Some(&caller),
            "tunnel_proxied_request",
            Some("agent-a"),
            serde_json::json!({"request_id": "req-1"}),
        )
        .await
        .unwrap();

        let entries = obs.get_audit_log(10).await;
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert_eq!(e.caller.as_ref(), Some(&caller));
        assert_eq!(e.event_type, "tunnel_proxied_request");
        assert_eq!(e.agent_did.as_deref(), Some("agent-a"));
        assert_eq!(e.details["request_id"], "req-1");
    }

    /// `audit` (no caller) must leave `caller` unset — backward
    /// compatibility for legacy call sites that haven't been migrated yet.
    #[tokio::test]
    async fn audit_without_caller_leaves_caller_unset() {
        let obs = Observability::new("tunnel");
        obs.audit(
            "agent_spawn",
            Some("agent-a"),
            serde_json::json!({}),
        )
        .await
        .unwrap();

        let entries = obs.get_audit_log(10).await;
        assert_eq!(entries.len(), 1);
        assert!(entries[0].caller.is_none());
    }

    /// `audit_security_with_caller` must stamp the resolved caller as a
    /// typed `Principal` on the emitted event AND mark the event as
    /// `AuditSeverity::Security` (issue #26 review: the audit_security
    /// half of the migration was missing — security events are the
    /// ones operators query by user when investigating an incident).
    #[tokio::test]
    async fn audit_security_with_caller_records_caller_and_severity() {
        use crate::auth::Principal;
        let obs = Observability::new("tunnel");
        let caller = Principal::User("user:alice".to_string());
        obs.audit_security_with_caller(
            Some(&caller),
            "permission_denied",
            Some("agent-a"),
            serde_json::json!({"resource": "team:eng"}),
        )
        .await
        .unwrap();

        let entries = obs.get_audit_log(10).await;
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert_eq!(e.caller.as_ref(), Some(&caller));
        assert_eq!(e.severity, AuditSeverity::Security);
        assert_eq!(e.event_type, "permission_denied");
        assert_eq!(e.agent_did.as_deref(), Some("agent-a"));
        assert_eq!(e.details["resource"], "team:eng");
    }

    /// `audit_security` (no caller) must leave `caller` unset — backward
    /// compatibility for legacy call sites that haven't been migrated yet.
    #[tokio::test]
    async fn audit_security_without_caller_leaves_caller_unset() {
        let obs = Observability::new("tunnel");
        obs.audit_security(
            "permission_denied_legacy",
            Some("agent-a"),
            serde_json::json!({}),
        )
        .await
        .unwrap();

        let entries = obs.get_audit_log(10).await;
        assert_eq!(entries.len(), 1);
        assert!(entries[0].caller.is_none());
        assert_eq!(entries[0].severity, AuditSeverity::Security);
    }
}
