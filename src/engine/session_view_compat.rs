//! Compatibility shim: implements `peko_engine::SessionCore` for root's
//! `Session` so the lifted `ToolExecutor` (Phase 9b.N.3,
//! `crates/engine/src/tool_executor.rs`) and `CompactionOrchestrator`
//! (Phase 9b.N.4, `crates/engine/src/compaction_orchestrator.rs`)
//! can persist results without holding a direct borrow of root's
//! [`crate::session::Session`].
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

    async fn record_compaction(
        session: &mut Self,
        summary: &str,
        messages_compacted: usize,
        tokens_before: usize,
        tokens_after: usize,
        compaction_number: usize,
        details: Option<&serde_json::Value>,
    ) -> anyhow::Result<()> {
        // Phase 9b.N.4 widens the `details` field from
        // `summary_format::CompactionDetails` to `serde_json::Value`
        // in `peko_engine::CompactionEntry`. Root's `record_compaction`
        // still takes `Option<&CompactionDetails>` — re-deserialize
        // the value back into the concrete type before forwarding.
        // `null` and `None` map to `None`; deserialization failure
        // is treated as `None` (the on-disk record still has the
        // raw JSON via `serde_json::to_value(d)` in the compactor).
        let concrete_details = details.and_then(|v| {
            serde_json::from_value::<crate::session::compaction::summary_format::CompactionDetails>(
                v.clone(),
            )
            .ok()
        });
        Session::record_compaction(
            session,
            summary,
            messages_compacted,
            tokens_before,
            tokens_after,
            compaction_number,
            concrete_details.as_ref(),
        )
        .await
    }

    async fn load_previous_compaction_summary(session: &Self) -> anyhow::Result<Option<String>> {
        Session::load_previous_compaction_summary(session).await
    }

    async fn update_context_cache(
        session: &Self,
        messages: &[peko_message::LlmMessage],
    ) -> anyhow::Result<()> {
        Session::update_context_cache(session, messages).await
    }

    // ============================================================
    // Phase 9b.N.5b.9b additions: agentic_loop surface
    // ============================================================

    async fn id(session: &Self) -> String {
        session.id.clone()
    }

    async fn add_user(session: &mut Self, content: String) -> anyhow::Result<()> {
        Session::add_user(session, content).await
    }

    async fn set_model(session: &mut Self, provider: &str, model: &str) {
        Session::set_model(session, provider, model);
    }

    async fn record_model_change(
        session: &mut Self,
        provider: &str,
        model_id: &str,
    ) -> anyhow::Result<()> {
        Session::record_model_change(session, provider, model_id).await
    }

    async fn set_model_context_limit(session: &mut Self, limit: usize) {
        Session::set_model_context_limit(session, limit);
    }

    async fn add_assistant(
        session: &mut Self,
        content: String,
        tool_calls: Option<Vec<peko_message::ToolCallInfo>>,
        usage: Option<peko_message::TokenUsage>,
    ) -> anyhow::Result<()> {
        // Convert `peko_message::ToolCallInfo` → root's legacy `ToolCall`
        // struct so `Session::add_assistant` keeps its current signature.
        // The legacy struct only carries `name` + `parameters`; the
        // `id` and `result` fields from `ToolCallInfo` are dropped.
        // Every existing call site passes `None`, so this conversion is
        // dead in practice — kept for forward-compatibility with future
        // callers that surface tool-call IDs.
        let legacy_tool_calls = tool_calls.map(|calls| {
            calls
                .into_iter()
                .map(|info| crate::engine::ToolCall {
                    name: info.name,
                    parameters: info.parameters,
                })
                .collect()
        });
        Session::add_assistant(session, content, legacy_tool_calls, usage).await
    }

    async fn add_assistant_with_blocks(
        session: &mut Self,
        content_blocks: Vec<peko_message::ContentBlock>,
        tool_calls: Option<Vec<peko_message::ToolCallBlock>>,
        thinking: Option<peko_message::ThinkingBlock>,
        usage: Option<peko_message::TokenUsage>,
    ) -> anyhow::Result<()> {
        Session::add_assistant_with_blocks(session, content_blocks, tool_calls, thinking, usage)
            .await
    }

    async fn load_history(session: &Self) -> anyhow::Result<Vec<peko_message::LlmMessage>> {
        Session::load_history(session).await
    }
}
