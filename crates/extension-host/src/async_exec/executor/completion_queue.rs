//! Per-session inbox of completed async tasks and user steering messages
//! waiting to be injected into the next agentic loop iteration.
//!
//! Distinct from [`super::queue::AsyncResultQueueManager`], which is the
//! older delivery sink kept for backward compatibility. New code should
//! read from this inbox.
//!
//! ## Phase 2 — type consolidation
//!
//! Prior to Phase 2, this file defined its own `CompletionEvent`,
//! `SteeringMessage`, `InboxItem`, and `SessionInbox` structs that were
//! field-identical (but type-distinct) duplicates of the canonical
//! definitions in `peko_extension_host::inbox`. Two copies caused:
//!
//! 1. A field-by-field conversion shim in
//!    `src/engine/async_inbox_compat.rs` to lift root's `InboxItem`
//!    into `peko_engine::AsyncInboxItem`.
//! 2. An orphan-rule `impl AsyncCompletionLike for CompletionEvent`
//!    in `src/engine/async_completion_compat.rs` that just re-exposed
//!    root's field-identical copy as an `AsyncCompletionLike` (the
//!    canonical `peko_extension_host::CompletionEvent` already
//!    implements it in `crates/engine/src/async_completion.rs:43`).
//!
//! Phase 2 deletes both shims. The types are now single-sourced in
//! `peko_extension_host`. Root re-exports them through this module so
//! historical `crate::extensions::framework::async_exec::executor::*`
//! import paths keep resolving until Phase 15 deletes them outright.
//!
//! `SharedSessionInbox = Arc<peko_extension_host::SessionInbox>` keeps
//! the historical convenience alias for the existing
//! `Arc<SharedSessionInbox>` callers.
//!
//! ### Migration note
//!
//! New code should import directly from `peko_extension_host`:
//!
//! ```ignore
//! use peko_extension_host::{CompletionEvent, InboxItem, SessionInbox, SteeringMessage};
//! ```
//!
//! The root-level re-export in this file is a Phase 15 deletion
//! candidate per `AGENTS.md` §Cleanup invariant.

use std::sync::Arc;

// Re-export the canonical types from this crate's `crate::inbox`
// module. Phase 8b lifts the executor into `peko-extension-host`,
// so these types are now intra-crate re-exports — root retains
// compat shims under `crate::extensions::framework::async_exec::executor`
// until the framework tree is fully deleted.
pub use crate::inbox::{CompletionEvent, InboxItem, SessionInbox, SteeringMessage};

/// Convenience alias preserved from the pre-Phase-2 root type:
///
/// ```text
/// pub type SharedSessionInbox = Arc<SessionInbox>;
/// ```
///
/// where `SessionInbox` is now `peko_extension_host::SessionInbox`.
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
    //! module now lives in the same crate as `crate::inbox::*`.
    use super::*;

    #[test]
    fn shared_session_inbox_is_arc_of_session_inbox() {
        let _shared: SharedSessionInbox = Arc::new(SessionInbox::new());
    }
}
