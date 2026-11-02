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

use crate::ipc::handlers::RequestHandler;
use crate::ipc::packet::{RequestPacket, ResponsePacket};
use crate::ipc::response_sink::ResponseSink;
use crate::ipc::send_response::send_response;
use crate::ipc::server::PeerAddr;
use peko_auth::caller::CallerContext;

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

    /// Register with the Hub at `hub` as the account `token` signs in as,
    /// then (re)connect the tunnel.
    async fn register_runtime(
        &self,
        hub: &str,
        token: &str,
    ) -> anyhow::Result<crate::tunnel::registration::RuntimeRegistration>;

    /// Remove the Hub registration and close the tunnel.
    async fn unregister_runtime(&self, token: &str, discard_unsent: bool) -> anyhow::Result<()>;
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
            RequestPacket::TunnelStop { .. }
                | RequestPacket::TunnelStatus { .. }
                | RequestPacket::RuntimeRegister { .. }
                | RequestPacket::RuntimeUnregister { .. }
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
                    hub_url: crate::tunnel::credential::registered_hub_origin(),
                };
                send_response(sink, response).await?;
            }

            // Registration binds the runtime and its local user to a Hub
            // account: the local user's decision only (ADR-081 D3).
            RequestPacket::RuntimeRegister { request_id, .. }
            | RequestPacket::RuntimeUnregister { request_id, .. }
                if !caller.is_local() =>
            {
                let response = ResponsePacket::Error {
                    request_id,
                    message: "[permission_denied] only the local user registers this runtime"
                        .to_string(),
                };
                send_response(sink, response).await?;
            }

            RequestPacket::RuntimeRegister {
                request_id,
                hub,
                token,
            } => {
                let response = match self.host.register_runtime(&hub, &token).await {
                    Ok(registered) => ResponsePacket::RuntimeRegistered {
                        request_id,
                        hub: registered.origin,
                        runtime_did: registered.runtime_did,
                        account: registered.account,
                    },
                    Err(e) => ResponsePacket::Error {
                        request_id,
                        message: format!("{e:#}"),
                    },
                };
                send_response(sink, response).await?;
            }

            RequestPacket::RuntimeUnregister {
                request_id,
                token,
                discard_unsent,
            } => {
                let response = match self.host.unregister_runtime(&token, discard_unsent).await {
                    Ok(()) => ResponsePacket::Done {
                        request_id,
                        success: true,
                        error: None,
                    },
                    Err(e) => ResponsePacket::Error {
                        request_id,
                        message: format!("{e:#}"),
                    },
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Records registrations; never touches the network.
    #[derive(Default)]
    struct Host {
        registered: Mutex<Vec<(String, String)>>,
    }

    #[async_trait]
    impl TunnelHost for Host {
        async fn stop_tunnel(&self) {}
        async fn tunnel_connected(&self) -> bool {
            false
        }
        async fn register_runtime(
            &self,
            hub: &str,
            token: &str,
        ) -> anyhow::Result<crate::tunnel::registration::RuntimeRegistration> {
            self.registered
                .lock()
                .unwrap()
                .push((hub.to_string(), token.to_string()));
            Ok(crate::tunnel::registration::RuntimeRegistration {
                origin: hub.to_string(),
                runtime_did: "did:key:zRuntime".to_string(),
                account: "0b6c7a4e-0000-4000-8000-000000000000".to_string(),
            })
        }
        async fn unregister_runtime(
            &self,
            _token: &str,
            _discard_unsent: bool,
        ) -> anyhow::Result<()> {
            Ok(())
        }
    }

    struct Sink(Mutex<Vec<ResponsePacket>>);

    #[async_trait]
    impl ResponseSink for Sink {
        async fn send_bytes(&self, bytes: &[u8]) -> std::io::Result<()> {
            let packet = serde_json::from_slice(bytes).map_err(std::io::Error::other)?;
            self.0.lock().unwrap().push(packet);
            Ok(())
        }
    }

    async fn send(
        host: Arc<Host>,
        caller: CallerContext,
        request: RequestPacket,
    ) -> ResponsePacket {
        let sink = Sink(Mutex::new(Vec::new()));
        TunnelHandler::new(host)
            .handle(
                request,
                &caller,
                &sink,
                &PeerAddr::Ip("127.0.0.1:0".parse().unwrap()),
            )
            .await
            .unwrap();
        sink.0.into_inner().unwrap().pop().expect("a response")
    }

    /// pekohub ADR-007: the desktop registers its runtime with the token
    /// it signed in with, and gets the account back.
    #[tokio::test]
    async fn the_local_user_registers_the_runtime() {
        let host = Arc::new(Host::default());
        let response = send(
            host.clone(),
            CallerContext::local(),
            RequestPacket::RuntimeRegister {
                request_id: 7,
                hub: "https://api.pekohub.ai".into(),
                token: "access-token".into(),
            },
        )
        .await;
        match response {
            ResponsePacket::RuntimeRegistered {
                request_id,
                hub,
                account,
                ..
            } => {
                assert_eq!(request_id, 7);
                assert_eq!(hub, "https://api.pekohub.ai");
                assert!(!account.is_empty());
            }
            other => panic!("expected RuntimeRegistered, got {other:?}"),
        }
        assert_eq!(
            host.registered.lock().unwrap().as_slice(),
            &[(
                "https://api.pekohub.ai".to_string(),
                "access-token".to_string()
            )]
        );
    }

    /// ADR-081 D3: registering binds the local user to an account — no
    /// remote caller may do it.
    #[tokio::test]
    async fn a_remote_caller_cannot_register_the_runtime() {
        let host = Arc::new(Host::default());
        let response = send(
            host.clone(),
            CallerContext::from_jwt("someone".into()),
            RequestPacket::RuntimeRegister {
                request_id: 8,
                hub: "https://api.pekohub.ai".into(),
                token: "t".into(),
            },
        )
        .await;
        assert!(
            matches!(&response, ResponsePacket::Error { message, .. } if message.contains("permission_denied")),
            "{response:?}"
        );
        assert!(host.registered.lock().unwrap().is_empty());
    }
}
