//! MCP sampling (`sampling/createMessage`) handler.
//!
//! Implements the server-to-client request handler that lets an MCP server ask
//! the host model for a completion. The request is translated into Peko's
//! `LlmResolver` / `Provider::chat_with_tools` path and the response is mapped
//! back to the MCP `CreateMessageResult` format.
//!
//! Sampling binds the owner meter and an explicit principal/server model.
//! Missing model configuration fails closed; native callbacks require an active
//! owner request and never start principal runs.
//!
//! 2026-09 cleanup: migrated from `MeteredProvider` to
//! `StackedMeteredProvider` so cost folding into `budget_per_cycle`
//! is restored for MCP sampling (the legacy wrapper did not fold cost).

use crate::extensions::mcp::protocol::{
    client::ServerRequestHandler,
    types::{
        CreateMessageRequest, CreateMessageResult, JsonRpcError, SamplingContent, SamplingMessage,
        SamplingRole, Tool,
    },
};
use async_trait::async_trait;
use base64::Engine;
use peko_engine::StackedMeteredProvider;
use peko_message::{extract_dimensions_from_base64, ContentBlock, ImageSource, MessageRole};
use peko_provider_api::{ChatOptions, StopReason, ToolDefinition};
use peko_providers::resolver::{LlmResolver, ResolveRequest};
use peko_quota::{QuotaMeter, QuotaScope};
use std::sync::Arc;
use tracing::debug;

/// Handles MCP `sampling/createMessage` requests from an MCP server.
pub struct SamplingRequestHandler {
    resolver: Arc<LlmResolver>,
    /// The actual owner meter, supplied explicitly at connection startup.
    meter: Arc<QuotaMeter>,
    model: Arc<std::sync::RwLock<Option<String>>>,
    max_output_tokens: u32,
}

impl SamplingRequestHandler {
    /// Create a new sampling handler backed by the given resolver.
    #[must_use]
    pub fn new(resolver: Arc<LlmResolver>, meter: Arc<QuotaMeter>) -> Self {
        Self {
            resolver,
            meter,
            model: Arc::new(std::sync::RwLock::new(None)),
            max_output_tokens: 4096,
        }
    }

    pub fn with_model(
        mut self,
        model: Arc<std::sync::RwLock<Option<String>>>,
        max_output_tokens: u32,
    ) -> Self {
        self.model = model;
        self.max_output_tokens = max_output_tokens;
        self
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
                messages.push(peko_message::LlmMessage::system(system_prompt));
            }
        }

        for SamplingMessage { role, content } in req.messages {
            let role = match role {
                SamplingRole::User => MessageRole::User,
                SamplingRole::Assistant => MessageRole::Assistant,
            };
            let content = convert_sampling_content(content);
            messages.push(peko_message::LlmMessage {
                role,
                content: vec![content],
                timestamp: chrono::Utc::now(),
                metadata: std::collections::HashMap::new(),
                tool_call_id: None,
                usage: None,
            });
        }

        // Convert optional MCP tools to Peko tool definitions.
        let tools: Vec<ToolDefinition> = req
            .tools
            .unwrap_or_default()
            .iter()
            .map(convert_mcp_tool)
            .collect();

        // Use the explicitly bound model; server preferences cannot select another account.
        let agent_model = self
            .model
            .read()
            .map_err(|_| JsonRpcError {
                code: JsonRpcError::INTERNAL_ERROR,
                message: "Sampling model lock unavailable".into(),
                data: None,
            })?
            .clone()
            .ok_or_else(|| JsonRpcError {
                code: JsonRpcError::INTERNAL_ERROR,
                message: "No principal model configured for MCP sampling".to_string(),
                data: None,
            })?;
        let (provider, choice) = self
            .resolver
            .build_view(ResolveRequest {
                agent_model: Some(&agent_model),
                ..Default::default()
            })
            .await
            .map_err(|e| JsonRpcError {
                code: JsonRpcError::INTERNAL_ERROR,
                message: format!("Failed to resolve host model: {}", e),
                data: None,
            })?;
        let model_id = choice.model_id.clone();

        if let Some(ceiling) = self.meter.config().cost_per_call_max {
            if let Some(pricing) = self
                .resolver
                .catalog()
                .get(&agent_model)
                .await
                .and_then(|config| config.spec.and_then(|spec| spec.pricing))
            {
                let input_bound = serde_json::to_vec(&(&messages, &tools))
                    .map_or(4096, |messages| messages.len() as u64)
                    .max(4096);
                let output_bound = u64::from(
                    req.max_tokens
                        .unwrap_or(self.max_output_tokens)
                        .min(self.max_output_tokens),
                )
                .max(1000);
                let estimated =
                    peko_engine::compute_cost_usd(Some(pricing), input_bound, output_bound);
                if estimated > ceiling {
                    return Err(JsonRpcError {
                        code: JsonRpcError::INTERNAL_ERROR,
                        message: "MCP sampling refused by cost_per_call_max".into(),
                        data: None,
                    });
                }
            }
        }

        // F19: open `QuotaScope::with` so the `StackedMeteredProvider`
        // built below auto-charges this server's principal. We move the
        // provider into the closure (consumed) and rebuild it as
        // metered; the unwrapped response is what we return.
        let meter = Arc::clone(&self.meter);
        let options = ChatOptions {
            max_tokens: Some(
                req.max_tokens
                    .unwrap_or(self.max_output_tokens)
                    .min(self.max_output_tokens),
            ),
            ..Default::default()
        };

        let response_result = QuotaScope::with(meter, async move {
            let metered = StackedMeteredProvider::from_current_scope(provider);
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
            model: choice.model_id.clone(),
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
        SamplingContent::Image { data, mime_type } => {
            let dimensions = base64::engine::general_purpose::STANDARD
                .decode(&data)
                .ok()
                .and_then(|bytes| extract_dimensions_from_base64(&bytes, &mime_type));
            ContentBlock::Image {
                source: ImageSource::Base64 { data, dimensions },
                mime_type,
            }
        }
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
        let adapter = peko_providers::MockAdapter::new();
        adapter.queue_text("Hello from the host model");
        let tmp = tempfile::tempdir().unwrap();
        let catalog_path = tmp.path().join("models.toml");
        let (resolver, _adapter) = LlmResolver::mock(adapter, &catalog_path).await;

        let model = resolver.catalog().list_enabled().await[0].id.clone();
        let handler =
            SamplingRequestHandler::new(resolver, Arc::new(peko_quota::QuotaMeter::unlimited()))
                .with_model(Arc::new(std::sync::RwLock::new(Some(model))), 4096);
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
    /// `handle_request`, so a `StackedMeteredProvider` constructed
    /// there auto-charges the right meter.
    #[tokio::test]
    async fn test_sampling_handler_charges_principal_meter() {
        let adapter = peko_providers::MockAdapter::new();
        // Queue two completions so we can run two sampling requests
        // and verify each charges the meter independently.
        adapter.queue_text("first");
        adapter.queue_text("second");
        let tmp = tempfile::tempdir().unwrap();
        let catalog_path = tmp.path().join("models.toml");
        let (resolver, _adapter) = LlmResolver::mock(adapter, &catalog_path).await;

        // Build a meter with a high input-token limit so a successful
        // charge is observable via `snapshot()` without tripping.
        let meter = Arc::new(
            peko_quota::QuotaMeter::load_or_init(
                peko_quota::QuotaConfig {
                    input_tokens: Some(1_000_000),
                    ..Default::default()
                },
                None,
                chrono::Utc::now(),
            )
            .await
            .unwrap(),
        );

        let model = resolver.catalog().list_enabled().await[0].id.clone();
        let handler = SamplingRequestHandler::new(Arc::clone(&resolver), Arc::clone(&meter))
            .with_model(Arc::new(std::sync::RwLock::new(Some(model))), 4096);
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
