//! Hook input/output/result types
//!
//! Lifted from `src/extensions/framework/types/hook_io.rs` in Phase 7.
//! Notable changes vs the pre-Phase-7 version:
//!
//! - `HookOutput::Tool(...)` is now `peko_provider_api::ToolDefinition`
//!   (was `crate::providers::ToolDefinition`).
//! - `HookInput::CompactionPreparation { ... }` and
//!   `HookInput::CompactionResult { ... }` now carry the data as
//!   `serde_json::Value` blobs so the API crate doesn't depend on
//!   `crate::session::compaction::*` (root-only types). The host
//!   `src/extensions/framework/types/hook_io.rs` re-export keeps a
//!   `compaction_preparation_payload` / `compaction_result_payload`
//!   helper for ergonomic construction at the engine call site.
//! - The `From<ToolResult> for HookOutput` impl was lifted into this
//!   crate (orphan rule: `ToolResult` is from `peko-tools-core` and
//!   `HookOutput` is in this crate, so the impl must live in a crate
//!   that owns at least one of the two — we own `HookOutput`).

use crate::async_status::AsyncTaskStatus;
use crate::async_types::AsyncReceipt;
use crate::session::{MessageEnvelope, PromptBuildState, SessionSnapshot, ToolRegistryAccess};
use peko_message::{ContentBlock, LlmMessage};
use peko_provider_api::ToolDefinition;
use peko_tools_core::ToolResult;

/// Result of a hook handler invocation
#[derive(Debug)]
pub enum HookResult {
    /// Continue with modified output
    Continue(HookOutput),

    /// Continue with original input (pass-through)
    PassThrough,

    /// Stop propagation, handler consumed the event
    Handled,

    /// Replace entire result with this output
    Replace(HookOutput),

    /// Error occurred during handling
    Error(anyhow::Error),
}

/// Output from a hook handler
#[derive(Debug, Clone, Default)]
pub enum HookOutput {
    /// No output
    #[default]
    Unit,

    /// Text fragment (for prompt sections)
    Text(String),

    /// Tool registration
    Tool(ToolDefinition),

    /// Message transformation
    Message(ContentBlock),

    /// Generic JSON value
    Json(serde_json::Value),

    /// Multiple outputs
    Vec(Vec<HookOutput>),

    /// Async execution receipt (returned by `ToolExecuteAsync`)
    Receipt(AsyncReceipt),

    /// Task status (returned by `ToolCheckStatus`)
    TaskStatus(AsyncTaskStatus),

    /// Boolean result (for operations like cancel)
    Bool(bool),

    /// Vector of LlmMessages (for compaction/context hooks)
    MessageVec(Vec<LlmMessage>),
}

impl HookOutput {
    /// Create empty output
    #[must_use]
    pub fn unit() -> Self {
        Self::Unit
    }

    /// Create text output
    pub fn text(s: impl Into<String>) -> Self {
        Self::Text(s.into())
    }

    /// Create JSON output
    pub fn json(v: impl Into<serde_json::Value>) -> Self {
        Self::Json(v.into())
    }

    /// Combine multiple outputs
    #[must_use]
    pub fn combine(outputs: Vec<HookOutput>) -> Self {
        Self::Vec(outputs)
    }

    /// Convert to text if possible
    #[must_use]
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(s) => Some(s),
            _ => None,
        }
    }

    /// Convert to JSON if possible
    #[must_use]
    pub fn as_json(&self) -> Option<&serde_json::Value> {
        match self {
            Self::Json(v) => Some(v),
            _ => None,
        }
    }

    /// Convert to receipt if possible
    #[must_use]
    pub fn as_receipt(&self) -> Option<&AsyncReceipt> {
        match self {
            Self::Receipt(r) => Some(r),
            _ => None,
        }
    }

    /// Convert to task status if possible
    #[must_use]
    pub fn as_task_status(&self) -> Option<&AsyncTaskStatus> {
        match self {
            Self::TaskStatus(s) => Some(s),
            _ => None,
        }
    }

    /// Convert to bool if possible
    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Create a receipt output
    #[must_use]
    pub fn receipt(receipt: AsyncReceipt) -> Self {
        Self::Receipt(receipt)
    }

    /// Create a task status output
    #[must_use]
    pub fn task_status(status: AsyncTaskStatus) -> Self {
        Self::TaskStatus(status)
    }

    /// Create a boolean output
    #[must_use]
    pub fn bool(value: bool) -> Self {
        Self::Bool(value)
    }

    /// Create a message vector output
    #[must_use]
    pub fn message_vec(messages: Vec<LlmMessage>) -> Self {
        Self::MessageVec(messages)
    }
}

/// Wrap a `ToolResult` as a `HookOutput::Json` payload.
///
/// Lifted into the API crate in Phase 7. The orphan rule requires trait
/// impls that touch a foreign type to live in a crate that owns at
/// least one of the types involved; this crate owns `HookOutput` and
/// depends on `peko-tools-core` (which owns `ToolResult`), so the impl
/// belongs here.
impl From<ToolResult> for HookOutput {
    fn from(result: ToolResult) -> Self {
        Self::Json(result.to_json())
    }
}

/// Input to a hook handler
#[derive(Debug, Clone, Default)]
pub enum HookInput {
    /// No input
    #[default]
    Unit,

    /// Prompt build state
    PromptBuild(PromptBuildState),

    /// Tool registry access
    ToolRegistry(ToolRegistryAccess),

    /// Tool call parameters
    ToolCall {
        tool_name: String,
        params: serde_json::Value,
        /// Workspace directory for tool execution (optional)
        workspace: Option<String>,
        /// Agent identifier for reserved parameter injection (optional)
        agent_id: Option<String>,
        /// Session identifier for reserved parameter injection (optional)
        session_id: Option<String>,
        /// Resolved caller identity (pekohub sub, API key id, or `local`)
        /// — populated on tunneled requests so per-user permission
        /// checks (issue #17) and audit logging can attribute the call
        /// to a real user. `None` for local CLI invocations.
        caller_id: Option<String>,
        /// Principal identifier (post-PR-#94 root-agent unification).
        /// `None` when the call originates from a context that has no
        /// principal scope (legacy agent path, tests).
        /// Threaded into `ToolRuntimeContext` and `ToolContext` so
        /// extension-scoped tools (e.g. `Skill`) can resolve per-
        /// principal state at handle time without per-call re-
        /// registration on the shared global `ExtensionCore`.
        principal_id: Option<String>,
        /// Human-readable Principal name. Cron-scoped tools use this to
        /// create and filter jobs for the current Principal.
        principal_name: Option<String>,
        /// Capability grants for this tool call. When present, the
        /// execution gate checks this set instead of the mutable global
        /// `tool_config`, eliminating a TOCTOU race where concurrent
        /// agents overwrite each other's capability set on the shared core.
        capabilities: Option<Vec<String>>,
        /// Active extension IDs for this tool call. When present, the
        /// execution gate also verifies that the tool's owning extension
        /// is active. This prevents calling tools whose owning extension
        /// is installed but not authorized for the current Principal.
        active_extensions: Option<Vec<String>>,
        /// Optional abort signal receiver for soft-interrupt propagation.
        /// When `Some`, `BuiltinToolAdapter` builds the `ToolContext`
        /// from this receiver (via
        /// `ToolContext::for_hook_run_with_abort`) so the trait-default
        /// `ctx.is_aborted()` check in
        /// `peko_tools_core::traits` is meaningful in production —
        /// previously the adapter created a fresh never-aborted
        /// receiver and the check was a no-op. Bridges the engine's
        /// `CancellationToken` (soft-interrupt) into the tool layer
        /// without changing the public `AbortSignal`/`ToolContext` API.
        abort_signal: Option<tokio::sync::watch::Receiver<bool>>,
    },

    /// Async task status check
    TaskStatus { task_id: String, tool_name: String },

    /// Async task cancellation request
    TaskCancel { task_id: String, tool_name: String },

    /// Session snapshot
    SessionState(SessionSnapshot),

    /// Compaction preparation data (pre-compaction hook)
    ///
    /// Phase 7: data is carried as `serde_json::Value` because the
    /// pre-Phase-7 struct embedded `crate::session::compaction::*`
    /// types that live in the root crate only. The host helper
    /// `compaction_preparation_payload(...)` in
    /// `src/extensions/framework/types/hook_io.rs` constructs the
    /// JSON; the engine decodes it via
    /// `compaction_preparation::decode` at the consumer site.
    CompactionPreparation {
        /// Serialized `Vec<LlmMessage>` to be summarized
        messages_to_summarize: serde_json::Value,
        /// Serialized `Vec<LlmMessage>` preserved intact (turn prefix for split turns)
        turn_prefix_messages: serde_json::Value,
        /// Whether the cut landed mid-turn
        is_split_turn: bool,
        /// Previous compaction summary (for cumulative updates)
        previous_summary: Option<String>,
        /// Serialized `CompactionDetails`
        file_ops: serde_json::Value,
        /// Estimated tokens in the current context
        estimated_tokens: usize,
        /// Threshold tokens that triggered compaction
        threshold_tokens: usize,
        /// Model context window limit
        model_context_limit: usize,
        /// Serialized `CompactionConfig`
        settings: serde_json::Value,
    },

    /// Compaction result data (post-compaction hook)
    ///
    /// Phase 7: data is carried as `serde_json::Value`; see
    /// `CompactionPreparation` above for the rationale.
    CompactionResult {
        /// Summary text from the compaction
        summary: String,
        /// Number of messages that were compacted
        messages_compacted: usize,
        /// Tokens before compaction
        tokens_before: usize,
        /// Tokens after compaction
        tokens_after: usize,
        /// Compaction number (1st, 2nd, etc.)
        compaction_number: usize,
        /// Serialized `CompactionDetails` (file ops)
        details: Option<serde_json::Value>,
        /// Serialized `Vec<LlmMessage>` (summary + kept messages)
        messages_after: serde_json::Value,
    },

    /// Message envelope
    Message(MessageEnvelope),

    /// Generic JSON value
    Json(serde_json::Value),
}

/// Convert a `HookResult` from tool execution into a structured triplet.
///
/// Returns `(display_string, json_value, success)` where:
/// - `display_string` is the human-readable result (for LLM consumption)
/// - `json_value` is the structured result (for session storage)
/// - `success` indicates whether execution succeeded
///
/// This is the single place where `HookResult`→tool output semantics are defined,
/// ensuring `AgenticLoop` and `ToolRuntime` behave identically.
pub fn tool_result_from_hook(
    result: HookResult,
    tool_name: &str,
) -> (String, serde_json::Value, bool) {
    match result {
        HookResult::Continue(HookOutput::Json(result)) => {
            let s = result.to_string();
            (s, result, true)
        }
        HookResult::Continue(HookOutput::Text(result)) => {
            (result.clone(), serde_json::Value::String(result), true)
        }
        HookResult::Continue(HookOutput::Vec(outputs)) => {
            let result = outputs.iter().find_map(|o| match o {
                HookOutput::Json(v) => Some((v.to_string(), v.clone())),
                HookOutput::Text(t) => Some((t.clone(), serde_json::Value::String(t.clone()))),
                _ => None,
            });
            if let Some((s, v)) = result {
                (s, v, true)
            } else {
                let s = format!("Error: Unexpected Vec output from tool '{tool_name}'");
                (s.clone(), serde_json::Value::String(s), false)
            }
        }
        HookResult::Continue(_other) => {
            let s = format!("Error: Unexpected output type from tool '{tool_name}'");
            (s.clone(), serde_json::Value::String(s), false)
        }
        HookResult::PassThrough => {
            let s = format!("Tool '{tool_name}' not available");
            (s.clone(), serde_json::Value::String(s), false)
        }
        HookResult::Error(e) => {
            let s = format!("Error: {e}");
            (s.clone(), serde_json::Value::String(s), false)
        }
        HookResult::Handled => {
            let s = format!("Error: Tool '{tool_name}' execution was consumed by handler");
            (s.clone(), serde_json::Value::String(s), false)
        }
        HookResult::Replace(output) => {
            let s = format!("Error: Tool '{tool_name}' execution was replaced: {output:?}");
            (s.clone(), serde_json::Value::String(s), false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hook_output() {
        let text = HookOutput::text("Hello");
        assert_eq!(text.as_text(), Some("Hello"));
        assert!(text.as_json().is_none());

        let json = HookOutput::json(serde_json::json!({"key": "value"}));
        assert!(json.as_text().is_none());
        assert!(json.as_json().is_some());
    }

    #[test]
    fn test_hook_output_message_vec() {
        let messages = vec![LlmMessage::system("System"), LlmMessage::user("User")];
        let output = HookOutput::message_vec(messages);
        match output {
            HookOutput::MessageVec(msgs) => assert_eq!(msgs.len(), 2),
            _ => panic!("Expected MessageVec variant"),
        }
    }

    #[test]
    fn test_tool_result_from_hook() {
        let result = HookResult::Continue(HookOutput::json(serde_json::json!({"ok": true})));
        let (_s, v, ok) = tool_result_from_hook(result, "test");
        assert!(ok);
        assert_eq!(v, serde_json::json!({"ok": true}));

        let result = HookResult::PassThrough;
        let (s, _v, ok) = tool_result_from_hook(result, "test");
        assert!(!ok);
        assert!(s.contains("not available"));
    }

    /// Issue #17: `HookInput::ToolCall::caller_id` must carry the
    /// resolved caller through to the hook layer so per-user permission
    /// checks (issue #17 follow-up) and audit logging can attribute the
    /// call to a real user. P2-audit: `principal_id` rides alongside
    /// `caller_id` so extension-scoped tools (`Skill`, future
    /// additions) can resolve per-principal state at handle time.
    #[test]
    fn test_hook_input_tool_call_carries_caller_id() {
        let input = HookInput::ToolCall {
            tool_name: "shell".to_string(),
            params: serde_json::json!({"command": "ls"}),
            workspace: None,
            agent_id: Some("agent-a".to_string()),
            session_id: Some("sess-1".to_string()),
            caller_id: Some("user-42".to_string()),
            principal_id: Some("principal-z".to_string()),
            principal_name: None,
            capabilities: None,
            active_extensions: None,
            abort_signal: None,
        };
        match input {
            HookInput::ToolCall {
                ref tool_name,
                ref agent_id,
                ref session_id,
                ref caller_id,
                ref principal_id,
                ..
            } => {
                assert_eq!(tool_name, "shell");
                assert_eq!(agent_id.as_deref(), Some("agent-a"));
                assert_eq!(session_id.as_deref(), Some("sess-1"));
                assert_eq!(caller_id.as_deref(), Some("user-42"));
                assert_eq!(principal_id.as_deref(), Some("principal-z"));
            }
            _ => panic!("Expected ToolCall variant"),
        }
    }
}
