//! Per-domain request handlers (F6).
//!
//! The monolithic `handle_request` match in `ipc::server` is being
//! decomposed by IPC packet domain. Each domain lives in its own module
//! behind a [`RequestHandler`] trait, and the server tries registered
//! handlers first, falling through to the legacy match for variants that
//! have not yet been migrated.
//!
//! Boundary rule (F6): a handler module must not import another handler
//! module — domains are independent. Handler modules also must not
//! import `crate::daemon::state::AppState`; they receive narrow host
//! traits (e.g. [`system::SystemHost`]) defined alongside the handler.

use async_trait::async_trait;

use crate::auth::caller::CallerContext;
use crate::ipc::packet::RequestPacket;
use crate::ipc::response_sink::ResponseSink;
use crate::ipc::server::PeerAddr;

pub(crate) mod auth;
pub(crate) mod system;

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
    /// table; returning `false` falls through to the next handler (and
    /// ultimately the legacy match).
    fn matches(&self, request: &RequestPacket) -> bool;

    /// Handle `request` and emit zero or more responses via `sink`.
    /// On success, returns `Ok(())` even if the handler emitted an error
    /// response packet (mirrors the legacy match's contract).
    async fn handle(
        &self,
        request: RequestPacket,
        caller: &CallerContext,
        sink: &dyn ResponseSink,
        peer: &PeerAddr,
    ) -> anyhow::Result<()>;
}