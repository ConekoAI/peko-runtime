//! `provider_edit` domain request handler (RP6).
//!
//! Owns the `RequestPacket::{ModelUpdate, ModelRemove, ModelTest}`
//! IPC variants. These let the desktop mutate the model catalog and
//! live-validate entries without shelling out to the CLI.
//!
//! The handler holds a narrow [`ModelEditHost`] port; the daemon-side
//! implementation (`AppState`) is reached only through the trait, so this
//! module never imports `crate::daemon::state::AppState` directly.
//!
//! Boundary rules:
//! - Dependency inversion: the consumer (`ipc::handlers::provider_edit`)
//!   defines the [`ModelEditHost`] trait; the producer
//!   (`daemon::state`) implements it (same pattern as the rest of the
//!   F6/F7 handler family).
//! - F6: this module must not import any other `ipc::handlers::*` module.

use std::sync::Arc;

use async_trait::async_trait;

use crate::ipc::handlers::RequestHandler;
use crate::ipc::packet::{ModelSummary, ModelUpdateArgs, RequestPacket, ResponsePacket};
use crate::ipc::response_sink::ResponseSink;
use crate::ipc::send_response::send_response;
use crate::ipc::server::PeerAddr;
use peko_auth::caller::CallerContext;

/// Narrow port the `provider_edit` handler uses to mutate the
/// catalog. Split into three methods so each IPC variant maps to
/// exactly one host call; the daemon-side impl owns the
/// `ModelCatalog` interactions.
#[async_trait]
pub(crate) trait ModelEditHost: Send + Sync {
    /// Merge `args` into the existing catalog entry identified by
    /// `args.id` and persist the result. Returns the updated
    /// catalog-summary view.
    async fn update_model(&self, args: ModelUpdateArgs) -> anyhow::Result<ModelSummary>;

    /// Remove the model with this id. Returns `true` when an entry
    /// was actually removed.
    async fn remove_model(&self, id: &str) -> anyhow::Result<bool>;

    /// Live-ping the model identified by `id` and report the
    /// structured outcome. Returns `Err` for configuration-level
    /// errors; HTTP-level failures come back as a structured
    /// [`crate::providers::validator::CredentialTestOutcome`] with
    /// `ok = false`.
    async fn model_test(
        &self,
        id: &str,
    ) -> anyhow::Result<crate::providers::validator::CredentialTestOutcome>;
}

/// `provider_edit` domain request handler. Constructed with an
/// `Arc<dyn ModelEditHost>` (typically `Arc::new(app_state.clone())`
/// from the dispatcher).
pub(crate) struct ProviderEditHandler {
    host: Arc<dyn ModelEditHost>,
}

impl ProviderEditHandler {
    pub(crate) fn new(host: Arc<dyn ModelEditHost>) -> Self {
        Self { host }
    }
}

#[async_trait]
impl RequestHandler for ProviderEditHandler {
    fn domain(&self) -> &'static str {
        "provider_edit"
    }

    fn matches(&self, request: &RequestPacket) -> bool {
        matches!(
            request,
            RequestPacket::ModelUpdate { .. }
                | RequestPacket::ModelRemove { .. }
                | RequestPacket::ModelTest { .. }
        )
    }

    async fn handle(
        &self,
        request: RequestPacket,
        _caller: &CallerContext,
        sink: &dyn ResponseSink,
        _peer: &PeerAddr,
    ) -> anyhow::Result<()> {
        match request {
            RequestPacket::ModelUpdate { request_id, args } => {
                if args.id.is_empty() {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: "model id must not be empty".to_string(),
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                }

                match self.host.update_model(args).await {
                    Ok(model) => {
                        let response = ResponsePacket::ModelUpdated { request_id, model };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("{e:#}"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }
            RequestPacket::ModelRemove { request_id, id } => {
                if id.is_empty() {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: "model id must not be empty".to_string(),
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                }

                match self.host.remove_model(&id).await {
                    Ok(removed) => {
                        let response = ResponsePacket::ModelRemoved {
                            request_id,
                            id,
                            removed,
                        };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("{e:#}"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }
            RequestPacket::ModelTest { request_id, id } => {
                if id.is_empty() {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: "model id must not be empty".to_string(),
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                }

                let outcome = match self.host.model_test(&id).await {
                    Ok(o) => o,
                    Err(e) => crate::providers::validator::CredentialTestOutcome {
                        ok: false,
                        message: e.to_string(),
                        latency_ms: 0,
                        http_status: None,
                        model_used: None,
                    },
                };
                let response = ResponsePacket::ModelTested {
                    request_id,
                    id,
                    ok: outcome.ok,
                    message: outcome.message,
                    latency_ms: outcome.latency_ms,
                    http_status: outcome.http_status,
                    model_used: outcome.model_used,
                    tested_at: chrono::Utc::now().to_rfc3339(),
                };
                send_response(sink, response).await?;
            }
            // `matches()` returned true, so the exhaustive list above
            // covers every owned variant. This arm is unreachable.
            _ => unreachable!("ProviderEditHandler::matches allowed an unhandled variant"),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    //! Pin the wire shape for model update/remove/test so a runtime
    //! regression surfaces as a test failure rather than as the
    //! desktop's Settings panel silently failing.

    use super::*;
    use crate::ipc::response_sink::ResponseSink;
    use std::sync::{Arc, Mutex};

    struct StubHost {
        update_ok: Option<ModelSummary>,
        remove_ok: Option<bool>,
        test_ok: Option<crate::providers::validator::CredentialTestOutcome>,
    }
    #[async_trait]
    impl ModelEditHost for StubHost {
        async fn update_model(&self, _args: ModelUpdateArgs) -> anyhow::Result<ModelSummary> {
            match &self.update_ok {
                Some(p) => Ok(p.clone()),
                None => anyhow::bail!("stub host: no update staged"),
            }
        }

        async fn remove_model(&self, _id: &str) -> anyhow::Result<bool> {
            match self.remove_ok {
                Some(b) => Ok(b),
                None => anyhow::bail!("stub host: no remove staged"),
            }
        }

        async fn model_test(
            &self,
            _id: &str,
        ) -> anyhow::Result<crate::providers::validator::CredentialTestOutcome> {
            match &self.test_ok {
                Some(o) => Ok(o.clone()),
                None => anyhow::bail!("stub host: no test staged"),
            }
        }
    }

    struct CaptureSink(Arc<Mutex<Vec<u8>>>);
    #[async_trait]
    impl ResponseSink for CaptureSink {
        async fn send_bytes(&self, bytes: &[u8]) -> std::io::Result<()> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(())
        }
    }

    fn test_caller() -> CallerContext {
        CallerContext::local()
    }

    fn test_peer() -> PeerAddr {
        PeerAddr::Ip("127.0.0.1:0".parse().expect("loopback addr"))
    }

    fn stub_model() -> ModelSummary {
        ModelSummary {
            id: "openai".to_string(),
            display_name: "OpenAI".to_string(),
            template_id: Some("openai".to_string()),
            api_type: "openai".to_string(),
            base_url: "https://api.openai.com".to_string(),
            model_id: "gpt-4o".to_string(),
            context_window: Some(128_000),
            max_output_tokens: Some(16_384),
            headers: Default::default(),
            credential_id: None,
            requires_key: true,
            is_local: false,
            enabled: true,
        }
    }

    #[tokio::test]
    async fn model_update_empty_id_returns_error_response() {
        let host = StubHost {
            update_ok: None,
            remove_ok: None,
            test_ok: None,
        };
        let handler = ProviderEditHandler::new(Arc::new(host));
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sink = CaptureSink(buf.clone());

        handler
            .handle(
                RequestPacket::ModelUpdate {
                    request_id: 81,
                    args: ModelUpdateArgs {
                        id: "".to_string(),
                        ..Default::default()
                    },
                },
                &test_caller(),
                &sink,
                &test_peer(),
            )
            .await
            .expect("handle should succeed (the error is in the response packet)");

        let bytes = buf.lock().unwrap().clone();
        let json: serde_json::Value =
            serde_json::from_slice(&bytes).expect("response should be valid JSON");
        assert_eq!(
            json.get("type").and_then(|v| v.as_str()),
            Some("error"),
            "empty id must produce an error packet"
        );
    }

    #[tokio::test]
    async fn model_update_emits_model_updated() {
        let host = StubHost {
            update_ok: Some(stub_model()),
            remove_ok: None,
            test_ok: None,
        };
        let handler = ProviderEditHandler::new(Arc::new(host));
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sink = CaptureSink(buf.clone());

        handler
            .handle(
                RequestPacket::ModelUpdate {
                    request_id: 82,
                    args: ModelUpdateArgs {
                        id: "openai".to_string(),
                        display_name: Some("OpenAI (edited)".to_string()),
                        ..Default::default()
                    },
                },
                &test_caller(),
                &sink,
                &test_peer(),
            )
            .await
            .expect("handle should succeed");

        let bytes = buf.lock().unwrap().clone();
        let json: serde_json::Value =
            serde_json::from_slice(&bytes).expect("response should be valid JSON");
        assert_eq!(
            json.get("type").and_then(|v| v.as_str()),
            Some("model_updated"),
            "successful update must produce a model_updated packet"
        );
        assert_eq!(json.get("request_id").and_then(|v| v.as_u64()), Some(82));
        let model = json.get("model").expect("response should have model");
        assert_eq!(model.get("id").and_then(|v| v.as_str()), Some("openai"));
    }

    #[tokio::test]
    async fn model_remove_emits_model_removed() {
        let host = StubHost {
            update_ok: None,
            remove_ok: Some(true),
            test_ok: None,
        };
        let handler = ProviderEditHandler::new(Arc::new(host));
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sink = CaptureSink(buf.clone());

        handler
            .handle(
                RequestPacket::ModelRemove {
                    request_id: 83,
                    id: "openai".to_string(),
                },
                &test_caller(),
                &sink,
                &test_peer(),
            )
            .await
            .expect("handle should succeed");

        let bytes = buf.lock().unwrap().clone();
        let json: serde_json::Value =
            serde_json::from_slice(&bytes).expect("response should be valid JSON");
        assert_eq!(
            json.get("type").and_then(|v| v.as_str()),
            Some("model_removed")
        );
        assert_eq!(json.get("request_id").and_then(|v| v.as_u64()), Some(83));
        assert_eq!(json.get("id").and_then(|v| v.as_str()), Some("openai"));
        assert_eq!(json.get("removed").and_then(|v| v.as_bool()), Some(true));
    }

    #[tokio::test]
    async fn model_test_emits_model_tested() {
        let host = StubHost {
            update_ok: None,
            remove_ok: None,
            test_ok: Some(crate::providers::validator::CredentialTestOutcome {
                ok: false,
                message: "HTTP 401: invalid api key".to_string(),
                latency_ms: 187,
                http_status: Some(401),
                model_used: Some("gpt-4o".to_string()),
            }),
        };
        let handler = ProviderEditHandler::new(Arc::new(host));
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sink = CaptureSink(buf.clone());

        handler
            .handle(
                RequestPacket::ModelTest {
                    request_id: 84,
                    id: "openai".to_string(),
                },
                &test_caller(),
                &sink,
                &test_peer(),
            )
            .await
            .expect("handle should succeed");

        let bytes = buf.lock().unwrap().clone();
        let json: serde_json::Value =
            serde_json::from_slice(&bytes).expect("response should be valid JSON");
        assert_eq!(
            json.get("type").and_then(|v| v.as_str()),
            Some("model_tested")
        );
        assert_eq!(json.get("request_id").and_then(|v| v.as_u64()), Some(84));
        assert_eq!(json.get("id").and_then(|v| v.as_str()), Some("openai"));
        assert_eq!(json.get("ok").and_then(|v| v.as_bool()), Some(false));
        assert_eq!(json.get("latency_ms").and_then(|v| v.as_u64()), Some(187));
        assert_eq!(json.get("http_status").and_then(|v| v.as_u64()), Some(401));
        assert_eq!(
            json.get("model_used").and_then(|v| v.as_str()),
            Some("gpt-4o")
        );
        assert!(
            json.get("tested_at")
                .and_then(|v| v.as_str())
                .map(|s| !s.is_empty())
                .unwrap_or(false),
            "tested_at should be a non-empty ISO-8601 string"
        );
    }

    #[tokio::test]
    async fn model_test_maps_host_error_to_structured_failure() {
        let host = StubHost {
            update_ok: None,
            remove_ok: None,
            test_ok: None, // host will bail with "no test staged"
        };
        let handler = ProviderEditHandler::new(Arc::new(host));
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sink = CaptureSink(buf.clone());

        handler
            .handle(
                RequestPacket::ModelTest {
                    request_id: 85,
                    id: "openai".to_string(),
                },
                &test_caller(),
                &sink,
                &test_peer(),
            )
            .await
            .expect("handler should not bubble Err");

        let bytes = buf.lock().unwrap().clone();
        let json: serde_json::Value =
            serde_json::from_slice(&bytes).expect("response should be valid JSON");
        assert_eq!(
            json.get("type").and_then(|v| v.as_str()),
            Some("model_tested")
        );
        assert_eq!(json.get("ok").and_then(|v| v.as_bool()), Some(false));
        assert!(
            json.get("message")
                .and_then(|v| v.as_str())
                .map(|s| s.contains("no test staged"))
                .unwrap_or(false),
            "message should carry the original error reason"
        );
    }
}
