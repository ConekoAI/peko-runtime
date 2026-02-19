//! Provider trait

use async_trait::async_trait;

/// LLM Provider trait
#[async_trait]
pub trait Provider: Send + Sync {
    fn name(&self) -> &str;

    /// Complete a prompt (legacy/simple interface)
    async fn complete(&self, prompt: &str) -> anyhow::Result<String> {
        self.chat(prompt, "default", 0.7).await
    }

    /// Chat with optional system prompt (zeroclaw-compatible interface)
    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String>;

    /// Simple chat interface
    async fn chat(&self, message: &str, model: &str, temperature: f64) -> anyhow::Result<String> {
        self.chat_with_system(None, message, model, temperature)
            .await
    }

    /// Warm up the HTTP connection pool
    async fn warmup(&self) -> anyhow::Result<()> {
        Ok(())
    }
}
