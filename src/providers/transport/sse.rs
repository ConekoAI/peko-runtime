//! Server-Sent Events (SSE) parser
//!
//! Unified SSE parsing for all streaming provider responses.

use bytes::Bytes;
use futures::{Stream, StreamExt};
use std::pin::Pin;
use tracing::debug;

/// A parsed SSE event
#[derive(Debug, Clone)]
pub struct SseEvent {
    pub data: String,
}

/// SSE parser for streaming responses
pub struct SseParser;

impl SseParser {
    /// Parse a stream of bytes into SSE events
    pub fn parse_stream<S>(
        stream: S,
    ) -> Pin<Box<dyn Stream<Item = anyhow::Result<SseEvent>> + Send>>
    where
        S: Stream<Item = anyhow::Result<Bytes>> + Send + 'static,
    {
        Box::pin(stream.filter_map(|result| async move {
            match result {
                Ok(bytes) => {
                    let text = String::from_utf8_lossy(&bytes);
                    let events = Self::parse_chunk(&text);
                    // For simplicity, we yield events one at a time
                    // In production, you might want a more sophisticated approach
                    Some(Ok(events.into_iter().next()?))
                }
                Err(e) => Some(Err(e)),
            }
        }))
    }

    /// Parse a chunk of SSE data into events
    pub fn parse_chunk(chunk: &str) -> Vec<SseEvent> {
        let mut events = Vec::new();
        let mut current_data = String::new();

        for line in chunk.lines() {
            if line.is_empty() {
                // Empty line signals end of event
                if !current_data.is_empty() {
                    events.push(SseEvent {
                        data: current_data.clone(),
                    });
                    current_data.clear();
                }
            } else if let Some(data) = line.strip_prefix("data: ") {
                current_data.push_str(data);
            } else if line.starts_with(':') {
                // Comment, ignore
                debug!("SSE comment: {}", line);
            } else if let Some(id) = line.strip_prefix("id: ") {
                // Event ID, could be useful for debugging
                debug!("SSE event id: {}", id);
            } else if let Some(event_type) = line.strip_prefix("event: ") {
                // Event type, could be useful for specific handling
                debug!("SSE event type: {}", event_type);
            }
        }

        // Handle last event if chunk doesn't end with empty line
        if !current_data.is_empty() {
            events.push(SseEvent { data: current_data });
        }

        events
    }

    /// Parse a single SSE event from a data string
    /// Used by adapters to convert SSE data to provider-specific events
    pub fn parse_event(data: &str) -> Option<SseEvent> {
        if data.is_empty() {
            None
        } else {
            Some(SseEvent {
                data: data.to_string(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_sse() {
        let chunk = "data: hello\n\ndata: world\n\n";
        let events = SseParser::parse_chunk(chunk);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].data, "hello");
        assert_eq!(events[1].data, "world");
    }

    #[test]
    fn test_parse_multiline_data() {
        let chunk = "data: line1\ndata: line2\n\n";
        let events = SseParser::parse_chunk(chunk);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "line1line2");
    }

    #[test]
    fn test_parse_with_comments() {
        let chunk = ": comment\ndata: actual data\n\n";
        let events = SseParser::parse_chunk(chunk);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "actual data");
    }

    #[test]
    fn test_parse_no_trailing_newline() {
        let chunk = "data: hello";
        let events = SseParser::parse_chunk(chunk);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "hello");
    }

    #[test]
    fn test_parse_empty() {
        let chunk = "";
        let events = SseParser::parse_chunk(chunk);
        assert!(events.is_empty());
    }

    #[test]
    fn test_parse_json_data() {
        let chunk = r#"data: {"key": "value"}"#;
        let events = SseParser::parse_chunk(chunk);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, r#"{"key": "value"}"#);
    }
}
