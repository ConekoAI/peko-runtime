//! `quota` domain request handler (F18).
//!
//! Owns the per-principal quota IPC variants: `QuotaGet`, `QuotaSet`,
//! `QuotaReset`. The handler holds a narrow [`QuotaHost`] port; the
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

use crate::auth::caller::CallerContext;
use crate::ipc::handlers::RequestHandler;
use crate::ipc::packet::{RequestPacket, ResponsePacket};
use crate::ipc::response_sink::ResponseSink;
use crate::ipc::send_response::send_response;
use crate::ipc::server::PeerAddr;
use crate::principal::manager::PrincipalManager;
use crate::quota::{QuotaConfig, QuotaState};

/// Narrow port the `quota` handler uses to reach daemon state.
///
/// `AppState` is the sole implementor. `principal_manager` returns a
/// cheap reference so the trait is object-safe without `async_trait`.
/// The handler does the async lookups itself; this trait only exposes
/// the principal manager.
pub(crate) trait QuotaHost: Send + Sync {
    fn principal_manager(&self) -> &Arc<PrincipalManager>;
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
    async fn quota_status_response(
        &self,
        request_id: u64,
        principal_name: &str,
    ) -> ResponsePacket {
        let Some(principal) = self.host.principal_manager().get_by_name(principal_name).await else {
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
            RequestPacket::QuotaGet { request_id, name } => {
                let response = self.quota_status_response(request_id, &name).await;
                send_response(sink, response).await
            }

            RequestPacket::QuotaSet {
                request_id,
                name,
                config,
            } => {
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

            RequestPacket::QuotaReset { request_id, name } => {
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