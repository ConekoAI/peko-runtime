//! Tool trait

use crate::observability::performance::GLOBAL_METRICS;
use crate::tools::core::exec::{ToolContext, ToolError};
use async_trait::async_trait;

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

    /// Execute the tool with parameters.
    ///
    /// ⚠️ **TEST-ONLY IN PRODUCTION CONTEXTS**
    ///
    /// Production code must route tool execution through `ExtensionCore::invoke_hook`
    /// (or `ToolRuntime::execute_tool`) to ensure consistent behavior:
    /// - Workspace injection
    /// - Reserved parameter validation/injection
    /// - Tool permission checks (ADR-019)
    /// - Abort/timeout handling
    /// - Progress reporting
    /// - Metrics collection
    ///
    /// Direct calls to this method are appropriate for:
    /// - Unit tests of individual tools
    /// - The `BuiltinToolAdapter` wrapper (which bridges into ExtensionCore)
    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value>;

    /// Execute with full context (abort signal + progress callbacks).
    ///
    /// This is the canonical execution method on the trait. The default implementation
    /// delegates to `execute` for backward compatibility, but tools that support progress
    /// reporting should override this.
    ///
    /// ⚠️ **TEST-ONLY IN PRODUCTION CONTEXTS** — see note on `execute`.
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
