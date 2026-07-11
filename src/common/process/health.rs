//! Health check loop abstraction

use std::time::Duration;

/// Wait for a health check to return `true` within a timeout.
///
/// Polls the `check_fn` every `interval`. Returns `Ok(true)` as soon as the
/// check passes, or `Ok(false)` if the timeout is reached without the check
/// ever returning `true`.
///
/// # Example
/// ```ignore
/// let ready = wait_for_healthy(
///     || async { check_daemon_ping().await },
///     Duration::from_secs(10),
///     Duration::from_millis(500),
/// ).await?;
/// ```
pub async fn wait_for_healthy<F, Fut>(
    mut check_fn: F,
    timeout: Duration,
    interval: Duration,
) -> anyhow::Result<bool>
where
    F: FnMut() -> Fut + Send,
    Fut: std::future::Future<Output = bool> + Send,
{
    let start = std::time::Instant::now();
    loop {
        if check_fn().await {
            return Ok(true);
        }
        if start.elapsed() >= timeout {
            return Ok(false);
        }
        tokio::time::sleep(interval).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_wait_for_healthy_success() {
        let mut count = 0;
        let result = wait_for_healthy(
            || {
                count += 1;
                async move { count >= 2 }
            },
            Duration::from_secs(5),
            Duration::from_millis(50),
        )
        .await;
        assert!(result.unwrap());
        assert_eq!(count, 2);
    }

    #[tokio::test]
    async fn test_wait_for_healthy_timeout() {
        let result = wait_for_healthy(
            || async { false },
            Duration::from_millis(100),
            Duration::from_millis(50),
        )
        .await;
        assert!(!result.unwrap());
    }
}
