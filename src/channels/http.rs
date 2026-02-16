//! HTTP webhook channel

use super::Channel;

/// HTTP webhook channel
pub struct HttpChannel {
    endpoint: String,
}

impl HttpChannel {
    pub fn new(endpoint: &str) -> Self {
        Self {
            endpoint: endpoint.to_string(),
        }
    }
}

impl Channel for HttpChannel {
    fn name(&self) -> &str {
        "http"
    }

    async fn send(&self, _message: &str) -> anyhow::Result<()> {
        // TODO: Send HTTP request
        Ok(())
    }

    async fn receive(&mut self) -> anyhow::Result<Option<String>> {
        // TODO: Start HTTP server and receive
        Ok(None)
    }
}
