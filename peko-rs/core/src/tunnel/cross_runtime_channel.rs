//! Cross-runtime channel dispatch context.
//!
//! `peko-channel` cross-runtime PR-A commit 2. Holds the dependencies
//! the outbound channel-event sender needs. Mirrors
//! [`crate::tunnel::cross_runtime::CrossRuntimeA2aCtx`] but is
//! trimmed for the channel use case:
//!
//! - **No `pending` field.** Channel events are pure push (N-way
//!   fan-out, no per-request response). There is no response
//!   correlation registry — the source runtime queues a
//!   `TunnelChannelEvent` for each remote member's runtime and
//!   considers the send done.
//! - **No `principal_manager` or `chat_log_store` in this
//!   commit.** The outbound send path is owned by
//!   `peko-channel`'s `TunnelChannelPort` (added in PR-B), which
//!   has its own `ChannelStore` for local persistence and does not
//!   need to plumb through the principal manager. Adding them here
//!   would split the responsibility — the channel port already
//!   holds the principal that authored the event as part of the
//!   local `ChannelStore` write.
//!
//! Like `CrossRuntimeA2aCtx`, this is built once at daemon-state
//! startup and held behind an `Arc` so every per-channel
//! `TunnelChannelPort` instance shares the same signing key, tunnel
//! slot, and known-runtimes registry.

use std::sync::Arc;
use tokio::sync::RwLock;

use ed25519_dalek::SigningKey;

use crate::tunnel::hub_directory::AgentDirectory;
use crate::tunnel::known_runtimes::KnownRuntimes;
use crate::tunnel::TunnelHandle;

/// Cross-runtime channel dispatch context. Holds the dependencies
/// the outbound channel-event send path needs: the directory client
/// to resolve the runtime of each remote channel member, the
/// signing key for the envelope, the local runtime's `runtime_id`
/// (echoed into every outbound `source_runtime_id`), the live tunnel
/// handle slot, and the known-runtimes registry used for transport
/// selection (tunnel vs. direct).
///
/// Built once at daemon-state startup and held behind an `Arc` so
/// every per-channel `TunnelChannelPort` instance shares the same
/// signing key, tunnel slot, and known-runtimes registry.
pub struct CrossRuntimeChannelCtx {
    /// Directory client (`HubAgentDirectoryClient` in production,
    /// a `FakeAgentDirectory` in tests). The outbound send path
    /// calls `resolve_by_did` to learn which runtime hosts each
    /// remote channel member.
    pub directory: Arc<dyn AgentDirectory>,

    /// The runtime's own `PekoHubCredential` signing key. Used to
    /// sign every outbound `TunnelChannelEvent` envelope so the
    /// remote runtime can verify the source runtime identity
    /// end-to-end.
    pub signing_key: Arc<SigningKey>,

    /// The runtime's own `runtime_id` (did:key form). Echoed
    /// verbatim into the `source_runtime_id` field of every
    /// outbound `TunnelChannelEvent`.
    pub caller_runtime_id: String,

    /// Slot for the live outbound `TunnelHandle`. The
    /// `TunnelDispatcher` writes the freshest handle on every
    /// tunnel reconnect; the outbound send path reads it under the
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

    /// Local known-runtimes registry. Used to decide whether to
    /// use the PekoHub tunnel or a direct connection for a given
    /// peer runtime. (Direct-LAN support is deferred per the
    /// cross-runtime plan; the field is present now so PR-B's
    /// `TunnelChannelPort` does not need to re-plumb the registry
    /// when direct-LAN lands.)
    pub known_runtimes: Arc<RwLock<KnownRuntimes>>,
}

impl std::fmt::Debug for CrossRuntimeChannelCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CrossRuntimeChannelCtx")
            .field("directory", &"<dyn AgentDirectory>")
            .field("signing_key", &"<redacted: ed25519 SigningKey>")
            .field("caller_runtime_id", &self.caller_runtime_id)
            .field("tunnel", &self.tunnel)
            .field("known_runtimes", &self.known_runtimes)
            .finish()
    }
}