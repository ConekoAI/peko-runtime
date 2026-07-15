//! `credential` domain request handler (T-107).
//!
//! Owns two IPC variants:
//!
//! - `CredentialList` — the desktop's `useCredentialList` hook calls
//!   this so Settings → Credentials can paint per-pill "Key set"
//!   indicators and the FirstRunWalkthrough can detect existing
//!   configuration. The CLI `peko credential list` path reads the
//!   vault directly and is unchanged; this handler is purely the IPC
//!   surface.
//! - `CredentialTest` — live pings the provider's API with the stored
//!   key and reports whether it was accepted. Powers
//!   `peko credential test` and the desktop's Test button. Replaces
//!   the shape-only `Vault::test_provider_key` check that couldn't
//!   tell `sk-opena-12345` from a real key.
//!
//! The handler holds two narrow port traits ([`CredentialHost`] for
//! the sync list and [`CredentialTestLiveHost`] for the async ping);
//! the daemon-side implementation (`AppState`) is reached only
//! through the traits, so this module never imports
//! `crate::daemon::state::AppState` directly.
//!
//! Boundary rules:
//! - Dependency inversion: the consumer (`ipc::handlers::credential`)
//!   defines both traits; the producer (`daemon::state`) implements
//!   them (same pattern as the rest of the F6/F7 handler family).
//! - F6: this module must not import any other `ipc::handlers::*` module.

use std::sync::Arc;

use async_trait::async_trait;

use crate::auth::caller::CallerContext;
use crate::ipc::handlers::RequestHandler;
use crate::ipc::packet::{CredentialRow, RequestPacket, ResponsePacket};
use crate::ipc::response_sink::ResponseSink;
use crate::ipc::send_response::send_response;
use crate::ipc::server::PeerAddr;

/// Narrow port the `credential` handler uses to reach the vault for
/// the read-only `CredentialList` variant.
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

/// Narrow port the `credential` handler uses to live-ping a
/// provider's API. Async because the underlying
/// [`crate::providers::validator::Validator::test`] makes a real
/// HTTP request and respects cancellation through the runtime.
#[async_trait]
pub(crate) trait CredentialTestLiveHost: Send + Sync {
    /// Live-ping `provider` with the stored key (or no key for local
    /// providers like Ollama) and report the structured outcome.
    /// Returns `Err` for configuration-level errors (unknown
    /// provider, no key when one is required); HTTP-level failures
    /// (401, 5xx, connection refused) come back as a
    /// [`CredentialTestOutcome`] with `ok = false`.
    async fn test_credential_live(
        &self,
        provider: &str,
    ) -> anyhow::Result<crate::providers::validator::CredentialTestOutcome>;
}

/// `credential` domain request handler. Constructed with one
/// `Arc<dyn CredentialHost>` for the read-only list variant and one
/// `Arc<dyn CredentialTestLiveHost>` for the async live ping. The
/// dispatcher passes two clones of the same `Arc<AppState>`; the
/// downcasts happen at construction time.
pub(crate) struct CredentialHandler {
    host: Arc<dyn CredentialHost>,
    test_host: Arc<dyn CredentialTestLiveHost>,
}

impl CredentialHandler {
    pub(crate) fn new(
        host: Arc<dyn CredentialHost>,
        test_host: Arc<dyn CredentialTestLiveHost>,
    ) -> Self {
        Self { host, test_host }
    }
}

#[async_trait]
impl RequestHandler for CredentialHandler {
    fn domain(&self) -> &'static str {
        "credential"
    }

    fn matches(&self, request: &RequestPacket) -> bool {
        matches!(
            request,
            RequestPacket::CredentialList { .. } | RequestPacket::CredentialTest { .. }
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
            RequestPacket::CredentialList { request_id } => {
                let providers = self.host.list_credentials();
                let response = ResponsePacket::CredentialsListed {
                    request_id,
                    providers,
                };
                send_response(sink, response).await?;
            }
            RequestPacket::CredentialTest {
                request_id,
                provider,
            } => {
                // The validator's "Err" path is reserved for
                // configuration errors (unknown provider, no key when
                // one is required). HTTP-level failures (401, 5xx,
                // connection refused) come back as a structured
                // outcome with `ok = false` — we surface those as a
                // successful `CredentialTested` so the caller can
                // render the latency and reason without mapping.
                let outcome = match self.test_host.test_credential_live(&provider).await {
                    Ok(o) => o,
                    Err(e) => crate::providers::validator::CredentialTestOutcome {
                        ok: false,
                        message: e.to_string(),
                        latency_ms: 0,
                        http_status: None,
                        model_used: None,
                    },
                };
                let response = ResponsePacket::CredentialTested {
                    request_id,
                    provider,
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

    /// Stub for the async live-test port. The default is a happy
    /// path (ok, model used) so the existing list-only tests keep
    /// compiling without per-test setup; the dedicated credential
    /// test case overrides it to exercise the failure path.
    struct StubTestHost {
        outcome: crate::providers::validator::CredentialTestOutcome,
    }
    #[async_trait]
    impl CredentialTestLiveHost for StubTestHost {
        async fn test_credential_live(
            &self,
            _provider: &str,
        ) -> anyhow::Result<crate::providers::validator::CredentialTestOutcome> {
            Ok(self.outcome.clone())
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
        let test_host = StubTestHost {
            outcome: crate::providers::validator::CredentialTestOutcome {
                ok: true,
                message: "unused".into(),
                latency_ms: 0,
                http_status: None,
                model_used: None,
            },
        };
        let handler = CredentialHandler::new(Arc::new(host), Arc::new(test_host));
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
        assert_eq!(
            minimax.get("provider").and_then(|v| v.as_str()),
            Some("minimax")
        );
        assert_eq!(minimax.get("has_key").and_then(|v| v.as_bool()), Some(true));
        assert!(minimax.get("last_tested").map_or(true, |v| v.is_null()));

        let openai = &providers[1];
        assert_eq!(
            openai.get("provider").and_then(|v| v.as_str()),
            Some("openai")
        );
        assert_eq!(openai.get("has_key").and_then(|v| v.as_bool()), Some(false));
    }

    #[tokio::test]
    async fn credential_list_emits_empty_array_when_vault_is_empty() {
        // Edge case: a fresh profile with no credentials at all must
        // emit an empty `providers` array (not omit the field, not
        // null) so the desktop's `useCredentialList` reduces to `[]`
        // without an undefined-property error.
        let host = StubHost(Vec::new());
        let test_host = StubTestHost {
            outcome: crate::providers::validator::CredentialTestOutcome {
                ok: true,
                message: "unused".into(),
                latency_ms: 0,
                http_status: None,
                model_used: None,
            },
        };
        let handler = CredentialHandler::new(Arc::new(host), Arc::new(test_host));
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

    #[tokio::test]
    async fn credential_test_emits_structured_outcome_with_latency_and_reason() {
        // Pins the wire shape of `CredentialTested` end-to-end through
        // the handler so the desktop's Tauri command can rely on
        // `ok`, `message`, `latency_ms`, `http_status`, `model_used`,
        // and `tested_at` all round-tripping.
        let host = StubHost(Vec::new());
        let test_host = StubTestHost {
            outcome: crate::providers::validator::CredentialTestOutcome {
                ok: false,
                message: "HTTP 401: invalid api key".to_string(),
                latency_ms: 187,
                http_status: Some(401),
                model_used: Some("claude-haiku-4-5".to_string()),
            },
        };
        let handler = CredentialHandler::new(Arc::new(host), Arc::new(test_host));
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sink = CaptureSink(buf.clone());

        handler
            .handle(
                RequestPacket::CredentialTest {
                    request_id: 42,
                    provider: "minimax".to_string(),
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
            Some("credential_tested")
        );
        assert_eq!(json.get("request_id").and_then(|v| v.as_u64()), Some(42));
        assert_eq!(
            json.get("provider").and_then(|v| v.as_str()),
            Some("minimax")
        );
        assert_eq!(json.get("ok").and_then(|v| v.as_bool()), Some(false));
        assert_eq!(
            json.get("message").and_then(|v| v.as_str()),
            Some("HTTP 401: invalid api key")
        );
        assert_eq!(json.get("latency_ms").and_then(|v| v.as_u64()), Some(187));
        assert_eq!(json.get("http_status").and_then(|v| v.as_u64()), Some(401));
        assert_eq!(
            json.get("model_used").and_then(|v| v.as_str()),
            Some("claude-haiku-4-5")
        );
        // `tested_at` is computed at response-build time; we just
        // assert it's a non-empty string here.
        assert!(
            json.get("tested_at")
                .and_then(|v| v.as_str())
                .map(|s| !s.is_empty())
                .unwrap_or(false),
            "tested_at should be a non-empty ISO-8601 string"
        );
    }

    #[tokio::test]
    async fn credential_test_maps_unknown_provider_error_to_structured_failure() {
        // When the AppState side returns `Err` (e.g. unknown
        // provider), the handler must translate it into a
        // `CredentialTested { ok: false, message: ... }` rather than
        // bubbling up a `ResponsePacket::Error` — the desktop's Test
        // button always reads the structured shape.
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct FailingTestHost(AtomicUsize);
        #[async_trait]
        impl CredentialTestLiveHost for FailingTestHost {
            async fn test_credential_live(
                &self,
                _provider: &str,
            ) -> anyhow::Result<crate::providers::validator::CredentialTestOutcome> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Err(anyhow::anyhow!("unknown provider: minimax"))
            }
        }

        let host = StubHost(Vec::new());
        let test_host = FailingTestHost(AtomicUsize::new(0));
        let handler = CredentialHandler::new(Arc::new(host), Arc::new(test_host));
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sink = CaptureSink(buf.clone());

        handler
            .handle(
                RequestPacket::CredentialTest {
                    request_id: 43,
                    provider: "minimax".to_string(),
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
            Some("credential_tested")
        );
        assert_eq!(json.get("ok").and_then(|v| v.as_bool()), Some(false));
        assert_eq!(json.get("http_status").and_then(|v| v.as_u64()), None);
        assert!(
            json.get("message")
                .and_then(|v| v.as_str())
                .map(|s| s.contains("unknown provider"))
                .unwrap_or(false),
            "message should carry the original error reason"
        );
    }
}
