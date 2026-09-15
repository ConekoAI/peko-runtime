//! `runtime` domain request handler (F6 step 9).
//!
//! Owns the runtime identity IPC variants: `RuntimeId` and
//! `RuntimeInfo`, which surface the daemon's own identity and
//! metadata (ADR-032). The legacy `KnownRuntimes` trust registry
//! (List/Register/Trust/Remove) was removed in the ADR-057 cleanup:
//! `TrustLevel` was never consulted by any access decision, and the
//! direct-transport fields it carried were dead.
//!
//! The handler holds a narrow [`RuntimeHost`] port; the daemon-side
//! implementation (`AppState`) is reached only through the trait, so
//! this module never imports `crate::daemon::state::AppState`
//! directly.
//!
//! Boundary rules:
//! - Dependency inversion: the consumer (`ipc::handlers::runtime`)
//!   defines the [`RuntimeHost`] trait; the producer (`daemon::state`)
//!   implements it (same pattern as the rest of the F6/F7 handler family).
//! - F6: this module must not import any other `ipc::handlers::*` module.

use std::sync::Arc;

use async_trait::async_trait;

use crate::ipc::handlers::RequestHandler;
use crate::ipc::packet::{
    HostInfoResponse, RequestPacket, ResponsePacket, RuntimeMetadataResponse,
};
use crate::ipc::response_sink::ResponseSink;
use crate::ipc::send_response::send_response;
use crate::ipc::server::PeerAddr;
use peko_auth::caller::CallerContext;
use peko_identity::runtime::RuntimeIdentity;
use peko_identity::runtime_metadata::RuntimeMetadata;

/// Narrow port the `runtime` handler uses to reach daemon state.
///
/// `AppState` is the sole implementor. All methods are sync (cheap
/// references / owned values) so the trait is object-safe without
/// `async_trait`.
pub(crate) trait RuntimeHost: Send + Sync {
    /// This runtime's identity (ADR-032). Powers `RuntimeId`.
    fn runtime_identity(&self) -> &RuntimeIdentity;

    /// This runtime's metadata (display name, version, host info,
    /// capabilities). Powers `RuntimeInfo`.
    fn runtime_metadata(&self) -> &RuntimeMetadata;
}

/// `runtime` domain request handler. Constructed with an
/// `Arc<dyn RuntimeHost>` (typically `Arc::new(app_state.clone())`
/// from the dispatcher).
pub(crate) struct RuntimeHandler {
    host: Arc<dyn RuntimeHost>,
}

impl RuntimeHandler {
    pub(crate) fn new(host: Arc<dyn RuntimeHost>) -> Self {
        Self { host }
    }
}

#[async_trait]
impl RequestHandler for RuntimeHandler {
    fn domain(&self) -> &'static str {
        "runtime"
    }

    fn matches(&self, request: &RequestPacket) -> bool {
        matches!(
            request,
            RequestPacket::RuntimeId { .. } | RequestPacket::RuntimeInfo { .. }
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
            RequestPacket::RuntimeId { request_id } => {
                let did = self.host.runtime_identity().runtime_did.clone();
                let response = ResponsePacket::RuntimeId { request_id, did };
                send_response(sink, response).await?;
            }

            RequestPacket::RuntimeInfo { request_id } => {
                let meta = self.host.runtime_metadata();
                let response = ResponsePacket::RuntimeInfo {
                    request_id,
                    metadata: RuntimeMetadataResponse {
                        runtime_id: meta.runtime_id.clone(),
                        display_name: meta.display_name.clone(),
                        created_at: meta.created_at.to_rfc3339(),
                        last_seen_at: meta.last_seen_at.to_rfc3339(),
                        version: meta.version.clone(),
                        capabilities: meta.capabilities.clone(),
                        host_info: HostInfoResponse {
                            os: meta.host_info.os.clone(),
                            arch: meta.host_info.arch.clone(),
                            hostname: meta.host_info.hostname.clone(),
                        },
                    },
                };
                send_response(sink, response).await?;
            }

            // `matches()` returned true, so the exhaustive list above
            // covers every owned variant. This arm is unreachable.
            _ => unreachable!("RuntimeHandler::matches allowed an unhandled variant"),
        }
        Ok(())
    }
}
