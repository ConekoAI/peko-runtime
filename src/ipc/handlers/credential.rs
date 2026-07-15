//! `credential` domain request handler (T-107).
//!
//! Owns four IPC variants:
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
//! - `CredentialSet` — store or overwrite the API key for a provider
//!   in the vault. Powers the desktop's Settings → Add Key / Update
//!   Key flow; the CLI's `peko credential set` writes the vault
//!   directly without IPC.
//! - `CredentialDelete` — remove the stored key. Powers the desktop's
//!   Remove action; the CLI's `peko credential delete` writes the
//!   vault directly without IPC.
//!
//! The handler holds two narrow port traits ([`CredentialHost`] for
//! the sync read + set + delete and [`CredentialTestLiveHost`] for
//! the async ping); the daemon-side implementation (`AppState`) is
//! reached only through the traits, so this module never imports
//! `crate::daemon::state::AppState` directly.
//!
//! Boundary rules:
//! - Dependency inversion: the consumer (`ipc::handlers::credential`)
//!   defines both traits; the producer (`daemon::state`) implements
//!   them (same pattern as the rest of the F6/F7 handler family).
//! - F6: this module must not import any other `ipc::handlers::*` module.

use std::sync::Arc;

use async_trait::async_trait;
use secrecy::SecretString;

use crate::auth::caller::CallerContext;
use crate::ipc::handlers::RequestHandler;
use crate::ipc::packet::{CredentialRow, RequestPacket, ResponsePacket};
use crate::ipc::response_sink::ResponseSink;
use crate::ipc::send_response::send_response;
use crate::ipc::server::PeerAddr;

/// Narrow port the `credential` handler uses to reach the vault for
/// the list, set, and delete variants.
///
/// `AppState` is the sole implementor. Sync because
/// `Vault::list_providers`, `Vault::get_provider_key`,
/// `Vault::set_provider_key`, and `Vault::delete_provider_key` are
/// all in-memory (the latter two also do a synchronous `save()`)
/// against the already-loaded vault state — no async work is
/// required. Keeping the trait sync also avoids the `async_trait`
/// boxing overhead for what is essentially disk-flushed state.
pub(crate) trait CredentialHost: Send + Sync {
    /// Snapshot the credential vault into a list of rows the desktop
    /// can render directly. Each row carries the provider id, a
    /// `has_key` flag, and a `last_tested` timestamp (always `None`
    /// until the vault gains that field; see [`CredentialRow`]).
    fn list_credentials(&self) -> Vec<CredentialRow>;

    /// Store or overwrite the API key for `provider`. Empty
    /// `api_key` is rejected with an `Err` so the desktop can't
    /// accidentally wipe a key with a blank submit. Mirrors
    /// `Vault::set_provider_key`.
    fn set_credential(&self, provider: &str, api_key: &SecretString) -> anyhow::Result<()>;

    /// Remove the stored key for `provider`. Returns `Ok(true)` if a
    /// key was removed, `Ok(false)` if the vault had no entry to
    /// remove (idempotent — the desktop can re-issue on stale UI).
    /// Mirrors `Vault::delete_provider_key`.
    fn delete_credential(&self, provider: &str) -> anyhow::Result<bool>;
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
/// `Arc<dyn CredentialHost>` for the read/write variants and one
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
            RequestPacket::CredentialList { .. }
                | RequestPacket::CredentialTest { .. }
                | RequestPacket::CredentialSet { .. }
                | RequestPacket::CredentialDelete { .. }
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
            RequestPacket::CredentialSet {
                request_id,
                provider,
                api_key,
            } => {
                // Empty key submissions are rejected as a structured
                // `Error` so the desktop form can surface them inline
                // without a vault round-trip. Non-empty keys are
                // forwarded to `Vault::set_provider_key` which
                // encrypts and persists atomically.
                if api_key.is_empty() {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: format!("api_key for '{provider}' must not be empty"),
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                }
                let secret = SecretString::from(api_key);
                match self.host.set_credential(&provider, &secret) {
                    Ok(()) => {
                        let response = ResponsePacket::CredentialSetDone {
                            request_id,
                            provider,
                        };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("failed to store key for '{provider}': {e}"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }
            RequestPacket::CredentialDelete {
                request_id,
                provider,
            } => match self.host.delete_credential(&provider) {
                Ok(_) => {
                    let response = ResponsePacket::CredentialDeleted {
                        request_id,
                        provider,
                    };
                    send_response(sink, response).await?;
                }
                Err(e) => {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: format!("failed to delete key for '{provider}': {e}"),
                    };
                    send_response(sink, response).await?;
                }
            },
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
    use secrecy::ExposeSecret;
    use std::sync::{Arc, Mutex};

    /// Stub host — each test stages the rows it wants to exercise.
    /// Write-path methods are recorded into an `Arc<Mutex<_>>` so
    /// the set/delete tests can assert what the handler forwarded
    /// without standing up a real vault.
    struct StubHost {
        rows: Vec<CredentialRow>,
        writes: Arc<Mutex<Vec<(String, String)>>>,
        deletes: Arc<Mutex<Vec<String>>>,
        set_err: Option<String>,
        delete_err: Option<String>,
    }
    impl CredentialHost for StubHost {
        fn list_credentials(&self) -> Vec<CredentialRow> {
            self.rows.clone()
        }

        fn set_credential(
            &self,
            provider: &str,
            api_key: &SecretString,
        ) -> anyhow::Result<()> {
            if let Some(msg) = &self.set_err {
                return Err(anyhow::anyhow!("{msg}"));
            }
            self.writes
                .lock()
                .unwrap()
                .push((provider.to_string(), api_key.expose_secret().to_string()));
            Ok(())
        }

        fn delete_credential(&self, provider: &str) -> anyhow::Result<bool> {
            if let Some(msg) = &self.delete_err {
                return Err(anyhow::anyhow!("{msg}"));
            }
            self.deletes.lock().unwrap().push(provider.to_string());
            Ok(true)
        }
    }

    fn stub_host(rows: Vec<CredentialRow>) -> StubHost {
        StubHost {
            rows,
            writes: Arc::new(Mutex::new(Vec::new())),
            deletes: Arc::new(Mutex::new(Vec::new())),
            set_err: None,
            delete_err: None,
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
        let host = stub_host(vec![
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
        let host = stub_host(Vec::new());
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
        let host = stub_host(Vec::new());
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

        let host = stub_host(Vec::new());
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

    /// `CredentialSet` round-trips the api_key to the host and
    /// replies with `credential_set_done`. Pins the wire shape so a
    /// future change to the JSON envelope surfaces as a test
    /// failure rather than the desktop timing out (the symptom
    /// that motivated adding the variant — the original handler
    /// didn't claim it, so requests fell through to the dispatcher's
    /// generic "no handler registered" error and the desktop's
    /// `credential_set` mutation hung until the socket timeout).
    #[tokio::test]
    async fn credential_set_forwards_api_key_to_host_and_replies_with_done() {
        let host = stub_host(Vec::new());
        let writes = host.writes.clone();
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
                RequestPacket::CredentialSet {
                    request_id: 50,
                    provider: "minimax".to_string(),
                    api_key: "sk-test-123".to_string(),
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
            Some("credential_set_done"),
            "set success must emit credential_set_done"
        );
        assert_eq!(json.get("request_id").and_then(|v| v.as_u64()), Some(50));
        assert_eq!(
            json.get("provider").and_then(|v| v.as_str()),
            Some("minimax")
        );

        // Verify the host actually got the key (not empty / not
        // truncated).
        let writes = writes.lock().unwrap();
        assert_eq!(writes.len(), 1, "host should record exactly one write");
        assert_eq!(writes[0].0, "minimax");
        assert_eq!(writes[0].1, "sk-test-123");
    }

    /// Empty-key submissions are rejected as a structured
    /// `ResponsePacket::Error` rather than silently wiping the
    /// existing key. The desktop's Save button is guarded against
    /// this client-side, but the handler enforces it too as a
    /// defense-in-depth check.
    #[tokio::test]
    async fn credential_set_rejects_empty_key_with_error_response() {
        let host = stub_host(Vec::new());
        let writes = host.writes.clone();
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
                RequestPacket::CredentialSet {
                    request_id: 51,
                    provider: "minimax".to_string(),
                    api_key: String::new(),
                },
                &test_caller(),
                &sink,
                &test_peer(),
            )
            .await
            .expect("handle should succeed even on validation failure");

        let bytes = buf.lock().unwrap().clone();
        let json: serde_json::Value =
            serde_json::from_slice(&bytes).expect("response should be valid JSON");
        assert_eq!(
            json.get("type").and_then(|v| v.as_str()),
            Some("error"),
            "empty key must surface as ResponsePacket::Error"
        );
        assert!(
            json.get("message")
                .and_then(|v| v.as_str())
                .map(|s| s.contains("must not be empty"))
                .unwrap_or(false),
            "error message should explain the empty-key rule"
        );
        assert!(
            writes.lock().unwrap().is_empty(),
            "host must not be called for empty keys"
        );
    }

    /// Vault-side failures (e.g. encryption error) come back as a
    /// `ResponsePacket::Error` rather than bubbling the `Result::Err`
    /// out of `handle()` — the desktop always expects a structured
    /// response on the sink.
    #[tokio::test]
    async fn credential_set_maps_vault_failure_to_error_response() {
        let mut host = stub_host(Vec::new());
        host.set_err = Some("argon2id derivation failed".to_string());
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
                RequestPacket::CredentialSet {
                    request_id: 52,
                    provider: "minimax".to_string(),
                    api_key: "sk-ok".to_string(),
                },
                &test_caller(),
                &sink,
                &test_peer(),
            )
            .await
            .expect("handler must not propagate Err");

        let bytes = buf.lock().unwrap().clone();
        let json: serde_json::Value =
            serde_json::from_slice(&bytes).expect("response should be valid JSON");
        assert_eq!(json.get("type").and_then(|v| v.as_str()), Some("error"));
        assert!(
            json.get("message")
                .and_then(|v| v.as_str())
                .map(|s| s.contains("argon2id derivation failed"))
                .unwrap_or(false),
            "error message must carry the vault failure reason"
        );
    }

    /// `CredentialDelete` round-trips the provider id to the host
    /// and replies with `credential_deleted`. Mirrors the set test
    /// to pin both halves of the desktop's credentials write surface.
    #[tokio::test]
    async fn credential_delete_forwards_provider_to_host_and_replies_done() {
        let host = stub_host(Vec::new());
        let deletes = host.deletes.clone();
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
                RequestPacket::CredentialDelete {
                    request_id: 60,
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
            Some("credential_deleted"),
        );
        assert_eq!(json.get("request_id").and_then(|v| v.as_u64()), Some(60));
        assert_eq!(
            json.get("provider").and_then(|v| v.as_str()),
            Some("minimax")
        );
        assert_eq!(*deletes.lock().unwrap(), vec!["minimax".to_string()]);
    }
}
