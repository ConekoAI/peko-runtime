//! Subagent DTOs lifted from root (`src/agents/{agent_config,
//! subagent_error, subagent_executor, subagent_types}.rs` and
//! `src/extensions/framework/async_exec/executor/registry.rs`).
//!
//! Phase 10e hoists the **shapes** AgentTool needs through its
//! `SubagentRuntime` port — the heavy `SubagentExecutor` itself
//! stays in root because it pulls in `AsyncExecutor`,
//! `Observability`, quota meters, and per-principal scope state
//! that aren't built-in-tool territory. The DTOs are pure data;
//! they can live alongside the tool.
//!
//! Root re-exports each type via `pub use peko_tools_builtin::messaging::...;`
//! so existing `crate::agents::agent_config::AgentConfig`,
//! `crate::agents::subagent_error::SpawnError`, and
//! `crate::agents::subagent_types::SubagentRunView` paths keep working.

// ─── AgentConfig (lifted from src/agents/agent_config.rs) ──────────

use serde::{Deserialize, Serialize};

/// Agent configuration
///
/// Mirrors root's `crate::agents::agent_config::AgentConfig`.
/// `subject_wire_id` (the helper that returned the principal wire ID)
/// moved into a root-side free function (`root_agent_wire_id`) because
/// it depends on `crate::auth::Subject::principal_wire_id` — root-only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Unique identifier (DID will be generated from this)
    pub name: String,
    /// Human-readable description
    pub description: Option<String>,
    /// The agent's system prompt template (Markdown).
    pub prompt: Option<String>,
    /// Per-agent stable identifier (DID).
    #[serde(default)]
    pub agent_did: Option<String>,
    /// Whether the planning-todo family is enabled.
    #[serde(default = "default_true")]
    pub enable_task_tools: bool,
    /// Whether the async execution family is enabled.
    #[serde(default = "default_true")]
    pub enable_async_tools: bool,
    /// F35 — whether the synthetic `__tool_search` stub is registered.
    #[serde(default)]
    pub enable_tool_search: bool,
    /// Channel that triggered this agent's LLM calls.
    #[serde(default)]
    pub channel: Option<String>,
    /// Thinking level for the model.
    #[serde(default)]
    pub thinking_level: Option<String>,
    /// Whether this agent runs inside an isolated sandbox.
    #[serde(default)]
    pub sandbox_enabled: bool,
    /// Configured model aliases.
    #[serde(default)]
    pub model_aliases: Vec<String>,
}

fn default_true() -> bool {
    true
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            name: "unnamed-agent".to_string(),
            description: None,
            prompt: None,
            agent_did: None,
            enable_task_tools: true,
            enable_async_tools: true,
            enable_tool_search: false,
            channel: None,
            thinking_level: None,
            sandbox_enabled: false,
            model_aliases: Vec::new(),
        }
    }
}

// ─── SpawnError (lifted from src/agents/subagent_error.rs) ─────────

/// Errors that can occur when spawning a subagent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpawnError {
    /// The spawn depth limit was exceeded.
    DepthLimitExceeded { current: u32, max: u32 },
    /// The concurrent subagent run limit was exceeded.
    ConcurrentLimitExceeded { current: usize, max: usize },
    /// The subagent execution timed out.
    Timeout { seconds: u64 },
    /// The subagent execution failed with an error message.
    ExecutionFailed(String),
}

impl std::fmt::Display for SpawnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpawnError::DepthLimitExceeded { current, max } => {
                write!(f, "Maximum spawn depth exceeded: {current} (max: {max})")
            }
            SpawnError::ConcurrentLimitExceeded { current, max } => {
                write!(
                    f,
                    "Maximum concurrent subagent runs exceeded: {current} (max: {max})"
                )
            }
            SpawnError::Timeout { seconds } => {
                write!(f, "Subagent execution timed out after {seconds} seconds")
            }
            SpawnError::ExecutionFailed(msg) => {
                write!(f, "Subagent execution failed: {msg}")
            }
        }
    }
}

impl std::error::Error for SpawnError {}

// ─── SpawnCleanupPolicy (re-export of peko_extension_host) ─────────
//
// The enum already lives in `peko-extension-host` (Phase 8 commit 2).
// We re-export it here so consumers of the messaging module can
// refer to one place; root re-exports preserve the
// `crate::session::types::SpawnCleanupPolicy` path.
//
// Why re-export rather than copy: the enum's wire representation is
// referenced by `peko_extension_api::SubagentMetadata` (a framework
// host contract), and `peko-extension-host` is its owner. Splitting
// the type across crates would require a `From` impl at every
// boundary and the orphan rule blocks that.
pub use peko_extension_host::SpawnCleanupPolicy;

// ─── ExecutionConfig (lifted from src/agents/subagent_executor.rs) ─

/// Configuration for subagent execution.
#[derive(Debug, Clone)]
pub struct ExecutionConfig {
    /// Maximum execution time in seconds (0 = unlimited)
    pub timeout_seconds: u64,
    /// Cleanup policy for the session
    pub cleanup: SpawnCleanupPolicy,
    /// Optional label for the run
    pub label: Option<String>,
    /// Whether to announce completion to parent
    pub announce_completion: bool,
    /// Maximum spawn depth (0 = unlimited)
    pub max_depth: u32,
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            timeout_seconds: 300,
            cleanup: SpawnCleanupPolicy::Keep,
            label: None,
            announce_completion: true,
            max_depth: 1,
        }
    }
}

// ─── CompletedRun (lifted from src/agents/subagent_executor.rs) ────

/// A completed subagent run ready for announcement
#[derive(Debug, Clone)]
pub struct CompletedRun {
    /// The run that completed (view projected from unified registry)
    pub run: SubagentRunView,
    /// The parent session key
    pub parent_session_key: String,
    /// The announcement message
    pub announcement: String,
}

// ─── SubagentResult (lifted from src/extensions/framework/async_exec/executor/registry.rs)

/// Result of a subagent run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentResult {
    /// Final status
    pub status: peko_extension_api::AsyncTaskStatus,
    /// Output content (if successful)
    pub output: Option<String>,
    /// Error message (if failed)
    pub error: Option<String>,
    /// Token usage (input, output, total)
    pub token_usage: Option<(usize, usize, usize)>,
    /// Completion timestamp
    pub completed_at: chrono::DateTime<chrono::Utc>,
}

// ─── SubagentRunView (lifted from src/agents/subagent_types.rs) ────

/// A read-only view of an async task entry, projected into the
/// subagent domain model.
///
/// The `from_entry` projection method stayed in root because it
/// references `AsyncTaskEntry` / `TaskMetadata` — root-only types.
#[derive(Debug, Clone)]
pub struct SubagentRunView {
    pub run_id: String,
    pub child_session_key: String,
    pub parent_session_key: String,
    pub task: String,
    pub status: peko_extension_api::AsyncTaskStatus,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub cleanup: SpawnCleanupPolicy,
    pub label: Option<String>,
    pub result: Option<SubagentResult>,
    pub depth: u32,
    pub announce_completion: bool,
}
