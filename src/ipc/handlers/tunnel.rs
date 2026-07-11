//! `tunnel` domain request handler (F6 step 4).
//!
//! Owns the daemon-side tunnel control IPC variants: `TunnelStop`,
//! `TunnelStatus`. The handler holds a narrow [`TunnelHost`] port;
//! the daemon-side implementation (`AppState`) is reached only through
//! the trait, so this module never imports
//! `crate::daemon::state::AppState` directly.
//!
//! Note: the inbound-message `TunnelHost` defined in `crate::tunnel::host`
//! (F5) is a separate trait with different methods — it powers the
//! tunnel dispatcher (ADR-035), not the IPC control surface. The two
//! traits share a name but live in different modules and serve
//! different consumers, per the F5 + F7 dependency-inversion pattern.
//!
//! Boundary rules:
//! - Dependency inversion: the consumer (`ipc::handlers::tunnel`) defines
//!   the [`TunnelHost`] trait; the producer (`daemon::state`) implements
//!   it (same pattern as `SystemHost`, `AuthHost`, `ToolHost`).
//! - F6: this module must not import any other `ipc::handlers::*` module.

use std::sync::Arc;

use async_trait::async_trait;

use crate::auth::caller::CallerContext;
use crate::ipc::handlers::RequestHandler;
use crate::ipc::packet::{RequestPacket, ResponsePacket};
use crate::ipc::response_sink::ResponseSink;
use crate::ipc::send_response::send_response;
use crate::ipc::server::PeerAddr;

/// Narrow port the `tunnel` IPC handler uses to reach daemon state.
///
/// `AppState` is the sole implementor. All methods are async: they
/// drive the tunnel lifecycle (ADR-035) and stay behind `async_trait`
/// to match the rest of the F6/F7 handler family.
#[async_trait]
pub(crate) trait TunnelHost: Send + Sync {
    /// Stop the live outbound tunnel. Idempotent — safe to call when
    /// no tunnel is running.
    async fn stop_tunnel(&self);

    /// Whether the outbound tunnel is currently connected.
    async fn tunnel_connected(&self) -> bool;
}

/// `tunnel` domain request handler. Constructed with an `Arc<dyn TunnelHost>`
/// (typically `Arc::new(app_state.clone())` from the dispatcher).
pub(crate) struct TunnelHandler {
    host: Arc<dyn TunnelHost>,
}

impl TunnelHandler {
    pub(crate) fn new(host: Arc<dyn TunnelHost>) -> Self {
        Self { host }
    }
}

#[async_trait]
impl RequestHandler for TunnelHandler {
    fn domain(&self) -> &'static str {
        "tunnel"
    }

    fn matches(&self, request: &RequestPacket) -> bool {
        matches!(
            request,
            RequestPacket::TunnelStop { .. } | RequestPacket::TunnelStatus { .. }
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
            RequestPacket::TunnelStop { request_id } => {
                self.host.stop_tunnel().await;
                let response = ResponsePacket::Done {
                    request_id,
                    success: true,
                    error: None,
                };
                send_response(sink, response).await?;
            }

            RequestPacket::TunnelStatus { request_id } => {
                let configured = crate::tunnel::credential::has_pekohub_credential();
                let connected = self.host.tunnel_connected().await;
                let response = ResponsePacket::TunnelStatus {
                    request_id,
                    configured,
                    daemon_running: true,
                    connected,
                };
                send_response(sink, response).await?;
            }

            // `matches()` returned true, so the exhaustive list above
            // covers every owned variant. This arm is unreachable.
            _ => unreachable!("TunnelHandler::matches allowed an unhandled variant"),
        }
        Ok(())
    }
}