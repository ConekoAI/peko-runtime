//! Example tool demonstrating progress updates and abort signal support
//!
//! This is a template for building tools that:
//! - Report progress during long-running operations
//! - Respect abort signals for cancellation
//! - Handle timeouts
//! - Provide visibility into execution status

use crate::tools::context::{ToolContext, ToolError};
use crate::tools::Tool;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::time::Instant;

/// A long-running tool that demonstrates progress reporting and abort support
///
/// This tool simulates a batch processing operation that:
/// - Processes items in batches
/// - Reports progress periodically (with throttling)
/// - Checks for abort signals between batches
/// - Respects timeout limits
pub struct ProgressDemoTool {
    name: String,
    description: String,
}

impl ProgressDemoTool {
    pub fn new() -> Self {
        Self {
            name: "progress_demo".to_string(),
            description: "Demonstrates progress reporting and abort support. \
                Usage: {\"items\": [\"item1\", \"item2\", ...], \"batch_size\": 5, \"delay_ms\": 100}".to_string(),
        }
    }

    /// Process items with progress reporting
    async fn process_items(
        &self,
        items: Vec<String>,
        batch_size: usize,
        delay_ms: u64,
        ctx: &ToolContext,
    ) -> anyhow::Result<Value> {
        let total = items.len();
        let mut processed = 0;
        let mut results = Vec::new();

        // Record start time for timeout checking
        let start_time = Instant::now();

        // Report initial status
        ctx.report_status(format!("Starting batch processing of {} items", total))
            .await;

        for (idx, chunk) in items.chunks(batch_size).enumerate() {
            // Check if aborted before processing this batch
            if ctx.is_aborted() {
                ctx.report_status("Aborting: cleaning up...".to_string()).await;
                return Err(ToolError::Aborted.into());
            }

            // Check timeout
            if let Err(e) = ctx.check_timeout(start_time) {
                ctx.report_status("Timeout reached".to_string()).await;
                return Err(e.into());
            }

            // Report progress (may be throttled)
            let _percent = (processed * 100) / total;
            ctx.report_progress(
                processed,
                total,
                Some(format!(
                    "Processing batch {} (items {}-{} of {})",
                    idx + 1,
                    processed + 1,
                    (processed + chunk.len()).min(total),
                    total
                )),
            )
            .await;

            // Simulate work
            for item in chunk {
                // Check abort signal frequently during work
                if ctx.is_aborted() {
                    return Err(ToolError::Aborted.into());
                }

                // Check timeout frequently
                if let Err(e) = ctx.check_timeout(start_time) {
                    return Err(e.into());
                }

                // Process the item
                tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
                results.push(json!({
                    "item": item,
                    "status": "processed",
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                }));
            }

            processed += chunk.len();
        }

        // Report completion
        ctx.report_progress(total, total, Some("Processing complete".to_string()))
            .await;

        Ok(json!({
            "total_items": total,
            "processed": processed,
            "results": results,
            "aborted": false,
        }))
    }
}

#[async_trait]
impl Tool for ProgressDemoTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    /// Basic execution (without progress - delegates to context version)
    async fn execute(&self, params: Value) -> anyhow::Result<Value> {
        // This is called for basic usage without streaming context
        // We'll process without progress reporting
        let items: Vec<String> = params
            .get("items")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let batch_size = params
            .get("batch_size")
            .and_then(|v| v.as_u64())
            .unwrap_or(5) as usize;

        let delay_ms = params
            .get("delay_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(100);

        let total = items.len();
        let mut results = Vec::new();

        for chunk in items.chunks(batch_size) {
            for item in chunk {
                tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
                results.push(json!({
                    "item": item,
                    "status": "processed",
                }));
            }
        }

        Ok(json!({
            "total_items": total,
            "processed": results.len(),
            "results": results,
        }))
    }

    /// Context-aware execution with progress and abort support
    async fn execute_with_context(
        &self,
        params: Value,
        ctx: &ToolContext,
    ) -> anyhow::Result<Value> {
        let items: Vec<String> = params
            .get("items")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let batch_size = params
            .get("batch_size")
            .and_then(|v| v.as_u64())
            .unwrap_or(5) as usize;

        let delay_ms = params
            .get("delay_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(100);

        if items.is_empty() {
            ctx.report_status("No items to process".to_string()).await;
            return Ok(json!({ "message": "No items provided" }));
        }

        self.process_items(items, batch_size, delay_ms, ctx).await
    }

    /// This tool supports progress updates
    fn supports_progress(&self) -> bool {
        true
    }

    /// Estimate execution duration based on item count
    fn estimated_duration_ms(&self, params: &Value) -> u64 {
        let items = params
            .get("items")
            .and_then(|v| v.as_array())
            .map(|arr| arr.len())
            .unwrap_or(0);

        let delay_ms = params
            .get("delay_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(100);

        (items as u64) * delay_ms + 500 // Base overhead
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_progress_demo_tool_basic() {
        let tool = ProgressDemoTool::new();

        let params = json!({
            "items": vec!["a", "b", "c"],
            "delay_ms": 10,
        });

        let result = tool.execute(params).await.unwrap();

        assert_eq!(result["total_items"], 3);
        assert_eq!(result["processed"], 3);
    }

    #[tokio::test]
    async fn test_progress_demo_tool_with_context() {
        let tool = ProgressDemoTool::new();

        let (tx, mut rx) = tokio::sync::mpsc::channel(100);
        let signal = crate::tools::context::AbortSignal::new();
        let ctx = signal.create_context_with_events("run-1", "tool-1", "test", tx);

        let params = json!({
            "items": vec!["a", "b"],
            "delay_ms": 10,
            "batch_size": 1,
        });

        // Run in background so we can check events
        let handle = tokio::spawn(async move {
            tool.execute_with_context(params, &ctx).await
        });

        // Should receive progress events
        let mut progress_count = 0;
        while let Ok(Some(event)) = tokio::time::timeout(
            tokio::time::Duration::from_millis(500),
            rx.recv()
        ).await {
            match event {
                crate::engine::AgenticEvent::ToolUpdate { .. } => {
                    progress_count += 1;
                }
                _ => {}
            }
        }

        let result = handle.await.unwrap().unwrap();
        assert_eq!(result["total_items"], 2);
        assert!(progress_count > 0, "Should have received progress events");
    }

    #[tokio::test]
    async fn test_progress_demo_tool_abort() {
        let tool = ProgressDemoTool::new();

        let (tx, _rx) = tokio::sync::mpsc::channel(100);
        let signal = crate::tools::context::AbortSignal::new();
        let ctx = signal.create_context_with_events("run-1", "tool-1", "test", tx);

        let params = json!({
            "items": (0..100).map(|i| format!("item_{}", i)).collect::<Vec<_>>(),
            "delay_ms": 50,
            "batch_size": 10,
        });

        // Abort after 100ms
        let abort_signal = signal.clone();
        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            abort_signal.abort();
        });

        let result = tool.execute_with_context(params, &ctx).await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("aborted"));
    }
}
