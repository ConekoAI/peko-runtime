//! `provider_mcp` domain request handler (F6 step 12).
//!
//! Owns the small leftover misc IPC variants that don't fit cleanly
//! into another domain: `ProviderList`, `ProviderReload`,
//! `McpReload`. These power `peko providers list / reload` and
//! `peko mcp reload` — the live reload of the provider registry from
//! disk and the live reload of the MCP config from disk, both
//! followed by a fresh daemon-side re-read so the next request sees
//! the new state.
//!
//! The handler holds a narrow [`ProviderMcpHost`] port; the daemon-side
//! implementation (`AppState`) is reached only through the trait, so
//! this module never imports `crate::daemon::state::AppState`
//! directly.
//!
//! Boundary rules:
//! - Dependency inversion: the consumer (`ipc::handlers::provider_mcp`)
//!   defines the [`ProviderMcpHost`] trait; the producer
//!   (`daemon::state`) implements it (same pattern as the rest of the
//!   F6/F7 handler family).
//! - F6: this module must not import any other `ipc::handlers::*` module.
//!
//! `ProviderList` does not actually need any daemon state — it builds
//! a fresh `ProviderRegistry` from the on-disk provider configs every
//! call (matching the prior inlined behavior). The trait only carries
//! the two reload accessors used by `ProviderReload` / `McpReload`.

use std::sync::Arc;

use async_trait::async_trait;

use crate::auth::caller::CallerContext;
use crate::ipc::handlers::RequestHandler;
use crate::ipc::packet::{RequestPacket, ResponsePacket};
use crate::ipc::response_sink::ResponseSink;
use crate::ipc::send_response::send_response;
use crate::ipc::server::PeerAddr;

/// Narrow port the `provider_mcp` handler uses to reach daemon state.
///
/// `AppState` is the sole implementor. Both methods are async because
/// they drive live config-file reloads (provider registry + MCP
/// config); the trait needs `async_trait` for that reason.
#[async_trait::async_trait]
pub(crate) trait ProviderMcpHost: Send + Sync {
    /// Live reload the provider registry from disk, returning
    /// `(providers_count, keys_count)` on success. Powers
    /// `ProviderReload`.
    async fn reload_providers(&self) -> anyhow::Result<(usize, usize)>;

    /// Live reload the MCP config from disk, returning the count of
    /// configured MCP servers on success. Powers `McpReload`.
    async fn reload_mcp_config(&self) -> anyhow::Result<usize>;
}

/// `provider_mcp` domain request handler. Constructed with an
/// `Arc<dyn ProviderMcpHost>` (typically `Arc::new(app_state.clone())`
/// from the dispatcher).
pub(crate) struct ProviderMcpHandler {
    host: Arc<dyn ProviderMcpHost>,
}

impl ProviderMcpHandler {
    pub(crate) fn new(host: Arc<dyn ProviderMcpHost>) -> Self {
        Self { host }
    }
}

#[async_trait]
impl RequestHandler for ProviderMcpHandler {
    fn domain(&self) -> &'static str {
        "provider_mcp"
    }

    fn matches(&self, request: &RequestPacket) -> bool {
        matches!(
            request,
            RequestPacket::ProviderList { .. }
                | RequestPacket::ProviderReload { .. }
                | RequestPacket::McpReload { .. }
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
            RequestPacket::ProviderList { request_id } => {
                let registry = crate::providers::ProviderRegistry::new();
                let mut providers: Vec<crate::ipc::packet::ProviderInfo> = Vec::new();
                let mut seen_ids = std::collections::HashSet::new();
                for (_id, meta) in registry.iter() {
                    if !seen_ids.insert(meta.id) {
                        continue;
                    }
                    providers.push(crate::ipc::packet::ProviderInfo {
                        id: meta.id.to_string(),
                        display_name: meta.display_name.to_string(),
                        api_type: match meta.api_type {
                            crate::providers::registry::ApiType::OpenAICompletions => {
                                "openai".to_string()
                            }
                            crate::providers::registry::ApiType::AnthropicMessages => {
                                "anthropic".to_string()
                            }
                        },
                        default_model: meta.default_model.to_string(),
                        requires_key: !meta.api_key_env.is_empty(),
                        is_local: meta.api_key_env.is_empty(),
                    });
                }
                let response = ResponsePacket::ProviderList {
                    request_id,
                    providers,
                };
                send_response(sink, response).await?;
            }

            RequestPacket::ProviderReload { request_id } => {
                match self.host.reload_providers().await {
                    Ok((providers_count, keys_count)) => {
                        let response = ResponsePacket::ProviderReloaded {
                            request_id,
                            providers_count,
                            keys_count,
                        };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("provider reload failed: {e}"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            RequestPacket::McpReload { request_id } => {
                match self.host.reload_mcp_config().await {
                    Ok(servers_count) => {
                        let response = ResponsePacket::McpReloaded {
                            request_id,
                            servers_count,
                        };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("mcp reload failed: {e}"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            // `matches()` returned true, so the exhaustive list above
            // covers every owned variant. This arm is unreachable.
            _ => unreachable!("ProviderMcpHandler::matches allowed an unhandled variant"),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    //! Diagnostic for the desktop "settings page shows no provider configured"
    //! bug (T-105 follow-up): the desktop's fallback list fires when IPC
    //! returns empty. This test pins the wire shape the handler emits so
    //! upstream diagnostics can compare against what the runtime sends.

    use super::*;
    use crate::ipc::response_sink::ResponseSink;
    use async_trait::async_trait;
    use std::sync::{Arc, Mutex};

    /// Stub `ProviderMcpHost` — `ProviderList` does not need any host
    /// state (the handler builds a fresh registry), but the trait is
    /// required for construction.
    struct NoopHost;
    #[async_trait]
    impl ProviderMcpHost for NoopHost {
        async fn reload_providers(&self) -> anyhow::Result<(usize, usize)> {
            Ok((0, 0))
        }
        async fn reload_mcp_config(&self) -> anyhow::Result<usize> {
            Ok(0)
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

    #[tokio::test]
    async fn provider_list_emits_all_builtin_entries() {
        let handler = ProviderMcpHandler::new(Arc::new(NoopHost));
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sink = CaptureSink(buf.clone());

        handler
            .handle(
                RequestPacket::ProviderList { request_id: 7 },
                &test_caller(),
                &sink,
                &test_peer(),
            )
            .await
            .expect("handle should succeed");

        let bytes = buf.lock().unwrap().clone();
        let json: serde_json::Value =
            serde_json::from_slice(&bytes).expect("response should be valid JSON");

        let providers = json
            .get("providers")
            .and_then(|v| v.as_array())
            .expect("response should have a providers array");

        let ids: Vec<String> = providers
            .iter()
            .filter_map(|p| p.get("id").and_then(|v| v.as_str()).map(String::from))
            .collect();

        assert!(
            ids.iter().any(|id| id == "minimax"),
            "ProviderList should always include the minimax entry from \
             BUILT_IN_PROVIDERS; got: {ids:?}",
        );
        assert!(
            ids.contains(&"openai".to_string())
                && ids.contains(&"anthropic".to_string())
                && ids.contains(&"ollama".to_string()),
            "ProviderList should include the canonical built-ins; got: {ids:?}",
        );
        // Pin the response kind so future wire-shape changes surface
        // here rather than as a silent desktop regression.
        assert_eq!(
            json.get("type").and_then(|v| v.as_str()),
            Some("provider_list")
        );
        assert_eq!(json.get("request_id").and_then(|v| v.as_u64()), Some(7));
    }
}