//! Tool trait

use crate::observability::performance::GLOBAL_METRICS;
use crate::tools::context::ToolContext;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Errors that can occur during tool execution
pub use crate::tools::context::ToolError;

/// Result of a tool execution
///
/// This is a structured result that can represent success or failure,
/// with optional metadata for async tool execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolResult {
    /// Whether the tool execution succeeded
    pub success: bool,
    /// The result data (if success)
    pub data: Option<Value>,
    /// Error message (if failure)
    pub error: Option<String>,
    /// Optional metadata
    pub metadata: Option<Value>,
}

impl ToolResult {
    /// Create a successful tool result
    pub fn success(data: impl Into<Value>) -> Self {
        Self {
            success: true,
            data: Some(data.into()),
            error: None,
            metadata: None,
        }
    }

    /// Create a failed tool result
    pub fn failure(error: impl Into<String>) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(error.into()),
            metadata: None,
        }
    }

    /// Create a failed tool result with a standard error
    #[must_use]
    pub fn error(err: anyhow::Error) -> Self {
        Self::failure(err.to_string())
    }

    /// Add metadata to the result
    pub fn with_metadata(mut self, metadata: impl Into<Value>) -> Self {
        self.metadata = Some(metadata.into());
        self
    }

    /// Convert to JSON value for LLM consumption
    #[must_use]
    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or_else(|_| {
            serde_json::json!({
                "success": false,
                "error": "Failed to serialize tool result"
            })
        })
    }
}

/// Tool trait for agent capabilities
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;

    /// Get LLM-optimized description with usage guidance.
    ///
    /// This should include "Use when:" and "Don't use when:" guidance
    /// to help the LLM select the right tool.
    ///
    /// Example: "Execute terminal commands. Use when: running build/test commands,
    /// inspecting system state. Don't use when: a safer dedicated tool exists."
    fn description(&self) -> String;

    /// Convert to Any for downcasting
    ///
    /// This enables downcasting from `Arc<dyn Tool>` to concrete types
    /// for capability detection and trait implementation checking.
    fn as_any(&self) -> &dyn std::any::Any {
        // Default implementation panics - tools must override
        panic!("as_any not implemented for this tool")
    }

    /// Get the JSON Schema for this tool's parameters
    ///
    /// This is used for native tool calling APIs (`OpenAI`, Anthropic, etc.)
    /// Default implementation returns an empty object schema.
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {},
            "required": []
        })
    }

    /// Execute the tool with parameters
    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value>;

    /// Execute with full context (abort signal + progress callbacks)
    ///
    /// Default implementation delegates to `execute` for backward compatibility.
    /// Tools that want to support abort/updates should override this.
    ///
    /// # Arguments
    /// * `params` - Tool parameters from the LLM
    /// * `ctx` - Execution context with abort signal and progress callback
    ///
    /// # Returns
    /// Tool result or error (including abort errors)
    async fn execute_with_context(
        &self,
        params: serde_json::Value,
        ctx: &ToolContext,
    ) -> anyhow::Result<serde_json::Value> {
        use std::time::Instant;

        // Check abort before starting
        if ctx.is_aborted() {
            return Err(ToolError::Aborted.into());
        }

        // Check timeout before starting
        let start_time = Instant::now();
        ctx.check_timeout(start_time)?;

        // Report start status
        ctx.report_status(format!("Starting {}", self.name())).await;

        // Execute using the basic method with performance measurement (REQ-PF-004: < 5ms target)
        let tool_name = self.name().to_string();
        let result = self.execute(params).await;
        let elapsed = start_time.elapsed();

        // Record tool latency for performance monitoring
        GLOBAL_METRICS.record_tool_latency(&tool_name, elapsed);

        // Check abort after completion
        if ctx.is_aborted() {
            return Err(ToolError::Aborted.into());
        }

        // Check timeout after completion
        ctx.check_timeout(start_time)?;

        // Report completion
        ctx.report_status(format!("Completed {tool_name} in {elapsed:?}"))
            .await;

        result
    }

    /// Check if this tool supports progress updates
    ///
    /// Returns true if the tool implements custom progress reporting
    /// via `execute_with_context`. Default is false.
    fn supports_progress(&self) -> bool {
        false
    }

    /// Estimate execution duration for this tool call
    ///
    /// Returns an estimated duration in milliseconds.
    /// Used by the agent loop to decide whether to emit progress events.
    fn estimated_duration_ms(&self, _params: &serde_json::Value) -> u64 {
        1000 // Default 1 second
    }
}
