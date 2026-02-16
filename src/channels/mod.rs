//! Communication channels

pub mod cli;
pub mod http;

/// Channel trait for communication
pub trait Channel: Send + Sync {
    fn name(&self) -> &str;
    async fn send(&self, message: &str) -> anyhow::Result<()>;
    async fn receive(&mut self) -> anyhow::Result<Option<String>>;
}
