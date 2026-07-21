//! `peko-extension-host` — Phase 8 trait contracts and `common::registry`
//! home.
//!
//! Phase 8 is split across two commits:
//!
//! - **Commit 1 (this crate's initial state):** Define the
//!   cross-boundary traits that the root facade uses to wire the
//!   host's facilities without leaking root-only types into the
//!   host crate:
//!
//!   - [`inbox::SessionInboxSink`] / [`inbox::InboxSinkProvider`] /
//!     [`inbox::InboxSinkRegistry`] — the host executor looks up an
//!     `Arc<dyn SessionInboxSink>` and pushes completion events.
//!     Root's richer `session::InboxRegistry` (which also tracks run
//!     permits) implements `InboxSinkProvider` so the daemon can
//!     wire the executor against it without creating a cycle.
//!   - [`transport::DaemonTransport`] / [`transport::DaemonResponse`] —
//!     the host IPC transport depends only on this trait; root's
//!     `ipc::DaemonClient` implements it and is the production
//!     adapter. (The full transport trait surface —
//!     `AsyncTaskTransport` — stays in root because the concrete
//!     `create_transport` factory depends on
//!     `crate::ipc::ConnectionManager`. Phase 8 commit 2 will move
//!     it once the factory's root-coupling is lifted.)
//!
//!   Phase 8 commit 1 also moves [`registry::SimpleRegistry`] /
//!   [`registry::SharedRegistry`] out of `src/common/registry.rs`
//!   into this crate, with a root shim that re-exports for
//!   backwards compatibility until Phase 10 deletes it.
//!
//! - **Commit 2:** Define the remaining service traits
//!   (`LlmResolver`, `PrincipalMessageService`, `VaultAccess`,
//!   `PathResolver`) and move `SpawnCleanupPolicy` so the
//!   framework's other root couplings (which the Phase 8 audit
//!   surfaced) can be lifted. Then bulk-move the framework host
//!   files (registry, async executor, transport, manager, scaffold,
//!   services, integration, adapters, protocols, store, skill
//!   catalog) into this crate, replacing each with a 1-line root
//!   shim re-export.

pub mod inbox;
pub mod paths;
pub mod registry;
pub mod transport;
pub mod types;

// Re-export peko-extension-api surface for callers that want
// `peko_extension_host::api::*`.
pub use peko_extension_api as api;

// Convenience re-exports at the crate root.
pub use inbox::{
    CompletionEvent, InboxItem, InboxSinkProvider, InboxSinkRegistry, SessionInbox,
    SessionInboxSink, SteeringMessage,
};
pub use paths::default_async_tasks_dir;
pub use registry::{SharedRegistry, SimpleRegistry};
pub use transport::{DaemonResponse, DaemonResponseStream, DaemonTransport};
