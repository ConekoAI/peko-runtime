//! `ext_runtime` domain request handler (F6 step 11).
//!
//! Owns the runtime lifecycle IPC variants: `ExtStart`, `ExtStop`,
//! `ExtRestart`, `ExtStatus`. These drive the background extension
//! runtime manager (ADR-025) — the daemon-side state machine that
//! boots MCP servers, extension runtimes, etc.
//!
//! The handler holds a narrow [`ExtRuntimeHost`] port; the daemon-side
//! implementation (`AppState`) is reached only through the trait, so
//! this module never imports `crate::daemon::state::AppState`
//! directly.
//!
//! Boundary rules:
//! - Dependency inversion: the consumer (`ipc::handlers::ext_runtime`)
//!   defines the [`ExtRuntimeHost`] trait; the producer (`daemon::state`)
//!   implements it (same pattern as the rest of the F6/F7 handler family).
//! - F6: this module must not import any other `ipc::handlers::*` module.

use std::sync::Arc;

use async_trait::async_trait;

use crate::auth::caller::CallerContext;
use crate::daemon::background_runtime::{BackgroundRuntimeManager, ExtensionRuntimeStarterRegistry, StarterContext};
use crate::ipc::handlers::RequestHandler;
use crate::ipc::packet::{RequestPacket, ResponsePacket};
use crate::ipc::response_sink::ResponseSink;
use crate::ipc::send_response::send_response;
use crate::ipc::server::PeerAddr;

/// Narrow port the `ext_runtime` handler uses to reach the background
/// runtime machinery. `AppState` is the sole implementor. `starter_context`
/// is sync (returns an owned `StarterContext` value) so the trait is
/// object-safe without `async_trait`.
pub(crate) trait ExtRuntimeHost: Send + Sync {
    /// Runtime starter registry that knows how to start / stop / restart
    /// a runtime by extension id (ADR-025).
    fn runtime_starter_registry(&self) -> &Arc<ExtensionRuntimeStarterRegistry>;

    /// Snapshot the starter context (held `Arc`s into the daemon's
    /// background runtime subsystems) for the registry to operate on.
    fn starter_context(&self) -> StarterContext;

    /// Background runtime manager used to read live runtime state for
    /// `ExtStatus`.
    fn background_runtime_manager(&self) -> &Arc<BackgroundRuntimeManager>;
}

/// `ext_runtime` domain request handler. Constructed with an
/// `Arc<dyn ExtRuntimeHost>` (typically `Arc::new(app_state.clone())`
/// from the dispatcher).
pub(crate) struct ExtRuntimeHandler {
    host: Arc<dyn ExtRuntimeHost>,
}

impl ExtRuntimeHandler {
    pub(crate) fn new(host: Arc<dyn ExtRuntimeHost>) -> Self {
        Self { host }
    }
}

#[async_trait]
impl RequestHandler for ExtRuntimeHandler {
    fn domain(&self) -> &'static str {
        "ext_runtime"
    }

    fn matches(&self, request: &RequestPacket) -> bool {
        matches!(
            request,
            RequestPacket::ExtStart { .. }
                | RequestPacket::ExtStop { .. }
                | RequestPacket::ExtRestart { .. }
                | RequestPacket::ExtStatus { .. }
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
            RequestPacket::ExtStart {
                request_id,
                extension_id,
            } => {
                let registry = self.host.runtime_starter_registry().clone();
                let ctx = self.host.starter_context();
                match registry.start(&extension_id, &ctx).await {
                    Ok(()) => {
                        let response = ResponsePacket::ExtStarted {
                            request_id,
                            extension_id,
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

            RequestPacket::ExtStop {
                request_id,
                extension_id,
            } => {
                let registry = self.host.runtime_starter_registry().clone();
                let ctx = self.host.starter_context();
                match registry.stop(&extension_id, &ctx).await {
                    Ok(()) => {
                        let response = ResponsePacket::ExtStopped {
                            request_id,
                            extension_id,
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

            RequestPacket::ExtRestart {
                request_id,
                extension_id,
            } => {
                let registry = self.host.runtime_starter_registry().clone();
                let ctx = self.host.starter_context();
                match registry.restart(&extension_id, &ctx).await {
                    Ok(()) => {
                        let response = ResponsePacket::ExtRestarted {
                            request_id,
                            extension_id,
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

            RequestPacket::ExtStatus {
                request_id,
                extension_id,
            } => {
                let manager = self.host.background_runtime_manager().clone();
                match manager.get_state(&extension_id).await {
                    Some(runtime_state) => {
                        // Also get summary for restart_count and last_error.
                        let summaries = manager.list().await;
                        let summary = summaries.iter().find(|s| s.id == extension_id);
                        let restart_count = summary.map(|s| s.restart_count).unwrap_or(0);
                        let last_error = summary.and_then(|s| s.last_error.clone());

                        let response = ResponsePacket::ExtStatus {
                            request_id,
                            extension_id,
                            state: runtime_state.to_string(),
                            restart_count,
                            last_error,
                        };
                        send_response(sink, response).await?;
                    }
                    None => {
                        let response = ResponsePacket::ExtStatus {
                            request_id,
                            extension_id,
                            state: "not_found".to_string(),
                            restart_count: 0,
                            last_error: None,
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            // `matches()` returned true, so the exhaustive list above
            // covers every owned variant. This arm is unreachable.
            _ => unreachable!("ExtRuntimeHandler::matches allowed an unhandled variant"),
        }
        Ok(())
    }
}