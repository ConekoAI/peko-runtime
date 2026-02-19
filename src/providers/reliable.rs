//! Reliable provider wrapper with retry + fallback
//!
//! Wraps multiple providers to provide automatic retries and fallback behavior.

use super::Provider;
use async_trait::async_trait;
use std::time::Duration;
use tracing::{info, warn};

/// Provider wrapper with retry + fallback behavior
pub struct ReliableProvider {
    providers: Vec<(String, Box<dyn Provider>)>,
    max_retries: u32,
    base_backoff_ms: u64,
}

impl ReliableProvider {
    /// Create a new reliable provider
    ///
    /// # Arguments
    /// * `providers` - Vector of (name, provider) tuples, tried in order
    /// * `max_retries` - Maximum retries per provider before falling back
    /// * `base_backoff_ms` - Initial backoff in milliseconds (minimum 50)
    #[must_use] 
    pub fn new(
        providers: Vec<(String, Box<dyn Provider>)>,
        max_retries: u32,
        base_backoff_ms: u64,
    ) -> Self {
        Self {
            providers,
            max_retries,
            base_backoff_ms: base_backoff_ms.max(50),
        }
    }

    /// Create with sensible defaults (3 retries, 100ms base backoff)
    #[must_use] 
    pub fn with_defaults(providers: Vec<(String, Box<dyn Provider>)>) -> Self {
        Self::new(providers, 3, 100)
    }

    /// Add a provider to the chain
    pub fn add_provider(mut self, name: impl Into<String>, provider: Box<dyn Provider>) -> Self {
        self.providers.push((name.into(), provider));
        self
    }
}

#[async_trait]
impl Provider for ReliableProvider {
    fn name(&self) -> &'static str {
        "reliable"
    }

    async fn complete(&self, prompt: &str) -> anyhow::Result<String> {
        self.chat_with_system(None, prompt, "default", 0.7).await
    }

    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        let mut failures = Vec::new();

        for (provider_name, provider) in &self.providers {
            let mut backoff_ms = self.base_backoff_ms;

            for attempt in 0..=self.max_retries {
                match provider
                    .chat_with_system(system_prompt, message, model, temperature)
                    .await
                {
                    Ok(resp) => {
                        if attempt > 0 {
                            info!(
                                provider = provider_name,
                                attempt, "Provider recovered after retries"
                            );
                        }
                        return Ok(resp);
                    }
                    Err(e) => {
                        failures.push(format!(
                            "{} attempt {}/{}: {}",
                            provider_name,
                            attempt + 1,
                            self.max_retries + 1,
                            e
                        ));

                        if attempt < self.max_retries {
                            warn!(
                                provider = provider_name,
                                attempt = attempt + 1,
                                max_retries = self.max_retries,
                                "Provider call failed, retrying"
                            );
                            tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                            backoff_ms = (backoff_ms.saturating_mul(2)).min(10_000);
                        }
                    }
                }
            }

            warn!(provider = provider_name, "Switching to fallback provider");
        }

        anyhow::bail!("All providers failed. Attempts:\n{}", failures.join("\n"))
    }

    async fn warmup(&self) -> anyhow::Result<()> {
        if let Some((name, provider)) = self.providers.first() {
            info!(provider = name, "Warming up provider connection pool");
            provider.warmup().await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct MockProvider {
        calls: Arc<AtomicUsize>,
        fail_until_attempt: usize,
        response: &'static str,
    }

    #[async_trait]
    impl Provider for MockProvider {
        fn name(&self) -> &str {
            "mock"
        }

        async fn complete(&self, _prompt: &str) -> anyhow::Result<String> {
            let calls = self.calls.fetch_add(1, Ordering::SeqCst);
            if calls < self.fail_until_attempt {
                Err(anyhow::anyhow!("Simulated failure"))
            } else {
                Ok(self.response.to_string())
            }
        }

        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            self.complete("test").await
        }
    }

    #[tokio::test]
    async fn test_reliable_provider_succeeds_first_try() {
        let calls = Arc::new(AtomicUsize::new(0));
        let inner = MockProvider {
            calls: calls.clone(),
            fail_until_attempt: 0,
            response: "success",
        };

        let reliable = ReliableProvider::new(vec![("primary".to_string(), Box::new(inner))], 2, 10);

        let result = reliable.complete("test").await.unwrap();
        assert_eq!(result, "success");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_reliable_provider_retries_then_succeeds() {
        let calls = Arc::new(AtomicUsize::new(0));
        let inner = MockProvider {
            calls: calls.clone(),
            fail_until_attempt: 2,
            response: "recovered",
        };

        let reliable = ReliableProvider::new(vec![("primary".to_string(), Box::new(inner))], 3, 10);

        let result = reliable.complete("test").await.unwrap();
        assert_eq!(result, "recovered");
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_reliable_provider_fallback() {
        let calls1 = Arc::new(AtomicUsize::new(0));
        let calls2 = Arc::new(AtomicUsize::new(0));

        let primary = MockProvider {
            calls: calls1.clone(),
            fail_until_attempt: 999, // Always fails
            response: "primary",
        };

        let fallback = MockProvider {
            calls: calls2.clone(),
            fail_until_attempt: 0,
            response: "fallback",
        };

        let reliable = ReliableProvider::new(
            vec![
                ("primary".to_string(), Box::new(primary)),
                ("fallback".to_string(), Box::new(fallback)),
            ],
            1,
            10,
        );

        let result = reliable.complete("test").await.unwrap();
        assert_eq!(result, "fallback");
        assert_eq!(calls1.load(Ordering::SeqCst), 2); // 2 attempts (initial + 1 retry)
        assert_eq!(calls2.load(Ordering::SeqCst), 1);
    }
}
