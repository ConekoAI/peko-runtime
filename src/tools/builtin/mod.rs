//! Built-in tool implementations
//!
//! This module contains all built-in tools that ship with Pekobot:
//! - `fs`: Filesystem tools (read_file, write_file, glob, grep, str_replace_file)
//! - `messaging`: Messaging tools (agent_spawn, a2a_send, message_tool)
//! - `shell`: Shell execution
//! - `cron`: Scheduled jobs
//! - `session`: Session introspection
//! - `task_management`: Async task management

pub mod cron;
pub mod fs;
pub mod messaging;
pub mod session;
pub mod shell;
pub mod task_management;

pub use cron::CronTool;
pub use fs::{GlobTool, GrepTool, ReadFileTool, StrReplaceFileTool, WriteFileTool};
pub use messaging::{A2aSendTool, AgentSpawnTool, ChannelType, MessageConfig, MessageResult, MessageTool};
pub use session::{
    InMemorySessionRegistry, SessionCache, SessionInfo, SessionIntrospector,
    SessionRegistry as SessionIntrospectionRegistry, SessionTool,
};
pub use shell::ShellTool;
pub use task_management::TaskTool;

// Backward-compatible alias for `SessionIntrospector`.
#[deprecated(since = "0.2.0", note = "Use SessionIntrospector instead")]
pub use session::SessionIntrospector as AgentSessionRegistry;
