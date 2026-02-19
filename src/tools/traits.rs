//! Tool trait

use async_trait::async_trait;

/// Tool trait for agent capabilities
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;

    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value>;
}
