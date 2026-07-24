//! Async transport and execution infrastructure (Phase 8b shim)
//!
//! Phase 8b lifts `framework/transport/*` into the `peko_extension_host`
//! crate. The module + submodule names in root are kept as backwards-
//! compat shims that re-export from the new canonical home so historical
//! `crate::extensions::framework::transport::async_router::*` import
//! paths keep compiling until the framework tree is fully deleted in
//! Phase 8c.2.
//!
//! Phase 8c.1.D.6 relocated `create_transport` (the daemon-probing
//! factory) to `crate::ipc::create_transport`. The shim file under this
//! directory re-exports it for backwards compat.

// Concrete router (struct) lives at the new canonical home.
pub use peko_extension_host::transport::async_router::AsyncExecutionRouter;
pub use peko_extension_host::transport::async_router::ToolExecutionContext;
pub use peko_extension_host::transport::async_router::DEFAULT_TOOL_TIMEOUT_SECS;

// Transport implementations + factories.
pub use peko_extension_host::transport::async_transport::{
    create_local_transport, AsyncTaskTransport, BoxedExecutionFn, DaemonIpcTransport,
    LocalAsyncTransport, UnavailableAsyncTransport,
};

// Submodule shims so `transport::async_router::Foo` and
// `transport::async_transport::Bar` paths keep resolving for
// legacy callers that imported through the nested module.
pub mod async_router {
    //! Re-export of host-side router primitives.
    pub use peko_extension_host::transport::async_router::*;
}
pub mod async_transport {
    //! Re-export of host-side transport primitives.
    pub use peko_extension_host::transport::async_transport::*;
}

// Phase 8c.1.D.6: `create_transport` lives at `crate::ipc::create_transport`
// now. Re-exported here so historical `transport::create_transport()` calls
// (notably `services::new_auto()`) keep working until 8c.2.
pub use crate::ipc::create_transport::create_transport;
