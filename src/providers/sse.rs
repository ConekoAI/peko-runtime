//! Server-Sent Events (SSE) parsing utilities
//!
//! Used for streaming responses from `OpenAI`, Kimi, and other providers
//! that support Server-Sent Events.

/// An SSE event
#[derive(Debug, Clone)]
pub struct SseEvent {
    /// Event ID (optional)
    pub id: Option<String>,
    /// Event type (e.g., "message", "error")
    pub event: String,
    /// Event data
    pub data: String,
}

impl SseEvent {
    /// Create a new SSE event
    pub fn new(event: impl Into<String>, data: impl Into<String>) -> Self {
        Self {
            id: None,
            event: event.into(),
            data: data.into(),
        }
    }

    /// Check if this is a "done" event (`OpenAI` format)
    #[must_use]
    pub fn is_done(&self) -> bool {
        self.data.trim() == "[DONE]"
    }

    /// Parse the data as JSON
    pub fn parse_json<T: serde::de::DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_str(&self.data)
    }
}

/// Parse SSE events from a chunk of text
///
/// This is a simple parser that processes SSE data line by line.
/// It returns events as they become complete (separated by double newlines).
pub struct SseParser {
    buffer: String,
}

impl SseParser {
    /// Create a new SSE parser
    #[must_use]
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
        }
    }

    /// Feed data into the parser and get any complete events
    pub fn feed(&mut self, data: &str) -> Vec<SseEvent> {
        self.buffer.push_str(data);
        self.parse_events()
    }

    /// Parse complete events from the buffer
    fn parse_events(&mut self) -> Vec<SseEvent> {
        let mut events = Vec::new();
        let delimiter = "\n\n";

        while let Some(pos) = self.buffer.find(delimiter) {
            let event_text = self.buffer[..pos].to_string();
            self.buffer = self.buffer[pos + delimiter.len()..].to_string();

            if let Some(event) = parse_event_text(&event_text) {
                events.push(event);
            }
        }

        events
    }

    /// Get any remaining data (incomplete event)
    #[must_use]
    pub fn into_remaining(self) -> String {
        self.buffer
    }
}

impl Default for SseParser {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse a single SSE event from text
fn parse_event_text(text: &str) -> Option<SseEvent> {
    let mut id = None;
    let mut event = "message".to_string();
    let mut data_lines: Vec<&str> = Vec::new();

    for line in text.lines() {
        if line.is_empty() {
            continue;
        }

        if let Some(value) = line.strip_prefix("id: ") {
            id = Some(value.to_string());
        } else if let Some(value) = line.strip_prefix("id:") {
            id = Some(value.to_string());
        } else if let Some(value) = line.strip_prefix("event: ") {
            event = value.to_string();
        } else if let Some(value) = line.strip_prefix("event:") {
            event = value.to_string();
        } else if let Some(value) = line.strip_prefix("data: ") {
            data_lines.push(value);
        } else if let Some(value) = line.strip_prefix("data:") {
            data_lines.push(value);
        } else if line.starts_with(':') {
            // Comment line, ignore
        }
    }

    if data_lines.is_empty() {
        return None;
    }

    Some(SseEvent {
        id,
        event,
        data: data_lines.join("\n"),
    })
}

/// Parse a streaming response line (simple format used by some providers)
///
/// Format: `data: {...}\n\n`
#[must_use]
pub fn parse_sse_line(line: &str) -> Option<SseEvent> {
    if line.is_empty() {
        return None;
    }

    // Handle "data: {...}" format
    if let Some(data) = line.strip_prefix("data: ") {
        return Some(SseEvent::new("message", data));
    }

    // Ignore comments (lines starting with :)
    if line.starts_with(':') {
        return None;
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_event_text() {
        let text = r#"id: 123
event: message
data: Hello
data: world"#;

        let event = parse_event_text(text).unwrap();
        assert_eq!(event.id, Some("123".to_string()));
        assert_eq!(event.event, "message");
        assert_eq!(event.data, "Hello\nworld");
    }

    #[test]
    fn test_parse_event_simple() {
        let text = "data: Hello world";
        let event = parse_event_text(text).unwrap();
        assert_eq!(event.data, "Hello world");
    }

    #[test]
    fn test_is_done() {
        let event = SseEvent::new("message", "[DONE]");
        assert!(event.is_done());

        let event = SseEvent::new("message", "not done");
        assert!(!event.is_done());
    }

    #[test]
    fn test_sse_parser() {
        let mut parser = SseParser::new();

        // Feed partial data
        let events = parser.feed("data: Hello\n\ndata: World");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "Hello");

        // Feed remaining data
        let events = parser.feed("\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "World");
    }

    #[test]
    fn test_sse_parser_multiple() {
        let mut parser = SseParser::new();

        let events = parser.feed("data: One\n\ndata: Two\n\ndata: Three\n\n");
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].data, "One");
        assert_eq!(events[1].data, "Two");
        assert_eq!(events[2].data, "Three");
    }
}
