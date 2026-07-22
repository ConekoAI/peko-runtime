//! F37-adjacent session surface that [`ToolExecutor`] needs.
//!
//! [`ToolExecutor::execute`](super::tool_executor::ToolExecutor::execute) only
//! writes back into the session: a single `add_tool_result(...)` call per
//! dispatch to persist the outcome (tool text + `is_error` flag). The session
//! `id` is supplied separately by the caller (the agentic loop already holds
//! it for `HookInput::ToolCall::session_id` stamping) so this trait does not
//! need to expose an `id()` method — keeping the surface to exactly one
//! async write.
//!
//! The full `crate::session::Session` type still lives in root (Phase 9
//! did not lift it because it transitively pulls session compaction,
//! JSONL persistence, vault adapters, and `BackgroundCompactor` — all
//! root-only coupling).
//!
//! Phase 9b.N.3 introduces this narrow trait so the executor can move
//! into `peko-engine` without dragging `Session` along. The trait is
//! transient scaffolding; it disappears when Phase 9b.N.4
//! (compaction_orchestrator) + Phase 9b.N.5+ (agentic_loop) lift enough
//! of session into `peko-engine` that the wrapper trait can be replaced
//! with a direct `Session` ref.
//!
//! # Orphan-rule design
//!
//! The blanket impl pattern lets root's [`crate::session::Session`]
//! participate without an `Arc<RwLock<Session>>`-typed impl (orphan
//! rule blocks that — `Arc` is not a fundamental type for the orphan
//! rule, so a foreign-trait impl on `Arc<RwLock<Session>>` is rejected
//! because `Arc` covers the local `Session`). Instead:
//!
//! - `peko-engine` defines [`SessionCore`] (foreign to root).
//! - `peko-engine` provides the blanket impl `impl<T: SessionCore>
//!   SessionView for Arc<RwLock<T>>`.
//! - Root's `src/engine/session_view_compat.rs` impls
//!   `impl SessionCore for crate::session::Session` — `Session` is
//!   local to root, so the orphan rule allows the impl.
//!
//! This matches the [[prefer-concrete-over-speculative-abstraction]]
//! guideline: only the single method the executor needs is exposed,
//! and the marker trait is mechanical boilerplate to satisfy the
//! orphan rule (not speculative abstraction).

use anyhow::Result;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Combined marker + entry-point trait for the inner `Session`-like
/// type stored behind the `Arc<RwLock<T>>` blanket impl.
///
/// `SessionCore::add_tool_result` mirrors `crate::session::Session::add_tool_result`:
///
/// - `tool_call_id` is the dispatcher's `id` (unique per tool invocation).
/// - `tool_name` is the canonical tool name (e.g. `"Bash"`, `"Read"`).
/// - `result` is the human-readable display string (callers typically
///   own it; we accept `&str` because the executor already has the
///   resolved `String` from the funnel return).
/// - `is_error` is the F32a flag: `true` for failed dispatches so a
///   resumed session can distinguish a real tool failure from a
///   successful zero-data return (audit section 3 row 2).
///
/// Implementations may write the record to disk and/or update an
/// in-memory message buffer; the executor treats it as opaque
/// side-effects. The blanket impl on `Arc<RwLock<T>>` (see below) keeps
/// the lock management inside the trait — callers don't need to
/// acquire the write lock themselves, mirroring the pre-Phase-9b.N.3
/// ergonomic of passing `session: &Arc<RwLock<Session>>`.
///
/// # Orphan-rule rationale
///
/// We need a `peko-engine` impl of `SessionView` for
/// `Arc<RwLock<Session>>`, but the trait is foreign to root. The naive
/// form `impl SessionView for Arc<RwLock<Session>>` fails the orphan
/// rule because `Arc` is not a fundamental type — `Arc` covers its
/// inner `Session`, so `Session` doesn't satisfy the
/// "local-type-before-uncovered-parameter" rule. Wrapping the impl in
/// a generic `impl<T: SessionCore> SessionView for Arc<RwLock<T>>`
/// fixes this: `T` is an uncovered type parameter of the impl, and
/// the orphan rule explicitly permits foreign-trait impls over
/// generics bounded by a local trait.
/// [[https://doc.rust-lang.org/reference/items/implementations.html#trait-implementations]]
#[async_trait::async_trait]
pub trait SessionCore: Send + Sync + 'static {
    async fn add_tool_result(
        session: &mut Self,
        tool_call_id: &str,
        tool_name: &str,
        result: &str,
        is_error: bool,
    ) -> Result<()>;
}

/// Blanket impl: any `Arc<RwLock<T>>` for `T: SessionCore` can be used
/// as a [`SessionView`].
///
/// Locks the `RwLock` internally so callers stay free of lock
/// management. The orphan rule allows this impl because `T` is an
/// "uncovered" generic type parameter, and the impl crate (peko-engine)
/// owns both the trait and the impl.
#[async_trait::async_trait]
pub trait SessionView: Send + Sync + 'static {
    async fn add_tool_result(
        &self,
        tool_call_id: &str,
        tool_name: &str,
        result: &str,
        is_error: bool,
    ) -> Result<()>;
}

#[async_trait::async_trait]
impl<T> SessionView for Arc<tokio::sync::RwLock<T>>
where
    T: SessionCore,
{
    async fn add_tool_result(
        &self,
        tool_call_id: &str,
        tool_name: &str,
        result: &str,
        is_error: bool,
    ) -> Result<()> {
        let mut guard = self.write().await;
        T::add_tool_result(&mut *guard, tool_call_id, tool_name, result, is_error).await
    }
}
