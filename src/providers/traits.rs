//! Provider trait
//!
//! Defines the interface for LLM providers with support for:
//! - Basic text completion
//! - Chat with message history
//! - Native tool calling via provider APIs
//! - Streaming responses with structured events

use async_trait::async_trait;
use futures::Stream;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::pin::Pin;
use tokio::sync::mpsc;

/// Unique content block ID for streaming correlation
pub type ContentBlockId = String;

/// Block type for streaming events
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockType {
    Text,
    ToolCall,
    Thinking,
}

/// Content delta for streaming
#[derive(Debug, Clone)]
pub enum ContentDelta {
    Text(String),
    ToolCall {
        name: Option<String>,
        arguments: Value,
    },
}

/// Tool definition for native tool calling
///
/// Providers translate this into their native tool schema format
/// (e.g., `OpenAI`'s function calling format, Anthropic's tool use)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    /// Tool name (must match the tool's registered name)
    pub name: String,
    /// Tool description for the model
    pub description: String,
    /// JSON Schema for tool parameters
    pub parameters: Value,
}

/// Streaming event from provider
///
/// Providers emit these events during streaming responses.
/// This allows the agent loop to handle incremental updates,
/// tool calls, and reasoning content.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// Stream started
    Start {
        /// Provider name
        provider: String,
        /// Model being used
        model: String,
    },
    /// Text content started
    TextStart {
        /// Index in the content array
        content_index: usize,
    },
    /// Text delta (incremental content)
    TextDelta {
        /// Index in the content array
        content_index: usize,
        /// Delta text
        delta: String,
    },
    /// Text content complete
    TextEnd {
        /// Index in the content array
        content_index: usize,
        /// Full text content
        content: String,
    },
    /// Thinking/reasoning started
    ThinkingStart {
        /// Index in the content array
        content_index: usize,
    },
    /// Thinking delta
    ThinkingDelta {
        /// Index in the content array
        content_index: usize,
        /// Delta thinking text
        delta: String,
    },
    /// Thinking complete
    ThinkingEnd {
        /// Index in the content array
        content_index: usize,
        /// Full thinking content
        content: String,
    },
    /// Tool call started
    ToolCallStart {
        /// Index in the content array
        content_index: usize,
    },
    /// Tool call delta (for streaming arguments)
    ToolCallDelta {
        /// Index in the content array
        content_index: usize,
        /// Delta (JSON fragment)
        delta: String,
    },
    /// Tool call complete
    ToolCallEnd {
        /// Index in the content array
        content_index: usize,
        /// Complete tool call
        tool_call: crate::types::message::ContentBlock,
    },
    /// Stream completed
    Done {
        /// Stop reason
        stop_reason: StopReason,
    },
    /// Token usage information (typically sent at end of stream)
    Usage {
        /// Input tokens
        input: u64,
        /// Output tokens
        output: u64,
        /// Total tokens
        total: u64,
    },
    /// Error occurred
    Error {
        /// Error message
        message: String,
    },
}

/// Why a response stopped
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// Normal completion
    Stop,
    /// Hit token limit
    Length,
    /// Tool use requested
    ToolUse,
    /// Error occurred
    Error,
    /// Aborted by user
    Aborted,
}

/// Chat message for native tool calling API
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    /// Message role: system, user, assistant, tool
    pub role: MessageRole,
    /// Message content blocks
    pub content: Vec<crate::types::message::ContentBlock>,
    /// Tool calls (for assistant messages)
    pub tool_calls: Option<Vec<crate::types::provider::ToolCall>>,
    /// Tool call ID (for tool messages)
    pub tool_call_id: Option<String>,
}

/// Message role for chat API
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

/// Options for chat completion
#[derive(Debug, Clone, Default)]
pub struct ChatOptions {
    /// Temperature (0.0 - 2.0)
    pub temperature: Option<f32>,
    /// Maximum tokens to generate
    pub max_tokens: Option<u32>,
    /// API key (optional - uses env var if not provided)
    pub api_key: Option<String>,
    /// Additional headers
    pub headers: std::collections::HashMap<String, String>,
}

/// Response from chat completion
#[derive(Debug, Clone)]
pub struct ChatResponse {
    /// Message content blocks
    pub content: Vec<crate::types::message::ContentBlock>,
    /// Tool calls (if any)
    pub tool_calls: Vec<crate::types::message::ContentBlock>,
    /// Stop reason
    pub stop_reason: StopReason,
    /// Token usage
    pub usage: TokenUsage,
    /// Provider name
    pub provider: String,
    /// Model used
    pub model: String,
}

/// Token usage statistics
#[derive(Debug, Clone, Default)]
pub struct TokenUsage {
    /// Input tokens
    pub input: u64,
    /// Output tokens
    pub output: u64,
    /// Total tokens
    pub total: u64,
}

/// LLM Provider trait
#[async_trait]
pub trait Provider: Send + Sync {
    fn name(&self) -> &str;

    /// Complete a prompt (legacy/simple interface)
    async fn complete(&self, prompt: &str) -> anyhow::Result<String> {
        self.chat(prompt, "default", 0.7).await
    }

    /// Chat with optional system prompt (zeroclaw-compatible interface)
    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String>;

    /// Simple chat interface
    async fn chat(&self, message: &str, model: &str, temperature: f64) -> anyhow::Result<String> {
        self.chat_with_system(None, message, model, temperature)
            .await
    }

    /// Warm up the HTTP connection pool
    async fn warmup(&self) -> anyhow::Result<()> {
        Ok(())
    }

    /// Chat with native tool calling support (blocking)
    ///
    /// This is the primary interface for agentic loops.
    /// Providers implement native tool calling via their API.
    ///
    /// Default implementation falls back to legacy chat and ignores tools.
    async fn chat_with_tools(
        &self,
        messages: &[ChatMessage],
        _tools: &[ToolDefinition],
        _options: &ChatOptions,
    ) -> anyhow::Result<ChatResponse> {
        // Default: use legacy chat with text conversion
        let prompt = messages_to_prompt(messages);
        let response = self.complete(&prompt).await?;

        Ok(ChatResponse {
            content: vec![crate::types::message::ContentBlock::Text { text: response }],
            tool_calls: vec![],
            stop_reason: StopReason::Stop,
            usage: TokenUsage::default(),
            provider: self.name().to_string(),
            model: "default".to_string(),
        })
    }

    /// Stream chat with native tool calling support
    ///
    /// Returns a stream of events for real-time updates.
    /// Events include text deltas, tool calls, and reasoning.
    ///
    /// Default implementation returns an error - providers must implement this.
    async fn stream_with_tools(
        &self,
        _messages: &[ChatMessage],
        _tools: &[ToolDefinition],
        _options: &ChatOptions,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = anyhow::Result<StreamEvent>> + Send>>> {
        anyhow::bail!("stream_with_tools not implemented for this provider")
    }

    /// Helper function to check if this provider supports native tool calling
    fn supports_native_tools(&self) -> bool {
        // Override in providers that implement native tool calling
        false
    }

    /// Stream completion with events (legacy interface)
    ///
    /// Default implementation falls back to blocking `complete()`
    /// and emits a single Assistant event at the end.
    async fn complete_stream(
        &self,
        _prompt: &str,
        event_tx: mpsc::Sender<crate::engine::AgenticEvent>,
        run_id: String,
    ) -> anyhow::Result<()> {
        // Default: fall back to blocking completion
        use crate::engine::{AgenticEvent, LifecyclePhase};

        // Emit start event
        let _ = event_tx
            .send(AgenticEvent::Lifecycle {
                run_id: run_id.clone(),
                phase: LifecyclePhase::Start,
                error: None,
            })
            .await;

        // Emit running event
        let _ = event_tx
            .send(AgenticEvent::Lifecycle {
                run_id: run_id.clone(),
                phase: LifecyclePhase::Running,
                error: None,
            })
            .await;

        // Do blocking completion
        match self.complete(_prompt).await {
            Ok(response) => {
                // Emit assistant event using new event type
                let _ = event_tx
                    .send(AgenticEvent::AssistantText {
                        run_id: run_id.clone(),
                        text: response,
                        sequence: 1,
                        is_interstitial: false, // Final answer
                    })
                    .await;

                // Emit end event
                let _ = event_tx
                    .send(AgenticEvent::Lifecycle {
                        run_id,
                        phase: LifecyclePhase::End,
                        error: None,
                    })
                    .await;

                Ok(())
            }
            Err(e) => {
                // Emit error event
                let _ = event_tx
                    .send(AgenticEvent::Lifecycle {
                        run_id,
                        phase: LifecyclePhase::Error,
                        error: Some(e.to_string()),
                    })
                    .await;

                Err(e)
            }
        }
    }
}

/// Helper: Convert chat messages to a single prompt string (for fallback)
fn messages_to_prompt(messages: &[ChatMessage]) -> String {
    messages
        .iter()
        .map(|m| {
            let role = match m.role {
                MessageRole::System => "System",
                MessageRole::User => "User",
                MessageRole::Assistant => "Assistant",
                MessageRole::Tool => "Tool",
            };
            let content = m
                .content
                .iter()
                .map(|b| match b {
                    crate::types::message::ContentBlock::Text { text } => text.clone(),
                    _ => String::new(),
                })
                .collect::<String>();
            format!("{role}: {content}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}
