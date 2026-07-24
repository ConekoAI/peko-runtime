//! `quota` domain request handler (F18, F20).
//!
//! Owns the per-principal quota IPC variants: `QuotaGet`, `QuotaSet`,
//! `QuotaReset`. F20 extended the same variants to also handle
//! per-peer quota via the `--peer` CLI flag / `is_peer` IPC field.
//! The handler holds a narrow [`QuotaHost`] port; the
//! daemon-side implementation (`AppState`) is reached only through the
//! trait, so this module never imports
//! `crate::daemon::state::AppState` directly (F6 boundary rule).
//!
//! Boundary rules:
//! - Dependency inversion: the consumer (`ipc::handlers::quota`)
//!   defines the [`QuotaHost`] trait; the producer (`daemon::state`)
//!   implements it (same pattern as the rest of the F6/F7 family).
//! - F6: this module must not import any other `ipc::handlers::*`
//!   module.

use std::sync::Arc;

use async_trait::async_trait;

use crate::ipc::handlers::RequestHandler;
use crate::ipc::packet::{RequestPacket, ResponsePacket};
use crate::ipc::response_sink::ResponseSink;
use crate::ipc::send_response::send_response;
use crate::ipc::server::PeerAddr;
use crate::principal::manager::PrincipalManager;
use peko_auth::caller::CallerContext;
use peko_principal::peer::PeerRegistry;
use peko_quota::{QuotaConfig, QuotaState};

/// Narrow port the `quota` handler uses to reach daemon state.
///
/// `AppState` is the sole implementor. `principal_manager` returns a
/// cheap reference so the trait is object-safe without `async_trait`.
/// The handler does the async lookups itself; this trait only exposes
/// the principal manager and (F20) the peer registry.
pub(crate) trait QuotaHost: Send + Sync {
    fn principal_manager(&self) -> &Arc<PrincipalManager>;
    /// F20: peer registry. When the handler routes an `is_peer=true`
    /// request, it uses this to resolve / mutate the peer meter.
    /// Returned as a borrowed reference so the trait stays
    /// object-safe; `None` means peer attribution is not configured
    /// (the handler returns an error for `is_peer=true` requests in
    /// that case).
    fn peer_registry(&self) -> Option<&Arc<PeerRegistry>>;
}

/// `quota` domain request handler. Constructed with an
/// `Arc<dyn QuotaHost>` (typically `Arc::new(app_state.clone())` from
/// the dispatcher).
pub(crate) struct QuotaHandler {
    host: Arc<dyn QuotaHost>,
}

impl QuotaHandler {
    pub(crate) fn new(host: Arc<dyn QuotaHost>) -> Self {
        Self { host }
    }

    /// Build a [`ResponsePacket::QuotaStatus`] from the live meter
    /// behind `principal`. The response carries the principal's
    /// `QuotaState` (used counters, window bounds) and the
    /// `QuotaConfig` (limits + cycle) so the CLI can render the
    /// status without a second round-trip.
    async fn quota_status_response(&self, request_id: u64, principal_name: &str) -> ResponsePacket {
        let Some(principal) = self
            .host
            .principal_manager()
            .get_by_name(principal_name)
            .await
        else {
            return ResponsePacket::Error {
                request_id,
                message: format!("principal not found: {principal_name}"),
            };
        };
        let state: QuotaState = principal.quota_meter.snapshot();
        let config: QuotaConfig = principal.quota_meter.config().clone();
        ResponsePacket::QuotaStatus {
            request_id,
            state,
            config,
        }
    }

    /// F20: build a `QuotaStatus` from a peer's meter. Mirrors
    /// [`Self::quota_status_response`] but resolves through the
    /// `PeerRegistry`. Errors with a clear message when no peer
    /// registry is attached — the CLI surfaces this as a config
    /// problem (daemon not started with `--enable-peers` or
    /// equivalent).
    async fn peer_status_response(&self, request_id: u64, peer_id: &str) -> ResponsePacket {
        let Some(registry) = self.host.peer_registry() else {
            return ResponsePacket::Error {
                request_id,
                message: "peer attribution not configured on this daemon".to_string(),
            };
        };
        let peer = match registry.get_or_create(peer_id, chrono::Utc::now()).await {
            Ok(p) => p,
            Err(e) => {
                return ResponsePacket::Error {
                    request_id,
                    message: format!("failed to resolve peer '{peer_id}': {e}"),
                };
            }
        };
        let state: QuotaState = peer.quota_meter.snapshot();
        let config: QuotaConfig = peer.quota_meter.config().clone();
        ResponsePacket::QuotaStatus {
            request_id,
            state,
            config,
        }
    }
}

#[async_trait]
impl RequestHandler for QuotaHandler {
    fn domain(&self) -> &'static str {
        "quota"
    }

    fn matches(&self, request: &RequestPacket) -> bool {
        matches!(
            request,
            RequestPacket::QuotaGet { .. }
                | RequestPacket::QuotaSet { .. }
                | RequestPacket::QuotaReset { .. }
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
            RequestPacket::QuotaGet {
                request_id,
                name,
                is_peer,
            } => {
                let response = if is_peer {
                    self.peer_status_response(request_id, &name).await
                } else {
                    self.quota_status_response(request_id, &name).await
                };
                send_response(sink, response).await
            }

            RequestPacket::QuotaSet {
                request_id,
                name,
                is_peer,
                config,
            } => {
                if is_peer {
                    let Some(registry) = self.host.peer_registry() else {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: "peer attribution not configured on this daemon".to_string(),
                        };
                        return send_response(sink, response).await;
                    };
                    if let Err(e) = registry.set_config(&name, config.clone()).await {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("failed to persist peer quota config: {e}"),
                        };
                        return send_response(sink, response).await;
                    }
                    let response = self.peer_status_response(request_id, &name).await;
                    return send_response(sink, response).await;
                }

                let Some(principal) = self.host.principal_manager().get_by_name(&name).await else {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: format!("principal not found: {name}"),
                    };
                    return send_response(sink, response).await;
                };

                // Persist the new config onto PrincipalConfig so the
                // change survives a daemon restart. The meter's
                // live limits are not yet swapped — that requires
                // rebuilding the meter through the manager, which
                // is a heavier op and would race with in-flight
                // LLM calls. F18 takes the conservative path:
                // `update_config` + persist; the next `peko quota
                // set` after a restart picks up the new limits.
                let update_result = self
                    .host
                    .principal_manager()
                    .update_config(&name, |cfg| {
                        cfg.quota = Some(config.clone());
                    })
                    .await;
                if let Err(e) = update_result {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: format!("failed to persist quota config: {e}"),
                    };
                    return send_response(sink, response).await;
                }

                // Reflect the new config on the live meter so the
                // next `charge` consults the new limits. We replace
                // the in-memory `config` directly via the meter's
                // public surface — see `QuotaMeter::set_config`.
                principal.quota_meter.set_config(config);

                let response = self.quota_status_response(request_id, &name).await;
                send_response(sink, response).await
            }

            RequestPacket::QuotaReset {
                request_id,
                name,
                is_peer,
            } => {
                if is_peer {
                    let Some(registry) = self.host.peer_registry() else {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: "peer attribution not configured on this daemon".to_string(),
                        };
                        return send_response(sink, response).await;
                    };
                    if let Err(e) = registry.reset(&name, chrono::Utc::now()).await {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("failed to reset peer quota: {e}"),
                        };
                        return send_response(sink, response).await;
                    }
                    let response = self.peer_status_response(request_id, &name).await;
                    return send_response(sink, response).await;
                }

                let Some(principal) = self.host.principal_manager().get_by_name(&name).await else {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: format!("principal not found: {name}"),
                    };
                    return send_response(sink, response).await;
                };
                principal.quota_meter.reset(chrono::Utc::now()).await;
                let response = self.quota_status_response(request_id, &name).await;
                send_response(sink, response).await
            }

            // `matches()` returned true, so the exhaustive list above
            // covers every owned variant. This arm is unreachable.
            _ => unreachable!("QuotaHandler::matches allowed an unhandled variant"),
        }
    }
}
