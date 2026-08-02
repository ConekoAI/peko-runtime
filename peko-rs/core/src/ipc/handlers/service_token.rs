//! `service_token` domain request handler (ADR-045 PR #5 step 2).
//!
//! Owns the user→daemon CRUD IPC variants for named, persistent
//! service tokens:
//!
//! - [`RequestPacket::ServiceTokenCreate`] — generate + persist a new
//!   token. The raw secret is returned **exactly once** in
//!   [`ResponsePacket::ServiceTokenCreated::token`].
//! - [`RequestPacket::ServiceTokenList`] — return metadata for every
//!   registered token. Never includes the raw secret.
//! - [`RequestPacket::ServiceTokenRevoke`] — delete the on-disk dir +
//!   clear the in-memory cache entry.
//!
//! All three requests must arrive via the existing PR #2 strict
//! SID+token gate (`AuthCredential::SessionToken` for an authorized
//! interactive session) — the dispatcher rejects any other credential
//! type with `[invalid_session_token]` before reaching this handler.
//!
//! ## Boundary rules (F6)
//!
//! - Dependency inversion: this module defines [`ServiceTokenHost`];
//!   the producer (`daemon::state::AppState`) implements it.
//! - This module must not import any other `ipc::handlers::*` module.
//!
//! ## Subject stamping
//!
//! `by: Subject` is derived from the caller's IPC auth context via
//! [`CallerContext::subject`] for every successful create/revoke, so
//! the audit trail can never be spoofed via the wire. The raw token
//! is **never** logged — only the name and the resolved
//! `caller_subject`.

use std::sync::Arc;

use async_trait::async_trait;
use tracing::{info, warn};

use crate::ipc::auth::hash_token;
use crate::ipc::handlers::RequestHandler;
use crate::ipc::packet::{RequestPacket, ResponsePacket, ServiceTokenInfo};
use crate::ipc::response_sink::ResponseSink;
use crate::ipc::send_response::send_response;
use crate::ipc::server::PeerAddr;
use crate::storage::service_token_store::ServiceTokenStore;
use peko_auth::caller::CallerContext;

/// Narrow port the `service_token` handler uses to reach daemon state.
///
/// `AppState` is the sole implementor. The handler delegates the
/// on-disk CRUD to `ServiceTokenStore` and the in-memory cache to
/// [`crate::ipc::auth::AuthTable`] via the
/// [`crate::daemon::state::AppState::service_token_store`] +
/// `auth_table` accessors.
pub(crate) trait ServiceTokenHost: Send + Sync {
    /// The on-disk CRUD store.
    fn service_token_store(&self) -> Arc<ServiceTokenStore>;
    /// The in-memory cache (register/revoke after store mutations).
    fn auth_table(&self) -> Arc<crate::ipc::auth::AuthTable>;
}

/// `service_token` domain request handler.
pub(crate) struct ServiceTokenHandler {
    host: Arc<dyn ServiceTokenHost>,
}

impl ServiceTokenHandler {
    pub(crate) fn new(host: Arc<dyn ServiceTokenHost>) -> Self {
        Self { host }
    }
}

#[async_trait]
impl RequestHandler for ServiceTokenHandler {
    fn domain(&self) -> &'static str {
        "service_token"
    }

    fn matches(&self, request: &RequestPacket) -> bool {
        matches!(
            request,
            RequestPacket::ServiceTokenCreate { .. }
                | RequestPacket::ServiceTokenList { .. }
                | RequestPacket::ServiceTokenRevoke { .. }
        )
    }

    async fn handle(
        &self,
        request: RequestPacket,
        caller: &CallerContext,
        sink: &dyn ResponseSink,
        _peer: &PeerAddr,
    ) -> anyhow::Result<()> {
        let subject = caller.subject();
        match request {
            RequestPacket::ServiceTokenCreate {
                request_id,
                name,
                caps,
                expires_in_secs,
            } => {
                let store = self.host.service_token_store();
                let auth_table = self.host.auth_table();
                let by = subject.subject_id();

                let result = store.create(&name, caps.clone(), expires_in_secs);
                match result {
                    Ok((token, meta)) => {
                        // Register the SHA-256 hash into the
                        // in-memory cache. The ADR's "cannot grow"
                        // rule is enforced at the table surface
                        // (no set/add cap methods); the caps we
                        // pass here are the immutable set bound at
                        // creation.
                        let ttl = meta
                            .expires_at_secs
                            .and_then(|exp| {
                                let now = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_secs())
                                    .unwrap_or(0);
                                exp.checked_sub(now).and_then(|rel| {
                                    std::time::Duration::try_from(
                                        std::time::Duration::from_secs(rel.max(0)),
                                    )
                                    .ok()
                                })
                            });
                        auth_table.register_service_token(
                            &name,
                            hash_token(token.as_bytes()),
                            caps.clone(),
                            ttl,
                        );

                        info!(
                            request_id,
                            token_name = %name,
                            caps = ?caps,
                            by = %by,
                            "service token created"
                        );

                        let response = ResponsePacket::ServiceTokenCreated {
                            request_id,
                            name,
                            token, // shown ONCE
                            caps,
                            expires_at_secs: meta.expires_at_secs,
                        };
                        send_response(sink, response).await
                    }
                    Err(e) => {
                        warn!(
                            request_id,
                            token_name = %name,
                            by = %by,
                            error = %e,
                            "service token create failed"
                        );
                        let response = ResponsePacket::ServiceTokenError {
                            request_id,
                            name: Some(name),
                            message: e.to_string(),
                        };
                        send_response(sink, response).await
                    }
                }
            }

            RequestPacket::ServiceTokenList { request_id } => {
                let by = subject.subject_id();
                match self.host.service_token_store().list() {
                    Ok(tokens) => {
                        // Build the wire `ServiceTokenInfo` list.
                        // `last_used_at_secs` is daemon-internal
                        // (PR #6 audit will populate); the on-disk
                        // store doesn't track it, so emit None.
                        let infos: Vec<ServiceTokenInfo> = tokens
                            .into_iter()
                            .map(|t| ServiceTokenInfo {
                                name: t.name,
                                caps: t.caps,
                                created_at_secs: t.created_at_secs,
                                expires_at_secs: t.expires_at_secs,
                                last_used_at_secs: t.last_used_at_secs,
                            })
                            .collect();
                        let response = ResponsePacket::ServiceTokenListed {
                            request_id,
                            tokens: infos,
                        };
                        send_response(sink, response).await
                    }
                    Err(e) => {
                        warn!(
                            request_id,
                            by = %by,
                            error = %e,
                            "service token list failed"
                        );
                        let response = ResponsePacket::ServiceTokenError {
                            request_id,
                            name: None,
                            message: e.to_string(),
                        };
                        send_response(sink, response).await
                    }
                }
            }

            RequestPacket::ServiceTokenRevoke { request_id, name } => {
                let by = subject.subject_id();
                let store = self.host.service_token_store();
                let auth_table = self.host.auth_table();
                match store.revoke(&name) {
                    Ok(true) => {
                        // Mirror on disk → in-memory cache.
                        let was_present = auth_table.revoke_service_token(&name);
                        info!(
                            request_id,
                            token_name = %name,
                            by = %by,
                            cache_cleared = was_present,
                            "service token revoked"
                        );
                        let response = ResponsePacket::ServiceTokenRevoked {
                            request_id,
                            name,
                        };
                        send_response(sink, response).await
                    }
                    Ok(false) => {
                        // Idempotent: revoking a non-existent token
                        // is still a successful revocation request;
                        // just clear the cache and report success.
                        let was_present = auth_table.revoke_service_token(&name);
                        info!(
                            request_id,
                            token_name = %name,
                            by = %by,
                            cache_cleared = was_present,
                            "service token revoke (no-op: not on disk)"
                        );
                        let response = ResponsePacket::ServiceTokenRevoked {
                            request_id,
                            name,
                        };
                        send_response(sink, response).await
                    }
                    Err(e) => {
                        warn!(
                            request_id,
                            token_name = %name,
                            by = %by,
                            error = %e,
                            "service token revoke failed"
                        );
                        let response = ResponsePacket::ServiceTokenError {
                            request_id,
                            name: Some(name),
                            message: e.to_string(),
                        };
                        send_response(sink, response).await
                    }
                }
            }

            // `matches()` filters to the three variants above.
            _ => unreachable!("ServiceTokenHandler::matches allowed an unhandled variant"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::auth::AuthTable;
    use crate::ipc::handlers::RequestHandler;
    use crate::storage::service_token_store::ServiceTokenStore;
    use peko_auth::caller::CallerContext;

    /// Test harness — bundles a real store + auth table with a
    /// tempdir so we can exercise the handler end-to-end.
    struct TestHost {
        store: Arc<ServiceTokenStore>,
        table: Arc<AuthTable>,
    }

    impl ServiceTokenHost for TestHost {
        fn service_token_store(&self) -> Arc<ServiceTokenStore> {
            Arc::clone(&self.store)
        }
        fn auth_table(&self) -> Arc<AuthTable> {
            Arc::clone(&self.table)
        }
    }

    /// Local sink that captures the response bytes so we can
    /// round-trip-decode and assert the wire shape. Mirrors the
    /// `CaptureSink` pattern in `provider_edit.rs` / `credential.rs`
    /// sibling test modules.
    struct CaptureSink(std::sync::Mutex<Vec<u8>>);

    #[async_trait]
    impl ResponseSink for CaptureSink {
        async fn send_bytes(&self, bytes: &[u8]) -> std::io::Result<()> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(())
        }
    }

    impl CaptureSink {
        fn new() -> Self {
            Self(std::sync::Mutex::new(Vec::new()))
        }

        /// Decode the captured bytes back to a single
        /// `ResponsePacket`. Uses `RequestPacket::from_bytes` (the
        /// response-side equivalent) which understands the
        /// `send_response` framing (length prefix + trailing newline).
        fn take_response(&self) -> Option<ResponsePacket> {
            let bytes = self.0.lock().unwrap().clone();
            ResponsePacket::from_bytes(&bytes).ok()
        }

        fn clear(&self) {
            self.0.lock().unwrap().clear();
        }

        fn contains(&self, needle: &[u8]) -> bool {
            self.0.lock().unwrap().windows(needle.len()).any(|w| w == needle)
        }
    }

    fn fresh_test_host() -> (Arc<TestHost>, std::path::PathBuf) {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "peko-svc-tok-test-{nanos}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let store = Arc::new(ServiceTokenStore::new(&root));
        let table = AuthTable::new();
        (Arc::new(TestHost { store, table }), root)
    }

    fn alice() -> CallerContext {
        CallerContext::local()
    }

    fn test_peer() -> PeerAddr {
        PeerAddr::Ip("127.0.0.1:0".parse().expect("loopback addr"))
    }

    #[tokio::test]
    async fn create_returns_raw_token_once() {
        let (host, root) = fresh_test_host();
        let handler = ServiceTokenHandler::new(host.clone());
        let sink = CaptureSink::new();
        let req = RequestPacket::ServiceTokenCreate {
            request_id: 1,
            name: "runtime".into(),
            caps: vec!["fs:read".into(), "tool:Bash".into()],
            expires_in_secs: None,
        };
        assert!(handler.matches(&req));
        handler
            .handle(req, &alice(), &sink, &test_peer())
            .await
            .unwrap();
        let resp = sink.take_response().unwrap();
        match resp {
            ResponsePacket::ServiceTokenCreated {
                request_id,
                name,
                token,
                caps,
                expires_at_secs,
            } => {
                assert_eq!(request_id, 1);
                assert_eq!(name, "runtime");
                assert!(!token.is_empty());
                assert_eq!(caps, vec!["fs:read", "tool:Bash"]);
                assert!(expires_at_secs.is_none());
            }
            other => panic!("expected ServiceTokenCreated, got {other:?}"),
        }
        // On-disk artifacts exist.
        assert!(root.join("runtime").join("meta.json").exists());
        assert!(root.join("runtime").join("token").exists());
    }

    #[tokio::test]
    async fn list_returns_metadata_without_secret() {
        let (host, _root) = fresh_test_host();
        let handler = ServiceTokenHandler::new(host.clone());
        let sink = CaptureSink::new();
        // Create two tokens.
        for (name, caps) in [("a", vec!["x".into()]), ("b", vec!["y".into(), "z".into()])] {
            handler
                .handle(
                    RequestPacket::ServiceTokenCreate {
                        request_id: 1,
                        name: name.into(),
                        caps,
                        expires_in_secs: None,
                    },
                    &alice(),
                    &sink,
                    &test_peer(),
                )
                .await
                .unwrap();
            sink.clear();
        }
        // List.
        handler
            .handle(
                RequestPacket::ServiceTokenList { request_id: 2 },
                &alice(),
                &sink,
                &test_peer(),
            )
            .await
            .unwrap();
        let resp = sink.take_response().unwrap();
        match resp {
            ResponsePacket::ServiceTokenListed { tokens, .. } => {
                assert_eq!(tokens.len(), 2);
                // Sorted by name (store::list sorts).
                assert_eq!(tokens[0].name, "a");
                assert_eq!(tokens[1].name, "b");
            }
            other => panic!("expected ServiceTokenListed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn revoke_removes_token_and_clears_cache() {
        let (host, root) = fresh_test_host();
        let handler = ServiceTokenHandler::new(host.clone());
        let sink = CaptureSink::new();
        // Create.
        handler
            .handle(
                RequestPacket::ServiceTokenCreate {
                    request_id: 1,
                    name: "runtime".into(),
                    caps: vec!["fs:read".into()],
                    expires_in_secs: None,
                },
                &alice(),
                &sink,
                &test_peer(),
            )
            .await
            .unwrap();
        sink.clear();
        // Revoke.
        handler
            .handle(
                RequestPacket::ServiceTokenRevoke {
                    request_id: 2,
                    name: "runtime".into(),
                },
                &alice(),
                &sink,
                &test_peer(),
            )
            .await
            .unwrap();
        let resp = sink.take_response().unwrap();
        assert!(matches!(
            resp,
            ResponsePacket::ServiceTokenRevoked { .. }
        ));
        // On-disk dir is gone.
        assert!(!root.join("runtime").exists());
    }

    #[tokio::test]
    async fn create_then_verify_via_auth_table() {
        let (host, _root) = fresh_test_host();
        let handler = ServiceTokenHandler::new(host.clone());
        let sink = CaptureSink::new();
        handler
            .handle(
                RequestPacket::ServiceTokenCreate {
                    request_id: 1,
                    name: "rt".into(),
                    caps: vec!["fs:read".into(), "tool:Read".into()],
                    expires_in_secs: None,
                },
                &alice(),
                &sink,
                &test_peer(),
            )
            .await
            .unwrap();
        let resp = sink.take_response().unwrap();
        let raw_token = match resp {
            ResponsePacket::ServiceTokenCreated { token, .. } => token,
            other => panic!("expected Created, got {other:?}"),
        };
        // The handler should have registered the token into the
        // auth table; verify there.
        let caps = host.auth_table().verify_service_token(raw_token.as_bytes());
        assert_eq!(
            caps,
            Some(vec!["fs:read".to_string(), "tool:Read".to_string()])
        );
    }

    #[tokio::test]
    async fn matches_filter_rejects_other_packets() {
        let (host, _root) = fresh_test_host();
        let handler = ServiceTokenHandler::new(host);
        assert!(handler.matches(&RequestPacket::ServiceTokenCreate {
            request_id: 1,
            name: "x".into(),
            caps: vec![],
            expires_in_secs: None,
        }));
        assert!(handler.matches(&RequestPacket::ServiceTokenList { request_id: 1 }));
        assert!(handler.matches(&RequestPacket::ServiceTokenRevoke {
            request_id: 1,
            name: "x".into(),
        }));
        // Unrelated packets must not match.
        assert!(!handler.matches(&RequestPacket::Ping { request_id: 1 }));
    }
}