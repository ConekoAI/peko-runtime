//! Per-turn system prompt renderer.
//!
//! [`PromptRenderer`] is the single source of truth for the system prompt.
//! It is invoked by [`AgenticLoop::run_inner`](crate::engine::AgenticLoop::run_inner)
//! at the top of every iteration, fed a [`TurnPromptContext`], and returns
//! the freshly rendered Markdown body that becomes `messages[0]`.
//!
//! ## Design
//!
//! - **Stateless.** The renderer carries no per-iteration state. The
//!   capability-diff tracker lives on the [`AgenticLoop`] and is observed
//!   upstream of the renderer call.
//! - **Parallel hook dispatch.** The four hook-driven sections (`tools`,
//!   `skills`, `agents`, `mcp_context`) and the per-turn `SessionContextBuild`
//!   hook all fire concurrently via [`tokio::try_join!`]. Each handler is
//!   wrapped in a 2-second timeout; a slow or stuck handler soft-fails to
//!   empty so a single misbehaving extension can't stall the loop.
//! - **`mcp_context` normalized.** This section previously used plain
//!   `invoke_hook_text`; the rest use `_with_principal`. The renderer
//!   closes that inconsistency.
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
use crate::extensions::framework::types::SessionSnapshot;
use crate::extensions::framework::{ExtensionCore, HookInput, HookPoint};
use chrono::Local;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, warn};

/// Per-hook timeout budget. Two seconds is generous for the prompt-section
/// hooks (they only need to format a Markdown body from in-memory state)
/// and tight enough that a stuck handler cannot stall the agentic loop.
const HOOK_TIMEOUT: Duration = Duration::from_secs(2);

/// Renders the system prompt for one iteration.
///
/// Constructed once per agentic loop and shared across iterations. Cheap
/// to construct — just an `Arc` clone of the [`ExtensionCore`].
#[derive(Clone)]
pub struct PromptRenderer {
    extension_core: Arc<ExtensionCore>,
}

impl PromptRenderer {
    /// Create a new renderer bound to an [`ExtensionCore`].
    #[must_use]
    pub fn new(extension_core: Arc<ExtensionCore>) -> Self {
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

    /// Dispatch a single `PromptSystemSection` hook with a 2s timeout.
    /// Returns the empty string on timeout, missing handler, or error.
    async fn dispatch_text(&self, section: &str, ctx: &TurnPromptContext) -> String {
        let hook_point = HookPoint::PromptSystemSection {
            section: section.to_string(),
            priority: 100,
        };
        let principal_id = Some(ctx.principal_id.as_str());
        let capabilities = Some(ctx.capability_strings());
        let active_extensions = Some(ctx.active_extension_vec());
        let workspace = Some(ctx.workspace.to_string_lossy().to_string());

        let core = Arc::clone(&self.extension_core);
        let result = tokio::time::timeout(
            HOOK_TIMEOUT,
            core.invoke_hook_text_with_principal(
                hook_point,
                HookInput::Unit,
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

        let core = Arc::clone(&self.extension_core);
        let result = tokio::time::timeout(
            HOOK_TIMEOUT,
            core.invoke_hook_text_with_principal(
                HookPoint::SessionContextBuild,
                HookInput::SessionState(snapshot),
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
    use crate::extensions::framework::core::ExtensionCore;
    use std::path::PathBuf;

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
        let renderer = PromptRenderer::new(Arc::new(ExtensionCore::new()));
        let mut ctx = empty_ctx();
        ctx.body = String::new();
        let rendered = renderer.render_for_iteration(&ctx).await;
        assert_eq!(rendered, "You are test-agent.");
    }

    #[tokio::test]
    async fn render_replaces_inline_placeholders() {
        let renderer = PromptRenderer::new(Arc::new(ExtensionCore::new()));
        let ctx = empty_ctx();
        let rendered = renderer.render_for_iteration(&ctx).await;
        assert!(rendered.contains("You are test-agent"));
        assert!(rendered.contains("/tmp/workspace"));
        assert!(!rendered.contains("{{agent_name}}"));
    }

    #[tokio::test]
    async fn render_drops_unknown_placeholders() {
        let renderer = PromptRenderer::new(Arc::new(ExtensionCore::new()));
        let mut ctx = empty_ctx();
        ctx.body = "Hi {{agent_name}}; unknown: {{nope}}".to_string();
        let rendered = renderer.render_for_iteration(&ctx).await;
        assert_eq!(rendered, "Hi test-agent; unknown: ");
    }

    #[tokio::test]
    async fn render_emits_session_context_when_set() {
        let renderer = PromptRenderer::new(Arc::new(ExtensionCore::new()));
        let mut ctx = empty_ctx();
        ctx.body = "Hi {{agent_name}}\n\n{{session_context}}\n".to_string();
        // No SessionContextBuild handler registered → empty section → no header.
        let rendered = renderer.render_for_iteration(&ctx).await;
        assert!(!rendered.contains("## Session context"));
    }

    #[tokio::test]
    async fn render_emits_soft_cancel_when_pending() {
        let renderer = PromptRenderer::new(Arc::new(ExtensionCore::new()));
        let mut ctx = empty_ctx();
        ctx.body = "{{soft_cancel}}".to_string();
        ctx.soft_cancel_pending = true;
        let rendered = renderer.render_for_iteration(&ctx).await;
        assert!(rendered.contains("Cancellation requested"));
    }

    #[tokio::test]
    async fn render_omits_soft_cancel_when_not_pending() {
        let renderer = PromptRenderer::new(Arc::new(ExtensionCore::new()));
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
        let renderer = PromptRenderer::new(Arc::new(ExtensionCore::new()));
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
        let renderer = PromptRenderer::new(Arc::new(ExtensionCore::new()));
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
        let renderer = PromptRenderer::new(Arc::new(ExtensionCore::new()));
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
        let renderer = PromptRenderer::new(Arc::new(ExtensionCore::new()));
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
        let renderer = PromptRenderer::new(Arc::new(ExtensionCore::new()));
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
        let renderer = PromptRenderer::new(Arc::new(ExtensionCore::new()));
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
        let renderer = PromptRenderer::new(Arc::new(ExtensionCore::new()));
        let mut ctx = empty_ctx();
        ctx.body = "{{capability_diff}}".to_string();
        ctx.capability_diff = None;
        let rendered = renderer.render_for_iteration(&ctx).await;
        assert!(!rendered.contains("## Capability changes"));
    }
}
