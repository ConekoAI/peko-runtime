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

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use secrecy::{ExposeSecret, SecretString};

use crate::common::vault::{CredentialKind, RotationStrategy};
use crate::ipc::handlers::RequestHandler;
use crate::ipc::packet::{
    Credential as CredentialWire, CredentialRow, ModelSummary, RequestPacket, ResponsePacket,
    RotationBindingWire,
};
use crate::ipc::response_sink::ResponseSink;
use crate::ipc::send_response::send_response;
use crate::ipc::server::PeerAddr;
use peko_auth::caller::CallerContext;

/// Outcome of a credential delete. PR 3 / `feature/model-first-config`
/// splits the result so the IPC handler can emit a structured error
/// (with a dependents list) when the credential is in use, vs. the
/// normal removed/broken tuple on a successful delete.
#[derive(Debug)]
pub(crate) enum CredentialDeleteOutcome {
    /// Delete succeeded. `broken_references` is the count of catalog
    /// entries that pointed at the deleted credential and were
    /// detached (`credential_id = null`) before the delete — zero on
    /// a normal delete, non-zero when `force = true` and dependents
    /// existed.
    Removed {
        broken_references: u32,
    },
    /// Credential is referenced by one or more configured models and
    /// `force` was `false`. The handler emits `ResponsePacket::Error`
    /// with a "credential_in_use" message naming each dependent so the
    /// CLI / desktop can prompt for confirmation.
    InUse {
        dependents: Vec<ModelSummary>,
    },
}

/// Narrow port for the read/write credential variants.
#[async_trait]
pub(crate) trait CredentialHost: Send + Sync {
    /// Snapshot the credential vault into redacted rows. Optional
    /// `namespace` and `kind` filters restrict the listing.
    /// `is_referenced` / `referenced_by` on each row are populated
    /// from the model catalog by the daemon implementation; clients
    /// that don't have a catalog (none today, but tests stub them
    /// out) leave the fields default.
    fn list_credentials(
        &self,
        namespace: Option<&str>,
        kind: Option<CredentialKind>,
        include_system: bool,
    ) -> Vec<CredentialRow>;

    /// Fetch the full (non-material) record for one credential.
    fn get_credential(&self, id: &str) -> Option<CredentialWire>;

    /// Store or overwrite a credential at `(namespace, name)`. The
    /// host generates a fresh id and returns it. Empty `material` is
    /// rejected one layer up in the handler.
    ///
    /// PR 3: when `replace_on` is `Some(old_id)`, the host rewrites
    /// every catalog entry that pointed at `old_id` to point at the
    /// newly-stored id and returns the rewired count as the second
    /// tuple element. Pass `None` for the normal set flow.
    async fn set_credential(
        &self,
        namespace: &str,
        name: &str,
        kind: CredentialKind,
        material: &SecretString,
        metadata: Option<serde_json::Value>,
        replace_on: Option<&str>,
    ) -> anyhow::Result<(String, u32)>;

    /// Fetch the secret material for one credential. This is the only
    /// CredentialHost method that exposes the secret; the handler is
    /// expected to audit-log the reveal before returning it over IPC.
    fn get_credential_material(&self, id: &str) -> Option<SecretString>;

    /// Remove the credential with this `id`.
    ///
    /// When `force` is `false` (the default), the host refuses to
    /// delete a credential that any catalog entry still references
    /// and returns `Ok(CredentialDeleteOutcome::InUse { dependents })`
    /// so the handler can emit a structured "credential_in_use"
    /// error. When `force` is `true`, dependents are detached first
    /// and the response carries the broken count. Vault-level errors
    /// (lock contention, corrupt file, …) propagate as `Err`.
    async fn delete_credential(
        &self,
        id: &str,
        force: bool,
    ) -> anyhow::Result<CredentialDeleteOutcome>;

    /// PR 3: batch lookup for the catalog → credential join. Returns
    /// a map from credential id to the list of `ModelSummary` rows
    /// that reference it. Used by the list handler to populate
    /// `CredentialRow::is_referenced` / `referenced_by` without an
    /// N+1 round-trip per credential.
    async fn credential_references(&self) -> HashMap<String, Vec<ModelSummary>>;
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
                let mut references = self.host.credential_references().await;
                let mut providers = self.host.list_credentials(
                    namespace.as_deref(),
                    kind,
                    include_system.unwrap_or(false),
                );
                // PR 3 / `feature/model-first-config`: attach
                // `is_referenced` / `referenced_by` per row so the
                // desktop can paint dependents badges inline.
                for row in &mut providers {
                    let dependents = references.remove(&row.id).unwrap_or_default();
                    row.is_referenced = !dependents.is_empty();
                    row.referenced_by = dependents;
                }
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
                replace_on,
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
                    .set_credential(
                        &namespace,
                        &name,
                        kind,
                        &secret,
                        metadata,
                        replace_on.as_deref(),
                    )
                    .await
                {
                    Ok((id, rewired_models)) => {
                        if let Some(old_id) = replace_on.as_deref() {
                            tracing::info!(
                                new_credential_id = %id,
                                previous_credential_id = %old_id,
                                rewired_models,
                                "credential set with bulk replace_on"
                            );
                        }
                        let response = ResponsePacket::CredentialSetDone {
                            request_id,
                            id,
                            rewired_models,
                        };
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
            RequestPacket::CredentialDelete {
                request_id,
                id,
                force,
            } => match self.host.delete_credential(&id, force).await {
                Ok(CredentialDeleteOutcome::Removed { broken_references }) => {
                    if broken_references > 0 {
                        tracing::warn!(
                            credential_id = %id,
                            broken_references,
                            "credential force-deleted; detached dependent model(s)"
                        );
                    }
                    let response = ResponsePacket::CredentialDeleted {
                        request_id,
                        id,
                        broken_references,
                    };
                    send_response(sink, response).await?;
                }
                Ok(CredentialDeleteOutcome::InUse { dependents }) => {
                    let names = dependents
                        .iter()
                        .map(|m| m.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    let message = format!(
                        "credential '{id}' is referenced by {} model(s): {names}. \
                         Pass --force to break references (audit-logged), or \
                         use `peko credential set <new> --replace-on {id}` to swap them first.",
                        dependents.len(),
                    );
                    let response = ResponsePacket::Error { request_id, message };
                    send_response(sink, response).await?;
                }
                Err(e) => {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: format!("failed to delete credential '{id}': {e}"),
                    };
                    send_response(sink, response).await?;
                }
            },
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
        writes: Arc<Mutex<Vec<(String, String, String, String, String)>>>,
        deletes: Arc<Mutex<Vec<(String, bool)>>>,
        set_err: Option<String>,
        delete_err: Option<String>,
    }
    #[async_trait]
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

        async fn set_credential(
            &self,
            namespace: &str,
            name: &str,
            kind: CredentialKind,
            material: &SecretString,
            _metadata: Option<serde_json::Value>,
            replace_on: Option<&str>,
        ) -> anyhow::Result<(String, u32)> {
            if let Some(msg) = &self.set_err {
                return Err(anyhow::anyhow!("{msg}"));
            }
            self.writes.lock().unwrap().push((
                namespace.to_string(),
                name.to_string(),
                kind.as_str().to_string(),
                material.expose_secret().to_string(),
                replace_on.unwrap_or("").to_string(),
            ));
            Ok(("id-stub-123".to_string(), 0))
        }

        async fn delete_credential(
            &self,
            id: &str,
            force: bool,
        ) -> anyhow::Result<CredentialDeleteOutcome> {
            if let Some(msg) = &self.delete_err {
                return Err(anyhow::anyhow!("{msg}"));
            }
            self.deletes.lock().unwrap().push((id.to_string(), force));
            Ok(CredentialDeleteOutcome::Removed {
                broken_references: 0,
            })
        }

        async fn credential_references(&self) -> HashMap<String, Vec<ModelSummary>> {
            HashMap::new()
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
                is_referenced: false,
                referenced_by: Vec::new(),
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
                is_referenced: false,
                referenced_by: Vec::new(),
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
                    replace_on: None,
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
        assert_eq!(json.get("rewired_models").and_then(|v| v.as_u64()), Some(0));

        let writes = writes.lock().unwrap();
        assert_eq!(writes.len(), 1, "host should record exactly one write");
        assert_eq!(writes[0].0, "provider:minimax");
        assert_eq!(writes[0].1, "default");
        assert_eq!(writes[0].2, "api_key");
        assert_eq!(writes[0].3, "sk-test-123");
        assert_eq!(writes[0].4, "", "no replace_on supplied");
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
                    replace_on: None,
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
                    replace_on: None,
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
                    force: false,
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
        assert_eq!(json.get("broken_references").and_then(|v| v.as_u64()), Some(0));
        assert_eq!(
            *deletes.lock().unwrap(),
            vec![("id-minimax".to_string(), false)]
        );
    }

    #[tokio::test]
    async fn credential_list_include_system_forwards_flag() {
        struct FlagHost {
            flag: Arc<Mutex<Option<bool>>>,
        }
        #[async_trait]
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
            async fn set_credential(
                &self,
                _namespace: &str,
                _name: &str,
                _kind: CredentialKind,
                _material: &SecretString,
                _metadata: Option<serde_json::Value>,
                _replace_on: Option<&str>,
            ) -> anyhow::Result<(String, u32)> {
                Ok(("id-stub".to_string(), 0))
            }
            async fn delete_credential(
                &self,
                _id: &str,
                _force: bool,
            ) -> anyhow::Result<CredentialDeleteOutcome> {
                Ok(CredentialDeleteOutcome::Removed {
                    broken_references: 0,
                })
            }
            async fn credential_references(&self) -> HashMap<String, Vec<ModelSummary>> {
                HashMap::new()
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

    /// PR 3: when the host refuses a delete because dependents exist
    /// and `force = false`, the handler emits a structured Error
    /// message that names each dependent. CLI consumers parse the
    /// "credential '…' is referenced by N model(s): …" prefix to
    /// surface the dependents list with exit code 3.
    #[tokio::test]
    async fn credential_delete_in_use_emits_dependent_named_error() {
        struct InUseHost;
        #[async_trait]
        impl CredentialHost for InUseHost {
            fn list_credentials(
                &self,
                _namespace: Option<&str>,
                _kind: Option<CredentialKind>,
                _include_system: bool,
            ) -> Vec<CredentialRow> {
                Vec::new()
            }
            fn get_credential(&self, _id: &str) -> Option<CredentialWire> {
                None
            }
            fn get_credential_material(&self, _id: &str) -> Option<SecretString> {
                None
            }
            async fn set_credential(
                &self,
                _namespace: &str,
                _name: &str,
                _kind: CredentialKind,
                _material: &SecretString,
                _metadata: Option<serde_json::Value>,
                _replace_on: Option<&str>,
            ) -> anyhow::Result<(String, u32)> {
                Ok(("id-stub".to_string(), 0))
            }
            async fn delete_credential(
                &self,
                _id: &str,
                _force: bool,
            ) -> anyhow::Result<CredentialDeleteOutcome> {
                Ok(CredentialDeleteOutcome::InUse {
                    dependents: vec![ModelSummary {
                        id: "anthropic-sonnet".to_string(),
                        display_name: "Claude Sonnet 4.5".to_string(),
                        template_id: Some("anthropic".to_string()),
                        api_type: "anthropic".to_string(),
                        base_url: "https://api.anthropic.com".to_string(),
                        model_id: "claude-sonnet-4-5".to_string(),
                        context_window: Some(200_000),
                        max_output_tokens: Some(64_000),
                        headers: Default::default(),
                        credential_id: Some("cred-1".to_string()),
                        requires_key: true,
                        is_local: false,
                        enabled: true,
                    }],
                })
            }
            async fn credential_references(&self) -> HashMap<String, Vec<ModelSummary>> {
                HashMap::new()
            }
        }
        let handler = CredentialHandler::new(
            Arc::new(InUseHost),
            Arc::new(StubBindingHost),
        );
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sink = CaptureSink(buf.clone());

        handler
            .handle(
                RequestPacket::CredentialDelete {
                    request_id: 61,
                    id: "cred-1".to_string(),
                    force: false,
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
        assert_eq!(json.get("type").and_then(|v| v.as_str()), Some("error"));
        let msg = json
            .get("message")
            .and_then(|v| v.as_str())
            .expect("error message should be a string");
        assert!(
            msg.contains("is referenced by 1 model(s)"),
            "expected dependents count in message, got: {msg}"
        );
        assert!(
            msg.contains("anthropic-sonnet"),
            "expected dependent id in message, got: {msg}"
        );
        assert!(
            msg.contains("--force"),
            "expected --force pointer in message, got: {msg}"
        );
        assert!(
            msg.contains("--replace-on"),
            "expected --replace-on pointer in message, got: {msg}"
        );
    }

    /// PR 3: when `force = true` and dependents exist, the host
    /// returns `Removed { broken_references: N }` and the handler
    /// echoes the count back on the success response.
    #[tokio::test]
    async fn credential_delete_force_reports_broken_references() {
        struct ForceHost {
            force_seen: Arc<Mutex<bool>>,
        }
        #[async_trait]
        impl CredentialHost for ForceHost {
            fn list_credentials(
                &self,
                _namespace: Option<&str>,
                _kind: Option<CredentialKind>,
                _include_system: bool,
            ) -> Vec<CredentialRow> {
                Vec::new()
            }
            fn get_credential(&self, _id: &str) -> Option<CredentialWire> {
                None
            }
            fn get_credential_material(&self, _id: &str) -> Option<SecretString> {
                None
            }
            async fn set_credential(
                &self,
                _namespace: &str,
                _name: &str,
                _kind: CredentialKind,
                _material: &SecretString,
                _metadata: Option<serde_json::Value>,
                _replace_on: Option<&str>,
            ) -> anyhow::Result<(String, u32)> {
                Ok(("id-stub".to_string(), 0))
            }
            async fn delete_credential(
                &self,
                _id: &str,
                force: bool,
            ) -> anyhow::Result<CredentialDeleteOutcome> {
                *self.force_seen.lock().unwrap() = force;
                Ok(CredentialDeleteOutcome::Removed {
                    broken_references: 3,
                })
            }
            async fn credential_references(&self) -> HashMap<String, Vec<ModelSummary>> {
                HashMap::new()
            }
        }
        let force_seen = Arc::new(Mutex::new(false));
        let handler = CredentialHandler::new(
            Arc::new(ForceHost {
                force_seen: force_seen.clone(),
            }),
            Arc::new(StubBindingHost),
        );
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sink = CaptureSink(buf.clone());

        handler
            .handle(
                RequestPacket::CredentialDelete {
                    request_id: 62,
                    id: "cred-x".to_string(),
                    force: true,
                },
                &test_caller(),
                &sink,
                &test_peer(),
            )
            .await
            .expect("handle should succeed");

        assert!(*force_seen.lock().unwrap(), "force flag must reach host");
        let bytes = buf.lock().unwrap().clone();
        let json: serde_json::Value =
            serde_json::from_slice(&bytes).expect("response should be valid JSON");
        assert_eq!(
            json.get("type").and_then(|v| v.as_str()),
            Some("credential_deleted")
        );
        assert_eq!(
            json.get("broken_references").and_then(|v| v.as_u64()),
            Some(3)
        );
    }

    /// PR 3: list rows get `is_referenced` / `referenced_by`
    /// populated from `host.credential_references()`.
    #[tokio::test]
    async fn credential_list_attaches_reference_info() {
        struct RefsHost;
        #[async_trait]
        impl CredentialHost for RefsHost {
            fn list_credentials(
                &self,
                _namespace: Option<&str>,
                _kind: Option<CredentialKind>,
                _include_system: bool,
            ) -> Vec<CredentialRow> {
                vec![
                    CredentialRow {
                        id: "id-used".to_string(),
                        namespace: "llm".to_string(),
                        name: "anthropic-sonnet".to_string(),
                        kind: "api_key".to_string(),
                        has_key: true,
                        last_tested_at: None,
                        last_tested_ok: None,
                        system_owned: false,
                        is_referenced: false,
                        referenced_by: Vec::new(),
                    },
                    CredentialRow {
                        id: "id-unused".to_string(),
                        namespace: "llm".to_string(),
                        name: "old-key".to_string(),
                        kind: "api_key".to_string(),
                        has_key: true,
                        last_tested_at: None,
                        last_tested_ok: None,
                        system_owned: false,
                        is_referenced: false,
                        referenced_by: Vec::new(),
                    },
                ]
            }
            fn get_credential(&self, _id: &str) -> Option<CredentialWire> {
                None
            }
            fn get_credential_material(&self, _id: &str) -> Option<SecretString> {
                None
            }
            async fn set_credential(
                &self,
                _namespace: &str,
                _name: &str,
                _kind: CredentialKind,
                _material: &SecretString,
                _metadata: Option<serde_json::Value>,
                _replace_on: Option<&str>,
            ) -> anyhow::Result<(String, u32)> {
                Ok(("id-stub".to_string(), 0))
            }
            async fn delete_credential(
                &self,
                _id: &str,
                _force: bool,
            ) -> anyhow::Result<CredentialDeleteOutcome> {
                Ok(CredentialDeleteOutcome::Removed {
                    broken_references: 0,
                })
            }
            async fn credential_references(&self) -> HashMap<String, Vec<ModelSummary>> {
                let mut m = HashMap::new();
                m.insert(
                    "id-used".to_string(),
                    vec![ModelSummary {
                        id: "anthropic-sonnet".to_string(),
                        display_name: "Claude Sonnet 4.5".to_string(),
                        template_id: Some("anthropic".to_string()),
                        api_type: "anthropic".to_string(),
                        base_url: "https://api.anthropic.com".to_string(),
                        model_id: "claude-sonnet-4-5".to_string(),
                        context_window: Some(200_000),
                        max_output_tokens: Some(64_000),
                        headers: Default::default(),
                        credential_id: Some("id-used".to_string()),
                        requires_key: true,
                        is_local: false,
                        enabled: true,
                    }],
                );
                m
            }
        }
        let handler = CredentialHandler::new(
            Arc::new(RefsHost),
            Arc::new(StubBindingHost),
        );
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sink = CaptureSink(buf.clone());

        handler
            .handle(
                RequestPacket::CredentialList {
                    request_id: 70,
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
            .expect("response should have providers array");

        // Used credential is referenced; unused is not.
        let used = providers
            .iter()
            .find(|p| p.get("id").and_then(|v| v.as_str()) == Some("id-used"))
            .expect("id-used row must be present");
        assert_eq!(
            used.get("is_referenced").and_then(|v| v.as_bool()),
            Some(true)
        );
        let deps = used
            .get("referenced_by")
            .and_then(|v| v.as_array())
            .expect("referenced_by must be an array");
        assert_eq!(deps.len(), 1);
        assert_eq!(
            deps[0].get("id").and_then(|v| v.as_str()),
            Some("anthropic-sonnet")
        );

        let unused = providers
            .iter()
            .find(|p| p.get("id").and_then(|v| v.as_str()) == Some("id-unused"))
            .expect("id-unused row must be present");
        assert_eq!(
            unused.get("is_referenced").and_then(|v| v.as_bool()),
            Some(false)
        );
        assert!(
            unused.get("referenced_by").is_none()
                || unused
                    .get("referenced_by")
                    .and_then(|v| v.as_array())
                    .map(|a| a.is_empty())
                    .unwrap_or(true),
            "unused credential should not list dependents"
        );
    }
}
