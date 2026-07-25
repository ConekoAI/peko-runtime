//! `impl SessionCore for Session` — Phase 7 orphan-rule compliant
//! location. The trait lives in this crate (see `session_core.rs`);
//! the impl lives next to the canonical `Session` type, satisfying
//! the "local type before any uncovered type parameter" rule.
//!
//! Pre-Phase 7 this impl lived in root's
//! `src/engine/session_view_compat.rs`. Phase 7 lifts `Session` out
//! of root, which broke the root-side impl (foreign trait + foreign
//! type). Moving the impl here is the only correct fix; engine code
//! continues to import `peko_engine::SessionCore` (re-exported from
//! this crate) and consume `Arc<dyn SessionView>` exactly as before.

use crate::session_core::SessionCore;
use crate::unified::Session;

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
        // `details` is forwarded as `Option<&serde_json::Value>` by
        // the trait port to keep the engine crate free of the
        // concrete `summary_format::CompactionDetails` type. The
        // concrete `Session::record_compaction` still takes the typed
        // form — re-deserialize here at the trait impl boundary.
        let concrete_details = details.and_then(|v| {
            serde_json::from_value::<crate::compaction::summary_format::CompactionDetails>(
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
        // Convert `peko_message::ToolCallInfo` → legacy `ToolCall`
        // so the canonical `Session::add_assistant` keeps its current
        // signature. The legacy struct only carries `name` +
        // `parameters`; the `id` and `result` fields from `ToolCallInfo`
        // are dropped. Every existing call site passes `None`, so this
        // conversion is dead in practice — kept for forward-compat.
        let legacy_tool_calls = tool_calls.map(|calls| {
            calls
                .into_iter()
                .map(|info| crate::ToolCall {
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
