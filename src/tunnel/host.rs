//! Host port for the tunnel dispatcher (F5).
//!
//! Dependency-inversion seam: the tunnel (transport/protocol layer) must not
//! depend upward on `daemon` (the application shell). The dispatcher reaches
//! daemon services through this narrow trait; `daemon::state::AppState` is the
//! only type that implements it, and the dispatcher holds an
//! `Arc<dyn TunnelHost>`.
//!
//! The surface is exactly the operations the dispatcher needs, returned as
//! owned values so the trait is trivially object-safe. Boundary rule 9
//! (`src/tunnel/` must not import `crate::daemon`) keeps the seam from
//! regressing.

use std::sync::Arc;

use tokio::sync::RwLock;

use super::a2a_pending::PendingA2aResponses;
use super::TunnelHandle;
use crate::auth::jwt::JwtValidator;
use crate::observability::Observability;
use crate::principal::PrincipalManager;

/// Narrow host interface the tunnel dispatcher uses to reach daemon services.
///
/// Implemented only by `daemon::state::AppState`. Production and tests hand
/// the dispatcher an `Arc<dyn TunnelHost>`.
pub trait TunnelHost: Send + Sync {
    /// Principal manager used to list/lookup principals for announce + receive.
    fn principal_manager(&self) -> Arc<PrincipalManager>;

    /// This runtime's DID (used to derive stable instance IDs and audit tags).
    fn runtime_did(&self) -> String;

    /// Human-readable runtime display name for announce payloads.
    fn runtime_display_name(&self) -> String;

    /// Advertised direct endpoint, if one is configured and trusted.
    fn runtime_direct_endpoint(&self) -> Option<String>;

    /// JWT validator for verifying PekoHub-proxied caller identity.
    fn jwt_validator(&self) -> Option<JwtValidator>;

    /// Cross-runtime a2a response correlation registry.
    fn pending_a2a_responses(&self) -> Arc<PendingA2aResponses>;

    /// Observability handle for emitting audit events.
    fn observability(&self) -> Arc<Observability>;

    /// Slot the dispatcher writes the live outbound tunnel handle into on
    /// every inbound message, so the `CrossRuntimeA2aCtx` always sends on the
    /// freshest handle.
    fn tunnel_handle_slot(&self) -> Arc<RwLock<Option<TunnelHandle>>>;
}
