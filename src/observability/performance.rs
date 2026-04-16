//! Performance Measurement Module
//!
//! Provides timing and profiling hooks for Milestone 12 performance targets.
//! All measurements use high-resolution timestamps (Instant).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Performance metrics collector
#[derive(Debug, Clone)]
pub struct PerformanceMetrics {
    inner: Arc<Mutex<MetricsInner>>,
}

#[derive(Debug, Default)]
struct MetricsInner {
    cold_start_times: Vec<Duration>,
    warm_start_times: Vec<Duration>,
    first_token_latencies: Vec<Duration>,
    tool_latencies: HashMap<String, Vec<Duration>>,
    active_timers: HashMap<String, Instant>,
}

impl PerformanceMetrics {
    /// Create a new metrics collector
    #[must_use] 
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(MetricsInner::default())),
        }
    }

    /// Start a named timer
    pub fn start_timer(&self, name: &str) {
        let mut inner = self.inner.lock().unwrap();
        inner.active_timers.insert(name.to_string(), Instant::now());
    }

    /// Stop a named timer and record the duration
    #[must_use] 
    pub fn stop_timer(&self, name: &str) -> Option<Duration> {
        let mut inner = self.inner.lock().unwrap();
        let start = inner.active_timers.remove(name)?;
        let duration = start.elapsed();

        // Route to appropriate bucket
        match name {
            "cold_start" => inner.cold_start_times.push(duration),
            "warm_start" => inner.warm_start_times.push(duration),
            "first_token" => inner.first_token_latencies.push(duration),
            n if n.starts_with("tool.") => {
                let tool_name = n.strip_prefix("tool.").unwrap_or(n).to_string();
                inner
                    .tool_latencies
                    .entry(tool_name)
                    .or_default()
                    .push(duration);
            }
            _ => {}
        }

        Some(duration)
    }

    /// Record a cold start time
    pub fn record_cold_start(&self, duration: Duration) {
        let mut inner = self.inner.lock().unwrap();
        inner.cold_start_times.push(duration);
    }

    /// Record a warm start time
    pub fn record_warm_start(&self, duration: Duration) {
        let mut inner = self.inner.lock().unwrap();
        inner.warm_start_times.push(duration);
    }

    /// Record first token latency
    pub fn record_first_token(&self, duration: Duration) {
        let mut inner = self.inner.lock().unwrap();
        inner.first_token_latencies.push(duration);
    }

    /// Record tool latency
    pub fn record_tool_latency(&self, tool_name: &str, duration: Duration) {
        let mut inner = self.inner.lock().unwrap();
        inner
            .tool_latencies
            .entry(tool_name.to_string())
            .or_default()
            .push(duration);
    }

    /// Get cold start statistics
    #[must_use] 
    pub fn cold_start_stats(&self) -> Option<LatencyStats> {
        let inner = self.inner.lock().unwrap();
        LatencyStats::calculate(&inner.cold_start_times)
    }

    /// Get warm start statistics
    #[must_use] 
    pub fn warm_start_stats(&self) -> Option<LatencyStats> {
        let inner = self.inner.lock().unwrap();
        LatencyStats::calculate(&inner.warm_start_times)
    }

    /// Get first token statistics
    #[must_use] 
    pub fn first_token_stats(&self) -> Option<LatencyStats> {
        let inner = self.inner.lock().unwrap();
        LatencyStats::calculate(&inner.first_token_latencies)
    }

    /// Get tool latency statistics
    #[must_use] 
    pub fn tool_latency_stats(&self, tool_name: &str) -> Option<LatencyStats> {
        let inner = self.inner.lock().unwrap();
        inner
            .tool_latencies
            .get(tool_name)
            .and_then(|durations| LatencyStats::calculate(durations))
    }

    /// Get all tool names that have latency data
    #[must_use] 
    pub fn recorded_tools(&self) -> Vec<String> {
        let inner = self.inner.lock().unwrap();
        inner.tool_latencies.keys().cloned().collect()
    }

    /// Clear all metrics
    pub fn clear(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.cold_start_times.clear();
        inner.warm_start_times.clear();
        inner.first_token_latencies.clear();
        inner.tool_latencies.clear();
        inner.active_timers.clear();
    }

    /// Export metrics as JSON-serializable struct
    #[must_use] 
    pub fn export(&self) -> MetricsExport {
        let inner = self.inner.lock().unwrap();
        MetricsExport {
            cold_start: LatencyStats::calculate(&inner.cold_start_times),
            warm_start: LatencyStats::calculate(&inner.warm_start_times),
            first_token: LatencyStats::calculate(&inner.first_token_latencies),
            tools: inner
                .tool_latencies
                .iter()
                .filter_map(|(k, v)| LatencyStats::calculate(v).map(|s| (k.clone(), s)))
                .collect(),
        }
    }
}

impl Default for PerformanceMetrics {
    fn default() -> Self {
        Self::new()
    }
}

/// Latency statistics
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct LatencyStats {
    pub count: usize,
    pub min_ms: f64,
    pub max_ms: f64,
    pub mean_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
}

impl LatencyStats {
    /// Calculate statistics from a list of durations
    #[must_use] 
    pub fn calculate(durations: &[Duration]) -> Option<Self> {
        if durations.is_empty() {
            return None;
        }

        let count = durations.len();
        let mut millis: Vec<f64> = durations.iter().map(|d| d.as_secs_f64() * 1000.0).collect();
        millis.sort_by(|a, b| a.partial_cmp(b).unwrap());

        let min_ms = millis[0];
        let max_ms = millis[count - 1];
        let mean_ms = millis.iter().sum::<f64>() / count as f64;
        let p50_ms = percentile(&millis, 0.5);
        let p95_ms = percentile(&millis, 0.95);
        let p99_ms = percentile(&millis, 0.99);

        Some(Self {
            count,
            min_ms,
            max_ms,
            mean_ms,
            p50_ms,
            p95_ms,
            p99_ms,
        })
    }

    /// Check if this latency meets a target requirement
    #[must_use] 
    pub fn meets_target(&self, target_ms: f64) -> bool {
        self.p95_ms <= target_ms
    }
}

/// Exported metrics (JSON-serializable)
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MetricsExport {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cold_start: Option<LatencyStats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warm_start: Option<LatencyStats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_token: Option<LatencyStats>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub tools: HashMap<String, LatencyStats>,
}

fn percentile(sorted_data: &[f64], p: f64) -> f64 {
    if sorted_data.is_empty() {
        return 0.0;
    }
    let index = (p * (sorted_data.len() - 1) as f64).round() as usize;
    sorted_data[index.min(sorted_data.len() - 1)]
}

// ============================================================================
// Global Metrics Instance
// ============================================================================


/// Global performance metrics instance
pub static GLOBAL_METRICS: std::sync::LazyLock<PerformanceMetrics> = std::sync::LazyLock::new(PerformanceMetrics::new);

/// Convenience function to start a timer on the global metrics
pub fn start_timer(name: &str) {
    GLOBAL_METRICS.start_timer(name);
}

/// Convenience function to stop a timer on the global metrics
pub fn stop_timer(name: &str) -> Option<Duration> {
    GLOBAL_METRICS.stop_timer(name)
}

// ============================================================================
// Performance Guard (RAII timer)
// ============================================================================

/// RAII guard for timing operations
pub struct PerformanceGuard {
    name: String,
    start: Instant,
    recorded: bool,
}

impl PerformanceGuard {
    /// Create a new performance guard
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            start: Instant::now(),
            recorded: false,
        }
    }

    /// Get elapsed time without recording
    #[must_use] 
    pub fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }

    /// Record the elapsed time manually
    pub fn record(mut self) -> Duration {
        let elapsed = self.start.elapsed();
        self.recorded = true;

        // Route to appropriate global metric
        match self.name.as_str() {
            "cold_start" => GLOBAL_METRICS.record_cold_start(elapsed),
            "warm_start" => GLOBAL_METRICS.record_warm_start(elapsed),
            "first_token" => GLOBAL_METRICS.record_first_token(elapsed),
            n if n.starts_with("tool.") => {
                let tool_name = n.strip_prefix("tool.").unwrap_or(n);
                GLOBAL_METRICS.record_tool_latency(tool_name, elapsed);
            }
            _ => {}
        }

        elapsed
    }
}

impl Drop for PerformanceGuard {
    fn drop(&mut self) {
        if !self.recorded {
            // Auto-record on drop if not already recorded
            let elapsed = self.start.elapsed();
            match self.name.as_str() {
                "cold_start" => GLOBAL_METRICS.record_cold_start(elapsed),
                "warm_start" => GLOBAL_METRICS.record_warm_start(elapsed),
                "first_token" => GLOBAL_METRICS.record_first_token(elapsed),
                n if n.starts_with("tool.") => {
                    let tool_name = n.strip_prefix("tool.").unwrap_or(n);
                    GLOBAL_METRICS.record_tool_latency(tool_name, elapsed);
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn test_performance_metrics() {
        let metrics = PerformanceMetrics::new();

        // Record some cold start times
        metrics.record_cold_start(Duration::from_millis(300));
        metrics.record_cold_start(Duration::from_millis(400));
        metrics.record_cold_start(Duration::from_millis(500));

        let stats = metrics.cold_start_stats().unwrap();
        assert_eq!(stats.count, 3);
        assert!(stats.mean_ms >= 300.0 && stats.mean_ms <= 500.0);
    }

    #[test]
    fn test_performance_guard() {
        let _guard = PerformanceGuard::new("test_op");
        thread::sleep(Duration::from_millis(10));
        // Guard auto-records on drop
    }

    #[test]
    fn test_timer() {
        let metrics = PerformanceMetrics::new();

        metrics.start_timer("cold_start");
        thread::sleep(Duration::from_millis(10));
        let duration = metrics.stop_timer("cold_start");

        assert!(duration.is_some());
        assert!(duration.unwrap() >= Duration::from_millis(10));
    }

    #[test]
    fn test_meets_target() {
        let stats = LatencyStats {
            count: 10,
            min_ms: 100.0,
            max_ms: 500.0,
            mean_ms: 300.0,
            p50_ms: 300.0,
            p95_ms: 480.0,
            p99_ms: 500.0,
        };

        assert!(stats.meets_target(500.0));
        assert!(!stats.meets_target(400.0));
    }
}
