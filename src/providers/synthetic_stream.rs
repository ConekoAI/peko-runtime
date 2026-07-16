//! Synthetic stream generation for blocking providers
//!
//! Converts a blocking `ChatResponse` into a `Stream` of `StreamEvent`s
//! so the unified agentic loop can process all providers uniformly.

use crate::providers::{ChatResponse, ContentBlock, StreamEvent};
use futures::Stream;
use std::pin::Pin;

/// Synthesize a streaming response from a blocking provider response.
///
/// For providers that don't support SSE streaming, we call `chat_with_tools`
/// and emit synthetic `StreamEvent`s so the unified loop can process them
/// exactly like real streaming events.
pub fn synthesize_stream_from_blocking(
    response: ChatResponse,
    provider_name: &str,
) -> Pin<Box<dyn Stream<Item = anyhow::Result<StreamEvent>> + Send>> {
    let mut events: Vec<anyhow::Result<StreamEvent>> = Vec::new();

    events.push(Ok(StreamEvent::Start {
        provider: provider_name.to_string(),
        model: "default".to_string(),
    }));

    // Emit text content as a single delta
    let text: String = response
        .content
        .iter()
        .filter_map(|b| {
            if let ContentBlock::Text { text } = b {
                Some(text.as_str())
            } else {
                None
            }
        })
        .collect();
    if !text.is_empty() {
        events.push(Ok(StreamEvent::TextDelta {
            content_index: 0,
            delta: text,
        }));
        events.push(Ok(StreamEvent::TextEnd {
            content_index: 0,
            content: String::new(),
        }));
    }

    // Emit thinking content
    let thinking: String = response
        .content
        .iter()
        .filter_map(|b| {
            if let ContentBlock::Thinking { text, .. } = b {
                Some(text.as_str())
            } else {
                None
            }
        })
        .collect();
    if !thinking.is_empty() {
        events.push(Ok(StreamEvent::ThinkingDelta {
            content_index: 0,
            delta: thinking.clone(),
        }));
        events.push(Ok(StreamEvent::ThinkingEnd {
            content_index: 0,
            content: thinking,
        }));
    }

    // Emit tool calls
    for (i, tc) in response.tool_calls.iter().enumerate() {
        if let ContentBlock::ToolCall { .. } = tc {
            events.push(Ok(StreamEvent::ToolCallEnd {
                content_index: i,
                tool_call: tc.clone(),
            }));
        }
    }

    // Usage — forward the full breakdown so the engine accumulator
    // can fold cache / reasoning into the canonical input/output
    // buckets and also preserve the raw counts in the session JSONL.
    events.push(Ok(StreamEvent::Usage {
        input: response.usage.input,
        output: response.usage.output,
        total: response.usage.total,
        cache_creation_input_tokens: response.usage.cache_creation_input_tokens.unwrap_or(0),
        cache_read_input_tokens: response.usage.cache_read_input_tokens.unwrap_or(0),
        reasoning_output_tokens: response.usage.reasoning_output_tokens.unwrap_or(0),
    }));

    // Done
    events.push(Ok(StreamEvent::Done {
        stop_reason: response.stop_reason,
    }));

    Box::pin(futures::stream::iter(events))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{StopReason, TokenUsage};

    #[tokio::test]
    async fn test_synthesize_stream_basic() {
        let response = ChatResponse {
            content: vec![ContentBlock::Text {
                text: "Hello".to_string(),
            }],
            tool_calls: vec![],
            usage: TokenUsage::default(),
            stop_reason: StopReason::Stop,
            provider: "test".to_string(),
            model: "test".to_string(),
        };

        let stream = synthesize_stream_from_blocking(response, "test");
        let events: Vec<_> = futures::StreamExt::collect(stream).await;
        assert!(!events.is_empty());
    }
}
