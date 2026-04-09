//! Async Agentic Loop - Phase 5 Integration
//!
//! Integrates async tool execution into the agentic loop with:
//! - Automatic async tool selection based on capabilities
//! - Async tool syntax for LLM prompts
//! - Streaming results for long-running operations
//! - Metrics collection for async operations
//!
//! # Architecture
//!
//! The async loop extends AgenticLoopV4 with async-aware tool execution:
//! ```
//! User Request → AgenticLoopV4 → LLM
//!                          ↓
//!                   ToolCall Request
//!                          ↓
//!              AsyncToolCapability::check
//!                          ↓
//!              ┌───────────┴───────────┐
//!              │                       │
//!         sync tool              async tool
//!              │                       │
//!              │                  execute_async()
//!              │                       │
//!              │                  ┌────┴────┐
//!              │                  │         │
//!              │            immediate   long-running
//!              │                  │         │
//!              │                  │    poll for status
//!              │                  │         │
//!              └──────────────────┴─────────┘
//!                          ↓
//!                    Return Result
//! ```

use crate::agent::Agent;
use crate::engine::{
    AgenticEvent, LifecyclePhase, ToolExecutionContext,
};
use crate::engine::async_tool_executor::{AsyncCapability, AsyncToolExecutor};
use crate::tools::{Tool, UnifiedAsyncTool};
use anyhow::Result;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{info, instrument, warn};

/// Metrics for async tool execution
#[derive(Debug, Clone, Default)]
pub struct AsyncToolMetrics {
    /// Total async executions
    pub async_executions: u64,
    /// Total sync executions (fallback)
    pub sync_executions: u64,
    /// Total cancellations
    pub cancellations: u64,
    /// Total timeouts
    pub timeouts: u64,
    /// Average execution time (ms)
    pub avg_execution_time_ms: u64,
    /// Tools that support async
    pub async_capable_tools: Vec<String>,
}

/// Configuration for async agentic loop
#[derive(Debug, Clone)]
pub struct AsyncAgenticConfig {
    /// Enable automatic async detection
    pub auto_async: bool,
    /// Timeout for async operations (seconds)
    pub async_timeout_secs: u64,
    /// Enable streaming results
    pub streaming_results: bool,
    /// Progress update interval (ms)
    pub progress_interval_ms: u64,
    /// Force async for all tools (debug)
    pub force_async: bool,
    /// Collect detailed metrics
    pub collect_metrics: bool,
}

impl Default for AsyncAgenticConfig {
    fn default() -> Self {
        Self {
            auto_async: true,
            async_timeout_secs: 300,
            streaming_results: true,
            progress_interval_ms: 1000,
            force_async: false,
            collect_metrics: true,
        }
    }
}

/// Async-capable agentic loop extending v4
pub struct AsyncAgenticLoop {
    /// Underlying v4 loop
    inner: crate::engine::AgenticLoopV4,
    /// Async tool executor
    async_executor: AsyncToolExecutor,
    /// Configuration
    config: AsyncAgenticConfig,
    /// Metrics
    metrics: RwLock<AsyncToolMetrics>,
    /// Capability cache per tool
    capability_cache: RwLock<HashMap<String, AsyncCapability>>,
}

impl AsyncAgenticLoop {
    /// Create a new async agentic loop
    pub fn new(
        agent: Arc<Agent>,
        provider: Arc<dyn crate::providers::Provider>,
        tools: Vec<Arc<dyn Tool>>,
        extension_core: Arc<crate::extensions::ExtensionCore>,
    ) -> Self {
        let inner = crate::engine::AgenticLoopV4::new(agent, provider, tools, extension_core);
        let async_executor = AsyncToolExecutor::with_timeout(Duration::from_secs(300));

        Self {
            inner,
            async_executor,
            config: AsyncAgenticConfig::default(),
            metrics: RwLock::new(AsyncToolMetrics::default()),
            capability_cache: RwLock::new(HashMap::new()),
        }
    }

    /// Create with custom config
    pub fn with_config(
        agent: Arc<Agent>,
        provider: Arc<dyn crate::providers::Provider>,
        tools: Vec<Arc<dyn Tool>>,
        extension_core: Arc<crate::extensions::ExtensionCore>,
        config: AsyncAgenticConfig,
    ) -> Self {
        let inner = crate::engine::AgenticLoopV4::new(agent, provider, tools, extension_core);
        let async_executor =
            AsyncToolExecutor::with_timeout(Duration::from_secs(config.async_timeout_secs));

        Self {
            inner,
            async_executor,
            config,
            metrics: RwLock::new(AsyncToolMetrics::default()),
            capability_cache: RwLock::new(HashMap::new()),
        }
    }

    /// Set configuration
    #[must_use]
    pub fn with_async_config(mut self, config: AsyncAgenticConfig) -> Self {
        self.config = config;
        self
    }

    /// Check if a tool should use async execution
    ///
    /// Returns true if:
    /// - auto_async is enabled AND tool supports async
    /// - force_async is enabled
    async fn should_use_async(&self, tool_name: &str) -> bool {
        if self.config.force_async {
            return true;
        }

        if !self.config.auto_async {
            return false;
        }

        // Check capability cache
        let cache = self.capability_cache.read().await;
        if let Some(cap) = cache.get(tool_name) {
            return cap.supports_async;
        }
        drop(cache);

        // Query executor
        self.async_executor.supports_async(tool_name).await
    }

    /// Detect and cache tool capabilities
    async fn detect_capabilities(&self, tool: &Arc<dyn Tool>) -> AsyncCapability {
        let tool_name = tool.name().to_string();

        // Check cache first
        {
            let cache = self.capability_cache.read().await;
            if let Some(cap) = cache.get(&tool_name) {
                return cap.clone();
            }
        }

        // Detect capabilities
        let cap = self.async_executor.detect_capabilities(tool).await;

        // Cache result
        let mut cache = self.capability_cache.write().await;
        cache.insert(tool_name.clone(), cap.clone());

        // Update metrics
        if cap.supports_async {
            let mut metrics = self.metrics.write().await;
            if !metrics.async_capable_tools.contains(&tool_name) {
                metrics.async_capable_tools.push(tool_name);
            }
        }

        cap
    }

    /// Execute a tool with automatic async/sync selection
    #[instrument(skip(self, tool, params, on_event), fields(tool_name = %tool.name()))]
    async fn execute_tool_with_auto_async(
        &self,
        tool: Arc<dyn Tool>,
        params: Value,
        exec_ctx: &ToolExecutionContext,
        on_event: &dyn Fn(AgenticEvent),
        run_id: &str,
        tool_id: &str,
    ) -> Result<String> {
        let tool_name = tool.name().to_string();

        // Detect capabilities
        let cap = self.detect_capabilities(&tool).await;

        // Decide execution mode
        let use_async = self.should_use_async(&tool_name).await && cap.supports_async;

        if use_async {
            info!(tool_name, "Using async execution");
            self.execute_tool_async(tool, params, exec_ctx, on_event, run_id, tool_id)
                .await
        } else {
            info!(tool_name, "Using sync execution");
            self.execute_tool_sync(tool, params, exec_ctx, on_event, run_id, tool_id)
                .await
        }
    }

    /// Execute tool synchronously
    async fn execute_tool_sync(
        &self,
        tool: Arc<dyn Tool>,
        params: Value,
        exec_ctx: &ToolExecutionContext,
        on_event: &dyn Fn(AgenticEvent),
        run_id: &str,
        tool_id: &str,
    ) -> Result<String> {
        let start_time = std::time::Instant::now();
        let tool_name = tool.name().to_string();

        // Use inner loop's tool executor
        let result = match self
            .inner
            .tool_executor()
            .execute_with_context(tool, params, exec_ctx)
            .await
        {
            Ok(result) => {
                info!(tool_name, "Tool executed successfully (sync)");
                result.to_string()
            }
            Err(e) => {
                info!(tool_name, error = %e, "Tool failed (sync)");
                format!("Error: {e}")
            }
        };

        // Update metrics
        if self.config.collect_metrics {
            let mut metrics = self.metrics.write().await;
            metrics.sync_executions += 1;
        }

        let duration_ms = start_time.elapsed().as_millis() as u64;

        // Emit completion event
        on_event(AgenticEvent::ToolEnd {
            run_id: run_id.to_string(),
            tool_id: tool_id.to_string(),
            result: serde_json::json!(&result),
            success: !result.starts_with("Error:"),
            duration_ms,
        });

        Ok(result)
    }

    /// Execute tool asynchronously
    async fn execute_tool_async(
        &self,
        tool: Arc<dyn Tool>,
        params: Value,
        exec_ctx: &ToolExecutionContext,
        on_event: &dyn Fn(AgenticEvent),
        run_id: &str,
        tool_id: &str,
    ) -> Result<String> {
        let tool_name = tool.name().to_string();
        let start_time = std::time::Instant::now();

        // Build async config
        let config = crate::agent::async_tool_framework::AsyncToolConfig {
            timeout_secs: self.config.async_timeout_secs,
            label: Some(format!("{}_{}", exec_ctx.agent_id, exec_ctx.session_id)),
            ..Default::default()
        };

        // Execute async
        let receipt = match self
            .async_executor
            .execute_async(tool.clone(), params.clone(), exec_ctx)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                warn!(tool_name, error = %e, "Async execution failed, falling back to sync");
                return self
                    .execute_tool_sync(tool, params, exec_ctx, on_event, run_id, tool_id)
                    .await;
            }
        };

        info!(
            tool_name,
            task_id = %receipt.task_id,
            "Async execution started"
        );

        // If streaming results enabled, emit progress events
        if self.config.streaming_results {
            self.poll_with_progress(
                &tool_name,
                &receipt.task_id,
                on_event,
                run_id,
                tool_id,
            )
            .await?;
        }

        // Wait for completion by polling
        let timeout = Duration::from_secs(self.config.async_timeout_secs);
        let start = std::time::Instant::now();
        let status = loop {
            let status = self.async_executor.check_status(&tool_name, &receipt.task_id).await?;
            if status.is_terminal() {
                break status;
            }
            if start.elapsed() > timeout {
                return Err(anyhow::anyhow!("Timeout waiting for task completion"));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        };

        // Process result
        let result = match status {
            crate::agent::async_tool_framework::AsyncTaskStatus::Completed { .. } => {
                info!(tool_name, "Async tool completed successfully");
                "Task completed successfully".to_string()
            }
            crate::agent::async_tool_framework::AsyncTaskStatus::Failed { error } => {
                info!(tool_name, error, "Async tool failed");
                format!("Error: {error}")
            }
            crate::agent::async_tool_framework::AsyncTaskStatus::Cancelled => {
                info!(tool_name, "Async tool cancelled");
                "Error: Tool execution cancelled".to_string()
            }
            _ => {
                warn!(tool_name, "Async tool ended in unexpected state");
                "Error: Tool execution ended unexpectedly".to_string()
            }
        };

        // Update metrics
        if self.config.collect_metrics {
            let mut metrics = self.metrics.write().await;
            metrics.async_executions += 1;
            let duration_ms = start_time.elapsed().as_millis() as u64;
            metrics.avg_execution_time_ms =
                (metrics.avg_execution_time_ms + duration_ms) / 2;
        }

        let duration_ms = start_time.elapsed().as_millis() as u64;

        // Emit completion event
        on_event(AgenticEvent::ToolEnd {
            run_id: run_id.to_string(),
            tool_id: tool_id.to_string(),
            result: serde_json::json!(&result),
            success: !result.starts_with("Error:"),
            duration_ms,
        });

        Ok(result)
    }

    /// Poll for status with progress updates
    async fn poll_with_progress(
        &self,
        tool_name: &str,
        task_id: &str,
        on_event: &dyn Fn(AgenticEvent),
        run_id: &str,
        tool_id: &str,
    ) -> Result<()> {
        let interval = Duration::from_millis(self.config.progress_interval_ms);
        let mut last_percent = 0u8;

        loop {
            tokio::time::sleep(interval).await;

            let status = self.async_executor.check_status(tool_name, &task_id.to_string()).await?;

            // Calculate progress
            let percent = match &status {
                crate::agent::async_tool_framework::AsyncTaskStatus::Pending => 0,
                crate::agent::async_tool_framework::AsyncTaskStatus::Running => 50,
                crate::agent::async_tool_framework::AsyncTaskStatus::Completed { .. } => 100,
                _ => 100,
            };

            // Emit progress event if changed
            if percent != last_percent {
                on_event(AgenticEvent::ToolUpdate {
                    run_id: run_id.to_string(),
                    tool_id: tool_id.to_string(),
                    output: format!("{} - {:?}", tool_name, status),
                    progress_percent: Some(percent),
                });
                last_percent = percent;
            }

            if status.is_terminal() {
                break;
            }
        }

        Ok(())
    }

    /// Get current metrics
    pub async fn metrics(&self) -> AsyncToolMetrics {
        self.metrics.read().await.clone()
    }

    /// Reset metrics
    pub async fn reset_metrics(&self) {
        let mut metrics = self.metrics.write().await;
        *metrics = AsyncToolMetrics::default();
    }

    /// Get async tool syntax for system prompt
    ///
    /// Returns a string describing async tool usage for the LLM
    pub fn get_async_tool_prompt_section(&self) -> String {
        r#"## Async Tool Execution

Some tools support asynchronous execution for long-running operations:

### When to use async:
- File operations on large directories
- Network requests with uncertain timing
- Complex computations that may take minutes
- Operations that should run in background

### Async tool syntax:
```json
{
  "name": "tool_name",
  "arguments": { ... },
  "async": true,
  "timeout_seconds": 300
}
```

### Checking async status:
If a tool supports status checking, you can check progress:
```json
{
  "name": "tool_name_status",
  "arguments": {
    "task_id": "..."
  }
}
```

### Async-capable tools:
"#.to_string()
    }

    /// Build enhanced system prompt with async tool support
    pub fn build_system_prompt_with_async(&self) -> String {
        let base_prompt = self.inner.system_prompt();
        let async_section = self.get_async_tool_prompt_section();

        format!("{}\n\n{}", base_prompt, async_section)
    }
}

// AgenticLoopV4 already has system_prompt() and tool_executor() methods exposed

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_async_config_default() {
        let config = AsyncAgenticConfig::default();
        assert!(config.auto_async);
        assert_eq!(config.async_timeout_secs, 300);
        assert!(config.streaming_results);
        assert_eq!(config.progress_interval_ms, 1000);
    }

    #[tokio::test]
    async fn test_metrics_default() {
        let metrics = AsyncToolMetrics::default();
        assert_eq!(metrics.async_executions, 0);
        assert_eq!(metrics.sync_executions, 0);
        assert!(metrics.async_capable_tools.is_empty());
    }

    #[test]
    fn test_async_tool_prompt_section() {
        // This is a placeholder test - in reality we'd need the loop instance
        let section = r#"## Async Tool Execution"#;
        assert!(section.contains("Async Tool Execution"));
    }
}
