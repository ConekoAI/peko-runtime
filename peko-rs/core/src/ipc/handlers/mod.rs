//! Per-domain request handlers (F6).
//!
//! The monolithic `handle_request` match in `ipc::server` has been
//! fully decomposed by IPC packet domain. Every `RequestPacket`
//! variant is owned by exactly one domain handler behind a
//! [`RequestHandler`] trait; [`RequestDispatcher::dispatch`] walks
//! the registered handlers in order and the first match wins.
//!
//! Boundary rule (F6): a handler module must not import another handler
//! module — domains are independent. Handler modules also must not
//! import `crate::daemon::state::AppState`; they receive narrow host
//! traits (e.g. [`system::SystemHost`]) defined alongside the handler.

use std::sync::Arc;

use async_trait::async_trait;
use tracing::trace;

use crate::daemon::state::AppState;
use crate::ipc::packet::{RequestPacket, ResponsePacket};
use crate::ipc::response_sink::ResponseSink;
use crate::ipc::send_response::send_response;
use crate::ipc::server::PeerAddr;
use peko_auth::caller::CallerContext;

pub(crate) mod auth;
pub(crate) mod capability;
pub(crate) mod credential;
pub(crate) mod cron;
pub(crate) mod ext_runtime;
pub(crate) mod extension;
pub(crate) mod instance;
pub(crate) mod principal;
pub(crate) mod provider_add;
pub(crate) mod provider_edit;
pub(crate) mod provider_mcp;
pub(crate) mod provider_templates;
pub(crate) mod quota;
pub(crate) mod runtime;
pub(crate) mod system;
pub(crate) mod tool;
pub(crate) mod tunnel;

use auth::AuthHandler;
use capability::CapabilityHandler;
use credential::CredentialHandler;
use cron::CronHandler;
use ext_runtime::ExtRuntimeHandler;
use extension::ExtensionHandler;
use instance::InstanceHandler;
use principal::PrincipalHandler;
use provider_add::ProviderAddHandler;
use provider_edit::ProviderEditHandler;
use provider_mcp::ProviderMcpHandler;
use provider_templates::ProviderTemplatesHandler;
use quota::QuotaHandler;
use runtime::RuntimeHandler;
use system::SystemHandler;
use tool::ToolHandler;
use tunnel::TunnelHandler;

/// A per-domain IPC request handler.
///
/// Implementors own their own dependencies (captured in `self`) so the
/// dispatcher can hold them as `Arc<dyn RequestHandler>` and reach them
/// without knowing the concrete state type.
#[async_trait]
pub(crate) trait RequestHandler: Send + Sync {
    /// Short, human-readable domain name (for logging/debug).
    fn domain(&self) -> &'static str;

    /// `true` iff this handler owns `request` and should handle it.
    /// The dispatcher uses this to route without an explicit variant
    /// table; returning `false` falls through to the next handler.
    fn matches(&self, request: &RequestPacket) -> bool;

    /// Handle `request` and emit zero or more responses via `sink`.
    /// On success, returns `Ok(())` even if the handler emitted an error
    /// response packet.
    async fn handle(
        &self,
        request: RequestPacket,
        caller: &CallerContext,
        sink: &dyn ResponseSink,
        peer: &PeerAddr,
    ) -> anyhow::Result<()>;
}

/// Central dispatcher for every `RequestPacket` variant.
///
/// Constructed once per request from a shared `AppState` clone and used
/// by `IpcServer::handle_request` as the single dispatch entrypoint.
/// The legacy monolithic match in `server.rs` is retired — every
/// variant is now routed through the table below.
///
/// Handler priority is structural: first match wins. The order is
/// arbitrary but stable; the daemon never depends on it for correctness
/// (each handler's `matches` partition must be disjoint — see the
/// per-handler `matches()` impls).
pub(crate) struct RequestDispatcher;

impl RequestDispatcher {
    /// Dispatch a single `request` to the first matching handler.
    ///
    /// On a clean match the handler runs to completion and returns its
    /// result. If no handler claims the variant, the dispatcher emits a
    /// single `ResponsePacket::Error` carrying the request id and a
    /// stable `no handler registered` message — the only way to reach
    /// this branch is to add a new `RequestPacket` variant without
    /// either registering a handler or widening an existing
    /// `matches()` claim.
    pub(crate) async fn dispatch(
        state: AppState,
        request: RequestPacket,
        caller: &CallerContext,
        sink: &dyn ResponseSink,
        peer: &PeerAddr,
    ) -> anyhow::Result<()> {
        let host = Arc::new(state);
        let handlers: [Arc<dyn RequestHandler>; 17] = [
            Arc::new(SystemHandler::new(host.clone())),
            Arc::new(AuthHandler::new(host.clone())),
            Arc::new(ToolHandler::new(host.clone())),
            Arc::new(TunnelHandler::new(host.clone())),
            Arc::new(CapabilityHandler::new(host.clone())),
            Arc::new(InstanceHandler::new(host.clone())),
            Arc::new(ExtRuntimeHandler::new(host.clone())),
            Arc::new(CronHandler::new(host.clone())),
            Arc::new(RuntimeHandler::new(host.clone())),
            Arc::new(ExtensionHandler::new(host.clone())),
            Arc::new(ProviderMcpHandler::new(host.clone())),
            Arc::new(QuotaHandler::new(host.clone())),
            Arc::new(CredentialHandler::new(host.clone(), host.clone())),
            Arc::new(PrincipalHandler::new(host.clone())),
            // T-109b: `ModelTemplates` + `ModelAdd` are the
            // IPC seam for the desktop's "Add Model" modal. They
            // sit adjacent to `ProviderMcp` (catalog/reload) so all
            // model-mutation variants are colocated in the
            // dispatch table.
            Arc::new(ProviderTemplatesHandler::new(host.clone())),
            Arc::new(ProviderAddHandler::new(host.clone())),
            // RP6: model update / remove / test live next to
            // the add handler so the whole catalog-mutation surface is
            // routed as one group.
            Arc::new(ProviderEditHandler::new(host)),
        ];

        for handler in &handlers {
            if handler.matches(&request) {
                trace!("dispatching to {} handler", handler.domain());
                return handler.handle(request, caller, sink, peer).await;
            }
        }

        // Defense-in-depth: no handler claimed the variant. Surface a
        // protocol-level error to the caller rather than dropping the
        // packet silently. In normal operation (every existing variant
        // is claimed) this branch is unreachable.
        let request_id = request.request_id();
        let response = ResponsePacket::Error {
            request_id,
            message: format!("no handler registered for request variant (request_id={request_id})"),
        };
        send_response(sink, response).await
    }
}
