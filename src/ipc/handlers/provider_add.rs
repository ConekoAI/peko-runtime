//! `provider_add` domain request handler (T-109b).
//!
//! Owns the `RequestPacket::ProviderAdd` IPC variant. The desktop's
//! "Add Provider" modal calls this so the picker can add a new
//! provider to the catalog without shelling out to the CLI.
//! Mirrors `peko provider add` — same template + custom modes,
//! same `--key` + `--set-default` folding, same bare-invocation
//! guard.
//!
//! The handler holds a narrow [`ProviderAddHost`] port; the
//! daemon-side implementation (`AppState`) is reached only through
//! the trait, so this module never imports
//! `crate::daemon::state::AppState` directly.
//!
//! Boundary rules:
//! - Dependency inversion: the consumer (`ipc::handlers::provider_add`)
//!   defines the [`ProviderAddHost`] trait; the producer
//!   (`daemon::state`) implements it (same pattern as the rest of
//!   the F6/F7 handler family).
//! - F6: this module must not import any other `ipc::handlers::*` module.

use std::sync::Arc;

use async_trait::async_trait;

use crate::auth::caller::CallerContext;
use crate::ipc::handlers::RequestHandler;
use crate::ipc::packet::{ProviderAddArgs, ProviderInfo, RequestPacket, ResponsePacket};
use crate::ipc::response_sink::ResponseSink;
use crate::ipc::send_response::send_response;
use crate::ipc::server::PeerAddr;

/// Narrow port the `provider_add` handler uses to mutate the
/// catalog + vault. Single async method so the handler stays a
/// thin translator and the domain logic (entry construction,
/// upsert, vault write, default promotion) lives in one place —
/// the `AppState` impl — exactly mirroring the
/// `peko provider add` CLI flow (`commands/provider.rs:280`).
///
/// The host returns the catalog-summary `ProviderInfo` of the
/// newly-inserted entry so the handler can emit
/// `ResponsePacket::ProviderAdded` without a follow-up
/// `provider_list` call.
#[async_trait]
pub(crate) trait ProviderAddHost: Send + Sync {
    async fn add_provider(&self, args: ProviderAddArgs) -> anyhow::Result<ProviderInfo>;
}

/// `provider_add` domain request handler. Constructed with an
/// `Arc<dyn ProviderAddHost>` (typically `Arc::new(app_state.clone())`
/// from the dispatcher).
pub(crate) struct ProviderAddHandler {
    host: Arc<dyn ProviderAddHost>,
}

impl ProviderAddHandler {
    pub(crate) fn new(host: Arc<dyn ProviderAddHost>) -> Self {
        Self { host }
    }
}

#[async_trait]
impl RequestHandler for ProviderAddHandler {
    fn domain(&self) -> &'static str {
        "provider_add"
    }

    fn matches(&self, request: &RequestPacket) -> bool {
        matches!(request, RequestPacket::ProviderAdd { .. })
    }

    async fn handle(
        &self,
        request: RequestPacket,
        _caller: &CallerContext,
        sink: &dyn ResponseSink,
        _peer: &PeerAddr,
    ) -> anyhow::Result<()> {
        match request {
            RequestPacket::ProviderAdd { request_id, args } => {
                // Bare-invocation guard — same shape as the CLI
                // (`commands/provider.rs:284`). Either `template` or
                // `custom` must be supplied; an empty request is a
                // user error, not a system error, so we reply with
                // `ResponsePacket::Error` carrying the same hint
                // string the CLI prints. This keeps the two
                // surfaces symmetric (F6/F7 contract).
                if args.template.is_none() && !args.custom {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: "either template or custom must be set.\n\n\
                                  Quick start:\n\
                                    provider_add template=anthropic key=$ANTHROPIC_API_KEY set_default=true\n\n\
                                  List templates:\n\
                                    provider_templates"
                            .to_string(),
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                }

                match self.host.add_provider(args).await {
                    Ok(provider) => {
                        let response = ResponsePacket::ProviderAdded {
                            request_id,
                            provider,
                        };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        // User-facing domain errors (bad template id,
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
    //! as the desktop's "Add Provider" modal reporting success on
    //! an empty request. Mirrors
    //! `credential_list_emits_rows_with_has_key_flag`.

    use super::*;
    use crate::ipc::response_sink::ResponseSink;
    use std::sync::{Arc, Mutex};

    /// Stub host — each test stages the catalog entry it wants
    /// the host to "return". The bare-invocation tests don't even
    /// touch the host (the handler short-circuits), but a stub is
    /// still needed to construct the handler.
    struct StubHost(Option<ProviderInfo>);
    #[async_trait]
    impl ProviderAddHost for StubHost {
        async fn add_provider(&self, _args: ProviderAddArgs) -> anyhow::Result<ProviderInfo> {
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

    fn stub_anthropic() -> ProviderInfo {
        ProviderInfo {
            id: "anthropic".to_string(),
            display_name: "Anthropic".to_string(),
            api_type: "anthropic".to_string(),
            base_url: "https://api.anthropic.com".to_string(),
            requires_key: true,
            is_local: false,
            enabled: true,
            models: vec![],
            default_model_id: "claude-sonnet-4-5".to_string(),
            headers: Default::default(),
            is_default: false,
        }
    }

    #[tokio::test]
    async fn provider_add_bare_invocation_returns_error_response() {
        // Neither `template` nor `custom` is set — the handler
        // must reply with `ResponsePacket::Error` carrying the
        // same hint string the CLI's `peko provider add` prints,
        // not a system error and not a successful `ProviderAdded`.
        let host = StubHost(None);
        let handler = ProviderAddHandler::new(Arc::new(host));
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sink = CaptureSink(buf.clone());

        handler
            .handle(
                RequestPacket::ProviderAdd {
                    request_id: 71,
                    args: ProviderAddArgs::default(),
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
    async fn provider_add_template_emits_provider_added() {
        // Happy path: the host returns a ProviderInfo, the handler
        // wraps it in `ResponsePacket::ProviderAdded`. We assert
        // the wire shape so a future field addition (e.g. adding
        // `headers` to `ProviderInfo`) surfaces as a test diff.
        let host = StubHost(Some(stub_anthropic()));
        let handler = ProviderAddHandler::new(Arc::new(host));
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sink = CaptureSink(buf.clone());

        handler
            .handle(
                RequestPacket::ProviderAdd {
                    request_id: 72,
                    args: ProviderAddArgs {
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
            Some("provider_added"),
            "successful add must produce a provider_added packet"
        );
        assert_eq!(json.get("request_id").and_then(|v| v.as_u64()), Some(72));

        let provider = json
            .get("provider")
            .expect("response should have a provider object");
        assert_eq!(
            provider.get("id").and_then(|v| v.as_str()),
            Some("anthropic")
        );
        assert_eq!(
            provider.get("requires_key").and_then(|v| v.as_bool()),
            Some(true)
        );
    }

    #[tokio::test]
    async fn provider_add_host_error_becomes_error_response() {
        // Domain errors from the host (e.g. unknown template id,
        // missing required custom field) must surface as
        // `ResponsePacket::Error` with the host's message — the
        // desktop modal renders this directly under the form.
        let host = StubHost(None); // host will bail with "no entry staged"
        let handler = ProviderAddHandler::new(Arc::new(host));
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sink = CaptureSink(buf.clone());

        handler
            .handle(
                RequestPacket::ProviderAdd {
                    request_id: 73,
                    args: ProviderAddArgs {
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
