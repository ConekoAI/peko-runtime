//! Streaming tool call parser
//!
//! Handles parsing tool calls from streaming LLM responses.
//! Supports both complete JSON and incremental/delta formats.

use serde_json::Value;
use std::collections::HashMap;
use tracing::warn;

/// State for a tool call being constructed from a stream
#[derive(Debug, Clone, Default)]
pub struct StreamingToolCall {
    /// Unique identifier for this tool call
    pub id: String,
    /// Tool name (may be partial during streaming)
    pub name: Option<String>,
    /// Tool arguments as partial JSON string
    pub arguments_json: String,
    /// Whether this tool call is complete
    pub is_complete: bool,
    /// Parsed arguments (available once complete)
    pub parsed_arguments: Option<Value>,
    /// Whether the arguments only parsed after heuristic JSON recovery was applied.
    /// When true, `parsed_arguments` may differ from what the model actually emitted
    /// (e.g. a truncated stream was closed off); callers should treat it as suspect.
    pub arguments_recovered: bool,
}

impl StreamingToolCall {
    /// Create a new streaming tool call
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: None,
            arguments_json: String::new(),
            is_complete: false,
            parsed_arguments: None,
            arguments_recovered: false,
        }
    }

    /// Update with a name delta
    pub fn update_name(&mut self, name_delta: &str) {
        if let Some(ref mut name) = self.name {
            name.push_str(name_delta);
        } else {
            self.name = Some(name_delta.to_string());
        }
    }

    /// Update with an arguments delta
    pub fn update_arguments(&mut self, args_delta: &str) {
        self.arguments_json.push_str(args_delta);
    }

    /// Set the complete name
    pub fn set_name(&mut self, name: impl Into<String>) {
        self.name = Some(name.into());
    }

    /// Finalize the tool call, parsing the arguments
    pub fn finalize(&mut self) -> Result<(), ToolCallParseError> {
        if self.name.is_none() {
            return Err(ToolCallParseError::MissingName);
        }

        // Try to parse the accumulated JSON as-is first.
        match serde_json::from_str::<Value>(&self.arguments_json) {
            Ok(parsed) => {
                self.parsed_arguments = Some(parsed);
                self.is_complete = true;
                Ok(())
            }
            Err(e) => {
                // The raw JSON did not parse. Attempt a best-effort heuristic repair
                // (balancing delimiters, stripping a trailing comma). This can produce
                // valid-but-wrong arguments from a truncated stream, so it is never
                // silent: we flag the call and warn so callers/telemetry can react.
                let fixed = self.try_fix_json();
                match serde_json::from_str::<Value>(&fixed) {
                    Ok(parsed) => {
                        self.parsed_arguments = Some(parsed);
                        self.is_complete = true;
                        self.arguments_recovered = true;
                        warn!(
                            tool = self.name.as_deref().unwrap_or("unknown"),
                            error = %e,
                            raw = %truncate_for_log(&self.arguments_json),
                            recovered = %truncate_for_log(&fixed),
                            "tool call arguments parsed only after heuristic JSON recovery; \
                             arguments may differ from what the model emitted"
                        );
                        Ok(())
                    }
                    Err(_) => Err(ToolCallParseError::InvalidJson(e.to_string())),
                }
            }
        }
    }

    /// Attempt to fix common JSON formatting issues.
    ///
    /// Delimiter counting is string-aware: braces, brackets, and quotes that appear
    /// inside string literals are ignored, so arguments whose values legitimately
    /// contain `{`, `}`, `[`, or `]` are not miscounted and corrupted.
    fn try_fix_json(&self) -> String {
        let mut fixed = self.arguments_json.trim().to_string();

        // Strip a single trailing comma first (before balancing) so we don't
        // produce an invalid `[1,2,]` / `{"a":1,}` by appending a closer after it.
        if fixed.ends_with(',') {
            fixed.pop();
        }

        // Count unmatched openers, ignoring delimiters inside string literals.
        let (mut open_braces, mut open_brackets) = (0i32, 0i32);
        let mut in_string = false;
        let mut escaped = false;
        for c in fixed.chars() {
            if in_string {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == '"' {
                    in_string = false;
                }
                continue;
            }
            match c {
                '"' => in_string = true,
                '{' => open_braces += 1,
                '}' => open_braces -= 1,
                '[' => open_brackets += 1,
                ']' => open_brackets -= 1,
                _ => {}
            }
        }

        // If the string was left open, close it before balancing containers.
        if in_string {
            fixed.push('"');
        }

        // Append missing closers in the correct order is ambiguous for interleaved
        // structures, but for the common truncation cases (object/array cut short)
        // brackets close before braces.
        for _ in 0..open_brackets.max(0) {
            fixed.push(']');
        }
        for _ in 0..open_braces.max(0) {
            fixed.push('}');
        }

        fixed
    }

    /// Check if we have enough to show a preview
    #[must_use]
    pub fn has_preview(&self) -> bool {
        self.name.is_some() || !self.arguments_json.is_empty()
    }

    /// Get a preview of the tool call for display
    #[must_use]
    pub fn preview(&self) -> String {
        let name = self.name.as_deref().unwrap_or("unknown");
        if self.arguments_json.is_empty() {
            name.to_string()
        } else {
            format!(
                "{}({})",
                name,
                &self.arguments_json[..self.arguments_json.len().min(50)]
            )
        }
    }
}

/// Truncate a string for log output so a large arguments blob doesn't flood logs.
fn truncate_for_log(s: &str) -> String {
    const MAX: usize = 200;
    if s.len() <= MAX {
        s.to_string()
    } else {
        let mut end = MAX;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}… ({} bytes)", &s[..end], s.len())
    }
}

/// Errors that can occur during tool call parsing
#[derive(Debug, Clone, PartialEq)]
pub enum ToolCallParseError {
    /// Tool name is missing
    MissingName,
    /// Arguments JSON is invalid
    InvalidJson(String),
    /// Tool call ID is missing or invalid
    InvalidId,
    /// Stream ended unexpectedly
    IncompleteStream,
}

impl std::fmt::Display for ToolCallParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingName => write!(f, "Tool call missing name"),
            Self::InvalidJson(msg) => write!(f, "Invalid JSON: {msg}"),
            Self::InvalidId => write!(f, "Invalid tool call ID"),
            Self::IncompleteStream => write!(f, "Stream ended before tool call complete"),
        }
    }
}

impl std::error::Error for ToolCallParseError {}

/// Parser for multiple concurrent streaming tool calls
#[derive(Debug, Default)]
pub struct ToolCallStreamParser {
    /// Active tool calls being constructed
    calls: HashMap<String, StreamingToolCall>,
    /// Completed tool calls
    completed: Vec<StreamingToolCall>,
}

impl ToolCallStreamParser {
    /// Create a new parser
    #[must_use]
    pub fn new() -> Self {
        Self {
            calls: HashMap::new(),
            completed: Vec::new(),
        }
    }

    /// Start a new tool call
    pub fn start_call(&mut self, id: impl Into<String>) -> &mut StreamingToolCall {
        let id = id.into();
        let call = StreamingToolCall::new(&id);
        self.calls.insert(id.clone(), call);
        self.calls.get_mut(&id).unwrap()
    }

    /// Update a tool call with a name delta
    pub fn update_name(&mut self, id: &str, delta: &str) -> Option<&StreamingToolCall> {
        if let Some(call) = self.calls.get_mut(id) {
            call.update_name(delta);
            Some(call)
        } else {
            None
        }
    }

    /// Update a tool call with an arguments delta
    pub fn update_arguments(&mut self, id: &str, delta: &str) -> Option<&StreamingToolCall> {
        if let Some(call) = self.calls.get_mut(id) {
            call.update_arguments(delta);
            Some(call)
        } else {
            None
        }
    }

    /// Finalize a tool call
    pub fn finalize_call(
        &mut self,
        id: &str,
    ) -> Result<Option<StreamingToolCall>, ToolCallParseError> {
        if let Some(mut call) = self.calls.remove(id) {
            call.finalize()?;
            self.completed.push(call.clone());
            Ok(Some(call))
        } else {
            Ok(None)
        }
    }

    /// Get all completed tool calls
    #[must_use]
    pub fn completed_calls(&self) -> &[StreamingToolCall] {
        &self.completed
    }

    /// Get active (incomplete) tool calls
    #[must_use]
    pub fn active_calls(&self) -> &HashMap<String, StreamingToolCall> {
        &self.calls
    }

    /// Check if all calls are complete
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.calls.is_empty() && !self.completed.is_empty()
    }

    /// Take all completed calls (clears internal state)
    pub fn take_completed(&mut self) -> Vec<StreamingToolCall> {
        std::mem::take(&mut self.completed)
    }

    /// Finalize any remaining active calls
    pub fn finalize_all(&mut self) -> Result<Vec<StreamingToolCall>, ToolCallParseError> {
        let ids: Vec<String> = self.calls.keys().cloned().collect();
        for id in ids {
            self.finalize_call(&id)?;
        }
        Ok(self.take_completed())
    }
}

/// Parse tool calls from complete text (non-streaming fallback)
#[must_use]
pub fn parse_tool_calls_from_text(text: &str) -> Vec<StreamingToolCall> {
    let mut results = Vec::new();

    // Try to find tool calls in various formats

    // Format 1: Markdown code block with JSON
    // ```json
    // {"name": "tool_name", "arguments": {...}}
    // ```
    if let Some(json) = extract_json_code_block(text) {
        if let Ok(parsed) = serde_json::from_str::<Value>(&json) {
            if let Some(call) = value_to_tool_call(parsed) {
                results.push(call);
            }
        }
    }

    // Format 2: TOOL_CALL: prefix
    // TOOL_CALL: tool_name({"arg": "value"})
    if results.is_empty() {
        if let Some(call) = parse_tool_call_prefix(text) {
            results.push(call);
        }
    }

    results
}

/// Extract JSON from markdown code block
fn extract_json_code_block(text: &str) -> Option<String> {
    // Look for ```json ... ``` or ``` ... ```
    for pattern in ["```json", "```"] {
        if let Some(start) = text.find(pattern) {
            let after_start = start + pattern.len();
            if let Some(end) = text[after_start..].find("```") {
                return Some(text[after_start..after_start + end].trim().to_string());
            }
        }
    }
    None
}

/// Parse `TOOL_CALL`: prefix format
fn parse_tool_call_prefix(text: &str) -> Option<StreamingToolCall> {
    if let Some(start) = text.find("TOOL_CALL:") {
        let rest = &text[start + 10..];
        if let Some(paren_idx) = rest.find('(') {
            let name = rest[..paren_idx].trim();
            if let Some(close_idx) = rest.find(')') {
                let args_str = &rest[paren_idx + 1..close_idx];
                let args = if args_str.trim().is_empty() {
                    serde_json::json!({})
                } else {
                    serde_json::from_str(args_str)
                        .unwrap_or_else(|_| serde_json::json!({ "value": args_str }))
                };

                let mut call =
                    StreamingToolCall::new(format!("tc_{}", chrono::Utc::now().timestamp_millis()));
                call.set_name(name);
                call.arguments_json = args.to_string();
                let _ = call.finalize();
                return Some(call);
            }
        }
    }
    None
}

/// Convert a JSON value to a `StreamingToolCall`
fn value_to_tool_call(value: Value) -> Option<StreamingToolCall> {
    let obj = value.as_object()?;

    let name = obj.get("name")?.as_str()?;
    let args = obj
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));

    let id = obj.get("id").and_then(|v| v.as_str()).map_or_else(
        || format!("tc_{}", chrono::Utc::now().timestamp_millis()),
        std::string::ToString::to_string,
    );

    let mut call = StreamingToolCall::new(id);
    call.set_name(name);
    call.arguments_json = args.to_string();
    let _ = call.finalize();

    Some(call)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_streaming_tool_call_construction() {
        let mut call = StreamingToolCall::new("tc_1");

        // Stream name character by character
        call.update_name("w");
        call.update_name("e");
        call.update_name("b");
        call.update_name("_");
        call.update_name("search");

        assert_eq!(call.name, Some("web_search".to_string()));
        assert!(!call.is_complete);

        // Stream arguments
        call.update_arguments("{");
        call.update_arguments('"'.to_string().as_str());
        call.update_arguments("query");
        call.update_arguments('"'.to_string().as_str());
        call.update_arguments(":");
        call.update_arguments('"'.to_string().as_str());
        call.update_arguments("rust async");
        call.update_arguments('"'.to_string().as_str());
        call.update_arguments("}");

        // Finalize
        call.finalize().unwrap();
        assert!(call.is_complete);
        assert!(call.parsed_arguments.is_some());
    }

    #[test]
    fn test_json_recovery_closes_truncated_object() {
        // A stream cut off before the closing brace should recover and be flagged.
        let mut call = StreamingToolCall::new("tc_1");
        call.set_name("web_search");
        call.update_arguments(r#"{"query": "rust async""#);

        call.finalize().unwrap();
        assert!(call.is_complete);
        assert!(call.arguments_recovered, "recovery flag must be set");
        assert_eq!(
            call.parsed_arguments.unwrap()["query"],
            serde_json::json!("rust async")
        );
    }

    #[test]
    fn test_json_recovery_strips_trailing_comma() {
        let mut call = StreamingToolCall::new("tc_1");
        call.set_name("tool");
        call.update_arguments(r#"{"a": 1,"#);

        call.finalize().unwrap();
        assert!(call.arguments_recovered);
        assert_eq!(call.parsed_arguments.unwrap()["a"], serde_json::json!(1));
    }

    #[test]
    fn test_clean_json_is_not_flagged_as_recovered() {
        let mut call = StreamingToolCall::new("tc_1");
        call.set_name("tool");
        call.update_arguments(r#"{"a": 1}"#);

        call.finalize().unwrap();
        assert!(
            !call.arguments_recovered,
            "well-formed JSON must not be marked recovered"
        );
    }

    #[test]
    fn test_recovery_does_not_miscount_braces_inside_strings() {
        // Braces inside a string value must not be treated as structural delimiters.
        // The object is already balanced; recovery (if it ran) must not append a stray `}`.
        let mut call = StreamingToolCall::new("tc_1");
        call.set_name("write");
        call.update_arguments(r#"{"body": "func() { return [1]; }"}"#);

        call.finalize().unwrap();
        assert!(
            !call.arguments_recovered,
            "balanced JSON with braces in a string must parse without recovery"
        );
        assert_eq!(
            call.parsed_arguments.unwrap()["body"],
            serde_json::json!("func() { return [1]; }")
        );
    }

    #[test]
    fn test_recovery_closes_open_string_with_braces() {
        // Truncated mid-string: only the string and object need closing; the `{`
        // inside the string must not inflate the brace count.
        let mut call = StreamingToolCall::new("tc_1");
        call.set_name("write");
        call.update_arguments(r#"{"body": "open { brace"#);

        call.finalize().unwrap();
        assert!(call.arguments_recovered);
        assert_eq!(
            call.parsed_arguments.unwrap()["body"],
            serde_json::json!("open { brace")
        );
    }

    #[test]
    fn test_parser_multiple_calls() {
        let mut parser = ToolCallStreamParser::new();

        // Start first call
        let call1 = parser.start_call("tc_1");
        call1.set_name("tool_a");
        call1.update_arguments("{\"x\": 1}");

        // Start second call
        let call2 = parser.start_call("tc_2");
        call2.set_name("tool_b");
        call2.update_arguments("{\"y\": 2}");

        // Finalize first
        parser.finalize_call("tc_1").unwrap();
        assert_eq!(parser.completed_calls().len(), 1);
        assert_eq!(parser.active_calls().len(), 1);

        // Finalize second
        parser.finalize_call("tc_2").unwrap();
        assert_eq!(parser.completed_calls().len(), 2);
        assert!(parser.active_calls().is_empty());
    }

    #[test]
    fn test_parse_from_markdown() {
        let text = r#"I'll search for that.
```json
{
  "name": "web_search",
  "arguments": {"query": "rust async"}
}
```"#;

        let calls = parse_tool_calls_from_text(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, Some("web_search".to_string()));
    }

    #[test]
    fn test_parse_from_prefix() {
        let text = "TOOL_CALL: Read({\"path\": \"/tmp\"})";

        let calls = parse_tool_calls_from_text(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, Some("Read".to_string()));
    }

    #[test]
    fn test_preview() {
        let mut call = StreamingToolCall::new("tc_1");
        call.set_name("web_search");
        call.update_arguments("{\"query\": \"rust programming language\"}");

        let preview = call.preview();
        assert!(preview.contains("web_search"));
        assert!(preview.contains("query"));
    }
}
