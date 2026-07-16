//! `provider_add` domain request handler (T-109b).
//!
//! Owns the `RequestPacket::ModelAdd` IPC variant. The desktop's
//! "Add Model" modal calls this so the picker can add a new
//! model to the catalog without shelling out to the CLI.
//! Mirrors `peko model add` — same preset + custom modes,
//! same `--key` folding, same bare-invocation guard.
//!
//! The handler holds a narrow [`ModelAddHost`] port; the
//! daemon-side implementation (`AppState`) is reached only through
//! the trait, so this module never imports
//! `crate::daemon::state::AppState` directly.
//!
//! Boundary rules:
//! - Dependency inversion: the consumer (`ipc::handlers::provider_add`)
//!   defines the [`ModelAddHost`] trait; the producer
//!   (`daemon::state`) implements it (same pattern as the rest of
//!   the F6/F7 handler family).
//! - F6: this module must not import any other `ipc::handlers::*` module.

use std::sync::Arc;

use async_trait::async_trait;

use crate::auth::caller::CallerContext;
use crate::ipc::handlers::RequestHandler;
use crate::ipc::packet::{ModelAddArgs, ModelSummary, RequestPacket, ResponsePacket};
use crate::ipc::response_sink::ResponseSink;
use crate::ipc::send_response::send_response;
use crate::ipc::server::PeerAddr;

/// Narrow port the `provider_add` handler uses to mutate the
/// catalog + vault. Single async method so the handler stays a
/// thin translator and the domain logic (entry construction,
/// upsert, vault write) lives in one place —
/// the `AppState` impl — exactly mirroring the
/// `peko model add` CLI flow.
///
/// The host returns the catalog-summary `ModelSummary` of the
/// newly-inserted entry so the handler can emit
/// `ResponsePacket::ModelAdded` without a follow-up
/// `model_list` call.
#[async_trait]
pub(crate) trait ModelAddHost: Send + Sync {
    async fn add_model(&self, args: ModelAddArgs) -> anyhow::Result<ModelSummary>;
}

/// `provider_add` domain request handler. Constructed with an
/// `Arc<dyn ModelAddHost>` (typically `Arc::new(app_state.clone())`
/// from the dispatcher).
pub(crate) struct ProviderAddHandler {
    host: Arc<dyn ModelAddHost>,
}

impl ProviderAddHandler {
    pub(crate) fn new(host: Arc<dyn ModelAddHost>) -> Self {
        Self { host }
    }
}

#[async_trait]
impl RequestHandler for ProviderAddHandler {
    fn domain(&self) -> &'static str {
        "provider_add"
    }

    fn matches(&self, request: &RequestPacket) -> bool {
        matches!(request, RequestPacket::ModelAdd { .. })
    }

    async fn handle(
        &self,
        request: RequestPacket,
        _caller: &CallerContext,
        sink: &dyn ResponseSink,
        _peer: &PeerAddr,
    ) -> anyhow::Result<()> {
        match request {
            RequestPacket::ModelAdd { request_id, args } => {
                // Bare-invocation guard — same shape as the CLI.
                // Either `template` or `custom` must be supplied; an
                // empty request is a user error, not a system error,
                // so we reply with `ResponsePacket::Error` carrying
                // the same hint string the CLI prints. This keeps the
                // two surfaces symmetric (F6/F7 contract).
                if args.template.is_none() && !args.custom {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: "either template or custom must be set.\n\n\
                                  Quick start:\n\
                                    model_add template=anthropic key=$ANTHROPIC_API_KEY\n\n\
                                  List presets:\n\
                                    model_templates"
                            .to_string(),
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                }

                match self.host.add_model(args).await {
                    Ok(model) => {
                        let response = ResponsePacket::ModelAdded { request_id, model };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        // User-facing domain errors (bad preset id,
                        // missing required field for a custom add,
                        // unknown --api-format, etc.) become
                        // `ResponsePacket::Error` with the host's
                        // message — same surface as the CLI's
                        // `anyhow::bail!`. System errors (catalog
                        // persistence fail) propagate via `?` and
                        // are logged by the server.
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("{e:#}"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }
            // `matches()` returned true, so the exhaustive list above
            // covers every owned variant. This arm is unreachable.
            _ => unreachable!("ProviderAddHandler::matches allowed an unhandled variant"),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    //! Pin the wire shape and the bare-invocation guard so a
    //! runtime regression surfaces as a test failure rather than
    //! as the desktop's "Add Model" modal reporting success on
    //! an empty request. Mirrors
    //! `credential_list_emits_rows_with_has_key_flag`.

    use super::*;
    use crate::ipc::response_sink::ResponseSink;
    use std::sync::{Arc, Mutex};

    /// Stub host — each test stages the catalog entry it wants
    /// the host to "return". The bare-invocation tests don't even
    /// touch the host (the handler short-circuits), but a stub is
    /// still needed to construct the handler.
    struct StubHost(Option<ModelSummary>);
    #[async_trait]
    impl ModelAddHost for StubHost {
        async fn add_model(&self, _args: ModelAddArgs) -> anyhow::Result<ModelSummary> {
            match &self.0 {
                Some(p) => Ok(p.clone()),
                None => anyhow::bail!("stub host: no entry staged"),
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

    fn stub_anthropic() -> ModelSummary {
        ModelSummary {
            id: "anthropic".to_string(),
            display_name: "Anthropic".to_string(),
            template_id: Some("anthropic".to_string()),
            api_type: "anthropic".to_string(),
            base_url: "https://api.anthropic.com".to_string(),
            model_id: "claude-sonnet-4-5".to_string(),
            context_window: Some(200_000),
            max_output_tokens: Some(8_192),
            capabilities: vec![],
            headers: Default::default(),
            credential_id: None,
            requires_key: true,
            is_local: false,
            enabled: true,
        }
    }

    #[tokio::test]
    async fn model_add_bare_invocation_returns_error_response() {
        // Neither `template` nor `custom` is set — the handler
        // must reply with `ResponsePacket::Error` carrying the
        // same hint string the CLI's `peko model add` prints,
        // not a system error and not a successful `ModelAdded`.
        let host = StubHost(None);
        let handler = ProviderAddHandler::new(Arc::new(host));
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sink = CaptureSink(buf.clone());

        handler
            .handle(
                RequestPacket::ModelAdd {
                    request_id: 71,
                    args: ModelAddArgs::default(),
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
            "bare invocation must produce an error packet"
        );
        let message = json.get("message").and_then(|v| v.as_str()).unwrap_or("");
        assert!(
            message.contains("template") && message.contains("custom"),
            "error message should mention both template and custom, got: {message}"
        );
    }

    #[tokio::test]
    async fn model_add_template_emits_model_added() {
        // Happy path: the host returns a ModelSummary, the handler
        // wraps it in `ResponsePacket::ModelAdded`. We assert
        // the wire shape so a future field addition (e.g. adding
        // `headers` to `ModelSummary`) surfaces as a test diff.
        let host = StubHost(Some(stub_anthropic()));
        let handler = ProviderAddHandler::new(Arc::new(host));
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sink = CaptureSink(buf.clone());

        handler
            .handle(
                RequestPacket::ModelAdd {
                    request_id: 72,
                    args: ModelAddArgs {
                        template: Some("anthropic".to_string()),
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
            Some("model_added"),
            "successful add must produce a model_added packet"
        );
        assert_eq!(json.get("request_id").and_then(|v| v.as_u64()), Some(72));

        let model = json
            .get("model")
            .expect("response should have a model object");
        assert_eq!(model.get("id").and_then(|v| v.as_str()), Some("anthropic"));
        assert_eq!(
            model.get("requires_key").and_then(|v| v.as_bool()),
            Some(true)
        );
    }

    #[tokio::test]
    async fn model_add_host_error_becomes_error_response() {
        // Domain errors from the host (e.g. unknown preset id,
        // missing required custom field) must surface as
        // `ResponsePacket::Error` with the host's message — the
        // desktop modal renders this directly under the form.
        let host = StubHost(None); // host will bail with "no entry staged"
        let handler = ProviderAddHandler::new(Arc::new(host));
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sink = CaptureSink(buf.clone());

        handler
            .handle(
                RequestPacket::ModelAdd {
                    request_id: 73,
                    args: ModelAddArgs {
                        custom: true,
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
            "host error must produce an error packet"
        );
        let message = json.get("message").and_then(|v| v.as_str()).unwrap_or("");
        assert!(
            message.contains("no entry staged"),
            "error message should propagate the host's bail, got: {message}"
        );
    }
}
