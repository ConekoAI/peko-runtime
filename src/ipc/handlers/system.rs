//! `system` domain request handler (F6 step 1 / F7 first handle).
//!
//! Owns the daemon health / introspection IPC variants:
//! `Ping`, `Shutdown`, `Status`, `SystemStatus`, `SystemDoctor`,
//! `SystemClean`. The handler holds a narrow [`SystemHost`] port — the
//! daemon-side implementation (`AppState`) is reached only through the
//! trait, so this module never imports `crate::daemon::state::AppState`
//! directly.
//!
//! Boundary rules:
//! - F5-style dependency inversion: the consumer (`ipc::handlers::system`)
//!   defines the [`SystemHost`] trait; the producer (`daemon::state`)
//!   implements it. See the impl in `daemon::state::AppState`.
//! - F6: this module must not import any other `ipc::handlers::*` module.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;

use crate::ipc::handlers::RequestHandler;
use crate::ipc::packet::{DoctorCheck, RequestPacket, ResponsePacket};
use crate::ipc::response_sink::ResponseSink;
use crate::ipc::send_response::send_response;
use crate::ipc::server::PeerAddr;
use peko_auth::caller::CallerContext;

/// Narrow port the `system` handler uses to reach daemon state.
///
/// `AppState` is the sole implementor. Methods are async where the
/// underlying state is behind a lock; `uptime_seconds` and `cache_dir`
/// are pure reads and stay sync to keep `SystemHost` cheap to call.
#[async_trait]
pub(crate) trait SystemHost: Send + Sync {
    fn uptime_seconds(&self) -> u64;
    fn cache_dir(&self) -> PathBuf;
    async fn is_degraded(&self) -> bool;
    async fn is_ready(&self) -> bool;
    async fn instance_count(&self) -> u64;
    async fn tunnel_health(&self) -> crate::daemon::state::TunnelHealth;
    async fn request_shutdown(&self, force: bool);
    /// How this daemon was launched (`Sidecar` for peko-desktop's bundled
    /// child, `Headless` for any CLI invocation). Surfaced in
    /// `ResponsePacket::Status::mode` so peers can adopt a foreign
    /// daemon instead of spawning a competing child (ADR-043 adoption).
    /// Sync because the snapshot field is `Copy`.
    fn launch_mode(&self) -> crate::daemon::LaunchMode;
}

/// `system` domain request handler. Constructed with an `Arc<dyn SystemHost>`
/// (typically `Arc::new(app_state.clone())` from the dispatcher).
pub(crate) struct SystemHandler {
    host: Arc<dyn SystemHost>,
}

impl SystemHandler {
    pub(crate) fn new(host: Arc<dyn SystemHost>) -> Self {
        Self { host }
    }
}

#[async_trait]
impl RequestHandler for SystemHandler {
    fn domain(&self) -> &'static str {
        "system"
    }

    fn matches(&self, request: &RequestPacket) -> bool {
        matches!(
            request,
            RequestPacket::Ping { .. }
                | RequestPacket::Shutdown { .. }
                | RequestPacket::Status { .. }
                | RequestPacket::SystemStatus { .. }
                | RequestPacket::SystemDoctor { .. }
                | RequestPacket::SystemClean { .. }
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
            RequestPacket::Ping { request_id } => {
                let response = ResponsePacket::Pong {
                    request_id,
                    uptime_secs: self.host.uptime_seconds(),
                    version: crate::VERSION.to_string(),
                };
                send_response(sink, response).await?;
            }

            RequestPacket::Shutdown { request_id, force } => {
                self.host.request_shutdown(force).await;
                let response = ResponsePacket::ShuttingDown { request_id };
                send_response(sink, response).await?;
            }

            RequestPacket::Status { request_id } => {
                let health = self.host.tunnel_health().await;
                let response = ResponsePacket::Status {
                    request_id,
                    uptime_secs: self.host.uptime_seconds(),
                    version: crate::VERSION.to_string(),
                    tunnel_state: health.state_str().to_string(),
                    tunnel_reconnect_attempts: health.reconnect_attempts(),
                    tunnel_last_error: health.last_error().map(str::to_string),
                    degraded: self.host.is_degraded().await,
                    mode: Some(self.host.launch_mode()),
                };
                send_response(sink, response).await?;
            }

            RequestPacket::SystemStatus { request_id } => {
                let response = ResponsePacket::SystemStatus {
                    request_id,
                    version: crate::VERSION.to_string(),
                    uptime_secs: self.host.uptime_seconds(),
                    degraded: self.host.is_degraded().await,
                    instance_count: self.host.instance_count().await,
                    ready: self.host.is_ready().await,
                };
                send_response(sink, response).await?;
            }

            RequestPacket::SystemDoctor { request_id } => {
                let mut checks = Vec::new();

                let ready = self.host.is_ready().await;
                checks.push(DoctorCheck {
                    name: "daemon_ready".to_string(),
                    status: if ready { "pass" } else { "fail" }.to_string(),
                    message: if ready {
                        "Daemon is ready to serve requests".to_string()
                    } else {
                        "Daemon is not ready".to_string()
                    },
                    suggestion: if !ready {
                        Some("Check daemon logs for startup errors".to_string())
                    } else {
                        None
                    },
                });

                let degraded = self.host.is_degraded().await;
                checks.push(DoctorCheck {
                    name: "not_degraded".to_string(),
                    status: if !degraded { "pass" } else { "warn" }.to_string(),
                    message: if !degraded {
                        "Daemon is operating normally".to_string()
                    } else {
                        "Daemon is in degraded mode".to_string()
                    },
                    suggestion: if degraded {
                        Some("Check resource usage and consider restarting".to_string())
                    } else {
                        None
                    },
                });

                let uptime = self.host.uptime_seconds();
                checks.push(DoctorCheck {
                    name: "uptime".to_string(),
                    status: "pass".to_string(),
                    message: format!("Daemon uptime: {seconds} seconds", seconds = uptime),
                    suggestion: None,
                });

                let passed = checks.iter().filter(|c| c.status == "pass").count() as u32;
                let failed = checks.iter().filter(|c| c.status == "fail").count() as u32;
                let warnings = checks.iter().filter(|c| c.status == "warn").count() as u32;

                let response = ResponsePacket::SystemDoctor {
                    request_id,
                    checks,
                    passed,
                    failed,
                    warnings,
                };
                send_response(sink, response).await?;
            }

            RequestPacket::SystemClean { request_id, scope } => {
                let cache_dir = self.host.cache_dir();
                let mut cleaned = Vec::new();
                let mut bytes_freed: u64 = 0;

                let scope = scope.as_deref().unwrap_or("all");

                if (scope == "all" || scope == "cache") && cache_dir.exists() {
                    match std::fs::read_dir(&cache_dir) {
                        Ok(entries) => {
                            for entry in entries.flatten() {
                                let path = entry.path();
                                if let Ok(meta) = entry.metadata() {
                                    bytes_freed += meta.len();
                                }
                                if path.is_file() {
                                    let _ = std::fs::remove_file(&path);
                                    cleaned.push(path.to_string_lossy().to_string());
                                } else if path.is_dir() {
                                    let _ = std::fs::remove_dir_all(&path);
                                    cleaned.push(path.to_string_lossy().to_string());
                                }
                            }
                        }
                        Err(e) => {
                            let response = ResponsePacket::Error {
                                request_id,
                                message: format!("Failed to clean cache: {e}"),
                            };
                            send_response(sink, response).await?;
                            return Ok(());
                        }
                    }
                }

                let response = ResponsePacket::SystemCleaned {
                    request_id,
                    cleaned,
                    bytes_freed,
                };
                send_response(sink, response).await?;
            }

            // `matches()` returned true, so the exhaustive list above
            // covers every owned variant. This arm is unreachable.
            _ => unreachable!("SystemHandler::matches allowed an unhandled variant"),
        }
        Ok(())
    }
}
