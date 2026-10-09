//! Built-in domain tools and runtime contracts. Cron lives in peko-cron.

pub mod async_control;
pub mod bash;
pub mod channel;
pub mod fs;
pub mod messaging;
pub mod paths;
pub mod plan;
pub mod session;
pub mod skill;
pub mod tasks;

#[cfg(test)]
mod schema_tests;
#[cfg(test)]
pub(crate) mod test_harness;

pub mod model_call;
pub mod model_list;
pub mod role_catalog;
pub mod workflow;

// Public domain tools and runtime ports.
pub use async_control::{AsyncRuntime, AsyncTool, SharedAsyncRuntime};
pub use bash::BashTool;
pub use channel::{ChannelReadTool, ChannelSendTool};
pub use fs::{EditTool, GlobTool, GrepTool, ReadTool, WriteTool};
pub use messaging::{
    AgentTool, SharedSubagentRuntime, SpawnAuditEvent, SpawnRequest, SubagentRuntime,
};
pub use model_call::{ModelCallTool, MODEL_CALL_TOOL_NAME};
pub use model_list::{ModelListTool, MODEL_LIST_TOOL_NAME};
pub use plan::PlanTool;
pub use role_catalog::AgentCatalogTool;
pub use session::caller_aware::CallerAwareSessionTool;
pub use session::{SessionCache, SessionInfo, SessionTool, SharedSessionRuntime};
pub use skill::{SharedSkillRuntime, SkillEntry, SkillFrontmatter, SkillTool};
pub use tasks::{TaskTool, Todo, TodoStatus};
pub use workflow::{
    WorkflowTool, WorkspaceWorkflowsPromptHandler, MAX_WORKFLOW_DEPTH,
    WORKFLOW_CATALOG_HOOK_PRIORITY, WORKFLOW_TOOL_NAME,
};

// Phase F4: thin re-exports of items that stay in the sat so root
// callers can keep importing them through `crate::tools::builtin::*`
// without caring which side of the fold line they're on.
pub use paths as paths_reexport;
