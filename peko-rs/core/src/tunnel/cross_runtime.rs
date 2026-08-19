//! Cross-runtime dispatch context for the `send_peer` tool — the
//! shared bundle for the outbound principal-to-principal path.
//!
//! Sprint 3 Phase 12b: principal-to-principal DM runs over **channels**
//! now — the retired RPC stack (signed `PrincipalToPrincipalRequest`
//! envelopes, the pending-response registry, the direct transport) is
//! gone, and with it the signing key / pending registry / tunnel slot /
//! direct manager / known-runtimes / chat-log fields this ctx used to
//! carry. What remains is exactly what the channel-based path needs:
//!
//! - the **directory** (which runtime hosts the target principal),
//! - the caller's own **runtime id** (same-runtime detection),
//! - the **principal manager** (peer-child + DM-channel provisioning
//!   via `ensure_peer_child_ingress`),
//! - the concrete **`TunnelChannelPort`** (DM channel posts, the reply
//!   broadcast subscription, remote-member reads, and the
//!   `fanout_dm_invite` first-contact invite), and
//! - the per-call **response timeout**.
//!
//! Lives in `tunnel/` (not `tools/`) because both `extension` and
//! `tools` reference it, and `tools` already depends on
//! `extension`. Putting the type in `tunnel/cross_runtime` keeps the
//! dependency graph acyclic: both the bootstrap side
//! (`extension::core::ExtensionServices` holds the ctx as an
//! optional slot) and the consumer side
//! (`crate::tunnel::principal_send_tool::SendPeerTool`) import it
//! from here.

use std::sync::Arc;
use std::time::Duration;

use crate::principal::PrincipalManager;
use crate::tunnel::hub_directory::AgentDirectory;
use crate::tunnel::TunnelChannelPort;

/// Cross-runtime dispatch context for `send_peer`. Built once at
/// daemon-state startup and held behind an `Arc` so every per-agent
/// `SendPeerTool` instance shares the same directory, manager, and
/// channel port.
pub struct CrossRuntimeA2aCtx {
    /// Directory client (`HubAgentDirectoryClient` in production,
    /// a `FakeAgentDirectory` in tests). The outbound path calls
    /// `resolve_by_did` / `resolve_by_handle` to learn which runtime
    /// hosts the target principal.
    pub directory: Arc<dyn AgentDirectory>,

    /// The runtime's own `runtime_id` (did:key form). A resolution
    /// whose `runtime_id` matches takes the same-runtime (two-channel
    /// local) branch; anything else takes the cross-runtime branch.
    pub caller_runtime_id: String,

    /// Principal manager for the caller's runtime. Used to resolve
    /// principals by DID and to find-or-create the peer standing
    /// child + DM channel (`ensure_peer_child_ingress`).
    pub principal_manager: Arc<PrincipalManager>,

    /// The daemon's concrete channel port. The tool needs the
    /// concrete type (not `Arc<dyn ChannelPort>`) for the DM-specific
    /// surface the trait doesn't carry: `fanout_dm_invite` (the
    /// first-contact invite takes the caller's real DID, which the
    /// bare `ChannelPort::invite` path cannot resolve) and
    /// `local().list_remote_members` (first-contact detection).
    pub channel_port: Arc<TunnelChannelPort>,

    /// How long to wait for the peer's reply post on the DM channel
    /// before surfacing a timeout error to the calling agent.
    /// Production default is 60s; tests use sub-second values.
    pub response_timeout: Duration,
}

impl std::fmt::Debug for CrossRuntimeA2aCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CrossRuntimeA2aCtx")
            .field("directory", &"<dyn AgentDirectory>")
            .field("caller_runtime_id", &self.caller_runtime_id)
            .field("principal_manager", &"<PrincipalManager>")
            .field("channel_port", &self.channel_port)
            .field("response_timeout", &self.response_timeout)
            .finish()
    }
}
