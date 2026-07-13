//! MCP sampling (`sampling/createMessage`) handler.
//!
//! Implements the server-to-client request handler that lets an MCP server ask
//! the host model for a completion. The request is translated into Peko's
//! `LlmResolver` / `Provider::chat_with_tools` path and the response is mapped
//! back to the MCP `CreateMessageResult` format.
//!
//! F19: `SamplingRequestHandler` carries the principal's quota meter
//! (captured at server-start time) and opens a `QuotaScope::with`
//! around the LLM call so a `MeteredProvider` constructed inside
//! auto-charges the right principal. Daemon-level auto-start passes
//! an unlimited meter (no principal); tool-call-driven auto-start
//! captures the principal from `ToolContext::principal_id` and passes
//! the matching `Arc<QuotaMeter>`.

use crate::common::types::message::{ContentBlock, ImageSource, MessageRole};
use crate::extensions::mcp::protocol::{
    client::ServerRequestHandler,
    types::{
        CreateMessageRequest, CreateMessageResult, JsonRpcError, SamplingContent, SamplingMessage,
        SamplingRole, Tool,
    },
};
use crate::providers::resolver::{LlmResolver, ResolveRequest};
use crate::providers::traits::{ChatOptions, StopReason, ToolDefinition};
use crate::providers::MeteredProvider;
use crate::quota::{QuotaMeter, QuotaScope};
use async_trait::async_trait;
use std::sync::Arc;
use tracing::debug;

/// Handles MCP `sampling/createMessage` requests from an MCP server.
pub struct SamplingRequestHandler {
    resolver: Arc<LlmResolver>,
    /// F19: principal's quota meter for this server. Built once at
    /// `McpManager::start_server` time — either the principal's real
    /// meter (tool-call-driven auto-start with a `principal_id`) or an
    /// unlimited meter (daemon-level auto-start with `None`).
    meter: Arc<QuotaMeter>,
}

impl SamplingRequestHandler {
    /// Create a new sampling handler backed by the given resolver.
    #[must_use]
    pub fn new(resolver: Arc<LlmResolver>, meter: Arc<QuotaMeter>) -> Self {
        Self { resolver, meter }
    }
}

#[async_trait]
impl ServerRequestHandler for SamplingRequestHandler {
    async fn handle_request(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> std::result::Result<serde_json::Value, JsonRpcError> {
        if method != "sampling/createMessage" {
            return Err(JsonRpcError {
                code: JsonRpcError::METHOD_NOT_FOUND,
                message: format!("Method '{}' not found", method),
                data: None,
            });
        }

        let req: CreateMessageRequest = match params {
            Some(params) => serde_json::from_value(params).map_err(|e| JsonRpcError {
                code: JsonRpcError::INVALID_PARAMS,
                message: format!("Invalid sampling request parameters: {}", e),
                data: None,
            })?,
            None => {
                return Err(JsonRpcError {
                    code: JsonRpcError::INVALID_PARAMS,
                    message: "Missing sampling request parameters".to_string(),
                    data: None,
                });
            }
        };

        debug!(
            "Handling sampling/createMessage request with {} message(s)",
            req.messages.len()
        );

        // Build the message list for the host model.
        let mut messages = Vec::new();

        // Prepend an explicit system prompt if the server provided one.
        if let Some(system_prompt) = req.system_prompt {
            if !system_prompt.is_empty() {
                messages.push(crate::common::types::message::LlmMessage::system(
                    system_prompt,
                ));
            }
        }

        for SamplingMessage { role, content } in req.messages {
            let role = match role {
                SamplingRole::User => MessageRole::User,
                SamplingRole::Assistant => MessageRole::Assistant,
            };
            let content = convert_sampling_content(content);
            messages.push(crate::common::types::message::LlmMessage {
                role,
                content: vec![content],
                timestamp: chrono::Utc::now(),
                metadata: std::collections::HashMap::new(),
                tool_call_id: None,
            });
        }

        // Convert optional MCP tools to Peko tool definitions.
        let tools: Vec<ToolDefinition> = req
            .tools
            .unwrap_or_default()
            .iter()
            .map(convert_mcp_tool)
            .collect();

        // Resolve the default provider/model first. `resolver.build`
        // is not metered (it just resolves config) and we need `choice`
        // outside the QuotaScope to fill `CreateMessageResult::model`.
        let (provider, choice) = self
            .resolver
            .build(ResolveRequest::default())
            .await
            .map_err(|e| JsonRpcError {
                code: JsonRpcError::INTERNAL_ERROR,
                message: format!("Failed to resolve host model: {}", e),
                data: None,
            })?;
        let model_id = choice.model.id.clone();

        // F19: open `QuotaScope::with` so the `MeteredProvider` built
        // below auto-charges this server's principal. We move the
        // provider into the closure (consumed) and rebuild it as
        // metered; the unwrapped response is what we return.
        let meter = Arc::clone(&self.meter);
        let options = ChatOptions {
            max_tokens: req.max_tokens,
            ..Default::default()
        };

        let response_result = QuotaScope::with(meter, async move {
            let metered = MeteredProvider::from_current_scope(provider);
            metered
                .chat_with_tools(&model_id, &messages, &tools, &options)
                .await
                .map_err(|e| JsonRpcError {
                    code: JsonRpcError::INTERNAL_ERROR,
                    message: format!("Host model completion failed: {}", e),
                    data: None,
                })
        })
        .await;

        let response = response_result?;

        // Extract the assistant-facing content. If the model returned tool calls
        // instead of text, serialize them as JSON text so the server still gets a
        // text result.
        let text_content = extract_text_content(&response.content);
        let content = if text_content.is_empty() && !response.tool_calls.is_empty() {
            serde_json::to_string(&response.tool_calls).unwrap_or_default()
        } else {
            text_content
        };

        let result = CreateMessageResult {
            role: SamplingRole::Assistant,
            content: SamplingContent::Text { text: content },
            model: choice.model.id.clone(),
            stop_reason: Some(map_stop_reason(&response.stop_reason)),
        };

        serde_json::to_value(result).map_err(|e| JsonRpcError {
            code: JsonRpcError::INTERNAL_ERROR,
            message: format!("Failed to serialize sampling result: {}", e),
            data: None,
        })
    }
}

fn convert_sampling_content(content: SamplingContent) -> ContentBlock {
    match content {
        SamplingContent::Text { text } => ContentBlock::Text { text },
        SamplingContent::Image { data, mime_type } => ContentBlock::Image {
            source: ImageSource::Base64 { data },
            mime_type,
        },
    }
}

fn convert_mcp_tool(tool: &Tool) -> ToolDefinition {
    ToolDefinition {
        name: tool.name.clone(),
        description: tool.description.clone(),
        parameters: tool.input_schema.clone(),
    }
}

fn extract_text_content(blocks: &[ContentBlock]) -> String {
    let mut text = String::new();
    for block in blocks {
        if let ContentBlock::Text { text: t } = block {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(t);
        }
    }
    text
}

fn map_stop_reason(stop_reason: &StopReason) -> String {
    match stop_reason {
        StopReason::Stop => "stop",
        StopReason::Length => "length",
        StopReason::ToolUse => "tool_use",
        StopReason::Error => "error",
        StopReason::Aborted => "aborted",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extensions::mcp::protocol::types::{
        CreateMessageRequest, SamplingMessage, SamplingRole,
    };
    use serde_json::json;

    #[tokio::test]
    async fn test_sampling_create_message_handler() {
        let adapter = crate::providers::MockAdapter::new();
        adapter.queue_text("Hello from the host model");
        let tmp = tempfile::tempdir().unwrap();
        let catalog_path = tmp.path().join("providers.toml");
        let (resolver, _adapter) = LlmResolver::mock(adapter, &catalog_path).await;

        let handler = SamplingRequestHandler::new(
            resolver,
            Arc::new(crate::quota::QuotaMeter::unlimited()),
        );
        let req = CreateMessageRequest {
            messages: vec![SamplingMessage {
                role: SamplingRole::User,
                content: SamplingContent::Text {
                    text: "Say hello".to_string(),
                },
            }],
            model_preferences: None,
            system_prompt: Some("You are a test assistant.".to_string()),
            max_tokens: Some(100),
            tools: None,
            include_context: None,
        };

        let result = handler
            .handle_request("sampling/createMessage", Some(json!(req)))
            .await
            .unwrap();
        let result: CreateMessageResult = serde_json::from_value(result).unwrap();
        assert!(matches!(result.role, SamplingRole::Assistant));
        assert_eq!(result.model, "mock-model");
        assert!(
            matches!(result.content, SamplingContent::Text { ref text } if text == "Hello from the host model")
        );
        assert_eq!(result.stop_reason, Some("stop".to_string()));
    }

    #[test]
    fn test_sampling_message_conversion() {
        let msg = SamplingMessage {
            role: SamplingRole::User,
            content: SamplingContent::Text {
                text: "hello".to_string(),
            },
        };
        let block = convert_sampling_content(msg.content);
        assert!(matches!(block, ContentBlock::Text { text } if text == "hello"));
    }

    #[test]
    fn test_convert_mcp_tool() {
        let tool = Tool {
            name: "test".to_string(),
            description: "desc".to_string(),
            input_schema: json!({"type": "object"}),
        };
        let def = convert_mcp_tool(&tool);
        assert_eq!(def.name, "test");
        assert_eq!(def.description, "desc");
    }

    /// F19: a `SamplingRequestHandler` built with a real
    /// (non-unlimited) `QuotaMeter` must charge that meter on every
    /// sampling request — the quota scope is opened inside
    /// `handle_request`, so a `MeteredProvider` constructed there auto-
    /// charges the right meter.
    #[tokio::test]
    async fn test_sampling_handler_charges_principal_meter() {
        let adapter = crate::providers::MockAdapter::new();
        // Queue two completions so we can run two sampling requests
        // and verify each charges the meter independently.
        adapter.queue_text("first");
        adapter.queue_text("second");
        let tmp = tempfile::tempdir().unwrap();
        let catalog_path = tmp.path().join("providers.toml");
        let (resolver, _adapter) = LlmResolver::mock(adapter, &catalog_path).await;

        // Build a meter with a high input-token limit so a successful
        // charge is observable via `snapshot()` without tripping.
        let meter = Arc::new(
            crate::quota::QuotaMeter::load_or_init(
                crate::quota::QuotaConfig {
                    input_tokens: Some(1_000_000),
                    ..Default::default()
                },
                None,
                chrono::Utc::now(),
            )
            .await
            .unwrap(),
        );

        let handler = SamplingRequestHandler::new(Arc::clone(&resolver), Arc::clone(&meter));
        let req = CreateMessageRequest {
            messages: vec![SamplingMessage {
                role: SamplingRole::User,
                content: SamplingContent::Text {
                    text: "Say hi".to_string(),
                },
            }],
            model_preferences: None,
            system_prompt: None,
            max_tokens: Some(50),
            tools: None,
            include_context: None,
        };

        let before = meter.snapshot();
        assert_eq!(before.output_tokens, 0);

        handler
            .handle_request("sampling/createMessage", Some(json!(req.clone())))
            .await
            .unwrap();
        let after_first = meter.snapshot();
        assert!(
            after_first.output_tokens > before.output_tokens,
            "expected meter to be charged after first sampling request: before={}, after={}",
            before.output_tokens,
            after_first.output_tokens
        );
        // request_count should always increment by 1 per call.
        assert_eq!(after_first.request_count, 1);

        handler
            .handle_request("sampling/createMessage", Some(json!(req)))
            .await
            .unwrap();
        let after_second = meter.snapshot();
        assert!(
            after_second.output_tokens > after_first.output_tokens,
            "expected meter to accumulate across sampling requests: first={}, second={}",
            after_first.output_tokens,
            after_second.output_tokens
        );
        assert_eq!(after_second.request_count, 2);
    }
}
