//! `SessionCore` — narrow trait port the engine uses to read / write
//! session state without holding a direct borrow of root's
//! `peko_session::Session` type.
//!
//! # Phase 7 location rationale
//!
//! Phase 9b.N.3 placed `SessionCore` in `peko-engine` because the
//! lifted `ToolExecutor` was the canonical consumer. Phase 7 lifted
//! `Session` itself out of root and into `peko-session`. The orphan
//! rule then broke the engine-side definition: `impl SessionCore
//! for peko_session::Session` is rejected when `SessionCore` lives in
//! `peko-engine` because both are foreign to root. Moving the trait
//! to `peko-session` (where `Session` is local) makes the impl
//! legal again without needing root-side shim code.
//!
//! `peko-engine` re-exports both `SessionCore` and `SessionView`
//! from this module so the lifted `ToolExecutor`,
//! `CompactionOrchestrator`, and `AgenticLoop` keep their existing
//! `peko_engine::SessionCore` / `peko_engine::SessionView` import
//! paths unchanged.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

/// Combined marker + entry-point trait for the inner `Session`-like
/// type stored behind the `Arc<RwLock<T>>` blanket impl.
///
/// The trait exposes the surface the engine needs:
/// - Tool result write-back (`add_tool_result`)
/// - Compaction bookkeeping (`record_compaction`,
///   `load_previous_compaction_summary`, `update_context_cache`)
/// - Agentic-loop message appends (`add_user`, `add_assistant`,
///   `add_assistant_with_blocks`, `set_model`,
///   `record_model_change`, `set_model_context_limit`, `id`,
///   `load_history`)
///
/// Implementations may write the record to disk and/or update an
/// in-memory message buffer; callers treat them as opaque
/// side-effects.
#[async_trait]
pub trait SessionCore: Send + Sync + 'static {
    async fn add_tool_result(
        session: &mut Self,
        tool_call_id: &str,
        tool_name: &str,
        result: &str,
        is_error: bool,
    ) -> Result<()>;

    #[allow(clippy::too_many_arguments)]
    async fn record_compaction(
        session: &mut Self,
        summary: &str,
        messages_compacted: usize,
        tokens_before: usize,
        tokens_after: usize,
        compaction_number: usize,
        details: Option<&serde_json::Value>,
    ) -> Result<()>;

    async fn load_previous_compaction_summary(session: &Self) -> Result<Option<String>>;

    async fn update_context_cache(
        session: &Self,
        messages: &[peko_message::LlmMessage],
    ) -> Result<()>;

    async fn id(session: &Self) -> String;

    /// Read the persisted token counters: `(total_input, total_output,
    /// last_total)`. `last_total` is the most recent provider-reported
    /// cumulative token count for the conversation; it survives
    /// restarts and JSONL reloads (every assistant message persists
    /// its `usage.total_tokens` via [`SessionCore::add_assistant`]),
    /// so it remains authoritative when the in-memory `messages`
    /// slice is empty.
    ///
    /// Default: all zeros — implementors without a persisted counter
    /// (test stubs, in-memory sessions) compile unchanged.
    async fn token_usage(session: &Self) -> (usize, usize, usize) {
        let _ = session;
        (0, 0, 0)
    }

    async fn add_user(session: &mut Self, content: String) -> Result<()>;

    /// Add a user-role message tagged with an explicit [`MessageSource`].
    ///
    /// Distinct from `add_user` so callers can mark automation-originated
    /// notes (subagent results, cron notifications, A2A deliveries,
    /// hook injections, peer pushes) without losing the source
    /// information on the next load. The default impl falls through to
    /// `add_user` so test stubs compile unchanged — production
    /// `peko_session::Session` overrides it to persist the source tag
    /// onto the `SessionMessage`'s `RoleMetadata`.
    async fn add_user_with_source(
        session: &mut Self,
        content: String,
        source: crate::events::MessageSource,
    ) -> Result<()> {
        let _ = source;
        Self::add_user(session, content).await
    }

    async fn set_model(session: &mut Self, provider: &str, model: &str);

    async fn record_model_change(session: &mut Self, provider: &str, model_id: &str) -> Result<()>;

    async fn set_model_context_limit(session: &mut Self, limit: usize);

    async fn add_assistant(
        session: &mut Self,
        content: String,
        tool_calls: Option<Vec<peko_message::ToolCallInfo>>,
        usage: Option<peko_message::TokenUsage>,
    ) -> Result<()>;

    #[allow(clippy::too_many_arguments)]
    async fn add_assistant_with_blocks(
        session: &mut Self,
        content_blocks: Vec<peko_message::ContentBlock>,
        tool_calls: Option<Vec<peko_message::ToolCallBlock>>,
        thinking: Option<peko_message::ThinkingBlock>,
        usage: Option<peko_message::TokenUsage>,
    ) -> Result<()>;

    async fn load_history(session: &Self) -> Result<Vec<peko_message::LlmMessage>>;

    /// Peek the persisted compaction-request flag (agent-owned session
    /// management, plan D2). The compaction orchestrator ORs this into
    /// its threshold decision. Default: no request — implementors
    /// without a persistent flag (test stubs, in-memory sessions)
    /// compile unchanged.
    async fn peek_compact_request(session: &mut Self) -> bool {
        let _ = session;
        false
    }

    /// Clear the persisted compaction-request flag. The orchestrator
    /// calls this only when compaction genuinely starts, so a crashed
    /// run doesn't lose the request. Default: no-op.
    async fn clear_compact_request(session: &mut Self) {
        let _ = session;
    }
}

/// Caller-facing facade: takes `&self` (lock-encapsulated).
///
/// Any `Arc<RwLock<T>>` for `T: SessionCore` automatically gets a
/// `SessionView` impl via the blanket impl below — callers don't
/// need to acquire the write lock themselves.
#[async_trait]
pub trait SessionView: Send + Sync + 'static {
    async fn add_tool_result(
        &self,
        tool_call_id: &str,
        tool_name: &str,
        result: &str,
        is_error: bool,
    ) -> Result<()>;

    #[allow(clippy::too_many_arguments)]
    async fn record_compaction(
        &self,
        summary: &str,
        messages_compacted: usize,
        tokens_before: usize,
        tokens_after: usize,
        compaction_number: usize,
        details: Option<&serde_json::Value>,
    ) -> Result<()>;

    async fn load_previous_compaction_summary(&self) -> Result<Option<String>>;

    async fn update_context_cache(&self, messages: &[peko_message::LlmMessage]) -> Result<()>;

    async fn id(&self) -> String;

    /// See [`SessionCore::token_usage`]. Default: all zeros.
    async fn token_usage(&self) -> (usize, usize, usize) {
        (0, 0, 0)
    }

    async fn add_user(&self, content: String) -> Result<()>;

    /// Add a user-role message tagged with an explicit [`MessageSource`].
    ///
    /// See [`SessionCore::add_user_with_source`] for the source-tag
    /// semantics. No default impl — callers that don't care about the
    /// source tag should use [`SessionView::add_user`] instead, so the
    /// trait surface stays explicit about intent.
    async fn add_user_with_source(
        &self,
        content: String,
        source: crate::events::MessageSource,
    ) -> Result<()>;

    async fn set_model(&self, provider: &str, model: &str);

    async fn record_model_change(&self, provider: &str, model_id: &str) -> Result<()>;

    async fn set_model_context_limit(&self, limit: usize);

    async fn add_assistant(
        &self,
        content: String,
        tool_calls: Option<Vec<peko_message::ToolCallInfo>>,
        usage: Option<peko_message::TokenUsage>,
    ) -> Result<()>;

    #[allow(clippy::too_many_arguments)]
    async fn add_assistant_with_blocks(
        &self,
        content_blocks: Vec<peko_message::ContentBlock>,
        tool_calls: Option<Vec<peko_message::ToolCallBlock>>,
        thinking: Option<peko_message::ThinkingBlock>,
        usage: Option<peko_message::TokenUsage>,
    ) -> Result<()>;

    async fn load_history(&self) -> Result<Vec<peko_message::LlmMessage>>;

    /// Peek the persisted compaction-request flag (plan D2). Default:
    /// no request — see [`SessionCore::peek_compact_request`].
    async fn peek_compact_request(&self) -> bool {
        false
    }

    /// Clear the persisted compaction-request flag once compaction
    /// genuinely starts. Default: no-op.
    async fn clear_compact_request(&self) {}
}

#[async_trait]
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

    #[allow(clippy::too_many_arguments)]
    async fn record_compaction(
        &self,
        summary: &str,
        messages_compacted: usize,
        tokens_before: usize,
        tokens_after: usize,
        compaction_number: usize,
        details: Option<&serde_json::Value>,
    ) -> Result<()> {
        let mut guard = self.write().await;
        T::record_compaction(
            &mut *guard,
            summary,
            messages_compacted,
            tokens_before,
            tokens_after,
            compaction_number,
            details,
        )
        .await
    }

    async fn load_previous_compaction_summary(&self) -> Result<Option<String>> {
        let guard = self.read().await;
        T::load_previous_compaction_summary(&*guard).await
    }

    async fn update_context_cache(&self, messages: &[peko_message::LlmMessage]) -> Result<()> {
        let guard = self.read().await;
        T::update_context_cache(&*guard, messages).await
    }

    async fn id(&self) -> String {
        let guard = self.read().await;
        T::id(&*guard).await
    }

    async fn token_usage(&self) -> (usize, usize, usize) {
        let guard = self.read().await;
        T::token_usage(&*guard).await
    }

    async fn add_user(&self, content: String) -> Result<()> {
        let mut guard = self.write().await;
        T::add_user(&mut *guard, content).await
    }

    async fn add_user_with_source(
        &self,
        content: String,
        source: crate::events::MessageSource,
    ) -> Result<()> {
        let mut guard = self.write().await;
        T::add_user_with_source(&mut *guard, content, source).await
    }

    async fn set_model(&self, provider: &str, model: &str) {
        let mut guard = self.write().await;
        T::set_model(&mut *guard, provider, model).await
    }

    async fn record_model_change(&self, provider: &str, model_id: &str) -> Result<()> {
        let mut guard = self.write().await;
        T::record_model_change(&mut *guard, provider, model_id).await
    }

    async fn set_model_context_limit(&self, limit: usize) {
        let mut guard = self.write().await;
        T::set_model_context_limit(&mut *guard, limit).await
    }

    async fn add_assistant(
        &self,
        content: String,
        tool_calls: Option<Vec<peko_message::ToolCallInfo>>,
        usage: Option<peko_message::TokenUsage>,
    ) -> Result<()> {
        let mut guard = self.write().await;
        T::add_assistant(&mut *guard, content, tool_calls, usage).await
    }

    #[allow(clippy::too_many_arguments)]
    async fn add_assistant_with_blocks(
        &self,
        content_blocks: Vec<peko_message::ContentBlock>,
        tool_calls: Option<Vec<peko_message::ToolCallBlock>>,
        thinking: Option<peko_message::ThinkingBlock>,
        usage: Option<peko_message::TokenUsage>,
    ) -> Result<()> {
        let mut guard = self.write().await;
        T::add_assistant_with_blocks(&mut *guard, content_blocks, tool_calls, thinking, usage).await
    }

    async fn load_history(&self) -> Result<Vec<peko_message::LlmMessage>> {
        let guard = self.read().await;
        T::load_history(&*guard).await
    }

    async fn peek_compact_request(&self) -> bool {
        let mut guard = self.write().await;
        T::peek_compact_request(&mut *guard).await
    }

    async fn clear_compact_request(&self) {
        let mut guard = self.write().await;
        T::clear_compact_request(&mut *guard).await;
    }
}
