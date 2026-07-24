//! Phase 8c.1.D.6 deletion notice.
//!
//! The daemon-probing factory previously lived here. It was relocated to
//! `crate::ipc::create_transport` in Phase 8c.1.D.6 so it sits next to
//! the `DaemonClient` it probes. This file is kept as a backwards-compat
//! shim that re-exports from the new location so historical
//! `crate::extensions::framework::transport::create_transport_shim::*`
//! paths keep compiling until 8c.2 deletes this whole module tree.

pub use crate::ipc::create_transport::*;