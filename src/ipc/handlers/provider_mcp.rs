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
/// `AppState` is the sole implementor. All three methods are async
/// because they drive live config-file reads and reloads (provider
/// catalog + MCP config); the trait needs `async_trait` for that
/// reason.
#[async_trait::async_trait]
pub(crate) trait ProviderMcpHost: Send + Sync {
    /// Live reload the provider registry from disk, returning
    /// `(providers_count, keys_count)` on success. Powers
    /// `ProviderReload`.
    async fn reload_providers(&self) -> anyhow::Result<(usize, usize)>;

    /// Live reload the MCP config from disk, returning the count of
    /// configured MCP servers on success. Powers `McpReload`.
    async fn reload_mcp_config(&self) -> anyhow::Result<usize>;

    /// Snapshot every catalog entry (enabled + disabled) as the
    /// `ProviderInfo` wire shape. Powers `ProviderList`. Reads go
    /// through the daemon's `Arc<ProviderCatalog>` so the response
    /// matches what the resolver sees — including any user-added
    /// entries that don't appear in the static `BUILT_IN_TEMPLATES`.
    async fn list_catalog_providers(&self) -> Vec<crate::ipc::packet::ProviderInfo>;
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
                // Read the catalog through the host so the response
                // reflects every entry the user has added via
                // `peko provider add` — including disabled entries.
                // Pre-RP1 this handler walked the in-memory
                // `BUILT_IN_PROVIDERS` list, which silently omitted
                // any catalog-only provider. The bug surfaced as
                // `peko provider list` showing entries that
                // disappeared from `ProviderList` IPC.
                let providers = self.host.list_catalog_providers().await;
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
    //!
    //! Post-RP1 the handler reads from a stub catalog (via
    //! `list_catalog_providers`) rather than rebuilding the in-memory
    //! registry. The stub here stands in for `AppState`'s catalog
    //! projection.

    use super::*;
    use crate::ipc::response_sink::ResponseSink;
    use async_trait::async_trait;
    use std::sync::{Arc, Mutex};

    /// Stub host backed by an in-memory list of `ProviderInfo`s.
    struct StubHost(Vec<crate::ipc::packet::ProviderInfo>);
    #[async_trait]
    impl ProviderMcpHost for StubHost {
        async fn reload_providers(&self) -> anyhow::Result<(usize, usize)> {
            Ok((self.0.len(), 0))
        }
        async fn reload_mcp_config(&self) -> anyhow::Result<usize> {
            Ok(0)
        }
        async fn list_catalog_providers(&self) -> Vec<crate::ipc::packet::ProviderInfo> {
            self.0.clone()
        }
    }

    fn anthropic_info() -> crate::ipc::packet::ProviderInfo {
        crate::ipc::packet::ProviderInfo {
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
        }
    }

    fn ollama_info() -> crate::ipc::packet::ProviderInfo {
        crate::ipc::packet::ProviderInfo {
            id: "ollama".to_string(),
            display_name: "Ollama".to_string(),
            api_type: "openai".to_string(),
            base_url: "http://localhost:11434/v1".to_string(),
            requires_key: false,
            is_local: true,
            enabled: true,
            models: vec![],
            default_model_id: "llama3.1".to_string(),
            headers: Default::default(),
        }
    }

    /// Disables-flavor entry to verify enabled=false flows through.
    fn disabled_info() -> crate::ipc::packet::ProviderInfo {
        crate::ipc::packet::ProviderInfo {
            id: "minimax".to_string(),
            display_name: "MiniMax (disabled)".to_string(),
            api_type: "anthropic".to_string(),
            base_url: "https://api.minimaxi.com/anthropic".to_string(),
            requires_key: true,
            is_local: false,
            enabled: false,
            models: vec![],
            default_model_id: "MiniMax-M3".to_string(),
            headers: Default::default(),
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
    async fn provider_list_emits_catalog_entries() {
        // Pre-RP1 this test asserted that BUILT_IN_PROVIDERS flowed
        // through. Post-RP1 the handler reads the catalog via the
        // host, so we stage the equivalent rows here and assert the
        // wire shape — including `api_format`, `base_url`, `enabled`,
        // `default_model_id`, and `models`.
        let host = StubHost(vec![
            anthropic_info(),
            ollama_info(),
            disabled_info(),
        ]);
        let handler = ProviderMcpHandler::new(Arc::new(host));
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

        // Pin the response kind so future wire-shape changes surface
        // here rather than as a silent desktop regression.
        assert_eq!(
            json.get("type").and_then(|v| v.as_str()),
            Some("provider_list")
        );
        assert_eq!(json.get("request_id").and_then(|v| v.as_u64()), Some(7));

        let providers = json
            .get("providers")
            .and_then(|v| v.as_array())
            .expect("response should have a providers array");
        assert_eq!(providers.len(), 3, "all three staged rows must flow");

        // The wire shape carries the new fields. Spot-check the first
        // row — a future field addition surfaces as a test diff here
        // rather than as a silent desktop regression.
        let anthropic = &providers[0];
        assert_eq!(
            anthropic.get("id").and_then(|v| v.as_str()),
            Some("anthropic")
        );
        assert_eq!(
            anthropic.get("api_format").and_then(|v| v.as_str()),
            Some("anthropic")
        );
        assert_eq!(
            anthropic.get("base_url").and_then(|v| v.as_str()),
            Some("https://api.anthropic.com")
        );
        assert_eq!(
            anthropic.get("requires_key").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            anthropic.get("is_local").and_then(|v| v.as_bool()),
            Some(false)
        );
        assert_eq!(
            anthropic.get("enabled").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            anthropic.get("default_model_id").and_then(|v| v.as_str()),
            Some("claude-sonnet-4-5")
        );
        assert!(
            anthropic.get("models").and_then(|v| v.as_array()).is_some(),
            "models[] must be present (even when empty)"
        );

        // The disabled row must round-trip `enabled = false`.
        let disabled = &providers[2];
        assert_eq!(
            disabled.get("id").and_then(|v| v.as_str()),
            Some("minimax")
        );
        assert_eq!(
            disabled.get("enabled").and_then(|v| v.as_bool()),
            Some(false)
        );
    }

    /// Regression for the original bug: a user-added catalog entry
    /// (one not in `BUILT_IN_TEMPLATES`) must round-trip through the
    /// `ProviderList` IPC. Pre-RP1 the handler rebuilt a fresh
    /// `ProviderRegistry` from `BUILT_IN_PROVIDERS` and silently
    /// dropped user additions.
    #[tokio::test]
    async fn provider_list_emits_user_added_providers() {
        let custom = crate::ipc::packet::ProviderInfo {
            id: "my-internal-llm".to_string(),
            display_name: "Internal LLM".to_string(),
            api_type: "openai".to_string(),
            base_url: "http://internal-llm.internal/v1".to_string(),
            requires_key: true,
            is_local: false,
            enabled: true,
            models: vec![],
            default_model_id: "custom-model".to_string(),
            headers: Default::default(),
        };
        let host = StubHost(vec![custom.clone()]);
        let handler = ProviderMcpHandler::new(Arc::new(host));
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sink = CaptureSink(buf.clone());

        handler
            .handle(
                RequestPacket::ProviderList { request_id: 9 },
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
            ids.contains(&"my-internal-llm".to_string()),
            "user-added catalog entries must flow through ProviderList IPC, got: {ids:?}"
        );
    }
}