//! Mock provider adapter for unit testing
//!
//! Enables testing the agentic loop and engine without real API keys.
//! Responses are queued ahead of time and returned in FIFO order.

use super::{
    adapters::ApiAdapter,
    AuthConfig, ChatOptions, ChatResponse, ContentBlock, LlmMessage, StopReason, StreamEvent,
    ToolDefinition,
};
use anyhow::Result;
use serde_json::Value;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

/// A queued response from the mock provider
#[derive(Debug, Clone)]
pub enum MockResponse {
    /// Return a successful chat response
    Success(ChatResponse),
    /// Return an error
    Error(String),
    /// Return a stream of events
    Stream(Vec<StreamEvent>),
}

/// Mock adapter for unit testing
///
/// Configure responses ahead of time, then use in the agentic loop.
/// Each call to `chat_with_tools` or `stream_with_tools` consumes one queued response.
#[derive(Debug, Clone)]
pub struct MockAdapter {
    model: String,
    /// Queue of responses for `chat_with_tools`
    chat_responses: Arc<Mutex<Vec<MockResponse>>>,
    /// Queue of responses for `stream_with_tools`
    stream_responses: Arc<Mutex<Vec<MockResponse>>>,
    /// Record of all requests made (for assertions)
    recorded_requests: Arc<Mutex<Vec<RecordedRequest>>>,
}

/// A recorded request for test assertions
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub messages: Vec<LlmMessage>,
    pub tools: Vec<ToolDefinition>,
    pub stream: bool,
}

impl MockAdapter {
    /// Create a new mock adapter
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            chat_responses: Arc::new(Mutex::new(Vec::new())),
            stream_responses: Arc::new(Mutex::new(Vec::new())),
            recorded_requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Queue a response for `chat_with_tools`
    pub fn queue_chat_response(&self, response: MockResponse) {
        if let Ok(mut queue) = self.chat_responses.lock() {
            queue.push(response);
        }
    }

    /// Queue a response for `stream_with_tools`
    pub fn queue_stream_response(&self, response: MockResponse) {
        if let Ok(mut queue) = self.stream_responses.lock() {
            queue.push(response);
        }
    }

    /// Queue a simple text response for both chat and stream
    pub fn queue_text(&self, text: impl Into<String>) {
        let text = text.into();
        let response = ChatResponse {
            content: vec![ContentBlock::Text { text: text.clone() }],
            tool_calls: vec![],
            stop_reason: StopReason::Stop,
            usage: crate::providers::TokenUsage::default(),
            provider: "mock".to_string(),
            model: self.model.clone(),
        };
        self.queue_chat_response(MockResponse::Success(response.clone()));

        let stream_events = vec![
            StreamEvent::Start {
                provider: "mock".to_string(),
                model: self.model.clone(),
            },
            StreamEvent::TextDelta {
                content_index: 0,
                delta: text,
            },
            StreamEvent::TextEnd {
                content_index: 0,
                content: String::new(),
            },
            StreamEvent::Usage {
                input: 0,
                output: 0,
                total: 0,
            },
            StreamEvent::Done {
                stop_reason: StopReason::Stop,
            },
        ];
        self.queue_stream_response(MockResponse::Stream(stream_events));
    }

    /// Queue a tool call response
    pub fn queue_tool_call(&self, id: impl Into<String>, name: impl Into<String>, arguments: Value) {
        let id = id.into();
        let name = name.into();
        let response = ChatResponse {
            content: vec![],
            tool_calls: vec![ContentBlock::ToolCall {
                id: id.clone(),
                name: name.clone(),
                arguments: arguments.clone(),
            }],
            stop_reason: StopReason::ToolUse,
            usage: crate::providers::TokenUsage::default(),
            provider: "mock".to_string(),
            model: self.model.clone(),
        };
        self.queue_chat_response(MockResponse::Success(response.clone()));

        let stream_events = vec![
            StreamEvent::Start {
                provider: "mock".to_string(),
                model: self.model.clone(),
            },
            StreamEvent::ToolCallStart { content_index: 0 },
            StreamEvent::ToolCallEnd {
                content_index: 0,
                tool_call: ContentBlock::ToolCall {
                    id,
                    name,
                    arguments,
                },
            },
            StreamEvent::Usage {
                input: 0,
                output: 0,
                total: 0,
            },
            StreamEvent::Done {
                stop_reason: StopReason::ToolUse,
            },
        ];
        self.queue_stream_response(MockResponse::Stream(stream_events));
    }

    /// Queue an error response
    pub fn queue_error(&self, message: impl Into<String>) {
        let msg = message.into();
        self.queue_chat_response(MockResponse::Error(msg.clone()));
        self.queue_stream_response(MockResponse::Error(msg));
    }

    /// Get all recorded requests
    pub fn recorded_requests(&self) -> Vec<RecordedRequest> {
        self.recorded_requests
            .lock()
            .map(|r| r.clone())
            .unwrap_or_default()
    }

    /// Clear all queued responses and recorded requests
    pub fn clear(&self) {
        if let Ok(mut q) = self.chat_responses.lock() {
            q.clear();
        }
        if let Ok(mut q) = self.stream_responses.lock() {
            q.clear();
        }
        if let Ok(mut r) = self.recorded_requests.lock() {
            r.clear();
        }
    }

    /// Pop the next chat response
    fn pop_chat_response(&self) -> Option<MockResponse> {
        self.chat_responses.lock().ok()?.pop()
    }

    /// Pop the next stream response
    fn pop_stream_response(&self) -> Option<MockResponse> {
        self.stream_responses.lock().ok()?.pop()
    }

    /// Record a request
    fn record_request(&self, messages: &[LlmMessage], tools: Option<&[ToolDefinition]>, stream: bool) {
        if let Ok(mut r) = self.recorded_requests.lock() {
            r.push(RecordedRequest {
                messages: messages.to_vec(),
                tools: tools.map_or_else(Vec::new, |t| t.to_vec()),
                stream,
            });
        }
    }

    /// Execute a mock chat (non-streaming)
    pub fn chat_with_tools(
        &self,
        messages: &[LlmMessage],
        tools: Option<&[ToolDefinition]>,
        _options: &ChatOptions,
    ) -> Result<ChatResponse> {
        self.record_request(messages, tools, false);
        match self.pop_chat_response() {
            Some(MockResponse::Success(response)) => Ok(response),
            Some(MockResponse::Error(msg)) => Err(anyhow::anyhow!(msg)),
            Some(MockResponse::Stream(events)) => {
                // Convert stream events to a ChatResponse
                let text: String = events
                    .iter()
                    .filter_map(|e| match e {
                        StreamEvent::TextDelta { delta, .. } => Some(delta.as_str()),
                        _ => None,
                    })
                    .collect();
                let tool_calls: Vec<ContentBlock> = events
                    .iter()
                    .filter_map(|e| match e {
                        StreamEvent::ToolCallEnd { tool_call, .. } => Some(tool_call.clone()),
                        _ => None,
                    })
                    .collect();
                let stop_reason = events
                    .iter()
                    .find_map(|e| match e {
                        StreamEvent::Done { stop_reason } => Some(*stop_reason),
                        _ => None,
                    })
                    .unwrap_or(StopReason::Stop);
                Ok(ChatResponse {
                    content: if text.is_empty() {
                        vec![]
                    } else {
                        vec![ContentBlock::Text { text }]
                    },
                    tool_calls,
                    stop_reason,
                    usage: crate::providers::TokenUsage::default(),
                    provider: "mock".to_string(),
                    model: self.model.clone(),
                })
            }
            None => Err(anyhow::anyhow!(
                "Mock adapter response queue empty for chat_with_tools"
            )),
        }
    }

    /// Execute a mock stream
    pub fn stream_with_tools(
        &self,
        messages: &[LlmMessage],
        tools: Option<&[ToolDefinition]>,
        _options: &ChatOptions,
    ) -> Result<Pin<Box<dyn futures::Stream<Item = anyhow::Result<StreamEvent>> + Send>>> {
        self.record_request(messages, tools, true);
        let events = match self.pop_stream_response() {
            Some(MockResponse::Stream(events)) => events,
            Some(MockResponse::Success(response)) => {
                // Convert ChatResponse to stream events
                let mut evs = vec![
                    StreamEvent::Start {
                        provider: "mock".to_string(),
                        model: self.model.clone(),
                    },
                ];
                let text: String = response
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect();
                if !text.is_empty() {
                    evs.push(StreamEvent::TextDelta {
                        content_index: 0,
                        delta: text.clone(),
                    });
                    evs.push(StreamEvent::TextEnd {
                        content_index: 0,
                        content: text,
                    });
                }
                for (i, tc) in response.tool_calls.iter().enumerate() {
                    evs.push(StreamEvent::ToolCallStart { content_index: i });
                    evs.push(StreamEvent::ToolCallEnd {
                        content_index: i,
                        tool_call: tc.clone(),
                    });
                }
                evs.push(StreamEvent::Usage {
                    input: response.usage.input,
                    output: response.usage.output,
                    total: response.usage.total,
                });
                evs.push(StreamEvent::Done {
                    stop_reason: response.stop_reason,
                });
                evs
            }
            Some(MockResponse::Error(msg)) => {
                return Err(anyhow::anyhow!(msg));
            }
            None => {
                return Err(anyhow::anyhow!(
                    "Mock adapter response queue empty for stream_with_tools"
                ));
            }
        };

        Ok(Box::pin(futures::stream::iter(
            events.into_iter().map(Ok),
        )))
    }
}

impl ApiAdapter for MockAdapter {
    fn name(&self) -> &'static str {
        "mock"
    }

    fn default_model(&self) -> &str {
        &self.model
    }

    fn base_url(&self) -> &str {
        "http://mock"
    }

    fn build_request(
        &self,
        _messages: &[LlmMessage],
        _tools: Option<&[ToolDefinition]>,
        _options: &ChatOptions,
        _stream: bool,
    ) -> Result<(String, Value)> {
        Ok(("/mock/completions".to_string(), serde_json::json!({})))
    }

    fn parse_response(&self, _response: Value) -> Result<ChatResponse> {
        Ok(ChatResponse {
            content: vec![ContentBlock::Text {
                text: "mock".to_string(),
            }],
            tool_calls: vec![],
            stop_reason: StopReason::Stop,
            usage: crate::providers::TokenUsage::default(),
            provider: "mock".to_string(),
            model: self.model.clone(),
        })
    }

    fn parse_sse_event(&self, _data: &str) -> Result<Option<StreamEvent>> {
        Ok(None)
    }

    fn auth_config(&self, _api_key: &str) -> AuthConfig {
        AuthConfig::Bearer {
            token: "mock".to_string(),
        }
    }

    fn supports_native_tools(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mock_adapter_creation() {
        let adapter = MockAdapter::new("mock-model");
        assert_eq!(adapter.name(), "mock");
        assert_eq!(adapter.default_model(), "mock-model");
    }

    #[test]
    fn test_mock_queue_text() {
        let adapter = MockAdapter::new("mock-model");
        adapter.queue_text("Hello, world!");

        let response = adapter
            .chat_with_tools(&[], None, &ChatOptions::default())
            .unwrap();
        assert_eq!(response.content.len(), 1);
        assert!(matches!(&response.content[0], ContentBlock::Text { text } if text == "Hello, world!"));
    }

    #[test]
    fn test_mock_queue_tool_call() {
        let adapter = MockAdapter::new("mock-model");
        adapter.queue_tool_call("tc_1", "test_tool", serde_json::json!({"arg": 1}));

        let response = adapter
            .chat_with_tools(&[], None, &ChatOptions::default())
            .unwrap();
        assert_eq!(response.tool_calls.len(), 1);
        assert!(matches!(&response.tool_calls[0], ContentBlock::ToolCall { name, .. } if name == "test_tool"));
    }

    #[test]
    fn test_mock_queue_error() {
        let adapter = MockAdapter::new("mock-model");
        adapter.queue_error("Something went wrong");

        let result = adapter.chat_with_tools(&[], None, &ChatOptions::default());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Something went wrong"));
    }

    #[tokio::test]
    async fn test_mock_stream_text() {
        let adapter = MockAdapter::new("mock-model");
        adapter.queue_text("Streamed text");

        let stream = adapter
            .stream_with_tools(&[], None, &ChatOptions::default())
            .unwrap();
        let events: Vec<_> = futures::StreamExt::collect(stream).await;
        assert!(!events.is_empty());

        let texts: Vec<String> = events
            .into_iter()
            .filter_map(|r| r.ok())
            .filter_map(|e| match e {
                StreamEvent::TextDelta { delta, .. } => Some(delta),
                _ => None,
            })
            .collect();
        assert_eq!(texts.join(""), "Streamed text");
    }

    #[test]
    fn test_mock_records_requests() {
        let adapter = MockAdapter::new("mock-model");
        adapter.queue_text("Hi");

        let messages = vec![LlmMessage::user("Hello")];
        let tools = vec![ToolDefinition {
            name: "test".to_string(),
            description: "test tool".to_string(),
            parameters: serde_json::json!({}),
        }];

        let _ = adapter.chat_with_tools(&messages, Some(&tools), &ChatOptions::default());

        let recorded = adapter.recorded_requests();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].messages.len(), 1);
        assert_eq!(recorded[0].tools.len(), 1);
        assert!(!recorded[0].stream);
    }
}
