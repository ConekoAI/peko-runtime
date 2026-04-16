//! Performance Metrics API Routes
//!
//! Provides endpoints for querying performance metrics collected during
//! Milestone 12 testing and optimization.
//!
//! Endpoints:
//! - GET /metrics/performance - Get all performance metrics
//! - POST /metrics/performance/reset - Reset all metrics

use crate::api::error::ApiError;
use crate::api::state::AppState;
use crate::observability::performance::{MetricsExport, GLOBAL_METRICS};
use axum::{
    extract::State,
    routing::{get, post},
    Json, Router,
};
use serde::Serialize;

/// Performance metrics response
#[derive(Debug, Serialize)]
pub struct PerformanceMetricsResponse {
    /// Whether all targets are being met
    pub all_targets_met: bool,
    /// Cold start metrics (target: < 500ms)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cold_start: Option<MetricWithTarget>,
    /// Warm start metrics (target: < 100ms)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warm_start: Option<MetricWithTarget>,
    /// First token latency metrics (target: < 500ms)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_token: Option<MetricWithTarget>,
    /// Tool latency metrics (target: < 5ms)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolMetricsSummary>,
    /// Raw metrics export
    pub raw: MetricsExport,
}

/// Metric with target information
#[derive(Debug, Serialize)]
pub struct MetricWithTarget {
    pub target_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub mean_ms: f64,
    pub min_ms: f64,
    pub max_ms: f64,
    pub count: usize,
    pub meets_target: bool,
}

/// Tool metrics summary
#[derive(Debug, Serialize)]
pub struct ToolMetricsSummary {
    pub target_ms: f64,
    pub tools: Vec<ToolMetric>,
}

/// Individual tool metric
#[derive(Debug, Serialize)]
pub struct ToolMetric {
    pub name: String,
    pub p95_ms: f64,
    pub mean_ms: f64,
    pub count: usize,
    pub meets_target: bool,
}

/// Get all performance metrics
async fn get_performance_metrics(
    State(_state): State<AppState>,
) -> Result<Json<PerformanceMetricsResponse>, ApiError> {
    let export = GLOBAL_METRICS.export();

    let cold_start = export.cold_start.map(|s| MetricWithTarget {
        target_ms: 500.0,
        p95_ms: s.p95_ms,
        p99_ms: s.p99_ms,
        mean_ms: s.mean_ms,
        min_ms: s.min_ms,
        max_ms: s.max_ms,
        count: s.count,
        meets_target: s.meets_target(500.0),
    });

    let warm_start = export.warm_start.map(|s| MetricWithTarget {
        target_ms: 100.0,
        p95_ms: s.p95_ms,
        p99_ms: s.p99_ms,
        mean_ms: s.mean_ms,
        min_ms: s.min_ms,
        max_ms: s.max_ms,
        count: s.count,
        meets_target: s.meets_target(100.0),
    });

    let first_token = export.first_token.map(|s| MetricWithTarget {
        target_ms: 500.0,
        p95_ms: s.p95_ms,
        p99_ms: s.p99_ms,
        mean_ms: s.mean_ms,
        min_ms: s.min_ms,
        max_ms: s.max_ms,
        count: s.count,
        meets_target: s.meets_target(500.0),
    });

    let tools = if export.tools.is_empty() {
        None
    } else {
        let tool_metrics: Vec<ToolMetric> = export
            .tools
            .iter()
            .map(|(name, stats)| ToolMetric {
                name: name.clone(),
                p95_ms: stats.p95_ms,
                mean_ms: stats.mean_ms,
                count: stats.count,
                meets_target: stats.meets_target(5.0),
            })
            .collect();

        Some(ToolMetricsSummary {
            target_ms: 5.0,
            tools: tool_metrics,
        })
    };

    // Check if all targets are met
    let all_targets_met = cold_start.as_ref().is_none_or(|m| m.meets_target)
        && warm_start.as_ref().is_none_or(|m| m.meets_target)
        && first_token.as_ref().is_none_or(|m| m.meets_target)
        && tools
            .as_ref()
            .is_none_or(|t| t.tools.iter().all(|tool| tool.meets_target));

    Ok(Json(PerformanceMetricsResponse {
        all_targets_met,
        cold_start,
        warm_start,
        first_token,
        tools,
        raw: export,
    }))
}

/// Reset all performance metrics
async fn reset_performance_metrics(
    State(_state): State<AppState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    GLOBAL_METRICS.clear();
    Ok(Json(serde_json::json!({
        "status": "ok",
        "message": "Performance metrics reset"
    })))
}

/// Create router for metrics routes
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/metrics/performance", get(get_performance_metrics))
        .route(
            "/metrics/performance/reset",
            post(reset_performance_metrics),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observability::performance::LatencyStats;

    #[test]
    fn test_metric_with_target_creation() {
        let stats = LatencyStats {
            count: 10,
            min_ms: 50.0,
            max_ms: 150.0,
            mean_ms: 100.0,
            p50_ms: 100.0,
            p95_ms: 145.0,
            p99_ms: 150.0,
        };

        let metric = MetricWithTarget {
            target_ms: 200.0,
            p95_ms: stats.p95_ms,
            p99_ms: stats.p99_ms,
            mean_ms: stats.mean_ms,
            min_ms: stats.min_ms,
            max_ms: stats.max_ms,
            count: stats.count,
            meets_target: stats.meets_target(200.0),
        };

        assert!(metric.meets_target);
        assert_eq!(metric.p95_ms, 145.0);
    }

    #[test]
    fn test_tool_metric_creation() {
        let tool_metric = ToolMetric {
            name: "read_file".to_string(),
            p95_ms: 3.0,
            mean_ms: 2.0,
            count: 100,
            meets_target: true,
        };

        assert_eq!(tool_metric.name, "read_file");
        assert!(tool_metric.meets_target);
    }
}
