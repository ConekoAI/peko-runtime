//! Provider trait

use async_trait::async_trait;

/// LLM Provider trait
#[async_trait]
pub trait Provider: Send + Sync {
    fn name(&self) -> &str;
    
    async fn complete(
        &self,
        prompt: &str,
    ) -> anyhow::Result<String>;
}
