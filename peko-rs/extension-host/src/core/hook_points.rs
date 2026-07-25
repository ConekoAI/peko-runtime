//! Hook point definitions for the Extension system
//!
//! This module defines all the hook points in the agentic loop where extensions
//! can attach. Each hook point represents a specific phase in the agent's lifecycle.

use std::fmt;

/// All possible hook points in the agentic loop
///
/// Extensions register handlers for specific hook points to inject behavior
/// at precise locations in the agent lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum HookPoint {
    // ═══════════════════════════════════════════════════════════════════════════
    // PROMPT LIFECYCLE
    // ═══════════════════════════════════════════════════════════════════════════
    /// Inject content into a system prompt section
    ///
    /// Called during: `SystemPromptBuilder.build()`
    ///
    /// Handlers receive: `HookInput::PromptBuild`
    /// Handlers return: `HookOutput::Text` (content to inject)
    ///
    /// # Fields
    /// - `section`: Name of the section ("tools", "skills", "runtime", etc.)
    /// - `priority`: Ordering within section (higher = earlier)
    PromptSystemSection { section: String, priority: i32 },

    /// Modify messages before sending to LLM
    ///
    /// Called during: Before `provider.chat()`
    ///
    /// Handlers receive: `HookInput::Message`
    /// Handlers return: `HookOutput::Message` (modified message)
    PromptPreProcess,

    /// Transform LLM response before parsing
    ///
    /// Called during: After provider response
    ///
    /// Handlers receive: `HookInput::Message`
    /// Handlers return: `HookOutput::Message` (modified response)
    PromptPostProcess,

    // ═══════════════════════════════════════════════════════════════════════════
    // TOOL LIFECYCLE
    // ═══════════════════════════════════════════════════════════════════════════
    /// Register tools for native tool calling
    ///
    /// Called during: Agentic loop initialization
    ///
    /// Handlers receive: `HookInput::ToolRegistry`
    /// Handlers return: `HookOutput::Tool` or `HookOutput::Vec` of tools
    ToolRegister,

    /// Intercept tool execution (wrapper/middleware)
    ///
    /// Called during: Tool execution
    ///
    /// Handlers receive: `HookInput::ToolCall { tool_name, params }`
    /// Handlers return: `HookOutput::Json` (modified result) or `HookResult::PassThrough`
    ///
    /// # Fields
    /// - `tool_name`: Specific tool name, or pattern for matching multiple tools
    ToolExecute { tool_name: String },

    /// F31x: pre-tool-use notification. Fires *before* `ToolExecute`
    /// middleware, so a handler can record/log/observe the
    /// impending call. Observe-only in v1 — returning
    /// `HookResult::Handled` does NOT abort the tool call (use
    /// `ToolExecute` for that today; `PreToolUse` mutation
    /// power is deferred to a follow-up). Mirrors codex's
    /// `PreToolUse` semantics without the abort contract.
    ///
    /// Handlers receive: `HookInput::ToolCall { tool_name, params }`
    /// Handlers return: `HookResult::PassThrough` or
    /// `HookResult::Continue(HookOutput::Unit)`.
    PreToolUse { tool_name: String },

    /// F31x: post-tool-use notification. Fires *after* `ToolExecute`
    /// middleware returns, just before the result is added to the
    /// LLM message list. Observe-only in v1 — any output the handler
    /// returns is ignored (mutation power is deferred). Code is
    /// free to record/log/diff the tool result here.
    ///
    /// Handlers receive: `HookInput::ToolCall { tool_name, params, ... }`
    /// with the original params; the result itself is NOT in the
    /// input payload today (would require a new `HookInput`
    /// variant — deferred). Use `ToolResultTransform` for
    /// post-call mutation.
    PostToolUse { tool_name: String },

    /// Modify tool result before returning to LLM
    ///
    /// Called during: After tool execution
    ///
    /// Handlers receive: `HookInput::Json` (tool result)
    /// Handlers return: `HookOutput::Json` (modified result)
    ToolResultTransform,

    /// Execute tool asynchronously
    ///
    /// Called during: Tool execution when async mode requested
    ///
    /// Handlers receive: `HookInput::ToolCall { tool_name, params }`
    /// Handlers return: `HookOutput::Receipt(AsyncReceipt)` or `HookResult::PassThrough`
    ///
    /// If no handler returns a receipt, falls back to sync-in-background
    ///
    /// # Fields
    /// - `tool_name`: Specific tool name, or pattern for matching multiple tools
    ToolExecuteAsync { tool_name: String },

    /// Check status of async task
    ///
    /// Called during: Status polling for async tasks
    ///
    /// Handlers receive: `HookInput::TaskStatus { task_id, tool_name }`
    /// Handlers return: `HookOutput::TaskStatus(AsyncTaskStatus)`
    ///
    /// # Fields
    /// - `tool_name`: Specific tool name pattern
    ToolCheckStatus { tool_name: String },

    /// Cancel async task
    ///
    /// Called during: Task cancellation request
    ///
    /// Handlers receive: `HookInput::TaskCancel { task_id, tool_name }`
    /// Handlers return: `HookOutput::Bool(success)`
    ///
    /// # Fields
    /// - `tool_name`: Specific tool name pattern
    ToolCancel { tool_name: String },

    // ═══════════════════════════════════════════════════════════════════════════
    // SESSION LIFECYCLE
    // ═══════════════════════════════════════════════════════════════════════════
    /// Hook into session state changes
    ///
    /// Called during: Session creation, update, compaction
    ///
    /// Handlers receive: `HookInput::SessionState`
    /// Handlers return: `HookOutput::Unit` or modifications
    SessionStateChange,

    /// Custom compaction strategies
    ///
    /// Called during: Session compaction/summarization
    ///
    /// Handlers receive: `HookInput::SessionState`
    /// Handlers return: `HookOutput::Text` (compacted summary)
    SessionCompaction,

    /// Modify context window (what gets sent to LLM)
    ///
    /// Called during: Context building before LLM call
    ///
    /// Handlers receive: `HookInput::SessionState`
    /// Handlers return: `HookOutput::Json` (modified context)
    SessionContextBuild,

    /// Bootstrap context injected at session start
    ///
    /// Called once when a new session is created (the `startup` event).
    /// Extensions can return text that is persisted on the session and
    /// rendered into the system prompt at the `{{session_context}}`
    /// placeholder.
    ///
    /// Handlers receive: `HookInput::SessionState` with `event` metadata
    /// Handlers return: `HookOutput::Text` (bootstrap context)
    SessionStart,

    /// Post-compaction augmentation
    ///
    /// Called AFTER compaction completes (whether by built-in or extension).
    /// Extensions may augment, validate, or log the compacted result.
    ///
    /// Handlers receive: `HookInput::SessionState`
    /// Handlers return: `HookOutput::MessageVec` (replace final message list)
    SessionCompactionPost,

    // ═══════════════════════════════════════════════════════════════════════════
    // I/O LIFECYCLE
    // ═══════════════════════════════════════════════════════════════════════════
    /// Register input channels (CLI, Discord, etc.)
    ///
    /// Called during: Agent initialization
    ///
    /// Handlers receive: `HookInput::Unit`
    /// Handlers return: `HookOutput::Json` (channel configuration)
    ChannelInput,

    /// Register output handlers (rendering, formatting)
    ///
    /// Called during: Agent initialization
    ///
    /// Handlers receive: `HookInput::Unit`
    /// Handlers return: `HookOutput::Json` (output handler configuration)
    ChannelOutput,

    /// Transform outgoing messages
    ///
    /// Called during: Before sending message to channel
    ///
    /// Handlers receive: `HookInput::Message`
    /// Handlers return: `HookOutput::Message` (modified message)
    MessagePreSend,

    /// Transform incoming messages
    ///
    /// Called during: After receiving message from channel
    ///
    /// Handlers receive: `HookInput::Message`
    /// Handlers return: `HookOutput::Message` (modified message)
    MessagePostReceive,

    // ═══════════════════════════════════════════════════════════════════════════
    // EVENT LIFECYCLE
    // ═══════════════════════════════════════════════════════════════════════════
    /// Subscribe to system events
    ///
    /// Called during: Event emission
    ///
    /// Handlers receive: `HookInput::SystemEvent`
    /// Handlers return: `HookResult::Handled` to consume, or `PassThrough`
    ///
    /// # Fields
    /// - `topic_pattern`: Pattern for matching events (e.g., "instance.*", "principal.created")
    EventSubscribe { topic_pattern: String },

    /// Emit custom events
    ///
    /// Called during: Custom event emission
    ///
    /// Handlers receive: `HookInput::SystemEvent`
    /// Handlers return: `HookOutput::Event` (additional events to emit)
    EventEmit,

    // ═══════════════════════════════════════════════════════════════════════════
    // AGENT LIFECYCLE
    // ═══════════════════════════════════════════════════════════════════════════
    /// Hook into agent initialization
    ///
    /// Called during: Agent startup
    ///
    /// Handlers receive: `HookInput::Unit`
    /// Handlers return: `HookOutput::Unit` or initialization data
    AgentInit,

    /// Hook into agent shutdown
    ///
    /// Called during: Agent shutdown
    ///
    /// Handlers receive: `HookInput::Unit`
    /// Handlers return: `HookOutput::Unit`
    AgentShutdown,

    /// F31x: post-agent-run notification. Fires *after* the agent
    /// loop exits (success / cap-hit / soft-interrupt / error),
    /// once per `run_with_resume` call. Differs from
    /// `AgentShutdown` in that `AgentShutdown` fires once per
    /// process teardown (from `Agent::stop()`); `Stop` fires once
    /// per run. Observe-only in v1.
    ///
    /// Handlers receive: `HookInput::Json(serde_json::Value)`
    /// carrying `{ "reason": "end"|"max_iterations"|"interrupted"|"error",
    /// "iterations": N, "max_iterations": N, "interrupted": bool,
    /// "success": bool }`.
    /// Handlers return: `HookResult::PassThrough` or
    /// `HookResult::Continue(HookOutput::Unit)`.
    Stop,

    /// F31x: post-agent-process notification. Fires once per
    /// `Agent::stop()` call — covers process teardown. Currently
    /// `Agent::stop()` is dead code (no production caller); wiring
    /// it into the daemon's teardown path at `daemon/mod.rs:443-477`
    /// is a follow-up PR. The hook is shipped here so extensions
    /// can register handlers ahead of that wiring.
    ///
    /// Handlers receive: `HookInput::Json(serde_json::Value)`
    /// carrying `{ "agent_name": ..., "agent_did": ... }`.
    /// Handlers return: `HookResult::PassThrough` or
    /// `HookResult::Continue(HookOutput::Unit)`.
    AfterAgent,

    /// Hook between iterations (for monitoring/intervention)
    ///
    /// Called during: Between agent loop iterations
    ///
    /// Handlers receive: `HookInput::Json` (iteration state)
    /// Handlers return: `HookOutput::Json` (modified state)
    ///
    /// # Fields
    /// - `iteration`: Current iteration number
    AgentIteration { iteration: usize },
}

impl HookPoint {
    /// Get a string representation of the hook point category
    #[must_use]
    pub fn category(&self) -> &'static str {
        match self {
            Self::PromptSystemSection { .. } | Self::PromptPreProcess | Self::PromptPostProcess => {
                "prompt"
            }

            Self::ToolRegister
            | Self::ToolExecute { .. }
            | Self::ToolExecuteAsync { .. }
            | Self::ToolCheckStatus { .. }
            | Self::ToolCancel { .. }
            | Self::ToolResultTransform
            | Self::PreToolUse { .. }
            | Self::PostToolUse { .. } => "tool",

            Self::SessionStateChange
            | Self::SessionCompaction
            | Self::SessionContextBuild
            | Self::SessionCompactionPost
            | Self::SessionStart => "session",

            Self::ChannelInput
            | Self::ChannelOutput
            | Self::MessagePreSend
            | Self::MessagePostReceive => "io",

            Self::EventSubscribe { .. } | Self::EventEmit => "event",

            Self::AgentInit
            | Self::AgentShutdown
            | Self::AgentIteration { .. }
            | Self::AfterAgent => "agent",
            Self::Stop => "loop",
        }
    }

    /// Get the hook point name
    #[must_use]
    pub fn name(&self) -> String {
        match self {
            Self::PromptSystemSection { section, .. } => {
                format!("prompt.system_section.{section}")
            }
            Self::PromptPreProcess => "prompt.pre_process".to_string(),
            Self::PromptPostProcess => "prompt.post_process".to_string(),

            Self::ToolRegister => "tool.register".to_string(),
            Self::ToolExecute { tool_name } => {
                format!("tool.execute.{tool_name}")
            }
            Self::ToolExecuteAsync { tool_name } => {
                format!("tool.execute_async.{tool_name}")
            }
            Self::ToolCheckStatus { tool_name } => {
                format!("tool.check_status.{tool_name}")
            }
            Self::ToolCancel { tool_name } => {
                format!("tool.cancel.{tool_name}")
            }
            Self::PreToolUse { tool_name } => {
                format!("tool.pre.{tool_name}")
            }
            Self::PostToolUse { tool_name } => {
                format!("tool.post.{tool_name}")
            }
            Self::ToolResultTransform => "tool.result_transform".to_string(),

            Self::SessionStateChange => "session.state_change".to_string(),
            Self::SessionCompaction => "session.compaction".to_string(),
            Self::SessionContextBuild => "session.context_build".to_string(),
            Self::SessionCompactionPost => "session.compaction_post".to_string(),
            Self::SessionStart => "session.start".to_string(),

            Self::ChannelInput => "io.channel_input".to_string(),
            Self::ChannelOutput => "io.channel_output".to_string(),
            Self::MessagePreSend => "io.message_pre_send".to_string(),
            Self::MessagePostReceive => "io.message_post_receive".to_string(),

            Self::EventSubscribe { topic_pattern } => {
                format!("event.subscribe.{topic_pattern}")
            }
            Self::EventEmit => "event.emit".to_string(),

            Self::AgentInit => "agent.init".to_string(),
            Self::AgentShutdown => "agent.shutdown".to_string(),
            Self::AfterAgent => "agent.after".to_string(),
            Self::Stop => "loop.stop".to_string(),
            Self::AgentIteration { iteration } => {
                format!("agent.iteration.{iteration}")
            }
        }
    }

    /// Check if this hook point matches a pattern
    ///
    /// Patterns can use wildcards:
    /// - `tool.execute.*` matches any tool execution
    /// - `prompt.*` matches any prompt hook
    /// - `event.subscribe.instance.*` matches instance events
    #[must_use]
    pub fn matches(&self, pattern: &str) -> bool {
        let name = self.name();

        // Exact match
        if name == pattern {
            return true;
        }

        // Handle wildcards
        let pattern_parts: Vec<&str> = pattern.split('.').collect();
        let name_parts: Vec<&str> = name.split('.').collect();

        if pattern_parts.len() > name_parts.len() {
            return false;
        }

        for (i, pattern_part) in pattern_parts.iter().enumerate() {
            if *pattern_part == "*" {
                // Wildcard matches any segment
                continue;
            }
            if i >= name_parts.len() || name_parts[i] != *pattern_part {
                return false;
            }
        }

        true
    }

    /// Get priority if applicable
    #[must_use]
    pub fn priority(&self) -> Option<i32> {
        match self {
            Self::PromptSystemSection { priority, .. } => Some(*priority),
            _ => None,
        }
    }
}

impl fmt::Display for HookPoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name())
    }
}

/// Builder for creating hook points with specific configurations
pub struct HookPointBuilder;

impl HookPointBuilder {
    /// Create a prompt system section hook point
    pub fn prompt_section(section: impl Into<String>) -> HookPoint {
        HookPoint::PromptSystemSection {
            section: section.into(),
            priority: crate::types::DEFAULT_HOOK_PRIORITY,
        }
    }

    /// Create a prompt system section hook point with priority
    pub fn prompt_section_with_priority(section: impl Into<String>, priority: i32) -> HookPoint {
        HookPoint::PromptSystemSection {
            section: section.into(),
            priority,
        }
    }

    /// Create a tool execution hook point for a specific tool
    pub fn tool_execute(tool_name: impl Into<String>) -> HookPoint {
        HookPoint::ToolExecute {
            tool_name: tool_name.into(),
        }
    }

    /// Create a tool execution hook point with wildcard pattern
    pub fn tool_execute_pattern(pattern: impl Into<String>) -> HookPoint {
        HookPoint::ToolExecute {
            tool_name: pattern.into(),
        }
    }

    /// Create an async tool execution hook point for a specific tool
    pub fn tool_execute_async(tool_name: impl Into<String>) -> HookPoint {
        HookPoint::ToolExecuteAsync {
            tool_name: tool_name.into(),
        }
    }

    /// Create an async tool execution hook point with wildcard pattern
    pub fn tool_execute_async_pattern(pattern: impl Into<String>) -> HookPoint {
        HookPoint::ToolExecuteAsync {
            tool_name: pattern.into(),
        }
    }

    /// Create a tool status check hook point for a specific tool
    pub fn tool_check_status(tool_name: impl Into<String>) -> HookPoint {
        HookPoint::ToolCheckStatus {
            tool_name: tool_name.into(),
        }
    }

    /// Create a tool status check hook point with wildcard pattern
    pub fn tool_check_status_pattern(pattern: impl Into<String>) -> HookPoint {
        HookPoint::ToolCheckStatus {
            tool_name: pattern.into(),
        }
    }

    /// Create a tool cancel hook point for a specific tool
    pub fn tool_cancel(tool_name: impl Into<String>) -> HookPoint {
        HookPoint::ToolCancel {
            tool_name: tool_name.into(),
        }
    }

    /// Create a tool cancel hook point with wildcard pattern
    pub fn tool_cancel_pattern(pattern: impl Into<String>) -> HookPoint {
        HookPoint::ToolCancel {
            tool_name: pattern.into(),
        }
    }

    /// Create an event subscription hook point
    pub fn event_subscribe(topic_pattern: impl Into<String>) -> HookPoint {
        HookPoint::EventSubscribe {
            topic_pattern: topic_pattern.into(),
        }
    }

    /// Create an agent iteration hook point
    #[must_use]
    pub fn agent_iteration(iteration: usize) -> HookPoint {
        HookPoint::AgentIteration { iteration }
    }

    /// F31x: pre-tool-use hook point for a specific tool name.
    /// Fires before `ToolExecute` middleware. Observe-only in v1.
    pub fn pre_tool_use(tool_name: impl Into<String>) -> HookPoint {
        HookPoint::PreToolUse {
            tool_name: tool_name.into(),
        }
    }

    /// F31x: pre-tool-use hook point with wildcard pattern (e.g.
    /// `"*"`, `"mcp:*"`). The `HookRegistry::get_hooks_for_point`
    /// wildcard grammar matches via `HookPoint::matches()`.
    pub fn pre_tool_use_pattern(pattern: impl Into<String>) -> HookPoint {
        HookPoint::PreToolUse {
            tool_name: pattern.into(),
        }
    }

    /// F31x: post-tool-use hook point for a specific tool name.
    /// Fires after `ToolExecute` returns, before the result is
    /// added to the LLM message list. Observe-only in v1.
    pub fn post_tool_use(tool_name: impl Into<String>) -> HookPoint {
        HookPoint::PostToolUse {
            tool_name: tool_name.into(),
        }
    }

    /// F31x: post-tool-use hook point with wildcard pattern.
    pub fn post_tool_use_pattern(pattern: impl Into<String>) -> HookPoint {
        HookPoint::PostToolUse {
            tool_name: pattern.into(),
        }
    }

    /// F31x: post-run-stop hook point. Fires once per
    /// `run_with_resume` call, regardless of exit reason. Payload
    /// via `HookInput::Json(value)`. Observe-only.
    #[must_use]
    pub fn stop() -> HookPoint {
        HookPoint::Stop
    }

    /// F31x: post-agent-process hook point. Fires once per
    /// `Agent::stop()` call (currently dead code — see
    /// `agents/agent.rs:874-892`). Payload via `HookInput::Json(value)`.
    /// Observe-only.
    #[must_use]
    pub fn after_agent() -> HookPoint {
        HookPoint::AfterAgent
    }
}

/// Predefined common hook points for convenience
pub mod common {
    use super::HookPoint;

    /// Hook into the tools section of the system prompt
    #[must_use]
    pub fn tools_section() -> HookPoint {
        HookPoint::PromptSystemSection {
            section: "tools".to_string(),
            priority: 100,
        }
    }

    /// Hook into the skills section of the system prompt
    #[must_use]
    pub fn skills_section() -> HookPoint {
        HookPoint::PromptSystemSection {
            section: "skills".to_string(),
            priority: 100,
        }
    }

    /// Hook into the runtime section of the system prompt
    #[must_use]
    pub fn runtime_section() -> HookPoint {
        HookPoint::PromptSystemSection {
            section: "runtime".to_string(),
            priority: 100,
        }
    }

    /// Register tools
    #[must_use]
    pub fn tool_register() -> HookPoint {
        HookPoint::ToolRegister
    }

    /// Handle channel input
    #[must_use]
    pub fn channel_input() -> HookPoint {
        HookPoint::ChannelInput
    }

    /// Handle channel output
    #[must_use]
    pub fn channel_output() -> HookPoint {
        HookPoint::ChannelOutput
    }

    /// Subscribe to all events
    #[must_use]
    pub fn all_events() -> HookPoint {
        HookPoint::EventSubscribe {
            topic_pattern: "*".to_string(),
        }
    }

    /// Subscribe to instance events
    #[must_use]
    pub fn instance_events() -> HookPoint {
        HookPoint::EventSubscribe {
            topic_pattern: "instance.*".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hook_point_category() {
        assert_eq!(
            HookPoint::PromptSystemSection {
                section: "tools".to_string(),
                priority: 100,
            }
            .category(),
            "prompt"
        );

        assert_eq!(HookPoint::ToolRegister.category(), "tool");

        assert_eq!(HookPoint::ChannelInput.category(), "io");
    }

    #[test]
    fn test_hook_point_name() {
        let hp = HookPoint::PromptSystemSection {
            section: "skills".to_string(),
            priority: 50,
        };
        assert_eq!(hp.name(), "prompt.system_section.skills");

        let hp = HookPoint::ToolExecute {
            tool_name: "Read".to_string(),
        };
        assert_eq!(hp.name(), "tool.execute.Read");
    }

    #[test]
    fn test_hook_point_matches() {
        let hp = HookPoint::ToolExecute {
            tool_name: "Read".to_string(),
        };

        assert!(hp.matches("tool.execute.Read"));
        assert!(hp.matches("tool.execute.*"));
        assert!(hp.matches("tool.*"));
        assert!(!hp.matches("prompt.*"));
        assert!(!hp.matches("tool.execute.Write"));
    }

    #[test]
    fn test_common_hook_points() {
        let tools = common::tools_section();
        assert!(
            matches!(tools, HookPoint::PromptSystemSection { section, .. } if section == "tools")
        );

        let register = common::tool_register();
        assert!(matches!(register, HookPoint::ToolRegister));
    }

    #[test]
    fn test_builder() {
        let hp = HookPointBuilder::prompt_section("custom");
        assert!(
            matches!(hp, HookPoint::PromptSystemSection { section, .. } if section == "custom")
        );

        let hp = HookPointBuilder::tool_execute("my_tool");
        assert!(matches!(hp, HookPoint::ToolExecute { tool_name } if tool_name == "my_tool"));

        let hp = HookPointBuilder::event_subscribe("instance.*");
        assert!(
            matches!(hp, HookPoint::EventSubscribe { topic_pattern } if topic_pattern == "instance.*")
        );
    }

    #[test]
    fn test_async_hook_points() {
        // Test ToolExecuteAsync
        let hp = HookPoint::ToolExecuteAsync {
            tool_name: "shell".to_string(),
        };
        assert_eq!(hp.name(), "tool.execute_async.shell");
        assert_eq!(hp.category(), "tool");
        assert!(hp.matches("tool.execute_async.shell"));
        assert!(hp.matches("tool.execute_async.*"));
        assert!(hp.matches("tool.*"));

        // Test ToolCheckStatus
        let hp = HookPoint::ToolCheckStatus {
            tool_name: "Agent".to_string(),
        };
        assert_eq!(hp.name(), "tool.check_status.Agent");
        assert_eq!(hp.category(), "tool");
        assert!(hp.matches("tool.check_status.Agent"));

        // Test ToolCancel
        let hp = HookPoint::ToolCancel {
            tool_name: "long_task".to_string(),
        };
        assert_eq!(hp.name(), "tool.cancel.long_task");
        assert_eq!(hp.category(), "tool");
        assert!(hp.matches("tool.cancel.long_task"));

        // Test builders
        let hp = HookPointBuilder::tool_execute_async("my_async_tool");
        assert!(
            matches!(hp, HookPoint::ToolExecuteAsync { tool_name } if tool_name == "my_async_tool")
        );

        let hp = HookPointBuilder::tool_check_status("my_async_tool");
        assert!(
            matches!(hp, HookPoint::ToolCheckStatus { tool_name } if tool_name == "my_async_tool")
        );

        let hp = HookPointBuilder::tool_cancel("my_async_tool");
        assert!(matches!(hp, HookPoint::ToolCancel { tool_name } if tool_name == "my_async_tool"));
    }

    #[test]
    fn test_session_start_hook_point() {
        let hp = HookPoint::SessionStart;
        assert_eq!(hp.name(), "session.start");
        assert_eq!(hp.category(), "session");
        assert!(hp.matches("session.start"));
        assert!(hp.matches("session.*"));
        assert!(!hp.matches("session.state_change"));
    }

    // ===================================================================
    // F31x — PreToolUse / PostToolUse / Stop / AfterAgent
    // ===================================================================

    #[test]
    fn test_f31x_pre_tool_use_hook_point() {
        let hp = HookPoint::PreToolUse {
            tool_name: "Echo".to_string(),
        };
        assert_eq!(hp.name(), "tool.pre.Echo");
        assert_eq!(hp.category(), "tool");
        assert!(hp.matches("tool.pre.Echo"));
        assert!(hp.matches("tool.pre.*"));
        assert!(hp.matches("tool.*"));
        assert!(!hp.matches("tool.execute.Echo"));
    }

    #[test]
    fn test_f31x_post_tool_use_hook_point() {
        let hp = HookPoint::PostToolUse {
            tool_name: "mcp:identity:echo".to_string(),
        };
        assert_eq!(hp.name(), "tool.post.mcp:identity:echo");
        assert_eq!(hp.category(), "tool");
        assert!(hp.matches("tool.post.mcp:identity:echo"));
        assert!(hp.matches("tool.post.*"));
    }

    #[test]
    fn test_f31x_stop_hook_point() {
        let hp = HookPoint::Stop;
        assert_eq!(hp.name(), "loop.stop");
        assert_eq!(hp.category(), "loop");
        assert!(hp.matches("loop.stop"));
        assert!(hp.matches("loop.*"));
        assert!(!hp.matches("loop.start"));
    }

    #[test]
    fn test_f31x_after_agent_hook_point() {
        let hp = HookPoint::AfterAgent;
        assert_eq!(hp.name(), "agent.after");
        assert_eq!(hp.category(), "agent");
        assert!(hp.matches("agent.after"));
        assert!(hp.matches("agent.*"));
        assert!(!hp.matches("agent.shutdown"));
    }

    #[test]
    fn test_f31x_pre_post_tool_use_builders() {
        // Specific-name builders round-trip through the HookPoint
        // shape. Wildcard-pattern builders store the pattern as the
        // `tool_name` field (the registry's wildcard grammar matches
        // via `HookPoint::name()` against `tool.pre.<name>`).
        let specific = HookPointBuilder::pre_tool_use("Bash");
        assert!(matches!(
            specific,
            HookPoint::PreToolUse { ref tool_name } if tool_name == "Bash"
        ));
        let pattern = HookPointBuilder::pre_tool_use_pattern("mcp:*");
        assert!(matches!(
            pattern,
            HookPoint::PreToolUse { ref tool_name } if tool_name == "mcp:*"
        ));

        let post_specific = HookPointBuilder::post_tool_use("Bash");
        assert!(matches!(
            post_specific,
            HookPoint::PostToolUse { ref tool_name } if tool_name == "Bash"
        ));
        let post_pattern = HookPointBuilder::post_tool_use_pattern("Bash");
        assert!(matches!(
            post_pattern,
            HookPoint::PostToolUse { ref tool_name } if tool_name == "Bash"
        ));
    }

    #[test]
    fn test_f31x_stop_after_agent_builders() {
        assert!(matches!(HookPointBuilder::stop(), HookPoint::Stop));
        assert!(matches!(
            HookPointBuilder::after_agent(),
            HookPoint::AfterAgent
        ));
    }
}
