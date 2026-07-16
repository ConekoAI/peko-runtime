//! `instance` domain request handler (F6 step 10).
//!
//! Owns the per-instance status / exposure IPC variants:
//! `InstanceSetStatus`, `InstanceSetExposure`. These drive the
//! tunnel-side instance state machine (ADR-035).
//!
//! The handler holds a narrow [`InstanceHost`] port; the daemon-side
//! implementation (`AppState`) is reached only through the trait, so
//! this module never imports `crate::daemon::state::AppState`
//! directly.
//!
//! Boundary rules:
//! - Dependency inversion: the consumer (`ipc::handlers::instance`)
//!   defines the [`InstanceHost`] trait; the producer (`daemon::state`)
//!   implements it (same pattern as `SystemHost`, `AuthHost`, `ToolHost`,
//!   `TunnelHost`, `CapabilityHost`).
//! - F6: this module must not import any other `ipc::handlers::*` module.

use std::sync::Arc;

use async_trait::async_trait;
use tracing::warn;

use crate::auth::caller::CallerContext;
use crate::ipc::handlers::RequestHandler;
use crate::ipc::packet::{RequestPacket, ResponsePacket};
use crate::ipc::response_sink::ResponseSink;
use crate::ipc::send_response::send_response;
use crate::ipc::server::PeerAddr;

/// Narrow port the `instance` handler uses to reach the tunnel
/// dispatcher. `AppState` is the sole implementor. `tunnel_dispatcher`
/// is async and may return `None` when the tunnel is not active —
/// the handler treats that as an error response rather than a panic,
/// preserving the legacy behavior.
#[async_trait]
pub(crate) trait InstanceHost: Send + Sync {
    /// Snapshot the live outbound tunnel dispatcher, if one is running.
    async fn tunnel_dispatcher(&self) -> Option<crate::tunnel::TunnelDispatcher>;
}

/// `instance` domain request handler. Constructed with an
/// `Arc<dyn InstanceHost>` (typically `Arc::new(app_state.clone())`
/// from the dispatcher).
pub(crate) struct InstanceHandler {
    host: Arc<dyn InstanceHost>,
}

impl InstanceHandler {
    pub(crate) fn new(host: Arc<dyn InstanceHost>) -> Self {
        Self { host }
    }
}

#[async_trait]
impl RequestHandler for InstanceHandler {
    fn domain(&self) -> &'static str {
        "instance"
    }

    fn matches(&self, request: &RequestPacket) -> bool {
        matches!(
            request,
            RequestPacket::InstanceSetStatus { .. } | RequestPacket::InstanceSetExposure { .. }
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
            RequestPacket::InstanceSetStatus {
                request_id,
                agent_name,
                status,
            } => {
                let status_enum = match status.as_str() {
                    "online" => crate::tunnel::protocol::InstanceStatus::Online,
                    "offline" => crate::tunnel::protocol::InstanceStatus::Offline,
                    "busy" => crate::tunnel::protocol::InstanceStatus::Busy,
                    "error" => crate::tunnel::protocol::InstanceStatus::Error,
                    other => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!(
                                "Invalid status '{other}'. Expected: online, offline, busy, error"
                            ),
                        };
                        send_response(sink, response).await?;
                        return Ok(());
                    }
                };

                match self.host.tunnel_dispatcher().await {
                    Some(dispatcher) => {
                        match dispatcher
                            .set_instance_status(&agent_name, status_enum)
                            .await
                        {
                            Ok(()) => {
                                let response = ResponsePacket::Done {
                                    request_id,
                                    success: true,
                                    error: None,
                                };
                                send_response(sink, response).await?;
                            }
                            Err(e) => {
                                let response = ResponsePacket::Error {
                                    request_id,
                                    message: format!("Failed to set instance status: {e}"),
                                };
                                send_response(sink, response).await?;
                            }
                        }
                    }
                    None => {
                        warn!(
                            "InstanceSetStatus called while tunnel is not active \
                             (agent_name={agent_name}, status={status})"
                        );
                        let response = ResponsePacket::Error {
                            request_id,
                            message: "Tunnel is not active".to_string(),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            RequestPacket::InstanceSetExposure {
                request_id,
                agent_name,
                exposure,
            } => {
                let exposure_enum = match exposure.as_str() {
                    "unexposed" => crate::tunnel::protocol::InstanceExposure::Unexposed,
                    "private" => crate::tunnel::protocol::InstanceExposure::Private,
                    "public" => crate::tunnel::protocol::InstanceExposure::Public,
                    other => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!(
                                "Invalid exposure '{other}'. Expected: unexposed, private, public"
                            ),
                        };
                        send_response(sink, response).await?;
                        return Ok(());
                    }
                };

                match self.host.tunnel_dispatcher().await {
                    Some(dispatcher) => {
                        match dispatcher
                            .set_instance_exposure(&agent_name, exposure_enum)
                            .await
                        {
                            Ok(()) => {
                                let response = ResponsePacket::Done {
                                    request_id,
                                    success: true,
                                    error: None,
                                };
                                send_response(sink, response).await?;
                            }
                            Err(e) => {
                                let response = ResponsePacket::Error {
                                    request_id,
                                    message: format!("Failed to set instance exposure: {e}"),
                                };
                                send_response(sink, response).await?;
                            }
                        }
                    }
                    None => {
                        warn!(
                            "InstanceSetExposure called while tunnel is not active \
                             (agent_name={agent_name}, exposure={exposure})"
                        );
                        let response = ResponsePacket::Error {
                            request_id,
                            message: "Tunnel is not active".to_string(),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            // `matches()` returned true, so the exhaustive list above
            // covers every owned variant. This arm is unreachable.
            _ => unreachable!("InstanceHandler::matches allowed an unhandled variant"),
        }
        Ok(())
    }
}
