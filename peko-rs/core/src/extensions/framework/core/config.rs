//! Host service handles for channel, model, and cross-runtime adapters.

use std::sync::Arc;

/// Extension services available to hook handlers
///
/// This provides access to shared services like logging, configuration,
/// and other cross-cutting concerns.
pub struct ExtensionServices {
    // Sprint 9 Commit 4: `principal_message_service` slot retired.
    // `StatelessAgentService` was the sole
    // `PrincipalMessageService` impl; its only caller was the
    // chat-gateway adapter framework deleted in Commit 3. Per-peer
    // standing children (the agent-session paradigm) own principal
    // dispatch directly via `PrincipalManager::receive_streaming`.
    /// Cross-runtime a2a dispatch context (issue #29). Set by the
    /// daemon-state after the tunnel client is built and the
    /// `HubAgentDirectoryClient` is ready. `None` on runtimes that
    /// haven't run `peko tunnel setup` (no PekoHub credential) or
    /// are running offline.
    ///
    /// Stored as `Arc<dyn Any + Send + Sync>` so the framework does
    /// not depend on the concrete `tunnel::CrossRuntimeA2aCtx` type.
    /// Consumers downcast to the concrete type when building tools.
    cross_runtime_a2a_ctx:
        std::sync::RwLock<Option<Arc<dyn std::any::Any + Send + Sync + 'static>>>,

    /// Runtime LLM resolver. Set by AppState once the resolver is built
    /// so that extension code (e.g. MCP sampling) can request host-model
    /// completions without holding provider-specific state.
    llm_resolver: std::sync::RwLock<Option<Arc<peko_providers::LlmResolver>>>,

    /// Channel port (sprint 4 — `ChannelSend` per-agent tool needs the
    /// file-backed `ChannelPort` so the bare / group / principal branches
    /// can post to channels. Set by AppState once the channel store is
    /// wired; `None` on tests that construct an `ExtensionServices` via
    /// `new()` without a real channel store.
    channel_port: std::sync::RwLock<Option<Arc<dyn peko_channel::ChannelPort>>>,
}

impl std::fmt::Debug for ExtensionServices {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtensionServices")
            .field(
                "cross_runtime_a2a_ctx",
                &"<RwLock<Option<Arc<dyn Any + Send + Sync>>>>",
            )
            .field("llm_resolver", &"<RwLock<Option<Arc<LlmResolver>>>>")
            .field("channel_port", &"<RwLock<Option<Arc<dyn ChannelPort>>>>")
            // Sprint 9 Commit 4: `principal_message_service` field
            // removed; Debug formatter no longer emits it.
            .finish_non_exhaustive()
    }
}

impl ExtensionServices {
    /// Create new extension services (ADR-066 P3: the async-router slot
    /// moved to the `ToolDispatcher`; `ExtensionServices` now carries
    /// only the hook-context slots: channel port, cross-runtime ctx).
    #[must_use]
    pub fn new() -> Self {
        Self {
            // Issue #29: cross-runtime a2a ctx starts as None and
            // is filled in by the daemon-state after the tunnel
            // client is wired. Until then, every per-agent
            // PrincipalSendTool is built without a ctx and falls back to
            // the local-only path (the same behavior as pre-#29).
            cross_runtime_a2a_ctx: std::sync::RwLock::new(None),
            llm_resolver: std::sync::RwLock::new(None),
            channel_port: std::sync::RwLock::new(None),
        }
    }

    // Sprint 9 Commit 4: `set_principal_message_service` and
    // `principal_message_service` getter retired along with the
    // slot. Per-peer standing children (the agent-session paradigm)
    // own principal dispatch via `PrincipalManager::receive_streaming`.

    /// Set the cross-runtime a2a dispatch context (issue #29). The
    /// daemon-state calls this after the tunnel client is built and
    /// the `HubAgentDirectoryClient` is wired; the per-agent tool
    /// constructor in `agent.rs` reads via `cross_runtime_a2a_ctx`
    /// and injects the ctx into each `PrincipalSendTool` it builds.
    pub fn set_cross_runtime_a2a_ctx(&self, ctx: Arc<dyn std::any::Any + Send + Sync + 'static>) {
        if let Ok(mut guard) = self.cross_runtime_a2a_ctx.write() {
            *guard = Some(ctx);
        }
    }

    /// Get the cross-runtime a2a dispatch context, if one is set.
    /// Returns `None` on runtimes that haven't initialized
    /// cross-runtime dispatch (offline runtimes, runtimes without
    /// a PekoHub credential, runtimes before this PR's bootstrap
    /// follow-up).
    #[must_use]
    pub fn cross_runtime_a2a_ctx(&self) -> Option<Arc<dyn std::any::Any + Send + Sync + 'static>> {
        self.cross_runtime_a2a_ctx
            .read()
            .ok()
            .and_then(|g| g.clone())
    }

    /// Set the runtime LLM resolver. Called by AppState once the resolver
    /// has been constructed.
    pub fn set_llm_resolver(&self, resolver: Arc<peko_providers::LlmResolver>) {
        if let Ok(mut guard) = self.llm_resolver.write() {
            *guard = Some(resolver);
        }
    }

    /// Get the runtime LLM resolver, if one has been set.
    #[must_use]
    pub fn llm_resolver(&self) -> Option<Arc<peko_providers::LlmResolver>> {
        self.llm_resolver.read().ok().and_then(|g| g.clone())
    }

    /// Set the channel port. Called by AppState once the channel store
    /// has been wired (the same handle that `PrincipalManager::channel_port`
    /// already caches). The per-agent `ChannelSendTool` constructor in
    /// `agent.rs` reads via `channel_port` so the bare / group / principal
    /// branches can post to channels.
    pub fn set_channel_port(&self, port: Arc<dyn peko_channel::ChannelPort>) {
        if let Ok(mut guard) = self.channel_port.write() {
            *guard = Some(port);
        }
    }

    /// Get the channel port, if one is installed.
    #[must_use]
    pub fn channel_port(&self) -> Option<Arc<dyn peko_channel::ChannelPort>> {
        self.channel_port.read().ok().and_then(|g| g.clone())
    }
}

impl Default for ExtensionServices {
    fn default() -> Self {
        Self::new()
    }
}
