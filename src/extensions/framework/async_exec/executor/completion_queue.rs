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

// Re-export the canonical types from `peko_extension_host`. These are
// the single source of truth — root no longer carries its own
// field-identical copies.
pub use peko_extension_host::{CompletionEvent, InboxItem, SessionInbox, SteeringMessage};

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
    //! Tests run against `peko_extension_host::SessionInbox` directly
    //! — there is no separate root type to test. The duplication that
    //! existed before Phase 2 is gone, so the test suite is now
    //! hosted on the canonical type in `peko-extension-host` (see
    //! `crates/extension-host/src/inbox.rs`).
    //!
    //! This module exists to pin the root re-exports and the
    //! `SharedSessionInbox` alias so that any accidental decoupling
    //! between the root path and the canonical home surfaces here as
    //! a compile error rather than a runtime type confusion.
    use super::*;

    #[test]
    fn re_exports_resolve_to_peko_extension_host() {
        // Type-level assertion: the root type IS the host type.
        // `std::ptr::eq` is not valid for types, so we go through
        // `type_id` which is the canonical "same type" check Rust
        // exposes.
        assert_eq!(
            std::any::TypeId::of::<CompletionEvent>(),
            std::any::TypeId::of::<peko_extension_host::CompletionEvent>(),
        );
        assert_eq!(
            std::any::TypeId::of::<SteeringMessage>(),
            std::any::TypeId::of::<peko_extension_host::SteeringMessage>(),
        );
        assert_eq!(
            std::any::TypeId::of::<InboxItem>(),
            std::any::TypeId::of::<peko_extension_host::InboxItem>(),
        );
        assert_eq!(
            std::any::TypeId::of::<SessionInbox>(),
            std::any::TypeId::of::<peko_extension_host::SessionInbox>(),
        );
    }

    #[test]
    fn shared_session_inbox_is_arc_of_session_inbox() {
        let _shared: SharedSessionInbox = Arc::new(SessionInbox::new());
    }
}
