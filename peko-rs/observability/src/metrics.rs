//! Metrics - Performance counters and histograms

use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

/// Metrics collector
pub struct MetricsCollector {
    /// Counters
    counters: HashMap<String, AtomicU64>,
    /// Histograms (simplified as Vec of values)
    histograms: HashMap<String, Vec<u64>>,
    /// Gauges (current values). Read by `snapshot` to keep the output
    /// schema stable; currently no production path writes gauges.
    gauges: HashMap<String, AtomicU64>,
    /// Max histogram samples
    max_samples: usize,
}

impl MetricsCollector {
    /// Create new collector
    #[must_use]
    pub fn new() -> Self {
        Self {
            counters: HashMap::new(),
            histograms: HashMap::new(),
            gauges: HashMap::new(),
            max_samples: 1000,
        }
    }

    /// Increment counter
    pub fn counter(&mut self, name: &str, value: u64) {
        self.counters
            .entry(name.to_string())
            .or_insert_with(|| AtomicU64::new(0))
            .fetch_add(value, Ordering::Relaxed);
    }

    /// Record histogram value
    pub fn histogram(&mut self, name: &str, value: u64) {
        let values = self.histograms.entry(name.to_string()).or_default();

        values.push(value);

        // Keep only recent samples
        if values.len() > self.max_samples {
            values.remove(0);
        }
    }

    /// Get histogram stats
    #[must_use]
    pub fn get_histogram_stats(&self, name: &str) -> Option<HistogramStats> {
        let values = self.histograms.get(name)?;

        if values.is_empty() {
            return None;
        }

        let count = values.len() as u64;
        let sum: u64 = values.iter().sum();
        let min = *values.iter().min().unwrap();
        let max = *values.iter().max().unwrap();
        let avg = sum / count;

        // Calculate percentiles (simplified)
        let mut sorted = values.clone();
        sorted.sort_unstable();
        let p50 = sorted[sorted.len() / 2];
        let p95_idx = ((sorted.len() as f64 * 0.95) as usize).min(sorted.len() - 1);
        let p95 = sorted[p95_idx];

        Some(HistogramStats {
            count,
            min,
            max,
            avg,
            p50,
            p95,
        })
    }

    /// Get all metrics snapshot
    pub async fn snapshot(&self) -> serde_json::Value {
        let counters: HashMap<String, u64> = self
            .counters
            .iter()
            .map(|(k, v)| (k.clone(), v.load(Ordering::Relaxed)))
            .collect();

        let gauges: HashMap<String, u64> = self
            .gauges
            .iter()
            .map(|(k, v)| (k.clone(), v.load(Ordering::Relaxed)))
            .collect();

        let histograms: HashMap<String, HistogramStats> = self
            .histograms
            .keys()
            .filter_map(|k| self.get_histogram_stats(k).map(|s| (k.clone(), s)))
            .collect();

        serde_json::json!({
            "counters": counters,
            "gauges": gauges,
            "histograms": histograms,
            "timestamp": chrono::Utc::now().to_rfc3339(),
        })
    }
}

impl Default for MetricsCollector {
    fn default() -> Self {
        Self::new()
    }
}

/// Histogram statistics
#[derive(Debug, Clone, Serialize)]
pub struct HistogramStats {
    pub count: u64,
    pub min: u64,
    pub max: u64,
    pub avg: u64,
    pub p50: u64,
    pub p95: u64,
}
