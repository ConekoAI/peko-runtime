//! Tracer - Distributed tracing for request flows

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

/// Distributed tracer
pub struct Tracer {
    /// Span counter for IDs
    counter: AtomicU64,
}

/// A trace span
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceSpan {
    /// Unique span ID
    pub id: String,
    /// Parent span ID (if any)
    pub parent_id: Option<String>,
    /// Span name
    pub name: String,
    /// When started
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// When ended (None if active)
    pub ended_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Duration (ms)
    pub duration_ms: Option<u64>,
    /// Attributes
    pub attributes: HashMap<String, serde_json::Value>,
    /// Status
    pub status: SpanStatus,
}

/// Span status
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum SpanStatus {
    Active,
    Completed,
    Error,
    Cancelled,
}

impl Tracer {
    /// Create new tracer
    #[must_use]
    pub fn new() -> Self {
        Self {
            counter: AtomicU64::new(0),
        }
    }

    /// Start a new span
    pub fn start_span(&self, name: &str, parent_id: Option<&str>) -> TraceSpan {
        let id = format!("span-{}", self.counter.fetch_add(1, Ordering::Relaxed));

        TraceSpan {
            id,
            parent_id: parent_id.map(std::string::ToString::to_string),
            name: name.to_string(),
            started_at: chrono::Utc::now(),
            ended_at: None,
            duration_ms: None,
            attributes: HashMap::new(),
            status: SpanStatus::Active,
        }
    }
}

impl Default for Tracer {
    fn default() -> Self {
        Self::new()
    }
}

impl TraceSpan {
    /// Set attribute
    pub fn set_attr(&mut self, key: &str, value: impl Into<serde_json::Value>) {
        self.attributes.insert(key.to_string(), value.into());
    }

    /// Get attribute
    #[must_use]
    pub fn get_attr(&self, key: &str) -> Option<&serde_json::Value> {
        self.attributes.get(key)
    }

    /// Mark as error
    pub fn set_error(&mut self, error: &str) {
        self.attributes.insert("error".to_string(), error.into());
        self.status = SpanStatus::Error;
    }

    /// Check if active
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.status == SpanStatus::Active
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_span_creation() {
        let tracer = Tracer::new();
        let span = tracer.start_span("test_op", None);

        assert_eq!(span.name, "test_op");
        assert!(span.is_active());
    }

    #[test]
    fn test_span_with_parent() {
        let tracer = Tracer::new();
        let parent = tracer.start_span("parent", None);
        let child = tracer.start_span("child", Some(&parent.id));

        assert_eq!(child.parent_id, Some(parent.id));
    }
}
