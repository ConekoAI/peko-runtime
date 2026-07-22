//! Compatibility shim: implements `peko_engine::SessionCore` for root's
//! `Session` so the lifted `ToolExecutor` (Phase 9b.N.3,
//! `crates/engine/src/tool_executor.rs`) can persist tool results
//! without holding a direct borrow of root's [`crate::session::Session`].
//!
//! The impl lives here rather than in `peko-engine` because of the
//! orphan rule: `peko_engine::SessionCore` is a foreign trait, and
//! `Session` is a root-only type. The `impl SessionCore for Session`
//! form is allowed because `Session` is local to root (see the orphan
//! rule's "local type before any uncovered type parameter" clause).
//!
//! The blanket `impl<T: SessionCore> SessionView for Arc<RwLock<T>>`
//! in `crates/engine/src/session_view.rs` then gives
//! `Arc<RwLock<Session>>` a `SessionView` impl for free. Callers in
//! the agentic loop pass `&session` directly — the same ergonomic as
//! pre-Phase-9b.N.3 — without ever knowing about `SessionCore`.
//!
//! Module location: rooted at `src/engine/session_view_compat.rs` so
//! `src/engine/mod.rs` declares it via `pub mod`, mirroring the
//! `src/engine/extension_core_funnel_compat.rs` (Phase 9b.N.2) and
//! `src/engine/async_completion_compat.rs` (Phase 9b.N.1) patterns.

use crate::session::Session;
use peko_engine::SessionCore;

#[async_trait::async_trait]
impl SessionCore for Session {
    async fn add_tool_result(
        session: &mut Self,
        tool_call_id: &str,
        tool_name: &str,
        result: &str,
        is_error: bool,
    ) -> anyhow::Result<()> {
        Session::add_tool_result(session, tool_call_id, tool_name, result, is_error).await
    }
}
