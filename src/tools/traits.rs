//! Tool trait

use crate::tools::context::ToolContext;
use async_trait::async_trait;

/// Errors that can occur during tool execution
pub use crate::tools::context::ToolError;

/// Tool trait for agent capabilities
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;

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

        // Execute using the basic method
        let result = self.execute(params).await;

        // Check abort after completion
        if ctx.is_aborted() {
            return Err(ToolError::Aborted.into());
        }

        // Check timeout after completion
        ctx.check_timeout(start_time)?;

        // Report completion
        ctx.report_status(format!("Completed {}", self.name())).await;

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
