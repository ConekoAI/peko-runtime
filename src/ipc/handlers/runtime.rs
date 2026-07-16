//! `runtime` domain request handler (F6 step 9).
//!
//! Owns the runtime registry / identity IPC variants: `RuntimeId`,
//! `RuntimeInfo`, `RuntimeList`, `RuntimeRegister`, `RuntimeTrust`,
//! `RuntimeRemove`. These surface the daemon's own identity (ADR-032)
//! and the persistent `KnownRuntimes` registry the tunnel uses to
//! trust peer runtimes.
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

use crate::auth::caller::CallerContext;
use crate::identity::runtime::RuntimeIdentity;
use crate::identity::runtime_metadata::RuntimeMetadata;
use crate::ipc::handlers::RequestHandler;
use crate::ipc::packet::{
    HostInfoResponse, KnownRuntimeResponse, RequestPacket, ResponsePacket, RuntimeMetadataResponse,
};
use crate::ipc::response_sink::ResponseSink;
use crate::ipc::send_response::send_response;
use crate::ipc::server::PeerAddr;
use crate::tunnel::known_runtimes::{KnownRuntimes, TrustLevel};

/// Narrow port the `runtime` handler uses to reach daemon state.
///
/// `AppState` is the sole implementor. All methods are sync (cheap
/// references / owned values) so the trait is object-safe without
/// `async_trait`. The actual per-request awaits against the
/// `KnownRuntimes` lock happen inside the handler.
pub(crate) trait RuntimeHost: Send + Sync {
    /// This runtime's identity (ADR-032). Powers `RuntimeId`.
    fn runtime_identity(&self) -> &RuntimeIdentity;

    /// This runtime's metadata (display name, version, host info,
    /// capabilities). Powers `RuntimeInfo`.
    fn runtime_metadata(&self) -> &RuntimeMetadata;

    /// Persistent registry of peer runtimes the tunnel has seen
    /// (ADR-032). Read/write both go through the inner `tokio::RwLock`.
    fn known_runtimes(&self) -> &Arc<tokio::sync::RwLock<KnownRuntimes>>;

    /// Daemon config dir, used to build a `PathResolver` for
    /// `KnownRuntimes::save`.
    fn config_dir(&self) -> std::path::PathBuf;

    /// Daemon data dir, used to build a `PathResolver` for
    /// `KnownRuntimes::save`.
    fn data_dir(&self) -> std::path::PathBuf;

    /// Daemon cache dir, used to build a `PathResolver` for
    /// `KnownRuntimes::save`.
    fn cache_dir(&self) -> std::path::PathBuf;
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
            RequestPacket::RuntimeId { .. }
                | RequestPacket::RuntimeInfo { .. }
                | RequestPacket::RuntimeList { .. }
                | RequestPacket::RuntimeRegister { .. }
                | RequestPacket::RuntimeTrust { .. }
                | RequestPacket::RuntimeRemove { .. }
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

            RequestPacket::RuntimeList { request_id } => {
                let registry = self.host.known_runtimes().read().await;
                let runtimes: Vec<KnownRuntimeResponse> = registry
                    .list()
                    .iter()
                    .map(|r| KnownRuntimeResponse {
                        runtime_id: r.runtime_id.clone(),
                        display_name: r.display_name.clone(),
                        last_seen: Some(r.last_seen.to_rfc3339()),
                        connection_endpoint: r.connection_endpoint.clone(),
                        trust_level: format!("{:?}", r.trust_level).to_lowercase(),
                    })
                    .collect();
                let response = ResponsePacket::RuntimeList {
                    request_id,
                    runtimes,
                };
                send_response(sink, response).await?;
            }

            RequestPacket::RuntimeRegister {
                request_id,
                runtime_id,
                display_name,
            } => {
                let mut registry = self.host.known_runtimes().write().await;
                registry.register(&runtime_id, &display_name, None, TrustLevel::Untrusted);
                let resolver = crate::common::paths::PathResolver::with_dirs(
                    self.host.config_dir(),
                    self.host.data_dir(),
                    self.host.cache_dir(),
                );
                match registry.save(&resolver) {
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
                            message: e.to_string(),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            RequestPacket::RuntimeTrust {
                request_id,
                runtime_id,
            } => {
                let mut registry = self.host.known_runtimes().write().await;
                match registry.trust(&runtime_id, TrustLevel::Authorized) {
                    Ok(()) => {
                        let resolver = crate::common::paths::PathResolver::with_dirs(
                            self.host.config_dir(),
                            self.host.data_dir(),
                            self.host.cache_dir(),
                        );
                        let _ = registry.save(&resolver);
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
                            message: e.to_string(),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            RequestPacket::RuntimeRemove {
                request_id,
                runtime_id,
            } => {
                let mut registry = self.host.known_runtimes().write().await;
                match registry.remove(&runtime_id) {
                    Ok(()) => {
                        let resolver = crate::common::paths::PathResolver::with_dirs(
                            self.host.config_dir(),
                            self.host.data_dir(),
                            self.host.cache_dir(),
                        );
                        let _ = registry.save(&resolver);
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
                            message: e.to_string(),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            // `matches()` returned true, so the exhaustive list above
            // covers every owned variant. This arm is unreachable.
            _ => unreachable!("RuntimeHandler::matches allowed an unhandled variant"),
        }
        Ok(())
    }
}
