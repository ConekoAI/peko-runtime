//! Per-session inbox of completed async tasks and user steering messages
//! waiting to be injected into the next agentic loop iteration.
//!
//! The canonical types (`CompletionEvent`, `SteeringMessage`,
//! `InboxItem`, `SessionInbox`) are defined in `peko_extension_api` and
//! `crate::extensions::framework::inbox`; this module re-exports them so
//! the historical
//! `crate::extensions::framework::async_exec::executor::*` import paths
//! keep resolving, and provides the `SharedSessionInbox` convenience
//! alias (`Arc<SessionInbox>`) for existing callers (e.g.
//! `AsyncExecutor::inbox_registry`, the `AsyncInboxAdapter` in
//! `src/engine/async_inbox_compat.rs`).

use std::sync::Arc;

pub use crate::extensions::framework::inbox::SessionInbox;
pub use peko_extension_api::{CompletionEvent, InboxItem, SteeringMessage};

/// Convenience alias preserved from the pre-Phase-2 root type:
///
/// ```text
/// pub type SharedSessionInbox = Arc<SessionInbox>;
/// ```
///
/// where `SessionInbox` is now `crate::extensions::framework::inbox::SessionInbox`.
/// Callers that held an `Arc<SharedSessionInbox>` (e.g.
/// `AsyncExecutor::inbox_registry`, `agentic_loop_compat` tests, the
/// `AsyncInboxAdapter` in `src/engine/async_inbox_compat.rs`) keep
/// working without a type rename.
pub type SharedSessionInbox = Arc<SessionInbox>;

#[cfg(test)]
mod tests {
    //! Verify the `SharedSessionInbox` alias still constructs from
    //! the canonical `inbox::SessionInbox`. The type-id assertions
    //! from the pre-Phase-8b root shim no longer apply because this
    //! module now lives in the same crate as `crate::extensions::framework::inbox::*`.
    use super::*;

    #[test]
    fn shared_session_inbox_is_arc_of_session_inbox() {
        let _shared: SharedSessionInbox = Arc::new(SessionInbox::new());
    }
}
