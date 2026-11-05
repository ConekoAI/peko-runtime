//! `ModelDiscover`: the chat models an endpoint lists, read from the
//! endpoint with the given key, so a model is chosen without knowing its
//! id. Nothing is stored; `ModelAdd` does that.
//!
//! Also answers the retired `ModelTemplates` with an empty list for
//! clients built against the old vendor preset gallery (ADR-068).

use crate::ipc::handlers::RequestHandler;
use crate::ipc::packet::{ModelDiscoverArgs, RequestPacket, ResponsePacket};
use crate::ipc::response_sink::ResponseSink;
use crate::ipc::send_response::send_response;
use crate::ipc::server::PeerAddr;
use async_trait::async_trait;
use peko_auth::caller::CallerContext;
use peko_providers::validator::Validator;
use peko_providers::{ApiFormat, ModelConfig};

pub(crate) struct ProviderTemplatesHandler;

impl ProviderTemplatesHandler {
    pub(crate) fn new() -> Self {
        Self
    }
}

/// The chat models the endpoint in `args` lists.
pub(crate) async fn discover(args: ModelDiscoverArgs) -> Result<Vec<String>, String> {
    let base_url = args.base_url.trim();
    if base_url.is_empty() {
        return Err("a base URL is required".to_string());
    }
    let api_format = ApiFormat::from_wire(args.api_format.trim())
        .ok_or_else(|| format!("unknown API format '{}'", args.api_format))?;
    let key = args
        .key
        .filter(|k| !k.trim().is_empty())
        .map(|k| secrecy::SecretString::from(k.trim().to_string()));
    let mut probe = ModelConfig::new("discover", api_format, base_url, String::new());
    probe.requires_key = key.is_some();
    Validator::list_models(&probe, key.as_ref()).await
}

#[async_trait]
impl RequestHandler for ProviderTemplatesHandler {
    fn domain(&self) -> &'static str {
        "provider_templates"
    }
    fn matches(&self, request: &RequestPacket) -> bool {
        matches!(
            request,
            RequestPacket::ModelTemplates { .. } | RequestPacket::ModelDiscover { .. }
        )
    }
    async fn handle(
        &self,
        request: RequestPacket,
        _caller: &CallerContext,
        sink: &dyn ResponseSink,
        _peer: &PeerAddr,
    ) -> anyhow::Result<()> {
        let response = match request {
            RequestPacket::ModelTemplates { request_id } => ResponsePacket::ModelTemplates {
                request_id,
                presets: vec![],
            },
            RequestPacket::ModelDiscover { request_id, args } => match discover(args).await {
                Ok(models) => ResponsePacket::ModelsDiscovered { request_id, models },
                Err(message) => ResponsePacket::Error {
                    request_id,
                    message,
                },
            },
            _ => unreachable!("ProviderTemplatesHandler::matches allowed an unhandled variant"),
        };
        send_response(sink, response).await?;
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

    async fn respond(request: RequestPacket) -> serde_json::Value {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let sink = CaptureSink(bytes.clone());
        ProviderTemplatesHandler::new()
            .handle(
                request,
                &CallerContext::local(),
                &sink,
                &PeerAddr::Ip("127.0.0.1:0".parse().unwrap()),
            )
            .await
            .unwrap();
        let bytes = bytes.lock().unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn discover_lists_the_endpoints_chat_models() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let _ = s.read(&mut buf).await;
            let body =
                r#"{"data":[{"id":"text-embedding-3-small"},{"id":"gpt-4o"},{"id":"gpt-5"}]}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = s.write_all(resp.as_bytes()).await;
        });
        let r = respond(RequestPacket::ModelDiscover {
            request_id: 8,
            args: ModelDiscoverArgs {
                base_url: format!("http://{addr}"),
                api_format: "openai_completions".into(),
                key: Some("sk-test".into()),
            },
        })
        .await;
        assert_eq!(r["type"], "models_discovered", "{r}");
        assert_eq!(r["models"], serde_json::json!(["gpt-4o", "gpt-5"]));
    }

    #[tokio::test]
    async fn discover_refuses_what_it_cannot_reach() {
        let r = respond(RequestPacket::ModelDiscover {
            request_id: 9,
            args: ModelDiscoverArgs {
                base_url: "https://example.com/v1".into(),
                api_format: "nope".into(),
                key: None,
            },
        })
        .await;
        assert_eq!(r["type"], "error");
        assert_eq!(r["message"], "unknown API format 'nope'");
        let r = respond(RequestPacket::ModelDiscover {
            request_id: 10,
            args: ModelDiscoverArgs::default(),
        })
        .await;
        assert_eq!(r["message"], "a base URL is required");
    }

    #[test]
    fn a_discover_request_never_prints_its_key() {
        let args = ModelDiscoverArgs {
            key: Some("sk-ant-secret".into()),
            ..Default::default()
        };
        assert!(!format!("{args:?}").contains("secret"));
    }
}
