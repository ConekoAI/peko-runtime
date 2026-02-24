//! Tracer - Distributed tracing for request flows

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

/// Distributed tracer
pub struct Tracer {
    /// Active spans
    spans: HashMap<String, TraceSpan>,
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

/// Trace context (propagated across calls)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceContext {
    /// Trace ID
    pub trace_id: String,
    /// Current span ID
    pub span_id: String,
    /// Whether tracing is sampled
    pub sampled: bool,
}

impl Tracer {
    /// Create new tracer
    #[must_use] 
    pub fn new() -> Self {
        Self {
            spans: HashMap::new(),
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

    /// End a span
    pub fn end_span(&self, span: &mut TraceSpan) {
        span.ended_at = Some(chrono::Utc::now());
        span.duration_ms = Some(
            span.ended_at
                .unwrap()
                .signed_duration_since(span.started_at)
                .num_milliseconds() as u64,
        );
        span.status = SpanStatus::Completed;
    }

    /// Create root context
    pub fn create_root_context(&self) -> TraceContext {
        TraceContext {
            trace_id: format!("trace-{}", self.counter.fetch_add(1, Ordering::Relaxed)),
            span_id: format!("span-{}", self.counter.fetch_add(1, Ordering::Relaxed)),
            sampled: true,
        }
    }

    /// Create child context
    pub fn create_child_context(&self, parent: &TraceContext) -> TraceContext {
        TraceContext {
            trace_id: parent.trace_id.clone(),
            span_id: format!("span-{}", self.counter.fetch_add(1, Ordering::Relaxed)),
            sampled: parent.sampled,
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

impl TraceContext {
    /// Create new root context
    #[must_use] 
    pub fn new() -> Self {
        let tracer = Tracer::new();
        tracer.create_root_context()
    }

    /// Create with specific trace ID
    pub fn with_trace_id(trace_id: impl Into<String>) -> Self {
        Self {
            trace_id: trace_id.into(),
            span_id: format!("span-{}", std::process::id()),
            sampled: true,
        }
    }

    /// Propagate to child
    #[must_use] 
    pub fn child(&self) -> Self {
        Self {
            trace_id: self.trace_id.clone(),
            span_id: format!("span-{}", std::process::id()),
            sampled: self.sampled,
        }
    }
}

impl Default for TraceContext {
    fn default() -> Self {
        Self::new()
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

    #[test]
    fn test_trace_context() {
        let ctx = TraceContext::new();
        let child = ctx.child();

        assert_eq!(ctx.trace_id, child.trace_id);
        assert_ne!(ctx.span_id, child.span_id);
    }
}
