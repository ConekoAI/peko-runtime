//! Tools for agents
//!
//! Core tools only. On-demand tools are downloaded from Pekohub or installed locally.

pub mod agent_management;
pub mod apply_patch;
pub mod browser;
pub mod context; // Tool context with abort signals and progress callbacks
pub mod cron_tool;
pub mod factory;
pub mod fetch;
pub mod filesystem;
pub mod http;
pub mod memory_tool;
pub mod message_tool;
pub mod process;
pub mod progress_demo; // Example tool with progress updates
pub mod session_introspection;
pub mod session_messaging;
pub mod traits;
pub mod web_search;

pub use agent_management::{
    AgentBroadcastTool, AgentInfoTool, AgentSpawnTool, AgentsListTool, ManagerCommand,
};
pub use apply_patch::{ApplyPatchConfig, ApplyPatchTool};
pub use browser::BrowserTool;
// Re-export context types for tool monitoring and abortion
pub use context::{
    wrap_tool, AbortSignal, AbortableTool, ToolAdapter, ToolContext, ToolWithContext,
};
pub use cron_tool::CronTool;
pub use factory::{ToolFactory, ToolFactoryConfig};
pub use fetch::{FetchConfig, FetchTool};
pub use filesystem::FileSystemTool;
pub use http::{HttpMethod, HttpTool};
pub use memory_tool::{MemoryTool, MemoryToolFactory};
pub use message_tool::{ChannelType, MessageConfig, MessageResult, MessageTool};
pub use process::ProcessTool;
pub use progress_demo::ProgressDemoTool; // Demo tool with progress support
pub use session_introspection::{
    InMemorySessionRegistry, SessionInfo, SessionRegistry as SessionIntrospectionRegistry,
    SessionStatusTool, SessionsHistoryTool, SessionsListTool,
};
pub use session_messaging::{AgentInbox, SessionMessagingTool};
pub use traits::{Tool, ToolError};
pub use web_search::{WebSearchConfig, WebSearchTool};
