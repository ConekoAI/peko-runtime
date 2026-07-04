//! MCP sampling (`sampling/createMessage`) handler.
//!
//! Implements the server-to-client request handler that lets an MCP server ask
//! the host model for a completion. The request is translated into Peko's
//! `LlmResolver` / `Provider::chat_with_tools` path and the response is mapped
//! back to the MCP `CreateMessageResult` format.

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
use async_trait::async_trait;
use std::sync::Arc;
use tracing::debug;

/// Handles MCP `sampling/createMessage` requests from an MCP server.
pub struct SamplingRequestHandler {
    resolver: Arc<LlmResolver>,
}

impl SamplingRequestHandler {
    /// Create a new sampling handler backed by the given resolver.
    #[must_use]
    pub fn new(resolver: Arc<LlmResolver>) -> Self {
        Self { resolver }
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

        // Resolve the default provider/model and complete.
        let (provider, choice) = self
            .resolver
            .build(ResolveRequest::default())
            .await
            .map_err(|e| JsonRpcError {
                code: JsonRpcError::INTERNAL_ERROR,
                message: format!("Failed to resolve host model: {}", e),
                data: None,
            })?;

        let options = ChatOptions {
            max_tokens: req.max_tokens,
            ..Default::default()
        };

        let response = provider
            .chat_with_tools(&choice.model.id, &messages, &tools, &options)
            .await
            .map_err(|e| JsonRpcError {
                code: JsonRpcError::INTERNAL_ERROR,
                message: format!("Host model completion failed: {}", e),
                data: None,
            })?;

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

        let handler = SamplingRequestHandler::new(resolver);
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
}
