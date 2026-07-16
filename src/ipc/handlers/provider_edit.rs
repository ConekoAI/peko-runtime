//! `provider_edit` domain request handler (RP6).
//!
//! Owns the `RequestPacket::{ProviderUpdate, ProviderRemove,
//! ProviderSetDefault}` IPC variants. These let the desktop mutate the
//! provider catalog without shelling out to the CLI.
//!
//! The handler holds a narrow [`ProviderEditHost`] port; the daemon-side
//! implementation (`AppState`) is reached only through the trait, so this
//! module never imports `crate::daemon::state::AppState` directly.
//!
//! Boundary rules:
//! - Dependency inversion: the consumer (`ipc::handlers::provider_edit`)
//!   defines the [`ProviderEditHost`] trait; the producer
//!   (`daemon::state`) implements it (same pattern as the rest of the
//!   F6/F7 handler family).
//! - F6: this module must not import any other `ipc::handlers::*` module.

use std::sync::Arc;

use async_trait::async_trait;

use crate::auth::caller::CallerContext;
use crate::ipc::handlers::RequestHandler;
use crate::ipc::packet::{ProviderInfo, ProviderUpdateArgs, RequestPacket, ResponsePacket};
use crate::ipc::response_sink::ResponseSink;
use crate::ipc::send_response::send_response;
use crate::ipc::server::PeerAddr;

/// Narrow port the `provider_edit` handler uses to mutate the
/// catalog. Split into three methods so each IPC variant maps to
/// exactly one host call; the daemon-side impl owns the
/// `ProviderCatalog` interactions.
#[async_trait]
pub(crate) trait ProviderEditHost: Send + Sync {
    /// Merge `args` into the existing catalog entry identified by
    /// `args.id` and persist the result. Returns the updated
    /// catalog-summary view.
    async fn update_provider(&self, args: ProviderUpdateArgs) -> anyhow::Result<ProviderInfo>;

    /// Remove the provider with this id. Returns `true` when an entry
    /// was actually removed.
    async fn remove_provider(&self, id: &str) -> anyhow::Result<bool>;

    /// Promote `provider` (and optionally `model`) to the runtime
    /// default. Returns the provider id and the effective model id
    /// that was stored.
    async fn set_default_provider(
        &self,
        provider: &str,
        model: Option<String>,
    ) -> anyhow::Result<(String, String)>;
}

/// `provider_edit` domain request handler. Constructed with an
/// `Arc<dyn ProviderEditHost>` (typically `Arc::new(app_state.clone())`
/// from the dispatcher).
pub(crate) struct ProviderEditHandler {
    host: Arc<dyn ProviderEditHost>,
}

impl ProviderEditHandler {
    pub(crate) fn new(host: Arc<dyn ProviderEditHost>) -> Self {
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
            RequestPacket::ProviderUpdate { .. }
                | RequestPacket::ProviderRemove { .. }
                | RequestPacket::ProviderSetDefault { .. }
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
            RequestPacket::ProviderUpdate { request_id, args } => {
                if args.id.is_empty() {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: "provider id must not be empty".to_string(),
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                }

                match self.host.update_provider(args).await {
                    Ok(provider) => {
                        let response = ResponsePacket::ProviderUpdated {
                            request_id,
                            provider,
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
            RequestPacket::ProviderRemove { request_id, id } => {
                if id.is_empty() {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: "provider id must not be empty".to_string(),
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                }

                match self.host.remove_provider(&id).await {
                    Ok(removed) => {
                        let response = ResponsePacket::ProviderRemoved {
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
            RequestPacket::ProviderSetDefault {
                request_id,
                provider,
                model,
            } => {
                if provider.is_empty() {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: "provider id must not be empty".to_string(),
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                }

                match self.host.set_default_provider(&provider, model).await {
                    Ok((provider, model)) => {
                        let response = ResponsePacket::ProviderDefaultSet {
                            request_id,
                            provider,
                            model,
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
            // `matches()` returned true, so the exhaustive list above
            // covers every owned variant. This arm is unreachable.
            _ => unreachable!("ProviderEditHandler::matches allowed an unhandled variant"),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    //! Pin the wire shape for provider edit/remove/set-default so a
    //! runtime regression surfaces as a test failure rather than as the
    //! desktop's Settings panel silently failing.

    use super::*;
    use crate::ipc::response_sink::ResponseSink;
    use std::sync::{Arc, Mutex};

    struct StubHost {
        update_ok: Option<ProviderInfo>,
        remove_ok: Option<bool>,
        set_default_ok: Option<(String, String)>,
    }
    #[async_trait]
    impl ProviderEditHost for StubHost {
        async fn update_provider(&self, _args: ProviderUpdateArgs) -> anyhow::Result<ProviderInfo> {
            match &self.update_ok {
                Some(p) => Ok(p.clone()),
                None => anyhow::bail!("stub host: no update staged"),
            }
        }

        async fn remove_provider(&self, _id: &str) -> anyhow::Result<bool> {
            match self.remove_ok {
                Some(b) => Ok(b),
                None => anyhow::bail!("stub host: no remove staged"),
            }
        }

        async fn set_default_provider(
            &self,
            _provider: &str,
            _model: Option<String>,
        ) -> anyhow::Result<(String, String)> {
            match &self.set_default_ok {
                Some(t) => Ok(t.clone()),
                None => anyhow::bail!("stub host: no default staged"),
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

    fn stub_provider() -> ProviderInfo {
        ProviderInfo {
            id: "openai".to_string(),
            display_name: "OpenAI".to_string(),
            api_type: "openai".to_string(),
            base_url: "https://api.openai.com".to_string(),
            requires_key: true,
            is_local: false,
            enabled: true,
            models: vec![],
            default_model_id: "gpt-4o".to_string(),
            headers: Default::default(),
            is_default: false,
        }
    }

    #[tokio::test]
    async fn provider_update_empty_id_returns_error_response() {
        let host = StubHost {
            update_ok: None,
            remove_ok: None,
            set_default_ok: None,
        };
        let handler = ProviderEditHandler::new(Arc::new(host));
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sink = CaptureSink(buf.clone());

        handler
            .handle(
                RequestPacket::ProviderUpdate {
                    request_id: 81,
                    args: ProviderUpdateArgs {
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
    async fn provider_update_emits_provider_updated() {
        let host = StubHost {
            update_ok: Some(stub_provider()),
            remove_ok: None,
            set_default_ok: None,
        };
        let handler = ProviderEditHandler::new(Arc::new(host));
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sink = CaptureSink(buf.clone());

        handler
            .handle(
                RequestPacket::ProviderUpdate {
                    request_id: 82,
                    args: ProviderUpdateArgs {
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
            Some("provider_updated"),
            "successful update must produce a provider_updated packet"
        );
        assert_eq!(json.get("request_id").and_then(|v| v.as_u64()), Some(82));
        let provider = json.get("provider").expect("response should have provider");
        assert_eq!(provider.get("id").and_then(|v| v.as_str()), Some("openai"));
    }

    #[tokio::test]
    async fn provider_remove_emits_provider_removed() {
        let host = StubHost {
            update_ok: None,
            remove_ok: Some(true),
            set_default_ok: None,
        };
        let handler = ProviderEditHandler::new(Arc::new(host));
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sink = CaptureSink(buf.clone());

        handler
            .handle(
                RequestPacket::ProviderRemove {
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
            Some("provider_removed")
        );
        assert_eq!(json.get("request_id").and_then(|v| v.as_u64()), Some(83));
        assert_eq!(json.get("id").and_then(|v| v.as_str()), Some("openai"));
        assert_eq!(json.get("removed").and_then(|v| v.as_bool()), Some(true));
    }

    #[tokio::test]
    async fn provider_set_default_emits_provider_default_set() {
        let host = StubHost {
            update_ok: None,
            remove_ok: None,
            set_default_ok: Some(("openai".to_string(), "gpt-4o".to_string())),
        };
        let handler = ProviderEditHandler::new(Arc::new(host));
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sink = CaptureSink(buf.clone());

        handler
            .handle(
                RequestPacket::ProviderSetDefault {
                    request_id: 84,
                    provider: "openai".to_string(),
                    model: None,
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
            Some("provider_default_set")
        );
        assert_eq!(
            json.get("provider").and_then(|v| v.as_str()),
            Some("openai")
        );
        assert_eq!(json.get("model").and_then(|v| v.as_str()), Some("gpt-4o"));
    }
}
