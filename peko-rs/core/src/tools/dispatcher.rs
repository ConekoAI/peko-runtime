//! `ToolDispatcher` — the single tool-execution point (ADR-066 D2).
//!
//! One function executes a tool call: builds the `ToolContext`
//! (principal/session/agent identity, abort receiver), applies the
//! execution timeout and panic isolation + background detach (the
//! behavior kept from `transport::async_router`), bridges abort
//! signals, fires the observe-only `PreToolUse` / `PostToolUse` hooks
//! through the workspace dispatcher, writes the call to its principal's
//! tool ledger (ADR-079), and emits the `tool.call` audit event.
//!
//! Replaces the P2 stack (`BuiltinExecuteHandler` + `WorkspaceHookDispatcher`
//! priority dispatch + companion-hook codegen) for the execution path:
//! tools dispatch straight from the [`ToolCatalog`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use futures::FutureExt;
use serde_json::Value;
use tracing::{debug, warn};

use peko_engine::ToolCallSpec;
use peko_tools_core::{ActingFor, ToolInterruptNotice};

use crate::extensions::framework::transport::async_router::{
    AsyncExecutionRouter, ToolExecutionContext,
};
use crate::extensions::workspace_dispatcher::WorkspaceHookDispatcher;
use crate::extensions::workspace_dispatcher::WorkspaceHookPoint;
use crate::extensions::workspace_io::HookInput;
use crate::security::ledger::{self, redact::Redactor, Ledger, Outcome};
use crate::tools::catalog::ToolCatalog;

/// What [`ToolDispatcher::screen_hot_memory`] decided.
enum MemoryScreen {
    /// Nothing held back.
    Pass,
    /// Some lines held back; the rest is written, and the note is added
    /// to the result.
    Partly(String),
    /// Nothing left to write, or filing failed: the call's result.
    Done(Dispatched),
}

/// The single tool-execution point. Cheap to clone (every field is an
/// `Arc`).
#[derive(Clone)]
pub struct ToolDispatcher {
    run_binding: std::sync::Weak<crate::tools::runtime::ToolingRuntime>,
    catalog: Arc<ToolCatalog>,
    hooks: Arc<WorkspaceHookDispatcher>,
    router: Arc<AsyncExecutionRouter>,
    audit: Option<Arc<peko_observability::Observability>>,
    /// Each principal's Async runtime and executor, bound at installation.
    /// Calls from a bound principal get a background spawner on their
    /// context and route through that principal's executor, so all of the
    /// principal's background work — Async spawn, a tool starting itself,
    /// a call detached on timeout — registers in its one task registry.
    /// Shared with run overlays.
    principal_async: Arc<
        std::sync::RwLock<std::collections::HashMap<peko_subject::PrincipalId, PrincipalAsync>>,
    >,
    /// Each principal's workspace root, bound when its tool bag is
    /// installed. The ADR-078 path floor confines and protects against
    /// it. Shared with run overlays.
    principal_workspaces: Arc<
        std::sync::RwLock<std::collections::HashMap<peko_subject::PrincipalId, std::path::PathBuf>>,
    >,
    /// The ADR-078 D7 policy gate, bound by the daemon. Unbound, a call
    /// that needs review is refused. Shared with run overlays.
    policy_gate: Arc<std::sync::RwLock<Option<Arc<dyn crate::security::judge::PolicyGate>>>>,
    /// Recent review verdicts per `(actor, call digest)`, so a run
    /// repeating a call does not pay for a second review.
    review_cache: Arc<
        std::sync::Mutex<
            std::collections::HashMap<
                String,
                (std::time::Instant, crate::security::judge::Verdict),
            >,
        >,
    >,
    /// Each principal's tool ledger (ADR-079), bound with its services.
    /// A principal without one (tests, the system principal) is audited
    /// only globally. Shared with run overlays.
    ledgers:
        Arc<std::sync::RwLock<std::collections::HashMap<peko_subject::PrincipalId, Arc<Ledger>>>>,
    /// Principals whose ledger could not be opened: their calls are
    /// treated as failed ledger writes (ADR-079 D5), never as unledgered.
    unledgered: Arc<std::sync::RwLock<std::collections::HashSet<peko_subject::PrincipalId>>>,
    /// Redacts ledger params and excerpts; the daemon binds the runtime's
    /// known secrets. Shared with run overlays.
    redactor: Arc<Redactor>,
}

/// How long a review verdict is reused for the same actor and call.
const REVIEW_CACHE_TTL: std::time::Duration = std::time::Duration::from_mins(10);

/// A principal's Async runtime and the executor behind it.
#[derive(Clone)]
struct PrincipalAsync {
    runtime: crate::tools::builtin::SharedAsyncRuntime,
    executor: Arc<crate::async_exec::executor::AsyncExecutor>,
}

impl std::fmt::Debug for ToolDispatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolDispatcher")
            .field("has_audit_sink", &self.audit.is_some())
            .finish()
    }
}

impl ToolDispatcher {
    /// Rebind lookup while retaining the same hooks, router, and audit sink.
    pub(crate) fn with_catalog(
        &self,
        catalog: Arc<ToolCatalog>,
        run_binding: std::sync::Weak<crate::tools::runtime::ToolingRuntime>,
    ) -> Self {
        Self {
            run_binding,
            catalog,
            ..self.clone()
        }
    }
    /// Compose the dispatcher over the catalog + hook registry + the
    /// timeout/detach router. `audit` is the observability hub the
    /// `tool.call` event lands on (`None` for tests / standalone
    /// contexts — execution is unaffected).
    #[must_use]
    pub fn new(
        catalog: Arc<ToolCatalog>,
        hooks: Arc<WorkspaceHookDispatcher>,
        router: Arc<AsyncExecutionRouter>,
        audit: Option<Arc<peko_observability::Observability>>,
    ) -> Self {
        Self {
            run_binding: std::sync::Weak::new(),
            catalog,
            hooks,
            router,
            audit,
            principal_async: Arc::default(),
            principal_workspaces: Arc::default(),
            policy_gate: Arc::default(),
            review_cache: Arc::default(),
            ledgers: Arc::default(),
            unledgered: Arc::default(),
            redactor: Arc::default(),
        }
    }

    /// Bind the ADR-078 D7 policy gate.
    pub fn set_policy_gate(&self, gate: Arc<dyn crate::security::judge::PolicyGate>) {
        *self.policy_gate.write().expect("policy gate lock poisoned") = Some(gate);
    }

    /// The bound policy gate, if any.
    pub fn policy_gate(&self) -> Option<Arc<dyn crate::security::judge::PolicyGate>> {
        self.policy_gate
            .read()
            .expect("policy gate lock poisoned")
            .clone()
    }

    /// The gate's verdict on `call`: approved replays pass, recent
    /// verdicts are reused, and an unbound gate refuses.
    async fn review(
        &self,
        call: &crate::security::judge::GatedCall,
    ) -> crate::security::judge::Verdict {
        use crate::security::judge::Verdict;
        let Some(gate) = self.policy_gate() else {
            return Verdict::Deny("no policy reviewer is configured on this runtime".into());
        };
        if gate.is_approved_replay(call).await {
            return Verdict::Allow;
        }
        let key = format!("{}|{}", call.stamp.actor, call.digest());
        {
            let mut cache = self.review_cache.lock().expect("review cache poisoned");
            cache.retain(|_, (at, _)| at.elapsed() < REVIEW_CACHE_TTL);
            if let Some((_, verdict)) = cache.get(&key) {
                return verdict.clone();
            }
        }
        let verdict = gate.review(call).await;
        if !matches!(verdict, Verdict::AskOwner(_)) {
            self.review_cache
                .lock()
                .expect("review cache poisoned")
                .insert(key, (std::time::Instant::now(), verdict.clone()));
        }
        verdict
    }

    /// Bind `principal`'s workspace root for the ADR-078 path floor.
    pub(crate) fn bind_workspace(
        &self,
        principal: peko_subject::PrincipalId,
        workspace: std::path::PathBuf,
    ) {
        self.principal_workspaces
            .write()
            .expect("principal workspaces lock poisoned")
            .insert(principal, workspace);
    }

    fn workspace_for(&self, principal: &peko_subject::PrincipalId) -> Option<std::path::PathBuf> {
        self.principal_workspaces
            .read()
            .expect("principal workspaces lock poisoned")
            .get(principal)
            .cloned()
    }

    /// Bind `principal`'s Async runtime (its background spawner) and the
    /// executor behind it (where its calls route).
    pub(crate) fn bind_async_runtime(
        &self,
        principal: peko_subject::PrincipalId,
        runtime: crate::tools::builtin::SharedAsyncRuntime,
        executor: Arc<crate::async_exec::executor::AsyncExecutor>,
    ) {
        self.principal_async
            .write()
            .expect("principal async lock poisoned")
            .insert(principal, PrincipalAsync { runtime, executor });
    }

    /// Bind `principal`'s tool ledger in `dir` (ADR-079). Rebinding to
    /// the same directory is a no-op.
    pub(crate) fn bind_ledger(
        &self,
        principal: &peko_subject::PrincipalId,
        dir: &std::path::Path,
    ) -> Result<()> {
        if self
            .ledger(principal)
            .is_some_and(|bound| bound.dir() == dir)
        {
            return Ok(());
        }
        let opened = match Ledger::open(dir, &principal.0) {
            Ok(opened) => Arc::new(opened),
            Err(e) => {
                self.unledgered
                    .write()
                    .expect("unledgered lock poisoned")
                    .insert(principal.clone());
                return Err(e);
            }
        };
        self.unledgered
            .write()
            .expect("unledgered lock poisoned")
            .remove(principal);
        self.ledgers
            .write()
            .expect("ledgers lock poisoned")
            .insert(principal.clone(), opened);
        Ok(())
    }

    /// `principal`'s bound ledger.
    pub(crate) fn ledger(&self, principal: &peko_subject::PrincipalId) -> Option<Arc<Ledger>> {
        self.ledgers
            .read()
            .expect("ledgers lock poisoned")
            .get(principal)
            .cloned()
    }

    /// Bind where the ledger redactor learns the runtime's own secrets.
    pub fn set_secret_source(&self, source: Arc<dyn ledger::redact::SecretSource>) {
        self.redactor.set_source(source);
    }

    /// Checkpoint every bound ledger's head into the global audit log
    /// (ADR-079 D5). `force` ignores the hourly cadence (shutdown).
    pub async fn checkpoint_ledgers(&self, force: bool) {
        let ledgers: Vec<Arc<Ledger>> = self
            .ledgers
            .read()
            .expect("ledgers lock poisoned")
            .values()
            .cloned()
            .collect();
        for ledger in ledgers {
            self.checkpoint(&ledger, force).await;
        }
    }

    async fn checkpoint(&self, ledger: &Ledger, force: bool) {
        let (Some(audit), Some((seq, hash))) = (self.audit.as_ref(), ledger.take_checkpoint(force))
        else {
            return;
        };
        let details = serde_json::json!({
            "principal_id": ledger.principal_id(),
            "seq": seq,
            "hash": hash,
        });
        if let Err(e) = audit.audit(ledger::EVENT_CHECKPOINT, None, details).await {
            warn!("failed to checkpoint the tool ledger: {e:#}");
        }
    }

    /// Delete ledger days older than each principal's retention (ADR-079
    /// D7), recording where each pruned chain now starts.
    pub async fn prune_ledgers(&self, retention_days: impl Fn(&peko_subject::PrincipalId) -> u32) {
        let ledgers: Vec<(peko_subject::PrincipalId, Arc<Ledger>)> = self
            .ledgers
            .read()
            .expect("ledgers lock poisoned")
            .iter()
            .map(|(id, ledger)| (id.clone(), Arc::clone(ledger)))
            .collect();
        for (principal, ledger) in ledgers {
            let days = retention_days(&principal).max(1);
            let cutoff = chrono::Utc::now().date_naive() - chrono::Days::new(u64::from(days));
            let dir = ledger.dir().to_path_buf();
            let pruned = tokio::task::spawn_blocking(move || {
                peko_observability::chain::prune_before(&dir, ledger::PREFIX, cutoff)
            })
            .await;
            match pruned {
                Ok(Ok(Some((seq, hash)))) => {
                    if let Some(audit) = self.audit.as_ref() {
                        let details = serde_json::json!({
                            "principal_id": principal.0,
                            "seq": seq,
                            "hash": hash,
                            "before": cutoff.to_string(),
                        });
                        if let Err(e) = audit.audit(ledger::EVENT_PRUNED, None, details).await {
                            warn!("failed to record a ledger prune: {e:#}");
                        }
                    }
                }
                Ok(Ok(None)) => {}
                Ok(Err(e)) => warn!(principal = %principal.0, "tool ledger prune failed: {e:#}"),
                Err(e) => warn!(principal = %principal.0, "tool ledger prune task failed: {e}"),
            }
        }
    }

    fn principal_async(&self, principal: &peko_subject::PrincipalId) -> Option<PrincipalAsync> {
        self.principal_async
            .read()
            .expect("principal async lock poisoned")
            .get(principal)
            .cloned()
    }

    /// The catalog this dispatcher executes against.
    #[must_use]
    pub fn catalog(&self) -> &Arc<ToolCatalog> {
        &self.catalog
    }

    /// The timeout/detach router (for `wait_for_async_tasks`-style
    /// drains at shutdown).
    #[must_use]
    pub fn router(&self) -> &Arc<AsyncExecutionRouter> {
        &self.router
    }

    /// Execute one tool call. Returns the `(display, json, success)`
    /// triplet: an unknown tool, a schema violation, and a tool error
    /// all arrive as `success: false` data, never a transport error;
    /// only dispatcher-internal failures produce `Err`.
    pub async fn execute(&self, call: ToolCallSpec) -> Result<(String, Value, bool)> {
        let start = Instant::now();
        let call_id = format!("call_{}", uuid::Uuid::now_v7().simple());
        let described = self.describe(&call).await;

        // ADR-079 D2: the start record is durable before anything runs,
        // so a call that hangs, crashes the daemon, or is refused still
        // leaves a trace. D5: when it cannot be written, only owner runs
        // proceed.
        let principal = call
            .principal_id
            .as_deref()
            .filter(|id| !id.is_empty())
            .map(|id| peko_subject::PrincipalId(id.to_owned()));
        let mut ledger = principal.as_ref().and_then(|id| self.ledger(id));
        let unledgered = principal.as_ref().is_some_and(|id| {
            self.unledgered
                .read()
                .expect("unledgered lock poisoned")
                .contains(id)
        });
        if unledgered && ledger.is_none() {
            let e = anyhow::anyhow!("the tool ledger could not be opened");
            self.emit_ledger_failure(&call, &call_id, &e).await;
            if !call.acting_for.as_ref().is_some_and(ActingFor::is_owner) {
                let text =
                    "Error: the tool ledger is unavailable, so this call was not run".to_string();
                return Ok((text.clone(), Value::String(text), false));
            }
        }
        if let Some(bound) = &ledger {
            let record = ledger::start_record(
                &ledger::Start {
                    call_id: &call_id,
                    spec: &call,
                    action: described.action.as_deref(),
                    source: Some(&described.source),
                    params_digest: &described.params_digest,
                },
                &self.redactor,
            );
            if let Err(e) = bound.append(record).await {
                self.emit_ledger_failure(&call, &call_id, &e).await;
                if !call.acting_for.as_ref().is_some_and(ActingFor::is_owner) {
                    let text = "Error: the tool ledger is unavailable, so this call was not run"
                        .to_string();
                    return Ok((text.clone(), Value::String(text), false));
                }
                ledger = None;
            }
        }
        let late = ledger.as_ref().map(|bound| {
            Arc::new(LateEnd::new(
                Arc::clone(bound),
                Arc::clone(&self.redactor),
                call_id.clone(),
            ))
        });

        self.fire_observe(
            WorkspaceHookPoint::PreToolUse {
                tool_name: Some(call.tool_name.clone()),
            },
            &call,
        )
        .await;
        let dispatched = self.execute_inner(&call, &call_id, late.clone()).await;
        self.fire_observe(
            WorkspaceHookPoint::PostToolUse {
                tool_name: Some(call.tool_name.clone()),
            },
            &call,
        )
        .await;
        let elapsed = start.elapsed();

        let (outcome, success) = match &dispatched {
            Ok(d) => (d.outcome, d.success),
            Err(_) => (Outcome::Error, false),
        };
        if let Some(bound) = &ledger {
            let end = match &dispatched {
                Ok(d) => ledger::End {
                    outcome: d.outcome,
                    duration_ms: Some(elapsed.as_millis() as u64),
                    exit_code: ledger::exit_code(&d.json),
                    task_id: (d.outcome == Outcome::Detached)
                        .then(|| ledger::detached_task_id(&d.json))
                        .flatten(),
                    result: Some(ledger::excerpt(&d.text, &d.json, &self.redactor)),
                    late: false,
                },
                Err(e) => ledger::End {
                    outcome: Outcome::Error,
                    duration_ms: Some(elapsed.as_millis() as u64),
                    exit_code: None,
                    task_id: None,
                    result: Some(ledger::excerpt(
                        &format!("Error: {e:#}"),
                        &Value::Null,
                        &self.redactor,
                    )),
                    late: false,
                },
            };
            if let Err(e) = bound.append(ledger::end_record(&call_id, &end)).await {
                self.emit_ledger_failure(&call, &call_id, &e).await;
            }
            // The detached end is down: the body's own end may follow.
            if let (Some(late), Some(task_id)) = (&late, end.task_id) {
                late.detached(task_id).await;
            }
            self.checkpoint(bound, false).await;
        }
        self.emit_audit(&call, &call_id, &described, outcome, success, elapsed)
            .await;
        dispatched.map(|d| (d.text, d.json, d.success))
    }

    /// The parts of a call's description both logs share: its declared
    /// action, its source, and the digest of its params.
    async fn describe(&self, call: &ToolCallSpec) -> Described {
        let principal = call
            .principal_id
            .as_deref()
            .map(|id| peko_subject::PrincipalId(id.to_owned()))
            .unwrap_or_else(|| peko_subject::PrincipalId::system().clone());
        let metadata = match call.catalog_binding.as_ref().and_then(|binding| {
            binding
                .as_any()
                .downcast_ref::<crate::tools::catalog::CatalogBinding>()
        }) {
            Some(binding) => binding.get(&call.tool_name, &principal).map(|(_, m)| m),
            None => self
                .catalog
                .get(&call.tool_name, &principal)
                .await
                .map(|(_, m)| m),
        };
        // Only disclose a declared discriminator value. An unknown action can
        // contain arbitrary argument text and must stay inside the digest.
        let action = metadata.as_ref().and_then(|metadata| {
            let discriminator = &metadata.parameters["properties"]["action"];
            let action = call
                .params
                .get("action")
                .unwrap_or(&discriminator["default"])
                .as_str()?;
            discriminator["enum"]
                .as_array()?
                .iter()
                .any(|value| value.as_str() == Some(action))
                .then(|| action.to_owned())
        });
        let source = match metadata.as_ref().map(|metadata| &metadata.source) {
            Some(crate::tools::metadata::ToolSource::Mcp { server, .. }) => format!("mcp:{server}"),
            Some(crate::tools::metadata::ToolSource::BuiltIn) => "builtin".to_string(),
            None => "unavailable".to_string(),
        };
        let params_digest = {
            use sha2::{Digest, Sha256};
            let canonical = call.params.to_string();
            let digest = Sha256::digest(canonical.as_bytes());
            format!("sha256:{}", hex_encode(&digest))
        };
        Described {
            action,
            source,
            params_digest,
        }
    }

    async fn execute_inner(
        &self,
        call: &ToolCallSpec,
        call_id: &str,
        late: Option<Arc<LateEnd>>,
    ) -> Result<Dispatched> {
        let tool_name = call.tool_name.clone();
        // An empty id is no principal: tools must see `None` (and fail
        // closed), and background work it spawns must be system-owned —
        // never owned by a principal named "".
        let caller_principal = call.principal_id.as_deref().filter(|id| !id.is_empty());
        let principal_id = caller_principal
            .map(|id| peko_subject::PrincipalId(id.to_string()))
            .unwrap_or_else(|| peko_subject::PrincipalId::system().clone());

        // 1. Resolve the tool from the catalog (presence =
        //    executability — ADR-066 D1).
        let binding = call.catalog_binding.as_ref().and_then(|binding| {
            binding
                .as_any()
                .downcast_ref::<crate::tools::catalog::CatalogBinding>()
        });
        let resolved = if let Some(binding) = binding {
            binding.get(&tool_name, &principal_id)
        } else if call.catalog_binding.is_some() {
            None
        } else {
            self.catalog.get(&tool_name, &principal_id).await
        };
        let Some((tool, metadata)) = resolved else {
            let text = format!("Tool '{tool_name}' not available");
            return Ok(Dispatched::refused(text, Outcome::Error));
        };

        // 2. F32b — validate LLM-emitted args against the tool's
        //    declared JSON Schema before any preprocessing.
        // Workflow depth is injected by the trusted IPC run-token path,
        // not part of the public schema. Validate only its public args;
        // retain the injected value for the runner's recursion guard.
        let builtin = metadata.source == crate::tools::metadata::ToolSource::BuiltIn;
        let public_params = if builtin
            && tool_name == crate::tools::builtin::WORKFLOW_TOOL_NAME
            && call.params.get("_workflow_depth").is_some()
        {
            let mut params = call.params.clone();
            if let Some(object) = params.as_object_mut() {
                object.remove("_workflow_depth");
            }
            Some(params)
        } else {
            None
        };
        if let Err(msg) = AsyncExecutionRouter::validate_tool_args(
            &metadata.parameters,
            public_params.as_ref().unwrap_or(&call.params),
            &tool_name,
        ) {
            warn!(tool = %tool_name, "Tool arg validation failed; returning as tool failure");
            let text = format!("Error: {msg}");
            return Ok(Dispatched::refused(text, Outcome::Error));
        }

        // 3. ADR-078 floor: a call without a stamp is unattributed and
        //    reaches nothing; an untrusted run never gets the shell or an
        //    MCP server its owner did not mark safe.
        let stamp = call
            .acting_for
            .clone()
            .unwrap_or_else(peko_tools_core::ActingFor::unattributed);
        let mcp_untrusted_ok = match &metadata.source {
            crate::tools::metadata::ToolSource::Mcp { untrusted_ok, .. } => Some(*untrusted_ok),
            crate::tools::metadata::ToolSource::BuiltIn => None,
        };
        if let Some(reason) =
            crate::security::floor::tool_refusal(&stamp, &tool_name, mcp_untrusted_ok).or_else(
                || crate::security::floor::action_refusal(&stamp, &tool_name, &call.params),
            )
        {
            self.emit_floor_denial(call, &stamp, &reason).await;
            let text = format!("Error: {reason}");
            return Ok(Dispatched::refused(text, Outcome::FloorDenied));
        }

        // 4. Build the ToolContext once so the cancel watcher and the
        //    exec closure share the same abort receiver / identity
        //    fields.
        let mut base_ctx = peko_tools_core::ToolContext::for_hook_run(
            call.run_id.clone().unwrap_or_default(),
            call.tool_call_id
                .clone()
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
            &tool_name,
        )
        .with_call_id(call_id)
        .with_agent_id(call.agent_id.clone().unwrap_or_default())
        .with_session_id(call.session_id.clone().unwrap_or_default());
        if let Some(stamp) = call.acting_for.clone() {
            base_ctx = base_ctx.with_acting_for(stamp);
        }
        // No workspace stays `None`: an empty one would be copied into a
        // background task's dispatch and injected as `cwd: ""`.
        if let Some(workspace) = call.workspace.as_deref().filter(|w| !w.is_empty()) {
            base_ctx = base_ctx.with_workspace(workspace);
        }
        if let Some(id) = caller_principal {
            base_ctx = base_ctx.with_principal_id(id);
            if let Some(name) = call.principal_name.as_deref().filter(|n| !n.is_empty()) {
                base_ctx = base_ctx.with_principal_name(name);
            }
        }
        let principal_async = caller_principal.and_then(|_| self.principal_async(&principal_id));
        base_ctx.background = peko_tools_core::BackgroundContext {
            progress: call.background_progress.clone(),
            spawner: principal_async.as_ref().map(|bound| {
                Arc::new(crate::tools::builtin::async_control::RuntimeSpawner(
                    Arc::clone(&bound.runtime),
                )) as Arc<dyn peko_tools_core::BackgroundSpawner>
            }),
        };
        let tool_ctx = match call.abort_signal.as_ref() {
            Some(rx) => base_ctx.with_abort_signal(rx.clone()),
            None => base_ctx,
        };

        // Cancel watcher: if the abort signal fires, invoke
        // `on_interrupt` and stash the notice — the framework always
        // emits a notice on cancel, even for tools that do not
        // implement `InterruptibleTool` (the blanket impl supplies a
        // soft default).
        let cancel_fired = Arc::new(AtomicBool::new(false));
        let notice_slot: Arc<tokio::sync::Mutex<Option<ToolInterruptNotice>>> =
            Arc::new(tokio::sync::Mutex::new(None));
        let interrupt_watch = if let Some(mut rx) = call.abort_signal.clone() {
            let cancel_fired_w = cancel_fired.clone();
            let notice_slot_w = notice_slot.clone();
            let tool_w = tool.clone();
            let tool_ctx_w = tool_ctx.clone();
            Some(tokio_util::task::AbortOnDropHandle::new(tokio::spawn(
                async move {
                    if *rx.borrow() {
                        // Already aborted before we started watching.
                    } else if rx.changed().await.is_err() || !*rx.borrow() {
                        // Sender dropped without a flip, or flipped to false
                        // — do nothing.
                        return;
                    }
                    cancel_fired_w.store(true, Ordering::SeqCst);
                    let notice = tool_w.on_interrupt("", &tool_ctx_w).await;
                    *notice_slot_w.lock().await = Some(notice);
                },
            )))
        } else {
            None
        };

        // 5. Route through the timeout/detach machinery.
        let tool_exec_ctx = ToolExecutionContext::new(
            call.agent_id
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
            call.session_id
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
            "hook_run".to_string(),
        )
        .with_workspace(call.workspace.clone().unwrap_or_else(|| ".".to_string()))
        .with_principal_id(caller_principal.map(str::to_string))
        .with_executor(principal_async.map(|bound| bound.executor));

        let mut params = call.params.clone();
        apply_workspace_injection(&mut params, &tool_name, call.workspace.as_deref());

        // ADR-078 path floor: confinement, the cross-principal boundary,
        // and protected files (a non-owner's write becomes a proposal).
        let workspace = caller_principal.and_then(|_| self.workspace_for(&principal_id));
        // ADR-080 D4: lines a scheduled owner run adds to hot memory
        // without an owner-only source, held back and filed.
        let mut memory_note = None;
        if stamp.is_untrusted() {
            // The file tools default to a runtime-wide directory; an
            // untrusted run's relative paths are its workspace's instead.
            confine_relative_paths(&mut params, &tool_name, workspace.as_deref());
        }
        if let Some((target, write)) = crate::security::floor::file_tool_target(&tool_name, &params)
        {
            if stamp.is_untrusted()
                && tool_name == "Glob"
                && params["pattern"]
                    .as_str()
                    .is_some_and(crate::security::floor::glob_pattern_escapes)
            {
                let reason = "Glob patterns may not leave their base path while acting for an \
                              untrusted actor";
                self.emit_floor_denial(call, &stamp, reason).await;
                let text = format!("Error: {reason}");
                return Ok(Dispatched::refused(text, Outcome::FloorDenied));
            }
            match crate::security::floor::check_path(&stamp, workspace.as_deref(), &target, write) {
                crate::security::floor::PathDecision::Allow => {}
                crate::security::floor::PathDecision::Deny(reason) => {
                    self.emit_floor_denial(call, &stamp, &reason).await;
                    let text = format!("Error: {reason}");
                    return Ok(Dispatched::refused(text, Outcome::FloorDenied));
                }
                crate::security::floor::PathDecision::Propose { relative } => {
                    let workspace = workspace.expect("a proposal needs a workspace");
                    let filed = crate::security::proposals::file(
                        &workspace, &stamp, &relative, &tool_name, &params,
                    )
                    .await;
                    return Ok(match filed {
                        Ok(path) => {
                            let text = format!(
                                "`{relative}` is protected: only the owner writes it. Your \
                                 change was filed for the owner's review at {}; it is not \
                                 applied.",
                                path.display()
                            );
                            let json = serde_json::json!({
                                "proposed": true,
                                "target": relative,
                                "proposal": path.display().to_string(),
                                "message": text,
                            });
                            Dispatched {
                                text,
                                json,
                                success: true,
                                outcome: Outcome::Proposed,
                            }
                        }
                        Err(e) => {
                            let text = format!("Error: could not file the proposal: {e:#}");
                            Dispatched::refused(text, Outcome::Error)
                        }
                    });
                }
            }
            if write && crate::security::memory_gate::is_scheduled_owner(&stamp) {
                if let Some(workspace) = workspace.as_deref() {
                    match self
                        .screen_hot_memory(
                            &principal_id,
                            workspace,
                            &target,
                            &stamp,
                            &tool_name,
                            &mut params,
                        )
                        .await
                    {
                        MemoryScreen::Pass => {}
                        MemoryScreen::Partly(note) => memory_note = Some(note),
                        MemoryScreen::Done(dispatched) => return Ok(dispatched),
                    }
                }
            }
        }

        // ADR-078 D7: a non-owner run's sensitive call goes to the
        // policy reviewer, which never sees the conversation behind it.
        let owner_decides = crate::security::judge::owner_decides(&stamp, &tool_name, &params);
        if owner_decides.is_some()
            || crate::security::judge::needs_review(&stamp, &tool_name, &params)
        {
            use crate::security::judge::{GatedCall, Verdict};
            let gated = GatedCall {
                principal_id: principal_id.0.clone(),
                principal_name: call.principal_name.clone(),
                session_id: call.session_id.clone(),
                agent_id: call.agent_id.clone(),
                workspace: call.workspace.clone(),
                stamp: stamp.clone(),
                tool_name: tool_name.clone(),
                params: params.clone(),
                call_id: Some(call_id.to_string()),
            };
            let verdict = match owner_decides {
                Some(reason) => match self.policy_gate() {
                    Some(gate) if gate.is_approved_replay(&gated).await => Verdict::Allow,
                    _ => Verdict::AskOwner(reason.to_string()),
                },
                None => self.review(&gated).await,
            };
            match verdict {
                Verdict::Allow => {}
                Verdict::Deny(reason) => {
                    let reason = format!("not allowed by the owner's policy: {reason}");
                    self.emit_floor_denial(call, &stamp, &reason).await;
                    let text = format!("Error: {reason}");
                    return Ok(Dispatched::refused(text, Outcome::PolicyDenied));
                }
                Verdict::AskOwner(reason) => {
                    let filed = match self.policy_gate() {
                        Some(gate) => gate.ask_owner(&gated, &reason).await,
                        None => Err(anyhow::anyhow!("no policy reviewer is configured")),
                    };
                    let (text, json, outcome) = match filed {
                        Ok(id) => {
                            let text = format!(
                                "Waiting for the owner's approval ({id}): {reason}. Nothing was \
                                 done yet. If the owner approves, the result arrives in this \
                                 conversation as a completed task."
                            );
                            let json = serde_json::json!({
                                "pending_approval": id,
                                "reason": reason,
                                "message": text,
                            });
                            (text, json, Outcome::AwaitingOwner)
                        }
                        Err(e) => {
                            let text = format!(
                                "Error: the owner must approve this, but asking failed: {e:#}"
                            );
                            (text.clone(), Value::String(text), Outcome::Error)
                        }
                    };
                    return Ok(Dispatched {
                        text,
                        json,
                        success: false,
                        outcome,
                    });
                }
            }
        }

        // Detached tools (including Workflow subprocesses) retain their
        // caller's binding until execution ends, without a registry cycle.
        let run_binding = self.run_binding.upgrade();
        // Kept for the cancel branch: the watcher above is dropped (and so
        // aborted) when execution ends, which can cut it off before it
        // stores the tool's notice.
        let (notice_tool, notice_ctx) = (tool.clone(), tool_ctx.clone());
        let source = metadata.source.clone();
        let late_tool_name = tool_name.clone();
        let exec = move |p| {
            let tool = tool.clone();
            let tool_ctx = tool_ctx.clone();
            async move {
                let _run_binding = run_binding;
                let _interrupt_watch = interrupt_watch;
                let result = std::panic::AssertUnwindSafe(tool.execute_with_context(p, &tool_ctx))
                    .catch_unwind()
                    .await
                    .unwrap_or_else(|_| Err(anyhow::anyhow!("Tool panicked during execution")));
                // A call detached on timeout ends here, long after the
                // dispatcher returned its receipt (ADR-079 D2).
                if let Some(late) = late {
                    late.body_finished(settle(&late_tool_name, &source, &result));
                }
                result
            }
        };
        // A background-task body already runs in the background under its
        // task's timeout and cancellation: run it inline. Routing it would
        // register a second task and impose the foreground timeout, which
        // detached the work and completed the task with a mere receipt.
        let result = if call.background_progress.is_some() {
            exec(params).await
        } else {
            self.router
                .route(&tool_name, &mut params, &tool_exec_ctx, exec)
                .await
        };

        // 6. Cancel wins over a natural completion.
        let cancelled = cancel_fired.load(Ordering::SeqCst)
            || call.abort_signal.as_ref().is_some_and(|rx| *rx.borrow());
        if cancelled {
            let stashed = notice_slot.lock().await.take();
            let notice = match stashed {
                Some(notice) => notice,
                // The tool returned before the watcher stored its notice:
                // ask the tool directly so its preserved / rolled-back
                // report still reaches the agent.
                None => notice_tool.on_interrupt("", &notice_ctx).await,
            };
            let text = notice.to_tool_result_text();
            return Ok(Dispatched {
                json: Value::String(text.clone()),
                text,
                success: true,
                outcome: Outcome::Cancelled,
            });
        }
        let (mut text, mut json, success) = settle(&tool_name, &metadata.source, &result);
        if let Some(note) = memory_note {
            text = format!("{text}\n\n{note}");
            if let Some(object) = json.as_object_mut() {
                object.insert("memory_held".into(), Value::String(note));
            }
        }
        let outcome =
            if call.background_progress.is_none() && ledger::detached_task_id(&json).is_some() {
                Outcome::Detached
            } else {
                outcome_of(success, &json)
            };
        Ok(Dispatched {
            text,
            json,
            success,
            outcome,
        })
    }

    /// ADR-080 D4, D8: screen a scheduled owner run's `Write` / `Edit` of
    /// `kb/MEMORY.md` or `kb/CONVENTIONS.md`. Added lines whose sources
    /// are not all owner-only are removed from `params` and filed for the
    /// owner; the rest is written. When nothing is left to write, the
    /// call ends here.
    async fn screen_hot_memory(
        &self,
        principal_id: &peko_subject::PrincipalId,
        workspace: &std::path::Path,
        target: &std::path::Path,
        stamp: &peko_tools_core::ActingFor,
        tool_name: &str,
        params: &mut Value,
    ) -> MemoryScreen {
        use crate::security::memory_gate::{self, SourceClass, HOT_MEMORY, SOURCED_HOT_FILES};
        let workspace = crate::security::floor::resolve(workspace);
        let target = crate::security::floor::resolve(target);
        let Some(hot) = crate::security::floor::protected_relative(&workspace, &target)
            .and_then(|relative| SOURCED_HOT_FILES.iter().find(|f| **f == relative))
            .copied()
        else {
            return MemoryScreen::Pass;
        };
        let key = match tool_name {
            "Write" => "content",
            "Edit" => "new_string",
            _ => return MemoryScreen::Pass,
        };
        let Some(candidate) = params[key].as_str().map(str::to_string) else {
            return MemoryScreen::Pass;
        };
        let existing = tokio::fs::read_to_string(&target).await.unwrap_or_default();
        let gate = self.policy_gate();
        let mut classes = std::collections::HashMap::new();
        for source in memory_gate::cited(&existing, &candidate) {
            let class = match &gate {
                Some(gate) => gate.source_class(&principal_id.0, &source).await,
                None => SourceClass::Unknown,
            };
            classes.insert(source, class);
        }
        let (kept, held) = memory_gate::screen(&existing, &candidate, &classes);
        if held.is_empty() {
            return MemoryScreen::Pass;
        }
        let filed = match crate::security::proposals::file_held(&workspace, stamp, hot, &held).await
        {
            Ok(paths) => paths,
            Err(e) => {
                let text = format!("Error: could not file the held-back lines: {e:#}");
                return MemoryScreen::Done(Dispatched::refused(text, Outcome::Error));
            }
        };
        let mut reasons = String::new();
        for h in &held {
            reasons.push_str(&format!("\n- {} — {}", h.line.trim(), h.describe()));
        }
        let elsewhere = if hot == HOT_MEMORY {
            "Keep facts about other people in `kb/people/` notes instead."
        } else {
            "Keep lessons from conversations with others in `kb/dreams/` instead."
        };
        let note = format!(
            "{} line(s) were not written to `{hot}` and were filed for the owner ({}). \
             A scheduled run adds a line there only when it ends with `(source: …)` naming \
             sessions or channels that only the owner speaks in. {elsewhere}{reasons}",
            held.len(),
            filed
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
        let unchanged = match tool_name {
            "Write" => kept == existing,
            _ => params["old_string"].as_str() == Some(kept.as_str()),
        };
        if unchanged {
            let json = serde_json::json!({
                "proposed": true,
                "target": hot,
                "proposals": filed.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
                "message": note,
            });
            return MemoryScreen::Done(Dispatched {
                text: note,
                json,
                success: true,
                outcome: Outcome::Proposed,
            });
        }
        params[key] = Value::String(kept);
        MemoryScreen::Partly(note)
    }

    /// Fire an observe-only hook point for the call (PreToolUse /
    /// PostToolUse). The result is discarded by design; the 2s
    /// soft-fail budget applies to each handler in `WorkspaceHookDispatcher`.
    async fn fire_observe(&self, point: WorkspaceHookPoint, call: &ToolCallSpec) {
        let input = HookInput::ToolCall {
            tool_name: call.tool_name.clone(),
            params: call.params.clone(),
            workspace: call.workspace.clone(),
            agent_id: call.agent_id.clone(),
            session_id: call.session_id.clone(),
            acting_for: call.acting_for.clone(),
            principal_id: call.principal_id.clone(),
            principal_name: call.principal_name.clone(),
            abort_signal: None,
        };
        let _ = self.hooks.invoke_hook(point, input).await;
    }

    /// Emit a `Security` audit event for a call the ADR-078 floor refused.
    async fn emit_floor_denial(
        &self,
        call: &ToolCallSpec,
        stamp: &peko_tools_core::ActingFor,
        reason: &str,
    ) {
        tracing::info!(tool = %call.tool_name, actor = %stamp.actor, "trust floor refused a call: {reason}");
        let Some(audit) = self.audit.as_ref() else {
            return;
        };
        let caller = stamp.actor.parse::<peko_subject::ActorRef>().ok();
        let result = audit
            .audit_security_with_caller(
                caller.as_ref(),
                "tool.floor_denied",
                call.agent_id.as_deref(),
                serde_json::json!({
                    "tool_name": call.tool_name,
                    "principal_id": call.principal_id,
                    "session_id": call.session_id,
                    "acting_for": stamp,
                    "reason": reason,
                }),
            )
            .await;
        if let Err(e) = result {
            debug!("failed to emit tool.floor_denied audit event: {e}");
        }
    }

    /// Emit the `tool.call` audit event (Info severity): tool name,
    /// declared action, the call's ids, principal, session, caller, a
    /// digest of the params (never the params themselves — those go to
    /// the principal's ledger), outcome, success, and duration.
    async fn emit_audit(
        &self,
        call: &ToolCallSpec,
        call_id: &str,
        described: &Described,
        outcome: Outcome,
        success: bool,
        elapsed: std::time::Duration,
    ) {
        let Some(audit) = self.audit.as_ref() else {
            return;
        };
        // ADR-078: the stamp's actor is the attributed caller. A group
        // stamp names a channel, not an actor, and attributes to nobody.
        let caller = call
            .acting_for
            .as_ref()
            .and_then(|stamp| stamp.actor.parse::<peko_subject::ActorRef>().ok());
        let result = audit
            .audit_with_caller(
                caller.as_ref(),
                "tool.call",
                call.agent_id.as_deref(),
                serde_json::json!({
                    "call_id": call_id,
                    "run_id": call.run_id,
                    "tool_call_id": call.tool_call_id,
                    "parent": call.parent,
                    "tool_name": call.tool_name,
                    "action": described.action,
                    "agent_id": call.agent_id,
                    "principal_id": call.principal_id,
                    "principal_name": call.principal_name,
                    "session_id": call.session_id,
                    "acting_for": call.acting_for,
                    "params_digest": described.params_digest,
                    "outcome": outcome.as_str(),
                    "success": success,
                    "duration_ms": elapsed.as_millis() as u64,
                }),
            )
            .await;
        if let Err(e) = result {
            warn!("failed to emit tool.call audit event: {e:#}");
        }
    }

    /// Emit a `Security` audit event for a ledger write that failed
    /// (ADR-079 D5).
    async fn emit_ledger_failure(&self, call: &ToolCallSpec, call_id: &str, error: &anyhow::Error) {
        warn!(tool = %call.tool_name, "tool ledger write failed: {error:#}");
        let Some(audit) = self.audit.as_ref() else {
            return;
        };
        let result = audit
            .audit_security(
                ledger::EVENT_WRITE_FAILED,
                call.agent_id.as_deref(),
                serde_json::json!({
                    "call_id": call_id,
                    "tool_name": call.tool_name,
                    "principal_id": call.principal_id,
                    "session_id": call.session_id,
                    "acting_for": call.acting_for,
                    "error": format!("{error:#}"),
                }),
            )
            .await;
        if let Err(e) = result {
            warn!(
                "failed to emit {} audit event: {e:#}",
                ledger::EVENT_WRITE_FAILED
            );
        }
    }
}

/// What [`ToolDispatcher::execute_inner`] settled on.
struct Dispatched {
    text: String,
    json: Value,
    success: bool,
    outcome: Outcome,
}

impl Dispatched {
    /// A refusal: the agent sees `text` as a failure.
    fn refused(text: String, outcome: Outcome) -> Self {
        Self {
            json: Value::String(text.clone()),
            text,
            success: false,
            outcome,
        }
    }
}

/// What both logs say about a call before it runs.
struct Described {
    action: Option<String>,
    source: String,
    params_digest: String,
}

/// The agent-facing `(text, json, success)` for a tool's raw result.
fn settle(
    tool_name: &str,
    source: &crate::tools::metadata::ToolSource,
    result: &Result<Value>,
) -> (String, Value, bool) {
    match result {
        Ok(value) => {
            let text = match value {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            // Agent admission refusals retain their structured envelope,
            // but are failures for audit, async dispatch and cron history.
            let success = !(tool_name == "Agent"
                && value["error_type"] == "ConcurrentLimitExceeded")
                && !(matches!(source, crate::tools::metadata::ToolSource::Mcp { .. })
                    && value["isError"] == true);
            (text, value.clone(), success)
        }
        Err(e) => {
            // `{:#}` keeps the cause chain: a `.context()` label
            // alone ("Failed to read file") hides the reason.
            let text = format!("Error: {e:#}");
            (text.clone(), Value::String(text), false)
        }
    }
}

/// The ledger outcome of a settled call. A tool that reports its own
/// failure (`"success": false`, e.g. a non-zero Bash exit) is an error
/// in the ledger even though the call itself succeeded.
fn outcome_of(success: bool, json: &Value) -> Outcome {
    if success && json.get("success") != Some(&Value::Bool(false)) {
        Outcome::Ok
    } else {
        Outcome::Error
    }
}

/// The late `end` of a call that may detach on the router's timeout
/// (ADR-079 D2). The dispatcher writes the `detached` end first, then
/// calls [`Self::detached`]; the body calls [`Self::body_finished`].
/// Whichever comes second writes the late end, so it always follows the
/// detached one.
struct LateEnd {
    ledger: Arc<Ledger>,
    redactor: Arc<Redactor>,
    call_id: String,
    started: Instant,
    state: std::sync::Mutex<LateState>,
}

enum LateState {
    Running,
    Detached,
    Finished((String, Value, bool), u64),
    Done,
}

impl LateEnd {
    fn new(ledger: Arc<Ledger>, redactor: Arc<Redactor>, call_id: String) -> Self {
        Self {
            ledger,
            redactor,
            call_id,
            started: Instant::now(),
            state: std::sync::Mutex::new(LateState::Running),
        }
    }

    fn record(
        &self,
        settled: &(String, Value, bool),
        duration_ms: u64,
    ) -> serde_json::Map<String, Value> {
        let (text, json, success) = settled;
        ledger::end_record(
            &self.call_id,
            &ledger::End {
                outcome: outcome_of(*success, json),
                duration_ms: Some(duration_ms),
                exit_code: ledger::exit_code(json),
                task_id: None,
                result: Some(ledger::excerpt(text, json, &self.redactor)),
                late: true,
            },
        )
    }

    fn body_finished(&self, settled: (String, Value, bool)) {
        let duration_ms = self.started.elapsed().as_millis() as u64;
        let mut state = self.state.lock().expect("late end poisoned");
        if matches!(*state, LateState::Detached) {
            *state = LateState::Done;
            drop(state);
            let record = self.record(&settled, duration_ms);
            let ledger = Arc::clone(&self.ledger);
            tokio::spawn(async move {
                if let Err(e) = ledger.append(record).await {
                    warn!("tool ledger write failed for a detached call: {e:#}");
                }
            });
        } else {
            *state = LateState::Finished(settled, duration_ms);
        }
    }

    async fn detached(&self, _task_id: String) {
        let finished = {
            let mut state = self.state.lock().expect("late end poisoned");
            match std::mem::replace(&mut *state, LateState::Detached) {
                LateState::Finished(settled, duration_ms) => {
                    *state = LateState::Done;
                    Some((settled, duration_ms))
                }
                _ => None,
            }
        };
        if let Some((settled, duration_ms)) = finished {
            if let Err(e) = self.ledger.append(self.record(&settled, duration_ms)).await {
                warn!("tool ledger write failed for a detached call: {e:#}");
            }
        }
    }
}

/// Hex-encode bytes (lowercase, no separators).
fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, b| {
            write!(out, "{b:02x}").expect("writing to String cannot fail");
            out
        })
}

/// Anchor an untrusted run's file-tool paths at its principal's
/// workspace (ADR-078 D6): relative paths join it, and a `Glob`/`Grep`
/// without a `path` searches it. With no bound workspace, nothing changes
/// and the floor denies the call.
fn confine_relative_paths(
    params: &mut Value,
    tool_name: &str,
    workspace: Option<&std::path::Path>,
) {
    let (Some(obj), Some(workspace)) = (params.as_object_mut(), workspace) else {
        return;
    };
    let key = match tool_name {
        "Read" | "Write" | "Edit" => "file_path",
        "Glob" | "Grep" => "path",
        _ => return,
    };
    let anchored = match obj.get(key).and_then(Value::as_str) {
        Some(raw) => {
            let path = crate::tools::builtin::paths::expand_tilde(raw);
            if path.is_absolute() {
                return;
            }
            workspace.join(path)
        }
        None if key == "path" => workspace.to_path_buf(),
        None => return,
    };
    obj.insert(
        key.to_string(),
        Value::String(anchored.to_string_lossy().into_owned()),
    );
}

/// Inject the caller's workspace into filesystem-tool params.
fn apply_workspace_injection(params: &mut Value, tool_name: &str, workspace: Option<&str>) {
    let Some(obj) = params.as_object_mut() else {
        return;
    };
    // Inject agent workspace into tool parameters for filesystem tools.
    if let Some(ws) = workspace.filter(|w| !w.is_empty()) {
        match tool_name {
            "Glob" => {
                if !obj.contains_key("path") {
                    obj.insert("path".to_string(), Value::String(ws.to_string()));
                }
            }
            "Grep" => {
                if !obj.contains_key("path") {
                    obj.insert("path".to_string(), Value::String(ws.to_string()));
                }
            }
            "Bash" => {
                if !obj.contains_key("cwd") {
                    obj.insert("cwd".to_string(), Value::String(ws.to_string()));
                }
            }
            "Write" | "Edit" | "Read" => {
                if let Some(path_str) = obj.get("file_path").and_then(|v| v.as_str()) {
                    let path_buf = std::path::PathBuf::from(path_str);
                    if !path_buf.is_absolute() {
                        let resolved = std::path::PathBuf::from(ws).join(path_str);
                        obj.insert(
                            "file_path".to_string(),
                            Value::String(resolved.to_string_lossy().to_string()),
                        );
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::metadata::ToolSource;
    use peko_tools_core::{Tool, ToolContext};

    struct ProbeTool;
    #[async_trait::async_trait]
    impl Tool for ProbeTool {
        fn name(&self) -> &str {
            "Read"
        }
        fn description(&self) -> String {
            "dispatcher probe".into()
        }
        fn parameters(&self) -> Value {
            serde_json::json!({"type":"object","properties":{"file_path":{"type":"string"},"panic":{"type":"boolean"},"action":{"type":"string","enum":["read"],"default":"read"}},"required":["file_path"]})
        }
        async fn execute(&self, _: Value) -> Result<Value> {
            unreachable!("context required")
        }
        async fn execute_with_context(&self, params: Value, ctx: &ToolContext) -> Result<Value> {
            assert_eq!(ctx.agent_id.as_deref(), Some("did:agent"));
            assert_eq!(ctx.session_id.as_deref(), Some("session"));
            assert_eq!(ctx.principal_id.as_deref(), Some("principal"));
            assert!(params["panic"] != true, "fixture panic");
            Ok(params)
        }
    }

    /// A call without a workspace injects nothing: an empty one used to
    /// reach background task bodies as `cwd: ""` and fail every spawn.
    #[test]
    fn empty_workspace_is_not_injected() {
        for tool in ["Bash", "Glob", "Grep"] {
            let mut params = serde_json::json!({});
            apply_workspace_injection(&mut params, tool, Some(""));
            assert_eq!(params, serde_json::json!({}), "{tool}");
            apply_workspace_injection(&mut params, tool, Some("/ws"));
            let key = if tool == "Bash" { "cwd" } else { "path" };
            assert_eq!(params[key], "/ws", "{tool}");
        }
    }

    /// A tool error reaches the model with its cause, not just the
    /// outermost `.context()` label.
    #[tokio::test]
    async fn tool_errors_keep_their_cause_chain() {
        use anyhow::Context as _;
        struct Failing;
        #[async_trait::async_trait]
        impl Tool for Failing {
            fn name(&self) -> &str {
                "Failing"
            }
            fn description(&self) -> String {
                "fails".into()
            }
            fn parameters(&self) -> Value {
                serde_json::json!({"type": "object"})
            }
            async fn execute(&self, _: Value) -> Result<Value> {
                Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "no such file",
                ))
                .context("Failed to read notes.md")
            }
        }
        let catalog = Arc::new(ToolCatalog::new());
        catalog
            .register_system(Arc::new(Failing), ToolSource::BuiltIn)
            .await;
        let dispatcher = ToolDispatcher::new(
            catalog,
            Arc::new(WorkspaceHookDispatcher::new()),
            Arc::new(AsyncExecutionRouter::new()),
            None,
        );
        let mut call = ToolCallSpec::new("Failing", serde_json::json!({}));
        call.acting_for = Some(peko_tools_core::ActingFor::runtime());
        call.principal_id = Some("principal".into());
        let (text, _, success) = dispatcher.execute(call).await.unwrap();
        assert!(!success);
        assert_eq!(text, "Error: Failed to read notes.md: no such file");
    }

    #[tokio::test]
    async fn attributed_dispatch_audits_success_validation_unknown_and_panic_once() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        let audit = Arc::new(
            peko_observability::Observability::with_audit_dir("test", dir.path().to_path_buf())
                .unwrap(),
        );
        let catalog = Arc::new(ToolCatalog::new());
        catalog
            .register_system(Arc::new(ProbeTool), ToolSource::BuiltIn)
            .await;
        let dispatcher = ToolDispatcher::new(
            catalog,
            Arc::new(WorkspaceHookDispatcher::new()),
            Arc::new(AsyncExecutionRouter::new()),
            Some(Arc::clone(&audit)),
        );
        for (name, params, expected) in [
            ("Read", serde_json::json!({"file_path":"secret"}), true),
            ("Read", serde_json::json!({}), false),
            ("Unknown", serde_json::json!({}), false),
            (
                "Read",
                serde_json::json!({"file_path":"secret","panic":true}),
                false,
            ),
        ] {
            let mut call = ToolCallSpec::new(name, params);
            call.agent_id = Some("did:agent".into());
            call.session_id = Some("session".into());
            call.principal_id = Some("principal".into());
            call.acting_for = Some(peko_tools_core::ActingFor {
                actor: "user:hub:alice".into(),
                ..peko_tools_core::ActingFor::runtime()
            });
            call.workspace = Some(workspace.to_string_lossy().into_owned());
            let (_, result, success) = dispatcher.execute(call).await.unwrap();
            assert_eq!(success, expected);
            if success {
                assert_eq!(
                    result["file_path"],
                    workspace.join("secret").to_string_lossy().as_ref()
                );
            }
        }
        let events = audit.get_audit_log(10).await;
        assert_eq!(events.len(), 4);
        for event in &events {
            assert_eq!(event.event_type, "tool.call");
            if event.details["tool_name"] == "Read" {
                assert_eq!(event.details["action"], "read", "declared default action");
            } else {
                assert!(event.details["action"].is_null());
            }
            assert_eq!(event.agent_did.as_deref(), Some("did:agent"));
            assert_eq!(event.details["principal_id"], "principal");
            assert_eq!(event.details["session_id"], "session");
            assert_eq!(event.details["acting_for"]["actor"], "user:hub:alice");
            assert!(!event.details.to_string().contains("secret"));
            assert_eq!(event.details["params_digest"].as_str().unwrap().len(), 71);
        }
        let contents: String = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|file| std::fs::read_to_string(file.unwrap().path()).unwrap())
            .collect();
        assert_eq!(
            contents.lines().count(),
            4,
            "each dispatch is durably audited once"
        );
    }

    /// Waits for the abort signal (or `ms`), then reports which happened.
    /// Its interrupt notice records a preserved and a rolled-back effect.
    struct Interruptible {
        ignore_abort: bool,
        ms: u64,
    }
    #[async_trait::async_trait]
    impl Tool for Interruptible {
        fn name(&self) -> &str {
            "Interruptible"
        }
        fn description(&self) -> String {
            "cancel probe".into()
        }
        async fn execute(&self, _: Value) -> Result<Value> {
            unreachable!("context required")
        }
        async fn execute_with_context(&self, _: Value, ctx: &ToolContext) -> Result<Value> {
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(self.ms);
            while std::time::Instant::now() < deadline {
                if !self.ignore_abort && ctx.is_aborted() {
                    return Ok(serde_json::json!("observed abort"));
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            Ok(serde_json::json!("finished"))
        }
        async fn on_interrupt(&self, id: &str, ctx: &ToolContext) -> ToolInterruptNotice {
            let mut notice = ToolInterruptNotice::soft_default(id, ctx.tool_name.as_str());
            notice.preserved = vec!["wrote a.txt".into()];
            notice.rolled_back = vec!["temp files".into()];
            notice
        }
    }

    async fn dispatch_with_abort(
        tool: Interruptible,
        abort_after: Option<std::time::Duration>,
    ) -> (
        (String, Value, bool),
        Vec<peko_observability::AuditEvent>,
        std::time::Duration,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let audit = Arc::new(
            peko_observability::Observability::with_audit_dir("test", dir.path().to_path_buf())
                .unwrap(),
        );
        let catalog = Arc::new(ToolCatalog::new());
        catalog
            .register_system(Arc::new(tool), ToolSource::BuiltIn)
            .await;
        let dispatcher = ToolDispatcher::new(
            catalog,
            Arc::new(WorkspaceHookDispatcher::new()),
            Arc::new(AsyncExecutionRouter::new()),
            Some(Arc::clone(&audit)),
        );
        let (tx, rx) = tokio::sync::watch::channel(abort_after == Some(Default::default()));
        let mut call = ToolCallSpec::new("Interruptible", serde_json::json!({}));
        call.acting_for = Some(peko_tools_core::ActingFor::runtime());
        call.principal_id = Some("principal".into());
        call.abort_signal = Some(rx);
        if let Some(delay) = abort_after.filter(|d| !d.is_zero()) {
            tokio::spawn(async move {
                tokio::time::sleep(delay).await;
                let _ = tx.send(true);
            });
        } else {
            std::mem::forget(tx);
        }
        let start = std::time::Instant::now();
        let result = dispatcher.execute(call).await.unwrap();
        let elapsed = start.elapsed();
        (result, audit.get_audit_log(10).await, elapsed)
    }

    /// An abort mid-call returns the tool's own interrupt notice — what was
    /// preserved and rolled back — as a successful result, audited once.
    #[tokio::test]
    async fn abort_mid_call_returns_the_tools_interrupt_notice() {
        let tool = Interruptible {
            ignore_abort: false,
            ms: 10_000,
        };
        let ((text, value, success), events, elapsed) =
            dispatch_with_abort(tool, Some(std::time::Duration::from_millis(100))).await;
        assert!(success, "a cancelled call is not a tool failure: {text}");
        assert!(
            text.starts_with("[Interruptible call was CANCELLED]"),
            "{text}"
        );
        assert!(text.contains("Preserved: wrote a.txt"), "{text}");
        assert!(text.contains("temp files"), "{text}");
        assert_eq!(value, Value::String(text.clone()));
        assert!(elapsed < std::time::Duration::from_secs(5), "{elapsed:?}");
        assert_eq!(events.len(), 1, "audited once");
        assert_eq!(events[0].details["tool_name"], "Interruptible");
    }

    /// Cancel wins: a tool that ignores the signal and completes still
    /// yields the interrupt notice, not its natural result.
    #[tokio::test]
    async fn cancel_wins_over_a_natural_completion() {
        let tool = Interruptible {
            ignore_abort: true,
            ms: 300,
        };
        let ((text, _, success), _, _) =
            dispatch_with_abort(tool, Some(std::time::Duration::from_millis(50))).await;
        assert!(success);
        assert!(text.contains("CANCELLED"), "{text}");
        assert!(!text.contains("finished"), "{text}");
    }

    /// A call aborted before it starts is reported as cancelled.
    #[tokio::test]
    async fn abort_before_the_call_starts_is_reported_as_cancelled() {
        let tool = Interruptible {
            ignore_abort: false,
            ms: 10_000,
        };
        let ((text, _, success), _, elapsed) =
            dispatch_with_abort(tool, Some(std::time::Duration::ZERO)).await;
        assert!(success);
        assert!(text.contains("CANCELLED"), "{text}");
        assert!(elapsed < std::time::Duration::from_secs(5), "{elapsed:?}");
    }

    /// Without an abort the tool's result passes through untouched.
    #[tokio::test]
    async fn no_abort_returns_the_tools_result() {
        let tool = Interruptible {
            ignore_abort: false,
            ms: 20,
        };
        let ((text, value, success), _, _) = dispatch_with_abort(tool, None).await;
        assert!(success);
        assert_eq!(value, serde_json::json!("finished"), "{text}");
    }
}
