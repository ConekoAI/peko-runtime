//! `auth` domain request handler (F6 step 2).
//!
//! Owns the API-key management + auth status IPC variants (ADR-034):
//! `AuthApiKeyCreate`, `AuthApiKeyList`, `AuthApiKeyRevoke`,
//! `AuthStatus`. The handler holds a narrow [`AuthHost`] port; the
//! daemon-side implementation (`AppState`) is reached only through the
//! trait, so this module never imports `crate::daemon::state::AppState`
//! directly.
//!
//! Boundary rules:
//! - Dependency inversion: the consumer (`ipc::handlers::auth`) defines
//!   the [`AuthHost`] trait; the producer (`daemon::state`) implements
//!   it (same pattern as `TunnelHost` in F5 and `SystemHost` in F6 step 1).
//! - F6: this module must not import any other `ipc::handlers::*` module.
//!
//! Auth access is restricted to local-trust callers for v0.1.0 (see
//! ADR-034); the handler enforces this via `caller.is_local()` exactly
//! like the legacy monolithic match.

use std::sync::Arc;

use async_trait::async_trait;

use crate::ipc::handlers::RequestHandler;
use crate::ipc::packet::{ApiKeySummary, RequestPacket, ResponsePacket};
use crate::ipc::response_sink::ResponseSink;
use crate::ipc::send_response::send_response;
use crate::ipc::server::PeerAddr;
use peko_auth::api_key::ApiKeyStore;
use peko_auth::caller::CallerContext;
use peko_auth::config::AuthConfig;
use peko_auth::types::ApiKeyScope;

/// Narrow port the `auth` handler uses to reach daemon state.
///
/// `AppState` is the sole implementor. Both methods are sync: they
/// return owned values (`AuthConfig` and an `Option<ApiKeyStore>` clone)
/// so the trait is trivially object-safe and the handler pays no
/// `async_trait` overhead.
pub(crate) trait AuthHost: Send + Sync {
    /// Auth configuration (local-trust / PekoHub JWT / API-key flags).
    fn auth_config(&self) -> AuthConfig;

    /// API key store backing ADR-034, if initialized.
    fn api_key_store(&self) -> Option<ApiKeyStore>;
}

/// `auth` domain request handler. Constructed with an `Arc<dyn AuthHost>`
/// (typically `Arc::new(app_state.clone())` from the dispatcher).
pub(crate) struct AuthHandler {
    host: Arc<dyn AuthHost>,
}

impl AuthHandler {
    pub(crate) fn new(host: Arc<dyn AuthHost>) -> Self {
        Self { host }
    }
}

#[async_trait]
impl RequestHandler for AuthHandler {
    fn domain(&self) -> &'static str {
        "auth"
    }

    fn matches(&self, request: &RequestPacket) -> bool {
        matches!(
            request,
            RequestPacket::AuthApiKeyCreate { .. }
                | RequestPacket::AuthApiKeyList { .. }
                | RequestPacket::AuthApiKeyRevoke { .. }
                | RequestPacket::AuthStatus { .. }
        )
    }

    async fn handle(
        &self,
        request: RequestPacket,
        caller: &CallerContext,
        sink: &dyn ResponseSink,
        _peer: &PeerAddr,
    ) -> anyhow::Result<()> {
        match request {
            RequestPacket::AuthApiKeyCreate {
                request_id,
                name,
                scopes,
            } => {
                if !caller.is_local() {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: "API key management requires local access".to_string(),
                    };
                    send_response(sink, response).await?;
                } else if let Some(store) = self.host.api_key_store() {
                    let parsed_scopes: Vec<ApiKeyScope> =
                        scopes.iter().filter_map(|s| s.parse().ok()).collect();
                    match store.create_key(name, parsed_scopes).await {
                        Ok((full_key, key_id)) => {
                            let response = ResponsePacket::AuthApiKeyCreated {
                                request_id,
                                key_id,
                                full_key,
                            };
                            send_response(sink, response).await?;
                        }
                        Err(e) => {
                            let response = ResponsePacket::Error {
                                request_id,
                                message: e.to_string(),
                            };
                            send_response(sink, response).await?;
                        }
                    }
                } else {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: "API key store not initialized".to_string(),
                    };
                    send_response(sink, response).await?;
                }
            }

            RequestPacket::AuthApiKeyList { request_id } => {
                if !caller.is_local() {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: "API key management requires local access".to_string(),
                    };
                    send_response(sink, response).await?;
                } else if let Some(store) = self.host.api_key_store() {
                    let keys = store.list_keys().await;
                    let summaries: Vec<ApiKeySummary> = keys
                        .into_iter()
                        .map(|k| ApiKeySummary {
                            id: k.id,
                            name: k.name,
                            created_at: k.created_at.to_rfc3339(),
                            last_used_at: k.last_used_at.map(|t| t.to_rfc3339()),
                            scopes: k.scopes.iter().map(|s| s.to_string()).collect(),
                            enabled: k.enabled,
                        })
                        .collect();
                    let response = ResponsePacket::AuthApiKeyList {
                        request_id,
                        keys: summaries,
                    };
                    send_response(sink, response).await?;
                } else {
                    let response = ResponsePacket::AuthApiKeyList {
                        request_id,
                        keys: Vec::new(),
                    };
                    send_response(sink, response).await?;
                }
            }

            RequestPacket::AuthApiKeyRevoke { request_id, key_id } => {
                if !caller.is_local() {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: "API key management requires local access".to_string(),
                    };
                    send_response(sink, response).await?;
                } else if let Some(store) = self.host.api_key_store() {
                    match store.revoke_key(&key_id).await {
                        Ok(true) => {
                            let response = ResponsePacket::AuthApiKeyRevoked { request_id, key_id };
                            send_response(sink, response).await?;
                        }
                        Ok(false) => {
                            let response = ResponsePacket::Error {
                                request_id,
                                message: format!("Key '{key_id}' not found"),
                            };
                            send_response(sink, response).await?;
                        }
                        Err(e) => {
                            let response = ResponsePacket::Error {
                                request_id,
                                message: e.to_string(),
                            };
                            send_response(sink, response).await?;
                        }
                    }
                } else {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: "API key store not initialized".to_string(),
                    };
                    send_response(sink, response).await?;
                }
            }

            RequestPacket::AuthStatus { request_id } => {
                let auth_config = self.host.auth_config();
                let api_key_count = if let Some(store) = self.host.api_key_store() {
                    store.list_keys().await.len()
                } else {
                    0
                };
                let response = ResponsePacket::AuthStatus {
                    request_id,
                    local_trust_enabled: auth_config.enable_local_trust(),
                    pekohub_jwt_enabled: auth_config.enable_pekohub_jwt(),
                    api_key_enabled: auth_config.enable_api_key(),
                    api_key_count,
                };
                send_response(sink, response).await?;
            }

            // `matches()` returned true, so the exhaustive list above
            // covers every owned variant. This arm is unreachable.
            _ => unreachable!("AuthHandler::matches allowed an unhandled variant"),
        }
        Ok(())
    }
}
