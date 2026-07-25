//! Per-turn system prompt renderer.
//!
//! [`PromptRenderer`] is the single source of truth for the system prompt.
//! It is invoked by `AgenticLoop::run_inner` at the top of every iteration,
//! fed a [`TurnPromptContext`], and returns the freshly rendered Markdown
//! body that becomes `messages[0]`. (The loop itself still lives in root
//! at `src/engine/agentic_loop.rs`; this module lifted in Phase 9b.N.5b.4
//! so the renderer can hold `Arc<dyn ToolFunnel>` instead of the concrete
//! root `ExtensionCore` type — the trait port keeps the renderer free of
//! root-only `HookPoint` / `HookInput` types.)
//!
//! ## Design
//!
//! - **Stateless.** The renderer carries no per-iteration state. The
//!   capability-diff tracker lives on the loop and is observed upstream
//!   of the renderer call.
//! - **Parallel hook dispatch.** The four hook-driven sections (`tools`,
//!   `skills`, `agents`, `mcp_context`) and the per-turn `SessionContextBuild`
//!   hook all fire concurrently via [`tokio::join!`]. Each handler is
//!   wrapped in a 2-second timeout; a slow or stuck handler soft-fails to
//!   empty so a single misbehaving extension can't stall the loop.
//! - **`mcp_context` normalized.** This section previously used plain
//!   `invoke_hook_text`; the rest use the trait-port
//!   [`ToolFunnel::invoke_prompt_section_hook`](peko_extension_host::ToolFunnel::invoke_prompt_section_hook).
//! - **`remove_missing=true` for placeholders.** Templates that omit
//!   any of the four control-surface placeholders get no section.
//!
//! ## Backward compatibility
//!
//! JSONL sessions written before this refactor may still contain
//! `MessageV2{role:"system"}` events. The loop overwrites `messages[0]`
//! on iteration 1, so a stale system message from disk is harmlessly
//! replaced. The renderer is also the right place to add a "stale
//! persisted system" warning later if telemetry warrants it.

// `CapabilityChange` + `CapabilityChangeKind` are referenced only by
// `#[cfg(test)]` blocks below, so `--lib` builds (no `--tests`) flag
// them as unused. Allow explicitly to keep `--lib` clean.
#[allow(unused_imports)]
use super::context::{
    CapabilityChange, CapabilityChangeKind, CapabilityDiff, IterationBudgetState, QuotaStateView,
    TurnPromptContext,
};
use super::placeholder::{replace_placeholders, Placeholder};
use chrono::Local;
use peko_extension_api::session::SessionSnapshot;
use peko_extension_host::ToolFunnel;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, warn};

/// Per-hook timeout budget. Two seconds is generous for the prompt-section
/// hooks (they only need to format a Markdown body from in-memory state)
/// and tight enough that a stuck handler cannot stall the agentic loop.
///
/// Phase 9b.2 lifted this constant to `peko-tools-core` (see
/// `peko_tools_core::HOOK_TIMEOUT`) so the engine crate can use the
/// same timeout value without taking a root-only dep on
/// `agents::prompt`. The local re-export keeps the body of this file
/// unchanged.
#[allow(unused_imports)]
pub(crate) use peko_tools_core::HOOK_TIMEOUT;

/// Renders the system prompt for one iteration.
///
/// Constructed once per agentic loop and shared across iterations. Cheap
/// to construct — just an `Arc` clone of the [`ToolFunnel`] trait object.
///
/// Phase 9b.N.5b.4 switched the field from `Arc<ExtensionCore>` (root-
/// only concrete type) to `Arc<dyn ToolFunnel>` so the renderer lifts
/// into `peko-engine` without dragging root `HookPoint` / `HookInput`
/// types along. The trait port is the same one the tool executor and
/// compaction orchestrator use (see `peko_extension_host::ToolFunnel`).
#[derive(Clone)]
pub struct PromptRenderer {
    extension_core: Arc<dyn ToolFunnel>,
}

impl PromptRenderer {
    /// Create a new renderer bound to an [`ExtensionCore`] via the
    /// canonical [`ToolFunnel`] trait port.
    #[must_use]
    pub fn new(extension_core: Arc<dyn ToolFunnel>) -> Self {
        Self { extension_core }
    }

    /// Render the system prompt for one iteration.
    ///
    /// Dispatches the four hook-driven sections plus `SessionContextBuild`
    /// in parallel (each with a 2s timeout) and assembles the final body
    /// via [`replace_placeholders`] with `remove_missing=true`.
    #[tracing::instrument(skip(self, ctx), fields(agent = %ctx.agent_name, iteration = ?ctx.iteration_budget.map(|i| i.iteration)))]
    pub async fn render_for_iteration(&self, ctx: &TurnPromptContext) -> String {
        // Empty body short-circuits to the one-line identity fallback so
        // callers that author agents without a body still get a
        // well-formed message.
        if ctx.body.trim().is_empty() {
            return format!("You are {}.", ctx.agent_name);
        }

        // Parallel hook dispatch. Each task is independent — a slow
        // `tools` handler must not delay `skills`. Each is wrapped in a
        // 2s timeout so a stuck handler cannot stall the loop; the
        // handler that hits the timeout simply returns an empty string
        // and the template's `remove_missing=true` strips any
        // leftover placeholder.
        let (tools, skills, agents, mcp, session_ctx) = tokio::join!(
            self.dispatch_text("tools", ctx),
            self.dispatch_text("skills", ctx),
            self.dispatch_text("agents", ctx),
            self.dispatch_text("mcp_context", ctx),
            self.dispatch_session_context(ctx),
        );

        let values = build_placeholder_values(ctx, &tools, &skills, &agents, &mcp, &session_ctx);
        replace_placeholders(&ctx.body, &values, true)
    }

    /// F23: render only the cache-stable prefix of the system prompt.
    ///
    /// Includes the agent body, inline identity / runtime / sandbox
    /// fields, and the four hook-driven sections (`tools`, `skills`,
    /// `agents`, `mcp_context`) — i.e. everything that is byte-stable
    /// across iterations within a session unless the profile or tool
    /// table mutates. Excludes per-iteration fields like
    /// `{{iteration_budget}}`, `{{quota_state}}`, `{{session_context}}`,
    /// `{{memory}}`, `{{timezone}}`, `{{soft_cancel}}`, and
    /// `{{capability_diff}}` (those go in [`render_per_turn`]).
    ///
    /// The engine loop caches the returned string in an `Arc<String>`
    /// and only re-renders when the profile or tool table changes.
    /// Adapter cache markers on this prefix give the provider
    /// byte-identical prefix matching turn-over-turn.
    #[tracing::instrument(skip(self, ctx), fields(agent = %ctx.agent_name))]
    pub async fn render_cache_stable(&self, ctx: &TurnPromptContext) -> String {
        // Parallel hook dispatch — same as the full render, but we only
        // consume the four stable-section hooks. The session-context
        // hook is volatile (it runs every iteration); we ignore its
        // result by reading an empty string into the values map.
        let (tools, skills, agents, mcp) = tokio::join!(
            self.dispatch_text("tools", ctx),
            self.dispatch_text("skills", ctx),
            self.dispatch_text("agents", ctx),
            self.dispatch_text("mcp_context", ctx),
        );

        let empty_session = String::new();
        let values =
            build_stable_placeholder_values(ctx, &tools, &skills, &agents, &mcp, &empty_session);
        replace_placeholders(&ctx.body, &values, true)
    }

    /// F23: render only the per-turn volatile suffix.
    ///
    /// Complements [`render_cache_stable`]: produces the trailing
    /// section of the system prompt that changes every iteration
    /// (`{{timezone}}`, `{{memory}}`, `{{session_context}}`,
    /// `{{iteration_budget}}`, `{{quota_state}}`, `{{soft_cancel}}`,
    /// `{{capability_diff}}`). The engine loop concatenates this with
    /// the cached prefix via [`assemble_system_prompt`]; the result is
    /// byte-stable across the prefix (cache hit) plus a fresh suffix
    /// each iteration.
    #[tracing::instrument(skip(self, ctx), fields(agent = %ctx.agent_name, iteration = ?ctx.iteration_budget.map(|i| i.iteration)))]
    pub async fn render_per_turn(&self, ctx: &TurnPromptContext) -> String {
        // The per-turn suffix is built from a stripped-down body that
        // only carries the volatile placeholders. We don't ship a
        // separate template per agent; instead we run
        // `replace_placeholders` over a synthetic body containing just
        // the volatile placeholders, with `remove_missing=true` so any
        // template that doesn't reference a volatile placeholder yields
        // an empty string (the engine loop will then reuse the cached
        // prefix as-is).
        //
        // The synthetic body lists each volatile placeholder on its
        // own line so each section is delimited.
        const VOLATILE_BODY: &str = "\
            {{timezone}}\n\
            {{memory}}\n\
            {{session_context}}\n\
            {{iteration_budget}}\n\
            {{quota_state}}\n\
            {{soft_cancel}}\n\
            {{capability_diff}}";

        let session_ctx = self.dispatch_session_context(ctx).await;
        let values = build_per_turn_placeholder_values(ctx, &session_ctx);
        replace_placeholders(VOLATILE_BODY, &values, true)
    }

    /// Assemble the cache-stable prefix and the per-turn suffix into
    /// the final system prompt string. When the suffix is empty
    /// (e.g. the template references no volatile placeholders), the
    /// prefix is returned verbatim so we don't add trailing
    /// whitespace.
    #[must_use]
    pub fn assemble_system_prompt(cache_stable: &str, per_turn: &str) -> String {
        if per_turn.is_empty() {
            cache_stable.to_string()
        } else {
            format!("{cache_stable}\n\n{per_turn}")
        }
    }

    /// Dispatch a single `PromptSystemSection` hook with a 2s timeout.
    /// Returns the empty string on timeout, missing handler, or error.
    async fn dispatch_text(&self, section: &str, ctx: &TurnPromptContext) -> String {
        // Phase 9b.N.5b.4 routes the hook firing through the trait port
        // (`ToolFunnel::invoke_prompt_section_hook`) so the renderer
        // never imports `HookPoint` / `HookInput` directly — those
        // types remain root-only until Phase 8's bulk move. The trait
        // impl (`src/engine/extension_core_funnel_compat.rs`) builds
        // `HookPoint::PromptSystemSection { section, priority }` +
        // `HookInput::Unit` internally and delegates to
        // `ExtensionCore::invoke_hook_text_with_principal`.
        let principal_id = Some(ctx.principal_id.as_str());
        let capabilities = Some(ctx.capability_strings());
        let active_extensions = Some(ctx.active_extension_vec());
        let workspace = Some(ctx.workspace.to_string_lossy().to_string());

        let core = Arc::clone(&self.extension_core);
        let result = tokio::time::timeout(
            HOOK_TIMEOUT,
            core.invoke_prompt_section_hook(
                section,
                100,
                principal_id,
                capabilities,
                active_extensions,
                workspace,
            ),
        )
        .await;

        match result {
            Ok(Some(text)) if !text.is_empty() => text,
            Ok(Some(_)) => String::new(),
            Ok(None) => {
                debug!(
                    section,
                    "PromptSystemSection hook returned no text; rendering empty section"
                );
                String::new()
            }
            Err(_) => {
                warn!(
                    section,
                    "PromptSystemSection hook exceeded 2s timeout; soft-failing to empty"
                );
                String::new()
            }
        }
    }

    /// Dispatch the per-turn `SessionContextBuild` hook. This is what
    /// `{{session_context}}` renders from. Distinct from the old
    /// `SessionStart` (now dormant) which only fired once.
    async fn dispatch_session_context(&self, ctx: &TurnPromptContext) -> String {
        let snapshot = SessionSnapshot {
            session_id: String::new(), // loop owns the real id; renderer's session_id is informational
            message_count: 0,
            context_tokens: 0,
            metadata: HashMap::new(),
        };

        // Phase 9b.N.5b.4: hook firing routes through the trait port
        // (`ToolFunnel::invoke_session_context_build_hook`) for the
        // same reason `dispatch_text` does — keeps the renderer free
        // of root-only `HookPoint` / `HookInput` types.
        let core = Arc::clone(&self.extension_core);
        let result = tokio::time::timeout(
            HOOK_TIMEOUT,
            core.invoke_session_context_build_hook(
                snapshot,
                Some(ctx.principal_id.as_str()),
                Some(ctx.capability_strings()),
                Some(ctx.active_extension_vec()),
                Some(ctx.workspace.to_string_lossy().to_string()),
            ),
        )
        .await;

        match result {
            Ok(Some(text)) => text,
            Ok(None) => String::new(),
            Err(_) => {
                warn!("SessionContextBuild hook exceeded 2s timeout; soft-failing to empty");
                String::new()
            }
        }
    }
}

/// Build the placeholder → value map for one iteration.
fn build_placeholder_values(
    ctx: &TurnPromptContext,
    tools: &str,
    skills: &str,
    agents: &str,
    mcp: &str,
    session_ctx: &str,
) -> HashMap<Placeholder, String> {
    let mut values = HashMap::new();

    // Inline placeholders
    values.insert(Placeholder::AgentName, ctx.agent_name.clone());
    values.insert(Placeholder::Workspace, ctx.workspace.display().to_string());
    values.insert(Placeholder::Channel, ctx.channel.clone());
    values.insert(Placeholder::ThinkingLevel, ctx.thinking_level.clone());
    values.insert(
        Placeholder::Timezone,
        Local::now().format("%:z").to_string(),
    );

    // Section placeholders (hook-driven)
    values.insert(Placeholder::Tools, format_tools_section(tools));
    values.insert(Placeholder::Skills, format_skills_section(skills));
    values.insert(Placeholder::Agents, format_agents_section(agents));
    values.insert(Placeholder::Runtime, format_runtime_section(ctx));
    values.insert(Placeholder::Sandbox, format_sandbox_section(ctx));
    values.insert(Placeholder::ModelAliases, format_model_aliases_section(ctx));
    values.insert(
        Placeholder::SelfUpdate,
        format_self_update_section(ctx.has_gateway),
    );
    values.insert(Placeholder::McpContext, mcp.to_string());
    values.insert(Placeholder::Memory, format_memory_section(ctx));
    values.insert(
        Placeholder::SessionContext,
        format_session_context_section(session_ctx),
    );

    // Control surfaces
    values.insert(
        Placeholder::IterationBudget,
        ctx.iteration_budget
            .as_ref()
            .map(IterationBudgetState::render)
            .unwrap_or_default(),
    );
    values.insert(
        Placeholder::QuotaState,
        ctx.quota_state
            .as_ref()
            .map(QuotaStateView::render)
            .unwrap_or_default(),
    );
    values.insert(
        Placeholder::SoftCancel,
        if ctx.soft_cancel_pending {
            render_soft_cancel_section()
        } else {
            String::new()
        },
    );
    values.insert(
        Placeholder::CapabilityDiff,
        ctx.capability_diff
            .as_ref()
            .map(CapabilityDiff::render)
            .unwrap_or_default(),
    );

    values
}

/// F23: build the placeholder → value map for the cache-stable prefix.
///
/// Same shape as `build_placeholder_values`, but only fills the
/// placeholders that are byte-stable across iterations: inline
/// identity, runtime, sandbox, model aliases, self-update, and the
/// four hook-driven section placeholders. Volatile placeholders
/// (`timezone`, `memory`, `session_context`, `iteration_budget`,
/// `quota_state`, `soft_cancel`, `capability_diff`) are omitted —
/// `remove_missing=true` strips them on render.
fn build_stable_placeholder_values(
    ctx: &TurnPromptContext,
    tools: &str,
    skills: &str,
    agents: &str,
    mcp: &str,
    _session_ctx: &str,
) -> HashMap<Placeholder, String> {
    let mut values = HashMap::new();

    // Inline identity / runtime (no volatile clock).
    values.insert(Placeholder::AgentName, ctx.agent_name.clone());
    values.insert(Placeholder::Workspace, ctx.workspace.display().to_string());
    values.insert(Placeholder::Channel, ctx.channel.clone());
    values.insert(Placeholder::ThinkingLevel, ctx.thinking_level.clone());
    // Placeholder::Timezone intentionally omitted — volatile.

    // Hook-driven sections.
    values.insert(Placeholder::Tools, format_tools_section(tools));
    values.insert(Placeholder::Skills, format_skills_section(skills));
    values.insert(Placeholder::Agents, format_agents_section(agents));
    values.insert(Placeholder::Runtime, format_runtime_section(ctx));
    values.insert(Placeholder::Sandbox, format_sandbox_section(ctx));
    values.insert(Placeholder::ModelAliases, format_model_aliases_section(ctx));
    values.insert(
        Placeholder::SelfUpdate,
        format_self_update_section(ctx.has_gateway),
    );
    values.insert(Placeholder::McpContext, mcp.to_string());
    // Memory, SessionContext, IterationBudget, QuotaState, SoftCancel,
    // CapabilityDiff intentionally omitted — volatile.

    values
}

/// F23: build the placeholder → value map for the per-turn suffix.
///
/// Only fills the volatile placeholders. Stable placeholders are
/// omitted; `remove_missing=true` strips any leftover references in
/// the synthetic body.
fn build_per_turn_placeholder_values(
    ctx: &TurnPromptContext,
    session_ctx: &str,
) -> HashMap<Placeholder, String> {
    let mut values = HashMap::new();

    values.insert(
        Placeholder::Timezone,
        Local::now().format("%:z").to_string(),
    );
    values.insert(Placeholder::Memory, format_memory_section(ctx));
    values.insert(
        Placeholder::SessionContext,
        format_session_context_section(session_ctx),
    );

    values.insert(
        Placeholder::IterationBudget,
        ctx.iteration_budget
            .as_ref()
            .map(IterationBudgetState::render)
            .unwrap_or_default(),
    );
    values.insert(
        Placeholder::QuotaState,
        ctx.quota_state
            .as_ref()
            .map(QuotaStateView::render)
            .unwrap_or_default(),
    );
    values.insert(
        Placeholder::SoftCancel,
        if ctx.soft_cancel_pending {
            render_soft_cancel_section()
        } else {
            String::new()
        },
    );
    values.insert(
        Placeholder::CapabilityDiff,
        ctx.capability_diff
            .as_ref()
            .map(CapabilityDiff::render)
            .unwrap_or_default(),
    );

    values
}

fn format_tools_section(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let mut lines = vec![
        "## Available Tools".to_string(),
        "You have access to the following tools. Use them wisely.".to_string(),
        String::new(),
        text.to_string(),
        String::new(),
        "### Tool Use Guidelines".to_string(),
        "- Think step by step. Use available tools when needed to accomplish tasks.".to_string(),
        "- Multiple tools can be called in parallel if they are independent.".to_string(),
        "- When you have the final answer, provide it directly without tool calls.".to_string(),
        String::new(),
        "### Tool Timeout and Async Execution".to_string(),
        "All tool calls have a constant 5-minute timeout. If a tool exceeds this timeout, the work is automatically detached to a background task and a receipt is returned.".to_string(),
        "To invoke a tool explicitly in the background, use the `task` tool with `action=\"spawn\"` and specify the target tool and parameters.".to_string(),
        "Use the `task` tool with `action=\"output\"` to retrieve the full result of a background task.".to_string(),
        String::new(),
        "Example:".to_string(),
        "```json".to_string(),
        r#"{"action": "spawn", "tool": "Agent", "params": {"prompt": "Analyze confidential data", "subagent_type": "researcher"}}"#.to_string(),
        "```".to_string(),
    ];
    lines.push(String::new());
    lines.join("\n")
}

fn format_skills_section(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    format!(
        r"## Skills (mandatory)
Before replying: scan <available_skills> <description> entries.
- If exactly one skill clearly applies: invoke the `Skill` tool with `name` = the skill name, then follow the returned body.
- If multiple could apply: choose the most specific one, then invoke `Skill` with that name and follow the returned body.
- If none clearly apply: do not invoke any skill.
Constraints: never invoke more than one skill up front; only invoke after selecting.

<available_skills>
{text}
</available_skills>"
    )
}

fn format_agents_section(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    format!(
        r"## Available Agents
When delegating, choose the most appropriate agent from the list below. Each agent has a name you can pass to the `Agent` tool as `subagent_type`.

<available_agents>
{text}
</available_agents>"
    )
}

fn format_runtime_section(ctx: &TurnPromptContext) -> String {
    let hostname = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "unknown".to_string());
    format!(
        "## Runtime\nAgent: {}\nHost: {hostname}\nOS: {}\nModel: {}\nChannel: {}",
        ctx.agent_name,
        std::env::consts::OS,
        ctx.resolved_model,
        ctx.channel,
    )
}

fn format_sandbox_section(ctx: &TurnPromptContext) -> String {
    if ctx.sandbox_enabled {
        "## Sandbox\nSandbox: enabled\nTools run in isolated environment with restricted access."
            .to_string()
    } else {
        String::new()
    }
}

fn format_model_aliases_section(ctx: &TurnPromptContext) -> String {
    if ctx.model_aliases.is_empty() {
        return String::new();
    }
    let mut lines = vec!["## Model Aliases".to_string()];
    lines.push(
        "Prefer aliases when specifying model overrides; full provider/model is also accepted."
            .to_string(),
    );
    for alias in &ctx.model_aliases {
        lines.push(format!("- {alias}"));
    }
    lines.join("\n")
}

fn format_self_update_section(has_gateway: bool) -> String {
    if has_gateway {
        "## Self-Update\n\
            Get Updates (self-update) is ONLY allowed when the user explicitly asks for it.\n\
            Do not run config.apply or update.run unless the user explicitly requests an update or config change; if it's not explicit, ask first.\n\
            Actions: config.get, config.schema, config.apply (validate + write full config, then restart), update.run (update deps or git, then restart).\n\
            After restart, OpenClaw pings the last active session automatically.".to_string()
    } else {
        String::new()
    }
}

fn format_memory_section(ctx: &TurnPromptContext) -> String {
    let Some(memory) = ctx.principal_memory.as_deref() else {
        return String::new();
    };
    let trimmed = memory.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    format!("## Your long-term memory (MEMORY.md)\n\n{trimmed}\n")
}

fn format_session_context_section(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    format!("## Session context\n\n{trimmed}\n")
}

fn render_soft_cancel_section() -> String {
    "## Cancellation requested\n\
     The user has signalled cancellation. Finish the current step cleanly,\
     return a concise final answer, and do not start a new tool round.\n"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use peko_extension_api::session::SessionSnapshot;
    use peko_extension_host::ToolFunnel;
    use peko_subject::PrincipalId;
    use std::path::PathBuf;

    /// No-op `ToolFunnel` impl used by the renderer's unit tests.
    ///
    /// Phase 9b.N.5b.4 can't import the root-owned `ExtensionCore` into
    /// `peko-engine` (it stays root until Phase 8's bulk move). The
    /// tests need a `ToolFunnel` that produces the same observable
    /// behavior as `ExtensionCore::new()` — an empty registry where no
    /// handlers are registered, so every hook call returns `None`.
    /// That keeps the existing test assertions (placeholder stripping
    /// with no handlers) valid.
    #[derive(Default)]
    struct EmptyExtensionCore;

    #[async_trait]
    impl ToolFunnel for EmptyExtensionCore {
        async fn is_parallelizable(&self, _tool_name: &str) -> bool {
            true
        }
        async fn pre_tool_use(
            &self,
            _tool_name: &str,
            _params: serde_json::Value,
            _workspace: Option<String>,
            _agent_id: Option<String>,
            _session_id: Option<String>,
            _caller_id: Option<String>,
            _principal_id: Option<String>,
            _principal_name: Option<String>,
            _capabilities: Option<Vec<String>>,
            _active_extensions: Option<Vec<String>>,
        ) {
        }
        async fn post_tool_use(
            &self,
            _tool_name: &str,
            _params: serde_json::Value,
            _workspace: Option<String>,
            _agent_id: Option<String>,
            _session_id: Option<String>,
            _caller_id: Option<String>,
            _principal_id: Option<String>,
            _principal_name: Option<String>,
            _capabilities: Option<Vec<String>>,
            _active_extensions: Option<Vec<String>>,
        ) {
        }
        async fn execute_tool_via_hook(
            &self,
            _tool_name: &str,
            _params: serde_json::Value,
            _workspace: Option<String>,
            _agent_id: Option<String>,
            _session_id: Option<String>,
            _caller_id: Option<String>,
            _principal_id: Option<String>,
            _principal_name: Option<String>,
            _capabilities: Option<Vec<String>>,
            _active_extensions: Option<Vec<String>>,
            _abort_signal: Option<tokio::sync::watch::Receiver<bool>>,
        ) -> anyhow::Result<(String, serde_json::Value, bool)> {
            anyhow::bail!("EmptyExtensionCore::execute_tool_via_hook not implemented")
        }
        async fn invoke_session_compaction_pre_hook(
            &self,
            _payload: peko_extension_api::hook_io::CompactionPreparationPayload,
        ) -> peko_extension_api::hook_io::HookDecision {
            peko_extension_api::hook_io::HookDecision::PassThrough
        }
        async fn invoke_session_compaction_post_hook(
            &self,
            _payload: peko_extension_api::hook_io::CompactionResultPayload,
        ) -> peko_extension_api::hook_io::HookDecision {
            peko_extension_api::hook_io::HookDecision::PassThrough
        }
        async fn invoke_session_state_change_hook(
            &self,
            _snapshot: SessionSnapshot,
        ) -> peko_extension_api::hook_io::HookDecision {
            peko_extension_api::hook_io::HookDecision::PassThrough
        }
        async fn invoke_stop_hook(&self, _merged: serde_json::Value) {}
        async fn invoke_after_agent_hook(&self, _merged: serde_json::Value) {}
        async fn set_session_key(&self, _agent_id: &str, _key: Option<String>) {}
        async fn list_tool_definitions_with_allowlist(
            &self,
            _capabilities: &peko_extension_api::Capabilities,
            _active_extensions: Option<&peko_extension_api::ActiveExtensionSet>,
            _principal_id: &PrincipalId,
        ) -> Vec<peko_provider_api::ToolDefinition> {
            Vec::new()
        }
        async fn has_deferred_tools_for(&self, _principal_id: &PrincipalId) -> bool {
            false
        }
        async fn invoke_prompt_section_hook(
            &self,
            _section: &str,
            _priority: i32,
            _principal_id: Option<&str>,
            _capabilities: Option<Vec<String>>,
            _active_extensions: Option<Vec<String>>,
            _workspace: Option<String>,
        ) -> Option<String> {
            None
        }
        async fn invoke_session_context_build_hook(
            &self,
            _snapshot: SessionSnapshot,
            _principal_id: Option<&str>,
            _capabilities: Option<Vec<String>>,
            _active_extensions: Option<Vec<String>>,
            _workspace: Option<String>,
        ) -> Option<String> {
            None
        }
    }

    fn empty_funnel() -> Arc<dyn ToolFunnel> {
        Arc::new(EmptyExtensionCore::default())
    }

    fn empty_ctx() -> TurnPromptContext {
        TurnPromptContext {
            principal_id: "test-principal".to_string(),
            agent_name: "test-agent".to_string(),
            body: "You are {{agent_name}} on {{workspace}}.".to_string(),
            capabilities: None,
            active_extensions: None,
            principal_memory: None,
            workspace: PathBuf::from("/tmp/workspace"),
            resolved_model: "default".to_string(),
            channel: "discord".to_string(),
            thinking_level: "medium".to_string(),
            sandbox_enabled: false,
            model_aliases: vec![],
            has_gateway: false,
            iteration_budget: None,
            quota_state: None,
            soft_cancel_pending: false,
            capability_diff: None,
            tool_definitions: vec![],
        }
    }

    #[tokio::test]
    async fn render_empty_body_falls_back_to_identity() {
        let renderer = PromptRenderer::new(empty_funnel());
        let mut ctx = empty_ctx();
        ctx.body = String::new();
        let rendered = renderer.render_for_iteration(&ctx).await;
        assert_eq!(rendered, "You are test-agent.");
    }

    #[tokio::test]
    async fn render_replaces_inline_placeholders() {
        let renderer = PromptRenderer::new(empty_funnel());
        let ctx = empty_ctx();
        let rendered = renderer.render_for_iteration(&ctx).await;
        assert!(rendered.contains("You are test-agent"));
        assert!(rendered.contains("/tmp/workspace"));
        assert!(!rendered.contains("{{agent_name}}"));
    }

    #[tokio::test]
    async fn render_drops_unknown_placeholders() {
        let renderer = PromptRenderer::new(empty_funnel());
        let mut ctx = empty_ctx();
        ctx.body = "Hi {{agent_name}}; unknown: {{nope}}".to_string();
        let rendered = renderer.render_for_iteration(&ctx).await;
        assert_eq!(rendered, "Hi test-agent; unknown: ");
    }

    #[tokio::test]
    async fn render_emits_session_context_when_set() {
        let renderer = PromptRenderer::new(empty_funnel());
        let mut ctx = empty_ctx();
        ctx.body = "Hi {{agent_name}}\n\n{{session_context}}\n".to_string();
        // No SessionContextBuild handler registered → empty section → no header.
        let rendered = renderer.render_for_iteration(&ctx).await;
        assert!(!rendered.contains("## Session context"));
    }

    #[tokio::test]
    async fn render_emits_soft_cancel_when_pending() {
        let renderer = PromptRenderer::new(empty_funnel());
        let mut ctx = empty_ctx();
        ctx.body = "{{soft_cancel}}".to_string();
        ctx.soft_cancel_pending = true;
        let rendered = renderer.render_for_iteration(&ctx).await;
        assert!(rendered.contains("Cancellation requested"));
    }

    #[tokio::test]
    async fn render_omits_soft_cancel_when_not_pending() {
        let renderer = PromptRenderer::new(empty_funnel());
        let mut ctx = empty_ctx();
        ctx.body = "{{soft_cancel}}".to_string();
        ctx.soft_cancel_pending = false;
        let rendered = renderer.render_for_iteration(&ctx).await;
        assert_eq!(rendered, "");
    }

    // Phase 3: control-surface end-to-end coverage. Each test pins a
    // single field on `ctx`, renders, and asserts on the resulting
    // Markdown body. Together these prove the renderer correctly wires
    // `{{iteration_budget}}`, `{{quota_state}}`, and
    // `{{capability_diff}}` from `TurnPromptContext` into the
    // rendered prompt. (`{{soft_cancel}}` is already covered above.)

    #[tokio::test]
    async fn render_includes_iteration_budget_when_set() {
        let renderer = PromptRenderer::new(empty_funnel());
        let mut ctx = empty_ctx();
        ctx.body = "{{iteration_budget}}".to_string();
        ctx.iteration_budget = Some(IterationBudgetState {
            iteration: 3,
            max_iterations: 10,
        });
        let rendered = renderer.render_for_iteration(&ctx).await;
        assert!(rendered.contains("## Iteration budget"));
        assert!(rendered.contains("Iteration 3 of 10"));
        assert!(!rendered.contains("Approaching limit"));
    }

    #[tokio::test]
    async fn render_includes_iteration_budget_approaching_limit() {
        let renderer = PromptRenderer::new(empty_funnel());
        let mut ctx = empty_ctx();
        ctx.body = "{{iteration_budget}}".to_string();
        // iter 9 of 10 triggers "Approaching limit" but not "Stop and finalize"
        ctx.iteration_budget = Some(IterationBudgetState {
            iteration: 9,
            max_iterations: 10,
        });
        let rendered = renderer.render_for_iteration(&ctx).await;
        assert!(rendered.contains("Approaching limit"));
        assert!(!rendered.contains("Stop and finalize"));
    }

    #[tokio::test]
    async fn render_emits_quota_state_with_pct_when_set() {
        use std::time::SystemTime;
        let renderer = PromptRenderer::new(empty_funnel());
        let mut ctx = empty_ctx();
        ctx.body = "{{quota_state}}".to_string();
        ctx.quota_state = Some(QuotaStateView {
            input_tokens: 50,
            output_tokens: 0,
            request_count: 3,
            window_end: SystemTime::UNIX_EPOCH,
            input_limit: Some(100),
            output_limit: None,
            request_limit: Some(10),
        });
        let rendered = renderer.render_for_iteration(&ctx).await;
        assert!(rendered.contains("## Quota status (current window)"));
        assert!(rendered.contains("Input tokens:"));
        assert!(rendered.contains("Output tokens:"));
        assert!(rendered.contains("Requests:"));
        assert!(rendered.contains("Window resets:"));
        assert!(rendered.contains("50/100"));
        assert!(rendered.contains("50%"));
        assert!(rendered.contains("3/10"));
    }

    #[tokio::test]
    async fn render_emits_quota_state_trip_message_when_exceeded() {
        use std::time::SystemTime;
        let renderer = PromptRenderer::new(empty_funnel());
        let mut ctx = empty_ctx();
        ctx.body = "{{quota_state}}".to_string();
        ctx.quota_state = Some(QuotaStateView {
            input_tokens: 100,
            output_tokens: 0,
            request_count: 0,
            window_end: SystemTime::UNIX_EPOCH,
            input_limit: Some(100),
            output_limit: None,
            request_limit: None,
        });
        let rendered = renderer.render_for_iteration(&ctx).await;
        assert!(rendered.contains("Quota tripped"));
    }

    #[tokio::test]
    async fn render_collapses_quota_state_when_unlimited() {
        use std::time::SystemTime;
        let renderer = PromptRenderer::new(empty_funnel());
        let mut ctx = empty_ctx();
        ctx.body = "{{quota_state}}".to_string();
        ctx.quota_state = Some(QuotaStateView {
            input_tokens: 0,
            output_tokens: 0,
            request_count: 0,
            window_end: SystemTime::UNIX_EPOCH,
            input_limit: None,
            output_limit: None,
            request_limit: None,
        });
        let rendered = renderer.render_for_iteration(&ctx).await;
        assert!(!rendered.contains("## Quota status"));
    }

    #[tokio::test]
    async fn render_emits_capability_diff_section_when_changed() {
        let renderer = PromptRenderer::new(empty_funnel());
        let mut ctx = empty_ctx();
        ctx.body = "{{capability_diff}}".to_string();
        let diff = CapabilityDiff {
            granted: vec![CapabilityChange {
                capability: "tool:Write".to_string(),
                kind: CapabilityChangeKind::Granted,
            }],
            revoked: vec![CapabilityChange {
                capability: "tool:Bash".to_string(),
                kind: CapabilityChangeKind::Revoked,
            }],
        };
        ctx.capability_diff = Some(diff);
        let rendered = renderer.render_for_iteration(&ctx).await;
        assert!(rendered.contains("## Capability changes since last turn"));
        assert!(rendered.contains("Granted:"));
        assert!(rendered.contains("- tool:Write"));
        assert!(rendered.contains("Revoked:"));
        assert!(rendered.contains("- tool:Bash"));
    }

    #[tokio::test]
    async fn render_omits_capability_diff_when_none() {
        let renderer = PromptRenderer::new(empty_funnel());
        let mut ctx = empty_ctx();
        ctx.body = "{{capability_diff}}".to_string();
        ctx.capability_diff = None;
        let rendered = renderer.render_for_iteration(&ctx).await;
        assert!(!rendered.contains("## Capability changes"));
    }

    // ---------- F23: cache_stable + per_turn split ----------

    /// Two renderings of the cache-stable prefix with the same context
    /// produce byte-identical strings — the foundation of provider
    /// prefix-cache hits. The volatile placeholders
    /// (`{{iteration_budget}}`, `{{quota_state}}`, `{{session_context}}`)
    /// are absent from the prefix; mutating them between renders
    /// must not change the prefix.
    #[tokio::test]
    async fn render_cache_stable_byte_identical_across_iterations() {
        let renderer = PromptRenderer::new(empty_funnel());
        let mut ctx = empty_ctx();
        ctx.body = "You are {{agent_name}} on {{workspace}}.".to_string();

        let prefix_first = renderer.render_cache_stable(&ctx).await;
        // Mutate only volatile fields; prefix must not change.
        ctx.iteration_budget = Some(IterationBudgetState {
            iteration: 5,
            max_iterations: 10,
        });
        ctx.soft_cancel_pending = true;
        let prefix_second = renderer.render_cache_stable(&ctx).await;

        assert_eq!(prefix_first, prefix_second);
    }

    /// `assemble_system_prompt` returns the prefix verbatim when the
    /// suffix is empty; otherwise concatenates with a blank-line
    /// separator.
    #[test]
    fn assemble_system_prompt_concatenates_with_separator() {
        // Empty suffix: prefix returned unchanged.
        assert_eq!(
            PromptRenderer::assemble_system_prompt("Hello.", ""),
            "Hello."
        );
        // Non-empty suffix: joined with one blank line.
        assert_eq!(
            PromptRenderer::assemble_system_prompt("Prefix.", "Suffix."),
            "Prefix.\n\nSuffix."
        );
    }

    /// The per-turn suffix contains the volatile placeholders the
    /// template references. Mutating a volatile field between calls
    /// changes the suffix (proves it isn't accidentally stable).
    #[tokio::test]
    async fn render_per_turn_changes_with_volatile_fields() {
        let renderer = PromptRenderer::new(empty_funnel());
        let mut ctx = empty_ctx();
        ctx.body = "{{iteration_budget}} {{quota_state}}".to_string();
        ctx.iteration_budget = Some(IterationBudgetState {
            iteration: 1,
            max_iterations: 10,
        });

        let suffix_first = renderer.render_per_turn(&ctx).await;
        ctx.iteration_budget = Some(IterationBudgetState {
            iteration: 9,
            max_iterations: 10,
        });
        let suffix_second = renderer.render_per_turn(&ctx).await;

        assert!(suffix_first.contains("Iteration 1 of 10"));
        assert!(suffix_second.contains("Approaching limit"));
    }
}
