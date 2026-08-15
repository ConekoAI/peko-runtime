//! Production [`SubagentRuntime`](crate::tools::builtin::messaging::SubagentRuntime) adapter.
//!
//! Bridges `crate::tools::builtin::messaging::AgentTool` to the root-side
//! [`SubagentExecutor`]. The tool itself lives in the built-in crate
//! (Phase 10e); the heavy executor stays in root because it is
//! deeply entangled with the async-framework, quota metering,
//! per-principal capability snapshots, and the `SubagentRunView`
//! projection.
//!
//! Per the Phase 10 plan rule ("Built-ins must not import daemon
//! state"), the built-in crate does NOT know about `SubagentExecutor`
//! directly. It speaks to the four-method [`SubagentRuntime`] port,
//! and this adapter wires each call to the right executor entry
//! point:
//!
//! | Port method                                | Executor entry point                                                                                              |
//! |--------------------------------------------|-------------------------------------------------------------------------------------------------------------------|
//! | [`is_subagent_enabled`]                    | `principal_capabilities` snapshot → `Capability::is_granted("agent:<name>")`; fail-closed when no snapshot is registered |
//! | [`resolve_agent_config`]                   | workspace `<ws>/agents/<n>/AGENT.md` (dir) or `<ws>/agents/<n>.md` (flat), then global `agents/<n>/config.toml`    |
//! | [`audit_spawn`]                            | `observability.audit("SubagentSpawn", ...)` — no-op when no hub is attached                                        |
//! | [`execute_and_wait`]                       | `SubagentExecutor::execute_and_wait` — returns the projected `SubagentRunView`                                     |
//! | [`request_compaction`]                     | `SubagentExecutor::request_compaction` — flags the target for engine-driven compaction, returns immediately        |
//!
//! Principal-id and principal-name accessors are pulled directly from
//! the executor's stable `principal_id`/`principal_name` getters.
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;

use crate::agents::agent_config::AgentConfig;
use crate::agents::subagent_executor::SubagentExecutor;
use crate::common::identifiers::parse_agent_name;
use crate::common::paths::PathResolver;
use crate::extensions::framework::subagent::SpawnCleanupPolicy;
use crate::tools::builtin::messaging::{
    AgentConfig as BuiltinAgentConfig, SpawnAuditEvent, SpawnRequest, SubagentRunView,
    SubagentRuntime,
};
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Root-side adapter that implements the built-in
/// [`SubagentRuntime`](crate::tools::builtin::messaging::SubagentRuntime)
/// port on top of [`SubagentExecutor`].
///
/// Constructed once per agent (or per principal — the executor is
/// already per-principal) and stored as `Arc<SubagentExecutorRuntime>`
/// in `SharedSubagentRuntime` for the `AgentTool` to consume.
pub struct SubagentExecutorRuntime {
    executor: Arc<SubagentExecutor>,
}

impl SubagentExecutorRuntime {
    /// Build the adapter that wraps `executor`.
    #[must_use]
    pub fn new(executor: Arc<SubagentExecutor>) -> Self {
        Self { executor }
    }

    /// Expose the wrapped executor for callers that still need the
    /// legacy `Arc<SubagentExecutor>` (e.g. async-task registry
    /// consumers). Use sparingly — new code should depend on the
    /// port instead.
    #[must_use]
    pub fn executor(&self) -> &Arc<SubagentExecutor> {
        &self.executor
    }

    /// Resolve an agent prompt from the principal's agents directory.
    ///
    /// Two on-disk shapes are supported:
    /// - directory layout: `<agents_dir>/<name>/AGENT.md`
    /// - flat layout: `<agents_dir>/<name>.md`
    ///
    /// **Phase A.** Caller passes the typed
    /// `SharedLayout::agents_dir` directly rather than the
    /// workspace root + a hand-rolled `"agents"` join.
    ///
    /// Errors if neither exists.
    fn resolve_principal_agent(
        name: &str,
        agents_dir: &Path,
    ) -> anyhow::Result<BuiltinAgentConfig> {
        let dir_layout = agents_dir.join(name).join("AGENT.md");
        let flat_layout = agents_dir.join(format!("{name}.md"));

        let agent_md = if dir_layout.exists() {
            dir_layout
        } else if flat_layout.exists() {
            flat_layout
        } else {
            anyhow::bail!(
                "No agent prompt found for principal agent '{name}' at {:?} or {:?}",
                dir_layout,
                flat_layout
            );
        };

        let prompt = load_agent_prompt(&agent_md)
            .with_context(|| format!("Failed to load principal agent prompt '{name}'"))?;

        Ok(BuiltinAgentConfig {
            name: prompt.name,
            description: prompt.frontmatter.description,
            prompt: Some(prompt.body),
            ..BuiltinAgentConfig::default()
        })
    }

    /// Load an agent config from the global `{PEKO_HOME}/agents/<n>/config.toml`.
    async fn resolve_global_agent(name: &str) -> anyhow::Result<BuiltinAgentConfig> {
        let agent_name = parse_agent_name(name)?;
        let resolver = PathResolver::new();
        let config_path = resolver.agent_config(agent_name);
        if !config_path.exists() {
            anyhow::bail!("Subagent type '{name}' not found at {config_path:?}");
        }
        let content = tokio::fs::read_to_string(&config_path).await?;
        toml::from_str(&content)
            .with_context(|| format!("Failed to parse agent config for '{name}'"))
    }

    /// Bridge from root's `AgentConfig` to the built-in's
    /// `BuiltinAgentConfig`. We carry both types across the port
    /// boundary; the built-in sees the projected shape only.
    #[allow(dead_code)]
    fn project_agent_config(config: &AgentConfig) -> BuiltinAgentConfig {
        BuiltinAgentConfig {
            name: config.name.clone(),
            description: config.description.clone(),
            prompt: config.prompt.clone(),
            agent_did: config.agent_did.clone(),
            enable_task_tools: config.enable_task_tools,
            enable_async_tools: config.enable_async_tools,
            enable_tool_search: config.enable_tool_search,
            // Phase 2 of `feature/multi-model-subagents`: project
            // the `enable_model_list` flag so the builtin's view of
            // the config matches the canonical `AgentConfig`.
            enable_model_list: config.enable_model_list,
            channel: config.channel.clone(),
            thinking_level: config.thinking_level.clone(),
            sandbox_enabled: config.sandbox_enabled,
            model_aliases: config.model_aliases.clone(),
        }
    }
}

#[async_trait]
impl SubagentRuntime for SubagentExecutorRuntime {
    fn is_subagent_enabled(&self, subagent_type: &str) -> bool {
        // ADR-019/Track B: enforce the per-principal agent capability
        // before loading any on-disk config. Missing authorization context
        // is denied, matching the canonical tool-execution funnel.
        self.executor.principal_capabilities().is_some_and(|caps| {
            let required = crate::extensions::framework::types::Capability::new(format!(
                "agent:{subagent_type}"
            ));
            caps.is_granted(&required)
        })
    }

    async fn resolve_agent_config(
        &self,
        name: &str,
        workspace: Option<&Path>,
        // Phase 1: forwarded into the returned `AgentConfig` via
        // the existing `model_aliases` so the spawn-time override
        // plumbing in `AgentTool` can pick it up. The actual
        // provider override happens in `execute_subagent_task` —
        // `resolve_agent_config` only carries the requested model
        // id for the agent-side `model_aliases` resolution.
        _model_override: Option<&str>,
    ) -> anyhow::Result<BuiltinAgentConfig> {
        // Prefer a principal-scoped AGENT.md when a workspace is bound;
        // fall through to the global agents/ registry on miss.
        let config = if let Some(workspace) = workspace {
            // Phase A: derive the typed agents dir from the
            // Shared root.
            let agents_dir = workspace.join("agents");
            match Self::resolve_principal_agent(name, &agents_dir) {
                Ok(config) => config,
                Err(e) => {
                    tracing::debug!(
                        "Principal agent '{name}' not found in agents dir '{}': {e}; falling back to global agent",
                        agents_dir.display()
                    );
                    Self::resolve_global_agent(name).await?
                }
            }
        } else {
            // Standalone / test path: resolve from the global layout only.
            Self::resolve_global_agent(name).await?
        };

        Ok(config)
    }

    async fn audit_spawn(&self, event: SpawnAuditEvent) {
        let Some(obs) = self.executor.observability() else {
            return;
        };

        let cleanup_label = match event.cleanup {
            SpawnCleanupPolicy::Keep => "keep",
            SpawnCleanupPolicy::Delete => "delete",
        };

        let details = serde_json::json!({
            "subagent_type": event.subagent_type,
            "principal_id": event.principal_id,
            "principal_name": event.principal_name,
            "isolated": event.isolated,
            "cleanup": cleanup_label,
            "description": event.description,
            "parent_session_key": event.parent_session_key,
            "model_id": event.model_id,
            // Phase 3 of `feature/multi-model-subagents` — the
            // conservative spawn-time cost estimate. `None` when
            // no `cost_per_call_max` is configured (so no
            // pre-flight ran) or the model has no pricing hint
            // (local / unpriced model — cost is `0.0` by
            // convention). Surface in `peko audit tail` so
            // operators can see what the parent's spawn-time
            // estimator saw.
            "cost_estimate_usd": event.cost_estimate_usd,
        });

        if let Err(e) = obs
            .audit("SubagentSpawn", event.principal_name.as_deref(), details)
            .await
        {
            tracing::warn!("Failed to audit subagent spawn: {e}");
        }
    }

    fn spawn_cost_estimate_usd(&self) -> Option<f64> {
        // Phase 3 — same heuristic the spawn-time pre-flight
        // uses: 4K input + 1K output projected through the
        // provider's `PricingHint`. Returns `None` when the
        // principal has no `cost_per_call_max` configured or
        // the provider carries no pricing hint. Match the
        // production pre-flight math exactly so the audit row
        // and the gate agree.
        let ceiling = self
            .executor
            .quota_meter()
            .as_ref()
            .and_then(|m| m.config().cost_per_call_max)?;
        let provider = self.executor.provider_for_cost_estimate()?;
        let pricing = provider.spec().and_then(|s| s.pricing)?;
        const EST_INPUT_TOKENS: u64 = 4_000;
        const EST_OUTPUT_TOKENS: u64 = 1_000;
        let input_cost = pricing
            .input_per_million
            .map_or(0.0, |rate| rate * EST_INPUT_TOKENS as f64 / 1_000_000.0);
        let output_cost = pricing
            .output_per_million
            .map_or(0.0, |rate| rate * EST_OUTPUT_TOKENS as f64 / 1_000_000.0);
        let estimated = input_cost + output_cost;
        // Surface the estimate regardless of whether the
        // ceiling would have rejected it — the audit log shows
        // what the estimator saw, not just the gate decision.
        // `ceiling` is consulted only to decide whether to
        // populate the field at all (pre-flight runs iff
        // ceiling is `Some`).
        let _ = ceiling;
        Some(estimated)
    }

    async fn execute_and_wait(&self, request: SpawnRequest) -> anyhow::Result<SubagentRunView> {
        let timeout_seconds = request.timeout_seconds;
        let parent_session_key = request.parent_session_key.clone();
        let prompt = request.prompt.clone();
        // Phase 1: the parent-driven model id (if any). Caller puts
        // it on `ExecutionConfig.model_override`; we lift it onto the
        // root-side `ExecutionConfig` and clear it from the source so
        // the built-in shape stays single-source. When `request.model`
        // is `Some`, it wins — the parent picked it directly.
        let model_override = request
            .model
            .clone()
            .or_else(|| request.config.model_override.clone());

        // Translate the built-in's `ExecutionConfig` to the root's
        // `ExecutionConfig`. Both are structurally identical (timeout,
        // cleanup, label, max_depth, announce_completion); we project
        // field-by-field to keep the type boundary explicit.
        let root_config = crate::agents::subagent_executor::ExecutionConfig {
            timeout_seconds: request.config.timeout_seconds,
            cleanup: request.config.cleanup,
            label: request.config.label,
            announce_completion: request.config.announce_completion,
            max_depth: request.config.max_depth,
            model_override,
            // Agent tool `name` → the child session's slug (stamped by
            // `spawn_and_execute`; ignored on the resume path).
            slug: request.name.clone(),
        };

        let view = if let Some(ref resume_target) = request.resume_session {
            // Phase 5b: persistent subagents — re-attach to an
            // existing spawned session. All D4 guards (existence,
            // spawn-kind, self/ancestor, subtree, archived, active
            // run) live in `SubagentExecutor::resume_and_execute`.
            self.executor
                .resume_and_wait(
                    &prompt,
                    resume_target,
                    &parent_session_key,
                    root_config,
                    timeout_seconds,
                    request.parent_cancel,
                )
                .await?
        } else {
            // Phase 5b: an explicitly-provided `parent_session_key`
            // (context seeding) must be the caller's own session or
            // inside its subtree. The auto-detected default (caller
            // key == parent key) always passes.
            if let Some(ref caller) = request.caller_session_key {
                self.executor
                    .validate_context_parent(&parent_session_key, caller)
                    .await?;
            }
            self.executor
                .execute_and_wait(
                    &prompt,
                    None,
                    request.isolated,
                    &parent_session_key,
                    root_config,
                    timeout_seconds,
                    request.parent_cancel,
                )
                .await?
        };

        // Translate the root's `SubagentRunView` to the built-in's.
        // The struct fields are identical by design; this projection
        // keeps the port boundary explicit so changes in either side
        // surface as a compile error.
        Ok(SubagentRunView {
            run_id: view.run_id,
            child_session_key: view.child_session_key,
            parent_session_key: view.parent_session_key,
            task: view.task,
            status: view.status,
            started_at: view.started_at,
            completed_at: view.completed_at,
            cleanup: match view.cleanup {
                peko_session::types::SpawnCleanupPolicy::Keep => SpawnCleanupPolicy::Keep,
                peko_session::types::SpawnCleanupPolicy::Delete => SpawnCleanupPolicy::Delete,
            },
            label: view.label,
            result: view
                .result
                .map(|r| crate::tools::builtin::messaging::SubagentResult {
                    status: r.status,
                    output: r.output,
                    error: r.error,
                    token_usage: r.token_usage,
                    completed_at: r.completed_at,
                }),
            depth: view.depth,
            announce_completion: view.announce_completion,
        })
    }

    /// `Agent` tool `action = "compact"` — delegate to the executor's
    /// guarded flag-set. Returns immediately (no LLM call, no
    /// completion signal).
    async fn request_compaction(
        &self,
        target: &str,
        caller_session_key: &str,
    ) -> anyhow::Result<crate::tools::builtin::session::CompactRequestOutcome> {
        self.executor
            .request_compaction(target, caller_session_key)
            .await
    }

    fn principal_id(&self) -> String {
        self.executor.principal_id().0.clone()
    }
    fn principal_name(&self) -> Option<String> {
        self.executor.principal_name().map(str::to_owned)
    }
}

// ─── Agent-prompt parsing (inlined to keep `agents/` free of `principal/` import) ─
//
// The workspace boundary rule "src/agents/ must NOT import from
// src/principal/" forbids this module from calling
// `crate::principal::agent_prompt::load_agent_prompt`. The
// implementation below is a self-contained copy of that parser
// (frontmatter + body). Keep the two implementations in sync; a
// future phase will lift agent-prompt parsing into a shared module.

// A thin Markdown prompt file with an optional YAML frontmatter.
#[derive(Debug, Clone)]
#[allow(dead_code)] // path kept for upcoming F-series prompt-parser move
struct AgentPrompt {
    name: String,
    path: PathBuf,
    frontmatter: AgentPromptFrontmatter,
    body: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct AgentPromptFrontmatter {
    name: Option<String>,
    description: Option<String>,
    color: Option<String>,
}

fn load_agent_prompt(path: &PathBuf) -> anyhow::Result<AgentPrompt> {
    let content = std::fs::read_to_string(path)?;
    let file_stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "agent".to_string());

    Ok(parse_agent_prompt(file_stem, path.clone(), &content))
}

fn parse_agent_prompt(
    name: impl Into<String>,
    path: impl Into<PathBuf>,
    content: &str,
) -> AgentPrompt {
    let name = name.into();
    let path = path.into();
    let (frontmatter, body) = parse_frontmatter(content);

    AgentPrompt {
        name: frontmatter.name.clone().unwrap_or(name),
        path,
        frontmatter,
        body,
    }
}

fn parse_frontmatter(content: &str) -> (AgentPromptFrontmatter, String) {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return (AgentPromptFrontmatter::default(), content.to_string());
    }

    let after_first = &trimmed[3..];
    if let Some(end_idx) = after_first.find("\n---") {
        let fm_text = &after_first[..end_idx];
        let body_start = end_idx + 4;
        let body = after_first[body_start..].trim_start().to_string();

        match serde_yaml::from_str::<AgentPromptFrontmatter>(fm_text) {
            Ok(fm) => return (fm, body),
            Err(_) => return (AgentPromptFrontmatter::default(), content.to_string()),
        }
    }

    (AgentPromptFrontmatter::default(), content.to_string())
}
