//! Compatibility endpoint for clients using the retired vendor preset gallery.
//! `ModelTemplates` returns an empty list; models are configured explicitly.

use crate::ipc::handlers::RequestHandler;
use crate::ipc::packet::{RequestPacket, ResponsePacket};
use crate::ipc::response_sink::ResponseSink;
use crate::ipc::send_response::send_response;
use crate::ipc::server::PeerAddr;
use async_trait::async_trait;
use peko_auth::caller::CallerContext;

pub(crate) struct ProviderTemplatesHandler;

impl ProviderTemplatesHandler {
    pub(crate) fn new() -> Self {
        Self
    }
}

#[async_trait]
impl RequestHandler for ProviderTemplatesHandler {
    fn domain(&self) -> &'static str {
        "provider_templates"
    }
    fn matches(&self, request: &RequestPacket) -> bool {
        matches!(request, RequestPacket::ModelTemplates { .. })
    }
    async fn handle(
        &self,
        request: RequestPacket,
        _caller: &CallerContext,
        sink: &dyn ResponseSink,
        _peer: &PeerAddr,
    ) -> anyhow::Result<()> {
        if let RequestPacket::ModelTemplates { request_id } = request {
            send_response(
                sink,
                ResponsePacket::ModelTemplates {
                    request_id,
                    presets: vec![],
                },
            )
            .await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    struct CaptureSink(Arc<Mutex<Vec<u8>>>);
    #[async_trait]
    impl ResponseSink for CaptureSink {
        async fn send_bytes(&self, bytes: &[u8]) -> std::io::Result<()> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(())
        }
    }

    #[tokio::test]
    async fn legacy_discovery_returns_empty_presets() {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let sink = CaptureSink(bytes.clone());
        ProviderTemplatesHandler::new()
            .handle(
                RequestPacket::ModelTemplates { request_id: 51 },
                &CallerContext::local(),
                &sink,
                &PeerAddr::Ip("127.0.0.1:0".parse().unwrap()),
            )
            .await
            .unwrap();
        let response: serde_json::Value = serde_json::from_slice(&bytes.lock().unwrap()).unwrap();
        assert_eq!(response["type"], "model_templates");
        assert_eq!(response["request_id"], 51);
        assert_eq!(response["presets"], serde_json::json!([]));
    }
}
