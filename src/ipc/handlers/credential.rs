//! `credential` domain request handler (T-107).
//!
//! Owns the `CredentialList` IPC variant — the desktop's
//! `useCredentialList` hook calls this so Settings → Credentials can
//! paint per-pill "Key set" indicators and the FirstRunWalkthrough
//! can detect existing configuration. The CLI `peko credential list`
//! path reads the vault directly and is unchanged; this handler is
//! purely the IPC surface.
//!
//! The handler holds a narrow [`CredentialHost`] port; the daemon-side
//! implementation (`AppState`) is reached only through the trait, so
//! this module never imports `crate::daemon::state::AppState`
//! directly.
//!
//! Boundary rules:
//! - Dependency inversion: the consumer (`ipc::handlers::credential`)
//!   defines the [`CredentialHost`] trait; the producer
//!   (`daemon::state`) implements it (same pattern as the rest of
//!   the F6/F7 handler family).
//! - F6: this module must not import any other `ipc::handlers::*` module.

use std::sync::Arc;

use async_trait::async_trait;

use crate::auth::caller::CallerContext;
use crate::ipc::handlers::RequestHandler;
use crate::ipc::packet::{CredentialRow, RequestPacket, ResponsePacket};
use crate::ipc::response_sink::ResponseSink;
use crate::ipc::send_response::send_response;
use crate::ipc::server::PeerAddr;

/// Narrow port the `credential` handler uses to reach the vault.
///
/// `AppState` is the sole implementor. Sync because
/// `Vault::list_providers` and `Vault::get_provider_key` are both
/// in-memory lookups against the already-loaded vault state — no
/// I/O is performed and no async work is required. Keeping the
/// trait sync also avoids the `async_trait` boxing overhead for a
/// pure-read surface.
pub(crate) trait CredentialHost: Send + Sync {
    /// Snapshot the credential vault into a list of rows the desktop
    /// can render directly. Each row carries the provider id, a
    /// `has_key` flag, and a `last_tested` timestamp (always `None`
    /// until the vault gains that field; see [`CredentialRow`]).
    fn list_credentials(&self) -> Vec<CredentialRow>;
}

/// `credential` domain request handler. Constructed with an
/// `Arc<dyn CredentialHost>` (typically `Arc::new(app_state.clone())`
/// from the dispatcher).
pub(crate) struct CredentialHandler {
    host: Arc<dyn CredentialHost>,
}

impl CredentialHandler {
    pub(crate) fn new(host: Arc<dyn CredentialHost>) -> Self {
        Self { host }
    }
}

#[async_trait]
impl RequestHandler for CredentialHandler {
    fn domain(&self) -> &'static str {
        "credential"
    }

    fn matches(&self, request: &RequestPacket) -> bool {
        matches!(request, RequestPacket::CredentialList { .. })
    }

    async fn handle(
        &self,
        request: RequestPacket,
        _caller: &CallerContext,
        sink: &dyn ResponseSink,
        _peer: &PeerAddr,
    ) -> anyhow::Result<()> {
        match request {
            RequestPacket::CredentialList { request_id } => {
                let providers = self.host.list_credentials();
                let response = ResponsePacket::CredentialsListed { request_id, providers };
                send_response(sink, response).await?;
            }
            // `matches()` returned true, so the exhaustive list above
            // covers every owned variant. This arm is unreachable.
            _ => unreachable!("CredentialHandler::matches allowed an unhandled variant"),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    //! Pin the wire shape the desktop consumes so a runtime regression
    //! surfaces as a test failure rather than as the desktop falling
    //! back to "no providers configured" (T-107). Mirrors the
    //! `provider_list_emits_all_builtin_entries` pattern that pinned
    //! `ProviderList` (PR #187).

    use super::*;
    use crate::ipc::response_sink::ResponseSink;
    use std::sync::{Arc, Mutex};

    /// Stub host — each test stages the rows it wants to exercise.
    struct StubHost(Vec<CredentialRow>);
    impl CredentialHost for StubHost {
        fn list_credentials(&self) -> Vec<CredentialRow> {
            self.0.clone()
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
    async fn credential_list_emits_rows_with_has_key_flag() {
        let host = StubHost(vec![
            CredentialRow {
                provider: "minimax".to_string(),
                has_key: true,
                last_tested: None,
            },
            CredentialRow {
                provider: "openai".to_string(),
                has_key: false,
                last_tested: None,
            },
        ]);
        let handler = CredentialHandler::new(Arc::new(host));
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sink = CaptureSink(buf.clone());

        handler
            .handle(
                RequestPacket::CredentialList { request_id: 7 },
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
            Some("credentials_listed"),
            "response kind must be credentials_listed (T-107 wire shape)"
        );
        assert_eq!(json.get("request_id").and_then(|v| v.as_u64()), Some(7));

        let providers = json
            .get("providers")
            .and_then(|v| v.as_array())
            .expect("response should have a providers array");
        assert_eq!(providers.len(), 2);

        // Field names must match the desktop's CredentialRow exactly —
        // `src-tauri/src/commands/settings.rs:287` reads `provider` /
        // `has_key` / `last_tested` and we pin those here.
        let minimax = &providers[0];
        assert_eq!(minimax.get("provider").and_then(|v| v.as_str()), Some("minimax"));
        assert_eq!(minimax.get("has_key").and_then(|v| v.as_bool()), Some(true));
        assert!(minimax.get("last_tested").map_or(true, |v| v.is_null()));

        let openai = &providers[1];
        assert_eq!(openai.get("provider").and_then(|v| v.as_str()), Some("openai"));
        assert_eq!(openai.get("has_key").and_then(|v| v.as_bool()), Some(false));
    }

    #[tokio::test]
    async fn credential_list_emits_empty_array_when_vault_is_empty() {
        // Edge case: a fresh profile with no credentials at all must
        // emit an empty `providers` array (not omit the field, not
        // null) so the desktop's `useCredentialList` reduces to `[]`
        // without an undefined-property error.
        let host = StubHost(Vec::new());
        let handler = CredentialHandler::new(Arc::new(host));
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sink = CaptureSink(buf.clone());

        handler
            .handle(
                RequestPacket::CredentialList { request_id: 8 },
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
            .expect("response should have a providers array (possibly empty)");
        assert!(providers.is_empty());
    }
}