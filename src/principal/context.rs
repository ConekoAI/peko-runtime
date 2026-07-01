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
use crate::extensions::framework::core::ExtensionCore;
use crate::principal::memory::PrincipalMemory;
use crate::principal::router::AgentPromptSummary;
use crate::providers::LlmResolver;
use crate::session::InboxRegistry;
use crate::tools::builtin::{AgentCatalogTool, PrincipalMemoryTool, PrincipalSessionsTool};

use super::config::PrincipalCapabilities;

/// Per-principal runtime state shared by the root agent and its
/// subagents.
///
/// Constructed once per principal at startup, cached on the
/// `SupervisorRouter`, and passed by reference into the principal's
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

    /// Per-principal `ExtensionCore` shared by every agent of this
    /// principal. Built lazily on first [`Self::core`] call and reused
    /// for the lifetime of the principal.
    ///
    /// `tokio::sync::OnceCell` (not `std::sync::OnceLock`) so the build
    /// can `await` the tool-registration futures without the
    /// `block_in_place`-on-current-thread-runtime trap that broke
    /// single-threaded `#[tokio::test]` callers.
    core: tokio::sync::OnceCell<Arc<ExtensionCore>>,
}

impl PrincipalContext {
    /// Build a `PrincipalContext` from already-resolved principal
    /// state. The core is *not* built until [`Self::core`] is called.
    pub fn new(
        workspace_path: PathBuf,
        memory: Arc<dyn PrincipalMemory>,
        inbox_registry: Arc<InboxRegistry>,
        session_creation_lock: Arc<tokio::sync::Mutex<()>>,
        capabilities: Arc<PrincipalCapabilities>,
        resolver: Option<Arc<LlmResolver>>,
        provider_hint: (Option<String>, Option<String>),
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
            core: tokio::sync::OnceCell::new(),
        }
    }

    /// Get the principal's `ExtensionCore`, building it on first call.
    ///
    /// The core is shared by every agent of the principal. It carries:
    ///   - the built-in tools registered by `ToolRuntime::register_builtins`
    ///   - the principal's discovered `<workspace>/agents/` as `{{agents}}` hooks
    ///   - the principal-scoped `principal_sessions` and `principal_memory` tools
    ///
    /// Visibility to any single agent is still governed by the agent's
    /// own capability whitelist; this method does not assume privilege.
    ///
    /// Concurrent first-callers race to build the core; `OnceCell`
    /// serialises them so exactly one build happens and the rest
    /// observe the result. If the build fails (e.g. tool registration
    /// errors), the error is logged and a bare core is returned so the
    /// principal can still run with the built-in tool set.
    pub async fn core(&self) -> Arc<ExtensionCore> {
        let core = self
            .core
            .get_or_init(|| async {
                let core = Arc::new(ExtensionCore::new());
                if let Err(e) = build_principal_core(
                    Arc::clone(&core),
                    &self.workspace_path,
                    Arc::clone(&self.memory),
                )
                .await
                {
                    tracing::warn!(
                        "failed to install principal-scoped tools on the core: {e}. \
                         Falling back to built-in tools only."
                    );
                }
                core
            })
            .await;
        Arc::clone(core)
    }

    /// Get the principal's resolved root agent prompt.
    pub fn root_prompt(&self) -> Option<Arc<crate::principal::agent_prompt::AgentPrompt>> {
        self.root_prompt.get().cloned()
    }

    /// Install the resolved root agent prompt. Called by
    /// `SupervisorRouter` once at construction; the prompt is reused
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

/// Wire the principal's tool bag onto a freshly-allocated core.
///
/// Built-ins (Read, Bash, glob, grep, Cron*, Task*, Async*, …) and
/// the principal's discovered `<workspace>/agents/` entries are
/// registered. The `agent_catalog` tool is *not* installed here — it
/// is the only per-call tool and the runner installs it via
/// [`install_agent_catalog`] on each message.
async fn build_principal_core(
    core: Arc<ExtensionCore>,
    workspace_path: &Path,
    memory: Arc<dyn PrincipalMemory>,
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

    // Principal-scoped tools: sessions and memory.
    BuiltinToolAdapter::register_tool(
        &core,
        Arc::new(PrincipalSessionsTool::new(Arc::clone(&memory))),
    )
    .await?;
    BuiltinToolAdapter::register_tool(
        &core,
        Arc::new(PrincipalMemoryTool::new(Arc::clone(&memory))),
    )
    .await?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::principal::config::PrincipalCapabilities;
    use crate::principal::memory::DefaultPrincipalMemory;
    use std::sync::Arc;

    /// `core()` returns the same `Arc` on every call: the
    /// per-principal `ExtensionCore` is built once and reused for
    /// the principal's lifetime. This is the Phase-3 perf contract —
    /// no per-message `ExtensionCore::new()` churn.
    #[tokio::test]
    async fn core_is_cached_across_calls() {
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
        );

        let a = ctx.core().await;
        let b = ctx.core().await;
        assert!(Arc::ptr_eq(&a, &b), "core() must return the same Arc on every call");
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
        );

        // `set_root_prompt` requires an `AgentPrompt`; constructing one
        // with a minimal body is enough for the idempotency check.
        use crate::principal::agent_prompt::AgentPrompt;
        let prompt = AgentPrompt {
            name: "supervisor".to_string(),
            path: PathBuf::from("builtin:supervisor"),
            body: "test body".to_string(),
            frontmatter: Default::default(),
        };
        let first = ctx.set_root_prompt(prompt.clone());
        let second = ctx.set_root_prompt(prompt);
        assert!(Arc::ptr_eq(&first, &second));
    }
}
