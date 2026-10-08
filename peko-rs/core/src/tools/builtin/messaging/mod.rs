//! Agent tool, execution DTOs, and the SubagentRuntime port.
//! Production adapters live in agents; installation composes their bindings.

pub mod agent;
pub(crate) mod caller_aware;
pub mod dto;
pub mod subagent_runtime;

pub use agent::{AgentArgs, AgentTool};
pub use dto::{ExecutionConfig, SpawnCleanupPolicy, SpawnError, SubagentResult, SubagentRunView};
pub use subagent_runtime::{SharedSubagentRuntime, SpawnAuditEvent, SpawnRequest, SubagentRuntime};
