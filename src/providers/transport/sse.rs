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
    ///
    /// This uses a channel-based approach to handle stateful parsing across
    /// chunk boundaries. It properly handles:
    /// - Multiple events per chunk
    /// - Partial events across chunk boundaries
    pub fn parse_stream<S>(
        stream: S,
    ) -> Pin<Box<dyn Stream<Item = anyhow::Result<SseEvent>> + Send>>
    where
        S: Stream<Item = anyhow::Result<Bytes>> + Send + 'static,
    {
        use tokio::sync::mpsc;
        use tokio_stream::wrappers::ReceiverStream;

        let (tx, rx) = mpsc::channel::<anyhow::Result<SseEvent>>(100);

        // Spawn a task to process the stream
        tokio::spawn(async move {
            let mut buffer = String::new();
            let mut stream = Box::pin(stream);

            while let Some(result) = stream.next().await {
                match result {
                    Ok(bytes) => {
                        let text = String::from_utf8_lossy(&bytes);
                        buffer.push_str(&text);

                        // Extract and send all complete events
                        while let Some(pos) = find_event_end(&buffer) {
                            let event_text = buffer[..pos].trim().to_string();
                            buffer.drain(..pos);

                            let events = Self::parse_chunk(&event_text);
                            for event in events {
                                if tx.send(Ok(event)).await.is_err() {
                                    return;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e)).await;
                        return;
                    }
                }
            }

            // Send any remaining event at end of stream
            if !buffer.trim().is_empty() {
                let events = Self::parse_chunk(&buffer);
                for event in events {
                    let _ = tx.send(Ok(event)).await;
                }
            }
            // tx dropped here, closing the channel
        });

        // Use ReceiverStream for reliable channel-to-stream conversion
        Box::pin(ReceiverStream::new(rx))
    }
}

/// Find the end of the first complete SSE event in the buffer
/// Returns the position after the event delimiter (double newline)
fn find_event_end(buffer: &str) -> Option<usize> {
    // Look for double newline (\n\n or \r\n\r\n)
    if let Some(pos) = buffer.find("\n\n") {
        return Some(pos + 2);
    }
    if let Some(pos) = buffer.find("\r\n\r\n") {
        return Some(pos + 4);
    }
    None
}

impl SseParser {
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
            } else if let Some(data) = line.strip_prefix("data:") {
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
    #[must_use]
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
