//! Exponential Backoff for Tunnel Reconnection
//!
//! Implements jitter-free exponential backoff with a configurable cap.

use std::time::Duration;

/// Exponential backoff for reconnection delays.
#[derive(Debug, Clone)]
pub struct ExponentialBackoff {
    current_secs: u64,
    initial_secs: u64,
    max_secs: u64,
    base: u64,
}

impl ExponentialBackoff {
    /// Create a new backoff with the given parameters.
    ///
    /// # Arguments
    /// * `initial_secs` - Starting delay in seconds
    /// * `max_secs` - Maximum delay cap in seconds
    /// * `base` - Multiplication factor (default: 2)
    #[must_use]
    pub fn new(initial_secs: u64, max_secs: u64, base: u64) -> Self {
        Self {
            current_secs: initial_secs,
            initial_secs,
            max_secs,
            base: base.max(2),
        }
    }

    /// Get the next delay and advance the backoff.
    #[must_use]
    pub fn next(&mut self) -> Duration {
        let delay = Duration::from_secs(self.current_secs);
        self.current_secs = (self.current_secs * self.base).min(self.max_secs);
        delay
    }

    /// Reset the backoff to the initial delay.
    pub fn reset(&mut self) {
        self.current_secs = self.initial_secs;
    }

    /// Create the default backoff: 1s, 2s, 4s, 8s, ... capped at 60s.
    #[must_use]
    pub fn default_tunnel() -> Self {
        Self::new(1, 60, 2)
    }
}

impl Default for ExponentialBackoff {
    fn default() -> Self {
        Self::default_tunnel()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exponential_backoff() {
        let mut backoff = ExponentialBackoff::new(1, 60, 2);
        assert_eq!(backoff.next(), Duration::from_secs(1));
        assert_eq!(backoff.next(), Duration::from_secs(2));
        assert_eq!(backoff.next(), Duration::from_secs(4));
        assert_eq!(backoff.next(), Duration::from_secs(8));
        assert_eq!(backoff.next(), Duration::from_secs(16));
        assert_eq!(backoff.next(), Duration::from_secs(32));
        assert_eq!(backoff.next(), Duration::from_mins(1));
        assert_eq!(backoff.next(), Duration::from_mins(1)); // capped
    }

    #[test]
    fn test_backoff_reset() {
        let mut backoff = ExponentialBackoff::new(1, 60, 2);
        assert_eq!(backoff.next(), Duration::from_secs(1));
        assert_eq!(backoff.next(), Duration::from_secs(2));
        backoff.reset();
        assert_eq!(backoff.next(), Duration::from_secs(1));
    }

    #[test]
    fn test_backoff_with_base_3() {
        let mut backoff = ExponentialBackoff::new(1, 30, 3);
        assert_eq!(backoff.next(), Duration::from_secs(1));
        assert_eq!(backoff.next(), Duration::from_secs(3));
        assert_eq!(backoff.next(), Duration::from_secs(9));
        assert_eq!(backoff.next(), Duration::from_secs(27));
        assert_eq!(backoff.next(), Duration::from_secs(30)); // capped
    }
}
