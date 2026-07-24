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

    async fn add_user(session: &mut Self, content: String) -> Result<()>;

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

    async fn add_user(&self, content: String) -> Result<()>;

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

    async fn add_user(&self, content: String) -> Result<()> {
        let mut guard = self.write().await;
        T::add_user(&mut *guard, content).await
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
}
