//! Per-principal runtime context.
//!
//! [`PrincipalContext`] bundles the state that all agents of a single
//! principal (the root agent and any subagents it spawns via the `Agent`
//! tool) need to operate:
//!
//! - the principal's own memory, inbox, and session-creation lock
//! - the principal's workspace path and provider resolver
//! - the principal's allowed extension list
//! - the principal's resolved (provider, model) preference
//!
//! It also owns a lazily-built, **per-principal** [`ExtensionCore`]
//! shared by every agent of that principal. The core is *not* privileged
//! over subagent cores — the root agent and every subagent resolve the
//! exact same core through this struct. Per-agent visibility is enforced
//! by each agent's own extension whitelist; the core just hosts the
//! tool *instances*.
//!
//! This is the post-Phase-1 realisation of the design rule "the root
//! agent is but another agent of the principal, simply user-facing".

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use crate::extensions::agent::{register_agents_with_core, AgentAdapter};
use crate::extensions::builtin::BuiltinToolAdapter;
use crate::extensions::framework::core::{global_core, ExtensionCore};
use crate::principal::memory::PrincipalMemory;
use crate::principal::router::AgentPromptSummary;
use crate::principal::seen_models::{seen_models_path, SeenModels};
use crate::tools::builtin::{AgentCatalogTool, SkillTool};
use peko_observability::Observability;
use peko_providers::LlmResolver;
use peko_session::InboxRegistry;
use peko_subject::PrincipalId;

use peko_extension_api::Capabilities;

/// Per-principal runtime state shared by the root agent and its
/// subagents.
///
/// Constructed once per principal at startup, cached on the
/// `RootRouter`, and passed by reference into the principal's
/// root-agent runner. Subagents don't need a fresh context — they read
/// the principal's tools off this struct's core, and their own
/// extension whitelist filters what's actually visible to them.
pub struct PrincipalContext {
    /// Principal's on-disk workspace root
    /// (`{config_dir}/principals/{name}`).
    pub workspace_path: PathBuf,
    /// Sessions directory for this principal. Mirrors
    /// `memory.sessions_dir()` so callers don't have to walk through
    /// the memory trait to find it.
    pub sessions_dir: PathBuf,
    /// Principal-scoped memory (sessions/artifacts/todos).
    pub memory: Arc<dyn PrincipalMemory>,
    /// Per-principal plan DAG port (PR #1 of four in the wiring
    /// sequence; tools wired in PR #2). The principal harness reaches
    /// plans through the dyn-trait boundary so future impls
    /// (in-memory, network-backed) slot in without rewriting this
    /// construction site. Used by `agent_runner.rs` to thread the
    /// handle into `Agent::with_principal_plan_port` so the seven
    /// `Plan*` tools are registered on the principal's agents.
    pub plan_port: Arc<dyn peko_plan::PlanPort>,
    /// Shared inbox the dispatcher pushes steering messages into.
    pub inbox_registry: Arc<InboxRegistry>,
    /// Held during root-agent session creation so concurrent peers
    /// don't race on shared session metadata.
    pub session_creation_lock: Arc<tokio::sync::Mutex<()>>,
    /// Principal's capability grants — what tools/skills/mcps/agents are
    /// enabled for this principal.
    pub capabilities: Arc<Capabilities>,
    /// LLM resolver used to validate provider hints and surface
    /// catalog defaults.
    pub resolver: Option<Arc<LlmResolver>>,
    /// Per-principal configured model preference from `principal.toml`.
    /// When `Some`, this model id is used for every LLM call for this
    /// principal unless overridden per-message.
    pub provider_hint: Option<String>,

    /// Per-message configured model override (e.g. `peko send --model`).
    /// Mirrored from `RouterContext` at root-agent construction time and
    /// threaded into `Agent::init_provider` so the resolver classifies
    /// the resolution as `ResolveSource::ExplicitOverride`. `None`
    /// preserves the principal-config chain.
    pub message_override: Option<String>,

    /// Built-in default prompt body — the compiled-in root agent
    /// prompt or a workspace-relative override. Captured at
    /// construction so the runner doesn't have to walk the principal's
    /// config every message.
    root_prompt: OnceLock<Arc<crate::principal::agent_prompt::AgentPrompt>>,

    /// The principal's runtime id. Stable across the principal's
    /// lifetime; carried through agent + subagent construction so
    /// descendant spawns inherit the same principal scope.
    principal_id: PrincipalId,
    /// Caller identity for outbound `principal_send` envelopes. Both
    /// fields are `None` until set via [`Self::set_caller_identity`]
    /// (usually at `RootRouter::build_context` time). When
    /// either is `None`, `Agent::init_builtins_async` skips
    /// registering `send_peer` — the tool needs a stable caller
    /// identity to attribute outbound requests under
    /// `Subject::Principal(caller_principal_did)`.
    caller_principal_did: OnceLock<String>,
    caller_runtime_id: OnceLock<String>,
    /// Optional observability hub. Set from the `RouterContext` by the root
    /// router so subagent spawns can be audited under the parent principal.
    observability: OnceLock<Arc<Observability>>,
    /// Snapshot of the extension IDs that are active for this principal on
    /// this message. Derived from the `RouterContext::active_extensions`
    /// snapshot and consulted by the agent's tool gate so a tool is only
    /// callable when both its capability is granted and its owning extension
    /// is active.
    active_extensions: OnceLock<peko_extension_api::ActiveExtensionSet>,
    // F19 (revised 2026-08-01 v2): the engine loop never managed to
    // fetch the principal's meter directly — the dispatcher boundary
    // is the only point with a `PrincipalManager` reference in scope,
    // and `PrincipalContext` is what flows down to
    // `run_root_agent_prompt_with_callback`. Resolved in
    // `RootRouter::build_context` from the `RouterContext` snapshot
    // populated by `PrincipalManager::build_router_context`. Set once
    // and read by `agent_runner` to charge the per-cycle counter on
    // every LLM call (Bug A).
    quota_meter: OnceLock<Arc<peko_quota::QuotaMeter>>,
    // Phase 4 (`feature/multi-model-subagents`):
    // per-principal set of model ids the principal has called.
    // `mark_model_seen(model_id)` returns `true` on the first use of
    // a model — the caller picks `Warning` severity for that
    // audit row, `Info` for subsequent calls. Persisted at
    // `<workspace_path>/seen_models.json` via the atomic-rename
    // pattern in `seen_models::SeenModels::save`. `BTreeSet`
    // matches the on-disk shape (`SeenModels::models`) so the
    // in-memory mirror and the file stay byte-equivalent after
    // save.
    seen_models: Arc<Mutex<BTreeSet<String>>>,
}

impl PrincipalContext {
    /// Build a `PrincipalContext` from already-resolved principal
    /// state.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        workspace_path: PathBuf,
        memory: Arc<dyn PrincipalMemory>,
        inbox_registry: Arc<InboxRegistry>,
        session_creation_lock: Arc<tokio::sync::Mutex<()>>,
        capabilities: Arc<Capabilities>,
        resolver: Option<Arc<LlmResolver>>,
        provider_hint: Option<String>,
        // Per-message configured model override. Mirrored from
        // `RouterContext` and consumed by `Agent::init_provider`.
        message_override: Option<String>,
        principal_id: PrincipalId,
        // PR #2 wiring: per-principal plan DAG port.
        plan_port: Arc<dyn peko_plan::PlanPort>,
    ) -> Self {
        let sessions_dir = memory.sessions_dir().clone();
        // Phase 4: load per-principal `seen_models.json`. A missing
        // file is expected on a fresh principal — fall open to an
        // empty set so the first LLM call surfaces a `Warning` row.
        // Parse failures are tolerated: the file is small and
        // self-healing (the next `mark_model_seen` rewrites it
        // cleanly), so dropping the corrupted content is preferable
        // to refusing the principal.
        let seen_path = seen_models_path(&workspace_path);
        let initial_seen = SeenModels::load(&seen_path).map_or_else(
            |e| {
                tracing::warn!(
                    "failed to parse seen_models.json at {}: {e}; \
                     starting with empty set (next mark_model_seen will rewrite)",
                    seen_path.display()
                );
                SeenModels::empty()
            },
            |seen| seen,
        );
        Self {
            workspace_path,
            sessions_dir,
            memory,
            inbox_registry,
            session_creation_lock,
            capabilities,
            resolver,
            provider_hint,
            message_override,
            plan_port,
            root_prompt: OnceLock::new(),
            principal_id,
            caller_principal_did: OnceLock::new(),
            caller_runtime_id: OnceLock::new(),
            observability: OnceLock::new(),
            active_extensions: OnceLock::new(),
            quota_meter: OnceLock::new(),
            seen_models: Arc::new(Mutex::new(initial_seen.models)),
        }
    }

    /// Get the principal's runtime id. Stable for the principal's
    /// lifetime; used to thread `principal_id` through the agent +
    /// subagent constructors so descendant spawns inherit the same
    /// principal scope.
    #[must_use]
    pub fn principal_id(&self) -> &PrincipalId {
        &self.principal_id
    }

    // F19: removed `quota_meter()` accessor. The engine loop fetches
    // the principal's meter directly from `Principal.quota_meter`
    // at run entrypoint.

    /// Get the principal's human-readable name.
    ///
    /// The name is derived from the final component of the principal's
    /// workspace path (`{config_dir}/principals/{name}`). It matches the
    /// name used by `PrincipalManager::get_by_name` and is the value cron
    /// tools stamp on jobs.
    #[must_use]
    pub fn name(&self) -> &str {
        self.workspace_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
    }

    /// Bind the caller's principal DID for outbound `principal_send`
    /// envelopes. Set once at `RootRouter::build_context` from
    /// `Principal::did()` (Phase 4b). Idempotent: subsequent calls
    /// return the existing value rather than overwriting.
    pub fn set_caller_principal_did(&self, did: String) -> Result<(), String> {
        self.caller_principal_did
            .set(did)
            .map_err(|existing| format!("caller_principal_did already set to {existing:?}"))
    }

    /// Bind the caller's runtime id for outbound `principal_send`
    /// envelopes. Set once (post-`start_tunnel`) from
    /// `CrossRuntimeA2aCtx::caller_runtime_id`. Idempotent.
    pub fn set_caller_runtime_id(&self, runtime_id: String) -> Result<(), String> {
        self.caller_runtime_id
            .set(runtime_id)
            .map_err(|existing| format!("caller_runtime_id already set to {existing:?}"))
    }

    /// Caller principal DID (if bound). Used to attribute
    /// `principal_send` outbound requests.
    #[must_use]
    pub fn caller_principal_did(&self) -> Option<&String> {
        self.caller_principal_did.get()
    }

    /// Caller runtime id (if bound). Echoed into the
    /// `caller_runtime_id` field of outbound `principal_send`
    /// envelopes for signature verification.
    #[must_use]
    pub fn caller_runtime_id(&self) -> Option<&String> {
        self.caller_runtime_id.get()
    }

    /// Bind the observability hub for this principal context. Idempotent.
    pub fn set_observability(
        &self,
        observability: Arc<Observability>,
    ) -> Result<(), Arc<Observability>> {
        self.observability.set(observability)
    }

    /// Get the observability hub, if bound.
    #[must_use]
    pub fn observability(&self) -> Option<&Arc<Observability>> {
        self.observability.get()
    }

    /// Bind the active extension snapshot for this principal context.
    /// Idempotent.
    pub fn set_active_extensions(
        &self,
        active_extensions: peko_extension_api::ActiveExtensionSet,
    ) -> Result<(), peko_extension_api::ActiveExtensionSet> {
        self.active_extensions.set(active_extensions)
    }

    /// Snapshot of extension IDs active for this principal. Returns an empty
    /// set if no snapshot has been bound.
    #[must_use]
    pub fn active_extensions(&self) -> &peko_extension_api::ActiveExtensionSet {
        self.active_extensions
            .get_or_init(peko_extension_api::ActiveExtensionSet::empty)
    }

    /// Bind the principal's quota meter (Bug A, 2026-08-01 v2). Set
    /// once in `RootRouter::build_context` from the `RouterContext`
    /// snapshot, then read by `agent_runner` to charge the per-cycle
    /// counter on every LLM call. Idempotent.
    pub fn set_quota_meter(
        &self,
        meter: Arc<peko_quota::QuotaMeter>,
    ) -> Result<(), Arc<peko_quota::QuotaMeter>> {
        self.quota_meter.set(meter)
    }

    /// Bound principal quota meter, if any.
    #[must_use]
    pub fn quota_meter(&self) -> Option<&Arc<peko_quota::QuotaMeter>> {
        self.quota_meter.get()
    }

    /// Phase 4 (`feature/multi-model-subagents`):
    /// idempotent first-use check for `model_id`. Pure read — does
    /// not mutate state. Use [`Self::mark_model_seen`] instead when
    /// the caller wants to record the use.
    #[must_use]
    pub fn has_model_seen(&self, model_id: &str) -> bool {
        let guard = self
            .seen_models
            .lock()
            .expect("seen_models mutex poisoned");
        guard.contains(model_id)
    }

    /// Phase 4 (`feature/multi-model-subagents`):
    /// return a clone of the `Arc<Mutex<BTreeSet<String>>>` that
    /// backs the seen-models state. Callers (the
    /// `principal/agent_runner.rs` audit-sink binding) keep the
    /// clone alive across an `&PrincipalContext` borrow so they
    /// can build `'static + Send + Sync` closures for the
    /// engine loop's `with_audit_sink`. Mutations performed by
    /// [`Self::mark_model_seen`] on the principal side are visible
    /// to clones because they share the same `Arc`.
    #[must_use]
    pub fn seen_models_handle(&self) -> Arc<Mutex<BTreeSet<String>>> {
        Arc::clone(&self.seen_models)
    }

    /// Phase 4 (`feature/multi-model-subagents`):
    /// record that `model_id` has been used by this principal.
    /// Returns `true` if this is the **first** use, `false`
    /// otherwise. The caller uses the boolean to pick
    /// `Warning` severity for the audit row on first use, `Info`
    /// thereafter (so `peko audit tail` can surface new-model
    /// warnings without spamming on every repeat call).
    ///
    /// ## Persistence
    ///
    /// The first-use insert persists to
    /// `<workspace_path>/seen_models.json` via the atomic-rename
    /// pattern in [`SeenModels::save`]. Repeat calls do not rewrite
    /// the file (the `add_then_save` no-op optimization). On
    /// persistence errors we keep the in-memory state in sync but
    /// log the error — the worst case is that the next principal
    /// boot replays the first-use warning once after a power
    /// failure, which is acceptable.
    pub fn mark_model_seen(&self, model_id: &str) -> bool {
        let path = seen_models_path(&self.workspace_path);
        let mut guard = self
            .seen_models
            .lock()
            .expect("seen_models mutex poisoned");
        let mut snapshot = SeenModels {
            version: 1,
            models: guard.clone(),
        };
        let fresh = snapshot
            .add_then_save(model_id, &path)
            .unwrap_or_else(|e| {
                tracing::warn!(
                    "failed to persist seen_models.json at {}: {e}; \
                     in-memory state remains authoritative",
                    path.display()
                );
                // `add_then_save` returned Err only after
                // successfully inserting into the in-memory set.
                // We don't have the boolean separately, but if
                // we hit this branch, the in-memory insert did
                // happen — assume fresh=true so the caller still
                // gets the audit warning. The next call on the
                // same id will short-circuit on the in-memory
                // set and return `false`.
                true
            });
        // Always sync the in-memory set, even when the save
        // failed — keeps `mark_model_seen` consistent with what
        // the caller would observe on the next call.
        *guard = snapshot.models;
        fresh
    }

    /// Get the daemon-global `ExtensionCore` and ensure the
    /// principal's tool bag is wired onto it.
    ///
    /// There is one daemon-wide [`ExtensionCore`]. The principal's
    /// discovered `<workspace>/agents/*` entries are installed on that
    /// core on first call via [`install_principal_tool_bag`];
    /// subsequent callers observe the same global core and the same
    /// tool bag.
    ///
    /// Visibility to any single agent is still governed by the agent's
    /// own extension whitelist; this method does not assume
    /// privilege.
    pub async fn core(&self) -> Arc<ExtensionCore> {
        let core = global_core().unwrap_or_else(|| {
            // Fall back to a freshly-allocated core if the daemon
            // hasn't initialised the global core yet. The
            // `Agent::new_*` callers depend on `global_core()` being
            // populated by `init_global_core` at app startup; this
            // branch is mostly a safety net for unit tests that
            // construct an `Agent` directly.
            Arc::new(ExtensionCore::new())
        });
        if !core.universal_extensions_loaded() {
            // Phase A: derive the typed agents dir from the
            // principal's Shared layout. `self.workspace_path` is
            // the Shared tier root, so the agents dir is exactly
            // `workspace_path.join("agents")`.
            let agents_dir = self.workspace_path.join("agents");
            // Phase 2 PR 1 (ADR-047): skills live under
            // `<workspace>/skills/<id>/SKILL.md`; the SkillTool's
            // runtime reads from that directory.
            let skills_dir = self.workspace_path.join("skills");
            // Phase 2 PR 2 (ADR-047 §2.3): MCP servers live under
            // `<workspace>/mcp/<id>/server.json`. The workspace
            // scanner registers each server config with the global
            // `McpManager` (lazy — servers start on first tool call)
            // and wraps the manager's running tools as
            // `McpToolProxy` / `InjectableMcpToolProxy` instances
            // onto the global core.
            let mcp_dir = self.workspace_path.join("mcp");
            // Phase 2 PR 3 (ADR-047 §2.1): universal tools live
            // under `<workspace>/tools/<id>/manifest.yaml`. The
            // scanner reads each manifest, constructs the
            // canonical `peko_tools_core::Tool` impl (no framework
            // hook layer), and registers it via
            // `BuiltinToolAdapter::register_tool`.
            let tools_dir = self.workspace_path.join("tools");
            // Channel port resolution (2026-08-18 reviewer finding):
            // principal contexts don't hold their own channel port —
            // the daemon builds the real file-backed port at startup
            // and installs it process-wide via
            // `peko_channel::set_global_channel_port`. We re-resolve
            // that port here so re-registering `ChannelRead` /
            // `ChannelSend` against the global core keeps the real
            // adapter; passing a `NoopChannelPort` would clobber the
            // daemon-registered tool instances
            // (`BuiltinToolAdapter::register_tool` unconditionally
            // overwrites the name-keyed instance side-table) and
            // leave the tools inert in production. The Noop remains
            // as the test / standalone fallback when no daemon has
            // installed a port.
            let channel_port = resolve_channel_port();
            if let Err(e) = install_principal_tool_bag(
                Arc::clone(&core),
                &agents_dir,
                &skills_dir,
                &mcp_dir,
                &tools_dir,
                &self.principal_id,
                channel_port,
            )
            .await
            {
                tracing::warn!(
                    "failed to install principal-scoped tools on the global core: {e}. \
                     Falling back to built-in tools only."
                );
            }
        }
        Arc::clone(&core)
    }

    /// Get the principal's resolved root agent prompt.
    pub fn root_prompt(&self) -> Option<Arc<crate::principal::agent_prompt::AgentPrompt>> {
        self.root_prompt.get().cloned()
    }

    /// Install the resolved root agent prompt. Called by
    /// `RootRouter` once at construction; the prompt is reused
    /// for the principal's lifetime.
    pub fn set_root_prompt(
        &self,
        prompt: crate::principal::agent_prompt::AgentPrompt,
    ) -> Arc<crate::principal::agent_prompt::AgentPrompt> {
        self.root_prompt.get_or_init(|| Arc::new(prompt)).clone()
    }

    /// Convenience for the principal's workspace path as `&Path`.
    pub fn workspace(&self) -> &Path {
        &self.workspace_path
    }

    /// Per-principal plan DAG port (PR #2 wiring).
    ///
    /// Threaded into `Agent::with_principal_plan_port` by
    /// `agent_runner.rs` so the seven `Plan*` tools are registered
    /// on the principal's agents. Read-only clone — callers that need
    /// to hold the handle past the context borrow should `.clone()`
    /// the `Arc`.
    #[must_use]
    pub fn plan_port(&self) -> &Arc<dyn peko_plan::PlanPort> {
        &self.plan_port
    }
}

/// Resolve the `ChannelPort` for the principal tool-bag install.
///
/// Prefers the daemon-installed process-global port (the real
/// file-backed adapter, registered by `daemon/state.rs` at startup);
/// falls back to [`peko_channel::NoopChannelPort`] in tests and
/// standalone contexts where no daemon has installed one. Kept as a
/// tiny free function so the fallback behavior is unit-testable.
fn resolve_channel_port() -> Arc<dyn peko_channel::ChannelPort> {
    peko_channel::global_channel_port()
        .unwrap_or_else(|| Arc::new(peko_channel::NoopChannelPort))
}

/// Wire the principal's tool bag onto the daemon-global `ExtensionCore`.
///
/// Built-ins (Read, Bash, glob, grep, Cron*, Task*, Async*, …) and
/// the principal's discovered `<workspace>/agents/` entries are
/// registered. The `agent_catalog` tool is *not* installed here — it
/// is the only per-call tool and the runner installs it via
/// [`install_agent_catalog`] on each message.
/// Phase A: caller passes the typed `SharedLayout::agents_dir`
/// directly so the hand-rolled `workspace_path.join("agents")`
/// join inside this function is gone.
///
/// Phase 2 PR 2 (ADR-047 §2.3) also takes the typed `mcp_dir` and
/// scans `<workspace>/mcp/<id>/server.json` (or `manifest.yaml`) for
/// MCP servers. Each server is registered with the global
/// `McpManager`; the manager's running tools are wrapped as
/// `McpToolProxy` / `InjectableMcpToolProxy` and registered on the
/// global core under the principal's scope.
///
/// `channel_port` is the `ChannelPort` used for the `ChannelRead` /
/// `ChannelSend` tool registrations. Production callers pass
/// [`resolve_channel_port`]'s result (the daemon-installed real
/// port); `Arc::new(NoopChannelPort)` is fine for tests that don't
/// have a real adapter wired up — `ChannelRead` will surface
/// `Adapter` errors instead of silently zero-returning.
#[allow(clippy::too_many_arguments)]
async fn install_principal_tool_bag(
    core: Arc<ExtensionCore>,
    agents_dir: &Path,
    skills_dir: &Path,
    mcp_dir: &Path,
    tools_dir: &Path,
    principal_id: &peko_subject::PrincipalId,
    channel_port: Arc<dyn peko_channel::ChannelPort>,
) -> anyhow::Result<()> {
    // Built-in tools.
    let path_resolver = crate::common::paths::PathResolver::new();
    if let Err(e) =
        crate::engine::tool_runtime::ToolRuntime::register_builtins(
            &core,
            &path_resolver,
            channel_port,
        )
        .await
    {
        tracing::warn!("ToolRuntime::register_builtins failed during core build: {e}");
    }

    // Register the singleton `Skill` tool once on the global core.
    // Per-principal enablement and workspace state are resolved at handle
    // time from the `ToolContext` carried with each invocation. Scoped to
    // this principal_id so concurrent principals each see their own Skill.
    //
    // Phase 2 PR 1 (ADR-047 §2.4): the runtime reads directly from
    // the principal's `<workspace>/skills/` directory — no catalog,
    // no adapter. The principal is responsible for installation; the
    // runtime's only job is to point the SkillTool at SKILL.md files.
    if let Err(e) = BuiltinToolAdapter::register_tool(
        core.as_ref(),
        Arc::new(SkillTool::new(std::sync::Arc::new(
            crate::extensions::skill::WorkspaceSkillRuntime::new(skills_dir.to_path_buf()),
        ))),
        principal_id,
    )
    .await
    {
        tracing::warn!("SkillTool registration failed during core build: {e}");
    }

    // Phase A: caller passes the typed `agents_dir` directly.
    if agents_dir.exists() {
        let adapter = AgentAdapter::new();
        let discovered = adapter.discover_agents(agents_dir);
        if let Err(e) = register_agents_with_core(&core, discovered).await {
            tracing::warn!("register_agents_with_core failed during core build: {e}");
        }
    }

    // Phase 2 PR 2 (ADR-047 §2.3): MCP servers are workspace-resident.
    // The scanner walks `<workspace>/mcp/<id>/server.json` and
    // registers each with the global McpManager. After that, every
    // running MCP tool becomes a `McpToolProxy` /
    // `InjectableMcpToolProxy` tool on the principal's core so the
    // agent sees them in its tool catalog.
    //
    // Servers themselves stay lazy — `McpToolProxy::call_with_auto_start`
    // starts the process on first tool call (matching the framework's
    // prior "do NOT auto-start on AgentInit" behaviour, which existed
    // because the framework hook fires for every agent and starting
    // there races `peko ext start`).
    if let Some(mcp_manager) = crate::extensions::mcp::global_mcp_manager() {
        if mcp_dir.exists() {
            match crate::extensions::mcp::load_workspace_mcp_servers(
                mcp_dir,
                &mcp_manager,
            )
            .await
            {
                Ok(loaded) => {
                    if loaded > 0 {
                        tracing::info!(
                            "registered {loaded} MCP server(s) from {}",
                            mcp_dir.display()
                        );
                    }
                }
                Err(e) => tracing::warn!(
                    "MCP workspace scan failed for {}: {e}",
                    mcp_dir.display()
                ),
            }
        }

        // Wrap each running MCP tool as a McpToolProxy / InjectableMcpToolProxy
        // and register it on the principal's core. The `Injectable…` variant
        // is used when the server has `reserved_parameters` configured so
        // the LLM never sees (and the runtime always injects) vault-backed
        // secret params.
        let manager_arc = mcp_manager.clone();
        let mgr = manager_arc.read().await;
        let proxy_tools = mgr.get_tools().await;
        drop(mgr);
        for tool in proxy_tools {
            if let Err(e) = BuiltinToolAdapter::register_tool(
                core.as_ref(),
                tool,
                principal_id,
            )
            .await
            {
                tracing::warn!("MCP tool registration failed during core build: {e}");
            }
        }
    } else if mcp_dir.exists() {
        tracing::warn!(
            "MCP workspace dir {} exists but no global McpManager is \
             installed; MCP tools will not be registered for this principal",
            mcp_dir.display()
        );
    }

    // Phase 2 PR 3 (ADR-047 §2.1, §2.4): universal tools live
    // under `<workspace>/tools/<id>/manifest.yaml`. The
    // workspace scanner reads each manifest, finds the executable
    // sibling, constructs the canonical `peko_tools_core::Tool`
    // impl (`protocol::UniversalToolAdapter`), and registers it
    // via `BuiltinToolAdapter::register_tool` — no framework
    // hook layer.
    //
    // No auto-start: the tool's process spawns on first
    // `execute()` call via `UniversalToolAdapter::execute`,
    // matching the framework's prior behaviour where the
    // `ToolExecute` hook fired lazily.
    if tools_dir.exists() {
        match crate::extensions::universal::load_workspace_universal_tools(
            tools_dir,
            core.as_ref(),
            principal_id,
        )
        .await
        {
            Ok(loaded) => {
                if loaded > 0 {
                    tracing::info!(
                        "registered {loaded} universal tool(s) from {}",
                        tools_dir.display()
                    );
                }
            }
            Err(e) => tracing::warn!(
                "Universal tools workspace scan failed for {}: {e}",
                tools_dir.display()
            ),
        }
    }

    // Cross-peer session introspection is handled by the per-agent `session`
    // tool, which now accepts `peer` and `agent_id` filters (see
    // `SessionRegistry::list_sessions`). Persistent principal memory is
    // delegated to the filesystem — the LLM uses `Read` / `Write` for
    // memory and the `RootRouter` / `PrincipalManager` paths persist
    // session artifacts internally via `PrincipalMemory::record_session`.

    // Mark the core as having run the universal-extension pass so
    // the lazy guard in `PrincipalContext::core` does not re-install
    // on every call.
    core.mark_universal_extensions_loaded();

    Ok(())
}

/// Install the per-call `agent_catalog` tool on the principal's core.
///
/// The catalog is the *only* per-call tool — its contents are the
/// currently-available `AgentPromptSummary` list, which can change
/// between messages if the principal's `capabilities` was
/// edited. Everything else on the core is stable. Scoped to the
/// owning principal_id so the catalog lives under each principal's
/// row in the registry and re-registration on each call idempotently
/// replaces the prior entry.
pub(crate) async fn install_agent_catalog(
    core: &ExtensionCore,
    available_agents: Vec<AgentPromptSummary>,
    principal_id: &peko_subject::PrincipalId,
) -> anyhow::Result<()> {
    BuiltinToolAdapter::register_tool(
        core,
        Arc::new(AgentCatalogTool::new(available_agents)),
        principal_id,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::principal::memory::DefaultPrincipalMemory;
    use crate::principal::seen_models::seen_models_path;
    use peko_extension_api::Capabilities;
    use peko_subject::PrincipalId;
    use serial_test::serial;
    use std::sync::Arc;

    /// Build a dummy plan port for the test fixtures below. The tests
    /// don't exercise the port — they only need *some*
    /// `Arc<dyn PlanPort>` to satisfy the `PrincipalContext::new`
    /// signature. `PlanStorage::new` against a tempdir is sufficient.
    fn test_plan_port(dir: &std::path::Path) -> Arc<dyn peko_plan::PlanPort> {
        let plans_dir = dir.join("plans");
        Arc::new(peko_plan::PlanStorage::new(plans_dir))
    }

    /// `core()` returns the daemon-global `ExtensionCore`. After the
    /// Phase-2 redo there is no per-principal core; the global core
    /// is shared across principals and the principal's tool bag is
    /// installed on first call via `install_principal_tool_bag`.
    ///
    /// The assertion is intentionally relaxed: the global core may
    /// have been pre-populated by a previous test that ran in the
    /// same process. Both outcomes prove the singleton semantics —
    /// `core()` returns *whatever* the daemon-global is, never a
    /// fresh per-call instance.
    #[tokio::test]
    #[serial]
    async fn core_returns_global_singleton() {
        let dir = tempfile::tempdir().unwrap();
        let memory: Arc<dyn PrincipalMemory> =
            Arc::new(DefaultPrincipalMemory::new(dir.path().to_path_buf()));

        // Initialise the global core for this test, then read it back
        // through `ctx.core()` and confirm pointer identity. If another
        // test in this binary already populated the global (the only
        // valid state in our process-shared design), accept that too —
        // the singleton semantics are still proven because `core()`
        // returns the daemon-global, not a fresh instance.
        let our_core = Arc::new(crate::extensions::framework::core::ExtensionCore::new());
        crate::extensions::framework::core::init_global_core(Arc::clone(&our_core));

        let ctx = PrincipalContext::new(
            dir.path().to_path_buf(),
            memory,
            Arc::new(InboxRegistry::new(
                crate::extensions::framework::async_exec::executor::executor::default_inbox_factory(
                ),
            )),
            Arc::new(tokio::sync::Mutex::new(())),
            Arc::new(Capabilities::default()),
            None,
            None,
            None,
            PrincipalId::generate(),
            test_plan_port(dir.path()),
        );

        let returned = ctx.core().await;
        let global = crate::extensions::framework::core::global_core()
            .expect("we just initialized the global");
        assert!(
            Arc::ptr_eq(&returned, &global),
            "ctx.core() must return the same Arc as global_core()"
        );
    }

    /// `resolve_channel_port` prefers the daemon-installed global port
    /// and falls back to `NoopChannelPort` when no daemon has run in
    /// this process (2026-08-18 Noop-clobber fix). Both branches are
    /// asserted because lib-test siblings that build an `AppState`
    /// install the global port process-wide, so which branch is live
    /// depends on test execution order — either proves the helper
    /// never invents a second real port.
    #[tokio::test]
    async fn resolve_channel_port_prefers_global_or_falls_back_to_noop() {
        match peko_channel::global_channel_port() {
            Some(global) => {
                let resolved = resolve_channel_port();
                assert!(
                    Arc::ptr_eq(&resolved, &global),
                    "with a global port installed, resolve_channel_port must return it"
                );
            }
            None => {
                let resolved = resolve_channel_port();
                let err = resolved
                    .peek(
                        &peko_channel::ChannelId::generate(),
                        &peko_channel::Checkpoint::default(),
                    )
                    .await
                    .expect_err("NoopChannelPort::peek always errors");
                assert!(
                    matches!(err, peko_channel::ChannelError::Adapter(ref msg) if msg.contains("NoopChannelPort")),
                    "fallback must be the NoopChannelPort, got: {err}"
                );
            }
        }
    }

    /// `set_root_prompt` is idempotent — once a principal's root
    /// prompt is installed, subsequent calls (which the runner
    /// shouldn't make, but might via test setup) are no-ops.
    #[test]
    fn root_prompt_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let memory: Arc<dyn PrincipalMemory> =
            Arc::new(DefaultPrincipalMemory::new(dir.path().to_path_buf()));

        let ctx = PrincipalContext::new(
            dir.path().to_path_buf(),
            memory,
            Arc::new(InboxRegistry::new(
                crate::extensions::framework::async_exec::executor::executor::default_inbox_factory(
                ),
            )),
            Arc::new(tokio::sync::Mutex::new(())),
            Arc::new(Capabilities::default()),
            None,
            None,
            None,
            PrincipalId::generate(),
            test_plan_port(dir.path()),
        );

        // `set_root_prompt` requires an `AgentPrompt`; constructing one
        // with a minimal body is enough for the idempotency check.
        use crate::principal::agent_prompt::AgentPrompt;
        let prompt = AgentPrompt {
            name: "root".to_string(),
            path: PathBuf::from("builtin:root"),
            body: "test body".to_string(),
            frontmatter: Default::default(),
        };
        let first = ctx.set_root_prompt(prompt.clone());
        let second = ctx.set_root_prompt(prompt);
        assert!(Arc::ptr_eq(&first, &second));
    }

    /// `principal_id()` returns the value passed at construction
    /// unchanged.
    #[test]
    fn principal_id_is_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let memory: Arc<dyn PrincipalMemory> =
            Arc::new(DefaultPrincipalMemory::new(dir.path().to_path_buf()));

        let id = PrincipalId::generate();
        let ctx = PrincipalContext::new(
            dir.path().to_path_buf(),
            memory,
            Arc::new(InboxRegistry::new(
                crate::extensions::framework::async_exec::executor::executor::default_inbox_factory(
                ),
            )),
            Arc::new(tokio::sync::Mutex::new(())),
            Arc::new(Capabilities::default()),
            None,
            None,
            None,
            id.clone(),
            test_plan_port(dir.path()),
        );
        assert_eq!(ctx.principal_id(), &id);
    }

    /// `plan_port()` returns the handle passed at construction unchanged.
    #[test]
    fn plan_port_is_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let memory: Arc<dyn PrincipalMemory> =
            Arc::new(DefaultPrincipalMemory::new(dir.path().to_path_buf()));

        let port = test_plan_port(dir.path());
        let ctx = PrincipalContext::new(
            dir.path().to_path_buf(),
            memory,
            Arc::new(InboxRegistry::new(
                crate::extensions::framework::async_exec::executor::executor::default_inbox_factory(
                ),
            )),
            Arc::new(tokio::sync::Mutex::new(())),
            Arc::new(Capabilities::default()),
            None,
            None,
            None,
            PrincipalId::generate(),
            port.clone(),
        );
        assert!(Arc::ptr_eq(ctx.plan_port(), &port));
    }

    /// Phase 4: `mark_model_seen` returns `true` on the first call
    /// for a model, `false` on subsequent calls. Persistence is
    /// verified end-to-end: a second `PrincipalContext` built
    /// against the same workspace sees the prior record.
    #[test]
    fn mark_model_seen_returns_true_once_then_false() {
        let dir = tempfile::tempdir().unwrap();
        let memory: Arc<dyn PrincipalMemory> =
            Arc::new(DefaultPrincipalMemory::new(dir.path().to_path_buf()));

        let ctx = PrincipalContext::new(
            dir.path().to_path_buf(),
            memory.clone(),
            Arc::new(InboxRegistry::new(
                crate::extensions::framework::async_exec::executor::executor::default_inbox_factory(
                ),
            )),
            Arc::new(tokio::sync::Mutex::new(())),
            Arc::new(Capabilities::default()),
            None,
            None,
            None,
            PrincipalId::generate(),
            test_plan_port(dir.path()),
        );

        // First call for `claude-sonnet-4-6`: fresh → `true`.
        assert!(ctx.mark_model_seen("claude-sonnet-4-6"));
        // Second call for the same model: already present → `false`.
        assert!(!ctx.mark_model_seen("claude-sonnet-4-6"));
        // A different model: fresh → `true`.
        assert!(ctx.mark_model_seen("claude-haiku-4-5"));

        // The persisted file should be loadable from disk.
        let path = seen_models_path(dir.path());
        let on_disk = SeenModels::load(&path).unwrap();
        assert_eq!(on_disk.models.len(), 2);
        assert!(on_disk.contains("claude-sonnet-4-6"));
        assert!(on_disk.contains("claude-haiku-4-5"));
    }

    /// Phase 4: a fresh `PrincipalContext` constructed against a
    /// workspace that already has `seen_models.json` should
    /// hydrate its in-memory set from disk. The first call to
    /// `mark_model_seen` on a recorded model id must return `false`.
    #[test]
    fn principal_context_hydrates_seen_models_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        // Seed the file by writing through `SeenModels::save`.
        let path = seen_models_path(dir.path());
        let mut prior = SeenModels::empty();
        prior
            .add_then_save("claude-sonnet-4-6", &path)
            .unwrap();
        assert!(path.exists());

        // Build a context pointing at the seeded workspace.
        let memory: Arc<dyn PrincipalMemory> =
            Arc::new(DefaultPrincipalMemory::new(dir.path().to_path_buf()));
        let ctx = PrincipalContext::new(
            dir.path().to_path_buf(),
            memory,
            Arc::new(InboxRegistry::new(
                crate::extensions::framework::async_exec::executor::executor::default_inbox_factory(
                ),
            )),
            Arc::new(tokio::sync::Mutex::new(())),
            Arc::new(Capabilities::default()),
            None,
            None,
            None,
            PrincipalId::generate(),
            test_plan_port(dir.path()),
        );

        // The pre-existing model should be reported as seen
        // (i.e. `mark_model_seen` returns `false`).
        assert!(!ctx.mark_model_seen("claude-sonnet-4-6"));
        // `has_model_seen` should agree.
        assert!(ctx.has_model_seen("claude-sonnet-4-6"));
        // A new model still gets the `true` first-use treatment.
        assert!(ctx.mark_model_seen("claude-haiku-4-5"));
    }

    /// Phase 4: corrupt `seen_models.json` is tolerated — the
    /// context boots with an empty set, and the next successful
    /// `mark_model_seen` rewrites the file cleanly.
    #[test]
    fn principal_context_tolerates_corrupt_seen_models() {
        let dir = tempfile::tempdir().unwrap();
        let path = seen_models_path(dir.path());
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(&path, "not valid json").unwrap();

        let memory: Arc<dyn PrincipalMemory> =
            Arc::new(DefaultPrincipalMemory::new(dir.path().to_path_buf()));
        let ctx = PrincipalContext::new(
            dir.path().to_path_buf(),
            memory,
            Arc::new(InboxRegistry::new(
                crate::extensions::framework::async_exec::executor::executor::default_inbox_factory(
                ),
            )),
            Arc::new(tokio::sync::Mutex::new(())),
            Arc::new(Capabilities::default()),
            None,
            None,
            None,
            PrincipalId::generate(),
            test_plan_port(dir.path()),
        );

        // The set is empty, so this is a first-use → `true`.
        assert!(ctx.mark_model_seen("claude-sonnet-4-6"));
        // The file is now valid JSON containing the model.
        let reloaded = SeenModels::load(&path).unwrap();
        assert!(reloaded.contains("claude-sonnet-4-6"));
    }
}
