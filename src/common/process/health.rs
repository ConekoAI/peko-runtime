//! Health check loop abstraction

use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio::time::interval;
use tracing::warn;

/// Type alias for a health check function
pub type HealthCheckFn = Arc<dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>> + Send + Sync>;

/// A running health check loop
pub struct HealthCheckLoop {
    handle: JoinHandle<()>,
}

impl HealthCheckLoop {
    /// Start a new health check loop
    ///
    /// The `check_fn` is called every `interval_secs`. If it returns `false`,
    /// `on_unhealthy` is called. The loop continues until the returned
    /// `HealthCheckLoop` is dropped or aborted.
    pub fn start<F, Fut>(
        name: impl Into<String>,
        interval_secs: u64,
        check_fn: F,
        mut on_unhealthy: impl FnMut() + Send + 'static,
    ) -> Self
    where
        F: Fn() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = bool> + Send + 'static,
    {
        let name = name.into();
        let mut ticker = interval(Duration::from_secs(interval_secs));

        let handle = tokio::spawn(async move {
            loop {
                ticker.tick().await;

                let healthy = check_fn().await;
                if !healthy {
                    warn!("Health check failed for '{}'", name);
                    on_unhealthy();
                }
            }
        });

        Self { handle }
    }

    /// Abort the health check loop
    pub fn abort(&self) {
        self.handle.abort();
    }

    /// Check if the loop has finished (should only happen if aborted or panicked)
    pub fn is_finished(&self) -> bool {
        self.handle.is_finished()
    }
}

impl Drop for HealthCheckLoop {
    fn drop(&mut self) {
        self.handle.abort();
    }
}
