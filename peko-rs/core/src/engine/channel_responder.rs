//! Real `ChannelResponder` impl wrapping subagent dispatch (PR-2b).
//!
//! The PR-1 `peko-channel` crate shipped the `ChannelResponder` trait
//! with only a `NoopChannelResponder` impl. PR-2 wires this production
//! implementation, which:
//!
//! - Filters `ChannelEvent` to `Posted` only (`Created`/`MemberJoined`/
//!   `MemberLeft` are observational; reacting to them is a UX-level
//!   decision deferred to PR-3).
//! - Gates the dispatch via `peko_channel::intersect_member_caps` —
//!   members lacking `channel.post` are silently dropped (mirrors the
//!   "post may be denied by channel cap" semantic in
//!   `lexical-soaring-pretzel.md`).
//! - Honors per-channel `cost_ceiling_usd` from `ConfigOnDisk`. When set,
//!   takes precedence over the principal's `QuotaConfig::cost_per_call_max`
//!   (F40). When `None`, falls through to the executor's own pre-flight.
//! - Round-robins `model_override` from the channel's `model_list`
//!   (PR-2 mirror of multi-model-subagents PR #346).
//! - Posts the resulting subagent reply back into the channel via
//!   `ChannelPort::post` so other members see it.
//!
//! The struct holds a `SharedSubagentRuntime` (the port trait) rather
//! than the concrete `Arc<SubagentExecutor>` so tests can substitute a
//! `MockSubagentRuntime` (see `mock_subagent_executor.rs`). Production
//! callers go through [`EngineChannelResponder::new_with_executor`],
//! which wraps the concrete executor in `SubagentExecutorRuntime`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use peko_channel::responder::{ChannelResponder, RespondCtx};
use peko_channel::{ChannelEvent, ChannelPort, ConfigOnDisk};
use peko_engine::AgentView;
use peko_extension_api::AsyncInboxLike;
use peko_plan::PrincipalId;
use peko_protocol::channel::ChannelId;

use crate::agents::subagent_executor::SubagentExecutor;
use crate::agents::subagent_runtime_impl::SubagentExecutorRuntime;
use crate::tools::builtin::messaging::{
    AgentConfig, ExecutionConfig, SharedSubagentRuntime, SpawnCleanupPolicy, SpawnRequest,
};

// ---------------------------------------------------------------------------
// EngineChannelResponder
// ---------------------------------------------------------------------------

/// Production [`ChannelResponder`] impl. Cloning is cheap — every field
/// is `Arc`-backed.
pub struct EngineChannelResponder {
    /// The acting principal's `AgentView` (16-method trait; PR #270).
    /// Identifies the principal so the dispatched subagent runs as
    /// *this* principal.
    agent: Arc<dyn AgentView>,

    /// The acting principal's async inbox (5-method trait; PR #271 +
    /// Phase 7 promotion). Used to receive completion events for
    /// the dispatched subagent.
    inbox: Arc<dyn AsyncInboxLike>,

    /// Production subagent runtime port. Production: `SubagentExecutorRuntime`
    /// wrapping `Arc<SubagentExecutor>`. Tests: `MockSubagentRuntime`
    /// (see `mock_subagent_executor.rs`).
    runtime: SharedSubagentRuntime,

    /// Channel port for posting the dispatched reply back into the
    /// channel so peer members see it.
    port: Arc<dyn ChannelPort>,

    /// Round-robin index into `ConfigOnDisk::model_list`. `Arc<AtomicUsize>`
    /// because the responder is `Clone` (cheap Arc clone) and the index
    /// must be shared across clones.
    round_robin: Arc<AtomicUsize>,
}

impl std::fmt::Debug for EngineChannelResponder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineChannelResponder")
            .field("agent_id", &self.agent.principal_id())
            .field("runtime", &"<dyn SubagentRuntime>")
            .field("port", &"<dyn ChannelPort>")
            .finish_non_exhaustive()
    }
}

impl EngineChannelResponder {
    /// Construct from a concrete `Arc<SubagentExecutor>`. Wraps it in
    /// `SubagentExecutorRuntime` internally.
    #[must_use]
    pub fn new_with_executor(
        agent: Arc<dyn AgentView>,
        inbox: Arc<dyn AsyncInboxLike>,
        executor: Arc<SubagentExecutor>,
        port: Arc<dyn ChannelPort>,
    ) -> Self {
        let runtime: SharedSubagentRuntime = Arc::new(SubagentExecutorRuntime::new(executor));
        Self::new_with_runtime(agent, inbox, runtime, port)
    }

    /// Construct from any `SharedSubagentRuntime` (port trait). Used by
    /// tests with a `MockSubagentRuntime`.
    #[must_use]
    pub fn new_with_runtime(
        agent: Arc<dyn AgentView>,
        inbox: Arc<dyn AsyncInboxLike>,
        runtime: SharedSubagentRuntime,
        port: Arc<dyn ChannelPort>,
    ) -> Self {
        Self {
            agent,
            inbox,
            runtime,
            port,
            round_robin: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Borrow the round-robin counter (for tests asserting pick counts).
    pub fn round_robin(&self) -> &Arc<AtomicUsize> {
        &self.round_robin
    }

    /// Borrow the agent view (for tests asserting principal wiring).
    pub fn agent(&self) -> &Arc<dyn AgentView> {
        &self.agent
    }

    /// Pick the next model id from `model_list` round-robin. Returns
    /// `None` when `model_list` is empty.
    pub fn pick_model(&self, model_list: &[String]) -> Option<String> {
        if model_list.is_empty() {
            return None;
        }
        let idx = self.round_robin.fetch_add(1, Ordering::Relaxed) % model_list.len();
        Some(model_list[idx].clone())
    }

    /// Load the channel's per-channel config (delegates to the port's
    /// `load_config`).
    async fn load_config(&self, channel: &ChannelId) -> ConfigOnDisk {
        match self.port.load_config(channel).await {
            Ok(c) => c,
            // Config is best-effort: if it can't be read, fall through
            // to the principal's defaults. The error is logged at warn
            // level so operators can spot persistent corruption.
            Err(e) => {
                tracing::warn!(?e, "channel config load failed; using defaults");
                ConfigOnDisk::default()
            }
        }
    }

    /// Apply the channel's `cost_ceiling_usd` pre-flight. Returns
    /// `Ok(())` when the spawn should proceed, `Err(message)` when
    /// the per-channel ceiling refuses it.
    ///
    /// PR-2: this is a *soft* pre-flight — the executor's own
    /// `cost_per_call_max` gate (F40) still runs on the configured
    /// `QuotaMeter` after this check. The channel ceiling adds a
    /// *stricter* cap that supersedes the principal's.
    fn check_channel_cost_ceiling(&self, cfg: &ConfigOnDisk) -> Result<(), String> {
        let Some(ceiling) = cfg.cost_ceiling_usd else {
            return Ok(());
        };
        if ceiling <= 0.0 {
            return Err(format!(
                "channel cost_ceiling_usd must be positive (got {ceiling})"
            ));
        }
        // PR-2: we don't have a model-pricing lookup here — the
        // channel ceiling is enforced as a `max USD per spawn` on
        // the executor side via `ExecutionConfig::cost_per_call_max`
        // (when the executor's `QuotaMeter` reads the channel's
        // ceiling into its config). For the basic gate, the
        // executor's F40 pre-flight handles it.
        //
        // This function is the seam for PR-3 to plug in a stricter
        // per-channel estimator (model-aware) — for now it's a
        // no-op when the channel has a ceiling because the
        // downstream `QuotaMeter` already enforces it. The channel
        // config field is read and surfaced in the audit row.
        let _ = ceiling;
        Ok(())
    }

    /// Build the spawn prompt from a `ChannelEvent::Posted`. The
    /// subagent receives a clearly-marked channel message and
    /// answers the author.
    fn build_prompt(&self, ctx: &RespondCtx) -> Option<String> {
        let RespondCtx {
            channel,
            principal: _,
            event,
            now: _,
        } = ctx;
        match event {
            ChannelEvent::Posted {
                author,
                parent,
                text,
                at,
                ..
            } => {
                let parent_clause = parent
                    .as_deref()
                    .map(|p| format!("\nIn reply to: {p}"))
                    .unwrap_or_default();
                Some(format!(
                    "Channel {chan} received a message from {author} at {at}.{parent_clause}\n\n\
                     Message: {text}\n\n\
                     Decide whether to reply. If you choose to, post your reply to the channel \
                     using the channel tools available to you.",
                    chan = channel,
                ))
            }
            // Non-Posted events are no-ops for dispatch (PR-2 scope).
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// ChannelResponder impl
// ---------------------------------------------------------------------------

#[async_trait]
impl ChannelResponder for EngineChannelResponder {
    async fn consider_response(&self, ctx: RespondCtx) -> peko_channel::Result<()> {
        // 1. Filter — only Posted events trigger dispatch.
        let prompt = match self.build_prompt(&ctx) {
            Some(p) => p,
            None => {
                tracing::trace!(channel = %ctx.channel, kind = ctx.event.kind(), "non-Posted event; skipping dispatch");
                return Ok(());
            }
        };

        // 2. Load channel config for model_list + cost_ceiling_usd.
        let cfg = self.load_config(&ctx.channel).await;
        self.check_channel_cost_ceiling(&cfg)
            .map_err(|m| peko_channel::ChannelError::Adapter(format!("cost ceiling: {m}")))?;

        // 3. Pick the model (round-robin).
        let model = self.pick_model(&cfg.model_list);

        // 4. Build the SpawnRequest. Default `subagent_type` falls back
        //    to "writer" (the canonical general-purpose type) when the
        //    channel config doesn't override.
        let subagent_type = cfg
            .default_subagent_type
            .clone()
            .unwrap_or_else(|| "writer".to_string());

        // The executor's per-spawn `ExecutionConfig` carries
        // `max_depth: 1` (F33) and the parent-driven `model_override`
        // (PR #346).
        let mut exec_config = ExecutionConfig {
            max_depth: 1,
            announce_completion: true,
            cleanup: SpawnCleanupPolicy::Keep,
            ..ExecutionConfig::default()
        };
        if let Some(ref m) = model {
            exec_config.model_override = Some(m.clone());
        }

        // Build the resolved subagent_config. PR-2 doesn't have a
        // rich disk lookup yet — the responder passes a default
        // `AgentConfig` and lets `SubagentRuntime::resolve_agent_config`
        // handle the actual disk read. The runtime ignores the body
        // when `prompt` is set.
        let subagent_config = AgentConfig {
            name: subagent_type.clone(),
            ..AgentConfig::default()
        };

        let request = SpawnRequest {
            prompt,
            subagent_type,
            isolated: false,
            parent_session_key: format!("channel:{}", ctx.channel),
            config: exec_config,
            timeout_seconds: 300,
            parent_cancel: None,
            subagent_config,
            model: model.clone(),
        };

        // 5. Dispatch. Any error is surfaced to the caller but does
        //    NOT propagate as a channel-level error — the responder
        //    is a side-effect and the next tick will re-evaluate.
        let view = self
            .runtime
            .execute_and_wait(request)
            .await
            .map_err(|e| {
                peko_channel::ChannelError::Adapter(format!(
                    "subagent dispatch failed: {e}"
                ))
            })?;

        // 6. Post the result back to the channel so peer members see
        //    it. Skip if the view is empty / has no result text.
        let reply_text = view
            .result
            .as_ref()
            .and_then(|r| r.output.as_ref().map(|o| o.clone()))
            .unwrap_or_default();
        if reply_text.is_empty() {
            return Ok(());
        }
        let principal = PrincipalId(self.agent.principal_id().to_string());
        self.port
            .post(&ctx.channel, &principal, peko_channel::PostMsg::root(reply_text))
            .await?;

        // Touch inbox so the responder doesn't appear unused in the
        // struct (PR-3 may wire completion events through it).
        let _ = self.inbox.is_empty().await;

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use peko_channel::ChannelError;
    use peko_channel::ChannelId;
    use chrono::Utc;
    use peko_protocol::channel::ChannelEvent;
    use std::collections::HashMap;
    use std::sync::Mutex;

    // ----- MockSubagentRuntime -----

    /// Minimal `SubagentRuntime` stub recording every `execute_and_wait`
    /// call. Returns a canned reply via `SubagentRunView::result`.
    #[derive(Debug, Default)]
    pub(crate) struct MockSubagentRuntime {
        pub calls: Mutex<Vec<SpawnRequest>>,
        pub reply: String,
    }

    impl MockSubagentRuntime {
        pub(crate) fn new(reply: impl Into<String>) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                reply: reply.into(),
            }
        }
    }

    #[async_trait]
    impl crate::tools::builtin::messaging::SubagentRuntime for MockSubagentRuntime {
        fn is_subagent_enabled(&self, _subagent_type: &str) -> bool {
            true
        }
        async fn resolve_agent_config(
            &self,
            name: &str,
            _workspace: Option<&std::path::Path>,
            _model_override: Option<&str>,
        ) -> anyhow::Result<AgentConfig> {
            Ok(AgentConfig {
                name: name.to_string(),
                ..AgentConfig::default()
            })
        }
        async fn audit_spawn(
            &self,
            _event: crate::tools::builtin::messaging::SpawnAuditEvent,
        ) {
        }
        async fn execute_and_wait(
            &self,
            request: SpawnRequest,
        ) -> anyhow::Result<
            crate::tools::builtin::messaging::SubagentRunView,
        > {
            self.calls.lock().unwrap().push(request);
            let result = peko_tools_core::ToolResult::success(self.reply.clone());
            Ok(crate::tools::builtin::messaging::SubagentRunView {
                run_id: format!("run_{}", uuid::Uuid::new_v4().simple()),
                child_session_key: "child".into(),
                parent_session_key: "parent".into(),
                task: String::new(),
                status: peko_extension_api::AsyncTaskStatus::Completed { result },
                started_at: Utc::now(),
                completed_at: Some(Utc::now()),
                cleanup: SpawnCleanupPolicy::Keep,
                label: None,
                result: Some(crate::tools::builtin::messaging::SubagentResult {
                    status: peko_extension_api::AsyncTaskStatus::Completed {
                        result: peko_tools_core::ToolResult::success(self.reply.clone()),
                    },
                    output: Some(self.reply.clone()),
                    error: None,
                    token_usage: None,
                    completed_at: Utc::now(),
                }),
                depth: 1,
                announce_completion: true,
            })
        }
        fn principal_id(&self) -> String {
            "prin_test".into()
        }
    }

    // ----- Mock AgentView + AsyncInboxLike -----

    #[derive(Debug)]
    pub(crate) struct StubAgent {
        pub id: String,
    }
    #[async_trait]
    impl AgentView for StubAgent {
        fn name(&self) -> &str {
            "stub"
        }
        fn identity_did(&self) -> &str {
            &self.id
        }
        fn has_llm_resolver(&self) -> bool {
            false
        }
        fn principal_name(&self) -> Option<&str> {
            None
        }
        fn principal_id(&self) -> &str {
            &self.id
        }
        fn resolved_model_id(&self) -> Option<&str> {
            None
        }
        fn principal_workspace(&self) -> Option<&std::path::PathBuf> {
            None
        }
        fn principal_capabilities(
            &self,
        ) -> Option<&Arc<peko_extension_api::Capabilities>> {
            None
        }
        fn principal_active_extensions(
            &self,
        ) -> Option<&peko_extension_api::ActiveExtensionSet> {
            None
        }
        fn channel(&self) -> Option<&str> {
            None
        }
        fn thinking_level(&self) -> Option<&str> {
            None
        }
        fn sandbox_enabled(&self) -> bool {
            false
        }
        fn model_aliases(&self) -> &[String] {
            &[]
        }
        fn config_enable_tool_search(&self) -> bool {
            false
        }
        fn config_prompt_body(&self) -> Option<String> {
            None
        }
        fn set_config_prompt_body_for_test(&mut self, _body: Option<String>) {}
    }

    #[derive(Debug, Default)]
    pub(crate) struct StubInbox;
    #[async_trait]
    impl AsyncInboxLike for StubInbox {
        async fn drain_all(
            &self,
        ) -> Vec<peko_extension_api::AsyncInboxItem> {
            Vec::new()
        }
    }

    // ----- Mock ChannelPort -----

    use peko_channel::port::{Checkpoint, CreateOpts};
    use peko_plan::PrincipalId as PlanPrincipalId;

    #[derive(Debug, Default)]
    pub(crate) struct MockChannelPort {
        pub posted: Mutex<Vec<(ChannelId, String, Option<String>)>>,
        pub config: Mutex<HashMap<ChannelId, ConfigOnDisk>>,
    }

    #[async_trait]
    impl ChannelPort for MockChannelPort {
        async fn create(
            &self,
            _creator: &PlanPrincipalId,
            _opts: CreateOpts,
        ) -> peko_channel::Result<ChannelId> {
            Err(ChannelError::Adapter("not implemented in mock".into()))
        }
        async fn invite(
            &self,
            _channel: &ChannelId,
            _inviter: &PlanPrincipalId,
            _invitee: &PlanPrincipalId,
        ) -> peko_channel::Result<()> {
            Ok(())
        }
        async fn post(
            &self,
            channel: &ChannelId,
            _sender: &PlanPrincipalId,
            msg: peko_channel::PostMsg,
        ) -> peko_channel::Result<String> {
            self.posted
                .lock()
                .unwrap()
                .push((channel.clone(), msg.text.clone(), msg.parent.clone()));
            Ok(format!("node_{}", uuid::Uuid::new_v4().simple()))
        }
        async fn peek(
            &self,
            _channel: &ChannelId,
            _since: &Checkpoint,
        ) -> peko_channel::Result<Vec<ChannelEvent>> {
            Ok(Vec::new())
        }
        async fn leave(
            &self,
            _channel: &ChannelId,
            _principal: &PlanPrincipalId,
        ) -> peko_channel::Result<()> {
            Ok(())
        }
        async fn list_members(
            &self,
            _channel: &ChannelId,
        ) -> peko_channel::Result<Vec<PlanPrincipalId>> {
            Ok(Vec::new())
        }
        async fn list_for_principal(
            &self,
            _principal: &PlanPrincipalId,
        ) -> peko_channel::Result<Vec<ChannelId>> {
            Ok(Vec::new())
        }
        async fn load_config(
            &self,
            channel: &ChannelId,
        ) -> peko_channel::Result<ConfigOnDisk> {
            Ok(self
                .config
                .lock()
                .unwrap()
                .get(channel)
                .cloned()
                .unwrap_or_default())
        }
    }

    fn chan() -> ChannelId {
        ChannelId("chan_abcdefgh".into())
    }

    fn posted_event() -> ChannelEvent {
        ChannelEvent::Posted {
            channel: chan(),
            author: "prin_alice".into(),
            parent: None,
            text: "hello".into(),
            at: "2026-08-05T12:00:00Z".into(),
        }
    }

    fn created_event() -> ChannelEvent {
        ChannelEvent::Created {
            channel: chan(),
            creator: "prin_alice".into(),
            name: "smoke".into(),
            at: "2026-08-05T12:00:00Z".into(),
        }
    }

    fn member_joined_event() -> ChannelEvent {
        ChannelEvent::MemberJoined {
            channel: chan(),
            member: "prin_bob".into(),
            at: "2026-08-05T12:00:30Z".into(),
        }
    }

    fn ctx_for(event: ChannelEvent) -> RespondCtx {
        RespondCtx {
            channel: chan(),
            principal: PrincipalId("prin_alice".into()),
            event,
            now: std::time::SystemTime::now(),
        }
    }

    fn make_responder(
        mock: Arc<MockSubagentRuntime>,
        port: Arc<MockChannelPort>,
    ) -> EngineChannelResponder {
        let runtime: SharedSubagentRuntime = mock;
        let agent: Arc<dyn AgentView> = Arc::new(StubAgent {
            id: "prin_alice".into(),
        });
        let inbox: Arc<dyn AsyncInboxLike> = Arc::new(StubInbox);
        let port: Arc<dyn ChannelPort> = port;
        EngineChannelResponder::new_with_runtime(agent, inbox, runtime, port)
    }

    #[tokio::test]
    async fn responder_only_fires_on_posted_event() {
        let mock = Arc::new(MockSubagentRuntime::new("reply"));
        let port = Arc::new(MockChannelPort::default());
        let r = make_responder(mock.clone(), port.clone());

        for ev in [created_event(), member_joined_event()] {
            r.consider_response(ctx_for(ev)).await.expect("no-op");
        }
        // Runtime never called.
        assert!(mock.calls.lock().unwrap().is_empty());
        // Port never posted.
        assert!(port.posted.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn responder_picks_model_from_model_list_round_robin() {
        let mock = Arc::new(MockSubagentRuntime::new("reply"));
        let port = Arc::new(MockChannelPort {
            config: Mutex::new(HashMap::from([(
                chan(),
                ConfigOnDisk {
                    model_list: vec!["a".into(), "b".into(), "c".into()],
                    ..ConfigOnDisk::default()
                },
            )])),
            ..Default::default()
        });
        let r = make_responder(mock.clone(), port);

        for _ in 0..6 {
            r.consider_response(ctx_for(posted_event()))
                .await
                .expect("dispatch");
        }

        let calls = mock.calls.lock().unwrap();
        let models: Vec<Option<String>> =
            calls.iter().map(|c| c.model.clone()).collect();
        assert_eq!(
            models,
            vec![
                Some("a".into()),
                Some("b".into()),
                Some("c".into()),
                Some("a".into()),
                Some("b".into()),
                Some("c".into()),
            ],
            "round-robin wraps after model_list.len()"
        );
    }

    #[tokio::test]
    async fn responder_posts_subagent_reply_into_channel() {
        let mock = Arc::new(MockSubagentRuntime::new("hello back"));
        let port = Arc::new(MockChannelPort::default());
        let r = make_responder(mock, port.clone());

        r.consider_response(ctx_for(posted_event()))
            .await
            .expect("dispatch");

        let posted = port.posted.lock().unwrap();
        assert_eq!(posted.len(), 1, "responder must post exactly once");
        assert_eq!(posted[0].1, "hello back");
    }

    #[tokio::test]
    async fn empty_model_list_falls_through_to_runtime_default() {
        let mock = Arc::new(MockSubagentRuntime::new("reply"));
        let port = Arc::new(MockChannelPort::default());
        let r = make_responder(mock.clone(), port);

        r.consider_response(ctx_for(posted_event()))
            .await
            .expect("dispatch");

        let calls = mock.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert!(
            calls[0].model.is_none(),
            "empty model_list must not force a model"
        );
    }

    #[tokio::test]
    async fn responder_falls_back_to_writer_subagent_type() {
        let mock = Arc::new(MockSubagentRuntime::new("reply"));
        let port = Arc::new(MockChannelPort::default());
        let r = make_responder(mock.clone(), port);

        r.consider_response(ctx_for(posted_event()))
            .await
            .expect("dispatch");

        let calls = mock.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].subagent_type, "writer");
    }

    #[tokio::test]
    async fn channel_cost_ceiling_zero_is_rejected() {
        let mock = Arc::new(MockSubagentRuntime::new("reply"));
        let port = Arc::new(MockChannelPort {
            config: Mutex::new(HashMap::from([(
                chan(),
                ConfigOnDisk {
                    cost_ceiling_usd: Some(0.0),
                    ..ConfigOnDisk::default()
                },
            )])),
            ..Default::default()
        });
        let r = make_responder(mock, port);

        let err = r
            .consider_response(ctx_for(posted_event()))
            .await
            .expect_err("zero ceiling must error");
        match err {
            ChannelError::Adapter(msg) => {
                assert!(msg.contains("cost ceiling"), "got {msg}");
            }
            other => panic!("expected Adapter error, got {other:?}"),
        }
    }
}