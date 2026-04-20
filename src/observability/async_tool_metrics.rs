//! Async Tool Metrics Collection
//!
//! Provides comprehensive metrics for async tool execution:
//! - Execution counts (async vs sync)
//! - Duration tracking
//! - Success/failure rates
//! - Cancellation counts
//! - Tool capability detection

use crate::agent::async_tool_framework::{AsyncTaskId, AsyncTaskStatus};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{debug, info};

/// Metrics for a single async task execution
#[derive(Debug, Clone)]
pub struct TaskExecutionMetrics {
    /// Task identifier
    pub task_id: AsyncTaskId,
    /// Tool name
    pub tool_name: String,
    /// When the task started
    pub start_time: Instant,
    /// When the task completed (if finished)
    pub end_time: Option<Instant>,
    /// Final status
    pub final_status: Option<AsyncTaskStatus>,
    /// Duration of execution
    pub duration: Option<Duration>,
    /// Whether execution was async
    pub was_async: bool,
    /// Progress updates received
    pub progress_updates: u32,
}

impl TaskExecutionMetrics {
    /// Create new task metrics
    #[must_use]
    pub fn new(task_id: AsyncTaskId, tool_name: String, was_async: bool) -> Self {
        Self {
            task_id,
            tool_name,
            start_time: Instant::now(),
            end_time: None,
            final_status: None,
            duration: None,
            was_async,
            progress_updates: 0,
        }
    }

    /// Record task completion
    pub fn complete(&mut self, status: AsyncTaskStatus) {
        self.end_time = Some(Instant::now());
        self.final_status = Some(status);
        self.duration = Some(self.start_time.elapsed());
    }

    /// Record progress update
    pub fn record_progress(&mut self) {
        self.progress_updates += 1;
    }

    /// Get duration (returns elapsed if not complete)
    #[must_use]
    pub fn duration(&self) -> Duration {
        self.duration.unwrap_or_else(|| self.start_time.elapsed())
    }

    /// Check if task succeeded
    #[must_use]
    pub fn succeeded(&self) -> bool {
        matches!(self.final_status, Some(AsyncTaskStatus::Completed { .. }))
    }

    /// Check if task failed
    #[must_use]
    pub fn failed(&self) -> bool {
        matches!(self.final_status, Some(AsyncTaskStatus::Failed { .. }))
    }

    /// Check if task was cancelled
    #[must_use]
    pub fn cancelled(&self) -> bool {
        matches!(self.final_status, Some(AsyncTaskStatus::Cancelled))
    }
}

/// Aggregated metrics for async tool execution
#[derive(Debug, Clone, Default)]
pub struct AsyncToolExecutionMetrics {
    /// Total number of async executions
    pub async_executions: u64,
    /// Total number of sync executions (fallback)
    pub sync_executions: u64,
    /// Total number of successful executions
    pub successful_executions: u64,
    /// Total number of failed executions
    pub failed_executions: u64,
    /// Total number of cancelled executions
    pub cancelled_executions: u64,
    /// Total number of timeouts
    pub timeouts: u64,
    /// Average async execution time (milliseconds)
    pub avg_async_duration_ms: f64,
    /// Average sync execution time (milliseconds)
    pub avg_sync_duration_ms: f64,
    /// Tool-specific metrics
    pub tool_metrics: HashMap<String, ToolSpecificMetrics>,
}

/// Metrics specific to a tool
#[derive(Debug, Clone, Default)]
pub struct ToolSpecificMetrics {
    /// Tool name
    pub tool_name: String,
    /// Async execution count
    pub async_count: u64,
    /// Sync execution count
    pub sync_count: u64,
    /// Success count
    pub success_count: u64,
    /// Failure count
    pub failure_count: u64,
    /// Total execution time (for averaging)
    pub total_duration_ms: u64,
    /// Average duration (calculated)
    pub avg_duration_ms: f64,
    /// Whether tool supports async
    pub supports_async: bool,
}

impl ToolSpecificMetrics {
    /// Update with new execution data
    pub fn record_execution(&mut self, was_async: bool, duration: Duration, success: bool) {
        if was_async {
            self.async_count += 1;
        } else {
            self.sync_count += 1;
        }

        if success {
            self.success_count += 1;
        } else {
            self.failure_count += 1;
        }

        let duration_ms = duration.as_millis() as u64;
        self.total_duration_ms += duration_ms;
        let total_count = self.async_count + self.sync_count;
        self.avg_duration_ms = self.total_duration_ms as f64 / total_count as f64;
    }
}

/// Async tool metrics collector
pub struct AsyncToolMetricsCollector {
    /// Active task metrics (in-flight)
    active_tasks: Arc<RwLock<HashMap<AsyncTaskId, TaskExecutionMetrics>>>,
    /// Completed task metrics (kept for analysis)
    completed_tasks: Arc<RwLock<Vec<TaskExecutionMetrics>>>,
    /// Maximum number of completed tasks to keep
    max_completed_tasks: usize,
    /// Aggregated metrics
    aggregated: Arc<RwLock<AsyncToolExecutionMetrics>>,
}

impl AsyncToolMetricsCollector {
    /// Create a new metrics collector
    #[must_use]
    pub fn new() -> Self {
        Self {
            active_tasks: Arc::new(RwLock::new(HashMap::new())),
            completed_tasks: Arc::new(RwLock::new(Vec::new())),
            max_completed_tasks: 1000,
            aggregated: Arc::new(RwLock::new(AsyncToolExecutionMetrics::default())),
        }
    }

    /// Create with custom max completed tasks
    #[must_use]
    pub fn with_max_completed_tasks(max: usize) -> Self {
        Self {
            active_tasks: Arc::new(RwLock::new(HashMap::new())),
            completed_tasks: Arc::new(RwLock::new(Vec::new())),
            max_completed_tasks: max,
            aggregated: Arc::new(RwLock::new(AsyncToolExecutionMetrics::default())),
        }
    }

    /// Start tracking a new task
    pub async fn start_task(&self, task_id: AsyncTaskId, tool_name: String, was_async: bool) {
        let tool_name_for_log = tool_name.clone();
        let metrics = TaskExecutionMetrics::new(task_id.clone(), tool_name, was_async);

        let mut active = self.active_tasks.write().await;
        active.insert(task_id.clone(), metrics);

        debug!(
            task_id = %task_id,
            tool_name = %tool_name_for_log,
            was_async = was_async,
            "Started tracking task metrics"
        );
    }

    /// Record task completion
    pub async fn complete_task(&self, task_id: &AsyncTaskId, status: AsyncTaskStatus) {
        let task_id_for_log = task_id.clone();

        // Move from active to completed
        let mut active = self.active_tasks.write().await;
        if let Some(mut metrics) = active.remove(task_id) {
            metrics.complete(status.clone());
            let duration = metrics.duration();
            let was_async = metrics.was_async;
            let tool_name = metrics.tool_name.clone();
            let success = metrics.succeeded();

            // Store completed metrics
            let mut completed = self.completed_tasks.write().await;
            completed.push(metrics);

            // Trim if needed
            if completed.len() > self.max_completed_tasks {
                completed.remove(0);
            }
            drop(completed);
            drop(active);

            // Update aggregated metrics
            self.update_aggregated(&tool_name, was_async, duration, success, status)
                .await;

            info!(
                task_id = %task_id_for_log,
                tool_name = %tool_name,
                duration_ms = %duration.as_millis(),
                success = %success,
                "Task completed"
            );
        }
    }

    /// Record progress update for a task
    pub async fn record_progress(&self, task_id: &AsyncTaskId) {
        let mut active = self.active_tasks.write().await;
        if let Some(metrics) = active.get_mut(task_id) {
            metrics.record_progress();
        }
    }

    /// Get current aggregated metrics
    pub async fn get_aggregated(&self) -> AsyncToolExecutionMetrics {
        self.aggregated.read().await.clone()
    }

    /// Get metrics for a specific tool
    pub async fn get_tool_metrics(&self, tool_name: &str) -> Option<ToolSpecificMetrics> {
        let aggregated = self.aggregated.read().await;
        aggregated.tool_metrics.get(tool_name).cloned()
    }

    /// Get active task count
    pub async fn active_task_count(&self) -> usize {
        self.active_tasks.read().await.len()
    }

    /// Get completed task count
    pub async fn completed_task_count(&self) -> usize {
        self.completed_tasks.read().await.len()
    }

    /// Reset all metrics
    pub async fn reset(&self) {
        let mut active = self.active_tasks.write().await;
        active.clear();
        drop(active);

        let mut completed = self.completed_tasks.write().await;
        completed.clear();
        drop(completed);

        let mut aggregated = self.aggregated.write().await;
        *aggregated = AsyncToolExecutionMetrics::default();
    }

    /// Generate summary report
    pub async fn generate_report(&self) -> String {
        let aggregated = self.aggregated.read().await;
        let active_count = self.active_tasks.read().await.len();

        format!(
            r"Async Tool Execution Metrics
=============================

Overall Statistics:
- Async executions: {}
- Sync executions: {}
- Successful: {}
- Failed: {}
- Cancelled: {}
- Timeouts: {}
- Active tasks: {}

Average Durations:
- Async: {:.2}ms
- Sync: {:.2}ms

Tool Breakdown:
{}
",
            aggregated.async_executions,
            aggregated.sync_executions,
            aggregated.successful_executions,
            aggregated.failed_executions,
            aggregated.cancelled_executions,
            aggregated.timeouts,
            active_count,
            aggregated.avg_async_duration_ms,
            aggregated.avg_sync_duration_ms,
            self.format_tool_breakdown(&aggregated.tool_metrics)
        )
    }

    /// Update aggregated metrics
    async fn update_aggregated(
        &self,
        tool_name: &str,
        was_async: bool,
        duration: Duration,
        success: bool,
        status: AsyncTaskStatus,
    ) {
        let mut aggregated = self.aggregated.write().await;
        let duration_ms = duration.as_millis() as f64;

        // Update counts
        if was_async {
            aggregated.async_executions += 1;
            // Update running average
            let count = aggregated.async_executions as f64;
            aggregated.avg_async_duration_ms =
                (aggregated.avg_async_duration_ms * (count - 1.0) + duration_ms) / count;
        } else {
            aggregated.sync_executions += 1;
            let count = aggregated.sync_executions as f64;
            aggregated.avg_sync_duration_ms =
                (aggregated.avg_sync_duration_ms * (count - 1.0) + duration_ms) / count;
        }

        if success {
            aggregated.successful_executions += 1;
        } else {
            aggregated.failed_executions += 1;
        }

        if matches!(status, AsyncTaskStatus::Cancelled) {
            aggregated.cancelled_executions += 1;
        }

        // Update tool-specific metrics
        let tool_metrics = aggregated
            .tool_metrics
            .entry(tool_name.to_string())
            .or_insert_with(|| ToolSpecificMetrics {
                tool_name: tool_name.to_string(),
                ..Default::default()
            });

        tool_metrics.record_execution(was_async, duration, success);
    }

    /// Format tool breakdown for report
    fn format_tool_breakdown(&self, tools: &HashMap<String, ToolSpecificMetrics>) -> String {
        if tools.is_empty() {
            return "  No tool data available".to_string();
        }

        let mut lines = Vec::new();
        for (name, metrics) in tools {
            lines.push(format!(
                "  {}: {} async, {} sync, {:.2}ms avg",
                name, metrics.async_count, metrics.sync_count, metrics.avg_duration_ms
            ));
        }
        lines.join("\n")
    }
}

impl Default for AsyncToolMetricsCollector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_metrics_collector_creation() {
        let collector = AsyncToolMetricsCollector::new();
        assert_eq!(collector.active_task_count().await, 0);
        assert_eq!(collector.completed_task_count().await, 0);
    }

    #[tokio::test]
    async fn test_task_tracking() {
        let collector = AsyncToolMetricsCollector::new();

        collector
            .start_task("task1".to_string(), "test_tool".to_string(), true)
            .await;
        assert_eq!(collector.active_task_count().await, 1);

        collector
            .complete_task(
                &"task1".to_string(),
                AsyncTaskStatus::Completed {
                    result: crate::tools::ToolResult::success(serde_json::json!({"ok": true})),
                },
            )
            .await;

        assert_eq!(collector.active_task_count().await, 0);
        assert_eq!(collector.completed_task_count().await, 1);
    }

    #[tokio::test]
    async fn test_aggregated_metrics() {
        let collector = AsyncToolMetricsCollector::new();

        collector
            .start_task("task1".to_string(), "tool_a".to_string(), true)
            .await;
        collector
            .complete_task(
                &"task1".to_string(),
                AsyncTaskStatus::Completed {
                    result: crate::tools::ToolResult::success(serde_json::json!({})),
                },
            )
            .await;

        let metrics = collector.get_aggregated().await;
        assert_eq!(metrics.async_executions, 1);
        assert_eq!(metrics.successful_executions, 1);
    }

    #[tokio::test]
    async fn test_report_generation() {
        let collector = AsyncToolMetricsCollector::new();

        collector
            .start_task("task1".to_string(), "test_tool".to_string(), true)
            .await;
        collector
            .complete_task(
                &"task1".to_string(),
                AsyncTaskStatus::Completed {
                    result: crate::tools::ToolResult::success(serde_json::json!({})),
                },
            )
            .await;

        let report = collector.generate_report().await;
        assert!(report.contains("Async Tool Execution Metrics"));
        assert!(report.contains("test_tool"));
    }
}
