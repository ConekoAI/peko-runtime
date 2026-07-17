//! `credential` domain request handler (T-107 + RP3A).
//!
//! Owns the credential + rotation-binding IPC surface. The consumer-side
//! port traits ([`CredentialHost`] and [`BindingHost`]) live here; the
//! daemon-side implementation in `AppState` reaches them through the
//! narrow ports.
//!
//! Boundary rules:
//! - Dependency inversion: the consumer (`ipc::handlers::credential`)
//!   defines all traits; the producer (`daemon::state`) implements them
//!   (same pattern as the rest of the F6/F7 handler family).
//! - F6: this module must not import any other `ipc::handlers::*` module.

use std::sync::Arc;

use async_trait::async_trait;
use secrecy::{ExposeSecret, SecretString};

use crate::auth::caller::CallerContext;
use crate::common::vault::{CredentialKind, RotationStrategy};
use crate::ipc::handlers::RequestHandler;
use crate::ipc::packet::{
    Credential as CredentialWire, CredentialRow, RequestPacket, ResponsePacket, RotationBindingWire,
};
use crate::ipc::response_sink::ResponseSink;
use crate::ipc::send_response::send_response;
use crate::ipc::server::PeerAddr;

/// Narrow port for the read/write credential variants.
pub(crate) trait CredentialHost: Send + Sync {
    /// Snapshot the credential vault into redacted rows. Optional
    /// `namespace` and `kind` filters restrict the listing.
    fn list_credentials(
        &self,
        namespace: Option<&str>,
        kind: Option<CredentialKind>,
        include_system: bool,
    ) -> Vec<CredentialRow>;

    /// Fetch the full (non-material) record for one credential.
    fn get_credential(&self, id: &str) -> Option<CredentialWire>;

    /// Store or overwrite a credential at `(namespace, name)`. The host
    /// generates a fresh id and returns it. Empty `material` is rejected
    /// one layer up in the handler.
    fn set_credential(
        &self,
        namespace: &str,
        name: &str,
        kind: CredentialKind,
        material: &SecretString,
        metadata: Option<serde_json::Value>,
    ) -> anyhow::Result<String>;

    /// Fetch the secret material for one credential. This is the only
    /// CredentialHost method that exposes the secret; the handler is
    /// expected to audit-log the reveal before returning it over IPC.
    fn get_credential_material(&self, id: &str) -> Option<SecretString>;

    /// Remove the credential with this `id`. Returns `Ok(true)` if a
    /// credential was removed (idempotent delete).
    fn delete_credential(&self, id: &str) -> anyhow::Result<bool>;
}

/// Narrow port for the rotation-binding variants.
pub(crate) trait BindingHost: Send + Sync {
    /// Enumerate every configured rotation binding.
    fn list_bindings(&self) -> Vec<RotationBindingWire>;

    /// Fetch one binding by slot key, if it exists.
    fn get_binding(&self, key: &str) -> Option<RotationBindingWire>;

    /// Store or overwrite the rotation binding for a slot.
    fn set_binding(
        &self,
        key: &str,
        strategy: RotationStrategy,
        order: Vec<String>,
    ) -> anyhow::Result<()>;

    /// Remove a binding by slot key. Returns `Ok(true)` if a binding
    /// was removed.
    fn delete_binding(&self, key: &str) -> anyhow::Result<bool>;
}

/// `credential` + `binding` domain request handler. Constructed with one
/// `Arc<dyn CredentialHost>` and one `Arc<dyn BindingHost>`.
pub(crate) struct CredentialHandler {
    host: Arc<dyn CredentialHost>,
    binding_host: Arc<dyn BindingHost>,
}

impl CredentialHandler {
    pub(crate) fn new(host: Arc<dyn CredentialHost>, binding_host: Arc<dyn BindingHost>) -> Self {
        Self { host, binding_host }
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
                | RequestPacket::CredentialGet { .. }
                | RequestPacket::CredentialGetMaterial { .. }
                | RequestPacket::CredentialSet { .. }
                | RequestPacket::CredentialDelete { .. }
                | RequestPacket::BindingList { .. }
                | RequestPacket::BindingGet { .. }
                | RequestPacket::BindingSet { .. }
                | RequestPacket::BindingDelete { .. }
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
            RequestPacket::CredentialList {
                request_id,
                namespace,
                kind,
                include_system,
            } => {
                let kind = match kind {
                    Some(k) => match parse_kind(&k) {
                        Some(parsed) => Some(parsed),
                        None => {
                            let response = ResponsePacket::Error {
                                request_id,
                                message: format!("unknown credential kind: {k}"),
                            };
                            send_response(sink, response).await?;
                            return Ok(());
                        }
                    },
                    None => None,
                };
                let providers = self.host.list_credentials(
                    namespace.as_deref(),
                    kind,
                    include_system.unwrap_or(false),
                );
                let response = ResponsePacket::CredentialsListed {
                    request_id,
                    providers,
                };
                send_response(sink, response).await?;
            }
            RequestPacket::CredentialGet { request_id, id } => {
                match self.host.get_credential(&id) {
                    Some(credential) => {
                        let response = ResponsePacket::CredentialGot {
                            request_id,
                            credential,
                        };
                        send_response(sink, response).await?;
                    }
                    None => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("credential not found: {id}"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }
            RequestPacket::CredentialGetMaterial {
                request_id,
                id,
                reason,
            } => {
                // Audit-log the reveal attempt at INFO. The material is
                // intentionally not reachable through `CredentialHost`;
                // the daemon implementation routes this directly to the
                // vault and returns the secret string.
                tracing::info!(credential_id = %id, reason = %reason, "credential material revealed via IPC");
                match self.host.get_credential_material(&id) {
                    Some(secret) => {
                        let response = ResponsePacket::CredentialMaterial {
                            request_id,
                            id,
                            material: secret.expose_secret().to_string(),
                        };
                        send_response(sink, response).await?;
                    }
                    None => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("credential not found: {id}"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }
            RequestPacket::CredentialSet {
                request_id,
                namespace,
                name,
                kind,
                material,
                metadata,
            } => {
                if material.is_empty() {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: format!("material for '{namespace}/{name}' must not be empty"),
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                }
                let kind = match parse_kind(&kind) {
                    Some(k) => k,
                    None => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("unknown credential kind: {kind}"),
                        };
                        send_response(sink, response).await?;
                        return Ok(());
                    }
                };
                let secret = SecretString::from(material);
                match self
                    .host
                    .set_credential(&namespace, &name, kind, &secret, metadata)
                {
                    Ok(id) => {
                        let response = ResponsePacket::CredentialSetDone { request_id, id };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!(
                                "failed to store credential '{namespace}/{name}': {e}"
                            ),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }
            RequestPacket::CredentialDelete { request_id, id } => {
                match self.host.delete_credential(&id) {
                    Ok(_) => {
                        let response = ResponsePacket::CredentialDeleted { request_id, id };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("failed to delete credential '{id}': {e}"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }
            RequestPacket::BindingList { request_id } => {
                let bindings = self.binding_host.list_bindings();
                let response = ResponsePacket::BindingsListed {
                    request_id,
                    bindings,
                };
                send_response(sink, response).await?;
            }
            RequestPacket::BindingGet { request_id, key } => {
                let mut bindings = Vec::new();
                if let Some(b) = self.binding_host.get_binding(&key) {
                    bindings.push(b);
                }
                let response = ResponsePacket::BindingsListed {
                    request_id,
                    bindings,
                };
                send_response(sink, response).await?;
            }
            RequestPacket::BindingSet {
                request_id,
                key,
                strategy,
                order,
            } => {
                let strategy = match parse_strategy(&strategy) {
                    Some(s) => s,
                    None => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("unknown rotation strategy: {strategy}"),
                        };
                        send_response(sink, response).await?;
                        return Ok(());
                    }
                };
                match self.binding_host.set_binding(&key, strategy, order) {
                    Ok(()) => {
                        let response = ResponsePacket::BindingSetDone { request_id, key };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("failed to store binding '{key}': {e}"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }
            RequestPacket::BindingDelete { request_id, key } => {
                match self.binding_host.delete_binding(&key) {
                    Ok(_) => {
                        let response = ResponsePacket::BindingDeleted { request_id, key };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("failed to delete binding '{key}': {e}"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }
            // `matches()` returned true, so the exhaustive list above
            // covers every owned variant. This arm is unreachable.
            _ => unreachable!("CredentialHandler::matches allowed an unhandled variant"),
        }
        Ok(())
    }
}

fn parse_kind(s: &str) -> Option<CredentialKind> {
    match s {
        "api_key" => Some(CredentialKind::ApiKey),
        "bearer_token" => Some(CredentialKind::BearerToken),
        "oauth_token" => Some(CredentialKind::OAuthToken),
        "basic_auth" => Some(CredentialKind::BasicAuth),
        "private_key" => Some(CredentialKind::PrivateKey),
        "generic_secret" => Some(CredentialKind::GenericSecret),
        _ => None,
    }
}

fn parse_strategy(s: &str) -> Option<RotationStrategy> {
    match s {
        "round_robin" => Some(RotationStrategy::RoundRobin),
        "last_resort" => Some(RotationStrategy::LastResort),
        "random" => Some(RotationStrategy::Random),
        _ => None,
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
    use crate::ipc::server::PeerAddr;
    use secrecy::ExposeSecret;
    use std::sync::{Arc, Mutex};

    struct StubHost {
        rows: Vec<CredentialRow>,
        writes: Arc<Mutex<Vec<(String, String, String, String)>>>,
        deletes: Arc<Mutex<Vec<String>>>,
        set_err: Option<String>,
        delete_err: Option<String>,
    }
    impl CredentialHost for StubHost {
        fn list_credentials(
            &self,
            _namespace: Option<&str>,
            _kind: Option<CredentialKind>,
            _include_system: bool,
        ) -> Vec<CredentialRow> {
            self.rows.clone()
        }

        fn get_credential(&self, _id: &str) -> Option<CredentialWire> {
            None
        }

        fn get_credential_material(&self, _id: &str) -> Option<SecretString> {
            None
        }

        fn set_credential(
            &self,
            namespace: &str,
            name: &str,
            kind: CredentialKind,
            material: &SecretString,
            _metadata: Option<serde_json::Value>,
        ) -> anyhow::Result<String> {
            if let Some(msg) = &self.set_err {
                return Err(anyhow::anyhow!("{msg}"));
            }
            self.writes.lock().unwrap().push((
                namespace.to_string(),
                name.to_string(),
                kind.as_str().to_string(),
                material.expose_secret().to_string(),
            ));
            Ok("id-stub-123".to_string())
        }

        fn delete_credential(&self, id: &str) -> anyhow::Result<bool> {
            if let Some(msg) = &self.delete_err {
                return Err(anyhow::anyhow!("{msg}"));
            }
            self.deletes.lock().unwrap().push(id.to_string());
            Ok(true)
        }
    }

    struct StubBindingHost;
    impl BindingHost for StubBindingHost {
        fn list_bindings(&self) -> Vec<RotationBindingWire> {
            Vec::new()
        }
        fn get_binding(&self, _key: &str) -> Option<RotationBindingWire> {
            None
        }
        fn set_binding(
            &self,
            _key: &str,
            _strategy: RotationStrategy,
            _order: Vec<String>,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        fn delete_binding(&self, _key: &str) -> anyhow::Result<bool> {
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

    fn handler(host: StubHost) -> CredentialHandler {
        CredentialHandler::new(Arc::new(host), Arc::new(StubBindingHost))
    }

    #[tokio::test]
    async fn credential_list_emits_rows_with_has_key_flag() {
        let host = stub_host(vec![
            CredentialRow {
                id: "id-minimax".to_string(),
                namespace: "provider:minimax".to_string(),
                name: "default".to_string(),
                kind: "api_key".to_string(),
                has_key: true,
                last_tested_at: None,
                last_tested_ok: None,
                system_owned: false,
            },
            CredentialRow {
                id: "id-openai".to_string(),
                namespace: "provider:openai".to_string(),
                name: "default".to_string(),
                kind: "api_key".to_string(),
                has_key: false,
                last_tested_at: None,
                last_tested_ok: None,
                system_owned: false,
            },
        ]);
        let handler = handler(host);
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sink = CaptureSink(buf.clone());

        handler
            .handle(
                RequestPacket::CredentialList {
                    request_id: 7,
                    namespace: None,
                    kind: None,
                    include_system: None,
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
            Some("credentials_listed"),
            "response kind must be credentials_listed (RP3A wire shape)"
        );
        assert_eq!(json.get("request_id").and_then(|v| v.as_u64()), Some(7));

        let providers = json
            .get("providers")
            .and_then(|v| v.as_array())
            .expect("response should have a providers array");
        assert_eq!(providers.len(), 2);

        // Field names must match the desktop's CredentialRow exactly.
        let minimax = &providers[0];
        assert_eq!(
            minimax.get("id").and_then(|v| v.as_str()),
            Some("id-minimax")
        );
        assert_eq!(
            minimax.get("namespace").and_then(|v| v.as_str()),
            Some("provider:minimax")
        );
        assert_eq!(
            minimax.get("name").and_then(|v| v.as_str()),
            Some("default")
        );
        assert_eq!(
            minimax.get("kind").and_then(|v| v.as_str()),
            Some("api_key")
        );
        assert_eq!(minimax.get("has_key").and_then(|v| v.as_bool()), Some(true));
        assert!(minimax.get("last_tested_at").map_or(true, |v| v.is_null()));
        assert!(minimax.get("last_tested_ok").is_none());

        let openai = &providers[1];
        assert_eq!(openai.get("id").and_then(|v| v.as_str()), Some("id-openai"));
        assert_eq!(openai.get("has_key").and_then(|v| v.as_bool()), Some(false));
    }

    #[tokio::test]
    async fn credential_list_emits_empty_array_when_vault_is_empty() {
        let host = stub_host(Vec::new());
        let handler = handler(host);
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sink = CaptureSink(buf.clone());

        handler
            .handle(
                RequestPacket::CredentialList {
                    request_id: 8,
                    namespace: None,
                    kind: None,
                    include_system: None,
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

        let providers = json
            .get("providers")
            .and_then(|v| v.as_array())
            .expect("response should have a providers array (possibly empty)");
        assert!(providers.is_empty());
    }

    #[tokio::test]
    async fn credential_set_forwards_api_key_to_host_and_replies_with_done() {
        let host = stub_host(Vec::new());
        let writes = host.writes.clone();
        let handler = handler(host);
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sink = CaptureSink(buf.clone());

        handler
            .handle(
                RequestPacket::CredentialSet {
                    request_id: 50,
                    namespace: "provider:minimax".to_string(),
                    name: "default".to_string(),
                    kind: "api_key".to_string(),
                    material: "sk-test-123".to_string(),
                    metadata: None,
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
        assert_eq!(json.get("id").and_then(|v| v.as_str()), Some("id-stub-123"));

        let writes = writes.lock().unwrap();
        assert_eq!(writes.len(), 1, "host should record exactly one write");
        assert_eq!(writes[0].0, "provider:minimax");
        assert_eq!(writes[0].1, "default");
        assert_eq!(writes[0].2, "api_key");
        assert_eq!(writes[0].3, "sk-test-123");
    }

    #[tokio::test]
    async fn credential_set_rejects_empty_key_with_error_response() {
        let host = stub_host(Vec::new());
        let writes = host.writes.clone();
        let handler = handler(host);
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sink = CaptureSink(buf.clone());

        handler
            .handle(
                RequestPacket::CredentialSet {
                    request_id: 51,
                    namespace: "provider:minimax".to_string(),
                    name: "default".to_string(),
                    kind: "api_key".to_string(),
                    material: String::new(),
                    metadata: None,
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

    #[tokio::test]
    async fn credential_set_maps_vault_failure_to_error_response() {
        let mut host = stub_host(Vec::new());
        host.set_err = Some("argon2id derivation failed".to_string());
        let handler = handler(host);
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sink = CaptureSink(buf.clone());

        handler
            .handle(
                RequestPacket::CredentialSet {
                    request_id: 52,
                    namespace: "provider:minimax".to_string(),
                    name: "default".to_string(),
                    kind: "api_key".to_string(),
                    material: "sk-ok".to_string(),
                    metadata: None,
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

    #[tokio::test]
    async fn credential_delete_forwards_id_to_host_and_replies_done() {
        let host = stub_host(Vec::new());
        let deletes = host.deletes.clone();
        let handler = handler(host);
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sink = CaptureSink(buf.clone());

        handler
            .handle(
                RequestPacket::CredentialDelete {
                    request_id: 60,
                    id: "id-minimax".to_string(),
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
        assert_eq!(json.get("id").and_then(|v| v.as_str()), Some("id-minimax"));
        assert_eq!(*deletes.lock().unwrap(), vec!["id-minimax".to_string()]);
    }

    #[tokio::test]
    async fn credential_list_include_system_forwards_flag() {
        struct FlagHost {
            flag: Arc<Mutex<Option<bool>>>,
        }
        impl CredentialHost for FlagHost {
            fn list_credentials(
                &self,
                _namespace: Option<&str>,
                _kind: Option<CredentialKind>,
                include_system: bool,
            ) -> Vec<CredentialRow> {
                *self.flag.lock().unwrap() = Some(include_system);
                Vec::new()
            }
            fn get_credential(&self, _id: &str) -> Option<CredentialWire> {
                None
            }
            fn get_credential_material(&self, _id: &str) -> Option<SecretString> {
                None
            }
            fn set_credential(
                &self,
                _namespace: &str,
                _name: &str,
                _kind: CredentialKind,
                _material: &SecretString,
                _metadata: Option<serde_json::Value>,
            ) -> anyhow::Result<String> {
                Ok("id-stub".to_string())
            }
            fn delete_credential(&self, _id: &str) -> anyhow::Result<bool> {
                Ok(true)
            }
        }

        let flag = Arc::new(Mutex::new(None));
        let handler = CredentialHandler::new(
            Arc::new(FlagHost { flag: flag.clone() }),
            Arc::new(StubBindingHost),
        );
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sink = CaptureSink(buf.clone());

        handler
            .handle(
                RequestPacket::CredentialList {
                    request_id: 9,
                    namespace: None,
                    kind: None,
                    include_system: Some(true),
                },
                &test_caller(),
                &sink,
                &test_peer(),
            )
            .await
            .expect("handle should succeed");

        assert_eq!(*flag.lock().unwrap(), Some(true));
    }
}
