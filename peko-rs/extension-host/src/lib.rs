//! `peko-extension-host` — Extension framework host (Phase 8).
//!
//! Phase 8a moves the bulk of `src/extensions/framework/` into this
//! crate: the `core` registry tree (`ExtensionCore`, `HookRegistry`,
//! `ToolRegistry`, hook points/handlers/config), the `types` data
//! tree (capabilities, hook IO, manifest, etc.), the global
//! `ExtensionStore`, the `skill_catalog`, the small `integration`
//! bridge, and the `scaffold` template engine.
//!
//! Phase 8b will move `framework/manager` + `framework/async_exec` +
//! `framework/transport` (3,500 lines). Phase 8c will move
//! `framework/services` + `framework/protocols` + `framework/adapters`
//! (2,800 lines). After 8c, `src/extensions/framework/` is empty and
//! gets deleted entirely.
//!
//! Trait contracts already present pre-8a:
//! - [`inbox::SessionInbox`] / [`inbox::InboxSinkProvider`] — async
//!   task inbox sinks that peko-session's `InboxRegistry` implements
//! - [`transport::DaemonTransport`] / [`transport::DaemonResponse`] —
//!   IPC transport projection for host-side async dispatch
//! - [`principal_message::PrincipalMessageService`] — A2A
//!   inter-principal messaging port
//! - [`vault::VaultAccess`] — credential vault access port
//! - [`subagent::SpawnCleanupPolicy`] — subagent cleanup policy enum
//! - [`tool_funnel::ToolFunnel`] — F37 `execute_tool_via_core` funnel
//! - [`registry::SimpleRegistry`] / [`registry::SharedRegistry`] —
//!   generic registry utilities
//!
//! Forbidden deps: peko-engine, peko-agents, peko-session, root.
//! Enforced via `scripts/check_workspace_deps.py`.

// Framework bulk-moved in 8a.
// (store.rs and core/async_bridge.rs stay in root until 8b/8c lift
// their cross-subtree deps; see the 8a plan.)
pub mod core;
pub mod integration;
pub mod scaffold;
pub mod skill_catalog;
// Phase 8c.1.D.2: ExtensionStore trait port + the data types its
// methods return. The concrete `ExtensionStore` impl stays in root
// (it depends on root-only adapters + storage + ExtensionCore); the
// host owns the trait contract + pure-data types.
pub mod store;
pub mod tool_funnel_impl;
pub mod types;

// Phase 8b lift: framework/{async_exec,manager,services,protocols} +
// the implementations under framework/transport/* moved into the host.
// `transport.rs` (the trait contract from 8a) is the parent module;
// `transport/{async_router,async_transport}.rs` are its submodules.
pub mod async_exec;
pub mod manager;
pub mod protocols;
pub mod services;

// Trait contracts from 8a commits 1 + 2.
pub mod inbox;
pub mod paths;
pub mod principal_message;
pub mod registry;
pub mod subagent;
pub mod tool_funnel;
pub mod transport;
pub mod vault;

// Re-export peko-extension-api surface for callers that want
// `peko_extension_host::api::*`.
pub use peko_extension_api as api;

// Re-exports from the moved framework/ subtrees (mirrors the
// pre-8a `crate::extensions::framework::*` public surface).
pub use core::{
    binding::{HookBinding, HookBindingBuilder},
    common,
    config::{ExtensionConfig, ExtensionServices, TelemetryService},
    context::{HookContext, HookState},
    handler::{HookHandler, HookHandlerFactory},
    hook_points::{HookPoint, HookPointBuilder},
    registry::{global_core, init_global_core, ExtensionCore, RegisteredHook},
};
pub use types::{
    tool_result_from_hook, ActiveExtensionSet, AsyncReceipt, Capabilities, Capability, ExtensionId,
    ExtensionManifest, HookId, HookInput, HookOutput, HookPriority, HookResult, MessageEnvelope,
    PromptBuildState, SessionSnapshot, ToolMetadata, ToolRegistryAccess, ToolSource,
    DEFAULT_HOOK_PRIORITY, FALLBACK_HOOK_PRIORITY, SYSTEM_HOOK_PRIORITY, USER_HOOK_PRIORITY,
};

// Convenience re-exports at the crate root.
pub use inbox::{
    CompletionEvent, InboxItem, InboxSinkProvider, InboxSinkRegistry, SessionInbox,
    SessionInboxSink, SteeringMessage,
};
pub use paths::{default_agent_workspace, default_async_tasks_dir, default_data_dir, PathResolver};
pub use principal_message::{
    PrincipalMessageRequest, PrincipalMessageResponse, PrincipalMessageService, ToolCallInfo,
};
pub use registry::{SharedRegistry, SimpleRegistry};
pub use subagent::SpawnCleanupPolicy;
pub use tool_funnel::ToolFunnel;
pub use transport::{
    async_transport::{
        create_local_transport, create_transport_with, AsyncTaskTransport, BoxedExecutionFn,
        DaemonIpcTransport, LocalAsyncTransport, UnavailableAsyncTransport,
    },
    DaemonTransport, ExecFn, PreprocessorFn, ToolExecConfig,
};
pub use vault::VaultAccess;

// Prelude for convenient imports.
pub mod prelude {
    pub use crate::core::{
        common, ExtensionCore, HookContext, HookHandler, HookPoint, HookPointBuilder,
    };
    pub use crate::types::{
        ExtensionId, ExtensionManifest, HookId, HookInput, HookOutput, HookResult,
    };
}
