//! Observability - Audit, metrics, and tracing for Pekobot
//!
//! Provides visibility into:
//! - What agents are doing (audit log)
//! - Performance metrics (counters, timers)
//! - Execution traces (request flows)

pub mod audit;
pub mod metrics;
pub mod performance;
pub mod tracer;

pub use audit::{AuditEvent, AuditLogger, AuditSeverity};
pub use metrics::MetricsCollector;
pub use performance::{
    start_timer, stop_timer, LatencyStats, MetricsExport, PerformanceGuard, PerformanceMetrics,
    GLOBAL_METRICS,
};
pub use tracer::{TraceSpan, Tracer};

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
        let mut audit = self.audit.write().await;
        audit
            .log(AuditEvent {
                timestamp: chrono::Utc::now(),
                component: self.component.clone(),
                event_type: event_type.to_string(),
                agent_did: agent_did.map(std::string::ToString::to_string),
                details,
                severity: AuditSeverity::Info,
            })
            .await
    }

    /// Log security-sensitive event
    pub async fn audit_security(
        &self,
        event_type: &str,
        agent_did: Option<&str>,
        details: serde_json::Value,
    ) -> Result<()> {
        let mut audit = self.audit.write().await;
        audit
            .log(AuditEvent {
                timestamp: chrono::Utc::now(),
                component: self.component.clone(),
                event_type: event_type.to_string(),
                agent_did: agent_did.map(std::string::ToString::to_string),
                details,
                severity: AuditSeverity::Security,
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
