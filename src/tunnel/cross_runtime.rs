//! Cross-runtime a2a dispatch context — shared bundle for the
//! outbound a2a path. Issue #29 Slice B (and follow-ups).
//!
//! Lives in `tunnel/` (not `tools/`) because both `extension` and
//! `tools` reference it, and `tools` already depends on
//! `extension`. Putting the type in `tunnel/cross_runtime` keeps the
//! dependency graph acyclic: both the bootstrap side
//! (`extension::core::ExtensionServices` holds the ctx as an
//! optional slot) and the consumer side
//! (`crate::tunnel::principal_send_tool::PrincipalSendTool`) import it
//! from here.

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

use ed25519_dalek::SigningKey;

use crate::principal::PrincipalManager;
use crate::tunnel::direct::DirectConnectionManager;
use crate::tunnel::hub_directory::AgentDirectory;
use crate::tunnel::known_runtimes::KnownRuntimes;
use crate::tunnel::{PendingA2aResponses, TunnelHandle};

/// Cross-runtime a2a dispatch context. Holds the dependencies the
/// outbound `principal_send` path needs: the directory client to resolve
/// the target, the pending registry to correlate the response, the
/// signing key for the envelope, the caller's runtime_id, the live
/// tunnel handle slot, the direct connection manager, the known-runtimes
/// registry for transport selection, and the per-call response timeout.
///
/// Built once at daemon-state startup (Slice B' / B+C) and held
/// behind an `Arc` so every per-agent `PrincipalSendTool` instance shares
/// the same registry, signing key, and tunnel slot.
pub struct CrossRuntimeA2aCtx {
    /// Directory client (`HubAgentDirectoryClient` in production,
    /// a `FakeAgentDirectory` in tests). The outbound path calls
    /// `resolve_by_did` / `resolve_by_handle` to learn where to
    /// send.
    pub directory: Arc<dyn AgentDirectory>,

    /// Response correlation registry. Shared with the inbound
    /// `AgentToAgentResponse` arm of the `TunnelDispatcher`.
    pub pending: Arc<PendingA2aResponses>,

    /// The runtime's own `PekoHubCredential` signing key. Used to
    /// sign the `AgentToAgentRequest` envelope so the target
    /// runtime can verify the caller's runtime identity end-to-end.
    pub signing_key: Arc<SigningKey>,

    /// The runtime's own `runtime_id` (did:key form). Echoed
    /// verbatim into the `caller_runtime_id` field of every
    /// outbound request.
    pub caller_runtime_id: String,

    /// Slot for the live outbound `TunnelHandle`. The
    /// `TunnelDispatcher` writes the freshest handle on every
    /// tunnel reconnect; the outbound path reads it under the
    /// lock. `None` means the tunnel is not currently connected,
    /// in which case the outbound path errors with a "tunnel not
    /// connected" message instead of trying to send on a stale
    /// handle.
    ///
    /// The slot is an `Arc<RwLock<...>>` (shared with the
    /// `TunnelDispatcher`'s handle-publisher) rather than a plain
    /// `TunnelHandle` so reconnects are visible without rebuilding
    /// the ctx.
    pub tunnel: Arc<RwLock<Option<TunnelHandle>>>,

    /// Manager for direct connections to peer runtimes. Used when
    /// transport selection chooses the direct path.
    pub direct_manager: Arc<DirectConnectionManager>,

    /// Local known-runtimes registry. Used to decide whether to use
    /// the PekoHub tunnel or a direct connection for a given peer.
    pub known_runtimes: Arc<RwLock<KnownRuntimes>>,

    /// Principal manager for the caller's runtime. Enables the local
    /// same-runtime shortcut in `principal_send`.
    pub principal_manager: Arc<PrincipalManager>,

    /// How long to wait for the matching `AgentToAgentResponse`
    /// before surfacing a `Timeout` error to the calling agent.
    /// Production default is 60s (configurable via daemon config
    /// in Slice B'); tests use sub-second values.
    pub response_timeout: Duration,
}

impl std::fmt::Debug for CrossRuntimeA2aCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CrossRuntimeA2aCtx")
            .field("directory", &"<dyn AgentDirectory>")
            .field("pending", &self.pending)
            .field("signing_key", &"<redacted: ed25519 SigningKey>")
            .field("caller_runtime_id", &self.caller_runtime_id)
            .field("tunnel", &self.tunnel)
            .field("direct_manager", &"<DirectConnectionManager>")
            .field("known_runtimes", &"<KnownRuntimes>")
            .field("principal_manager", &"<PrincipalManager>")
            .field("response_timeout", &self.response_timeout)
            .finish()
    }
}
