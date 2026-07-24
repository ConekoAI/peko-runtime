//! Phase 8c.1.D.3 + D.7 shim layer.
//!
//! After Phase 8c.1.D.3, the `Services` orchestrator struct, `config_service`,
//! and `tool_execution` were lifted into `peko_extension_host::services::*`.
//! This file keeps the historical public surface so callers don't migrate
//! until Phase 8c.2's path sweep.
//!
//! Phase 8c.2 will delete this whole module + its child files.

pub mod config_service;
pub mod reserved_params;
pub mod tool_execution;

// Re-export transport modules for backward compatibility
pub use crate::extensions::framework::transport::async_router;
pub use crate::extensions::framework::transport::async_transport;

// Re-export transport types for backward compatibility (now sourced from
// the canonical `peko_extension_host::transport::*` paths; the legacy root
// shim at `crate::extensions::framework::transport` remains until 8c.2).
pub use peko_extension_host::transport::async_router::{
    AsyncExecutionRouter, ToolExecutionContext, DEFAULT_TOOL_TIMEOUT_SECS,
};
pub use peko_extension_host::transport::async_transport::{
    create_local_transport, AsyncTaskTransport, DaemonIpcTransport, LocalAsyncTransport,
    UnavailableAsyncTransport,
};
// `create_transport` lives at the parent `transport::*` namespace (root-side
// shim that probes the daemon + delegates to host crate). Phase 8c.1.D.6
// will relocate it to `src/ipc/create_transport.rs`.
pub use crate::extensions::framework::transport::create_transport;

pub use config_service::{ConfigScope, ExtensionConfigService};
pub use reserved_params::{ParamSource, ReservedParamsConfig, ReservedParamsService};
pub use tool_execution::{ToolExecutionConfig, ToolExecutionService};

// `Services` orchestrator moved to `peko_extension_host::services::Services`
// in Phase 8c.1.D.3. Re-exported here so historical import paths like
// `crate::extensions::framework::services::Services` keep compiling.
pub use peko_extension_host::services::Services;
