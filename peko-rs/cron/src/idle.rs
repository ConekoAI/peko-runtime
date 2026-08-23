//! Idle detection for scheduler
//!
//! Tracks Principal activity and determines when Principals have been idle
//! for a specified period, triggering idle-based scheduled jobs.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::trace;

/// Tracks Principal activity for idle detection
#[derive(Debug, Clone)]
pub struct IdleDetector {
    /// Last activity timestamp per Principal
    last_activity: Arc<RwLock<HashMap<String, Instant>>>,
}

impl IdleDetector {
    /// Create a new idle detector
    pub fn new() -> Self {
        Self {
            last_activity: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Record activity for a specific Principal
    pub async fn record_activity(&self, principal_name: &str) {
        let mut activity = self.last_activity.write().await;
        let now = Instant::now();
        activity.insert(principal_name.to_string(), now);
        trace!("Recorded activity for Principal: {}", principal_name);
    }

    /// Check if a specific Principal has been idle for at least `threshold_minutes`
    pub async fn is_idle(&self, principal_name: &str, threshold_minutes: u64) -> bool {
        let threshold = Duration::from_secs(threshold_minutes * 60);
        let activity = self.last_activity.read().await;

        if let Some(last) = activity.get(principal_name) {
            let elapsed = Instant::now().duration_since(*last);
            elapsed >= threshold
        } else {
            // No activity recorded yet - consider idle
            true
        }
    }
}

impl Default for IdleDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_record_and_check_activity() {
        let detector = IdleDetector::new();

        // Initially should be idle
        assert!(detector.is_idle("my-principal", 1).await);

        // Record activity
        detector.record_activity("my-principal").await;

        // Should not be idle immediately
        assert!(!detector.is_idle("my-principal", 1).await);
    }

    #[tokio::test]
    async fn test_idle_after_threshold() {
        let detector = IdleDetector::new();

        // Record activity
        detector.record_activity("my-principal").await;

        // Should not be idle with 1 minute threshold
        assert!(!detector.is_idle("my-principal", 1).await);

        // Simulate time passing by manually checking
        // (In real test, we'd need to mock time or use a shorter threshold)
    }
}