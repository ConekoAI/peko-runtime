//! Idle detection for scheduler
//!
//! Tracks Principal activity and determines when Principals have been idle
//! for a specified period, triggering idle-based scheduled jobs.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{debug, trace};

/// Tracks Principal activity for idle detection
#[derive(Debug, Clone)]
pub struct IdleDetector {
    /// Last activity timestamp per Principal
    last_activity: Arc<RwLock<HashMap<String, Instant>>>,
    /// Global last activity (any Principal)
    global_last_activity: Arc<RwLock<Instant>>,
}

impl IdleDetector {
    /// Create a new idle detector
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            last_activity: Arc::new(RwLock::new(HashMap::new())),
            global_last_activity: Arc::new(RwLock::new(now)),
        }
    }

    /// Record activity for a specific Principal
    pub async fn record_activity(&self, principal_name: &str) {
        let mut activity = self.last_activity.write().await;
        let now = Instant::now();
        activity.insert(principal_name.to_string(), now);
        trace!("Recorded activity for Principal: {}", principal_name);

        // Also update global activity
        drop(activity);
        let mut global = self.global_last_activity.write().await;
        *global = now;
    }

    /// Record global activity (any agent)
    pub async fn record_global_activity(&self) {
        let mut global = self.global_last_activity.write().await;
        *global = Instant::now();
        trace!("Recorded global activity");
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

    /// Check if any Principal has been active within the threshold
    pub async fn is_global_idle(&self, threshold_minutes: u64) -> bool {
        let threshold = Duration::from_secs(threshold_minutes * 60);
        let global = self.global_last_activity.read().await;
        let elapsed = Instant::now().duration_since(*global);
        elapsed >= threshold
    }

    /// Get list of Principals that have been idle for at least `threshold_minutes`
    pub async fn get_idle_principals(&self, threshold_minutes: u64) -> Vec<String> {
        let threshold = Duration::from_secs(threshold_minutes * 60);
        let activity = self.last_activity.read().await;
        let now = Instant::now();

        activity
            .iter()
            .filter(|(_, last)| now.duration_since(**last) >= threshold)
            .map(|(principal_name, _)| principal_name.clone())
            .collect()
    }

    /// Get duration since last activity for a Principal
    pub async fn idle_duration(&self, principal_name: &str) -> Option<Duration> {
        let activity = self.last_activity.read().await;
        activity
            .get(principal_name)
            .map(|last| Instant::now().duration_since(*last))
    }

    /// Get global idle duration
    pub async fn global_idle_duration(&self) -> Duration {
        let global = self.global_last_activity.read().await;
        Instant::now().duration_since(*global)
    }

    /// Reset activity tracking for a Principal
    pub async fn reset_principal(&self, principal_name: &str) {
        let mut activity = self.last_activity.write().await;
        activity.remove(principal_name);
        debug!("Reset activity tracking for Principal: {}", principal_name);
    }

    /// Get all tracked Principals
    pub async fn tracked_principals(&self) -> Vec<String> {
        let activity = self.last_activity.read().await;
        activity.keys().cloned().collect()
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

    #[tokio::test]
    async fn test_global_activity() {
        let detector = IdleDetector::new();

        // Initially global should NOT be idle (activity recorded at creation)
        assert!(!detector.is_global_idle(1).await);

        // Record global activity again
        detector.record_global_activity().await;

        // Should still not be idle
        assert!(!detector.is_global_idle(1).await);
    }

    #[tokio::test]
    async fn test_get_idle_principals() {
        let detector = IdleDetector::new();

        // No Principals tracked yet
        let idle = detector.get_idle_principals(1).await;
        assert!(idle.is_empty());

        // Add some Principals
        detector.record_activity("principal-a").await;
        detector.record_activity("principal-b").await;

        // Both should be tracked but not idle yet
        let tracked = detector.tracked_principals().await;
        assert_eq!(tracked.len(), 2);

        // Check idle Principals (should be none with activity just recorded)
        let idle = detector.get_idle_principals(60).await; // 60 minute threshold
        assert!(idle.is_empty());
    }

    #[tokio::test]
    async fn test_idle_duration() {
        let detector = IdleDetector::new();

        // No activity recorded
        assert!(detector.idle_duration("my-principal").await.is_none());

        // Record activity
        detector.record_activity("my-principal").await;

        // Should have some duration (very small)
        let duration = detector.idle_duration("my-principal").await.unwrap();
        assert!(duration < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn test_reset_principal() {
        let detector = IdleDetector::new();

        // Record and verify
        detector.record_activity("my-principal").await;
        assert!(!detector.is_idle("my-principal", 1).await);

        // Reset
        detector.reset_principal("my-principal").await;

        // Should be considered idle (no activity recorded)
        assert!(detector.is_idle("my-principal", 1).await);
    }
}
