//! Audit Log - Security and compliance logging

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// Audit logger
pub struct AuditLogger {
    /// In-memory buffer (for production, use persistent storage)
    buffer: VecDeque<AuditEvent>,
    /// Maximum buffer size
    max_size: usize,
}

/// Audit event
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    /// When the event occurred
    pub timestamp: chrono::DateTime<chrono::Utc>,
    /// Which component logged it
    pub component: String,
    /// Type of event
    pub event_type: String,
    /// Which agent (if any)
    pub agent_did: Option<String>,
    /// Resolved caller identity (pekohub sub, API key id, or `local`) —
    /// populated on every event that flows through the request path so the
    /// audit trail is attributable to a real user. See issue #17.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_id: Option<String>,
    /// Event details
    pub details: serde_json::Value,
    /// Severity level
    pub severity: AuditSeverity,
}

/// Audit severity levels
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum AuditSeverity {
    Debug,
    Info,
    Warning,
    Error,
    Security,
}

impl AuditLogger {
    /// Create new audit logger
    #[must_use]
    pub fn new() -> Self {
        Self {
            buffer: VecDeque::with_capacity(1000),
            max_size: 10000,
        }
    }

    /// Create with custom buffer size
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            buffer: VecDeque::with_capacity(capacity),
            max_size: capacity * 10,
        }
    }

    /// Log an event
    pub async fn log(&mut self, event: AuditEvent) -> Result<()> {
        // In production, write to persistent storage (SQLite, file, etc.)
        // For now, keep in memory buffer

        if self.buffer.len() >= self.max_size {
            self.buffer.pop_front(); // Remove oldest
        }

        self.buffer.push_back(event);
        Ok(())
    }

    /// Get recent entries
    pub async fn get_entries(&self, limit: usize) -> Vec<AuditEvent> {
        self.buffer.iter().rev().take(limit).cloned().collect()
    }

    /// Get entries by agent
    pub async fn get_by_agent(&self, did: &str, limit: usize) -> Vec<AuditEvent> {
        self.buffer
            .iter()
            .filter(|e| e.agent_did.as_deref() == Some(did))
            .rev()
            .take(limit)
            .cloned()
            .collect()
    }

    /// Get security events
    pub async fn get_security_events(&self, limit: usize) -> Vec<AuditEvent> {
        self.buffer
            .iter()
            .filter(|e| e.severity == AuditSeverity::Security)
            .rev()
            .take(limit)
            .cloned()
            .collect()
    }

    /// Clear buffer
    pub fn clear(&mut self) {
        self.buffer.clear();
    }

    /// Current buffer size
    #[must_use]
    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }
}

impl Default for AuditLogger {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_audit_log() {
        let mut logger = AuditLogger::new();

        logger
            .log(AuditEvent {
                timestamp: chrono::Utc::now(),
                component: "test".to_string(),
                event_type: "agent_spawn".to_string(),
                agent_did: Some("did:1".to_string()),
                caller_id: None,
                details: serde_json::json!({}),
                severity: AuditSeverity::Info,
            })
            .await
            .unwrap();

        assert_eq!(logger.len(), 1);

        let entries = logger.get_entries(10).await;
        assert_eq!(entries.len(), 1);
    }

    #[tokio::test]
    async fn test_audit_log_capacity() {
        let mut logger = AuditLogger::with_capacity(5);

        // Add more events than capacity
        for i in 0..10 {
            logger
                .log(AuditEvent {
                    timestamp: chrono::Utc::now(),
                    component: "test".to_string(),
                    event_type: format!("event_{i}"),
                    agent_did: None,
                    caller_id: None,
                    details: serde_json::json!({}),
                    severity: AuditSeverity::Info,
                })
                .await
                .unwrap();
        }

        // Should only keep max_size entries (capacity * 10)
        assert_eq!(logger.len(), 10);
    }

    #[tokio::test]
    async fn test_get_entries_limit() {
        let mut logger = AuditLogger::new();

        // Add 5 events
        for i in 0..5 {
            logger
                .log(AuditEvent {
                    timestamp: chrono::Utc::now(),
                    component: "test".to_string(),
                    event_type: format!("event_{i}"),
                    agent_did: None,
                    caller_id: None,
                    details: serde_json::json!({}),
                    severity: AuditSeverity::Info,
                })
                .await
                .unwrap();
        }

        // Get only 3 entries
        let entries = logger.get_entries(3).await;
        assert_eq!(entries.len(), 3);

        // Should return most recent first (LIFO order)
        assert_eq!(entries[0].event_type, "event_4");
        assert_eq!(entries[1].event_type, "event_3");
        assert_eq!(entries[2].event_type, "event_2");
    }

    #[tokio::test]
    async fn test_get_by_agent() {
        let mut logger = AuditLogger::new();

        logger
            .log(AuditEvent {
                timestamp: chrono::Utc::now(),
                component: "test".to_string(),
                event_type: "event1".to_string(),
                agent_did: Some("agent_a".to_string()),
                caller_id: None,
                details: serde_json::json!({}),
                severity: AuditSeverity::Info,
            })
            .await
            .unwrap();

        logger
            .log(AuditEvent {
                timestamp: chrono::Utc::now(),
                component: "test".to_string(),
                event_type: "event2".to_string(),
                agent_did: Some("agent_b".to_string()),
                caller_id: None,
                details: serde_json::json!({}),
                severity: AuditSeverity::Info,
            })
            .await
            .unwrap();

        logger
            .log(AuditEvent {
                timestamp: chrono::Utc::now(),
                component: "test".to_string(),
                event_type: "event3".to_string(),
                agent_did: Some("agent_a".to_string()),
                caller_id: None,
                details: serde_json::json!({}),
                severity: AuditSeverity::Info,
            })
            .await
            .unwrap();

        let agent_a_entries = logger.get_by_agent("agent_a", 10).await;
        assert_eq!(agent_a_entries.len(), 2);
        assert!(agent_a_entries
            .iter()
            .all(|e| e.agent_did == Some("agent_a".to_string())));
    }

    #[tokio::test]
    async fn test_get_security_events() {
        let mut logger = AuditLogger::new();

        logger
            .log(AuditEvent {
                timestamp: chrono::Utc::now(),
                component: "test".to_string(),
                event_type: "normal_event".to_string(),
                agent_did: None,
                caller_id: None,
                details: serde_json::json!({}),
                severity: AuditSeverity::Info,
            })
            .await
            .unwrap();

        logger
            .log(AuditEvent {
                timestamp: chrono::Utc::now(),
                component: "test".to_string(),
                event_type: "security_event".to_string(),
                agent_did: None,
                caller_id: None,
                details: serde_json::json!({}),
                severity: AuditSeverity::Security,
            })
            .await
            .unwrap();

        logger
            .log(AuditEvent {
                timestamp: chrono::Utc::now(),
                component: "test".to_string(),
                event_type: "another_security".to_string(),
                agent_did: None,
                caller_id: None,
                details: serde_json::json!({}),
                severity: AuditSeverity::Security,
            })
            .await
            .unwrap();

        let security_events = logger.get_security_events(10).await;
        assert_eq!(security_events.len(), 2);
        assert!(security_events
            .iter()
            .all(|e| e.severity == AuditSeverity::Security));
    }

    #[tokio::test]
    async fn test_clear() {
        let mut logger = AuditLogger::new();

        logger
            .log(AuditEvent {
                timestamp: chrono::Utc::now(),
                component: "test".to_string(),
                event_type: "event".to_string(),
                agent_did: None,
                caller_id: None,
                details: serde_json::json!({}),
                severity: AuditSeverity::Info,
            })
            .await
            .unwrap();

        assert_eq!(logger.len(), 1);
        logger.clear();
        assert_eq!(logger.len(), 0);
        assert!(logger.is_empty());
    }

    #[test]
    fn test_default_implementation() {
        let logger = AuditLogger::default();
        assert!(logger.is_empty());
    }

    #[tokio::test]
    async fn test_audit_severity_levels() {
        let severities = [
            AuditSeverity::Debug,
            AuditSeverity::Info,
            AuditSeverity::Warning,
            AuditSeverity::Error,
            AuditSeverity::Security,
        ];

        let mut logger = AuditLogger::new();
        for (i, severity) in severities.iter().enumerate() {
            logger
                .log(AuditEvent {
                    timestamp: chrono::Utc::now(),
                    component: "test".to_string(),
                    event_type: format!("event_{i}"),
                    agent_did: None,
                    caller_id: None,
                    details: serde_json::json!({}),
                    severity: *severity,
                })
                .await
                .unwrap();
        }

        assert_eq!(logger.len(), 5);
    }

    /// Issue #17: `caller_id` must round-trip through serialization
    /// when populated AND must be omitted (not serialized as null) when
    /// unset — keeps the wire format compact for legacy events that
    /// pre-date the per-user attribution plumbing.
    #[test]
    fn audit_event_caller_id_serialization() {
        let with_caller = AuditEvent {
            timestamp: chrono::Utc::now(),
            component: "tunnel".to_string(),
            event_type: "tunnel_proxied_request".to_string(),
            agent_did: Some("agent-a".to_string()),
            caller_id: Some("user-42".to_string()),
            details: serde_json::json!({}),
            severity: AuditSeverity::Info,
        };
        let v: serde_json::Value = serde_json::to_value(&with_caller).unwrap();
        assert_eq!(v["caller_id"], "user-42");

        let without_caller = AuditEvent {
            timestamp: chrono::Utc::now(),
            component: "tunnel".to_string(),
            event_type: "agent_spawn".to_string(),
            agent_did: None,
            caller_id: None,
            details: serde_json::json!({}),
            severity: AuditSeverity::Info,
        };
        let v: serde_json::Value = serde_json::to_value(&without_caller).unwrap();
        assert!(
            v.get("caller_id").is_none(),
            "caller_id must be omitted (skip_serializing_if) when None, got: {v}"
        );
    }
}
