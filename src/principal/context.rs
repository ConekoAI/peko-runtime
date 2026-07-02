//! Per-principal runtime context.
//!
//! [`PrincipalContext`] bundles the state that all agents of a single
//! principal (the root agent and any subagents it spawns via the `Agent`
//! tool) need to operate:
//!
//! - the principal's own memory, inbox, and session-creation lock
//! - the principal's workspace path and provider resolver
//! - the principal's capability set (tools/skills/mcps/agents enabled)
//! - the principal's resolved (provider, model) preference
//!
//! It also owns a lazily-built, **per-principal** [`ExtensionCore`]
//! shared by every agent of that principal. The core is *not* privileged
//! over subagent cores — the root agent and every subagent resolve the
//! exact same core through this struct. Per-agent visibility is enforced
//! by each agent's own capability whitelist; the core just hosts the
//! tool *instances*.
//!
//! This is the post-Phase-1 realisation of the design rule "the root
//! agent is but another agent of the principal, simply user-facing".

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use crate::extensions::agent::{register_agents_with_core, AgentAdapter};
use crate::extensions::builtin::BuiltinToolAdapter;
use crate::extensions::framework::core::{global_core, ExtensionCore};
use crate::principal::memory::PrincipalMemory;
use crate::principal::router::AgentPromptSummary;
use crate::principal::PrincipalId;
use crate::providers::LlmResolver;
use crate::session::InboxRegistry;
use crate::tools::builtin::{AgentCatalogTool, SkillTool};

use super::config::PrincipalCapabilities;

/// Per-principal runtime state shared by the root agent and its
/// subagents.
///
/// Constructed once per principal at startup, cached on the
/// `RootRouter`, and passed by reference into the principal's
/// root-agent runner. Subagents don't need a fresh context — they read
/// the principal's tools off this struct's core, and their own
/// capability whitelist filters what's actually visible to them.
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
    /// Shared inbox the dispatcher pushes steering messages into.
    pub inbox_registry: Arc<InboxRegistry>,
    /// Held during root-agent session creation so concurrent peers
    /// don't race on shared session metadata.
    pub session_creation_lock: Arc<tokio::sync::Mutex<()>>,
    /// Principal's capability set — what tools/skills/mcps/agents are
    /// enabled for this principal.
    pub capabilities: Arc<PrincipalCapabilities>,
    /// LLM resolver used to validate provider hints and surface
    /// catalog defaults.
    pub resolver: Option<Arc<LlmResolver>>,
    /// Per-principal provider/model preference from `principal.toml`.
    /// When `Some`, overrides the catalog default for this principal.
    pub provider_hint: (Option<String>, Option<String>),

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
    /// registering `principal_send` — the tool needs a stable caller
    /// identity to attribute outbound requests under
    /// `Subject::Principal(caller_principal_did)`.
    caller_principal_did: OnceLock<String>,
    caller_runtime_id: OnceLock<String>,
}

impl PrincipalContext {
    /// Build a `PrincipalContext` from already-resolved principal
    /// state.
    pub fn new(
        workspace_path: PathBuf,
        memory: Arc<dyn PrincipalMemory>,
        inbox_registry: Arc<InboxRegistry>,
        session_creation_lock: Arc<tokio::sync::Mutex<()>>,
        capabilities: Arc<PrincipalCapabilities>,
        resolver: Option<Arc<LlmResolver>>,
        provider_hint: (Option<String>, Option<String>),
        principal_id: PrincipalId,
    ) -> Self {
        let sessions_dir = memory.sessions_dir().to_path_buf();
        Self {
            workspace_path,
            sessions_dir,
            memory,
            inbox_registry,
            session_creation_lock,
            capabilities,
            resolver,
            provider_hint,
            root_prompt: OnceLock::new(),
            principal_id,
            caller_principal_did: OnceLock::new(),
            caller_runtime_id: OnceLock::new(),
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
    /// own capability whitelist; this method does not assume
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
            if let Err(e) = install_principal_tool_bag(
                Arc::clone(&core),
                &self.workspace_path,
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
}

/// Wire the principal's tool bag onto the daemon-global `ExtensionCore`.
///
/// Built-ins (Read, Bash, glob, grep, Cron*, Task*, Async*, …) and
/// the principal's discovered `<workspace>/agents/` entries are
/// registered. The `agent_catalog` tool is *not* installed here — it
/// is the only per-call tool and the runner installs it via
/// [`install_agent_catalog`] on each message.
async fn install_principal_tool_bag(
    core: Arc<ExtensionCore>,
    workspace_path: &Path,
) -> anyhow::Result<()> {
    // Built-in tools.
    let path_resolver = crate::common::paths::PathResolver::new();
    if let Err(e) = crate::engine::tool_runtime::ToolRuntime::register_builtins(
        &core,
        &path_resolver,
    )
    .await
    {
        tracing::warn!("ToolRuntime::register_builtins failed during core build: {e}");
    }

    // Discover and register the principal's `<workspace>/agents/`.
    let agents_dir = workspace_path.join("agents");
    if agents_dir.exists() {
        let adapter = AgentAdapter::new();
        let discovered = adapter.discover_agents(&agents_dir);
        if let Err(e) = register_agents_with_core(&core, discovered).await {
            tracing::warn!("register_agents_with_core failed during core build: {e}");
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
/// between messages if the principal's `capabilities.agents` was
/// edited. Everything else on the core is stable.
pub(crate) async fn install_agent_catalog(
    core: &ExtensionCore,
    available_agents: Vec<AgentPromptSummary>,
) -> anyhow::Result<()> {
    BuiltinToolAdapter::register_tool(
        core,
        Arc::new(AgentCatalogTool::new(available_agents)),
    )
    .await
}

/// Install the per-call `Skill` tool on the principal's core.
///
/// Mirrors [`install_agent_catalog`] — the principal's enabled-skill
/// allowlist can change between messages (if `principal.toml` is
/// edited), so the tool is re-registered each message with the
/// current list. The skill bodies themselves are loaded on demand
/// from the daemon-global `skills_dir()` when the LLM invokes the
/// tool. `workspace_dir` is the principal's workspace root, used as
/// the cwd for any `` !`cmd` `` / `` ```! `` blocks the body contains.
pub(crate) async fn install_skill_tool(
    core: &ExtensionCore,
    skills_dir: PathBuf,
    enabled_skills: Vec<String>,
    workspace_dir: PathBuf,
) -> anyhow::Result<()> {
    BuiltinToolAdapter::register_tool(
        core,
        Arc::new(SkillTool::new(skills_dir, enabled_skills, workspace_dir)),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::principal::config::PrincipalCapabilities;
    use crate::principal::memory::DefaultPrincipalMemory;
    use crate::principal::PrincipalId;
    use std::sync::Arc;

    /// `core()` returns the daemon-global `ExtensionCore`. After the
    /// Phase-2 redo there is no per-principal core; the global core
    /// is shared across principals and the principal's tool bag is
    /// installed on first call via `install_principal_tool_bag`.
    #[tokio::test]
    async fn core_returns_global_singleton() {
        let dir = tempfile::tempdir().unwrap();
        let memory: Arc<dyn PrincipalMemory> = Arc::new(DefaultPrincipalMemory::new(
            dir.path().to_path_buf(),
        ));

        // Initialise the global core for this test.
        let core = Arc::new(crate::extensions::framework::ExtensionCore::new());
        crate::extensions::framework::core::init_global_core(Arc::clone(&core));

        let ctx = PrincipalContext::new(
            dir.path().to_path_buf(),
            memory,
            Arc::new(InboxRegistry::new()),
            Arc::new(tokio::sync::Mutex::new(())),
            Arc::new(PrincipalCapabilities::default()),
            None,
            (None, None),
            PrincipalId::generate(),
        );

        let a = ctx.core().await;
        assert!(Arc::ptr_eq(&a, &core));
    }

    /// `set_root_prompt` is idempotent — once a principal's root
    /// prompt is installed, subsequent calls (which the runner
    /// shouldn't make, but might via test setup) are no-ops.
    #[test]
    fn root_prompt_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let memory: Arc<dyn PrincipalMemory> = Arc::new(DefaultPrincipalMemory::new(
            dir.path().to_path_buf(),
        ));

        let ctx = PrincipalContext::new(
            dir.path().to_path_buf(),
            memory,
            Arc::new(InboxRegistry::new()),
            Arc::new(tokio::sync::Mutex::new(())),
            Arc::new(PrincipalCapabilities::default()),
            None,
            (None, None),
            PrincipalId::generate(),
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
        let memory: Arc<dyn PrincipalMemory> = Arc::new(DefaultPrincipalMemory::new(
            dir.path().to_path_buf(),
        ));

        let id = PrincipalId::generate();
        let ctx = PrincipalContext::new(
            dir.path().to_path_buf(),
            memory,
            Arc::new(InboxRegistry::new()),
            Arc::new(tokio::sync::Mutex::new(())),
            Arc::new(PrincipalCapabilities::default()),
            None,
            (None, None),
            id.clone(),
        );
        assert_eq!(ctx.principal_id(), &id);
    }
}
