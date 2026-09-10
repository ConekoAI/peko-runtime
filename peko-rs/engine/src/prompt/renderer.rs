//! Per-turn system prompt renderer.
//!
//! [`PromptRenderer`] is the single source of truth for the system prompt.
//! The loop renders [`PromptRenderer::render_cache_stable`] once per run
//! and freezes the result as `messages[0]`; per-iteration volatile
//! context (clock, memory, session context, workspace catalogs,
//! iteration budget, one-shot banners) is rendered by
//! [`PromptRenderer::render_runtime_context`] and appended by the loop
//! as a user-role `<runtime-context>` message at the TAIL of the
//! conversation. (The loop itself still lives in root
//! at `src/engine/agentic_loop.rs`; this module lifted in Phase 9b.N.5b.4
//! so the renderer can hold `Arc<dyn ToolFunnel>` instead of the concrete
//! root `ExtensionCore` type — the trait port keeps the renderer free of
//! root-only `HookPoint` / `HookInput` types.)
//!
//! ## Design
//!
//! - **Frozen system prompt + tail injection.** Both Anthropic explicit
//!   `cache_control` breakpoints and OpenAI/DeepSeek automatic prefix
//!   caching match from the front of the payload, so the head must be
//!   byte-stable across iterations. The old scheme concatenated a
//!   volatile suffix (`{{current_time}}`, `{{iteration_budget}}`, ...)
//!   into `messages[0]` every iteration, which mutated the head and
//!   destroyed prefix caching for the ENTIRE history. The 2026-09-10
//!   fix freezes `messages[0]` and moves volatile context to an
//!   append-only tail message with per-section change detection
//!   ([`RuntimeContextState`]) so large rarely-changing sections are
//!   not re-injected. Matches the codex / kimi-code / deepseek-harness
//!   layout.
//! - **Stateless renderer.** The renderer carries no per-iteration
//!   state. The capability-diff tracker and the runtime-context
//!   change-detection state live on the loop and are passed in.
//! - **Parallel hook dispatch.** The `mcp_context` section and the
//!   per-turn `SessionContextBuild` / `skills` / `agents` hooks all fire
//!   concurrently via [`tokio::join!`]. Each handler is wrapped
//!   in a 2-second timeout; a slow or stuck handler soft-fails to empty so
//!   a single misbehaving extension can't stall the loop. The `tools`
//!   section is intentionally not dispatched — tool catalogs travel on
//!   the wire as the `tools[]` JSON-schema array (see
//!   `crate::agentic_loop::build_tool_definitions` and the
//!   `list_tool_definitions_with_allowlist` filter in
//!   `peko_core::extensions::framework::core::registry`).
//! - **`skills` / `agents` ride the tail.** Both sections render in the
//!   runtime-context message ([`PromptRenderer::render_runtime_context`]),
//!   not the frozen system prompt, so agents/skills added to the
//!   workspace appear on the very next iteration while `messages[0]`
//!   stays byte-identical for the provider prefix cache. Per-section
//!   change detection means the catalogs are only re-injected when the
//!   scan result actually changes.
//! - **`mcp_context` normalized.** This section previously used plain
//!   `invoke_hook_text`; the rest use the trait-port
//!   [`ToolFunnel::invoke_prompt_section_hook`](peko_extension_api::ToolFunnel::invoke_prompt_section_hook).
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
    render_quota_tripped_section, CapabilityChange, CapabilityChangeKind, CapabilityDiff,
    IterationBudgetState, TurnPromptContext,
};
use super::placeholder::{replace_placeholders, Placeholder};
use async_trait::async_trait;
use chrono::{Local, Utc};
use peko_extension_api::session::SessionSnapshot;
use peko_extension_api::ToolFunnel;
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

/// Source for the `{{mcp_context}}` system-prompt section.
///
/// Phase 2 PR 2 (ADR-047 §2.3) deletes the framework `McpAdapter`,
/// so the `PromptSystemSection { section: "mcp_context" }` hook has
/// no handler. The renderer instead consults an
/// `McpPromptContextProvider` directly. The default
/// [`EmptyMcpPromptContextProvider] returns the empty string
/// (templates that reference `{{mcp_context}}` get the placeholder
/// stripped by `remove_missing=true`); the daemon constructs a real
/// provider wrapping the global `McpManager`.
#[async_trait]
pub trait McpPromptContextProvider: Send + Sync {
    /// Render the `mcp_context` Markdown body. The empty string means
    /// "no MCP servers configured".
    async fn render_mcp_context(&self) -> String;
}

/// Default no-op provider — used by tests and any process that has
/// no global `McpManager` initialised (so MCP context never
/// silently leaks state from a prior test in the same process).
#[derive(Default)]
pub struct EmptyMcpPromptContextProvider;

#[async_trait]
impl McpPromptContextProvider for EmptyMcpPromptContextProvider {
    async fn render_mcp_context(&self) -> String {
        String::new()
    }
}

/// Renders the system prompt for one iteration.
///
/// Constructed once per agentic loop and shared across iterations. Cheap
/// to construct — just an `Arc` clone of the [`ToolFunnel`] trait object.
///
/// Phase 9b.N.5b.4 switched the field from `Arc<ExtensionCore>` (root-
/// only concrete type) to `Arc<dyn ToolFunnel>` so the renderer lifts
/// into `peko-engine` without dragging root `HookPoint` / `HookInput`
/// types along. The trait port is the same one the tool executor and
/// compaction orchestrator use (see `peko_extension_api::ToolFunnel`).
///
/// Phase 2 PR 2 (ADR-047 §2.3) added the `mcp_context_provider`
/// field. The `{{mcp_context}}` placeholder no longer fires a
/// `PromptSystemSection` framework hook — the framework `McpAdapter`
/// is gone — so the renderer calls the provider directly instead.
#[derive(Clone)]
pub struct PromptRenderer {
    extension_core: Arc<dyn ToolFunnel>,
    mcp_context_provider: Arc<dyn McpPromptContextProvider>,
}

impl PromptRenderer {
    /// Create a new renderer bound to an [`ExtensionCore`] via the
    /// canonical [`ToolFunnel`] trait port, with a default
    /// (empty) MCP context provider.
    #[must_use]
    pub fn new(extension_core: Arc<dyn ToolFunnel>) -> Self {
        Self {
            extension_core,
            mcp_context_provider: Arc::new(EmptyMcpPromptContextProvider),
        }
    }

    /// Create a new renderer with an explicit MCP context provider.
    /// Used by the daemon which wraps the global `McpManager` in a
    /// provider that calls
    /// `peko_extensions_mcp::render_mcp_prompt_context`.
    #[must_use]
    pub fn with_mcp_context_provider(
        extension_core: Arc<dyn ToolFunnel>,
        mcp_context_provider: Arc<dyn McpPromptContextProvider>,
    ) -> Self {
        Self {
            extension_core,
            mcp_context_provider,
        }
    }

    /// Render the system prompt for one iteration.
    ///
    /// Dispatches the three hook-driven sections plus `SessionContextBuild`
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
        // `skills` handler must not delay `agents`. Each is wrapped in a
        // 2s timeout so a stuck handler cannot stall the loop; the
        // handler that hits the timeout simply returns an empty string
        // and the template's `remove_missing=true` strips any
        // leftover placeholder.
        //
        // Phase 2 PR 2: `mcp_context` no longer goes through
        // `dispatch_text`; the framework McpAdapter was deleted, so
        // the framework hook has no handler. The provider field on
        // the renderer is the canonical source.
        let (skills, agents, mcp, session_ctx) = tokio::join!(
            self.dispatch_text("skills", ctx),
            self.dispatch_text("agents", ctx),
            self.mcp_context_provider.render_mcp_context(),
            self.dispatch_session_context(ctx),
        );

        let values = build_placeholder_values(ctx, &skills, &agents, &mcp, &session_ctx);
        replace_placeholders(&ctx.body, &values, true)
    }

    /// F23: render the cache-stable system prompt.
    ///
    /// Includes the agent body, inline identity / runtime / sandbox
    /// fields, and the `mcp_context` section — i.e. everything that is
    /// byte-stable across iterations within a session unless the profile
    /// mutates. Excludes per-iteration fields like `{{iteration_budget}}`,
    /// `{{quota_tripped}}`, `{{session_context}}`, `{{memory}}`,
    /// `{{current_time}}`, `{{soft_cancel}}`, and
    /// `{{capability_diff}}` (those go in the tail runtime-context
    /// message — see [`render_runtime_context`]).
    /// `{{timezone}}` is retired from the per-turn context (redundant
    /// with `{{current_time}}`); it still resolves in
    /// [`render_for_iteration`] for legacy templates.
    /// `{{skills}}` and `{{agents}}` also go in
    /// [`render_runtime_context`]: they are workspace-scanned catalogs,
    /// so rendering them in the tail message lets new workspace files
    /// appear on the next iteration while the frozen system prompt
    /// stays byte-identical for the provider prefix cache. Tool
    /// catalogs are not part of the system prompt; they reach the model
    /// on the wire as the `tools[]` JSON-schema array.
    ///
    /// The engine loop renders this once per run, caches the string in
    /// an `Arc<String>`, and freezes it as `messages[0]` — it is never
    /// rebuilt mid-run. Adapter cache markers on this fully-static
    /// prompt give the provider byte-identical prefix matching
    /// turn-over-turn.
    #[tracing::instrument(skip(self, ctx), fields(agent = %ctx.agent_name))]
    pub async fn render_cache_stable(&self, ctx: &TurnPromptContext) -> String {
        // `mcp_context` is the only hook-driven section left in the
        // prefix — `skills` / `agents` moved to the volatile suffix
        // (see the doc above). The session-context hook is volatile
        // (it runs every iteration); we ignore its result by reading
        // an empty string into the values map.
        //
        // Phase 2 PR 2: `mcp_context` sourced from the provider field,
        // not from a framework hook.
        let mcp = self.mcp_context_provider.render_mcp_context().await;

        let values = build_stable_placeholder_values(ctx, &mcp);
        replace_placeholders(&ctx.body, &values, true)
    }

    /// Render the per-iteration runtime-context message body.
    ///
    /// Complements [`render_cache_stable`]: produces the volatile
    /// context that used to ride in the system prompt's per-turn
    /// suffix (`{{current_time}}`, `{{memory}}`,
    /// `{{session_context}}`, `{{agents}}`, `{{skills}}`,
    /// `{{iteration_budget}}`, `{{quota_tripped}}`, `{{soft_cancel}}`,
    /// `{{capability_diff}}`). The engine loop appends the returned
    /// string as a user-role message at the TAIL of the conversation,
    /// so `messages[0]` stays byte-identical across iterations and the
    /// provider's front-anchored prefix cache (Anthropic explicit
    /// breakpoints, OpenAI/DeepSeek automatic) keeps hitting.
    ///
    /// Section injection policy:
    ///
    /// - **Always** (tiny): the iteration-budget line.
    /// - **On change**: current time (minute granularity, so it changes
    ///   at most once a minute), memory, session context, and the
    ///   agents/skills workspace catalogs. Each section's rendered text
    ///   is value-compared against the last injected value carried in
    ///   `state`; only changed sections are re-injected, so large
    ///   rarely-changing sections (memory, catalogs) don't bloat every
    ///   iteration.
    /// - **Event-edged**: `quota_tripped`, `soft_cancel`,
    ///   `capability_diff` — included only when the corresponding
    ///   `TurnPromptContext` field fires (the loop computes the rising
    ///   edges upstream).
    ///
    /// Returns `None` when no section is due this iteration (in
    /// practice only possible when `iteration_budget` is unset, since
    /// that line is always included when present). The body is wrapped
    /// in a `<runtime-context>...</runtime-context>` envelope.
    ///
    /// `{{quota_state}}` was retired 2026-09-09 — the principal's live
    /// quota snapshot now lives on the `session` tool's `status` action
    /// (`QuotaSnapshot`). `{{quota_tripped}}` stays as a single-shot
    /// rising-edge banner (mirrors `{{soft_cancel}}`) so the agent
    /// still sees an advisory the moment the principal trips.
    #[tracing::instrument(skip(self, ctx, state), fields(agent = %ctx.agent_name, iteration = ?ctx.iteration_budget.map(|i| i.iteration)))]
    pub async fn render_runtime_context(
        &self,
        ctx: &TurnPromptContext,
        state: &mut RuntimeContextState,
    ) -> Option<String> {
        // Parallel hook dispatch: the workspace-catalog sections
        // (`agents`, `skills`) and `SessionContextBuild` are independent,
        // so fire them concurrently. Each `dispatch_text` carries its
        // own 2s timeout. The handlers mtime-cache their scans, so
        // rendering every iteration purely for the change compare is
        // cheap.
        let (agents, skills, session_ctx) = tokio::join!(
            self.dispatch_text("agents", ctx),
            self.dispatch_text("skills", ctx),
            self.dispatch_session_context(ctx),
        );

        let mut sections: Vec<String> = Vec::new();

        // On-change sections. Empty renders are recorded but never
        // injected (there is nothing to retract — a section that
        // disappears simply stops riding the tail).
        if let Some(section) = state.take_changed(SectionSlot::CurrentTime, render_current_time()) {
            sections.push(section);
        }
        if let Some(section) = state.take_changed(SectionSlot::Memory, format_memory_section(ctx)) {
            sections.push(section);
        }
        if let Some(section) = state.take_changed(
            SectionSlot::SessionContext,
            format_session_context_section(ctx, &session_ctx),
        ) {
            sections.push(section);
        }
        if let Some(section) =
            state.take_changed(SectionSlot::Agents, format_agents_section(&agents))
        {
            sections.push(section);
        }
        if let Some(section) =
            state.take_changed(SectionSlot::Skills, format_skills_section(&skills))
        {
            sections.push(section);
        }

        // Always-on section: the iteration budget is one line, so it
        // rides every iteration and doubles as the turn heartbeat.
        if let Some(budget) = ctx.iteration_budget.as_ref() {
            sections.push(budget.render().trim_end().to_string());
        }

        // Event-edged sections: the loop sets these fields only on the
        // iteration the event fires.
        if ctx.quota_tripped {
            sections.push(render_quota_tripped_section().trim_end().to_string());
        }
        if ctx.soft_cancel_pending {
            sections.push(render_soft_cancel_section().trim_end().to_string());
        }
        if let Some(diff) = ctx.capability_diff.as_ref() {
            let rendered = diff.render();
            if !rendered.is_empty() {
                sections.push(rendered.trim_end().to_string());
            }
        }

        if sections.is_empty() {
            return None;
        }
        Some(format!(
            "<runtime-context>\n{}\n</runtime-context>",
            sections.join("\n\n")
        ))
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
            // The real session id — the loop knows it and threads it
            // through `TurnPromptContext` (previously `String::new()`).
            session_id: ctx.session_id.clone(),
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

/// The on-change sections tracked by [`RuntimeContextState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SectionSlot {
    CurrentTime,
    Memory,
    SessionContext,
    Agents,
    Skills,
}

/// Per-section last-injected values for the tail runtime-context
/// message.
///
/// Lives on the agentic loop as per-run state (one per `run_inner`
/// invocation) and is passed to
/// [`PromptRenderer::render_runtime_context`] every iteration. A
/// section is re-injected only when its rendered text differs from the
/// value recorded here, so large rarely-changing sections (memory,
/// agents/skills catalogs) ride the tail once and are not re-injected
/// until they actually change. `None` = never injected this run, so
/// iteration 1 injects every non-empty section.
#[derive(Debug, Default)]
pub struct RuntimeContextState {
    current_time: Option<String>,
    memory: Option<String>,
    session_context: Option<String>,
    agents: Option<String>,
    skills: Option<String>,
}

impl RuntimeContextState {
    /// Compare-and-record one section. Returns `Some(rendered)` when
    /// the rendered text differs from the last injected value (or the
    /// section was never rendered this run); returns `None` when
    /// unchanged. Empty renders are recorded but never returned — a
    /// section that renders empty simply doesn't ride the tail (there
    /// is nothing to retract; prior injections age out via compaction).
    fn take_changed(&mut self, slot: SectionSlot, rendered: String) -> Option<String> {
        let cell = match slot {
            SectionSlot::CurrentTime => &mut self.current_time,
            SectionSlot::Memory => &mut self.memory,
            SectionSlot::SessionContext => &mut self.session_context,
            SectionSlot::Agents => &mut self.agents,
            SectionSlot::Skills => &mut self.skills,
        };
        if cell.as_deref() == Some(rendered.as_str()) {
            return None;
        }
        let changed = (!rendered.is_empty()).then_some(rendered.clone());
        *cell = Some(rendered);
        changed
    }
}

/// Render the `{{current_time}}` line at MINUTE granularity. The clock
/// rides the runtime-context tail message, not the frozen system
/// prompt; truncating to whole minutes means the section changes at
/// most once a minute, so back-to-back iterations within the same
/// minute don't re-inject it (change detection in
/// [`RuntimeContextState`]).
///
/// The RFC3339 local timestamp already carries the offset, so no
/// separate timezone line is emitted.
fn render_current_time() -> String {
    format!(
        "Current time: {} (local) / {} (UTC)",
        Local::now().format("%Y-%m-%dT%H:%M%:z"),
        Utc::now().format("%Y-%m-%dT%H:%MZ")
    )
}

/// Build the placeholder → value map for one iteration.
fn build_placeholder_values(
    ctx: &TurnPromptContext,
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

    // Section placeholders (hook-driven). Tools are wire-only — see
    // `crate::agentic_loop::build_tool_definitions`.
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
        format_session_context_section(ctx, session_ctx),
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
        Placeholder::QuotaTripped,
        if ctx.quota_tripped {
            render_quota_tripped_section()
        } else {
            String::new()
        },
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
/// `mcp_context` section. `skills` and `agents` moved to the tail
/// runtime-context message ([`PromptRenderer::render_runtime_context`])
/// so new workspace files appear on the next iteration; the other
/// volatile placeholders (`timezone`, `memory`, `session_context`,
/// `iteration_budget`, `quota_tripped`, `soft_cancel`,
/// `capability_diff`) are omitted as before — `remove_missing=true`
/// strips them on render.
fn build_stable_placeholder_values(
    ctx: &TurnPromptContext,
    mcp: &str,
) -> HashMap<Placeholder, String> {
    let mut values = HashMap::new();

    // Inline identity / runtime (no volatile clock).
    values.insert(Placeholder::AgentName, ctx.agent_name.clone());
    values.insert(Placeholder::Workspace, ctx.workspace.display().to_string());
    values.insert(Placeholder::Channel, ctx.channel.clone());
    values.insert(Placeholder::ThinkingLevel, ctx.thinking_level.clone());
    // Placeholder::Timezone intentionally omitted — volatile.

    // Hook-driven sections. Tools are wire-only. Skills / Agents
    // intentionally omitted — they moved to the tail runtime-context
    // message.
    values.insert(Placeholder::Runtime, format_runtime_section(ctx));
    values.insert(Placeholder::Sandbox, format_sandbox_section(ctx));
    values.insert(Placeholder::ModelAliases, format_model_aliases_section(ctx));
    values.insert(
        Placeholder::SelfUpdate,
        format_self_update_section(ctx.has_gateway),
    );
    values.insert(Placeholder::McpContext, mcp.to_string());
    // Memory, SessionContext, IterationBudget, QuotaTripped, SoftCancel,
    // CapabilityDiff intentionally omitted — volatile.

    values
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
When delegating, choose the most appropriate agent from the list below. Each agent has a name you can pass to the `Agent` tool as the `agent` argument.

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

fn format_session_context_section(ctx: &TurnPromptContext, text: &str) -> String {
    // Peer-conversation lines (peer-ingress turns only): the DM
    // channel that reaches the user + the peer's subject, so the
    // model can target `ChannelSend` / cron reminders correctly.
    // `None` fields are skipped; a run with neither renders exactly
    // the pre-conversation-mode section.
    let mut lines = String::new();
    if let Some(channel) = ctx.conversation_channel.as_deref() {
        lines.push_str(&format!("conversation channel: {channel}\n"));
    }
    if let Some(peer) = ctx.conversation_peer.as_deref() {
        lines.push_str(&format!("peer: {peer}\n"));
    }
    let trimmed = text.trim();
    if !trimmed.is_empty() {
        lines.push_str(trimmed);
        lines.push('\n');
    }
    if lines.is_empty() {
        return String::new();
    }
    format!("## Session context\n\n{lines}")
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
    use peko_extension_api::ToolFunnel;
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
    /// with no handlers) valid. `SessionContextBuild` snapshots are
    /// captured for inspection (the session-id plumbing test).
    ///
    /// Tests that need a prompt section to render text populate
    /// `section_texts` (keyed by section name, e.g. `"agents"`) —
    /// `invoke_prompt_section_hook` then returns that text, mimicking
    /// a registered `PromptSystemSection` handler.
    #[derive(Default)]
    struct EmptyExtensionCore {
        session_context_snapshots: std::sync::Mutex<Vec<SessionSnapshot>>,
        section_texts: std::sync::Mutex<HashMap<String, String>>,
    }

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
            section: &str,
            _priority: i32,
            _principal_id: Option<&str>,
            _capabilities: Option<Vec<String>>,
            _active_extensions: Option<Vec<String>>,
            _workspace: Option<String>,
        ) -> Option<String> {
            self.section_texts
                .lock()
                .expect("section_texts mutex poisoned")
                .get(section)
                .cloned()
        }
        async fn invoke_session_context_build_hook(
            &self,
            _snapshot: SessionSnapshot,
            _principal_id: Option<&str>,
            _capabilities: Option<Vec<String>>,
            _active_extensions: Option<Vec<String>>,
            _workspace: Option<String>,
        ) -> Option<String> {
            self.session_context_snapshots
                .lock()
                .expect("snapshots mutex poisoned")
                .push(_snapshot);
            None
        }
    }

    fn empty_funnel() -> Arc<dyn ToolFunnel> {
        Arc::new(EmptyExtensionCore::default())
    }

    /// Funnel with `agents` / `skills` prompt-section handlers that
    /// return fixed catalog text — mimics the workspace-scanning
    /// handlers registered in root.
    fn catalog_funnel() -> Arc<dyn ToolFunnel> {
        let core = EmptyExtensionCore::default();
        {
            let mut texts = core.section_texts.lock().expect("section_texts poisoned");
            texts.insert(
                "agents".to_string(),
                "- Reviewer (id: reviewer): reviews code (location: /w/agents/reviewer/AGENT.md)"
                    .to_string(),
            );
            texts.insert(
                "skills".to_string(),
                "- docker: Docker ops (skills/docker/SKILL.md)".to_string(),
            );
        }
        Arc::new(core)
    }

    fn empty_ctx() -> TurnPromptContext {
        TurnPromptContext {
            principal_id: "test-principal".to_string(),
            session_id: "test-session".to_string(),
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
            conversation_channel: None,
            conversation_peer: None,
            iteration_budget: None,
            quota_tripped: false,
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
    // `{{iteration_budget}}`, `{{quota_tripped}}`, and
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
    async fn render_emits_quota_tripped_banner_when_pending() {
        let renderer = PromptRenderer::new(empty_funnel());
        let mut ctx = empty_ctx();
        ctx.body = "{{quota_tripped}}".to_string();
        ctx.quota_tripped = true;
        let rendered = renderer.render_for_iteration(&ctx).await;
        assert!(rendered.contains("## Quota tripped"));
        assert!(rendered.contains("non-essential"));
    }

    #[tokio::test]
    async fn render_omits_quota_tripped_when_false() {
        let renderer = PromptRenderer::new(empty_funnel());
        let mut ctx = empty_ctx();
        ctx.body = "{{quota_tripped}}".to_string();
        ctx.quota_tripped = false;
        let rendered = renderer.render_for_iteration(&ctx).await;
        assert_eq!(rendered, "");
    }

    #[tokio::test]
    async fn render_drops_unknown_quota_state_marker() {
        // `{{quota_state}}` was retired 2026-09-09; legacy templates
        // that still reference it must have the marker stripped
        // rather than crash or leak an old section.
        let renderer = PromptRenderer::new(empty_funnel());
        let mut ctx = empty_ctx();
        ctx.body = "before {{quota_state}} after".to_string();
        let rendered = renderer.render_for_iteration(&ctx).await;
        assert!(!rendered.contains("{{quota_state}}"));
        assert!(rendered.contains("before "));
        assert!(rendered.contains(" after"));
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

    // ---------- Frozen system prompt + tail runtime context ----------

    /// Two renderings of the cache-stable prefix with the same context
    /// produce byte-identical strings — the foundation of provider
    /// prefix-cache hits. The volatile placeholders
    /// (`{{iteration_budget}}`, `{{quota_tripped}}`, `{{session_context}}`)
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

    /// The frozen system prompt must not carry any volatile content:
    /// no leftover `{{placeholder}}` tokens and no volatile sections
    /// (clock, memory, iteration budget, catalogs). Everything the
    /// provider's front-anchored prefix cache matches on is static.
    #[tokio::test]
    async fn render_cache_stable_contains_no_volatile_content() {
        let renderer = PromptRenderer::new(catalog_funnel());
        let mut ctx = empty_ctx();
        ctx.body = "You are {{agent_name}}.\n{{current_time}}\n{{memory}}\n\
                    {{iteration_budget}}\n{{agents}}\n{{skills}}\n{{session_context}}"
            .to_string();
        ctx.principal_memory = Some("remember this".to_string());
        ctx.iteration_budget = Some(IterationBudgetState {
            iteration: 1,
            max_iterations: 10,
        });

        let prefix = renderer.render_cache_stable(&ctx).await;
        assert!(!prefix.contains("{{"), "prefix was: {prefix}");
        assert!(!prefix.contains("Current time:"), "prefix was: {prefix}");
        assert!(!prefix.contains("remember this"), "prefix was: {prefix}");
        assert!(
            !prefix.contains("## Iteration budget"),
            "prefix was: {prefix}"
        );
        assert!(
            !prefix.contains("## Available Agents"),
            "prefix was: {prefix}"
        );
        assert!(
            !prefix.contains("## Skills (mandatory)"),
            "prefix was: {prefix}"
        );
        assert!(
            prefix.contains("You are test-agent."),
            "prefix was: {prefix}"
        );
    }

    /// The runtime-context message is wrapped in the
    /// `<runtime-context>` envelope; the iteration-budget line rides
    /// every render (it is the always-on section), and mutating it
    /// between renders changes the message (proves the tail isn't
    /// accidentally stable).
    #[tokio::test]
    async fn runtime_context_envelope_and_iteration_budget() {
        let renderer = PromptRenderer::new(empty_funnel());
        let mut state = RuntimeContextState::default();
        let mut ctx = empty_ctx();
        ctx.iteration_budget = Some(IterationBudgetState {
            iteration: 1,
            max_iterations: 10,
        });

        let first = renderer
            .render_runtime_context(&ctx, &mut state)
            .await
            .expect("iteration budget is always due");
        assert!(first.starts_with("<runtime-context>\n"), "got: {first}");
        assert!(first.ends_with("\n</runtime-context>"), "got: {first}");
        assert!(first.contains("Iteration 1 of 10"), "got: {first}");

        ctx.iteration_budget = Some(IterationBudgetState {
            iteration: 9,
            max_iterations: 10,
        });
        let second = renderer
            .render_runtime_context(&ctx, &mut state)
            .await
            .expect("iteration budget is always due");
        assert!(second.contains("Iteration 9 of 10"), "got: {second}");
        assert!(second.contains("Approaching limit"), "got: {second}");
    }

    /// Change detection: the second render with unchanged inputs
    /// re-injects NONE of the on-change sections (clock, memory,
    /// session context, catalogs) — only the always-on iteration
    /// budget line changes.
    #[tokio::test]
    async fn runtime_context_renders_only_changed_sections() {
        let renderer = PromptRenderer::new(catalog_funnel());
        let mut state = RuntimeContextState::default();
        let mut ctx = empty_ctx();
        ctx.principal_memory = Some("remember this".to_string());
        ctx.conversation_channel = Some("chan_abc123".to_string());
        ctx.iteration_budget = Some(IterationBudgetState {
            iteration: 1,
            max_iterations: 10,
        });

        // Iteration 1: every non-empty section is due (state is cold).
        let first = renderer
            .render_runtime_context(&ctx, &mut state)
            .await
            .expect("first render must inject sections");
        assert!(first.contains("Current time:"), "got: {first}");
        assert!(first.contains("remember this"), "got: {first}");
        assert!(
            first.contains("conversation channel: chan_abc123"),
            "got: {first}"
        );
        assert!(first.contains("## Available Agents"), "got: {first}");
        assert!(first.contains("## Skills (mandatory)"), "got: {first}");

        // Iteration 2: unchanged inputs → the heavy sections are NOT
        // re-injected; only the fresh iteration-budget line rides.
        ctx.iteration_budget = Some(IterationBudgetState {
            iteration: 2,
            max_iterations: 10,
        });
        let second = renderer
            .render_runtime_context(&ctx, &mut state)
            .await
            .expect("iteration budget is always due");
        assert!(second.contains("Iteration 2 of 10"), "got: {second}");
        assert!(!second.contains("Current time:"), "got: {second}");
        assert!(!second.contains("remember this"), "got: {second}");
        assert!(!second.contains("## Available Agents"), "got: {second}");
        assert!(!second.contains("## Skills (mandatory)"), "got: {second}");

        // Iteration 3: memory changed → only memory is re-injected.
        ctx.principal_memory = Some("remember this AND that".to_string());
        ctx.iteration_budget = Some(IterationBudgetState {
            iteration: 3,
            max_iterations: 10,
        });
        let third = renderer
            .render_runtime_context(&ctx, &mut state)
            .await
            .expect("iteration budget is always due");
        assert!(third.contains("remember this AND that"), "got: {third}");
        assert!(!third.contains("## Available Agents"), "got: {third}");
        assert!(!third.contains("Current time:"), "got: {third}");
    }

    /// The clock renders at minute granularity (no seconds), so the
    /// current-time section changes at most once a minute and
    /// back-to-back iterations within the same minute don't re-inject
    /// it.
    #[tokio::test]
    async fn runtime_context_current_time_is_minute_granularity() {
        let renderer = PromptRenderer::new(empty_funnel());
        let mut state = RuntimeContextState::default();
        let mut ctx = empty_ctx();
        ctx.iteration_budget = Some(IterationBudgetState {
            iteration: 1,
            max_iterations: 10,
        });

        let first = renderer
            .render_runtime_context(&ctx, &mut state)
            .await
            .expect("first render must inject the clock");
        let expected = format!(
            "Current time: {} (local) / {} (UTC)",
            Local::now().format("%Y-%m-%dT%H:%M%:z"),
            Utc::now().format("%Y-%m-%dT%H:%MZ")
        );
        assert!(first.contains(&expected), "got: {first}");
        // No seconds component: with seconds the local timestamp would
        // read `HH:MM:SS+oo:oo`, so the minute string would be followed
        // by `:SS` instead of the offset / `Z`.
        let with_seconds = format!("Current time: {}:", Local::now().format("%Y-%m-%dT%H:%M"));
        assert!(!first.contains(&with_seconds), "got: {first}");

        // A second render in the same minute must NOT re-inject the
        // clock (change detection sees the identical minute string).
        ctx.iteration_budget = Some(IterationBudgetState {
            iteration: 2,
            max_iterations: 10,
        });
        let second = renderer
            .render_runtime_context(&ctx, &mut state)
            .await
            .expect("iteration budget is always due");
        assert!(!second.contains("Current time:"), "got: {second}");
    }

    #[tokio::test]
    async fn runtime_context_omits_timezone_line() {
        // The bare `+08:00` line was redundant with the clock line
        // (whose RFC3339 local timestamp already carries the offset);
        // it must not appear in the runtime-context message.
        let renderer = PromptRenderer::new(empty_funnel());
        let mut state = RuntimeContextState::default();
        let mut ctx = empty_ctx();
        ctx.iteration_budget = Some(IterationBudgetState {
            iteration: 1,
            max_iterations: 10,
        });
        let body = renderer
            .render_runtime_context(&ctx, &mut state)
            .await
            .expect("iteration budget is always due");
        let offset = Local::now().format("%:z").to_string();
        assert!(
            !body.lines().any(|line| line.trim() == offset),
            "runtime context must not contain a bare timezone-offset line; got: {body}"
        );
    }

    /// Event-edged sections ride only on the iteration they fire:
    /// `quota_tripped` / `soft_cancel` / `capability_diff` are included
    /// when the ctx flags them and absent otherwise (the loop computes
    /// the rising edges upstream of the renderer).
    #[tokio::test]
    async fn runtime_context_event_edged_sections() {
        let renderer = PromptRenderer::new(empty_funnel());
        let mut state = RuntimeContextState::default();
        let mut ctx = empty_ctx();
        ctx.quota_tripped = true;
        ctx.soft_cancel_pending = true;
        ctx.capability_diff = Some(CapabilityDiff {
            granted: vec![CapabilityChange {
                capability: "tool:Write".to_string(),
                kind: CapabilityChangeKind::Granted,
            }],
            revoked: vec![],
        });

        let fired = renderer
            .render_runtime_context(&ctx, &mut state)
            .await
            .expect("edged sections fire");
        assert!(fired.contains("## Quota tripped"), "got: {fired}");
        assert!(fired.contains("## Cancellation requested"), "got: {fired}");
        assert!(
            fired.contains("## Capability changes since last turn"),
            "got: {fired}"
        );

        ctx.quota_tripped = false;
        ctx.soft_cancel_pending = false;
        ctx.capability_diff = None;
        // Nothing due at all now (no iteration budget set, on-change
        // sections unchanged) → the whole message is skipped.
        let quiet = renderer.render_runtime_context(&ctx, &mut state).await;
        assert!(quiet.is_none(), "got: {quiet:?}");
    }

    /// The `agents` / `skills` workspace catalogs render in the
    /// runtime-context message when a `PromptSystemSection` handler
    /// returns text for them.
    #[tokio::test]
    async fn runtime_context_includes_agents_and_skills_sections() {
        let renderer = PromptRenderer::new(catalog_funnel());
        let mut state = RuntimeContextState::default();
        let mut ctx = empty_ctx();
        ctx.iteration_budget = Some(IterationBudgetState {
            iteration: 1,
            max_iterations: 10,
        });
        let body = renderer
            .render_runtime_context(&ctx, &mut state)
            .await
            .expect("sections due on first render");
        assert!(body.contains("## Available Agents"), "got: {body}");
        assert!(body.contains("<available_agents>"), "got: {body}");
        assert!(
            body.contains("- Reviewer (id: reviewer): reviews code"),
            "got: {body}"
        );
        assert!(body.contains("## Skills (mandatory)"), "got: {body}");
        assert!(body.contains("<available_skills>"), "got: {body}");
        assert!(body.contains("- docker: Docker ops"), "got: {body}");
    }

    /// The `agents` / `skills` catalogs must NOT leak into the
    /// cache-stable prefix — they moved to the tail runtime-context
    /// message so new workspace files appear on the next iteration and
    /// the prefix stays byte-stable.
    #[tokio::test]
    async fn render_cache_stable_omits_agents_and_skills_sections() {
        let renderer = PromptRenderer::new(catalog_funnel());
        let mut ctx = empty_ctx();
        // Even when the template references the placeholders, the
        // prefix strips them (`remove_missing=true`) — the sections
        // only ever render via the tail runtime-context message.
        ctx.body = "{{agents}}\n{{skills}}\nYou are {{agent_name}}.".to_string();
        let prefix = renderer.render_cache_stable(&ctx).await;
        assert!(
            !prefix.contains("## Available Agents"),
            "prefix was: {prefix}"
        );
        assert!(
            !prefix.contains("## Skills (mandatory)"),
            "prefix was: {prefix}"
        );
        assert!(!prefix.contains("reviewer"), "prefix was: {prefix}");
        assert!(!prefix.contains("docker"), "prefix was: {prefix}");
        assert!(
            prefix.contains("You are test-agent."),
            "prefix was: {prefix}"
        );
    }

    /// Without registered section handlers the runtime-context message
    /// omits the catalog sections entirely (no empty headers).
    #[tokio::test]
    async fn runtime_context_omits_catalogs_without_handlers() {
        let renderer = PromptRenderer::new(empty_funnel());
        let mut state = RuntimeContextState::default();
        let mut ctx = empty_ctx();
        ctx.iteration_budget = Some(IterationBudgetState {
            iteration: 1,
            max_iterations: 10,
        });
        let body = renderer
            .render_runtime_context(&ctx, &mut state)
            .await
            .expect("iteration budget is always due");
        assert!(!body.contains("## Available Agents"), "got: {body}");
        assert!(!body.contains("## Skills (mandatory)"), "got: {body}");
    }

    /// Prompt identity: the `SessionContextBuild` hook receives the
    /// real session id threaded through `TurnPromptContext` (it used
    /// to be a hardcoded `String::new()`).
    #[tokio::test]
    async fn session_context_hook_receives_real_session_id() {
        let core = Arc::new(EmptyExtensionCore::default());
        let funnel: Arc<dyn ToolFunnel> = core.clone();
        let renderer = PromptRenderer::new(funnel);
        let mut ctx = empty_ctx();
        ctx.session_id = "root:user:alice".to_string();
        ctx.body = "{{session_context}}".to_string();

        let _ = renderer.render_for_iteration(&ctx).await;

        let snapshots = core
            .session_context_snapshots
            .lock()
            .expect("snapshots mutex poisoned");
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].session_id, "root:user:alice");
    }
}
