//! Cross-runtime dispatch context for the ChannelSend tool — the
//! shared bundle for the outbound principal-to-principal path.
//! (Sprint 4: this used to be the `send_peer` tool's context;
//! ChannelSend's `principal:<did>` branch reuses it 1:1.)
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
//! (`crate::tools::builtin::channel::ChannelSendTool` — sprint 4
//!   unified tool, principal branch) import it
//! from here.

use std::sync::Arc;

use crate::principal::PrincipalManager;
use crate::tunnel::hub_directory::AgentDirectory;
use crate::tunnel::TunnelChannelPort;

/// Cross-runtime dispatch context for `ChannelSend` (principal branch).
/// Published by daemon tunnel setup behind an `Arc`. Principal-scoped
/// ChannelSend adapters read the current service slot per invocation.
pub struct CrossRuntimeA2aCtx {
    /// Directory client (`HubAgentDirectoryClient` in production,
    /// a `FakeAgentDirectory` in tests). The outbound path calls
    /// `resolve_by_hub_id` / `resolve_by_handle` to learn which runtime
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
}

impl CrossRuntimeA2aCtx {
    /// A directory's foreign storage ID is metadata, not a local identity.
    /// Same-runtime claims must identify an actual hosted principal.
    pub(crate) async fn actor_subject(
        &self,
        resolved: &super::hub_directory::AgentResolution,
    ) -> anyhow::Result<peko_subject::Subject> {
        if resolved.runtime_id != self.caller_runtime_id {
            return Ok(peko_subject::Subject::Principal(
                format!("hub:{}", resolved.hub_id).into(),
            ));
        }
        let principal = self
            .principal_manager
            .get(peko_subject::PrincipalId(resolved.local_id.clone()))
            .await
            .ok_or_else(|| anyhow::anyhow!("Directory returned an unknown local principal"))?;
        let config = principal.config.read().await;
        anyhow::ensure!(
            config.did.as_ref().map(|did| did.0.as_str()) == Some(resolved.agent_did.as_str())
                && config.actor_key.as_deref() == Some(resolved.identity_key.as_str()),
            "Directory local identity does not match the hosted principal"
        );
        Ok(peko_subject::Subject::from(&principal.id))
    }
}

impl std::fmt::Debug for CrossRuntimeA2aCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CrossRuntimeA2aCtx")
            .field("directory", &"<dyn AgentDirectory>")
            .field("caller_runtime_id", &self.caller_runtime_id)
            .field("principal_manager", &"<PrincipalManager>")
            .field("channel_port", &self.channel_port)
            .finish()
    }
}
