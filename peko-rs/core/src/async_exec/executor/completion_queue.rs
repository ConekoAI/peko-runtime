//! Completion queue compatibility surface for session-owned events and the host inbox.

use std::sync::Arc;

pub use crate::async_exec::inbox::SessionInbox;
pub use peko_session::{CompletionEvent, InboxItem, SteeringMessage};

/// Convenience alias preserved from the pre-Phase-2 root type:
///
/// ```text
/// pub type SharedSessionInbox = Arc<SessionInbox>;
/// ```
///
/// where `SessionInbox` is now `crate::async_exec::inbox::SessionInbox`.
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
    //! module now lives in the same crate as `crate::async_exec::inbox::*`.
    use super::*;

    #[test]
    fn shared_session_inbox_is_arc_of_session_inbox() {
        let _shared: SharedSessionInbox = Arc::new(SessionInbox::new());
    }
}
