//! Subagent Executor
//!
//! Async task executor for subagents. Handles:
//! - Spawning subagent sessions
//! - Executing agents in those sessions
//! - Tracking run status via the unified async task registry
//! - Announcing results back to parents
//! - Timeout and cancellation handling
//!
//! All state is stored in the unified `AsyncTaskRegistry` (see Issue 008).
//! This module no longer maintains a separate `SubagentRegistry`.
//!
//! The executor carries a `principal_id` (the spawning principal's DID)
//! rather than an `Arc<ExtensionCore>` — there is one daemon-global
//! [`crate::extensions::framework::core::ExtensionCore`] (`global_core()`) and
//! principals share it. Per-principal tool instances (sessions/memory/
//! catalog) are registered on that single global core keyed by the
//! principal, so per-subagent visibility is still scoped to the
//! principal's tool bag without each subagent needing its own core.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use tokio::sync::RwLock;
use tracing::{error, info, warn};

use crate::agents::agent_config::AgentConfig;
use crate::agents::subagent_announce::{build_subagent_system_prompt, build_subagent_task_message};
use crate::agents::subagent_error::SpawnError;
use crate::agents::subagent_types::SubagentRunView;
use crate::extensions::framework::async_exec::executor::{
    get_or_create_registry_for_agent, AsyncExecutor, AsyncResultDeliveryMode,
    AsyncResultQueueManager, AsyncTaskStatus, AsyncToolConfig, SharedAsyncResultQueueManager,
    SharedAsyncTaskRegistry, SubagentMetadata, TaskMetadata, WaitResult,
};
use crate::extensions::framework::async_exec::executor::{
    AsyncTaskStatus as SubagentStatus, SubagentResult,
};
use peko_extension_api::SpawnCleanupPolicy;
use crate::extensions::framework::types::Capabilities;
use peko_auth::Subject;
use peko_observability::Observability;
use peko_session::context::SessionContext;
use peko_session::manager::SessionManager;
use peko_subject::PrincipalId;

// B4 cleanup: the announcement cluster (`CompletedRun`,
// `with_announcement_channel`, `create_announcement_channel`,
// `get_completed_for_announcement`, `announcement_sender`,
// `send_announcement`, `subagent_announce::format_announcement`)
// was retired end-to-end. The `announce_completion` field on
// `SubagentRunView` is kept for backward-compat reads but no
// longer has any writer; remove it in a follow-up once the
// index-of-views migrates to a typed "delivery" channel.

/// Shared streaming event sink for child turns (agent-session
/// paradigm, sprint 2 Phase 6).
///
/// Carries the same `AgenticEvent` stream shape the IPC
/// `principal_send` drain loop consumes (`AssistantDelta` /
/// `AssistantText` / `Lifecycle` / `ToolStart` / `ToolEnd` / `Usage`)
/// — a streaming child turn feeds the caller's sink directly instead
/// of dropping events like the final-only resume path. `Arc`-shared
/// so the sink can move into the spawned run task while the caller
/// keeps no borrow.
pub type AgenticEventSink = Arc<dyn Fn(peko_engine::AgenticEvent) + Send + Sync>;

/// Completion summary of a streaming child turn
/// ([`SubagentExecutor::resume_streaming`]).
///
/// `final_text` is the child session's final answer (what the IPC
/// handler's `PrincipalSent`/`PrincipalSentDone` `content` carries
/// today). `token_usage` is the `(input, output, total)` projection
/// recorded on the run's `SubagentResult`; the per-call `Usage` event
/// also flows through the event sink, so streaming callers that
/// accumulate from events (the IPC drain loop) do not need this.
#[derive(Debug, Clone)]
pub struct StreamingResumeOutcome {
    /// The registered run id (AsyncTaskRegistry key).
    pub run_id: String,
    /// The child agent's final answer.
    pub final_text: String,
    /// Token usage `(input, output, total)` accumulated over the run.
    pub token_usage: Option<(usize, usize, usize)>,
}

/// Configuration for subagent execution
#[derive(Debug, Clone)]
pub struct ExecutionConfig {
    /// Maximum execution time in seconds (0 = unlimited)
    pub timeout_seconds: u64,
    /// Cleanup policy for the session
    pub cleanup: SpawnCleanupPolicy,
    /// Optional label for the run
    pub label: Option<String>,
    /// Whether to announce completion to parent
    pub announce_completion: bool,
    /// Maximum spawn depth (0 = unlimited)
    pub max_depth: u32,
    /// Phase 1 of `feature/multi-model-subagents`: optional
    /// catalog model id the parent picked for this spawn. The
    /// adapter at `agents::subagent_runtime_impl.rs` projects this
    /// from the built-in's `ExecutionConfig::model_override`. When
    /// `Some`, `execute_subagent_task` clones the inherited
    /// provider and overrides `default_model_id` before handing it
    /// to `Agent::new_with_shared_executor`. `None` means
    /// "inherit the parent's model verbatim" (pre-Phase-1
    /// behavior).
    pub model_override: Option<String>,
    /// Slug for the child's session metadata (Agent tool `name`
    /// param) — the per-parent-unique path segment for `/a/b`
    /// addressing. `None` leaves the child slugless (the historical
    /// behavior). Validated + uniqueness-checked at spawn time in
    /// `spawn_and_execute`; the write lands via
    /// `SessionManager::set_session_slug` right after the spawn
    /// session is created (the same index entry
    /// `stamp_spawn_parent_linkage` patches).
    pub slug: Option<String>,
    /// The requested agent template (Agent tool `agent` param),
    /// threaded by the runtime adapter for the standing-child
    /// attach check: when `slug` matches a standing session that
    /// carries a `[children]` declaration, the requested type must
    /// match the declared one. `None` skips the check (callers that
    /// don't know the type).
    pub agent: Option<String>,
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            timeout_seconds: 300, // 5 minutes default
            cleanup: SpawnCleanupPolicy::Keep,
            label: None,
            announce_completion: true,
            max_depth: 1, // Default: no nested spawns
            model_override: None,
            slug: None,
            agent: None,
        }
    }
}

/// Everything [`SubagentExecutor::register_subagent_run`] needs to
/// register + execute one subagent run, independent of whether the
/// child session was freshly spawned or re-attached (Agent tool
/// `action = "resume"`).
struct SubagentRunSpec {
    run_id: String,
    task: String,
    /// The caller's current session (announcement target + ownership
    /// anchor for the guarded cleanup delete).
    parent_session_key: String,
    /// Registry-facing child key: the spawn overlay key for spawned
    /// runs, the plain session id for resumed runs.
    child_session_key: String,
    /// The child's plain session id (uuid / live id).
    child_session_id: String,
    /// The resolved child session (spawn: the new overlay's base;
    /// resume: the opened existing session with its prior history).
    child_base: Arc<RwLock<peko_session::Session>>,
    child_depth: u32,
    config: ExecutionConfig,
    parent_cancel: Option<tokio_util::sync::CancellationToken>,
    /// Streaming event sink (sprint 2 Phase 6). `Some` runs the child
    /// agent via `Agent::execute_streaming_with_session`
    /// (`OrchestratorConfig::live()`) and forwards every
    /// `AgenticEvent` to the sink; `None` keeps the final-only
    /// `execute_with_session` path (events dropped).
    stream_events: Option<AgenticEventSink>,
}

/// The output of [`SubagentExecutor::resume_preflight`]: everything
/// the registration step needs once the resume guard stack has
/// passed. Shared by the final-only (`resume_and_execute`) and
/// streaming (`resume_streaming`) resume paths.
struct ResumePreflight {
    run_id: String,
    /// The path-resolved canonical target session id.
    session_id: String,
    /// The opened existing session (prior history attached).
    child_base: Arc<RwLock<peko_session::Session>>,
    child_depth: u32,
}

/// Executor for subagent tasks
///
/// All task state lives in the unified `AsyncTaskRegistry`. This struct
/// orchestrates subagent-specific logic (session creation, depth tracking,
/// result formatting) but delegates all state storage to the framework.
#[derive(Clone)]
pub struct SubagentExecutor {
    /// Unified async executor for background task execution
    unified_executor: AsyncExecutor,
    /// Agent name for the executor
    agent_name: String,
    /// Maximum concurrent runs
    max_concurrent: usize,
    /// Provider for LLM execution
    provider: Option<Arc<peko_providers::Provider>>,
    /// Agent configuration for creating subagents
    agent_config: Option<AgentConfig>,
    /// Session manager for accessing sessions
    session_manager: Arc<RwLock<SessionManager>>,
    /// Optional principal workspace. When set, spawned subagents are scoped to
    /// this workspace so their own `Agent` tool resolves nested subagents from
    /// `<workspace>/agents/<name>/AGENT.md`. Propagated down the spawn tree so
    /// delegation works at every depth, not just the first level.
    principal_workspace: Option<std::path::PathBuf>,
    /// The spawning principal's runtime id. Carried so per-principal tool
    /// registration on the global core can be looked up without
    /// re-reading the principal context, and so descendant subagents
    /// inherit the same principal scope down the spawn tree.
    principal_id: PrincipalId,
    /// The spawning principal's human-readable name. Carried so
    /// Principal-scoped tools (e.g. cron) inherit the correct target.
    principal_name: Option<String>,
    /// Snapshot of the spawning principal's capability grants.
    /// `None` means unbound (no capability filtering). `Some(empty)`
    /// means deny-all. Propagated to descendant subagents so a
    /// restricted root agent cannot spawn a more-privileged child.
    principal_capabilities: Option<Arc<Capabilities>>,
    /// Snapshot of the spawning principal's active extension IDs.
    /// `None` means unbound (no active-extension check). Propagated to
    /// descendant subagents.
    active_extensions: Option<crate::extensions::framework::types::ActiveExtensionSet>,
    /// Optional observability hub for audit/metrics. When set, subagent
    /// spawns are recorded in the audit log under the parent principal.
    observability: Option<Arc<Observability>>,
    /// F39: snapshot of the spawning principal's `QuotaMeter`. The
    /// spawned `tokio::task` does NOT inherit the parent's
    /// `QuotaScope::with` task-local (F19 design assumption was wrong
    /// on this point — task-locals don't cross `tokio::spawn`), so
    /// the subagent re-opens its own `QuotaScope::with(this_meter, ...)`
    /// inside the spawned closure. `None` means
    /// `QuotaMeter::unlimited()` fallback so subagents of principals
    /// with no quota config still run (no behavior change vs F19).
    quota_meter: Option<Arc<peko_quota::meter::QuotaMeter>>,
    /// B5d (per-agent attribution, 2026-08-22): per-agent
    /// `QuotaMeter` constructed at executor `new` time. The agent
    /// name is the audit label; the meter is `QuotaMeter::unlimited()`
    /// by default so it never trips a cap, only accumulates counters
    /// for read-back via [`agent_meter_usage`](Self::agent_meter_usage).
    /// B5e wires the agent's loop to charge this meter on every
    /// LLM call inside a second `QuotaScope::with` (alongside the
    /// principal meter — both charge, neither trips the other).
    /// The principal's per-cycle consumption is then the sum of
    /// every agent's `agent_meter.snapshot()` for audit.
    agent_meter: Arc<peko_quota::meter::QuotaMeter>,
    /// Snapshot of the spawning principal's plan DAG port. Propagated
    /// into the spawned `Agent` via
    /// `Agent::with_principal_plan_port` so the seven `Plan*` tools
    /// are wired into the subagent's tool bag (depth-1+ children can
    /// manage plans on behalf of their spawning principal). `None`
    /// means unbound — subagents do not register `Plan*` tools.
    principal_plan_port: Option<Arc<dyn peko_plan::PlanPort>>,
    /// The spawning principal's DID, set post-construction from the
    /// parent Agent's `with_caller_principal_did` binding (a
    /// `OnceLock` so the set works through the `Arc` the parent
    /// holds). Bound onto each spawned child Agent so `send_peer`
    /// registers down the whole tree with correct caller attribution.
    caller_principal_did: std::sync::OnceLock<String>,
    /// WS3 (implicit session management, 2026-08-11): the daemon-shared
    /// inbox registry. Completions pushed by subagent spawns MUST land
    /// in this registry so the parent agentic loop's per-iteration
    /// drain sees them; otherwise WS3's `persist_subagent_completions`
    /// never fires. `None` falls back to a per-executor standalone
    /// registry (kept for tests and CLI one-shots where no daemon
    /// state is wired).
    inbox_registry: Option<Arc<peko_session::InboxRegistry>>,
}

impl SubagentExecutor {
    /// Create a new subagent executor
    ///
    /// Uses the global per-agent async task registry so that status queries
    /// and result delivery work across stateless requests.
    ///
    /// `principal_id` is the spawning principal's runtime id. There is no
    /// per-principal `ExtensionCore` — the executor and its subagents look
    /// tools up on the daemon-global
    /// [`crate::extensions::framework::core::global_core`].
    #[must_use]
    pub fn new(
        session_manager: Arc<RwLock<SessionManager>>,
        agent_name: impl Into<String>,
        max_concurrent: usize,
        principal_id: PrincipalId,
    ) -> Self {
        let agent_name = agent_name.into();
        let async_registry = get_or_create_registry_for_agent(&agent_name);
        let async_queue_manager = Arc::new(RwLock::new(AsyncResultQueueManager::new()));
        let unified_executor = AsyncExecutor::with_registries(
            async_registry,
            async_queue_manager,
            crate::extensions::framework::async_exec::executor::standalone_inbox_registry(),
        );

        Self {
            unified_executor,
            agent_name,
            max_concurrent,
            provider: None,
            agent_config: None,
            session_manager,
            principal_workspace: None,
            principal_id,
            principal_name: None,
            principal_capabilities: None,
            active_extensions: None,
            observability: None,
            quota_meter: None,
            // B5d: per-agent attribution meter. Audit-only by default;
            // B5e wires it into the LLM call path alongside the
            // principal meter.
            agent_meter: Arc::new(peko_quota::meter::QuotaMeter::unlimited()),
            principal_plan_port: None,
            caller_principal_did: std::sync::OnceLock::new(),
            inbox_registry: None,
        }
    }

    /// WS3 (implicit session management, 2026-08-11): bind the
    /// daemon-shared inbox registry so subagent completions land in
    /// the same registry the parent agentic loop drains. Without
    /// this binding the executor's `AsyncExecutor` creates its own
    /// private registry and WS3's `persist_subagent_completions`
    /// hook never fires for production runs.
    #[must_use]
    pub fn with_inbox_registry(
        mut self,
        registry: Option<Arc<peko_session::InboxRegistry>>,
    ) -> Self {
        if let Some(reg) = registry {
            self.inbox_registry = Some(reg.clone());
            // Rebuild the AsyncExecutor against the shared registry so
            // completion pushes actually reach the loop's drain site.
            let async_registry = get_or_create_registry_for_agent(&self.agent_name);
            let async_queue_manager = Arc::new(RwLock::new(AsyncResultQueueManager::new()));
            self.unified_executor =
                AsyncExecutor::with_registries(async_registry, async_queue_manager, reg);
        }
        self
    }

    /// F39: set the spawning principal's `QuotaMeter`. The subagent
    /// re-opens this meter via `QuotaScope::with` inside the spawned
    /// task (task-locals don't cross `tokio::spawn` — F19's design
    /// assumption on this point was incorrect).
    #[must_use]
    pub fn with_quota_meter(mut self, meter: Option<Arc<peko_quota::meter::QuotaMeter>>) -> Self {
        self.quota_meter = meter;
        self
    }

    /// F39: get the spawning principal's `QuotaMeter`, if set.
    #[must_use]
    pub fn quota_meter(&self) -> Option<&Arc<peko_quota::meter::QuotaMeter>> {
        self.quota_meter.as_ref()
    }

    /// B5d: bind a per-agent `QuotaMeter`. Replaces the
    /// `unlimited()` default constructed at `new` time. Use this
    /// when a caller wants the agent meter to enforce a real cap
    /// instead of accumulating audit-only counters. The audit-label
    /// behavior (counters always readable via
    /// [`agent_meter_usage`](Self::agent_meter_usage)) is unchanged.
    #[must_use]
    pub fn with_agent_meter(mut self, meter: Arc<peko_quota::meter::QuotaMeter>) -> Self {
        self.agent_meter = meter;
        self
    }

    /// B5d: the per-agent `QuotaMeter`. Always `Some` (the
    /// constructor seeds an unlimited fallback). Returned by
    /// reference so the inner `Arc` can be cloned cheaply.
    #[must_use]
    pub fn agent_meter(&self) -> &Arc<peko_quota::meter::QuotaMeter> {
        &self.agent_meter
    }

    /// B5d: snapshot the agent's accumulated usage (input /
    /// output tokens, request count, cost) inside the current
    /// quota window. Audit-only — the principal can sum every
    /// agent's snapshot to compute its per-window consumption
    /// (`sum(agent_meters)` for each agent ever spawned by the
    /// principal). `QuotaMeter::unlimited()` meters still
    /// accumulate counters; only `check()` is short-circuited.
    #[must_use]
    pub fn agent_meter_usage(&self) -> peko_quota::QuotaState {
        self.agent_meter.snapshot()
    }

    /// Phase 3 of `feature/multi-model-subagents`: the bound
    /// provider's `Arc` for cost estimation. Used by
    /// `SubagentRuntime::spawn_cost_estimate_usd` to pull the
    /// `ModelSpec::pricing` field at audit time. Returns `None`
    /// when no provider has been bound (test paths).
    #[must_use]
    pub fn provider_for_cost_estimate(&self) -> Option<&Arc<peko_providers::Provider>> {
        self.provider.as_ref()
    }

    /// Get the spawning principal's runtime id.
    #[must_use]
    pub fn principal_id(&self) -> &PrincipalId {
        &self.principal_id
    }

    /// Bind the spawning principal's DID (post-construction; set
    /// through the `Arc` the parent Agent holds). Idempotent — later
    /// sets are ignored, matching `PrincipalContext::set_caller_principal_did`.
    pub fn set_caller_principal_did(&self, did: String) {
        let _ = self.caller_principal_did.set(did);
    }

    /// The spawning principal's DID, if bound.
    #[must_use]
    pub fn caller_principal_did(&self) -> Option<&String> {
        self.caller_principal_did.get()
    }

    /// Get the spawning principal's human-readable name, if known.
    #[must_use]
    pub fn principal_name(&self) -> Option<&str> {
        self.principal_name.as_deref()
    }

    /// Set the spawning principal's human-readable name.
    #[must_use]
    pub fn with_principal_name(mut self, name: impl Into<String>) -> Self {
        self.principal_name = Some(name.into());
        self
    }

    /// Set the spawning principal's capability snapshot.
    #[must_use]
    pub fn with_principal_capabilities(mut self, capabilities: Option<Arc<Capabilities>>) -> Self {
        self.principal_capabilities = capabilities;
        self
    }

    /// Set the active extension set for the spawning principal.
    #[must_use]
    pub fn with_active_extensions(
        mut self,
        active_extensions: Option<crate::extensions::framework::types::ActiveExtensionSet>,
    ) -> Self {
        self.active_extensions = active_extensions;
        self
    }

    /// Get the spawning principal's capability snapshot, if bound.
    #[must_use]
    pub fn principal_capabilities(&self) -> Option<&Arc<Capabilities>> {
        self.principal_capabilities.as_ref()
    }

    /// Get the active extension set, if bound.
    #[must_use]
    pub fn active_extensions(
        &self,
    ) -> Option<&crate::extensions::framework::types::ActiveExtensionSet> {
        self.active_extensions.as_ref()
    }

    /// Set the observability hub used to audit subagent spawns.
    #[must_use]
    pub fn with_observability(mut self, observability: Option<Arc<Observability>>) -> Self {
        self.observability = observability;
        self
    }

    /// Get the observability hub, if bound.
    #[must_use]
    pub fn observability(&self) -> Option<&Arc<Observability>> {
        self.observability.as_ref()
    }

    /// Create an executor with an explicit registry (for testing and nested spawns)
    #[must_use]
    pub fn with_registry(
        async_registry: SharedAsyncTaskRegistry,
        session_manager: Arc<RwLock<SessionManager>>,
        agent_name: impl Into<String>,
        max_concurrent: usize,
        principal_id: PrincipalId,
    ) -> Self {
        let async_queue_manager = Arc::new(RwLock::new(AsyncResultQueueManager::new()));
        let unified_executor = AsyncExecutor::with_registries(
            async_registry,
            async_queue_manager,
            crate::extensions::framework::async_exec::executor::standalone_inbox_registry(),
        );

        Self {
            unified_executor,
            agent_name: agent_name.into(),
            max_concurrent,
            provider: None,
            agent_config: None,
            session_manager,
            principal_workspace: None,
            principal_id,
            principal_name: None,
            principal_capabilities: None,
            active_extensions: None,
            observability: None,
            quota_meter: None,
            // B5d: per-agent attribution meter. Audit-only by default;
            // B5e wires it into the LLM call path alongside the
            // principal meter.
            agent_meter: Arc::new(peko_quota::meter::QuotaMeter::unlimited()),
            principal_plan_port: None,
            caller_principal_did: std::sync::OnceLock::new(),
            inbox_registry: None,
        }
    }

    /// Set the provider for LLM execution
    #[must_use]
    pub fn with_provider(mut self, provider: Arc<peko_providers::Provider>) -> Self {
        self.provider = Some(provider);
        self
    }

    /// Set the agent configuration
    #[must_use]
    pub fn with_agent_config(mut self, config: AgentConfig) -> Self {
        self.agent_config = Some(config);
        self
    }

    /// The agent configuration snapshot child runs inherit, if bound.
    /// `None` means [`execute_subagent_task`] falls back to a default
    /// config with `prompt: None` (the pre-Phase-6 blank-persona gap).
    #[must_use]
    pub fn agent_config(&self) -> Option<&AgentConfig> {
        self.agent_config.as_ref()
    }

    /// Scope spawned subagents to a Principal workspace so nested delegation
    /// resolves subagents from `<workspace>/agents/<name>/AGENT.md`.
    #[must_use]
    pub fn with_principal_workspace(mut self, workspace: std::path::PathBuf) -> Self {
        self.principal_workspace = Some(workspace);
        self
    }

    /// Snapshot of the spawning principal's workspace, if bound.
    /// Used by `SubagentExecutorRuntime::workspace()` so the
    /// built-in `AgentTool` resolves subagent configs from the
    /// principal's `<workspace>/agents/` directory before falling
    /// back to the global layout.
    #[must_use]
    pub fn principal_workspace(&self) -> Option<&std::path::Path> {
        self.principal_workspace.as_deref()
    }

    /// Set the spawning principal's plan DAG port. Propagated into the
    /// spawned `Agent` via `Agent::with_principal_plan_port` so depth-1
    /// children register the seven `Plan*` built-in tools against the
    /// same per-Principal store. `None` is the default; depth-1+
    /// children of unbound principals do not register `Plan*` tools.
    #[must_use]
    pub fn with_principal_plan_port(mut self, plan_port: Arc<dyn peko_plan::PlanPort>) -> Self {
        self.principal_plan_port = Some(plan_port);
        self
    }

    /// Snapshot of the spawning principal's plan DAG port, if bound.
    #[must_use]
    pub fn principal_plan_port(&self) -> Option<&Arc<dyn peko_plan::PlanPort>> {
        self.principal_plan_port.as_ref()
    }

    /// Get a reference to the async task registry (unified)
    #[must_use]
    pub fn registry(&self) -> &SharedAsyncTaskRegistry {
        self.unified_executor.registry()
    }

    /// Get a reference to the async queue manager
    #[must_use]
    pub fn async_queue_manager(&self) -> &SharedAsyncResultQueueManager {
        self.unified_executor.queue_manager()
    }

    /// Get a reference to the unified executor
    #[must_use]
    pub fn unified_executor(&self) -> &AsyncExecutor {
        &self.unified_executor
    }

    /// Spawn and execute a subagent
    ///
    /// Returns the `run_id` immediately. The execution happens in the background.
    ///
    /// `parent_cancel` is the soft-interrupt `CancellationToken` from
    /// the parent agent's `AgenticLoop` (PR #128). When set, a
    /// `child_token()` is derived so the sub-agent's own
    /// `AgenticLoop` observes a cancel at iteration boundaries —
    /// closing the gap where interrupting a parent left its
    /// sub-agents running. The child token also fires on
    /// `is_cancelled()` inside the closure below so the
    /// `AsyncTaskStatus::Cancelled` write path runs cleanly.
    pub async fn spawn_and_execute(
        &self,
        task: &str,
        parent_session_key: &str,
        config: ExecutionConfig,
        parent_cancel: Option<tokio_util::sync::CancellationToken>,
    ) -> Result<String> {
        // B4 cleanup: `parent_ctx` and `isolated` were both dropped.
        // `parent_ctx` was only consumed by the dead `validate_context_parent`
        // branch (deleted below); `isolated` was never deserialized
        // from `AgentArgs` and `SessionManager::spawn_session` now
        // always runs in shared-context mode.
        // Standing-child attach (agent-session paradigm, Phase 2): a
        // `name` (config.slug) that already matches a STANDING spawned
        // session in the caller's subtree re-attaches to it via the
        // resume path instead of minting a fresh session — the call's
        // `task` drives the turn. A `name` colliding with a
        // non-standing session is a structured refusal (rename
        // semantics live in the session tool). This runs BEFORE the
        // cost pre-flight and before anything is minted;
        // `resume_and_execute` re-runs the full guard stack (cost
        // pre-flight included) for the attach branch.
        if let Some(ref slug) = config.slug {
            use crate::session::ownership::{caller_context, descendants_of};
            let (caller, metas) = {
                let mut manager = self.session_manager.write().await;
                let metas = manager.list_all_sessions(false).await?;
                (caller_context(parent_session_key, &metas), metas)
            };
            // The caller's subtree = its own session + all descendants.
            let subtree: std::collections::HashSet<String> =
                descendants_of(&caller.current_session_id, &metas)
                    .into_iter()
                    .chain(std::iter::once(caller.current_session_id.clone()))
                    .collect();
            let found = metas.iter().find(|m| {
                m.slug.as_deref() == Some(slug.as_str())
                    && subtree.contains(&m.session_id.to_string())
            });
            if let Some(found) = found {
                if found.standing && found.trigger == "spawn" {
                    let child_id = found.session_id.to_string();
                    // When the session carries a `[children]`
                    // declaration, the requested agent template must
                    // match it. Unrecoverable declarations (no event /
                    // unreadable JSONL) skip the check — the param is
                    // required as usual.
                    if let Some(ref requested) = config.agent {
                        let sessions_dir =
                            self.session_manager.read().await.sessions_dir().cloned();
                        if let Some(dir) = sessions_dir {
                            if let Some(declared) =
                                crate::session::standing::declared_subagent_type(&dir, &child_id)
                                    .await
                            {
                                if declared != *requested {
                                    return Err(
                                        crate::session::standing::err_declared_type_mismatch(
                                            slug, &child_id, &declared, requested,
                                        ),
                                    );
                                }
                            }
                        }
                    }
                    info!(
                        "Attaching to standing child session: slug={} session={}",
                        slug, child_id
                    );
                    return self
                        .resume_and_execute(
                            task,
                            // B2 fix: pass the slug path the model
                            // would have supplied (`/<slug>`), not the
                            // raw UUID. `resume_and_execute` resolves
                            // via `resolve_reference` which refuses raw
                            // UUIDs (the v5-derive fallback silently
                            // misrouted pre-fix; see PR review
                            // finding B2).
                            &format!("/{slug}"),
                            parent_session_key,
                            config,
                            parent_cancel,
                        )
                        .await;
                }
                // Non-standing collision. A DIRECT sibling keeps the
                // Phase 1b per-parent uniqueness refusal (identical to
                // what the fresh-spawn pre-flight below would produce);
                // a non-sibling subtree collision gets the
                // standing-specific structured refusal.
                let found_parent_str = found.parent_session_id.map(|id| id.to_string());
                // Sprint 6: `parent_session_id` in metadata is the
                // canonical v5 UUID form of the parent's session id;
                // canonicalize the input here so the sibling-vs-foreign
                // branch picks the right error.
                let parent_key = peko_session::SessionId::from(parent_session_key).to_string();
                return Err(
                    if found_parent_str.as_deref() == Some(parent_key.as_str()) {
                        peko_session::path::err_slug_conflict(
                            slug,
                            found.session_id,
                            Some(peko_session::SessionId::from(parent_key.as_str())),
                        )
                    } else {
                        crate::session::standing::err_name_not_standing(
                            slug,
                            &found.session_id.as_str(),
                        )
                    },
                );
            }
        }

        // Phase 3 — spawn-time pre-flight against
        // `cost_per_call_max`. Conservative token projection
        // (4K input + 1K output) multiplied by the chosen
        // model's `PricingHint`. Refuses the spawn before any
        // LLM traffic. No pre-flight when:
        //   * the principal has no quota config (`quota_meter`
        //     is `None`),
        //   * `cost_per_call_max` is `None` (no per-call cap),
        //   * the chosen provider has no `PricingHint` (local
        //     / unpriced model — can't estimate).
        if let Some(err) =
            pre_flight_cost_ceiling(self.quota_meter.as_deref(), self.provider.as_ref())
        {
            return Err(anyhow::anyhow!(err));
        }

        // Check depth limits.
        //
        // B8c.3: read the durable metadata chain via the unified
        // `subagent_depth_of` helper (same shape as the resume path
        // at `resume_and_execute`) instead of the
        // `AsyncTaskRegistry`'s in-memory task map. The registry
        // entries are GC'd after `cleanup_completed` (5 minutes) and
        // lost on daemon restart, so the depth check could let a
        // deeper-than-allowed spawn through the moment a parent run
        // aged out — the metadata chain is the durable answer.
        let child_depth = {
            use crate::session::ownership::subagent_depth_of;
            let metas = self
                .session_manager
                .write()
                .await
                .list_all_sessions(false)
                .await?;
            // The new subagent's depth = 1 + depth of its parent.
            // When `parent_session_key` itself is a spawned subagent,
            // the helper counts it as 1, so spawning from a depth-N
            // subagent yields depth N+1 — the test_spawn_depth_limit
            // invariant.
            1 + subagent_depth_of(parent_session_key, &metas)
        };

        if config.max_depth > 0 && child_depth > config.max_depth {
            return Err(anyhow::anyhow!(SpawnError::DepthLimitExceeded {
                current: child_depth,
                max: config.max_depth,
            }));
        }

        // Check concurrent run limits
        let active_count = self.count_active_runs().await;
        if active_count >= self.max_concurrent {
            return Err(anyhow::anyhow!(SpawnError::ConcurrentLimitExceeded {
                current: active_count,
                max: self.max_concurrent,
            }));
        }

        // Slug pre-flight (Agent tool `name` param): validate the
        // format and check per-parent uniqueness BEFORE spawning, so a
        // conflict refuses before any session is created. Siblings are
        // the existing children of the parent session. The production
        // path passes the caller's session id as `parent_session_key`;
        // when it doesn't resolve to a metadata entry (legacy session
        // KEY shapes) the pre-check is skipped and the post-spawn
        // `set_session_slug` below still enforces the invariant.
        if let Some(ref slug) = config.slug {
            peko_session::path::validate_slug(slug)?;
            let mut manager = self.session_manager.write().await;
            let metas = manager.list_all_sessions(false).await?;
            if metas
                .iter()
                .any(|m| m.session_id.to_string() == parent_session_key)
            {
                let parent_id = peko_session::SessionId::from(parent_session_key);
                if let Some(conflict) = peko_session::path::slug_conflict(
                    &metas,
                    Some(parent_id),
                    slug,
                    peko_session::SessionId::new(),
                ) {
                    return Err(peko_session::path::err_slug_conflict(
                        slug,
                        conflict,
                        Some(parent_id),
                    ));
                }
            }
        }

        // Generate run ID
        let run_id = format!("run_{}", uuid::Uuid::new_v4().simple());

        // Create spawn session
        let peer = Subject::Principal(format!("spawn_{}", uuid::Uuid::new_v4().simple()).into());
        let spawn_resolved = {
            let mut manager = self.session_manager.write().await;
            manager
                .spawn_session(
                    &self.agent_name,
                    &peer,
                    task,
                    parent_session_key,
                    Some(config.timeout_seconds),
                )
                .await
                .context("Failed to create spawn session")?
        };

        let child_session_key = spawn_resolved.context.full_session_key.clone();
        let child_session_id = spawn_resolved.context.session_id.clone();
        let child_base = spawn_resolved.handle.base().clone();

        // Stamp the child's slug (Agent tool `name`) onto the same
        // index entry `stamp_spawn_parent_linkage` just patched. The
        // pre-flight above already validated format + uniqueness when
        // the parent resolved; `set_session_slug` re-enforces both
        // (closing the race with a concurrent same-name spawn) and a
        // failure here refuses the spawn — the run is not yet
        // registered, so no LLM traffic has happened.
        if let Some(ref slug) = config.slug {
            self.session_manager
                .read()
                .await
                .set_session_slug(&child_session_id, Some(slug.clone()))
                .await?;
        }

        info!("Spawned subagent: run_id={} depth={}", run_id, child_depth);

        self.register_subagent_run(SubagentRunSpec {
            run_id,
            task: task.to_string(),
            parent_session_key: parent_session_key.to_string(),
            child_session_key,
            child_session_id,
            child_base,
            child_depth,
            config,
            parent_cancel,
            stream_events: None,
        })
        .await
    }

    /// Re-attach a new task run to an existing spawned session
    /// (`Agent` tool's `action = "resume"` — persistent subagents).
    ///
    /// Skips `spawn_session`: the target session is opened as-is, so
    /// the run continues with its full prior history. All D4 guards
    /// from the shared ownership module apply (enforced in
    /// [`Self::resume_preflight`], shared with
    /// [`Self::resume_streaming`]). The caller's current session is
    /// `parent_session_key` (the caller's session id on the production
    /// path).
    pub async fn resume_and_execute(
        &self,
        task: &str,
        resume_session_id: &str,
        parent_session_key: &str,
        config: ExecutionConfig,
        parent_cancel: Option<tokio_util::sync::CancellationToken>,
    ) -> Result<String> {
        let pre = self
            .resume_preflight(resume_session_id, parent_session_key, &config)
            .await?;

        info!(
            "Resuming subagent session: run_id={} session={} depth={}",
            pre.run_id, pre.session_id, pre.child_depth
        );

        self.register_subagent_run(SubagentRunSpec {
            run_id: pre.run_id,
            task: task.to_string(),
            parent_session_key: parent_session_key.to_string(),
            // No spawn overlay exists for a resumed session — register
            // with the plain session id in both slots.
            child_session_key: pre.session_id.clone(),
            child_session_id: pre.session_id,
            child_base: pre.child_base,
            child_depth: pre.child_depth,
            config,
            parent_cancel,
            stream_events: None,
        })
        .await
    }

    /// Streaming variant of [`Self::resume_and_execute`] (agent-session
    /// paradigm, sprint 2 Phase 6): the same resume guard stack and
    /// run registration, but the child agent runs via
    /// `Agent::execute_streaming_with_session`
    /// (`OrchestratorConfig::live()`), forwarding every
    /// [`peko_engine::AgenticEvent`] to `on_event` — the exact stream
    /// shape the IPC `principal_send` drain loop consumes — and this
    /// call blocks until the run reaches a terminal state, returning
    /// the final text + token usage.
    ///
    /// Built for the per-peer standing-child ingress paths (Phase 7
    /// swaps `route_streaming` for this): the shared registry key is
    /// load-bearing, so a streaming turn and a channel-driven or
    /// Agent-tool turn on the same child can never double-run.
    /// `parent_cancel` is observed by the child loop at iteration
    /// boundaries; a cancelled run surfaces as an error here.
    pub async fn resume_streaming(
        &self,
        task: &str,
        resume_session_id: &str,
        parent_session_key: &str,
        config: ExecutionConfig,
        on_event: AgenticEventSink,
        parent_cancel: Option<tokio_util::sync::CancellationToken>,
    ) -> Result<StreamingResumeOutcome> {
        // Wait slack mirrors the channel driver's
        // COMPLETION_WAIT_MARGIN_SECS: the run's own timeout fires
        // inside the task, so the waiter gives it room to land its
        // terminal status before declaring a wait timeout.
        // `timeout_seconds == 0` means unlimited — the wait must not
        // fire either.
        let wait_secs = if config.timeout_seconds == 0 {
            u64::MAX / 2
        } else {
            config.timeout_seconds + 30
        };

        let pre = self
            .resume_preflight(resume_session_id, parent_session_key, &config)
            .await?;

        info!(
            "Resuming subagent session (streaming): run_id={} session={} depth={}",
            pre.run_id, pre.session_id, pre.child_depth
        );

        let run_id = self
            .register_subagent_run(SubagentRunSpec {
                run_id: pre.run_id,
                task: task.to_string(),
                parent_session_key: parent_session_key.to_string(),
                // No spawn overlay exists for a resumed session —
                // register with the plain session id in both slots.
                child_session_key: pre.session_id.clone(),
                child_session_id: pre.session_id,
                child_base: pre.child_base,
                child_depth: pre.child_depth,
                config,
                parent_cancel,
                stream_events: Some(on_event),
            })
            .await?;

        let view = self.wait_for_run(&run_id, wait_secs).await?;
        let result = view.result.ok_or_else(|| {
            anyhow::anyhow!("streaming child run {run_id} completed without a result")
        })?;
        Ok(StreamingResumeOutcome {
            run_id,
            final_text: result.output.unwrap_or_default(),
            token_usage: result.token_usage,
        })
    }

    /// The full resume guard stack + session open shared by
    /// [`Self::resume_and_execute`] (final-only) and
    /// [`Self::resume_streaming`] (live event stream) so the two can
    /// never drift. Runs every D4 guard from the shared ownership
    /// module (see inline comments) plus the spawn-time cost
    /// pre-flight — a resume is still LLM traffic against the
    /// principal's meter. The caller's current session is
    /// `parent_session_key` (the caller's session id on the
    /// production path).
    async fn resume_preflight(
        &self,
        resume_session_id: &str,
        parent_session_key: &str,
        config: &ExecutionConfig,
    ) -> Result<ResumePreflight> {
        use crate::session::ownership::{
            caller_context, err_out_of_tree, err_resume_archived, err_resume_into_own_run,
            err_resume_not_spawned, err_run_active, in_subtree,
        };

        // Same spawn-time cost pre-flight as the spawn path — a resume
        // is still LLM traffic against the principal's meter.
        if let Some(err) =
            pre_flight_cost_ceiling(self.quota_meter.as_deref(), self.provider.as_ref())
        {
            return Err(anyhow::anyhow!(err));
        }

        let (caller, metas) = {
            let mut manager = self.session_manager.write().await;
            let metas = manager.list_all_sessions(false).await?;
            (caller_context(parent_session_key, &metas), metas)
        };

        // Resolve the LLM-facing slug path to a canonical session id
        // at the runtime boundary. `resolve_reference` accepts only
        // `/`-prefixed paths and the caller's own id; raw UUIDs and
        // caller-relative slugs are refused with a structured error
        // (the tool layer also pre-validates shape via `validate_path`,
        // but this is the authoritative resolution point — bypassing
        // it via `SessionId::from`'s v5 fallback would silently
        // misroute `/writer-1` to a deterministic UUID that doesn't
        // match any metadata, see PR review finding B2).
        let caller_id = peko_session::SessionId::from(caller.current_session_id.as_str());
        let resolved = peko_session::path::resolve_reference(&metas, caller_id, resume_session_id)?;
        let resume_session_id = resolved.as_str();

        // Guard: target must exist. Defense-in-depth — `resolve_reference`
        // already walks the slug path against `metas`, but a stale
        // metadata view between resolution and this lookup is theoretically
        // possible and the per-call guards want to be self-contained.
        let target_meta = metas
            .iter()
            .find(|m| m.session_id.to_string() == resume_session_id)
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "session '{resume_session_id}' not found — pass a slug path from the \
                     session tool's list (`path` field)"
                )
            })?;
        // Guard: cannot run inside the caller's own session or an
        // ancestor (would re-enter the caller's own run).
        if resume_session_id == caller.current_session_id
            || caller
                .ancestors
                .iter()
                .any(|a| a.as_str() == resume_session_id)
        {
            return Err(err_resume_into_own_run(&resume_session_id));
        }
        // Guard: only spawned subagent sessions can be re-attached.
        if target_meta.trigger != "spawn" {
            return Err(err_resume_not_spawned(&resume_session_id));
        }
        // Guard: subtree callers stay inside their subtree (principal-
        // level callers pass automatically).
        if !caller.is_base && !caller.privileged && !in_subtree(&caller, &resume_session_id, &metas)
        {
            return Err(err_out_of_tree(
                &resume_session_id,
                &caller.current_session_id,
            ));
        }
        // Guard: archived sessions have no business running.
        if target_meta.archived {
            return Err(err_resume_archived(&resume_session_id));
        }
        // Guard: refuse while a run is in flight for the target.
        // Mechanism: subagent runs do NOT hold `InboxRegistry` run
        // permits (those are only acquired for root sessions by the
        // IPC/principal-manager paths); the unified AsyncTaskRegistry
        // is the source of truth for subagent runs, keyed by
        // `child_session_key` / `child_session_id` in the task
        // metadata.
        {
            let registry = self.registry().read().await;
            if registry.has_active_subagent_run_for_child(&resume_session_id) {
                return Err(err_run_active(&resume_session_id));
            }
        }

        // Depth comes from the target session's OWN persisted parent
        // chain — not caller depth + 1: re-attaching keeps the
        // session's original spawn depth, so the depth-limit check
        // stays correct for the sub-tree below it even across daemon
        // restarts and registry GC. B8c.3 unifies this onto
        // `subagent_depth_of`, the same helper the spawn path uses,
        // so the two answers cannot drift.
        let child_depth =
            crate::session::ownership::subagent_depth_of(&resume_session_id, &metas);
        if config.max_depth > 0 && child_depth > config.max_depth {
            return Err(anyhow::anyhow!(SpawnError::DepthLimitExceeded {
                current: child_depth,
                max: config.max_depth,
            }));
        }

        // Check concurrent run limits
        let active_count = self.count_active_runs().await;
        if active_count >= self.max_concurrent {
            return Err(anyhow::anyhow!(SpawnError::ConcurrentLimitExceeded {
                current: active_count,
                max: self.max_concurrent,
            }));
        }

        let run_id = format!("run_{}", uuid::Uuid::new_v4().simple());

        // Open the existing session — its prior history is loaded from
        // its JSONL by the loop, so the resumed subagent continues with
        // full context.
        let child_base = {
            let mut manager = self.session_manager.write().await;
            manager
                .open_session(&resume_session_id)
                .await?
                .expect("metadata existed but session failed to open")
                .base()
                .clone()
        };

        Ok(ResumePreflight {
            run_id,
            session_id: resume_session_id.clone(),
            child_base,
            child_depth,
        })
    }

    /// Flag a session for engine-driven compaction at its next run
    /// (`Agent` tool `action = "compact"`). Returns immediately after
    /// setting the persisted `compact_requested` flag — no LLM call,
    /// no completion signal; the target's next resume reflects the
    /// compacted history.
    ///
    /// Guards mirror [`resume_and_execute`](Self::resume_and_execute)
    /// minus the spawn-trigger requirement (compact targets any session
    /// in the caller's tree) and plus the self/ancestor refusal
    /// (`err_compact_ancestor`) — the engine compacts the caller's own
    /// lineage automatically. This deliberately diverges from
    /// `SessionManagerRuntime::request_compaction`
    /// (`session/session_runtime_impl.rs`), which allows compacting the
    /// caller's own current session; the Agent path must not.
    pub async fn request_compaction(
        &self,
        target: &str,
        caller_session_key: &str,
    ) -> Result<crate::tools::builtin::session::CompactRequestOutcome> {
        use crate::session::ownership::{
            caller_context, err_compact_ancestor, err_compact_archived, err_out_of_tree,
            err_run_active, in_subtree,
        };

        let (caller, metas) = {
            let mut manager = self.session_manager.write().await;
            let metas = manager.list_all_sessions(false).await?;
            (caller_context(caller_session_key, &metas), metas)
        };

        // Resolve the LLM-facing slug path to a canonical session id
        // at the runtime boundary. Same rationale as
        // `resume_preflight`: bypassing `resolve_reference` and using
        // `SessionId::from`'s v5 fallback would silently misroute a
        // slug path to a deterministic UUID that doesn't match any
        // metadata (PR review finding B2).
        let caller_id = peko_session::SessionId::from(caller.current_session_id.as_str());
        let resolved = peko_session::path::resolve_reference(&metas, caller_id, target)?;
        let target = resolved.as_str();

        // Guard: target must exist. Defense-in-depth — same rationale
        // as in `resume_preflight`.
        let target_meta = metas
            .iter()
            .find(|m| m.session_id.to_string() == target)
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "session '{target}' not found — pass a slug path from the session \
                     tool's list (`path` field)"
                )
            })?;
        // Guard: the caller's own session or an ancestor compacts
        // automatically — refuse.
        if target == caller.current_session_id
            || caller.ancestors.iter().any(|a| a.as_str() == target)
        {
            return Err(err_compact_ancestor(&target));
        }
        // Guard: subtree callers stay inside their subtree (principal-
        // level callers pass automatically).
        if !caller.is_base && !caller.privileged && !in_subtree(&caller, &target, &metas) {
            return Err(err_out_of_tree(&target, &caller.current_session_id));
        }
        // Guard: archived sessions have no future run to consume the
        // request.
        if target_meta.archived {
            return Err(err_compact_archived(&target));
        }
        // Guard: refuse while a run is in flight for the target (same
        // registry source of truth as the resume path).
        {
            let registry = self.registry().read().await;
            if registry.has_active_subagent_run_for_child(&target) {
                return Err(err_run_active(&target));
            }
        }

        self.session_manager
            .read()
            .await
            .set_compact_requested(&target, true)
            .await?;
        Ok(crate::tools::builtin::session::CompactRequestOutcome {
            session_id: target.clone(),
            message: "Compaction scheduled — the engine summarizes the session at its next \
                      run. There is no completion signal; the next resume reflects the \
                      compacted history."
                .to_string(),
        })
    }

    /// Validate an explicitly-provided `parent_session_key` (spawn
    /// context seeding) against the caller's ownership tree. The
    /// caller's own session (the auto-detected default) always passes;
    /// principal-level callers pass for any session.
    ///
    /// Resolves the LLM-facing reference (absolute slug path or the
    /// caller's own id) to a canonical session id. The caller uses
    /// the resolved value downstream so the resolved id is the only
    /// thing it sees. Raw ids and caller-relative slugs are refused
    /// via `resolve_reference`.
    ///
    /// B4 cleanup: the deeper subtree check (caller.is_base /
    /// privileged / in_subtree) was unreachable in production — the
    /// producer always passes the caller's own key, so the early
    /// `caller_session_key` check above always wins. The dead branch
    /// was deleted.
    pub async fn validate_context_parent(
        &self,
        context_parent: &str,
        caller_session_key: &str,
    ) -> Result<String> {
        let mut manager = self.session_manager.write().await;
        let metas = manager.list_all_sessions(false).await?;

        // Resolve the LLM-facing slug path to a canonical session id.
        // Same rationale as `resume_preflight` / `request_compaction`:
        // `SessionId::from`'s v5 fallback would silently misroute a
        // slug path to a deterministic UUID that doesn't match any
        // metadata (PR review finding B2).
        let caller_id = peko_session::SessionId::from(caller_session_key);
        let context_parent_id =
            peko_session::path::resolve_reference(&metas, caller_id, context_parent)?;

        // The producer always passes the caller's own key, so this
        // early-return path always wins. The deeper subtree check
        // (caller.is_base / privileged / in_subtree) was unreachable
        // in production and is now deleted (B4).
        Ok(context_parent_id.to_string())
    }

    /// Register and dispatch one subagent run on the unified executor.
    ///
    /// Shared by the spawn path (fresh overlay session) and the resume
    /// path (re-attached existing session). This is the ONLY
    /// registration point for subagent runs.
    async fn register_subagent_run(&self, spec: SubagentRunSpec) -> Result<String> {
        let SubagentRunSpec {
            run_id,
            task,
            parent_session_key,
            child_session_key,
            child_session_id,
            child_base,
            child_depth,
            config,
            parent_cancel,
            stream_events,
        } = spec;

        // Build the metadata extension that carries subagent-specific data
        let metadata = TaskMetadata::Subagent(SubagentMetadata {
            child_session_key: child_session_key.clone(),
            child_session_id: Some(child_session_id.clone()),
            // B4 cleanup: the `Delete` variant of `SpawnCleanupPolicy`
            // was unreachable in production — `config.cleanup` always
            // deserializes as `Keep` (the default). The variant is
            // kept for backward-compat reads of legacy JSON config,
            // but the executor always stamps `Keep` here. The dead
            // Delete cleanup branch (lines below `if
            // cleanup_policy_clone == SpawnCleanupPolicy::Delete`)
            // was removed.
            cleanup: peko_session::types::SpawnCleanupPolicy::Keep,
            depth: child_depth,
            announce_completion: config.announce_completion,
            subagent_result: None,
        });

        // Execute using unified async executor — this is the ONLY registration point
        let async_config = AsyncToolConfig {
            delivery_mode: AsyncResultDeliveryMode::QueueWhenBusy,
            delivery_target: None,
            // `ExecutionConfig::timeout_seconds == 0` means UNLIMITED,
            // but the AsyncExecutor treats `Some(0)` as an immediate
            // timeout — map 0 to `None` so unlimited survives the hop.
            timeout_secs: (config.timeout_seconds > 0).then_some(config.timeout_seconds),
            timeout_millis: None,
            cleanup_after_delivery: false, // B4: Delete branch removed — see comment above
            label: config.label.clone(),
            wake_on_completion: true,
            principal_root_session_key: None,
        };

        // Clone values for the execution closure
        let registry_for_task = self.registry().clone();
        let registry_for_completion = self.registry().clone();
        let child_session_key_clone = child_session_key.clone();
        let child_session_id_clone = child_session_id.clone();
        let parent_session_key_clone = parent_session_key.clone();
        let task_clone = task.clone();
        let label_clone = config.label.clone();
        let run_id_clone = run_id.clone();
        let timeout = config.timeout_seconds;
        let agent_name = self.agent_name.clone();
        let provider_clone = self.provider.clone();
        let agent_config_clone = self.agent_config.clone();
        let principal_workspace_clone = self.principal_workspace.clone();
        let session_manager_clone = self.session_manager.clone();
        let principal_id_clone = self.principal_id.clone();
        let principal_capabilities_clone = self.principal_capabilities.clone();
        let active_extensions_clone = self.active_extensions.clone();
        let observability_clone = self.observability.clone();
        // F39: clone the parent's quota meter so the spawned task
        // can re-open `QuotaScope::with(...)` inside (task-locals don't
        // cross `tokio::spawn`).
        let parent_quota_meter_clone = self.quota_meter.clone();
        // Propagate the caller principal DID so the child Agent (and,
        // via its executor, deeper descendants) registers `send_peer`
        // with the principal's attribution.
        let caller_principal_did_clone = self.caller_principal_did().cloned();
        // Derive a child token inside the closure so the sub-agent
        // observes the parent's cancel via `child_cancel` without
        // extending the parent's lifetime past the closure's
        // `'static` bound. Without `child_token()` the child would
        // share a token with the parent, which is fine for cancel
        // propagation but means a child cancel would also cancel the
        // parent — wrong direction. Derivation fixes both directions.
        let child_cancel_for_closure = parent_cancel.as_ref().map(|t| t.child_token());
        // Phase 1: clone the model override so the spawned task can
        // honor the parent-driven model choice inside
        // `execute_subagent_task` (task-locals don't cross
        // `tokio::spawn`, but plain owned Strings do).
        let model_override_clone = config.model_override.clone();
        // Sprint 2 Phase 6: the streaming resume path's event sink.
        // Moved into the task closure; `None` keeps the final-only
        // execution path.
        let stream_events_for_closure = stream_events;
        // Sprint 2 Phase 7: the daemon-shared inbox registry (when
        // bound via `with_inbox_registry`) is handed to the child
        // Agent so its agentic loop drains the SAME registry the
        // principal ingress paths queue steering into (keyed by the
        // child session id). `None` keeps the per-call standalone
        // drain (tests / CLI one-shots).
        let inbox_registry_for_closure = self.inbox_registry.clone();
        // B5e (per-agent attribution): clone the executor's
        // per-agent meter into the spawned task so the run closure
        // can re-open `QuotaScope::with(agent_meter, ...)` inside
        // the spawned `tokio::task`. The AgenticLoop's
        // `StackedMeteredProvider::from_current_scope` walks the
        // full task-local stack via `QuotaScope::collect_stack()`,
        // so the inner-most call sees BOTH the principal meter
        // (outer scope) and the agent meter (this one). Both
        // meters charge on every LLM call — the principal meter
        // remains the hard cap; the agent meter accumulates audit
        // counters readable via `agent_meter_usage()`.
        let agent_meter_clone = Arc::clone(&self.agent_meter);

        self.unified_executor
            .execute_with_metadata(
                run_id.clone(),
                "Agent",
                serde_json::json!({
                    "task": task,
                    "label": &config.label,
                    "child_session_key": &child_session_key,
                    "child_session_id": &child_session_id,
                }),
                parent_session_key.clone(),
                async_config,
                metadata,
                move || async move {
                    info!(
                        "Starting subagent execution: run_id={} session={}",
                        run_id_clone, child_session_key_clone
                    );

                    // Build system prompt and task message
                    let system_prompt = build_subagent_system_prompt(
                        &parent_session_key_clone,
                        &child_session_key_clone,
                        &task_clone,
                        label_clone.as_deref(),
                        child_depth,
                        config.max_depth,
                    );

                    let task_message =
                        build_subagent_task_message(&task_clone, child_depth, config.max_depth);

                    // Execute with timeout. The cancel token is
                    // observed via two paths: (1) the child's
                    // `AgenticLoop` checks `is_cancelled()` at
                    // iteration boundaries and exits cleanly via
                    // `Lifecycle::Interrupted`; (2) the closure
                    // here checks `is_cancelled()` after the
                    // `exec_fut` resolves so the registry is
                    // updated with `AsyncTaskStatus::Cancelled`
                    // rather than `Failed` when the parent was
                    // interrupted.
                    let exec_fut = execute_subagent_task(
                        &agent_name,
                        &child_session_key_clone,
                        child_base,
                        &system_prompt,
                        &task_message,
                        model_override_clone, // Phase 1
                        provider_clone,
                        agent_config_clone,
                        session_manager_clone,
                        registry_for_task,
                        principal_id_clone,
                        principal_workspace_clone,
                        principal_capabilities_clone,
                        active_extensions_clone,
                        observability_clone,
                        child_cancel_for_closure.clone(),
                        parent_quota_meter_clone,
                        agent_meter_clone,
                        caller_principal_did_clone,
                        stream_events_for_closure,
                        inbox_registry_for_closure,
                    );
                    let result = if timeout > 0 {
                        match tokio::time::timeout(
                            tokio::time::Duration::from_secs(timeout),
                            exec_fut,
                        )
                        .await
                        {
                            Ok(r) => r,
                            Err(_) => {
                                warn!(
                                    "Subagent timed out: run_id={} timeout={}s",
                                    run_id_clone, timeout
                                );
                                Err(anyhow::anyhow!(SpawnError::Timeout { seconds: timeout }))
                            }
                        }
                    } else {
                        exec_fut.await
                    };

                    // Process result. If the parent was cancelled
                    // mid-flight, the child's loop returns
                    // `AgenticResult { interrupted: true }` —
                    // surface that as `Cancelled` instead of
                    // `Failed` so the parent's `peko async-list`
                    // shows the right state.
                    let cancelled = child_cancel_for_closure
                        .as_ref()
                        .is_some_and(tokio_util::sync::CancellationToken::is_cancelled);
                    let (status, output, error, token_usage): (
                        AsyncTaskStatus,
                        Option<String>,
                        Option<String>,
                        Option<(usize, usize, usize)>,
                    ) = if cancelled {
                        info!("Subagent cancelled by parent: run_id={}", run_id_clone);
                        (AsyncTaskStatus::Cancelled, None, None, None)
                    } else {
                        match result {
                            Ok(task_output) => {
                                info!(
                                    "Subagent completed successfully: run_id={}",
                                    run_id_clone
                                );
                                (
                                    AsyncTaskStatus::Completed {
                                        result: peko_tools_core::ToolResult::success(
                                            serde_json::json!({"output": &task_output.final_answer}),
                                        ),
                                    },
                                    Some(task_output.final_answer),
                                    None,
                                    task_output.token_usage,
                                )
                            }
                            Err(e) => {
                                error!("Subagent failed: run_id={} error={}", run_id_clone, e);
                                (
                                    AsyncTaskStatus::Failed {
                                        error: e.to_string(),
                                    },
                                    None,
                                    Some(e.to_string()),
                                    None,
                                )
                            }
                        }
                    };

                    // Update the unified registry with the subagent result.
                    // This is the ONLY state update — no dual registry sync.
                    {
                        let mut registry = registry_for_completion.write().await;
                        if let Some(entry) = registry.get_mut(&run_id_clone) {
                            // Respect cancellation — don't overwrite if already cancelled
                            if matches!(entry.status, AsyncTaskStatus::Cancelled) {
                                info!(
                                    "Subagent run {} was cancelled, skipping completion update",
                                    run_id_clone
                                );
                                return Ok(serde_json::json!({
                                    "output": null,
                                    "error": "Cancelled",
                                    "token_usage": null,
                                }));
                            }

                            // Update subagent-specific result in metadata
                            if let TaskMetadata::Subagent(ref mut meta) = entry.metadata {
                                meta.subagent_result = Some(SubagentResult {
                                    status: status.clone(),
                                    output: output.clone(),
                                    error: error.clone(),
                                    token_usage,
                                    completed_at: Utc::now(),
                                });
                            }
                        }
                        // Update status (this also sets completed_at)
                        registry.update_status(&run_id_clone, status);
                    }

                    info!(
                        "Subagent result queued for delivery to {}: run_id={}",
                        parent_session_key_clone, run_id_clone
                    );

                    // B4 cleanup: the `Delete` cleanup branch was removed.
                    // Spawn sessions are now always kept (`Keep` is the
                    // only production-reachable policy).

                    // Return async task result as opaque Value
                    Ok(serde_json::json!({
                        "output": output,
                        "error": error,
                        "token_usage": token_usage,
                    }))
                },
            )
            .await?;

        Ok(run_id)
    }

    /// Execute a subagent and wait for completion (sync mode)
    ///
    /// This is similar to `spawn_and_execute` but blocks until the subagent
    /// completes or times out. Used for sequential decomposition patterns.
    ///
    /// Returns the completed run view on success, or an error if the run fails or times out.
    ///
    /// `parent_cancel` is forwarded to `spawn_and_execute` so the
    /// sub-agent's `AgenticLoop` observes the parent's cancel token at
    /// iteration boundaries. When the parent is interrupted via
    /// `PrincipalStop`, the sub-agent exits cleanly with
    /// `interrupted: true` and the wait unblocks promptly. `None` for
    /// legacy non-cancelable call sites.
    pub async fn execute_and_wait(
        &self,
        task: &str,
        parent_session_key: &str,
        config: ExecutionConfig,
        timeout_secs: u64,
        parent_cancel: Option<tokio_util::sync::CancellationToken>,
    ) -> Result<SubagentRunView> {
        // Start the subagent (async mode initially)
        let run_id = self
            .spawn_and_execute(task, parent_session_key, config, parent_cancel)
            .await?;

        self.wait_for_run(&run_id, timeout_secs).await
    }

    /// Re-attach to an existing spawned session (Agent tool
    /// `action = "resume"`) and
    /// wait for completion. Mirror of [`Self::execute_and_wait`] over
    /// [`Self::resume_and_execute`].
    pub async fn resume_and_wait(
        &self,
        task: &str,
        resume_session_id: &str,
        parent_session_key: &str,
        config: ExecutionConfig,
        timeout_secs: u64,
        parent_cancel: Option<tokio_util::sync::CancellationToken>,
    ) -> Result<SubagentRunView> {
        let run_id = self
            .resume_and_execute(
                task,
                resume_session_id,
                parent_session_key,
                config,
                parent_cancel,
            )
            .await?;

        self.wait_for_run(&run_id, timeout_secs).await
    }

    /// Wait for a registered run to reach a terminal status and
    /// return its final view. Shared by the spawn and resume paths.
    async fn wait_for_run(&self, run_id: &str, timeout_secs: u64) -> Result<SubagentRunView> {
        let run_id = run_id.to_string();
        // Wait for completion using the unified registry.
        // IMPORTANT: Do NOT hold the read lock while sleeping, as the background
        // task needs to acquire a write lock to update status. Holding the read
        // lock continuously would starve the writer and deadlock.
        let wait_result = {
            let start = tokio::time::Instant::now();
            let timeout = Duration::from_secs(timeout_secs);

            // Register a completion waiter so we block on a notification
            // instead of busy-polling every 50ms. A buffer of 1 ensures a
            // completion that lands between registration and `recv()` is not
            // lost.
            let (tx, mut rx) = tokio::sync::mpsc::channel::<AsyncTaskStatus>(1);
            {
                let mut registry = self.registry().write().await;
                registry.register_waiter(&run_id, tx).await?;
            }

            loop {
                // Check status with a brief lock acquisition
                let status = {
                    let registry = self.registry().read().await;
                    registry.check_status(&run_id)
                };

                match status {
                    Some(s) if s.is_terminal() => {
                        let result = match s {
                            AsyncTaskStatus::Completed { result } => {
                                WaitResult::Completed { result }
                            }
                            AsyncTaskStatus::Failed { error } => WaitResult::Failed { error },
                            AsyncTaskStatus::Cancelled => WaitResult::Cancelled,
                            _ => WaitResult::Timeout,
                        };
                        break Ok(result);
                    }
                    None => {
                        break Err(anyhow::anyhow!("Run {run_id} not found in async registry"));
                    }
                    _ => {
                        // Still running — fall through and wait for a
                        // completion notification or the remaining timeout.
                    }
                }

                let remaining = timeout.saturating_sub(start.elapsed());
                if remaining.is_zero() {
                    break Ok(WaitResult::Timeout);
                }

                // Block until the task signals completion or the timeout
                // window closes. A spurious or late wakeup simply re-checks
                // status on the next iteration.
                let _ = tokio::time::timeout(remaining, rx.recv()).await;
            }
        };

        // Get the final run state
        let run = self
            .get_run(&run_id)
            .await
            .ok_or_else(|| anyhow::anyhow!("Run {run_id} not found after completion"))?;

        match wait_result {
            Ok(WaitResult::Completed { .. }) => Ok(run),
            Ok(WaitResult::Failed { error }) => Err(anyhow::anyhow!("Subagent failed: {error}")),
            Ok(WaitResult::Cancelled) => Err(anyhow::anyhow!("Subagent was cancelled")),
            Ok(WaitResult::Timeout) => {
                // Cancel the run on timeout
                self.cancel(&run_id).await.ok();
                Err(anyhow::anyhow!(
                    "Subagent execution timed out after {timeout_secs}s"
                ))
            }
            Err(e) => Err(anyhow::anyhow!("Error waiting for subagent: {e}")),
        }
    }

    /// Count total active subagent runs
    async fn count_active_runs(&self) -> usize {
        let registry = self.registry().read().await;
        registry
            .list_tasks(None)
            .into_iter()
            .filter(|e| e.tool_name == "Agent" && !e.status.is_terminal())
            .count()
    }

    /// Get a run by ID (projected view from unified registry)
    pub async fn get_run(&self, run_id: &str) -> Option<SubagentRunView> {
        let registry = self.registry().read().await;
        registry
            .get(&run_id.to_string())
            .and_then(SubagentRunView::from_entry)
    }

    /// Cancel a running subagent
    ///
    /// Single registry update — no dual sync needed.
    pub async fn cancel(&self, run_id: &str) -> Result<()> {
        self.unified_executor.cancel(&run_id.to_string()).await?;
        info!("Cancelled subagent task: run_id={}", run_id);
        Ok(())
    }
}

/// The outcome of [`execute_subagent_task`]: the child agent's final
/// answer plus the token usage accumulated over the run (projected to
/// the `(input, output, total)` tuple `SubagentResult::token_usage`
/// carries).
struct SubagentTaskOutput {
    final_answer: String,
    token_usage: Option<(usize, usize, usize)>,
}

/// Execute a subagent task
///
/// This is the core execution function that runs in a background task.
/// It:
/// 1. Loads the child session
/// 2. Creates a subagent Agent sharing the parent's session manager
/// 3. Runs the full `AgenticLoop` via `Agent::execute_with_session`
///    (final-only) or — when `stream_events` is `Some` —
///    `Agent::execute_streaming_with_session` (live `AgenticEvent`
///    stream; sprint 2 Phase 6)
/// 4. Returns the assistant's final answer + token usage
///
/// The child resolves tools from the daemon-global
/// [`crate::extensions::framework::core::global_core`]. The parent's
/// `principal_id` is propagated so the child's own `SubagentExecutor`
/// and any descendant spawns carry the same identity.
#[allow(clippy::too_many_arguments)]
async fn execute_subagent_task(
    agent_name: &str,
    session_key: &str,
    // Phase 5b: the already-resolved child session. The spawn path
    // passes the fresh overlay's base; the resume path passes the
    // opened existing session (so its prior history stays attached).
    // Resolution used to happen here by parsing the overlay key, which
    // cannot work for plain session ids (resume targets).
    child_session: Arc<RwLock<peko_session::Session>>,
    system_prompt: &str,
    task_message: &str,
    // Phase 1 of `feature/multi-model-subagents`: when the parent
    // picked a model for this spawn, `model_override` is the catalog
    // id. `None` means "inherit the parent's model verbatim"
    // (pre-Phase-1 behavior). Inside the function we clone the
    // inherited provider, stamp `default_model_id`, and pre-flight
    // `SpecGate::check` against the new spec before handing the
    // provider to `Agent::new_with_shared_executor`.
    model_override: Option<String>,
    provider: Option<Arc<peko_providers::Provider>>,
    agent_config: Option<AgentConfig>,
    session_manager: Arc<RwLock<SessionManager>>,
    async_registry: SharedAsyncTaskRegistry,
    principal_id: PrincipalId,
    principal_workspace: Option<std::path::PathBuf>,
    principal_capabilities: Option<Arc<Capabilities>>,
    active_extensions: Option<crate::extensions::framework::types::ActiveExtensionSet>,
    observability: Option<Arc<Observability>>,
    cancel: Option<tokio_util::sync::CancellationToken>,
    // F39: snapshot of the spawning principal's `QuotaMeter`. The
    // spawned `tokio::task` does NOT inherit the parent's
    // `QuotaScope::with` task-local, so we re-open the scope here
    // before calling `subagent.execute_with_session(...)` so the
    // subagent's `MeteredProvider::from_current_scope` charges the
    // parent principal. `None` falls open to
    // `QuotaMeter::unlimited()` (matches F19/F20 behavior).
    parent_quota_meter: Option<Arc<peko_quota::meter::QuotaMeter>>,
    // B5e: the executor's per-agent `QuotaMeter` (audit-only by
    // default; see `SubagentExecutor::agent_meter`). The spawned
    // `tokio::task` re-opens it as the innermost `QuotaScope::with`
    // layer so the AgenticLoop's
    // `StackedMeteredProvider::from_current_scope` walks both the
    // principal meter (outer) AND the agent meter (this one) and
    // charges each on every LLM call. The principal meter remains
    // the hard cap; the agent meter accumulates audit counters
    // readable via `SubagentExecutor::agent_meter_usage()`.
    agent_meter: Arc<peko_quota::meter::QuotaMeter>,
    // The spawning principal's DID, bound onto the child Agent so
    // `send_peer` registers down the tree with correct attribution.
    caller_principal_did: Option<String>,
    // Sprint 2 Phase 6: streaming event sink. `Some` runs the child
    // via `Agent::execute_streaming_with_session`
    // (`OrchestratorConfig::live()`) and forwards every `AgenticEvent`
    // to the sink (the IPC `principal_send` drain-loop shape); `None`
    // keeps the final-only `execute_with_session` path.
    stream_events: Option<AgenticEventSink>,
    // Sprint 2 Phase 7: the daemon-shared inbox registry, bound onto
    // the child Agent so its loop drains steering queued by the
    // principal ingress serial-queue fallback (keyed by the child
    // session id). `None` keeps the per-call standalone drain.
    inbox_registry: Option<Arc<peko_session::InboxRegistry>>,
) -> Result<SubagentTaskOutput> {
    info!(
        "Executing subagent task: agent={} session={}",
        agent_name, session_key
    );

    // If no provider, we can't do real execution
    let provider = match provider {
        Some(p) => p,
        None => {
            return Ok(SubagentTaskOutput {
                final_answer: format!(
                    "# Subagent Task\n\n**Task:** {task_message}\n\n**Status:** Completed (no provider configured)\n\nThe subagent executed without an LLM provider."
                ),
                token_usage: None,
            });
        }
    };

    // Phase 1: parent-driven model selection. When the parent
    // picked a model for this spawn, clone the inherited provider's
    // inner `Provider` (cheap — ProviderRuntimeOptions is small),
    // stamp `default_model_id`, and re-wrap in a new `Arc`. The
    // pre-existing Arc refcount is preserved on the original; the
    // new Arc is a sibling. `provider.spec()` is shared unchanged,
    // so `SpecGate::check` sees the same capability descriptor the
    // parent used.
    let provider = if let Some(ref id) = model_override {
        let inner = Arc::try_unwrap(provider).unwrap_or_else(|arc| (*arc).clone());
        let new_provider = inner.with_model_id(id.clone());
        info!(
            "Subagent '{}' dispatching with parent-picked model: {}",
            agent_name, id
        );
        Arc::new(new_provider)
    } else {
        provider
    };
    // Capture the resolved model id so `Agent::new_with_shared_executor`
    // sees a non-`None` `resolved_model_id` when a parent picked a
    // model (the inherited-provider branch currently returns `None`,
    // which would make the renderer fall back to the parent's
    // `default_model_id` instead of the chosen override).
    let resolved_model_id_override: Option<String> = model_override.clone();

    // Build agent config for the subagent
    let config = agent_config.unwrap_or_else(|| {
        let mut cfg = AgentConfig::default();
        cfg.name = agent_name.to_string();
        cfg
    });

    // Create a shared executor with the parent's registry so nested spawn depth
    // is tracked correctly across the whole tree. Propagate the principal
    // workspace and `principal_id` so grandchildren (and deeper) resolve their
    // subagents from the same workspace and inherit the same principal scope.
    let mut shared_executor_builder = SubagentExecutor::with_registry(
        async_registry,
        Arc::clone(&session_manager),
        agent_name,
        5,
        principal_id.clone(),
    )
    .with_provider(provider.clone())
    .with_agent_config(config.clone())
    .with_principal_capabilities(principal_capabilities.clone())
    .with_active_extensions(active_extensions.clone())
    .with_observability(observability.clone())
    // F39: nested sub-subagents must inherit the parent meter so
    // they too charge against the spawning principal (not
    // `unlimited()`). `parent_quota_meter` comes from the closure
    // that spawned this task — it reflects the chain back to the
    // root principal. When `None` (no quota config), subagent
    // meter attribution falls open to `QuotaMeter::unlimited()`
    // inside the nested `execute_subagent_task`, matching pre-F39
    // behavior.
    .with_quota_meter(parent_quota_meter.clone());
    if let Some(ref ws) = principal_workspace {
        shared_executor_builder = shared_executor_builder.with_principal_workspace(ws.clone());
    }
    let shared_executor = Arc::new(shared_executor_builder);

    // Create a subagent that shares the parent's session manager and executor registry.
    // Pass the parent's provider through so the child can run its own LLM calls —
    // `new_with_shared_executor` no longer re-resolves a provider (the v1
    // `[provider]` fallback was removed in PR #44) and would otherwise fail
    // `execute_with_session` with "No provider configured".
    //
    // Phase 1: when the parent picked a model for this spawn, the
    // provider above already carries the new `default_model_id` (we
    // cloned with `with_model_id` earlier). Forward
    // `model_override` as the `resolved_model_id` so the renderer
    // surfaces the parent-driven id rather than the inherited
    // default. The pre-flight SpecGate check below is what makes
    // the override authoritative — without it the loop would
    // accept requests the new model cannot serve and surface
    // confusing provider errors on the first LLM call.
    let mut subagent = crate::agents::Agent::new_with_shared_executor_with_model_override(
        config,
        session_manager,
        shared_executor,
        Some(provider.clone()),
        principal_capabilities,
        active_extensions,
        resolved_model_id_override.clone(),
    )
    .await
    .map_err(|e| anyhow::anyhow!("Failed to create subagent: {e}"))?;

    // Phase 1: pre-flight `SpecGate::check` against the resolved
    // provider's `ModelSpec`. The provider carries the override
    // id (or the inherited id when `None`), so this checks "can the
    // chosen model serve the tools the subagent will need?". The
    // request envelope here is empty — the gate looks at the
    // bound provider's `tool_support` / `thinking` / `image_input`
    // capabilities directly. A refusal here means "your chosen
    // model cannot use the tools we would hand it", which is the
    // right failure mode for a parent that picked e.g. a
    // text-only model for a tool-using subagent.
    if let Some(spec) = provider.spec() {
        if let Err(gate_err) = peko_engine::spec_gate::check(
            Some(spec),
            &provider.model_id(),
            provider.name(),
            &[],
            // Empty tool defs here — the gate only refuses when
            // the spec says `tool_support == None`, which is the
            // mismatch we want to surface. Per-call gates inside
            // the loop handle the actual tool set.
            &[],
            &peko_provider_api::ChatOptions::default(),
        ) {
            return Err(SpawnError::SpecGateFailed {
                model_id: provider.model_id().clone(),
                reason: gate_err.to_string(),
            }
            .into());
        }
    }

    // Scope the child's own `Agent` tool to the principal workspace so it can
    // resolve and delegate to nested subagents (depth 2+).
    if let Some(ws) = principal_workspace {
        subagent = subagent.with_principal_workspace(ws);
    }

    // Bind the caller principal DID so `send_peer` registers for the
    // child — and, via the executor propagation inside
    // `with_caller_principal_did`, for deeper descendants too.
    subagent = subagent.with_caller_principal_did(caller_principal_did);

    // Sprint 2 Phase 7: bind the daemon-shared inbox registry so the
    // child loop drains steering queued by ingress paths (the
    // `PrincipalManager` / IPC serial-queue fallback) into this child
    // session's inbox. `None` keeps the per-call standalone drain.
    subagent = subagent.with_inbox_registry(inbox_registry);

    // B4 cleanup: `DynamicSessionKeyProvider::set_session_key` was the
    // last live consumer of the session-key cell (write-only on the
    // production path — `get_session_key` had no readers). Nested
    // spawns now resolve the parent key through the runtime port /
    // `ToolContext::session_id` instead.

    // Combine subagent context and task into a single user message.
    // We pass history: None so that run_with_resume prepends the FULL system
    // prompt (including tool definitions from ExtensionCore). Previously we
    // passed the subagent context as a system message in history, which caused
    // run_with_resume to skip the full system prompt — leaving the subagent
    // without knowledge of available tools.
    let combined_prompt = format!("{}\n\n{}", system_prompt, task_message);

    // Execute the agentic loop with the child session
    info!(
        "Starting AgenticLoop for subagent: agent={} session={}",
        agent_name, session_key
    );

    // Clone child_session for potential recovery after execution
    let child_session_for_recovery = child_session.clone();

    // F39: subagent runs inside `QuotaScope::with(parent_quota_meter, ...)`
    // so the spawned `tokio::task`'s `MeteredProvider::from_current_scope`
    // charges against the parent principal's meter instead of falling
    // open to `unlimited()`. F19 removed this plumbing because the
    // original F19 design assumed the parent's `QuotaScope::with`
    // task-local would auto-propagate across `tokio::spawn` — it does
    // not (see `src/quota/scope.rs::scope_does_not_propagate_across_spawn`).
    //
    // Fallback to `unlimited()` when no parent meter is set — matches
    // the pre-F39 behavior for principals with no quota config.
    //
    // B5e: also wrap in a second `QuotaScope::with(agent_meter, ...)`
    // layer so the AgenticLoop's `StackedMeteredProvider::from_current_scope`
    // walks BOTH meters (principal outer, agent inner) and charges
    // each on every LLM call. Inner-most scope fires first (per the
    // `StackedMeteredProvider` ordering); trip-first wins. The agent
    // meter is unlimited by default — it accumulates audit counters
    // without tripping, while the principal meter remains the hard
    // cap. When the agent meter IS configured with a real cap
    // (via `SubagentExecutor::with_agent_meter`), both meters can
    // trip independently.
    //
    // F39: each `QuotaScope::with` layer is `Box::pin`-ed to avoid
    // compounding async stack frames — without this, the nested
    // wrap combined with `execute_with_session`'s large future stack
    // overflows the default 2MB tokio thread stack
    // (`subagent_inherits_parent_cancel` test fails with stack
    // overflow at default stack; passes with `RUST_MIN_STACK=8MB`).
    // The Box::pin is the clippy "large_futures" fix the compiler
    // suggests at `commands/agents` and elsewhere.
    let parent_quota_meter =
        parent_quota_meter.unwrap_or_else(|| Arc::new(peko_quota::meter::QuotaMeter::unlimited()));
    // Sprint 2 Phase 6: when a streaming sink is bound, run the child
    // through `execute_streaming_with_session`
    // (`OrchestratorConfig::live()`) so per-token `AssistantDelta`
    // events reach the caller; otherwise keep the final-only
    // `execute_with_session` path. Both arms produce the same
    // `AgenticResult`, boxed to a common future type.
    let run_fut: std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<peko_engine::AgenticResult>> + Send + '_>,
    > = match stream_events {
        Some(sink) => Box::pin(subagent.execute_streaming_with_session(
            &combined_prompt,
            Vec::new(), // subagents carry no recalled context
            child_session,
            None, // history: None => full system prompt (with tools) is prepended
            None, // caller_id: child turns attribute at the principal boundary
            move |event| sink(event),
            cancel,
            None, // explicit_meter override: None = use the task-local meter
        )),
        None => Box::pin(subagent.execute_with_session(
            &combined_prompt,
            Vec::new(), // subagents carry no recalled context
            child_session,
            None, // history: None => full system prompt (with tools) is prepended
            cancel,
            |_event| {
                // Non-streaming: ignore events
            },
            None, // explicit_meter override: None = use the task-local meter
        )),
    };
    let result = peko_quota::scope::QuotaScope::with(
        parent_quota_meter,
        peko_quota::scope::QuotaScope::with(agent_meter, run_fut),
    )
    .await;

    match result {
        Ok(agentic_result) => {
            let token_usage = Some((
                agentic_result.usage.input as usize,
                agentic_result.usage.output as usize,
                agentic_result.usage.total as usize,
            ));
            let mut final_answer = agentic_result.final_answer;

            // If the final answer is empty, try to recover from the session history.
            // This can happen when the LLM only makes tool calls without producing
            // text (accumulated_text is empty), or when the final assistant message
            // has empty text content.
            if final_answer.trim().is_empty() {
                if let Some(recovered) =
                    crate::agents::subagent_recovery::ResultRecovery::recover_from_session(
                        &child_session_for_recovery,
                    )
                    .await
                {
                    final_answer = recovered;
                }
            }

            info!(
                "Subagent task completed: agent={} session={} success={} iterations={} output_len={}",
                agent_name,
                session_key,
                agentic_result.success,
                agentic_result.iterations,
                final_answer.len()
            );
            Ok(SubagentTaskOutput {
                final_answer,
                token_usage,
            })
        }
        Err(e) => {
            error!(
                "Subagent task failed: agent={} session={} error={}",
                agent_name, session_key, e
            );
            Err(e)
        }
    }
}

/// Phase 3 of `feature/multi-model-subagents`: spawn-time
/// pre-flight against `cost_per_call_max`. Conservative token
/// projection (4K input + 1K output) multiplied by the chosen
/// model's `PricingHint`. Refuses the spawn before any LLM
/// traffic. Returns `Some(SpawnError::CostCeilingExceeded)` when
/// the estimate exceeds the ceiling, `None` when the spawn
/// should proceed or when no pre-flight applies (no `cost_per_call_max`
/// configured or no `PricingHint` available).
/// B8b.2: single source of truth for spawn-cost estimation.
///
/// Used by [`pre_flight_cost_ceiling`] (the production gate that
/// compares the estimate against the per-principal
/// `cost_per_call_max`) and [`SubagentExecutorRuntime::spawn_cost_estimate_usd`]
/// (the audit-side estimator that surfaces the same number on the
/// spawn row). Both surfaces previously carried their own copies of
/// the formula and the `4_000`/`1_000` constants; B8b.2 collapses
/// them here. The estimator used to acknowledge the drift risk in a
/// code comment ("Match the production pre-flight math exactly so
/// the audit row and the gate agree.") — collapsing the bodies
/// removes the drift risk by construction.
pub(crate) fn estimate_spawn_cost_usd(pricing: &peko_providers::spec::PricingHint) -> f64 {
    const EST_INPUT_TOKENS: u64 = 4_000;
    const EST_OUTPUT_TOKENS: u64 = 1_000;
    let input_cost = pricing
        .input_per_million
        .map_or(0.0, |rate| rate * EST_INPUT_TOKENS as f64 / 1_000_000.0);
    let output_cost = pricing
        .output_per_million
        .map_or(0.0, |rate| rate * EST_OUTPUT_TOKENS as f64 / 1_000_000.0);
    input_cost + output_cost
}

fn pre_flight_cost_ceiling(
    quota_meter: Option<&peko_quota::meter::QuotaMeter>,
    provider: Option<&Arc<peko_providers::Provider>>,
) -> Option<SpawnError> {
    let meter = quota_meter?;
    let ceiling = meter.config().cost_per_call_max?;
    let provider = provider?;
    let pricing = provider.spec().and_then(|s| s.pricing)?;
    let estimated = estimate_spawn_cost_usd(&pricing);
    if estimated > ceiling {
        Some(SpawnError::CostCeilingExceeded {
            estimated,
            ceiling,
            model_id: provider.model_id(),
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use peko_engine::StackedMeteredProvider;
    use peko_message::LlmMessage;
    use peko_provider_api::ChatOptions;
    use peko_providers::resolver::ResolveRequest;
    use peko_providers::MockAdapter;
    use peko_quota::scope::QuotaScope;
    use peko_quota::{QuotaConfig, QuotaCycle, QuotaMeter};
    use peko_session::manager::SessionManager;

    /// B8b.2: same shape as the production `pre_flight_cost_ceiling`
    /// but takes `&ModelSpec` directly so unit tests don't need to
    /// wire an `Arc<Provider>`. Both call
    /// [`estimate_spawn_cost_usd`] — formula is single-sourced.
    fn pre_flight_cost_ceiling_for_test(
        quota_meter: Option<&QuotaMeter>,
        provider_spec: Option<&peko_providers::spec::ModelSpec>,
        model_id: &str,
    ) -> Option<SpawnError> {
        let ceiling = quota_meter.and_then(|m| m.config().cost_per_call_max)?;
        let pricing = provider_spec.and_then(|s| s.pricing)?;
        let estimated = estimate_spawn_cost_usd(&pricing);
        if estimated > ceiling {
            Some(SpawnError::CostCeilingExceeded {
                estimated,
                ceiling,
                model_id: model_id.to_string(),
            })
        } else {
            None
        }
    }

    #[test]
    fn pre_flight_rejects_when_estimated_cost_exceeds_ceiling() {
        // Opus-priced ($15/M input, $75/M output) — 4K input +
        // 1K output = 4·15/1000 + 1·75/1000 = $0.060 + $0.075 =
        // $0.135. A ceiling of $0.10 rejects.
        let spec = peko_providers::spec::ModelSpec {
            pricing: Some(peko_providers::spec::PricingHint {
                input_per_million: Some(15.0),
                output_per_million: Some(75.0),
            }),
            ..Default::default()
        };
        let meter = QuotaMeter::new(
            QuotaConfig {
                cost_per_call_max: Some(0.10),
                ..Default::default()
            },
            None,
            Utc::now(),
        );
        let err = pre_flight_cost_ceiling_for_test(Some(&meter), Some(&spec), "claude-opus-4-8")
            .expect("Opus with $0.10 ceiling must trip");
        match err {
            SpawnError::CostCeilingExceeded {
                estimated,
                ceiling,
                model_id,
            } => {
                assert!((estimated - 0.135).abs() < 1e-9, "got {estimated}");
                assert!((ceiling - 0.10).abs() < 1e-9);
                assert_eq!(model_id, "claude-opus-4-8");
            }
            other => panic!("expected CostCeilingExceeded, got {other:?}"),
        }
    }

    #[test]
    fn pre_flight_accepts_when_estimated_cost_under_ceiling() {
        // Haiku ($1/M input, $5/M output) → 4K in + 1K out =
        // $0.004 + $0.005 = $0.009. A ceiling of $0.05 accepts.
        let spec = peko_providers::spec::ModelSpec {
            pricing: Some(peko_providers::spec::PricingHint {
                input_per_million: Some(1.0),
                output_per_million: Some(5.0),
            }),
            ..Default::default()
        };
        let meter = QuotaMeter::new(
            QuotaConfig {
                cost_per_call_max: Some(0.05),
                ..Default::default()
            },
            None,
            Utc::now(),
        );
        assert!(
            pre_flight_cost_ceiling_for_test(Some(&meter), Some(&spec), "haiku").is_none(),
            "Haiku under the $0.05 ceiling must pass"
        );
    }

    #[test]
    fn pre_flight_skipped_when_no_ceiling() {
        // `cost_per_call_max = None` ⇒ no pre-flight. Even
        // Opus-priced model is accepted.
        let spec = peko_providers::spec::ModelSpec {
            pricing: Some(peko_providers::spec::PricingHint {
                input_per_million: Some(15.0),
                output_per_million: Some(75.0),
            }),
            ..Default::default()
        };
        let meter = QuotaMeter::new(QuotaConfig::default(), None, Utc::now());
        assert!(pre_flight_cost_ceiling_for_test(Some(&meter), Some(&spec), "opus").is_none());
    }

    #[test]
    fn pre_flight_skipped_when_no_pricing_hint() {
        // Local / unpriced model — can't estimate, no pre-flight.
        let spec = peko_providers::spec::ModelSpec::default(); // pricing: None
        let meter = QuotaMeter::new(
            QuotaConfig {
                cost_per_call_max: Some(0.001), // tight cap
                ..Default::default()
            },
            None,
            Utc::now(),
        );
        assert!(pre_flight_cost_ceiling_for_test(Some(&meter), Some(&spec), "local").is_none());
    }

    #[test]
    fn pre_flight_skipped_when_no_meter() {
        // No `QuotaMeter` ⇒ no `cost_per_call_max` ⇒ no gate.
        let spec = peko_providers::spec::ModelSpec {
            pricing: Some(peko_providers::spec::PricingHint {
                input_per_million: Some(15.0),
                output_per_million: Some(75.0),
            }),
            ..Default::default()
        };
        assert!(pre_flight_cost_ceiling_for_test(None, Some(&spec), "opus").is_none());
    }

    /// F39 test fixture: build a `MockAdapter`-backed `Provider` and
    /// a single quota meter (request_count cap = 10 so a successful
    /// charge is observable without tripping).
    async fn make_provider_and_meter(
        quota_request_count: u64,
    ) -> (Arc<peko_providers::Provider>, Arc<QuotaMeter>) {
        let adapter = MockAdapter::new();
        // Two responses so the limit-trip test can run two calls.
        adapter.queue_text("first");
        adapter.queue_text("second");
        let tmp = tempfile::tempdir().unwrap();
        let catalog = tmp.path().join("models.toml");
        let (resolver, _adapter) = peko_providers::LlmResolver::mock(adapter, &catalog).await;
        let (provider, _choice) = resolver
            .build(ResolveRequest {
                override_model: Some("mock"),
                ..Default::default()
            })
            .await
            .unwrap();

        let meter = Arc::new(
            QuotaMeter::load_or_init(
                QuotaConfig {
                    request_count: Some(quota_request_count),
                    cycle: QuotaCycle::Hourly,
                    ..Default::default()
                },
                None,
                Utc::now(),
            )
            .await
            .unwrap(),
        );

        (provider, meter)
    }

    /// F39: a subagent LLM call wrapped in the F39 `QuotaScope::with`
    /// charges the spawning principal's `QuotaMeter`.
    ///
    /// Pins the F39 wiring contract: `execute_subagent_task` opens
    /// `QuotaScope::with(parent_meter, ...)` before calling
    /// `subagent.execute_with_session(...)`, and the subagent's
    /// `StackedMeteredProvider::from_current_scope` then charges
    /// that meter on every LLM call. Without the F39 wrap, the
    /// subagent's `MeteredProvider::from_current_scope` would see
    /// no active scope and fall open to `unlimited()` (F19 pre-fix
    /// behavior) — the request_count would stay at 0.
    #[tokio::test]
    async fn subagent_quota_charges_parent_meter() {
        let (provider, meter) = make_provider_and_meter(10).await;
        let before = meter.snapshot();
        assert_eq!(before.request_count, 0);

        // Mirror the F39 wrap: open `QuotaScope::with(parent_meter, ...)`
        // then construct a `StackedMeteredProvider::from_current_scope` —
        // same shape as `engine/agentic_loop.rs:753-755`.
        QuotaScope::with(meter.clone(), async {
            let stacked = StackedMeteredProvider::from_current_scope(provider);
            let _ = stacked
                .chat_with_tools(
                    "default",
                    &[LlmMessage::user("hi")],
                    &[],
                    &ChatOptions::default(),
                )
                .await
                .unwrap();
        })
        .await;

        let after = meter.snapshot();
        assert_eq!(
            after.request_count,
            before.request_count + 1,
            "subagent LLM call should charge the parent meter exactly once (F39 wiring)"
        );
    }

    /// F39: the F39 wrap observes the principal's `request_count`
    /// ceiling — a second subagent LLM call beyond the cap fails
    /// with a quota error and the request counter stays above the
    /// ceiling.
    ///
    /// The `QuotaMeter::charge` does NOT roll back the state when
    /// the limit trips (it returns `Err(QuotaError)` with the
    /// mutated state — see `quota/meter.rs:204-222`), so after the
    /// second call `request_count` is 2 and `check()` returns
    /// `Some(RequestCountExceeded)`.
    #[tokio::test]
    async fn subagent_quota_limit_trips_on_second_call() {
        let (provider, meter) = make_provider_and_meter(1).await;

        QuotaScope::with(meter.clone(), async {
            let stacked = StackedMeteredProvider::from_current_scope(provider);
            // First call: meter goes 0 → 1, exactly at the ceiling, OK.
            let first = stacked
                .chat_with_tools(
                    "default",
                    &[LlmMessage::user("hi")],
                    &[],
                    &ChatOptions::default(),
                )
                .await;
            assert!(
                first.is_ok(),
                "first call should succeed when meter is at the ceiling: {:?}",
                first.err()
            );
            assert_eq!(meter.snapshot().request_count, 1);
            assert!(
                meter.check().is_none(),
                "request_count=1 == limit=1 is still within the ceiling"
            );

            // Second call: meter would go 1 → 2, exceeds the limit,
            // `charge` returns `Err(RequestCountExceeded)` and
            // surfaces as an `anyhow::Error` from `chat_with_tools`.
            let second = stacked
                .chat_with_tools(
                    "default",
                    &[LlmMessage::user("hi again")],
                    &[],
                    &ChatOptions::default(),
                )
                .await;
            assert!(
                second.is_err(),
                "second call should fail with quota error: {:?}",
                second.as_ref().map(|c| &c.usage)
            );
            let err = second.err().unwrap();
            let msg = format!("{err:#}");
            assert!(
                msg.contains("request count quota exceeded"),
                "error should be a quota exceeded error, got: {msg}"
            );
        })
        .await;

        // Outside the scope: state still reflects the trip.
        let final_snapshot = meter.snapshot();
        assert_eq!(final_snapshot.request_count, 2);
        assert!(
            matches!(
                meter.check(),
                Some(peko_quota::error::QuotaError::RequestCountExceeded { .. })
            ),
            "meter should report the request_count limit tripped"
        );
    }

    /// B5e stacking: when both a principal meter and a per-agent meter
    /// are open (the production wrap in `execute_subagent_task`),
    /// the subagent's `StackedMeteredProvider` charges BOTH on
    /// every LLM call. The principal meter remains the hard cap;
    /// the agent meter accumulates audit counters readable via
    /// `SubagentExecutor::agent_meter_usage()`.
    ///
    /// Mirrors the production wrap at
    /// `subagent_executor.rs:2162-2174`: outer
    /// `QuotaScope::with(parent_meter, ...)` + inner
    /// `QuotaScope::with(agent_meter, ...)`. The
    /// `StackedMeteredProvider::from_current_scope` call walks
    /// the full task-local stack via `QuotaScope::collect_stack()`
    /// and charges every meter. Inner-most fires first (trip-first
    /// wins).
    #[tokio::test]
    async fn subagent_quota_stacks_principal_and_agent_meter() {
        let adapter = MockAdapter::new();
        adapter.queue_text("hi");
        let tmp = tempfile::tempdir().unwrap();
        let catalog = tmp.path().join("models.toml");
        let (resolver, _adapter) = peko_providers::LlmResolver::mock(adapter, &catalog).await;
        let (provider, _choice) = resolver
            .build(ResolveRequest {
                override_model: Some("mock"),
                ..Default::default()
            })
            .await
            .unwrap();

        let principal = Arc::new(
            QuotaMeter::load_or_init(
                QuotaConfig {
                    request_count: Some(10),
                    cycle: QuotaCycle::Hourly,
                    ..Default::default()
                },
                None,
                Utc::now(),
            )
            .await
            .unwrap(),
        );
        let agent_meter = Arc::new(
            QuotaMeter::load_or_init(
                QuotaConfig {
                    request_count: Some(10),
                    cycle: QuotaCycle::Hourly,
                    ..Default::default()
                },
                None,
                Utc::now(),
            )
            .await
            .unwrap(),
        );

        // Outer principal scope, inner agent scope — same nesting as
        // the B5e production wrap at
        // `subagent_executor.rs:2162-2174`. The inner scope is
        // `Box::pin`-ed in production but for this focused test
        // there is no large future underneath, so no pin needed.
        QuotaScope::with(principal.clone(), async {
            QuotaScope::with(agent_meter.clone(), async {
                let stacked = StackedMeteredProvider::from_current_scope(provider);
                let _ = stacked
                    .chat_with_tools(
                        "default",
                        &[LlmMessage::user("hi")],
                        &[],
                        &ChatOptions::default(),
                    )
                    .await
                    .unwrap();
            })
            .await;
        })
        .await;

        // Both meters should see exactly one charge.
        assert_eq!(
            principal.snapshot().request_count,
            1,
            "principal meter should be charged through the B5e wrap (outer scope)"
        );
        assert_eq!(
            agent_meter.snapshot().request_count,
            1,
            "agent meter should be charged through the B5e wrap (inner scope, B5e stacking)"
        );
    }

    #[tokio::test]
    async fn test_executor_creation() {
        let manager = Arc::new(RwLock::new(SessionManager::new()));
        let executor = SubagentExecutor::new(
            manager,
            "test_agent",
            5,
            peko_subject::PrincipalId::generate(),
        );

        assert_eq!(executor.agent_name, "test_agent");
    }

    /// B5d (per-agent attribution): the executor's agent_meter is
    /// always populated (defaults to `QuotaMeter::unlimited()`); the
    /// snapshot API starts at zero counters; and `with_agent_meter`
    /// replaces the default. Audit-only behavior — no cap trips
    /// because the default has no limits.
    #[tokio::test]
    async fn test_agent_meter_defaults_to_unlimited_and_charges_through_snapshot() {
        let manager = Arc::new(RwLock::new(SessionManager::new()));
        let executor = SubagentExecutor::new(
            manager,
            "test_agent",
            5,
            peko_subject::PrincipalId::generate(),
        );

        // Default-constructed agent_meter is an unlimited fallback.
        let initial = executor.agent_meter_usage();
        assert_eq!(initial.input_tokens, 0);
        assert_eq!(initial.output_tokens, 0);
        assert_eq!(initial.request_count, 0);
        assert!(
            executor.agent_meter().check().is_none(),
            "unlimited default must not trip on a fresh state"
        );

        // `with_agent_meter` replaces the default — useful when a
        // caller wants the agent meter to enforce a real cap.
        let capped = Arc::new(peko_quota::meter::QuotaMeter::new(
            peko_quota::config::QuotaConfig {
                request_count: Some(2),
                cycle: peko_quota::config::QuotaCycle::Hourly,
                ..Default::default()
            },
            None,
            chrono::Utc::now(),
        ));
        let executor = executor.with_agent_meter(capped.clone());
        assert!(
            Arc::ptr_eq(executor.agent_meter(), &capped),
            "with_agent_meter must replace the unlimited default"
        );
    }

    #[tokio::test]
    async fn test_execution_config_defaults() {
        let config = ExecutionConfig::default();
        assert_eq!(config.timeout_seconds, 300);
        assert!(matches!(config.cleanup, SpawnCleanupPolicy::Keep));
        assert!(config.label.is_none());
        assert!(config.announce_completion);
        assert_eq!(config.max_depth, 1);
    }

    #[tokio::test]
    async fn test_registry_operations() {
        let manager = Arc::new(RwLock::new(SessionManager::new()));
        let executor = SubagentExecutor::new(
            manager,
            "test_agent",
            5,
            peko_subject::PrincipalId::generate(),
        );

        // Initially empty
        assert_eq!(executor.count_active_runs().await, 0);
    }

    #[tokio::test]
    async fn test_session_cleanup_delete_policy() {
        use peko_auth::Subject;

        // Create a session manager with path resolver
        let path_resolver: Arc<dyn peko_subject::PathResolverLike> =
            Arc::new(peko_session::DefaultPathResolver::new());
        let manager = SessionManager::new()
            .with_path_resolver(path_resolver, "test_agent")
            .await
            .unwrap();
        let manager = Arc::new(RwLock::new(manager));

        // Create a parent session
        let parent_peer = Subject::User("parent".to_string());
        {
            let mut mgr = manager.write().await;
            let parent_handle = mgr
                .get_or_create_base("test_agent", &parent_peer)
                .await
                .unwrap();
            let parent_key = {
                let base = parent_handle.read().await;
                base.session_key.clone()
            };
            assert!(parent_key.contains("peer:user:parent"));
        }

        // Create a spawn overlay (simulating what spawn_and_execute does)
        let child_session_key = {
            let mut mgr = manager.write().await;
            let handle = mgr
                .create_spawn_overlay(
                    "test_agent",
                    &Subject::Principal("child".into()),
                    "test task",
                    false,
                    "agent:test_agent:peer:user:parent",
                )
                .await
                .unwrap();
            handle.full_session_key().await
        };
        assert!(child_session_key.contains("overlay:spawn:"));

        // Verify overlay exists
        {
            let mgr = manager.read().await;
            assert!(mgr.get_spawn_overlay(&child_session_key).is_some());
            assert_eq!(mgr.spawn_overlay_count(), 1);
        }

        // Simulate cleanup using cleanup_spawn
        {
            let mut mgr = manager.write().await;
            let cleaned = mgr.cleanup_spawn(&child_session_key).await;
            assert!(cleaned.is_ok(), "cleanup_spawn should succeed");
            assert!(cleaned.unwrap(), "cleanup_spawn should return true");
        }

        // Verify cleanup
        {
            let mgr = manager.read().await;
            assert_eq!(mgr.spawn_overlay_count(), 0);
        }
    }

    #[tokio::test]
    async fn test_principal_capabilities_propagation() {
        let manager = Arc::new(RwLock::new(SessionManager::new()));
        let allowed = Arc::new(Capabilities::with_grants(["tool:Read", "tool:Write"]));

        let executor = SubagentExecutor::new(
            manager.clone(),
            "test_agent",
            5,
            peko_subject::PrincipalId::generate(),
        )
        .with_principal_capabilities(Some(Arc::clone(&allowed)));

        assert_eq!(
            executor.principal_capabilities(),
            Some(&allowed),
            "builder should store the capability snapshot"
        );

        let cloned = executor.clone();
        assert_eq!(
            cloned.principal_capabilities(),
            Some(&allowed),
            "clone should preserve the capability snapshot"
        );
    }
}
