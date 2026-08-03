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

use crate::extensions::framework::store::ExtensionStore;
use crate::ipc::handlers::RequestHandler;
use crate::ipc::packet::{RequestPacket, ResponsePacket};
use crate::ipc::response_sink::ResponseSink;
use crate::ipc::send_response::send_response;
use crate::ipc::server::PeerAddr;
use crate::principal::manager::PrincipalManager;
use peko_auth::caller::CallerContext;
use peko_observability::{AuditSeverity, Observability};

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

    /// Observability hub for ADR-046 grant audit events.
    /// Returns `Arc<Observability>` (cloned cheaply) so the handler
    /// can call `audit_with_severity` after a successful grant.
    fn observability(&self) -> Arc<Observability>;
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
        caller: &CallerContext,
        sink: &dyn ResponseSink,
        _peer: &PeerAddr,
    ) -> anyhow::Result<()> {
        match request {
            RequestPacket::CapabilityGrant {
                request_id,
                principal,
                capability,
            } => {
                let cap = peko_extension_api::Capability::new(capability);
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
                        // ADR-046 trust+audit: every successful
                        // grant emits a `principal.capability_granted`
                        // audit event. High-power capabilities
                        // (tool:Bash, fs:* network, principal:*,
                        // runtime:*) escalate to `Warn` so they
                        // show up in the user's tail view with the
                        // ⚠ glyph. Low-power grants stay at `Info`.
                        let severity = if cap.is_high_power() {
                            AuditSeverity::Warning
                        } else {
                            AuditSeverity::Info
                        };
                        let details = serde_json::json!({
                            "principal_name": principal,
                            "capability": cap.to_string(),
                            "is_high_power": cap.is_high_power(),
                        });
                        // Best-effort: a logging failure must not
                        // fail the grant — the user's primary
                        // expectation is "the grant happened", the
                        // audit is the durable side effect.
                        let _ = self
                            .host
                            .observability()
                            .audit_with_severity(
                                severity,
                                Some(&caller.subject()),
                                "principal.capability_granted",
                                None,
                                details,
                            )
                            .await;

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
                let cap = peko_extension_api::Capability::new(capability);
                let pm = self.host.principal_manager().clone();

                // Capture whether the principal's grants currently satisfy
                // `cap` (literal or wildcard). We use this to surface
                // `removed: bool` on the response: a true revoke changed
                // the principal's effective authority; a no-op revoke did
                // not. Without this distinction the CLI/desktop can't
                // tell "✅ revoked" from "✅ nothing to revoke".
                let covered_before = match pm.get_by_name(&principal).await {
                    Some(p) => {
                        let cfg = p.config.read().await;
                        cfg.capabilities.is_granted(&cap)
                    }
                    None => false,
                };

                let result = pm
                    .update_config(&principal, |config| {
                        // Drop the literal grant if present and any
                        // wildcard grant whose prefix matches `cap`. Both
                        // are what `is_granted` would have consulted
                        // before the mutation.
                        config
                            .capabilities
                            .retain(|g| g.as_str() != cap.as_str() && !g.matches(&cap));
                    })
                    .await;

                match result {
                    Ok(_) => {
                        let still_covered = match pm.get_by_name(&principal).await {
                            Some(p) => {
                                let cfg = p.config.read().await;
                                cfg.capabilities.is_granted(&cap)
                            }
                            None => false,
                        };
                        let removed = covered_before && !still_covered;
                        let response = ResponsePacket::CapabilityRevoked {
                            request_id,
                            capability: cap.to_string(),
                            message: format!(
                                "Capability '{}' revoked from principal '{}'",
                                cap, principal
                            ),
                            removed,
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
                        let catalog = crate::principal::extension_store::ExtensionCatalog::build(
                            &capabilities,
                            &principal_ref.agent_prompts,
                            &global_items,
                        );

                        let detected = catalog.detected_capabilities();
                        let active = catalog.active_capabilities(&capabilities);
                        let active_extensions: Vec<String> = catalog
                            .items()
                            .iter()
                            .filter(|i| i.enabled)
                            .map(|i| i.id.clone())
                            .collect();

                        let response = ResponsePacket::CapabilityList {
                            request_id,
                            principal,
                            granted,
                            detected,
                            active,
                            active_extensions,
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
