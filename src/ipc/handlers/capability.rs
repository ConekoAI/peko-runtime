//! `capability` domain request handler (F6 step 8).
//!
//! Owns the principal-capability management IPC variants:
//! `CapabilityGrant`, `CapabilityList`, `CapabilityRevoke`. The handler
//! holds a narrow [`CapabilityHost`] port; the daemon-side
//! implementation (`AppState`) is reached only through the trait, so
//! this module never imports `crate::daemon::state::AppState`
//! directly.
//!
//! Boundary rules:
//! - Dependency inversion: the consumer (`ipc::handlers::capability`)
//!   defines the [`CapabilityHost`] trait; the producer (`daemon::state`)
//!   implements it (same pattern as `SystemHost`, `AuthHost`, `ToolHost`,
//!   `TunnelHost`).
//! - F6: this module must not import any other `ipc::handlers::*` module.
//!
//! Capability authority: grant/revoke flow through
//! `PrincipalManager::update_config`, which holds the single
//! per-principal write lock — there is no IPC-side bypass. The list
//! path returns `{granted, detected, active}` derived from the
//! per-principal `ExtensionCatalog` (built from capabilities +
//! `agent_prompts` + the daemon-wide `ExtensionStore::global_items()`),
//! so the response reflects the same enable set the runtime sees.

use std::sync::Arc;

use async_trait::async_trait;

use crate::auth::caller::CallerContext;
use crate::extensions::framework::store::ExtensionStore;
use crate::ipc::handlers::RequestHandler;
use crate::ipc::packet::{RequestPacket, ResponsePacket};
use crate::ipc::response_sink::ResponseSink;
use crate::ipc::send_response::send_response;
use crate::ipc::server::PeerAddr;
use crate::principal::manager::PrincipalManager;

/// Narrow port the `capability` handler uses to reach daemon state.
///
/// `AppState` is the sole implementor. Both methods are sync: they
/// return cheap references, so the trait is trivially object-safe and
/// the handler pays no `async_trait` overhead. The actual per-principal
/// reads/writes happen in the handler against these accessors.
pub(crate) trait CapabilityHost: Send + Sync {
    /// Principal manager used for `update_config` (grant/revoke) and
    /// `get_by_name` (list).
    fn principal_manager(&self) -> &Arc<PrincipalManager>;

    /// Extension store used to source `global_items()` for the list
    /// path's `ExtensionCatalog::build`.
    fn extension_store(&self) -> &Arc<ExtensionStore>;
}

/// `capability` domain request handler. Constructed with an
/// `Arc<dyn CapabilityHost>` (typically `Arc::new(app_state.clone())`
/// from the dispatcher).
pub(crate) struct CapabilityHandler {
    host: Arc<dyn CapabilityHost>,
}

impl CapabilityHandler {
    pub(crate) fn new(host: Arc<dyn CapabilityHost>) -> Self {
        Self { host }
    }
}

#[async_trait]
impl RequestHandler for CapabilityHandler {
    fn domain(&self) -> &'static str {
        "capability"
    }

    fn matches(&self, request: &RequestPacket) -> bool {
        matches!(
            request,
            RequestPacket::CapabilityGrant { .. }
                | RequestPacket::CapabilityList { .. }
                | RequestPacket::CapabilityRevoke { .. }
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
            RequestPacket::CapabilityGrant {
                request_id,
                principal,
                capability,
            } => {
                let cap = crate::principal::Capability::new(capability);
                let pm = self.host.principal_manager().clone();
                let result = pm
                    .update_config(&principal, |config| {
                        if !config.capabilities.contains(&cap) {
                            config.capabilities.push(cap.clone());
                        }
                    })
                    .await;

                match result {
                    Ok(_) => {
                        let response = ResponsePacket::CapabilityGranted {
                            request_id,
                            capability: cap.to_string(),
                            message: format!(
                                "Capability '{}' granted to principal '{}'",
                                cap, principal
                            ),
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
            }

            RequestPacket::CapabilityRevoke {
                request_id,
                principal,
                capability,
            } => {
                let cap = crate::principal::Capability::new(capability);
                let pm = self.host.principal_manager().clone();
                let result = pm
                    .update_config(&principal, |config| {
                        config.capabilities.remove(&cap);
                    })
                    .await;

                match result {
                    Ok(_) => {
                        let response = ResponsePacket::CapabilityRevoked {
                            request_id,
                            capability: cap.to_string(),
                            message: format!(
                                "Capability '{}' revoked from principal '{}'",
                                cap, principal
                            ),
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
            }

            RequestPacket::CapabilityList {
                request_id,
                principal,
            } => {
                let pm = self.host.principal_manager().clone();
                let store = self.host.extension_store().clone();
                match pm.get_by_name(&principal).await {
                    Some(principal_ref) => {
                        let capabilities = principal_ref.config.read().await.capabilities.clone();
                        let granted = capabilities.to_strings();

                        let global_items = store.global_items().await;
                        let catalog = crate::principal::ExtensionCatalog::build(
                            &capabilities,
                            &principal_ref.agent_prompts,
                            &global_items,
                        );

                        let detected = catalog.detected_capabilities();
                        let active = catalog.active_capabilities(&capabilities);

                        let response = ResponsePacket::CapabilityList {
                            request_id,
                            principal,
                            granted,
                            detected,
                            active,
                        };
                        send_response(sink, response).await?;
                    }
                    None => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Principal '{principal}' not found"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            // `matches()` returned true, so the exhaustive list above
            // covers every owned variant. This arm is unreachable.
            _ => unreachable!("CapabilityHandler::matches allowed an unhandled variant"),
        }
        Ok(())
    }
}