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
    pub fn new() -> Self {
        Self {
            buffer: VecDeque::with_capacity(1000),
            max_size: 10000,
        }
    }

    /// Create with custom buffer size
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
    pub fn len(&self) -> usize {
        self.buffer.len()
    }

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
                details: serde_json::json!({}),
                severity: AuditSeverity::Info,
            })
            .await
            .unwrap();

        assert_eq!(logger.len(), 1);

        let entries = logger.get_entries(10).await;
        assert_eq!(entries.len(), 1);
    }
}
